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
        // Sign-in is on the wire now (D22): `account/*` is experimental, so
        // the opt-in is required — without it every account method answers
        // `-32601` / `experimentalRequired`. `userShell` is the `!` escape
        // hatch, unchanged.
        ClientCapabilities {
            experimental_api: Some(true),
            requested_capabilities: Some(vec!["userShell".into()]),
            ..Default::default()
        },
    )?;
    let (tx, rx) = unbounded();
    let _ = forward(events, tx);
    Ok((Connection { client: Arc::new(client), server, warning }, rx))
}

/// Move every event from the client's crossbeam receiver onto a futures channel
/// a gpui task can await. The thread ends when the client's sender is dropped,
/// which happens when the client is (finding `support-13`: no join handle is
/// kept for shutdown ordering, which is the accepted design — this only
/// names each bridge thread distinctly, so a stack of them from repeated
/// reconnects is not one unlabelled thread repeated).
fn forward(
    events: crossbeam_channel::Receiver<MuseEvent>,
    tx: UnboundedSender<MuseEvent>,
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

/// Whether a failed `session/resume` is about *this session* rather than the
/// transport.
///
/// A session-scoped rejection leaves the wire `Ready`: the child is alive
/// and the handshake passed, so the sidebar, the palette and ⌘N keep
/// working while the rejection becomes a banner on that session's view.
/// Anything else — a dead child, an unanswered request, an unparseable frame,
/// or a server-level kind — is a transport failure and takes the wire down.
///
/// Decided per [`ErrorKind`]:
///
/// * session-scoped: every `session*` kind (`sessionInUse`, `sessionNotFound`,
///   `sessionAmbiguous`, `sessionNotLoaded`, `sessionStreamMismatch`,
///   `forkBoundaryInvalid`), `commandRejected` (in a resume it names this
///   session's state, e.g. a conflicting id), the approval/userInput kinds
///   (they name a prompt or approval, never the pipe), generic `notFound`
///   (an anchor or session the host no longer has), and the race artifacts
///   `cancelled`/`interrupted`/`noBoundary`;
/// * transport-level: everything `MuseError` carries without a kind
///   (`Closed`, `Timeout`, `Protocol`, `Io`), the handshake and framing kinds
///   (`parseError`, `invalidRequest`, `notInitialized`, `alreadyInitialized`,
///   `methodNotFound`, `invalidParams`, `experimentalRequired`), the
///   server-state kinds (`internal`, `overloaded`, `backpressured`,
///   `capabilityRequired`, `inputTooLarge`, `pageEventTooLarge`,
///   `outputResultTooLarge`, `outputUnavailable`, `viewTruncated`,
///   `boundaryPruned`, `boundaryUnusable`).
pub fn is_session_scoped(error: &MuseError) -> bool {
    match error {
        MuseError::Closed | MuseError::Timeout(_) | MuseError::Protocol(_) | MuseError::Io(_) => false,
        MuseError::Json(_) => false,
        MuseError::Rpc(_) => matches!(
            error.kind(),
            Some(
                ErrorKind::SessionInUse
                    | ErrorKind::SessionNotFound
                    | ErrorKind::SessionAmbiguous
                    | ErrorKind::SessionNotLoaded
                    | ErrorKind::SessionStreamMismatch
                    | ErrorKind::ForkBoundaryInvalid
                    | ErrorKind::CommandRejected
                    | ErrorKind::ApprovalNotFound
                    | ErrorKind::ApprovalAlreadyResolved
                    | ErrorKind::ApprovalChoiceInvalid
                    | ErrorKind::ApprovalRequirementStale
                    | ErrorKind::ApprovalReviewerUnavailable
                    | ErrorKind::UserInputNotFound
                    | ErrorKind::UserInputAlreadySettled
                    | ErrorKind::UserInputAnswerInvalid
                    | ErrorKind::NotFound
                    | ErrorKind::Cancelled
                    | ErrorKind::Interrupted
                    | ErrorKind::NoBoundary,
            )
        ),
    }
}

/// The banner a session-scoped resume rejection becomes on that session's
/// view.
pub fn lease_banner(error: &MuseError) -> String {
    if matches!(error.kind(), Some(ErrorKind::SessionInUse)) {
        return "This session is open in another window. Close it there, or start a new session.".to_owned();
    }
    format!("{}. {error}", title(error))
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

    fn rpc_error(kind: &str, code: i32, message: &str) -> MuseError {
        let object: muse_client::schema::ErrorObject = serde_json::from_value(serde_json::json!({
            "code": code,
            "message": message,
            "data": {"kind": kind},
        }))
        .expect("error object decodes");
        MuseError::Rpc(Box::new(object))
    }

    /// A resume rejection about the session banners the
    /// view and keeps the wire up; a dead child or a server-level kind takes
    /// the wire down.
    #[test]
    fn a_session_scoped_resume_rejection_never_takes_the_wire_down() {
        // The observed `-32021`: another host holds the lease.
        let in_use = rpc_error("sessionInUse", -32021, "session s1 is already in use");
        assert!(is_session_scoped(&in_use));
        assert_eq!(
            lease_banner(&in_use),
            "This session is open in another window. Close it there, or start a new session."
        );
        // The other session-scoped kinds banner too, titled by kind.
        for kind in [
            "sessionNotFound",
            "sessionAmbiguous",
            "sessionNotLoaded",
            "sessionStreamMismatch",
            "forkBoundaryInvalid",
            "commandRejected",
            "approvalAlreadyResolved",
            "userInputAlreadySettled",
            "notFound",
            "cancelled",
        ] {
            let error = rpc_error(kind, -32000, "resume failed");
            assert!(is_session_scoped(&error), "{kind} should banner, not drop the wire");
        }
        // Transport failures and server-level kinds are not session-scoped.
        assert!(!is_session_scoped(&MuseError::Closed));
        assert!(!is_session_scoped(&MuseError::Timeout("session/resume".into())));
        for kind in ["internal", "overloaded", "notInitialized", "invalidParams", "methodNotFound"] {
            assert!(!is_session_scoped(&rpc_error(kind, -32603, "resume failed")), "{kind} drops the wire");
        }
    }

    #[test]
    fn only_auth_shaped_turn_failures_route_to_the_login_screen() {
        assert!(looks_like_signed_out(Some("modelError"), "meta is not authenticated"));
        assert!(looks_like_signed_out(Some("configError"), "No credential for provider meta"));
        assert!(!looks_like_signed_out(Some("modelError"), "the model timed out"));
        // The right words but the wrong kind is still not an auth failure.
        assert!(!looks_like_signed_out(Some("stepLimit"), "not authenticated"));
    }

    /// **support-13 / A-MECH-19.** The bridge thread has no join handle kept
    /// for shutdown ordering by design (nothing here changes that), but it
    /// must still actually exit once the client's sender side is dropped —
    /// which is the whole reason that design is safe.
    #[test]
    fn the_bridge_thread_exits_when_its_sender_is_dropped() {
        use futures::StreamExt;
        let (events_tx, events_rx) = crossbeam_channel::unbounded::<MuseEvent>();
        let (tx, mut rx) = unbounded();
        let handle = forward(events_rx, tx).expect("spawn the bridge thread");
        assert!(handle.thread().name().is_some_and(|name| name.starts_with("muse-bridge-")));

        events_tx.send(MuseEvent::Closed(None)).expect("send while the bridge is alive");
        assert_eq!(futures::executor::block_on(rx.next()), Some(MuseEvent::Closed(None)));

        // Dropping every sender is what `MuseClient::shutdown`/`Drop` does;
        // the bridge's `recv()` then returns `Err` and the loop ends.
        drop(events_tx);
        handle.join().expect("the bridge thread panicked instead of exiting");
    }
}
