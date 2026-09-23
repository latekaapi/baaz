//! The one shape every wire call in this app has: run the blocking request on
//! the background executor, then come back to the entity with the result.
//!
//! Every `muse-client` request blocks (`docs/07-architecture.md` §3), so none
//! of them may be issued from the UI thread. The pattern that follows from
//! that — `background_spawn` the request, `cx.spawn` a foreground task, push
//! it onto the entity's `tasks` so it dies with the entity, `await` the call
//! and hand the result to `update` — used to be retyped at some forty sites
//! across `app.rs` and `session.rs` (finding `app-core-14`). [`WireCall`] is
//! that pattern, written once: a call site now says only **what the request
//! is** and **what to do with its result**.
//!
//! ```ignore
//! let client = self.client.clone()?;
//! self.wire_call(cx, move || client.account_read(), |this, result, cx| match result {
//!     Ok(state) => this.apply_account(state, cx),
//!     Err(error) => { /* … */ }
//! });
//! ```
//!
//! Two variants, because two kinds of completion exist:
//!
//! * [`WireCall::wire_call`] returns through `update`, which is what a
//!   completion that only touches the entity needs.
//! * [`WireCall::wire_call_in`] returns through `update_in`, for the
//!   completions that also need a [`Window`] — clearing a field, moving the
//!   focus, opening a session.
//!
//! Nothing else changes: the closure that used to be the body of the `update`
//! is the closure passed here, error handling included, and a completion that
//! arrives after the entity is gone is still dropped rather than panicking.
//!
//! The sites this does **not** cover are the ones that are not a request:
//! the event pump, the `--steps` loops, the timers behind the undo window,
//! the retry backoff and the two tickers, and the fire-and-forget
//! `account/loginCancel`.

use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, Context, Task, Window};
use provider::{Ack, Command, Provider, ProviderError};

/// One connected provider, shared with every background command task.
/// [`Provider::send`] is blocking, so the handle only ever travels onto the
/// background executor (see [`ProviderCall`]).
///
/// A mutex, not a bare `Arc`: the adapter trait is `Send` but not `Sync`
/// (the muse transport holds its I/O join handles), so a bare `Arc` could
/// not cross onto the executor. Commands therefore serialize on this lock —
/// today that serializes nothing in production, where the legacy lane still
/// carries every command; the follow-up that moves commands over can
/// revisit the granularity.
pub type SharedProvider = Arc<Mutex<Provider>>;

/// The background-call-then-update shape, for any entity that keeps its
/// foreground tasks in a `Vec<Task<()>>`.
///
/// Implementors hand over that vector and get both variants for free; there
/// is deliberately no other method, because there is deliberately no other
/// way to reach the wire.
pub trait WireCall: Sized + 'static {
    /// Where a foreground completion task is parked so it dies with the
    /// entity. Dropping the entity drops the task, which is what makes an
    /// in-flight request harmless.
    fn wire_tasks(&mut self) -> &mut Vec<Task<()>>;

    /// Run `work` on the background executor and hand its result to `then`
    /// on the UI thread, inside the entity's `update`.
    fn wire_call<T, W, F>(&mut self, cx: &mut Context<Self>, work: W, then: F)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        F: FnOnce(&mut Self, T, &mut Context<Self>) + 'static,
    {
        let call = cx.background_spawn(async move { work() });
        let task = cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| then(this, result, cx));
        });
        self.wire_tasks().push(task);
    }

    /// [`Self::wire_call`] for a completion that needs the window: the same
    /// call, returning through `update_in`.
    fn wire_call_in<T, W, F>(&mut self, cx: &mut Context<Self>, work: W, then: F)
    where
        T: Send + 'static,
        W: FnOnce() -> T + Send + 'static,
        F: FnOnce(&mut Self, T, &mut Window, &mut Context<Self>) + 'static,
    {
        let call = cx.background_spawn(async move { work() });
        let task = cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| then(this, result, window, cx));
        });
        self.wire_tasks().push(task);
    }
}

/// Issue a neutral [`Command`] on the background executor and come back
/// through `update`, with the provider's [`Ack`] or refusal.
///
/// This is [`WireCall`] with the request fixed: [`Provider::send`] blocks
/// like every wire call, so the command runs inside [`WireCall::wire_call`]
/// — the UI thread issues the intent and never waits — and the capability
/// gate runs there too, before any adapter code. A completion that arrives
/// after the entity is gone is dropped rather than panicking, exactly as
/// before.
///
/// No production caller yet — the committed tests below exercise the shape,
/// and the view migration moves the first production lane onto it — so the
/// shape ships as dead code until then rather than as a second copy typed
/// at each future call site.
#[allow(dead_code)]
pub trait ProviderCall: WireCall {
    /// Run `command` through [`Provider::send`] on the background executor
    /// and hand its result to `then` on the UI thread.
    fn provider_call<F>(
        &mut self,
        cx: &mut Context<Self>,
        provider: SharedProvider,
        command: Command,
        then: F,
    )
    where
        F: FnOnce(&mut Self, Result<Ack, ProviderError>, &mut Context<Self>) + 'static,
    {
        self.wire_call(
            cx,
            move || provider.lock().expect("provider mutex").send(command),
            then,
        );
    }

    /// [`Self::provider_call`] for a completion that needs the window.
    fn provider_call_in<F>(
        &mut self,
        cx: &mut Context<Self>,
        provider: SharedProvider,
        command: Command,
        then: F,
    )
    where
        F: FnOnce(&mut Self, Result<Ack, ProviderError>, &mut Window, &mut Context<Self>) + 'static,
    {
        self.wire_call_in(
            cx,
            move || provider.lock().expect("provider mutex").send(command),
            then,
        );
    }
}

/// Every entity that can reach the wire can issue a [`Command`]: there is
/// deliberately no other way to send one.
impl<T: WireCall> ProviderCall for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::scripted::ScriptedProvider;
    use provider::{ConnectInfo, SubmissionPart};

    /// A minimal entity holding foreground tasks, so the test drives the
    /// real [`ProviderCall`] shape rather than a retyped copy of it.
    struct Probe {
        tasks: Vec<Task<()>>,
        seen: Option<Result<String, String>>,
    }

    impl WireCall for Probe {
        fn wire_tasks(&mut self) -> &mut Vec<Task<()>> {
            &mut self.tasks
        }
    }

    /// `provider_call` issues the [`Command`] on the background executor
    /// and returns its ack through `update` — driven here by an adapter
    /// that is not `provider-muse`.
    #[gpui::test]
    fn a_command_goes_out_and_its_ack_comes_back(cx: &mut gpui::TestAppContext) {
        let mut provider = Provider::new(ScriptedProvider::new());
        provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("connect");
        let provider = Arc::new(Mutex::new(provider));

        let vc = cx.add_empty_window();
        let probe = vc.update(|_, cx| cx.new(|_| Probe { tasks: Vec::new(), seen: None }));
        vc.update(|_, cx| {
            probe.update(cx, |probe, cx| {
                probe.provider_call(
                    cx,
                    Arc::clone(&provider),
                    Command::SubmitInput {
                        request_id: "r-1".into(),
                        session_id: "s-scripted".into(),
                        parts: vec![SubmissionPart::Text("hello".into())],
                        display_text: None,
                    },
                    |probe, result, _| {
                        probe.seen = Some(match result {
                            Ok(Ack::TurnAccepted { turn_id }) => Ok(turn_id),
                            Ok(other) => Err(format!("wrong ack: {other:?}")),
                            Err(error) => Err(error.to_string()),
                        });
                    },
                );
            });
        });
        vc.run_until_parked();

        let seen = vc.update(|_, cx| probe.read(cx).seen.clone());
        assert_eq!(seen, Some(Ok("a-latest".to_owned())));
    }

    /// The window variant returns through `update_in` with the same ack.
    #[gpui::test]
    fn a_command_with_a_window_completion_comes_back_too(cx: &mut gpui::TestAppContext) {
        let mut provider = Provider::new(ScriptedProvider::new());
        provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("connect");
        let provider = Arc::new(Mutex::new(provider));

        let vc = cx.add_empty_window();
        let probe = vc.update(|_, cx| cx.new(|_| Probe { tasks: Vec::new(), seen: None }));
        vc.update(|_, cx| {
            probe.update(cx, |probe, cx| {
                probe.provider_call_in(
                    cx,
                    Arc::clone(&provider),
                    Command::FollowSession { session_id: "s-scripted".into(), after: None },
                    |probe, result, _window, _| {
                        probe.seen = Some(match result {
                            Ok(Ack::Accepted) => Ok("followed".to_owned()),
                            Ok(other) => Err(format!("wrong ack: {other:?}")),
                            Err(error) => Err(error.to_string()),
                        });
                    },
                );
            });
        });
        vc.run_until_parked();

        let seen = vc.update(|_, cx| probe.read(cx).seen.clone());
        assert_eq!(seen, Some(Ok("followed".to_owned())));
    }

    /// The gate refuses through the same shape: an `Unavailable`
    /// capability comes back as the typed refusal, never `Ok`.
    #[gpui::test]
    fn a_refusal_comes_back_through_the_same_shape(cx: &mut gpui::TestAppContext) {
        let mut provider = Provider::new(ScriptedProvider::new());
        provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("connect");
        let provider = Arc::new(Mutex::new(provider));

        let vc = cx.add_empty_window();
        let probe = vc.update(|_, cx| cx.new(|_| Probe { tasks: Vec::new(), seen: None }));
        vc.update(|_, cx| {
            probe.update(cx, |probe, cx| {
                probe.provider_call(
                    cx,
                    Arc::clone(&provider),
                    Command::ForkSession {
                        request_id: "r-9".into(),
                        session_id: "s".into(),
                        through_turn: None,
                        metadata_only: false,
                    },
                    |probe, result, _| {
                        probe.seen = Some(match result {
                            Err(error) if error.is_unsupported() => {
                                Ok(error.capability().unwrap_or("?").into())
                            }
                            Err(error) => Err(error.to_string()),
                            Ok(ack) => Err(format!("must refuse, not answer {ack:?}")),
                        });
                    },
                );
            });
        });
        vc.run_until_parked();

        let seen = vc.update(|_, cx| probe.read(cx).seen.clone());
        assert_eq!(seen, Some(Ok("fork-session".to_owned())));
    }
}
