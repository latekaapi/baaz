//! The `muse serve` connection and how its events reach the UI thread.
//!
//! One [`MuseClient`] per process. It already owns a reader and a writer thread,
//! so nothing here blocks the pipe; what this module adds is the **bridge**:
//! `MuseClient::events()` is a `crossbeam` receiver, which a gpui foreground
//! task cannot await, so one small forwarding thread moves every event onto a
//! `futures` channel that [`crate::app::Harness`] drains with `cx.spawn`.
//!
//! Commands are the other direction and they *do* block (a request waits up to
//! three minutes), so every one of them runs on gpui's background executor and
//! comes back to the entity through `update`. The UI thread issues intents; it
//! never waits on the wire.

use std::sync::Arc;

use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use muse_client::schema::{ClientCapabilities, ErrorKind, InitializeResult};
use muse_client::{MuseClient, MuseConfig, MuseError, MuseEvent, SchemaWarning};

/// What `clientInfo.name` reports. Must match `[a-z0-9_]+`.
pub const CLIENT_NAME: &str = "harness";

/// A live connection: the client and the handshake it came back with.
pub struct Connection {
    /// The child. Shared with every background command task.
    pub client: Arc<MuseClient>,
    /// What the server said at `initialize`.
    pub server: InitializeResult,
    /// A schema-fingerprint mismatch, which is a warning and never a failure.
    pub warning: Option<SchemaWarning>,
}

/// Spawn `muse serve`, `initialize`, and start the event bridge.
///
/// **Durable, always.** Phase 1 established that `--no-session-log` accepts a
/// turn and then emits no view events at all (`docs/01-transport.md` §4), so an
/// app that wants a transcript may never set it.
///
/// Blocking: run it on the background executor.
pub fn connect(program: &str) -> Result<(Connection, UnboundedReceiver<MuseEvent>), MuseError> {
    let client = MuseClient::spawn(&MuseConfig {
        program: program.into(),
        trust_workspace: true,
        no_session_log: false,
        extra_args: Vec::new(),
    })?;
    let events = client.events();
    let (server, warning) = client.initialize(
        CLIENT_NAME,
        env!("CARGO_PKG_VERSION"),
        ClientCapabilities { requested_capabilities: Some(vec!["userShell".into()]), ..Default::default() },
    )?;
    let (tx, rx) = unbounded();
    forward(events, tx);
    Ok((Connection { client: Arc::new(client), server, warning }, rx))
}

/// Move every event from the client's crossbeam receiver onto a futures channel
/// a gpui task can await. The thread ends when the client's sender is dropped,
/// which happens when the client is.
fn forward(events: crossbeam_channel::Receiver<MuseEvent>, tx: UnboundedSender<MuseEvent>) {
    let _ = std::thread::Builder::new().name("muse-bridge".into()).spawn(move || {
        while let Ok(event) = events.recv() {
            if tx.unbounded_send(event).is_err() {
                break;
            }
        }
    });
}

/// Where a failed command belongs on screen (spec §3.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// An inline banner over the composer with a dismiss.
    Banner,
    /// A modal dialog with one primary action.
    Dialog,
}

/// Classify a transport error into the place it is shown.
///
/// Branch on `error.data.kind`, never on the message — the wire contract
/// refuses to make the message a branch point. Codes with no `data` (a timeout,
/// a dead child, an unparseable frame) are identity-level and get the dialog.
pub fn severity(error: &MuseError) -> Severity {
    match error {
        MuseError::Rpc(_) => match error.kind() {
            Some(
                ErrorKind::SessionInUse
                | ErrorKind::SessionNotFound
                | ErrorKind::SessionAmbiguous
                | ErrorKind::ParseError
                | ErrorKind::NotInitialized
                | ErrorKind::ExperimentalRequired
                | ErrorKind::Internal
                | ErrorKind::Overloaded,
            ) => Severity::Dialog,
            _ => Severity::Banner,
        },
        MuseError::Closed | MuseError::Timeout(_) | MuseError::Protocol(_) | MuseError::Io(_) => Severity::Dialog,
        MuseError::Json(_) => Severity::Banner,
    }
}

/// A one-line title for an error, for a banner or a dialog heading.
pub fn title(error: &MuseError) -> String {
    match error.kind() {
        Some(ErrorKind::SessionInUse) => "Session already in use".into(),
        Some(ErrorKind::SessionNotFound) => "Session not found".into(),
        Some(ErrorKind::SessionAmbiguous) => "That session id is ambiguous".into(),
        Some(ErrorKind::ParseError) => "Muse sent something this build cannot read".into(),
        Some(ErrorKind::NotInitialized) => "Muse is not ready yet".into(),
        Some(ErrorKind::Internal) => "Muse hit an internal error".into(),
        Some(ErrorKind::Overloaded) => "Muse is overloaded".into(),
        Some(ErrorKind::Backpressured) => "Muse is busy".into(),
        Some(ErrorKind::CommandRejected) => "Muse refused the command".into(),
        _ => match error {
            MuseError::Closed => "Muse disconnected".into(),
            MuseError::Timeout(method) => format!("Muse did not answer {method}"),
            _ => "Something went wrong".into(),
        },
    }
}

/// Whether a turn failure looks like a credential problem and should route to
/// the login screen rather than an inline error (spec §3.2).
///
/// There is no auth error kind on the wire at all — MSP's `ErrorKind` has no
/// `unauthenticated` — so the only signal is the failure text the binary
/// carries (`meta is not authenticated`) or a `configError` that reads like one.
pub fn looks_like_signed_out(kind: Option<&str>, message: &str) -> bool {
    let message = message.to_lowercase();
    let phrase = message.contains("not authenticated")
        || message.contains("not signed in")
        || message.contains("unauthorized")
        || message.contains("no credential");
    phrase && matches!(kind, None | Some("modelError") | Some("configError") | Some("environmentError"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dead_child_is_a_dialog_and_a_timeout_names_its_method() {
        assert_eq!(severity(&MuseError::Closed), Severity::Dialog);
        assert_eq!(title(&MuseError::Timeout("turn/start".into())), "Muse did not answer turn/start");
    }

    #[test]
    fn only_auth_shaped_turn_failures_route_to_the_login_screen() {
        assert!(looks_like_signed_out(Some("modelError"), "meta is not authenticated"));
        assert!(looks_like_signed_out(Some("configError"), "No credential for provider meta"));
        assert!(!looks_like_signed_out(Some("modelError"), "the model timed out"));
        // The right words but the wrong kind is still not an auth failure.
        assert!(!looks_like_signed_out(Some("stepLimit"), "not authenticated"));
    }
}
