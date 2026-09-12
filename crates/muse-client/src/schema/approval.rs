//! The `approval/*` lane (SS5): what a server asks permission for, the
//! choices it mints, and how a decision travels back.
//!
//! Part of [`crate::schema`]; see that module for the conventions every
//! type here follows.

use serde::{Deserialize, Serialize};

use super::*;

/// A policy amendment a decision installed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalAmendment {
    /// How long the amendment lasts.
    pub durability: ApprovalAmendmentDurability,
    /// Human-readable preview of the rule that was installed.
    pub rule_preview: String,
}

open_enum! {
    /// How durable an installed approval amendment is.
    ApprovalAmendmentDurability {
        Session = "session",
        LocalPersistent = "localPersistent",
    }
}

/// The single change carried by an `approval/updated` event.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalChange {
    /// The choice that drove the change, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choice_id: Option<String>,
    /// The decision that drove the change, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
    /// Whether the re-parsed command is executable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<bool>,
    /// Open change discriminator.
    pub kind: String,
    /// Free-text detail, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Whether the subject was re-parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reparsed: Option<bool>,
    /// The stage token the change advanced to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requirement_id: Option<ApprovalRequirementRef>,
    /// Whether a persistent amendment was written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<ApprovalPersistenceStatus>,
}

/// One choice the client may offer the user for a pending approval.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalChoice {
    /// Whether this choice accepts free-text feedback on `approval/decide`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepts_feedback: Option<bool>,
    /// Server-minted choice id; the only value `approval/decide` accepts.
    pub choice_id: String,
    /// The decision this choice records.
    pub decision: ApprovalDecision,
    /// Presentation label.
    pub label: String,
    /// Preview of the rule this choice would install, when it installs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_preview: Option<String>,
    /// How far the choice reaches.
    pub scope: ApprovalChoiceScope,
}

open_enum! {
    /// How far an approval choice reaches.
    ApprovalChoiceScope {
        Once = "once",
        Session = "session",
        LocalPersistent = "localPersistent",
    }
}

/// `approval/decide` params (tdd SS5.4): the decision. A standard SS3 command — requires the
/// session loaded on this host, requires `commandId`, durable intake before ack, value-identical
/// replay.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecideParams {
    /// The approval being decided.
    pub approval_id: String,
    /// One of the current `availableChoices`; else -32052.
    pub choice_id: String,
    /// The SS3.1.1 idempotency handle (UUIDv7).
    pub command_id: String,
    /// Free-text guidance delivered to the model with the denial. Only valid on choices with
    /// `acceptsFeedback`. Never persisted in the durable audit record, so it is absent from
    /// `approval/resolved`. Omitted and explicit `null` both mean "no feedback".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// Must equal the approval's `currentRequirementId`. The multi-stage race guard: a decision
    /// aimed at stage 1 can never accidentally satisfy stage 2, and a stale value is -32053.
    pub requirement_id: ApprovalRequirementRef,
    /// The target session.
    pub session_id: String,
}

/// `approval/decide` result (tdd SS5.4). Admission-ack rules still apply: the authoritative outcome
/// is `approval/resolved` / `approval/updated` on the view stream.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalDecideResult {
    /// The approval this decision was applied to.
    pub approval_id: String,
    /// Echoes the client's id.
    pub command_id: String,
    /// Admission status.
    pub status: CommandStatus,
    /// `false` when the choice satisfied a stage but further requirements remain (the approval
    /// stays pending and an updated `approval/request` follows); `true` when this decision produced
    /// the terminal `DecisionApplied`.
    pub terminal: bool,
}

open_enum! {
    /// The decision recorded for an approval.
    ApprovalDecision {
        Approved = "approved",
        ApprovedForSession = "approvedForSession",
        ApprovedPolicyAmendment = "approvedPolicyAmendment",
        Denied = "denied",
        DeniedPolicyAmendment = "deniedPolicyAmendment",
        TimedOut = "timedOut",
        Abort = "abort",
    }
}

/// `approval/listPending` params (tdd SS5.7): the pull dual of the re-issued requests. A log-fold
/// read — no lease, works on loaded and unloaded sessions, never subscribes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalListPendingParams {
    /// The session to read.
    pub session_id: String,
}

/// `approval/listPending` result (tdd SS5.7). Ordering is by opening `viewCursor`; empty arrays
/// when nothing is pending. The result is point-in-time — to act on it race-safely,
/// `approval/decide` carries the `requirementId` guard regardless of how the client learned it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalListPendingResult {
    /// Exactly the `approval/request` params of tdd SS5.3, one per pending approval.
    pub approvals: Vec<ApprovalRequestParams>,
    /// Exactly the `userInput/request` params of tdd SS5.10, one per pending prompt.
    pub user_inputs: Vec<UserInputRequestParams>,
}

closed_enum! {
    /// The approval enforcement modes (tdd SS5.12). Closed by design (select-never-create): a
    /// client selects a preconfigured mode and can never construct one.
    ApprovalMode {
        AllowAll = "allowAll",
        PromptUnmatched = "promptUnmatched",
        OnRequest = "onRequest",
        DenyUnmatched = "denyUnmatched",
    }
}

open_enum! {
    /// Whether a mode change did anything (tdd SS5.12). Apply failures are `commandRejected`.
    ApprovalModeApplyOutcome {
        Completed = "completed",
        Noop = "noop",
    }
}

open_enum! {
    /// How an approval mode took effect (tdd SS5.12).
    ApprovalModeSource {
        Startup = "startup",
        Replay = "replay",
        ApprovalReconfigure = "approvalReconfigure",
    }
}

/// Where an approval subject came from.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalOrigin {
    /// The originating command, when one is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Open origin discriminator.
    pub kind: String,
    /// The originating URL, when one is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

open_enum! {
    /// Whether a persistent amendment was actually written.
    ApprovalPersistenceStatus {
        Succeeded = "succeeded",
        Failed = "failed",
    }
}

open_enum! {
    /// The policy engine's verdict on an approval.
    ApprovalPolicyResult {
        Allow = "allow",
        Deny = "deny",
    }
}

/// Full params shared by `approval/request` (a server→client **request**) and the
/// `approval/requested` notification.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequestParams {
    /// The pending approval's id.
    pub approval_id: String,
    /// The choices the user may pick from; `approval/decide` accepts only these `choiceId`s.
    pub available_choices: Vec<ApprovalChoice>,
    /// The stage token a decision must echo — a decision carrying anything else is -32053.
    pub current_requirement_id: ApprovalRequirementRef,
    /// The parked transcript item.
    pub item_id: String,
    /// Whether an LLM judge escalated this approval.
    pub judge_escalated: bool,
    /// Whether the action would write to a protected path.
    pub protected_write: bool,
    /// The model-authored arguments, verbatim.
    pub raw_args: String,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// What is being approved.
    pub subject: ApprovalSubject,
    /// The gated task.
    pub task_id: String,
    /// The gated provider tool-call id.
    pub tool_call_id: String,
    /// The gated tool.
    pub tool_name: String,
    /// The owning turn.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor — relay it, never parse it.
    pub view_cursor: String,
}

/// Approval-stage token carried in SS5 params and stale-requirement errors.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequirementRef {
    /// The approval this stage belongs to.
    pub approval_id: String,
    /// The stage's index within the approval's source.
    pub source_index: u32,
}

/// Winning terminal returned to a losing `approval/decide` command.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolutionSummary {
    /// The winning decision, verbatim.
    pub decision: String,
    /// Who resolved it, verbatim.
    pub resolved_by: String,
    /// The cursor the winning resolution landed at.
    pub view_cursor: String,
}

open_enum! {
    /// Who produced the terminal approval decision.
    ApprovalResolvedBy {
        User = "user",
        Policy = "policy",
        LlmJudge = "llmJudge",
    }
}

/// `approval/resolved` params: the first durable terminal decision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalResolvedParams {
    /// The amendment this decision installed, when it installed one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendment: Option<ApprovalAmendment>,
    /// The resolved approval.
    pub approval_id: String,
    /// The `approval/decide` that won, when a command decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by_command_id: Option<String>,
    /// The terminal decision.
    pub decision: ApprovalDecision,
    /// The gated item.
    pub item_id: String,
    /// The policy engine's verdict.
    pub policy_result: ApprovalPolicyResult,
    /// Who resolved it.
    pub resolved_by: ApprovalResolvedBy,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// One entry per satisfied stage.
    pub stage_evidence: Vec<ApprovalStageEvidence>,
    /// The owning turn.
    pub turn_id: String,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}

/// One stage of a multi-stage approval, as offered.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalStage {
    /// The stage's argv.
    pub argv: Vec<String>,
    /// Whether `argv` is the complete command or a parsed prefix.
    pub argv_complete: bool,
    /// 1-based stage position.
    pub position: u32,
    /// The stage token a decision aimed at this stage must carry.
    pub requirement_id: ApprovalRequirementRef,
    /// How the stage resolves.
    pub resolution: ApprovalStageResolution,
    /// A prefix the client may offer to persist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_prefix: Option<ApprovalSuggestedPrefix>,
    /// How many stages this approval has.
    pub total_stages: u32,
}

/// One satisfied stage, as recorded on the terminal decision.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalStageEvidence {
    /// The stage's argv.
    pub argv: Vec<String>,
    /// 1-based stage position.
    pub position: u32,
    /// The stage token.
    pub requirement_id: ApprovalRequirementRef,
    /// How the stage resolved.
    pub resolution: ApprovalStageResolution,
    /// How many stages this approval had.
    pub total_stages: u32,
}

/// How one approval stage resolves.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalStageResolution {
    /// The matched argv prefix, when the resolution matched one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argv_prefix: Option<Vec<String>>,
    /// Diagnostic detail, when recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// Open resolution discriminator.
    pub kind: String,
}

/// Open approval subject union, modelled as one flat object with per-kind optional members — the
/// schema names no object-variant union. Unknown kinds are rendered generically and never
/// auto-approved by clients (tdd SS5.2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalSubject {
    /// `fileAccess`: the requested access mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<String>,
    /// `shell`: the command text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// `network`: the host.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Open discriminator: `shell | fileAccess | network | process | tool`.
    pub kind: String,
    /// Where the subject came from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<ApprovalOrigin>,
    /// `fileAccess`: the path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `network`: the port.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u32>,
    /// `network`: the protocol.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<String>,
    /// `shell`: the parsed stages of a multi-stage command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stages: Option<Vec<ApprovalStage>>,
    /// `process`: the target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// `tool`: the tool name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// The workspace root the subject is evaluated against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
}

/// A prefix the client may offer to persist as a rule.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalSuggestedPrefix {
    /// The argv prefix.
    pub argv_prefix: Vec<String>,
    /// Presentation label.
    pub label: String,
}

/// `approval/updated` params: the refreshed pending view plus one change.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalUpdatedParams {
    /// The approval that changed.
    pub approval_id: String,
    /// The refreshed choice set.
    pub available_choices: Vec<ApprovalChoice>,
    /// The one change this event reports.
    pub change: ApprovalChange,
    /// The stage token a decision must now echo.
    pub current_requirement_id: ApprovalRequirementRef,
    /// The owning session.
    pub session_id: String,
    /// The durable records this event folded from.
    pub source_range: SourceRange,
    /// The refreshed subject.
    pub subject: ApprovalSubject,
    /// Opaque, strictly monotonic view cursor.
    pub view_cursor: String,
}
