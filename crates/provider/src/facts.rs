//! What a session is beside its transcript, neutrally named.
//!
//! A [`aui_protocol::Delta`] moves the transcript; these are the facts the
//! transcript has no slot for — the context meter, the queue strip, the
//! retry clock, what is waiting on the person, the model in effect, the
//! position a reconnect resumes from. An adapter that knows a fact reports
//! it; one that does not leaves it at its default, and the UI offers
//! nothing built on it (a provider with no queue never shows a strip).
//!
//! Every type here is plain data with no wire spelling: adapters translate
//! *into* these, exactly as they do for [`crate::Command`].

use std::collections::BTreeMap;

/// How close the context is to its limit, as the provider grades it. The
/// thresholds are the provider's, never derived here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ContextPressure {
    /// Plenty of room.
    #[default]
    Normal,
    /// Getting close: the provider will compact or refuse soon.
    Warning,
    /// Full: the next turn will be refused until the context shrinks.
    Blocked,
}

/// The latest context reading: what is used, out of what, at what pressure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextReading {
    /// Tokens the next request would carry.
    pub used_tokens: u64,
    /// The window those tokens count against; absent when the basis has no
    /// limit, so the meter reads "used, no denominator".
    pub window_tokens: Option<u64>,
    /// The provider's grade of how close that is.
    pub pressure: ContextPressure,
}

/// A session's running token totals. Never goes backward.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenTotals {
    /// Prompt (input) tokens.
    pub prompt_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Everything counted.
    pub total_tokens: u64,
}

/// One submission waiting behind the running turn, in the provider's order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedTurn {
    /// The turn id the queueing acknowledgement minted; what
    /// [`crate::Command::ReclaimQueued`] takes verbatim.
    pub turn_id: String,
    /// The submission's request id, under which its text is remembered.
    pub request_id: String,
}

/// The turn is between attempts: the provider will retry it on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryPlan {
    /// The turn being retried.
    pub turn_id: String,
    /// Which attempt failed.
    pub attempt: u32,
    /// How many attempts the provider allows.
    pub max_attempts: u32,
    /// The backoff before the next attempt. A duration, not a fire time: the
    /// countdown is derived from when this fact arrived.
    pub delay_ms: u64,
    /// The provider's human reason.
    pub reason: String,
}

/// One question inside a raised prompt, as much as answering it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromptQuestion {
    /// The question's id within its prompt.
    pub id: String,
    /// A short topic line; may be empty.
    pub header: String,
    /// Several options may be picked.
    pub multi: bool,
    /// The offered options' labels, in order.
    pub labels: Vec<String>,
}

/// A raised prompt still waiting on the person: one or several questions
/// (several make a wizard, one card at a time).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingPrompt {
    /// The questions, in order.
    pub questions: Vec<PromptQuestion>,
    /// When the provider resolves it on its own if nobody answers. A
    /// duration, not a deadline.
    pub auto_resolve_ms: Option<u64>,
}

/// The model in effect, as the provider reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelInEffect {
    /// Catalog model id.
    pub id: String,
}

/// Everything about a session beside its transcript.
///
/// Defaults mean "not reported": an adapter fills what it knows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionSide {
    /// The latest context reading.
    pub context: Option<ContextReading>,
    /// Running token totals.
    pub totals: TokenTotals,
    /// Submissions queued behind the running turn, in order.
    pub queued: Vec<QueuedTurn>,
    /// The running turn's retry plan, while it is between attempts.
    pub retry: Option<RetryPlan>,
    /// Approvals waiting on a decision: id → the stage token a decision
    /// must echo ([`crate::Command::DecideApproval`]). Opaque.
    pub pending_approvals: BTreeMap<String, String>,
    /// Prompts waiting on an answer, by prompt id.
    pub pending_prompts: BTreeMap<String, PendingPrompt>,
    /// The model in effect.
    pub model: Option<ModelInEffect>,
    /// The last observed transcript position, opaque: what a reconnect
    /// resumes after. Empty when the provider reports none.
    pub cursor: String,
    /// Truncated outputs whose full text the provider stores: item id →
    /// the stored-output reference [`crate::Command::ReadStoredOutput`]
    /// takes.
    pub stored_outputs: BTreeMap<String, String>,
}

/// What a session is beside its turns, as a header: the model, where it
/// runs, the branch. The approval mode has its own event
/// ([`crate::ProviderEvent::ApprovalModeChanged`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionHeader {
    /// The model label or id the provider reports.
    pub model: String,
    /// The working directory.
    pub cwd: String,
    /// The git branch checked out there, when known.
    pub branch: Option<String>,
}

/// How a turn ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnOutcome {
    /// It finished.
    Completed,
    /// The person (or a newer turn) stopped it.
    Interrupted,
    /// It failed. The transcript already shows the failure card; this is
    /// what the app does about it.
    Failed {
        /// The provider's message, when it gave one.
        message: Option<String>,
        /// The failure means the credential is gone: offer sign-in.
        signed_out: bool,
    },
}
