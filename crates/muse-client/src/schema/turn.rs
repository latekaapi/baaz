//! The `turn/*` lane (SS3.2-3.6): starting, steering and interrupting a
//! turn, plus the token, todo and trace records a turn produces.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

/// `userMessage` image attachment metadata (tdd SS4.5.2): metadata only — the durable bytes live in
/// the log and are reachable on the raw altitude.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAttachment {
    /// Pixel height, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// The image media type (e.g. `"image/png"`).
    pub media_type: String,
    /// Attachment type, `"image"` in v1 (free string: the vocabulary grows additively).
    #[serde(rename = "type")]
    pub r#type: String,
    /// Pixel width, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

/// One todo entry (tdd SS4.6.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    /// Present-tense active form, when the tool supplied one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,
    /// The todo status.
    pub status: TodoStatus,
    /// The todo text.
    pub text: String,
}

/// The latest todo-list fact in the snapshot (tdd SS4.6.3, SS4.9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoListState {
    /// The full todo list.
    pub items: Vec<TodoItem>,
    /// The snapshot revision. Diagnostics only, never an ordering guard.
    pub revision: u32,
    /// The tool that produced the snapshot, verbatim.
    pub source_tool: String,
}

open_enum! {
    /// Todo status (tdd SS4.6.3): closed in the runtime, wire-open.
    TodoStatus {
        Pending = "pending",
        InProgress = "inProgress",
        Completed = "completed",
        Cancelled = "cancelled",
    }
}

/// Raw token counters, verbatim from the durable record (tdd SS4.6.5). **Not directly summable
/// across providers** — sum the counted-once derivations instead.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenUsage {
    /// Cache read tokens, only when the provider distinguishes writes/reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<u64>,
    /// Cache write tokens, only when the provider distinguishes writes/reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<u64>,
    /// Provider-reported cache tokens (inside or beside `inputTokens`, provider-convention
    /// dependent — the reason `promptTokens` exists).
    pub cached_tokens: u64,
    /// Provider-reported input tokens.
    pub input_tokens: u64,
    /// Provider-reported output tokens.
    pub output_tokens: u64,
    /// Output tokens spent on reasoning, when the provider reports it.
    pub reasoning_tokens: u64,
}

/// Optional W3C trace context, on requests in both directions only — never on responses or
/// notifications (SS1.8).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceContext {
    /// W3C `traceparent` string; receivers that do not trace ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traceparent: Option<String>,
    /// W3C `tracestate` string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracestate: Option<String>,
}

/// `turn/cancel` params (tdd SS3.5): plain cancellation on the normal command lane. Same shape as
/// `turn/interrupt`, **without** `retract` — pairing a retract with a plain cancel is durably
/// rejected `not_paired_interrupt`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCancelParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The target session.
    pub session_id: String,
    /// The exact turn to cancel; omit to target the current foreground turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// `turn/cancel` result (tdd SS3.5): identical envelope to `turn/interrupt`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCancelResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The cancelled turn.
    pub turn_id: String,
}

/// `turn/completed` params (tdd SS4.5.1): the run's durable terminal record landed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedParams {
    /// Turn duration; absent means unmeasured, never fabricated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Present iff `terminal` is `"failed"`: mid-turn failures reach the client here — never as a
    /// JSON-RPC error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TurnError>,
    /// The runtime's free-text terminal reason, verbatim. Display and diagnostics only; never
    /// branch on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The turn terminal.
    pub terminal: TurnTerminal,
    /// Time to first token, when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_to_first_token_ms: Option<u64>,
    /// The terminated turn.
    pub turn_id: String,
    /// The turn's aggregate token usage, summed across the turn's model completions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// The settled turn-failure object (tdd SS4.5.1): `{kind, message, retryable}`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    /// The failure class.
    pub kind: TurnErrorKind,
    /// Human-readable failure text.
    pub message: String,
    /// The server's judgment that resubmitting the same input may succeed.
    pub retryable: bool,
}

open_enum! {
    /// Turn failure classes (tdd SS4.5.1).
    TurnErrorKind {
        StepLimit = "stepLimit",
        ConfigError = "configError",
        ProjectionError = "projectionError",
        LogError = "logError",
        WorkflowLaunchError = "workflowLaunchError",
        EnvironmentError = "environmentError",
        ModelError = "modelError",
        LaunchError = "launchError",
        AuthRequired = "authRequired",
    }
}

/// One ordered content part of a turn submission (tdd SS3.2). File mentions are text, not a part
/// type: write `@relative/path` in a text part. Modelled as a discriminated flat object, the
/// convention [`ApprovalSubject`] already established — what a flat object cannot express is
/// "`mediaType` is required exactly when `type` is `image`". `height` and `width` may only appear
/// together.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInputPart {
    /// Base64 payload, required on an `image` part. Invalid base64 or an empty payload is rejected
    /// with invalid params.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base64_data: Option<String>,
    /// Pixel height; must be provided together with `width` or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    /// Media type, required on an `image` part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
    /// User prompt text, on a `text` part. Multiple text parts are joined in order into the turn's
    /// prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The part type.
    #[serde(rename = "type")]
    pub r#type: TurnInputPartType,
    /// Pixel width; must be provided together with `height` or not at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
}

impl TurnInputPart {
    /// A `text` part carrying `text`.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            base64_data: None,
            height: None,
            media_type: None,
            text: Some(text.into()),
            r#type: TurnInputPartType::Text,
            width: None,
        }
    }

    /// An `image` part carrying a base64 payload and its media type.
    #[must_use]
    pub fn image(base64_data: impl Into<String>, media_type: impl Into<String>) -> Self {
        Self {
            base64_data: Some(base64_data.into()),
            height: None,
            media_type: Some(media_type.into()),
            text: None,
            r#type: TurnInputPartType::Image,
            width: None,
        }
    }
}

closed_enum! {
    /// The `type` discriminator of a turn input part (tdd SS3.2). Closed: an unknown part type is
    /// `invalidParams`.
    TurnInputPartType {
        Text = "text",
        Image = "image",
    }
}

/// `turn/interrupt` params (tdd SS3.4): the "user pressed stop" gesture, on the runtime's priority
/// lane.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// Pair a retract intent with the interrupt: if the turn is cancelled before any assistant
    /// output committed, the submission is durably retracted (view event `turn/retracted`) so the
    /// client may restore the prompt text. A rejected retract does not undo the interrupt. Default
    /// `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retract: Option<bool>,
    /// The target session.
    pub session_id: String,
    /// The exact turn to interrupt. Omit to target the session's current foreground turn, resolved
    /// at admission; prefer passing the explicit id when you have one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// `turn/interrupt` result (tdd SS3.4). Acceptance means the interrupt was admitted, **not** that
/// the turn is already stopped: the turn is over when you fold its `turn/completed` with terminal
/// `cancelled`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The interrupted turn.
    pub turn_id: String,
}

/// A turn named by the snapshot's active/queued lists (tdd SS4.9.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRef {
    /// The `commandId` of the submit that minted it.
    pub command_id: String,
    /// The turn's id, pre-minted per tdd SS3.1.4.
    pub turn_id: String,
}

/// `turn/retracted` params (tdd SS4.5.1): an interrupt-paired retract was durably accepted. The
/// retracted user-message item is re-emitted via `item/updated` with `retracted: true`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRetractedParams {
    /// The retracted submission's command id.
    pub command_id: String,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The turn whose submission was retracted.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `turn/retryScheduled` params (tdd SS4.5.1): a durable turn retry-scheduled fact folded — the
/// failing model attempt's scheduled retry is observable BEFORE the turn terminates. Non-terminal:
/// it never resolves a turn-wait. The delay is the recorded scheduled backoff, never an absolute
/// fire time — clients derive any countdown locally.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRetryScheduledParams {
    /// The attempt that failed (producer-normalized, >= 1).
    pub attempt: u32,
    /// The turn's model-retry chain bound.
    pub max_attempts: u32,
    /// The attempt about to run (producer contract: `attempt < nextAttempt <= maxAttempts`).
    pub next_attempt: u32,
    /// The recorded stopping reason through the shared task-detail normalization (non-empty; capped
    /// at 160 characters).
    pub reason: String,
    /// The scheduled backoff delay in milliseconds, verbatim from the durable fact.
    pub retry_delay_ms: u64,
    /// The owning session.
    pub session_id: String,
    /// The durable record this event folded from (`first == last`).
    pub source_range: SourceRange,
    /// The running turn whose model attempt is being retried.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

open_enum! {
    /// What `turn/start` did with the input (tdd SS3.2). Since `queue` is the default `ifBusy`,
    /// clients need this to distinguish the cases without folding history.
    TurnStartDisposition {
        Started = "started",
        Queued = "queued",
        Steered = "steered",
    }
}

/// `turn/start` params (tdd SS3.2). `providerRequestOptions` is deliberately absent — ruled to stay
/// off the published schema.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    /// The SS3.1.1 idempotency handle (UUIDv7). The fresh turn's `turnId` derives from it.
    pub command_id: String,
    /// Presentation form of the prompt for transcripts. Durable; carried on the resulting
    /// user-message view item. Never model-visible.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
    /// Disposition when a turn is already running; default `queue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub if_busy: Option<IfBusy>,
    /// Ordered content parts; required and non-empty.
    pub input: Vec<TurnInputPart>,
    /// Reasoning tier sampled at submission for this turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The target session.
    pub session_id: String,
}

/// `turn/start` result (tdd SS3.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// What happened to the input.
    pub disposition: TurnStartDisposition,
    /// `true` iff `disposition` is `started`; retained as the boolean shorthand — `disposition` is
    /// authoritative.
    pub started_new_turn: bool,
    /// Admission status.
    pub status: CommandStatus,
    /// The turn that will carry (or absorbed) this input. Authoritative: always take it from the
    /// ack rather than deriving it.
    pub turn_id: String,
}

/// `turn/started` params (tdd SS4.5.1): a foreground turn began running — fresh submits
/// immediately, queued submits at their launch boundary, never steered submits.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartedParams {
    /// The submitting command; `turnId == commandId` for fresh turns.
    pub command_id: String,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The started turn.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// `turn/steer` params (tdd SS3.3): exact-target steering into the currently running turn.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The turn you believe is running. Closes the race where the turn completes or is replaced
    /// between your read and your steer: input meant for turn A can never leak into turn B.
    pub expected_turn_id: String,
    /// Same content parts as `turn/start`.
    pub input: Vec<TurnInputPart>,
    /// Reasoning tier for the model calls this steer's input reaches. Applies forward, never
    /// backward; absent means unspecified, not "reset".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// The target session.
    pub session_id: String,
}

/// `turn/steer` result (tdd SS3.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnSteerResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The running turn that absorbed the input.
    pub turn_id: String,
}

open_enum! {
    /// Turn terminal vocabulary (tdd SS4.5.1): exactly the runtime's `RunTerminalKind` — closed in
    /// the runtime, wire-open for evolution.
    TurnTerminal {
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

/// `turn/unqueue` params (tdd SS3.6): reclaim a queued submit before it launches. It is **not** a
/// stop — a reclaim that arrives after its target launched is durably rejected, never silently
/// upgraded into a cancel.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUnqueueParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The target session.
    pub session_id: String,
    /// The queued turn to reclaim, exactly as the queueing `turn/start`'s ack minted it. Required,
    /// and never "whichever is newest".
    pub turn_id: String,
}

/// `turn/unqueue` result (tdd SS3.6). Admission only — but for this command admission *is* the
/// race: an admitted reclaim implies the turn will not launch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUnqueueResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The reclaimed turn.
    pub turn_id: String,
}

/// `turn/unqueued` params (tdd SS3.6, SS4.5.1): a queued submit's reclaim durably won; its
/// pre-minted turn never runs. No `turn/started`/`turn/completed` is ever emitted for this
/// `turnId`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnUnqueuedParams {
    /// The `commandId` of the `turn/start` that queued the reclaimed turn, so a client can restore
    /// the submission's text exactly as for `turn/retracted`.
    pub command_id: String,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from (the session stream's reclaim record).
    pub source_range: SourceRange,
    /// The reclaimed turn: pre-minted at admission, never launched.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}
