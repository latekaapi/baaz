//! Approval cards and their choices, with every field a fold does not
//! decide at its neutral default.

use aui_protocol::{
    ApprovalBadges, ApprovalBodyKind, ApprovalChoice, ApprovalDecision, ApprovalScope, ApprovalState,
    Block,
};

/// One answer a card offers: `id` rides `DecideApproval` back to the
/// adapter verbatim, so a fold offers only ids its wire can carry.
pub fn choice(id: impl Into<String>, label: &str, decision: ApprovalDecision, scope: ApprovalScope) -> ApprovalChoice {
    ApprovalChoice { id: id.into(), label: label.into(), decision, scope, rule_preview: None, accepts_feedback: false }
}

/// A pending approval card scoped to this command, with no rule, stages,
/// badges or capabilities.
pub fn card(
    id: &str,
    tool: &str,
    command: impl Into<String>,
    reason: impl Into<String>,
    cwd: impl Into<String>,
    body_kind: ApprovalBodyKind,
    choices: Vec<ApprovalChoice>,
) -> Block {
    Block::Approval {
        id: id.to_owned(),
        tool: tool.to_owned(),
        command: command.into(),
        reason: reason.into(),
        cwd: cwd.into(),
        capabilities: Vec::new(),
        scope: ApprovalScope::ThisCommand,
        body_kind,
        state: ApprovalState::Pending,
        rule: None,
        choices,
        stages: Vec::new(),
        current_stage: None,
        badges: ApprovalBadges::default(),
        feedback: None,
        resolved_by: None,
    }
}

/// How a card nobody can answer any more settles: [`ApprovalState::Denied`]
/// when the person refused it, else cancelled with `reason` — the ask was
/// abandoned, never refused on the person's behalf.
pub fn abandoned(denied_by_person: bool, reason: &str) -> ApprovalState {
    if denied_by_person {
        ApprovalState::Denied
    } else {
        ApprovalState::Cancelled { reason: Some(reason.to_owned()) }
    }
}
