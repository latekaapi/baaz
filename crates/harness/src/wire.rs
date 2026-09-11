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

use gpui::{AppContext as _, Context, Task, Window};

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
