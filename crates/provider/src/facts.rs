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

impl ContextReading {
    /// A reading from a provider that reports the numbers but grades
    /// nothing (Claude Code's `modelUsage`, Codex's `tokenUsage`; docs/82
    /// 5.5): Warning from nine tenths of the window, never Blocked — only
    /// a provider that refuses a full context says when it will.
    pub fn measured(used_tokens: u64, window_tokens: Option<u64>) -> Self {
        let window_tokens = window_tokens.filter(|window| *window > 0);
        let pressure = match window_tokens {
            Some(window) if used_tokens.saturating_mul(10) >= window.saturating_mul(9) => ContextPressure::Warning,
            _ => ContextPressure::Normal,
        };
        Self { used_tokens, window_tokens, pressure }
    }
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

/// What is waiting on the person in a session that is not open: a row's
/// needs-you state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Attention {
    /// An approval waits.
    ApprovalPending,
    /// A question waits.
    InputPending,
    /// Something this build has no name for, as the provider spells it.
    /// Never counted as either of the above.
    Other(String),
}

impl Attention {
    /// From its name (`approvalPending`, `inputPending`, anything else).
    pub fn parse(name: &str) -> Self {
        match name {
            "approvalPending" => Self::ApprovalPending,
            "inputPending" => Self::InputPending,
            other => Self::Other(other.to_owned()),
        }
    }

    /// Its name: what [`Self::parse`] reads back.
    pub fn name(&self) -> &str {
        match self {
            Self::ApprovalPending => "approvalPending",
            Self::InputPending => "inputPending",
            Self::Other(name) => name,
        }
    }
}

/// The text a transcript gap marker carries when filling history back in
/// stopped short; what follows it is the reason. (The other gap marker —
/// history the provider no longer holds — carries its own words.)
pub const BACKFILL_ABORTED_PREFIX: &str = "Backfill did not finish: ";

/// One skill a provider offers as a `/name` command in its sessions
/// ([`crate::ProviderEvent::SkillsListed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSkill {
    /// The name the command uses, without the slash.
    pub name: String,
    /// One line on what it does, when the provider says.
    pub description: Option<String>,
}

/// The one row a compaction shows, on every provider (docs/85 10.4):
/// "Context compacted · 29k → 1.9k" — the context before and, when the
/// provider says it, after.
pub fn compaction_text(before: u64, after: Option<u64>) -> String {
    fn short(n: u64) -> String {
        if n >= 1_000_000 {
            format!("{:.1}M", n as f64 / 1_000_000.0)
        } else if n >= 10_000 {
            format!("{}k", (n + 500) / 1000)
        } else if n >= 1000 {
            format!("{:.1}k", n as f64 / 1000.0)
        } else {
            n.to_string()
        }
    }
    match after {
        Some(after) => format!("Context compacted \u{b7} {} \u{2192} {}", short(before), short(after)),
        None => format!("Context compacted \u{b7} {} before", short(before)),
    }
}

#[cfg(test)]
mod compaction_text_tests {
    #[test]
    fn sizes_read_short() {
        assert_eq!(super::compaction_text(29_177, Some(1_867)), "Context compacted \u{b7} 29k \u{2192} 1.9k");
        assert_eq!(super::compaction_text(412_000, Some(38_000)), "Context compacted \u{b7} 412k \u{2192} 38k");
        assert_eq!(super::compaction_text(1_200_000, None), "Context compacted \u{b7} 1.2M before");
        assert_eq!(super::compaction_text(512, Some(90)), "Context compacted \u{b7} 512 \u{2192} 90");
    }
}
