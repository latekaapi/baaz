//! The `muse serve` connection and how its events reach the UI thread.
//!
//! [`connect`] spawns the child and shakes hands through
//! [`provider_muse::establish`], and bridges the hub's connection stream onto
//! a `futures` channel a gpui foreground task can await: crossbeam receivers
//! are competing consumers no foreground task can await, so one small
//! forwarding thread per stream moves every event across. Sessions ride the
//! hub as [`Provider`] lanes, each behind the enforced gate.
//!
//! Commands are the other direction and they *do* block (a request waits up to
//! three minutes), so every one of them runs on gpui's background executor and
//! comes back to the entity through `update`. The UI thread issues intents; it
//! never waits on the wire.
//!
//! **Durable, always** — the spawn must never set `--no-session-log`: it
//! accepts a turn and then emits no view events at all
//! (`docs/01-transport.md` §4), so an app that wants a transcript may never
//! set it. That flag lives in [`provider_muse::spawn`].

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use provider::{ConnectInfo, Provider, ProviderError, ProviderEvent};
#[cfg(test)]
use provider::ProviderAdapter;

/// What `clientInfo.name` reports. Must match `[a-z0-9_]+`.
pub const CLIENT_NAME: &str = "baaz";

/// The capabilities the app requests at the handshake. `userShell` is the
/// `!` escape hatch; `sessionMcp` is the terminal relay's route (T2: the
/// bridge rides `session/start` and `session/resume` only when the server
/// grants it — muse ≥ 1.3 does, older servers silently keep the old
/// behaviour). The experimental opt-in the account lane needs is set
/// inside `provider-muse`.
pub const REQUESTED_CAPABILITIES: &[&str] = &["userShell", "sessionMcp"];

/// A live connection: the hub and its connection stream.
pub struct Connected {
    /// The one `muse serve` child as a hub: a [`Provider`] lane per
    /// session, every command through the lane's gate.
    pub hub: provider_muse::MuseHub,
    /// The hub's connection stream (row facts, account and usage,
    /// connection loss), bridged onto a channel a gpui task can await.
    pub events: UnboundedReceiver<ProviderEvent>,
    /// Who answered: the agent's own name for itself.
    pub agent_name: String,
    /// Who answered: the agent's version string.
    pub agent_version: String,
    /// The schema-fingerprint warning, pre-formatted for the log. A
    /// mismatch is additive evolution, never a failure: log it and carry
    /// on. `None` when the fingerprints agreed.
    pub warning: Option<String>,
    /// Whether `initialize` granted `userShell`.
    pub user_shell: bool,
    /// Whether `initialize` granted `sessionMcp` (muse ≥ 1.3): the
    /// terminal relay's route. `false` means no route — sessions open
    /// exactly as they always did.
    pub session_mcp: bool,
}

/// A one-line title for a connection failure, for the reconnect dialog.
///
/// A refusal carries its own title; anything else keeps only the shape of
/// the failure: gone versus refused.
pub fn provider_title(error: &ProviderError) -> String {
    match error {
        ProviderError::Unavailable { .. } => "Muse disconnected".into(),
        ProviderError::Refused(refusal) => refusal.title.clone(),
        ProviderError::Rejected { .. } | ProviderError::Unsupported { .. } => {
            "Something went wrong".into()
        }
    }
}

/// Shake hands as this client. Used by [`connect`] and by tests driving a
/// non-muse adapter.
pub fn connect_info() -> ConnectInfo {
    ConnectInfo {
        client_name: CLIENT_NAME.to_owned(),
        client_version: env!("CARGO_PKG_VERSION").to_owned(),
        capabilities: REQUESTED_CAPABILITIES.iter().map(|c| c.to_string()).collect(),
    }
}

/// Spawn `muse serve`, `initialize`, and bridge the connection stream.
///
/// Blocking: run it on the background executor.
pub fn connect(program: &str) -> Result<Connected, ProviderError> {
    let established = provider_muse::establish(program, &connect_info())?;
    let events = bridge_events(established.hub.connection_events());
    Ok(Connected {
        hub: established.hub,
        events,
        agent_name: established.handshake.agent_name,
        agent_version: established.handshake.agent_version,
        warning: established.warning,
        user_shell: established.user_shell,
        session_mcp: established.session_mcp,
    })
}

/// Drive the connection path with any adapter: wrap it behind the
/// [`Provider`] gate, shake hands, and bridge its event stream. Test-only:
/// production connects through [`connect`], whose adapter is already
/// connected inside [`provider_muse::establish`]. Here the adapter is
/// [`ScriptedProvider`](provider::scripted::ScriptedProvider) or any other
/// non-muse implementation. What the test exercises is what the app holds:
/// gated dispatch plus the bridged event stream.
///
/// Blocking: run it on the background executor.
#[cfg(test)]
fn connect_with(
    adapter: impl ProviderAdapter + 'static,
    client: &ConnectInfo,
) -> Result<(Provider, UnboundedReceiver<ProviderEvent>), ProviderError> {
    let mut provider = Provider::new(adapter);
    provider.connect(client)?;
    Ok(gate(provider))
}

/// Move an adapter's event stream behind the gate's bridge: one forwarding
/// thread onto a futures channel a gpui task can await. Shared with the
/// provider-lane open path, whose freshly connected provider needs the same
/// bridge before its view starts draining.
pub(crate) fn gate(provider: Provider) -> (Provider, UnboundedReceiver<ProviderEvent>) {
    let events = bridge_events(provider.events());
    (provider, events)
}

/// Test-only bridge: move whatever the provider has already emitted on its
/// competing crossbeam stream into a futures channel, synchronously, with
/// no OS thread. The caller drains *after* the send that produced the
/// events, so everything the lane needs is already buffered before its
/// drain task starts — and nothing wakes a foreground task from a foreign
/// thread inside a `#[gpui::test]` (see `forward`). Production keeps the
/// real bridge thread above; this never runs outside tests.
#[cfg(test)]
pub(crate) fn gate_sync(provider: Provider) -> (Provider, UnboundedReceiver<ProviderEvent>) {
    let (tx, rx) = unbounded();
    for event in provider.events().try_iter() {
        if tx.unbounded_send(event).is_err() {
            break;
        }
    }
    (provider, rx)
}

/// A provider that never speaks on its own (a read-only lane): its stream
/// ends at once, so the view's drain task does too — no forwarding thread.
pub(crate) fn gate_quiet(provider: Provider) -> (Provider, UnboundedReceiver<ProviderEvent>) {
    let (_ended, events) = unbounded();
    (provider, events)
}

/// Move every event from a competing crossbeam receiver onto a futures
/// channel a gpui task can await. The thread ends when the sender side is
/// dropped, which happens when the adapter (or the hub) is.
fn forward<T: Send + 'static>(
    events: crossbeam_channel::Receiver<T>,
    tx: UnboundedSender<T>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    static CONNECTION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = CONNECTION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::thread::Builder::new().name(format!("muse-bridge-{n}")).spawn(move || {
        while let Ok(event) = events.recv() {
            if tx.unbounded_send(event).is_err() {
                break;
            }
        }
    })
}

/// Bridge one competing crossbeam event stream onto a futures channel.
pub(crate) fn bridge_events<T: Send + 'static>(events: crossbeam_channel::Receiver<T>) -> UnboundedReceiver<T> {
    let (tx, rx) = unbounded();
    let _ = forward(events, tx);
    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use provider::scripted::ScriptedProvider;
    use provider::{Ack, Command, SubmissionPart};

    /// support-13 / A-MECH-19, on the generic bridge: the bridge thread has
    /// no join handle kept for shutdown ordering by design (nothing here
    /// changes that), but it must still actually exit once the sender side
    /// is dropped — which is the whole reason that design is safe.
    #[test]
    fn the_bridge_thread_exits_when_its_sender_is_dropped() {
        use futures::StreamExt;
        let (events_tx, events_rx) = crossbeam_channel::unbounded::<ProviderEvent>();
        let (tx, mut rx) = unbounded();
        let handle = forward(events_rx, tx).expect("spawn the bridge thread");
        assert!(handle.thread().name().is_some_and(|name| name.starts_with("muse-bridge-")));

        events_tx
            .send(ProviderEvent::ConnectionLost { reason: "gone".into() })
            .expect("send while the bridge is alive");
        assert!(matches!(
            futures::executor::block_on(rx.next()),
            Some(ProviderEvent::ConnectionLost { .. })
        ));

        // Dropping every sender is what shutdown does; the bridge's
        // `recv()` then returns `Err` and the loop ends.
        drop(events_tx);
        handle.join().expect("the bridge thread panicked instead of exiting");
    }

    /// The C1b seam tripwire: the connection path runs on an adapter that
    /// is not `provider-muse` and knows nothing of MSP. `ScriptedProvider`
    /// answers as Codex through the same [`connect_with`] the app's shape
    /// is built on — gated `send`, bridged events — with no `muse-client`
    /// in reach. If this ever needs the wire crate, the seam is wrong.
    #[test]
    fn the_connection_path_runs_on_a_non_muse_provider() {
        use aui_protocol::{Provider as Backend, Session};

        let (provider, events) = connect_with(
            ScriptedProvider::new(),
            &ConnectInfo::new("baaz", "0.1.0"),
        )
        .expect("a scripted provider connects");
        assert_eq!(provider.id(), Backend::Codex);

        let Ack::Session { session_id, .. } = provider
            .send(Command::OpenSession {
                request_id: "r-1".into(),
                workspace: None,
                model: None,
                model_provider: None,
                mode: None,
            })
            .expect("open")
        else {
            panic!("open-session must ack a session");
        };

        let Ack::TurnAccepted { .. } = provider
            .send(Command::SubmitInput {
                request_id: "r-2".into(),
                session_id: session_id.clone(),
                parts: vec![SubmissionPart::Text("hello".into())],
                display_text: None,
                effort: None,
            })
            .expect("submit")
        else {
            panic!("submit-input must ack a turn");
        };

        // The scripted turn arrives over the bridged stream and folds into
        // a session, end to end through the connection path. The scripted
        // provider emits synchronously inside `send`, but the bridge thread
        // forwards asynchronously, so drain with a deadline rather than a
        // single poll.
        let mut bridged = events;
        let mut session = Session::new(&session_id, Backend::Codex, "scripted", "/tmp");
        let mut applied = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while applied < 4 && std::time::Instant::now() < deadline {
            match bridged.try_recv() {
                Ok(event) => {
                    let ProviderEvent::Deltas { deltas, .. } = event else {
                        panic!("scripted providers emit only deltas");
                    };
                    for delta in deltas {
                        assert!(session.apply(delta), "every canned delta must land");
                        applied += 1;
                    }
                }
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
        assert_eq!(applied, 4, "the whole script must arrive over the bridge");
    }

    /// The gate holds on the connection path too: an `Unavailable`
    /// capability refuses before any adapter code runs, whoever the adapter
    /// is.
    #[test]
    fn the_connection_path_refuses_what_the_provider_cannot_do() {
        let (provider, _events) =
            connect_with(ScriptedProvider::new(), &ConnectInfo::new("baaz", "0.1.0"))
                .expect("a scripted provider connects");
        match provider.send(Command::ForkSession {
            request_id: "r-9".into(),
            session_id: "s".into(),
            through_turn: None,
            metadata_only: false,
            mode: None,
        }) {
            Err(provider::ProviderError::Unsupported { capability, .. }) => {
                assert_eq!(capability, "fork-session");
            }
            other => panic!("fork-session must refuse, not answer {other:?}"),
        }
    }
}
