//! The `userInput/*` lane (SS4.9): structured questions, their options,
//! and the answer or clarification that settles one.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

/// One answer to one prompted question.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    /// Free-text answer (<=500 chars), on a free-text question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free_text: Option<String>,
    /// Optional note (<=500 chars).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The question being answered.
    pub question_id: String,
    /// The chosen option label, in `single` selection mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_label: Option<String>,
    /// The chosen option labels, in `multiple` selection mode, within the min/max bounds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_labels: Option<Vec<String>>,
}

/// `userInput/answer` params (tdd SS5.10.2): answer every question. Each answer carries
/// `questionId` then exactly one of `selectedLabel`, `selectedLabels` or `freeText`, plus an
/// optional `note`. Answers that do not match the prompt fail -32057 `userInputAnswerInvalid`.
/// Image attachments are a **reserved** field in v1 and are rejected if sent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswerParams {
    /// One entry per question.
    pub answers: Vec<UserInputAnswer>,
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The target session.
    pub session_id: String,
    /// The prompt being answered.
    pub user_input_id: String,
}

/// `userInput/answer` result (tdd SS5.10.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswerResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The prompt this answer settled.
    pub user_input_id: String,
}

/// `userInput/cancel` params (tdd SS5.10.2): decline to answer; the tool call resolves with a
/// cancelled result the model sees.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputCancelParams {
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// Why the prompt was declined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The target session.
    pub session_id: String,
    /// The prompt being declined.
    pub user_input_id: String,
}

/// `userInput/cancel` result (tdd SS5.10.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputCancelResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The prompt this cancellation settled.
    pub user_input_id: String,
}

/// A free-form clarification answer.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputClarification {
    /// The clarification body (<=500 chars).
    pub content: String,
    /// The body's format; `"text"` in v1 (open vocabulary).
    pub format: String,
}

/// `userInput/clarify` params (tdd SS5.10.2): answer with a free-form clarification instead of the
/// structured options — the "let me explain" path; the model receives the clarification text and
/// re-decides.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputClarifyParams {
    /// `format` is `"text"` in v1 (open); `content` is <=500 chars.
    pub clarification: UserInputClarification,
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// The target session.
    pub session_id: String,
    /// The prompt being clarified.
    pub user_input_id: String,
}

/// `userInput/clarify` result (tdd SS5.10.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputClarifyResult {
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// The prompt this clarification settled.
    pub user_input_id: String,
}

/// One selectable option on a prompted question.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOption {
    /// Longer description, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The option label — the value an answer names.
    pub label: String,
    /// A renderable preview of what the option does, when supplied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<UserInputOptionPreview>,
}

/// A renderable preview attached to a prompt option.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputOptionPreview {
    /// The preview body.
    pub content: String,
    /// The body's format.
    pub format: String,
}

open_enum! {
    /// How a user-input prompt settled.
    UserInputOutcome {
        Answered = "answered",
        Cancelled = "cancelled",
        Interrupted = "interrupted",
        Clarified = "clarified",
        TimedOut = "timedOut",
        Aborted = "aborted",
    }
}

/// One question inside a user-input prompt.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputQuestion {
    /// Short header shown above the question.
    pub header: String,
    /// The question id an answer must name.
    pub id: String,
    /// The selectable options.
    pub options: Vec<UserInputOption>,
    /// The question text.
    pub question: String,
    /// How many options may be chosen.
    pub selection: UserInputSelection,
}

/// Full params shared by `userInput/request` (a server→client **request**) and the
/// `userInput/requested` notification.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputRequestParams {
    /// When the prompt auto-resolves, in milliseconds, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resolution_ms: Option<u64>,
    /// The parked transcript item.
    pub item_id: String,
    /// Every question the prompt asks.
    pub questions: Vec<UserInputQuestion>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from; absent on an ephemeral-sourced prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_range: Option<SourceRange>,
    /// The prompting provider tool-call id.
    pub tool_call_id: String,
    /// The prompting tool.
    pub tool_name: String,
    /// The owning turn.
    pub turn_id: String,
    /// The prompt's id — the value the settling command must name.
    pub user_input_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// How many options a question accepts.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputSelection {
    /// Upper bound in `multiple` mode, when bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_selections: Option<u32>,
    /// Lower bound in `multiple` mode, when bounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_selections: Option<u32>,
    /// The selection mode.
    pub mode: UserInputSelectionMode,
}

closed_enum! {
    /// Whether a question takes one answer or several.
    UserInputSelectionMode {
        Single = "single",
        Multiple = "multiple",
    }
}

/// `userInput/settled` params: the first durable prompt settlement.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputSettledParams {
    /// The answers, when the prompt was answered; empty otherwise.
    pub answers: Vec<UserInputAnswer>,
    /// The clarification, when the prompt was clarified; `null` otherwise. Required-nullable.
    #[serde(default)]
    pub clarification: Option<UserInputClarification>,
    /// The command that settled it; `null` when nothing did. Required-nullable.
    #[serde(default)]
    pub decided_by_command_id: Option<String>,
    /// How the prompt settled.
    pub outcome: UserInputOutcome,
    /// The settlement reason; `null` when none was recorded. Required-nullable.
    #[serde(default)]
    pub reason: Option<String>,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The settled prompt.
    pub user_input_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// Winning terminal returned to a late user-input command.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputSettlementSummary {
    /// The winning outcome, verbatim.
    pub outcome: String,
    /// The cursor the winning settlement landed at.
    pub view_cursor: String,
}
