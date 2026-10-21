//! What comes back from a command: identity, status, and the query results
//! a transcript delta cannot carry.
//!
//! The render direction is already neutral ([`aui_protocol::Delta`]), so the
//! acks stay small: ids, labels, and counts. Full transcripts arrive as
//! deltas on [`crate::ProviderEvent`]; query results ride here because they
//! answer the caller's question synchronously.

use aui_protocol::{Delta, PermissionMode};

/// One row of a session listing: what a sidebar row is built from. Never
/// fabricated — absent stays absent, and a provider that knows only the id
/// leaves the rest at its default.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSummary {
    /// The session identity.
    pub session_id: String,
    /// Display title, when derivable.
    pub title: Option<String>,
    /// The name someone gave the session in the provider, when it keeps one.
    pub name: Option<String>,
    /// The first thing the person sent, when the provider reports it (a
    /// preview: the head is kept, the tail may be cut).
    pub first_prompt: Option<String>,
    /// The workspace root the session ran in.
    pub workspace: Option<String>,
    /// The workspace's branch, when the provider derives one.
    pub branch: Option<String>,
    /// When it was created, RFC 3339.
    pub created_at: String,
    /// When its content last moved, RFC 3339; absent before any content.
    pub last_activity_at: Option<String>,
    /// When the provider last touched its record, RFC 3339 — what an
    /// incremental listing ([`crate::Command::ListSessions`]'s
    /// `updated_after`) filters on.
    pub updated_at: String,
    /// A turn is running.
    pub running: bool,
    /// Exchanges so far.
    pub turns: u64,
    /// What is waiting on the person.
    pub attention: Vec<crate::facts::Attention>,
}

/// One row of the model catalog.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ModelSummary {
    /// Catalog model id.
    pub id: String,
    /// Presentation label.
    pub label: String,
    /// Whether this row is the named session's effective model.
    pub active: bool,
    /// Reasoning levels this model accepts, in provider order, as neutral
    /// level ids (`"low"`, `"medium"`, …). Empty when the provider names
    /// none — a model with no row, or no levels, offers no control.
    pub efforts: Vec<String>,
    /// A hidden row: the menu lists it only when it is the active one.
    pub hidden: bool,
    /// The provider's default row, when it names one.
    pub is_default: bool,
    /// The provider's one-line description, when it sends one.
    pub description: Option<String>,
    /// Approval modes the provider says this model cannot run in, so the
    /// mode picker can say so before a switch is refused. Empty when the
    /// provider names no limit — absent is never read as unsupported.
    pub unavailable_modes: Vec<PermissionMode>,
    /// The level the model runs at when none is picked, when the provider
    /// says.
    pub default_effort: Option<String>,
    /// The provider's own words for each level it describes: `(level,
    /// description)`.
    pub effort_notes: Vec<(String, String)>,
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
    /// The opaque stage token [`crate::Command::DecideApproval`] must echo
    /// back verbatim. `None` when the backend has no staged approvals.
    pub stage_token: Option<String>,
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

/// Which credential a provider runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AccountLane {
    /// No credential: the sign-in screen.
    LoggedOut,
    /// An API key from the environment.
    EnvKey,
    /// A stored API key.
    ApiKey,
    /// An account sign-in (OAuth / device code).
    AccountLogin,
    /// A lane this build has no name for, as the provider spells it.
    Other(String),
}

impl AccountLane {
    /// Whether a credential is in effect.
    pub fn signed_in(&self) -> bool {
        !matches!(self, AccountLane::LoggedOut)
    }

    /// Whether the credential is an API key (stored or from the
    /// environment).
    pub fn is_api_key(&self) -> bool {
        matches!(self, AccountLane::EnvKey | AccountLane::ApiKey)
    }
}

/// How a sign-in flow ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginOutcome {
    /// Signed in.
    Granted,
    /// The person refused it.
    Denied,
    /// It timed out before anyone approved it.
    Expired,
    /// It was cancelled.
    Cancelled,
    /// It failed (a bad key, a server fault).
    Failed,
    /// An outcome this build has no name for, as the provider spells it.
    Other(String),
}

/// What a command returned.
///
/// `PartialEq` but not `Eq`: [`Delta`] is only partially comparable.
#[derive(Clone, Debug, PartialEq)]
pub enum Ack {
    /// Admitted; the outcome arrives as events. Ack is admission, never
    /// outcome — view events for a command can arrive before its ack.
    Accepted,
    /// The provider took the command and had nothing to do, and says why
    /// (compacting a context with nothing to summarize). Not a failure.
    Noop {
        /// The provider's words.
        reason: String,
    },
    /// A session was opened, resumed, forked, or read.
    Session {
        /// The session identity.
        session_id: String,
        /// Display title, when the provider could derive one.
        title: Option<String>,
    },
    /// A stored session, read: who it is, and the first shell command in
    /// its history — a label for a session with nothing else to go by.
    SessionRead {
        /// The session identity.
        session_id: String,
        /// Display title, when the provider could derive one.
        title: Option<String>,
        /// The first shell command the person ran, when the history served
        /// holds one.
        first_shell_command: Option<String>,
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
        /// The input waits behind the running turn instead of starting
        /// one: it shows on the queue strip until it runs or is reclaimed
        /// ([`crate::Command::ReclaimQueued`]).
        queued: bool,
    },
    /// The model catalog snapshot.
    ModelCatalog {
        /// Visible rows, provider order.
        models: Vec<ModelSummary>,
        /// Which model vendor within the backend this catalog came from —
        /// not the backend itself (see [`crate::ProviderAdapter::id`]).
        /// The commands spell the same routing `model_provider`.
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
        /// Which credential is in effect.
        lane: AccountLane,
    },
    /// The provider's latest usage reading, `None` when it has observed
    /// nothing yet (a cold host answers before any turn). Never a guess.
    Usage {
        /// The reading.
        report: Option<crate::UsageReport>,
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
