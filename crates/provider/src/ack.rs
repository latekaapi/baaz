//! What comes back from a command: identity, status, and the query results
//! a transcript delta cannot carry.
//!
//! The render direction is already neutral ([`aui_protocol::Delta`]), so the
//! acks stay small: ids, labels, and counts. Full transcripts arrive as
//! deltas on [`crate::ProviderEvent`]; query results ride here because they
//! answer the caller's question synchronously.

use aui_protocol::Delta;

/// One row of a session listing: identity plus the display title, when the
/// provider could derive one. Never fabricated — absent stays absent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    /// The session identity.
    pub session_id: String,
    /// Display title, when derivable.
    pub title: Option<String>,
}

/// One row of the model catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSummary {
    /// Catalog model id.
    pub id: String,
    /// Presentation label.
    pub label: String,
    /// Whether this row is the named session's effective model.
    pub active: bool,
}

/// One pending approval, pointed at — not the full card, which arrives as a
/// delta. Enough to find it and decide it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingApproval {
    /// The approval's id — what [`crate::Command::DecideApproval`] names.
    pub id: String,
    /// The owning session.
    pub session_id: String,
    /// One-line human summary of what is being approved.
    pub headline: String,
}

/// One pending question, pointed at — the full prompt arrives as a delta.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    /// The prompt's id — what the question commands name.
    pub id: String,
    /// The owning session.
    pub session_id: String,
    /// One-line human summary of what is being asked.
    pub headline: String,
}

/// What a command returned.
///
/// `PartialEq` but not `Eq`: [`Delta`] is only partially comparable.
#[derive(Clone, Debug, PartialEq)]
pub enum Ack {
    /// Admitted; the outcome arrives as events. Ack is admission, never
    /// outcome — view events for a command can arrive before its ack.
    Accepted,
    /// A session was opened, resumed, forked, or read.
    Session {
        /// The session identity.
        session_id: String,
        /// Display title, when the provider could derive one.
        title: Option<String>,
    },
    /// A page of the stored-session index.
    SessionIndex {
        /// The page, newest activity first.
        sessions: Vec<SessionSummary>,
        /// The next page's cursor; absent on the last page.
        next_cursor: Option<String>,
    },
    /// Input was admitted; this turn carries it. Authoritative — never
    /// derive the turn locally.
    TurnAccepted {
        /// The turn that will carry (or absorbed) the input.
        turn_id: String,
    },
    /// The model catalog snapshot.
    ModelCatalog {
        /// Visible rows, provider order.
        models: Vec<ModelSummary>,
        /// The catalog's provider.
        provider: String,
    },
    /// A session's pending set, point-in-time.
    PendingWork {
        /// Pending approvals, oldest first.
        approvals: Vec<PendingApproval>,
        /// Pending questions, oldest first.
        questions: Vec<PendingQuestion>,
    },
    /// The credential lane in effect.
    Account {
        /// False exactly when the endpoint needs no caller credential.
        signed_in: bool,
        /// Display label for the credential in effect, when the provider
        /// sends one. Never key material.
        label: Option<String>,
    },
    /// A device-code flow started: the user completes it in a browser.
    /// Both fields are absent for the synchronous key-storing flow.
    LoginChallenge {
        /// Where the user signs in.
        verification_url: Option<String>,
        /// The code the user confirms or enters there.
        user_code: Option<String>,
    },
    /// A pending login flow was abandoned — or there was none.
    LoginCancelled {
        /// True iff a pending flow existed and was cancelled.
        cancelled: bool,
    },
    /// One page of transcript, already folded to render-ready deltas.
    TranscriptPage {
        /// The page's deltas, ascending.
        deltas: Vec<Delta>,
        /// The anchor for the next page in the same direction; absent at
        /// the end of the transcript in that direction.
        next_cursor: Option<String>,
    },
    /// One range of a tool call's stored output.
    StoredOutput {
        /// The served range, decoded.
        content: String,
        /// True when this range reached the end of the stored output.
        complete: bool,
    },
}
