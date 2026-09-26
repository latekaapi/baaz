//! W7b: provider approvals read as decisions, not wire dumps.
//!
//! Replays the recorded approval exchanges through the same folds the
//! live pumps use and pins what the single inline card is built from:
//! a per-tool face (never raw JSON, never a wire id), a pending tool
//! card in the present tense, and a card that settles only when the
//! outcome lands — allowed when the tool ran, denied when it was
//! refused. Remove any arm (the face, the pending verb, the settle)
//! and its test names the regression.
//!
//! The baaz-side title and the empty-thinking guard live beside the
//! render and are pinned in `transcript.rs` unit tests; the strip's
//! absence on provider lanes and the row sync in the lane tests.

use aui_protocol::{ApprovalState, Block, Delta, ToolKind, ToolStatus};
use provider::ProviderEvent;

fn fixture(kind: &str, name: &str) -> Vec<String> {
    let path = format!("{}/../../fixtures/{kind}/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture reads: {path}"))
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The child-side lines of a Claude Code exchange: everything but the
/// `host->cli` envelopes.
fn child_lines(lines: &[String]) -> Vec<&String> {
    lines.iter().filter(|line| !line.contains("\"host->cli\"")).collect()
}

fn claude_deltas(lines: &[String]) -> Vec<Delta> {
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    let mut deltas = Vec::new();
    for line in child_lines(lines) {
        provider_claude_code::fold::step_line(&mut fold, line, &mut |event| {
            if let ProviderEvent::Deltas { deltas: more, .. } = event {
                deltas.extend(more);
            }
        });
    }
    deltas
}

fn codex_deltas(lines: &[String]) -> Vec<Delta> {
    let mut fold = provider_codex::fold::CodexFold::new();
    let mut deltas = Vec::new();
    for line in lines {
        let (_, frame) =
            provider_codex::frame::decode_envelope(line).expect("fixture decodes");
        deltas.extend(fold.apply(&frame));
    }
    deltas
}

fn approvals(deltas: &[Delta]) -> Vec<&Block> {
    deltas
        .iter()
        .filter_map(|delta| match delta {
            Delta::BlockAdded { block: card @ Block::Approval { .. }, .. } => Some(card),
            _ => None,
        })
        .collect()
}

fn approval_updates(deltas: &[Delta]) -> Vec<&Block> {
    deltas
        .iter()
        .filter_map(|delta| match delta {
            Delta::BlockUpdated { block: card @ Block::Approval { .. }, .. } => Some(card),
            _ => None,
        })
        .collect()
}

/// `approval-default.jsonl`: the Write ask cards the path with a content
/// preview — never the input JSON, never a `toolu_` id — offers exactly
/// allow/deny, and the gated Write card waits in the present tense.
#[test]
fn claude_write_ask_faces_the_path_with_a_preview() {
    let lines = fixture("claude-code", "approval-default.jsonl");
    // Fold only up to the ask: nothing later has answered or run yet.
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    let mut deltas = Vec::new();
    for line in child_lines(&lines) {
        let mut asked = false;
        provider_claude_code::fold::step_line(&mut fold, line, &mut |event| {
            match &event {
                ProviderEvent::Deltas { deltas: more, .. } => deltas.extend(more.clone()),
                ProviderEvent::ApprovalRequested { .. } => asked = true,
                _ => {}
            }
        });
        if asked {
            break;
        }
    }
    let cards = approvals(&deltas);
    assert_eq!(cards.len(), 1, "one ask, one card: {deltas:?}");
    match cards[0] {
        Block::Approval { tool, command, reason, cwd, choices, state, .. } => {
            assert_eq!(tool, "Write");
            assert_eq!(*state, ApprovalState::Pending);
            assert_eq!(command, "/tmp/w4b/outside-claude.txt", "the path, not the JSON");
            assert!(reason.contains("/tmp/w4b/outside-claude.txt"), "the path: {reason}");
            assert!(reason.contains("APPROVED"), "the content preview: {reason}");
            assert!(!reason.contains("toolu_"), "no wire ids: {reason}");
            assert!(!reason.contains("\"file_path\""), "no raw JSON: {reason}");
            assert!(!command.contains('{'), "no raw JSON: {command}");
            assert!(!cwd.trim().is_empty(), "the run's directory, never blank");
            let ids: Vec<&str> = choices.iter().map(|choice| choice.id.as_str()).collect();
            assert_eq!(ids, ["allow", "deny"], "the full choice set the lane answers");
        }
        other => panic!("an approval card, got {other:?}"),
    }
    // No empty thinking shell: the fixture's redacted traces fold away.
    assert!(
        !deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded { block: Block::Thinking { text, .. }, .. } if text.trim().is_empty()
        )),
        "redacted thinking folds to no block"
    );
    // The gated call waits in the present tense, never "Wrote" beside a spinner.
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded {
                block: Block::ToolCall { kind: ToolKind::Write, verb, status: ToolStatus::Running, target, .. },
                ..
            } if verb == "Write" && target == "/tmp/w4b/outside-claude.txt"
        )),
        "the gated Write waits as Write: {deltas:?}"
    );
}

/// `approval-default.jsonl` to the end: the allowed Write ran, the card
/// settled to allowed, and the turn answered DONE.
#[test]
fn claude_allowed_write_runs_and_settles() {
    let lines = fixture("claude-code", "approval-default.jsonl");
    let deltas = claude_deltas(&lines);
    assert!(
        approval_updates(&deltas).iter().any(|card| matches!(
            card,
            Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }
        )),
        "the run settles the card to allowed"
    );
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded { block: Block::Text { text, .. }, .. } if text == "DONE"
        )),
        "the turn completes with the reply"
    );
}

/// `permission-deny.jsonl`: the refused call settles the card to denied
/// and the turn continues with the denial — the press never settles it.
#[test]
fn claude_denied_call_settles_denied_and_continues() {
    let lines = fixture("claude-code", "permission-deny.jsonl");
    let deltas = claude_deltas(&lines);
    let updates = approval_updates(&deltas);
    let settled = updates
        .iter()
        .filter(|card| {
            matches!(card, Block::Approval { state: ApprovalState::Denied, .. })
        })
        .count();
    assert_eq!(settled, 1, "the refusal settles the card to denied, once");
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded { block: Block::Text { text, .. }, .. }
                if text.contains("Denied by the owner")
        )),
        "the turn continues with the denial"
    );
}

/// `codex/approval-default.jsonl`: the command ask cards the inner
/// command with its cwd and the model's reason, offers all four
/// choices, and the gated execution waits as "Run" — finishing "Ran"
/// with the card allowed.
#[test]
fn codex_command_ask_waits_then_runs() {
    let lines = fixture("codex", "approval-default.jsonl");
    let deltas = codex_deltas(&lines);
    let cards = approvals(&deltas);
    assert_eq!(cards.len(), 1, "one ask, one card");
    match cards[0] {
        Block::Approval { tool, command, reason, cwd, choices, state, .. } => {
            assert_eq!(tool, "Bash");
            assert_eq!(*state, ApprovalState::Pending);
            assert!(command.contains("outside-codex.txt"), "the inner command: {command}");
            assert!(!command.contains("/bin/zsh"), "the wrapper stays out: {command}");
            assert_eq!(cwd, "/tmp/w4b/work");
            assert!(reason.contains("outside-codex.txt"), "the model's reason: {reason}");
            assert!(!reason.contains("exec-"), "no item ids: {reason}");
            assert!(!reason.contains('{'), "no raw JSON: {reason}");
            let ids: Vec<&str> = choices.iter().map(|choice| choice.id.as_str()).collect();
            assert_eq!(ids, ["accept", "accept-for-session", "decline", "cancel"]);
        }
        other => panic!("an approval card, got {other:?}"),
    }
    assert!(
        approval_updates(&deltas).iter().any(|card| matches!(
            card,
            Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }
        )),
        "the run settles the card to allowed"
    );
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded {
                block: Block::ToolCall { kind: ToolKind::Shell, verb, status: ToolStatus::Success, .. },
                ..
            } if verb == "Ran"
        )),
        "the finished execution reads done"
    );
}

/// `codex/approval.jsonl`: the approved command runs to completion.
#[test]
fn codex_approved_command_completes() {
    let lines = fixture("codex", "approval.jsonl");
    let deltas = codex_deltas(&lines);
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded {
                block: Block::ToolCall { kind: ToolKind::Shell, status: ToolStatus::Success, .. },
                ..
            }
        )),
        "the approved command ran"
    );
}

/// `codex/edit.jsonl`: the file-change ask names no forged path, and
/// the completing change lands the path with its diff on the done card.
#[test]
fn codex_file_change_lands_path_and_diff_on_done() {
    let lines = fixture("codex", "edit.jsonl");
    let deltas = codex_deltas(&lines);
    let cards = approvals(&deltas);
    assert_eq!(cards.len(), 1, "one ask, one card");
    match cards[0] {
        Block::Approval { tool, state, .. } => {
            assert_eq!(tool, "Edit");
            assert_eq!(*state, ApprovalState::Pending);
        }
        other => panic!("an approval card, got {other:?}"),
    }
    assert!(
        deltas.iter().any(|delta| matches!(
            delta,
            Delta::BlockAdded {
                block: Block::ToolCall { kind: ToolKind::Write | ToolKind::Edit, verb, status: ToolStatus::Success, target, .. },
                ..
            } if (verb == "Wrote" || verb == "Edited") && !target.is_empty()
        )),
        "the done card names the path in the past tense"
    );
}
