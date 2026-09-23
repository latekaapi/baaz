//! What comes back outside a command: the render stream plus the
//! out-of-band things a delta cannot carry.
//!
//! The render direction is already neutral, so this stays thin: a session
//! id with its [`aui_protocol::Delta`]s, and one variant per thing that is
//! not transcript — a connection ending, an approval raised, a question
//! asked. Small on purpose: anything a delta can say, the delta says.

use aui_protocol::Delta;

/// Something the provider did on its own — not the answer to a command.
#[derive(Clone, Debug, PartialEq)]
pub enum ProviderEvent {
    /// Transcript movement in `session_id`: fold these into the session in
    /// order. `session_id` is absent only for connection-level notices that
    /// belong to no session; a late delta for a gone turn is ignored by the
    /// fold, never an error.
    Deltas {
        /// The session that moved; absent when the notice is
        /// connection-level.
        session_id: Option<String>,
        /// Render-ready updates, in order.
        deltas: Vec<Delta>,
    },
    /// The provider raised an approval: decide it with
    /// [`crate::Command::DecideApproval`]. The full card also arrives as a
    /// delta; this is the tap on the shoulder, carrying only what a
    /// decision needs.
    ApprovalRequested {
        /// The owning session.
        session_id: String,
        /// The approval to decide.
        approval_id: String,
        /// One-line human summary of what is being approved.
        headline: String,
    },
    /// The provider asked a question: settle it with the question commands.
    /// The full prompt also arrives as a delta; this carries only what a
    /// reply needs.
    QuestionRaised {
        /// The owning session.
        session_id: String,
        /// The prompt to settle.
        question_id: String,
        /// One-line human summary of what is being asked.
        headline: String,
    },
    /// The provider is gone: child exited, socket down, heartbeat lost.
    /// Reconnect, then resume from the last observed position.
    ConnectionLost {
        /// What went wrong.
        reason: String,
    },
}
