//! The `muse serve` connection and how its events reach the UI thread.
//!
//! [`connect`] spawns the child and shakes hands through
//! [`provider_muse::establish`], then moves the adapter behind the enforced
//! [`Provider`] gate and bridges both event streams onto `futures` channels
//! a gpui foreground task can await: crossbeam receivers are competing
//! consumers no foreground task can await, so one small forwarding thread
//! per stream moves every event across.
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
//!
//! The [`Legacy`] bundle is transitional: session views still ride the raw
//! transport until they move onto [`Command`](provider::Command) one lane at
//! a time. It carries the one spawned child, its raw event stream, and the
//! handshake facts the app logs — never a second spawn. The follow-up that
//! moves the last view removes it, and with it the last `muse_client`
//! mentions in this file.

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use provider::{ConnectInfo, Provider, ProviderError, ProviderEvent};
#[cfg(test)]
use provider::ProviderAdapter;

// Transitional re-export: the MSP failure classifiers live in
// `provider-muse` (every branch is a wire spelling) while session views
// still speak the wire error.
pub use provider_muse::errors::{
    is_session_scoped, lease_banner, looks_like_signed_out, severity, title, Severity,
};

/// What `clientInfo.name` reports. Must match `[a-z0-9_]+`.
pub const CLIENT_NAME: &str = "baaz";

/// The capabilities the app requests at the handshake. `userShell` is the
/// `!` escape hatch; the experimental opt-in the account lane needs is set
/// inside `provider-muse`.
pub const REQUESTED_CAPABILITIES: &[&str] = &["userShell"];

/// A live connection: the provider behind its gate, the provider events,
/// and the transitional legacy bundle sharing the same child.
pub struct Connected {
    /// The one way to talk to the provider: the adapter behind the
    /// enforced capability gate. Every command goes through
    /// [`Provider::send`]; the raw dispatch is unreachable.
    pub provider: Provider,
    /// Provider events, bridged onto a channel a gpui task can await.
    pub events: UnboundedReceiver<ProviderEvent>,
    /// The transitional legacy bundle: same child, raw stream, handshake
    /// facts. Gone with the last legacy view.
    pub legacy: Legacy,
}

/// Transitional: what the app's legacy session views still need from the
/// one spawned child. Same child as [`Connected::provider`] — never a
/// second spawn.
pub struct Legacy {
    /// The spawned child, shared with the adapter.
    pub transport: provider_muse::SharedTransport,
    /// The raw transport events, bridged onto a channel a gpui task can
    /// await. Feeds the unchanged event pump and route.
    pub events: UnboundedReceiver<muse_client::MuseEvent>,
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
}

/// A one-line title for a connection failure, for the reconnect dialog.
///
/// Coarse by design: the neutral error carries no wire kind, so the
/// per-kind titles stay on the legacy path (see [`title`]) until the views
/// move. Only the shape of the failure survives: gone versus refused.
pub fn provider_title(error: &ProviderError) -> String {
    match error {
        ProviderError::Unavailable { .. } => "Muse disconnected".into(),
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

/// Spawn `muse serve`, `initialize`, and start both event bridges.
///
/// Blocking: run it on the background executor.
pub fn connect(program: &str) -> Result<Connected, ProviderError> {
    let established = provider_muse::establish(program, &connect_info())?;
    let legacy_events = bridge_events(established.legacy);
    let (provider, events) = gate(Provider::new(established.adapter));
    Ok(Connected {
        provider,
        events,
        legacy: Legacy {
            transport: established.transport,
            events: legacy_events,
            agent_name: established.handshake.agent_name,
            agent_version: established.handshake.agent_version,
            warning: established.warning,
            user_shell: established.user_shell,
        },
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
/// thread onto a futures channel a gpui task can await.
fn gate(provider: Provider) -> (Provider, UnboundedReceiver<ProviderEvent>) {
    let events = bridge_events(provider.events());
    (provider, events)
}

/// Move every event from a competing crossbeam receiver onto a futures
/// channel a gpui task can await. The thread ends when the sender side is
/// dropped, which happens when the adapter (and, for the legacy stream, the
/// transport) is.
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
fn bridge_events<T: Send + 'static>(events: crossbeam_channel::Receiver<T>) -> UnboundedReceiver<T> {
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
        }) {
            Err(provider::ProviderError::Unsupported { capability, .. }) => {
                assert_eq!(capability, "fork-session");
            }
            other => panic!("fork-session must refuse, not answer {other:?}"),
        }
    }
}
