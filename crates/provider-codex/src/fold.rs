//! Decoded frames into render-ready deltas: one primary lane plus a fallback,
//! and the reason.
//!
//! RENDERING LANES: **`item/completed` frames are the primary lane.**
//! `item/agentMessage/delta` frames are decoded and accumulated per
//! `(turn_id, item_id)` as a fallback lane; they emit no deltas on arrival.
//! When an `item/completed` closes an item, its buffered deltas are dropped
//! (the completed text renders whole). When a turn ends with buffered deltas
//! still open — the interrupted path, where `item/completed` never arrives
//! for the streamed message — `turn/completed` flushes them so the streamed
//! text survives. `item/started` is likewise carried, not rendered.
//!
//! Why `item/completed` stays primary:
//!
//! * `basic.jsonl` — a turn whose single `READY` arrives as one delta plus
//!   one completed item — renders its whole message through `item/completed`
//!   alone. The completed lane closes every message on the normal path; the
//!   interrupted path (`interrupt.jsonl`) never sends `item/completed` for
//!   the streamed message, so the delta fallback is what renders it there.
//! * `item/completed` carries whole `item.text`, so the fold needs no
//!   cross-frame assembly state on the normal path: no chunk bookkeeping,
//!   nothing to disagree about.
//! * The trap this task exists to avoid is folding both lanes and
//!   double-rendering every message. The fallback renders only text no
//!   completed item ever closed — rather than "deduplicating" two lanes with
//!   a heuristic — so there is no seam where the two paths can disagree on
//!   the normal path. The agreement test proves it: folding `basic.jsonl` or
//!   `approval.jsonl` with its `item/agentMessage/delta` lines stripped
//!   yields deltas identical to folding it whole, and a dedicated test proves
//!   the interrupted turn's streamed text still survives.
//!
//! What the fold emits, per frame:
//!
//! * `item/completed` with an `agentMessage` (non-empty text): one
//!   [`Delta::TurnStarted`] per unseen turn id, then one [`Delta::BlockAdded`]
//!   with a complete [`Block::Text`] (`streaming: false`).
//! * `item/completed` with a `userMessage`: one [`Delta::TurnStarted`] with a
//!   [`Turn::User`] per unseen item id.
//! * `item/completed` with `reasoning` (non-empty): a [`Block::Thinking`].
//! * `item/completed` with `commandExecution`: a [`Block::ToolCall`] shell
//!   card (`Success` on `completed` plus exit 0, `Error` on `completed`
//!   otherwise or on `failed`, `Cancelled` on `declined`, `Running` while
//!   `inProgress`; an unknown future status fails closed to `Error`, never
//!   back to a spinner).
//! * `item/completed` with anything else: a [`Block::Generic`] carrying the
//!   wire kind, status, and text — carried, never dropped.
//! * `turn/plan/updated`: the turn's plan as one [`Block::Todo`] — the same
//!   plan/todo block Claude Code's TodoWrite produces. The first update
//!   adds the card; every later update rewrites it wholesale at the same
//!   block index (replace, never append), so four updates still render
//!   one card showing the latest statuses.
//! * `turn/completed`: one [`Delta::TurnFinished`] per started turn, keyed on
//!   the wire turn id, with per-turn tokens from the `last` bucket (never the
//!   cumulative `total`) and `cost_usd: 0.0` — the neutral unknown default,
//!   never a measurement: the protocol reports tokens, never money, and there
//!   is deliberately no cost field on [`crate::frame::TokenUsage`].
//! * `thread/tokenUsage/updated`: recorded against `(thread_id, turn_id)`;
//!   no deltas. `account/rateLimits/updated`: the account snapshot; no
//!   deltas. Everything else: no deltas.

use std::collections::{HashMap, HashSet};

use aui_protocol::{
    ApprovalBadges, ApprovalBodyKind, ApprovalChoice, ApprovalDecision, ApprovalScope,
    ApprovalState, Attachment, AttachmentKind, Block, Delta, Diff, DiffKind, DiffLine, DiffStat,
    Hunk, ThinkingState, TodoItem, TodoState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta,
    UploadState,
};
use provider::ProviderEvent;
use serde_json::Value;

use crate::child::{
    ApprovalKind, FileChangeApprovalParams, MCP_ELICITATION_ID_PREFIX,
    PermissionsApprovalParams, elicitation_is_tool_approval,
};
use crate::frame::{decode_thread_item, FileChangeEntry, Frame, Item, Notification, TokenCounts};

/// The settled verb of a terminal tool card (D51).
pub const TERMINAL_RAN_VERB: &str = "Ran in terminal";

/// The running verb of a terminal tool card: an `mcpToolCall` that
/// completes still `inProgress` reads open, never done.
pub const TERMINAL_RUNNING_VERB: &str = "Running in terminal";

/// The header target of a terminal card whose call names no command:
/// what the card names when there is no command to name.
pub const TERMINAL_TOOL_TARGET: &str = "terminal";

/// Whether `tool` is one of the baaz terminal tools (`docs/14-terminal.md`
/// §4). An `mcpToolCall` from the baaz server for one of these folds to a
/// shell card with terminal verbs (D51) — the command from the call's
/// arguments, the run's output as the body — never the raw item JSON.
fn is_terminal_tool(tool: &str) -> bool {
    matches!(
        tool,
        "terminal_list"
            | "terminal_open"
            | "terminal_run"
            | "terminal_read"
            | "terminal_screen"
            | "terminal_send"
            | "terminal_close"
    )
}

/// The model's command out of an `mcpToolCall`'s arguments: the `command`
/// string of the observed object shape. `None` when the call names none
/// (list, open, send, close) — the card then names the tool, never a
/// guessed command.
fn mcp_command(arguments: &Value) -> Option<String> {
    arguments
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
}

/// Whether an `mcpToolCall` error refuses the call rather than failing
/// it: the wire reports a declined elicitation as a failed call with a
/// rejection message (`user rejected MCP tool call`), and the card must
/// read denied — never a success tick, never a forged run.
fn is_mcp_rejection(message: &str) -> bool {
    message.to_lowercase().contains("reject")
}

/// The joined `text` content of an `mcpToolCall` result
/// (`McpToolCallResult.content[]`): what a terminal tool's JSON answer
/// rides in. `None` when the call reported no text — never an empty
/// string standing in for output.
fn mcp_result_text(result: &Value) -> Option<String> {
    let mut text = String::new();
    for item in result.get("content")?.as_array()? {
        if item.get("type").and_then(Value::as_str) == Some("text") {
            text.push_str(item.get("text").and_then(Value::as_str).unwrap_or_default());
        }
    }
    (!text.is_empty()).then_some(text)
}

/// One terminal tool's JSON answer (`docs/14-terminal.md` §4), read off
/// an `mcpToolCall` result's text: `{tab, block, status, exit_code?,
/// duration_ms, output, …}`. `None` when the text is not that shape —
/// the card then bodies the text itself, never a forged parse.
struct TerminalMcpResult {
    output_lines: Option<Vec<String>>,
    exit_code: Option<i32>,
    tab: Option<String>,
    duration_ms: Option<u64>,
}

fn parse_terminal_mcp_result(text: &str) -> Option<TerminalMcpResult> {
    let object = serde_json::from_str::<Value>(text.trim()).ok()?;
    let object = object.as_object()?;
    let output_lines = object
        .get("output")
        .and_then(Value::as_str)
        .map(|output| output.lines().map(str::to_owned).collect());
    let exit_code = object
        .get("exit_code")
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    let tab = object
        .get("tab")
        .and_then(Value::as_str)
        .filter(|tab| !tab.trim().is_empty())
        .map(str::to_owned);
    let duration_ms = object.get("duration_ms").and_then(Value::as_u64);
    Some(TerminalMcpResult { output_lines, exit_code, tab, duration_ms })
}

/// The exit code an `mcpToolCall` item reports, when it reports one: a
/// terminal tool's answer parsed for `exit_code`. `None` for anything
/// else — never 0 by assumption.
fn mcp_exit_code(item: &Item) -> Option<i32> {
    let text = item.mcp_result().and_then(mcp_result_text)?;
    parse_terminal_mcp_result(&text)?.exit_code
}

/// One baaz terminal `mcpToolCall` as its shell card (D51): the header
/// names the call's command (never the tool, never raw JSON), the body
/// is the run's output, and the status is the wire's — a rejected call
/// reads denied, a failed one failed, a still-open one running. Only a
/// clean `completed` reads success, and a nonzero exit code fails it
/// closed to error.
fn terminal_mcp_card(item: &Item) -> (ToolStatus, Block) {
    let tool = item.mcp_tool();
    let command =
        mcp_command(item.mcp_arguments()).unwrap_or_else(|| tool.to_owned());
    let text = item.mcp_result().and_then(mcp_result_text);
    let parsed = text.as_deref().and_then(parse_terminal_mcp_result);
    let rejected = item.mcp_error().is_some_and(is_mcp_rejection);
    let status = match (item.status(), item.mcp_error()) {
        ("inProgress", _) => ToolStatus::Running,
        (_, Some(_)) if rejected => ToolStatus::Cancelled,
        (_, Some(_)) => ToolStatus::Error,
        ("completed", None) => match parsed.as_ref().and_then(|parsed| parsed.exit_code) {
            Some(0) | None => ToolStatus::Success,
            Some(_) => ToolStatus::Error,
        },
        // `failed` without an error message, and any unknown future
        // status: failed closed to error, never back to a spinner.
        _ => ToolStatus::Error,
    };
    let verb = match status {
        ToolStatus::Running => TERMINAL_RUNNING_VERB,
        ToolStatus::Cancelled => "Denied",
        _ => TERMINAL_RAN_VERB,
    };
    let tab = parsed.as_ref().and_then(|parsed| parsed.tab.clone());
    let target = match tab {
        Some(tab) if tab.trim() != command.trim() => format!("{command} · {tab}"),
        _ => command,
    };
    let error_lines: Vec<String> = item
        .mcp_error()
        .map(|error| error.lines().map(str::to_owned).collect())
        .unwrap_or_default();
    let output_lines = match (&status, parsed.as_ref().and_then(|parsed| parsed.output_lines.clone())) {
        (_, Some(lines)) => lines,
        (ToolStatus::Running, None) => Vec::new(),
        (_, None) => text
            .map(|text| text.lines().map(str::to_owned).collect())
            .unwrap_or(error_lines),
    };
    let exit_code = parsed.as_ref().and_then(|parsed| parsed.exit_code);
    let duration_ms =
        item.mcp_duration_ms().or_else(|| parsed.as_ref().and_then(|parsed| parsed.duration_ms));
    let card = Block::ToolCall {
        id: item.id().to_owned(),
        kind: ToolKind::Shell,
        verb: verb.into(),
        target,
        status,
        duration_ms,
        body: ToolBody::Shell { output_lines, exit_code, live: false },
        diff_stat: None,
    };
    (status, card)
}



/// The `(server, tool)` an MCP tool-call elicitation gates, parsed out
/// of Codex's own gate message: `Allow the <server> MCP server to run
/// tool "<tool>"?`. Strict — anything else is not a gate this fold
/// names a tool for — and the server must equal the request's
/// `serverName`, so a mismatched message never mislabels the call.
fn parse_tool_gate(server_name: &str, message: &str) -> Option<(String, String)> {
    let rest = message.strip_prefix("Allow the ")?;
    let (server, rest) = rest.split_once(" MCP server to run tool \"")?;
    let tool = rest.strip_suffix('?').filter(|rest| rest.ends_with('"'))?;
    let tool = tool.strip_suffix('"')?;
    if server.trim().is_empty()
        || server != server_name
        || tool.trim().is_empty()
        || tool.contains('"')
    {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

/// The model's command with the runner unwrapped: the server wraps what
/// the model asked for in `<shell> -c '<inner>'` (single- or
/// double-quoted), and the card titles the inner command — the wrapper
/// is how it ran, not what was asked. Unwraps one layer only; anything
/// else renders verbatim, and the full wrapper stays in the fixture.
fn display_command(command: &str) -> String {
    let Some(arg) = shell_wrapper_arg(command) else { return command.to_owned() };
    // The wrapper takes exactly one argument: anything that is not one
    // shell word (extra words, trailing garbage, unterminated quotes) is
    // not a clean wrapper invocation — leave the whole command verbatim
    // rather than guessing which half the model meant.
    parse_shell_word(arg)
        .filter(|(_, rest)| rest.is_empty())
        .map(|(word, _)| word)
        .unwrap_or_else(|| command.to_owned())
}

/// The single argument of a `-c` wrapper invocation, when `command` is
/// one: `<shell> (-c | -lc) <arg>` where the shell is `sh`, `bash`, or
/// `zsh` with an optional `/bin/` prefix. A command that merely starts
/// with a shell path but is not a `-c` wrapper (`/bin/zsh script.sh`)
/// is not unwrapped.
fn shell_wrapper_arg(command: &str) -> Option<&str> {
    // Longest prefixes first so `/bin/` never shadows the bare name.
    for shell in ["/bin/bash", "/bin/zsh", "/bin/sh", "bash", "zsh", "sh"] {
        for flag in ["-lc", "-c"] {
            let prefix = format!("{shell} {flag} ");
            if let Some(arg) = command.strip_prefix(prefix.as_str()) {
                return Some(arg);
            }
        }
    }
    None
}

/// Parse one POSIX shell word from the front of `input`, returning the
/// word's value plus the unparsed remainder. Single quotes carry
/// everything literally (so `'it'\''s'` is `it's`); inside double quotes
/// a backslash escapes only `"`, `\`, `$`, `` ` ``, and newline (so
/// `"a\"b"` is `a"b`); elsewhere a backslash escapes the next byte.
/// An unterminated quote is not a word — `None`.
fn parse_shell_word(input: &str) -> Option<(String, &str)> {
    let mut word = String::new();
    let mut chars = input.char_indices().peekable();
    match chars.peek() {
        Some((_, first)) if !first.is_whitespace() => {}
        _ => return None,
    }
    // The unparsed remainder: the whitespace run where the word stopped,
    // or empty when the word ran to the end of the string.
    let mut rest = "";
    while let Some((index, char)) = chars.next() {
        if char.is_whitespace() {
            rest = &input[index..];
            break;
        }
        match char {
            '\'' => {
                let mut closed = false;
                for (_, char) in chars.by_ref() {
                    if char == '\'' {
                        closed = true;
                        break;
                    }
                    word.push(char);
                }
                if !closed {
                    return None;
                }
            }
            '"' => {
                let mut closed = false;
                let mut escaped = false;
                for (_, char) in chars.by_ref() {
                    if escaped {
                        // Inside double quotes a backslash escapes only
                        // `"`, `\`, `$`, backtick, and newline; otherwise
                        // both bytes survive.
                        if !matches!(char, '"' | '\\' | '$' | '`' | '\n') {
                            word.push('\\');
                        }
                        word.push(char);
                        escaped = false;
                    } else if char == '\\' {
                        escaped = true;
                    } else if char == '"' {
                        closed = true;
                        break;
                    } else {
                        word.push(char);
                    }
                }
                if !closed {
                    return None;
                }
            }
            '\\' => {
                // A trailing backslash escapes nothing: not a word.
                let (_, next) = chars.next()?;
                word.push(next);
            }
            char => word.push(char),
        }
    }
    Some((word, rest))
}

/// The `+N/−N` chip counts over one `fileChange` item's own change
/// entries: `add` entries count their new-content lines; anything else
/// counts unified-diff `+`/`-` lines (`+++`/`---` headers skipped).
/// `files` is the entry count, never a line count.
fn file_change_stat(changes: &[FileChangeEntry]) -> DiffStat {
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for change in changes {
        if change.kind == "add" {
            added += change.diff.lines().count() as u64;
            continue;
        }
        for line in change.diff.lines() {
            if let Some(rest) = line.strip_prefix('+') {
                if !rest.starts_with('+') {
                    added += 1;
                }
            } else if let Some(rest) = line.strip_prefix('-') {
                if !rest.starts_with('-') {
                    removed += 1;
                }
            }
        }
    }
    DiffStat { added, removed, files: changes.len() as u64 }
}

/// One display hunk per change entry: `add` content as all-addition
/// rows, unified diffs parsed into add/del/context rows (line numbers
/// run from 1 within the entry — display order, not file offsets).
fn file_change_diff(path: &str, changes: &[FileChangeEntry]) -> Diff {
    let mut hunks = Vec::new();
    let mut total_added: u32 = 0;
    let mut total_removed: u32 = 0;
    for change in changes {
        let mut lines = Vec::new();
        let mut new_no: u32 = 1;
        let mut old_no: u32 = 1;
        if change.kind == "add" {
            for line in change.diff.lines() {
                lines.push(DiffLine {
                    kind: DiffKind::Add,
                    old_no: None,
                    new_no: Some(new_no),
                    text: line.to_owned(),
                });
                new_no += 1;
                total_added += 1;
            }
        } else {
            for line in change.diff.lines() {
                if let Some(rest) = line.strip_prefix('+') {
                    if rest.starts_with('+') {
                        continue;
                    }
                    lines.push(DiffLine {
                        kind: DiffKind::Add,
                        old_no: None,
                        new_no: Some(new_no),
                        text: rest.to_owned(),
                    });
                    new_no += 1;
                    total_added += 1;
                } else if let Some(rest) = line.strip_prefix('-') {
                    if rest.starts_with('-') {
                        continue;
                    }
                    lines.push(DiffLine {
                        kind: DiffKind::Del,
                        old_no: Some(old_no),
                        new_no: None,
                        text: rest.to_owned(),
                    });
                    old_no += 1;
                    total_removed += 1;
                } else {
                    let text = line.strip_prefix(' ').unwrap_or(line);
                    lines.push(DiffLine {
                        kind: DiffKind::Context,
                        old_no: Some(old_no),
                        new_no: Some(new_no),
                        text: text.to_owned(),
                    });
                    old_no += 1;
                    new_no += 1;
                }
            }
        }
        hunks.push(Hunk {
            header: format!("@@ {} @@", change.path),
            lines,
        });
    }
    Diff { path: path.to_owned(), hunks, added: total_added, removed: total_removed }
}

/// The money guard's feed: the latest `account/rateLimits/updated` push, kept
/// beside the seam's account shape. The window lengths ride along with the
/// percents so the usage card can label each window from its length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSnapshot {
    /// Primary window usage percent, when reported.
    pub used_percent: Option<u64>,
    /// Primary window length in minutes, when reported.
    pub primary_minutes: Option<u64>,
    /// Plan name (`prolite`, …), when reported.
    pub plan: Option<String>,
    /// Unix time the window resets, when reported.
    pub resets_at: Option<u64>,
    /// Secondary window usage percent, when reported.
    pub secondary_used_percent: Option<u64>,
    /// Secondary window length in minutes, when reported.
    pub secondary_minutes: Option<u64>,
    /// Unix time the secondary window resets, when reported.
    pub secondary_resets_at: Option<u64>,
}

impl AccountSnapshot {
    /// Record a push. Unknown fields are ignored; a push that names no
    /// window at all still counts as "a reading was seen". Accepts either
    /// the push params (`{rateLimits: …}`) or the bare limits object the
    /// `account/rateLimits/read` answer carries — same object, either
    /// wrapping.
    pub fn observe(&mut self, params: &Value) {
        let limits = params.get("rateLimits").unwrap_or(params);
        let primary = limits.get("primary");
        if let Some(used) =
            primary.and_then(|primary| primary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.used_percent = Some(used);
        }
        if let Some(minutes) = primary
            .and_then(|primary| primary.get("windowDurationMins"))
            .and_then(Value::as_u64)
        {
            self.primary_minutes = Some(minutes);
        }
        if let Some(plan) = limits.get("planType").and_then(Value::as_str) {
            self.plan = Some(plan.to_owned());
        }
        if let Some(resets) = primary
            .and_then(|primary| primary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.resets_at = Some(resets);
        }
        let secondary = limits.get("secondary");
        if let Some(used) =
            secondary.and_then(|secondary| secondary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.secondary_used_percent = Some(used);
        }
        if let Some(minutes) = secondary
            .and_then(|secondary| secondary.get("windowDurationMins"))
            .and_then(Value::as_u64)
        {
            self.secondary_minutes = Some(minutes);
        }
        if let Some(resets) = secondary
            .and_then(|secondary| secondary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.secondary_resets_at = Some(resets);
        }
    }

    /// The structured usage reading for the seam: the plan plus one window
    /// per reported percent, labelled from each window's length. `None`
    /// until a push or a rate-limits read has been seen.
    pub fn usage_report(&self) -> Option<provider::UsageReport> {
        if !self.has_reading() {
            return None;
        }
        let mut windows = Vec::new();
        if let Some(used) = self.used_percent {
            windows.push(provider::UsageWindow {
                label: self
                    .primary_minutes
                    .map(provider::window_label)
                    .unwrap_or_else(|| "Primary".into()),
                used_fraction: (used as f64 / 100.0).clamp(0.0, 1.0),
                resets_at: self.resets_at.map(|resets| resets as i64),
                window_minutes: self.primary_minutes,
            });
        }
        if let Some(used) = self.secondary_used_percent {
            windows.push(provider::UsageWindow {
                label: self
                    .secondary_minutes
                    .map(provider::window_label)
                    .unwrap_or_else(|| "Secondary".into()),
                used_fraction: (used as f64 / 100.0).clamp(0.0, 1.0),
                resets_at: self.secondary_resets_at.map(|resets| resets as i64),
                window_minutes: self.secondary_minutes,
            });
        }
        Some(provider::UsageReport { plan: self.plan.clone(), windows })
    }

    /// Whether any push has been seen at all.
    pub fn has_reading(&self) -> bool {
        self.used_percent.is_some() || self.plan.is_some() || self.resets_at.is_some()
    }

    /// The display label for [`provider::Ack::Account`]. `None` until the
    /// first push arrives.
    pub fn label(&self) -> Option<String> {
        if !self.has_reading() {
            return None;
        }
        let plan = self.plan.as_deref().unwrap_or("codex");
        let mut label = match self.used_percent {
            Some(used) => format!("Codex {plan} {used}%"),
            None => format!("Codex {plan}"),
        };
        if let Some(resets) = self.resets_at {
            label.push_str(&format!(" (resets {resets})"));
        }
        Some(label)
    }
}

/// The wire's plan step status vocabulary onto the transcript's: the
/// same mapping Claude Code's TodoWrite uses, so both providers render
/// one plan/todo block shape.
fn map_plan_state(status: &str) -> TodoState {
    match status {
        "inProgress" => TodoState::Running,
        "completed" => TodoState::Done,
        _ => TodoState::Pending,
    }
}

/// Where a pending approval card lives, carrying its own pending face so
/// the item's completion can settle it without rebuilding anything.
#[derive(Clone, Debug)]
struct CodexApprovalSite {
    /// The turn hosting the card.
    turn_id: String,
    /// The card's block index in that turn, for wholesale updates.
    block_index: usize,
    /// The pending card itself: the completion only flips its state.
    card: Block,
}

/// A pending MCP tool-call elicitation's card, keyed by the gated call's
/// `(turn, server, tool)` triple: the elicitation names no item id, so
/// its `mcpToolCall` item cannot address the card directly, and the
/// triple is the join the item settles through. The first pending
/// elicitation for a triple wins.
type ElicitationSite = String;

/// The three answers an MCP tool-call elicitation takes — allow once,
/// deny, deny and stop. The ids ride `DecideApproval` verbatim: the
/// adapter answers exactly these three on the elicitation lane, so the
/// card declares exactly these three. There is deliberately no
/// allow-for-session: the elicitation response schema admits no scope,
/// and a card must never offer what the wire cannot carry.
fn elicitation_choices() -> Vec<ApprovalChoice> {
    vec![
        ApprovalChoice {
            id: "accept".into(),
            label: "Allow once".into(),
            decision: ApprovalDecision::Once,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
        ApprovalChoice {
            id: "decline".into(),
            label: "Deny".into(),
            decision: ApprovalDecision::Deny,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
        ApprovalChoice {
            id: "cancel".into(),
            label: "Deny and stop".into(),
            decision: ApprovalDecision::Abort,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
    ]
}

/// The four plain decision tokens the command and file-change lanes
/// answer — allow once, allow for this session, deny, deny and stop.
/// The ids ride `DecideApproval` verbatim: the adapter answers exactly
/// these four on both lanes, so the card declares exactly these four.
fn codex_choices() -> Vec<ApprovalChoice> {
    vec![
        ApprovalChoice {
            id: "accept".into(),
            label: "Allow once".into(),
            decision: ApprovalDecision::Once,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
        ApprovalChoice {
            id: "accept-for-session".into(),
            label: "Allow for this session".into(),
            decision: ApprovalDecision::ApprovedForSession,
            scope: ApprovalScope::ThisSession,
            rule_preview: None,
            accepts_feedback: false,
        },
        ApprovalChoice {
            id: "decline".into(),
            label: "Deny".into(),
            decision: ApprovalDecision::Deny,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
        ApprovalChoice {
            id: "cancel".into(),
            label: "Deny and stop".into(),
            decision: ApprovalDecision::Abort,
            scope: ApprovalScope::ThisCommand,
            rule_preview: None,
            accepts_feedback: false,
        },
    ]
}

/// The fold: decoded frames in, [`Delta`]s out.
///
/// Stateful only where the wire is relational: turn id → started turn (many
/// items share one turn), `(thread_id, turn_id)` → per-turn tokens (usage
/// arrives on its own notification, after the text), and the server-minted
/// session mapping.
#[derive(Clone, Debug, Default)]
pub struct CodexFold {
    session_id: Option<String>,
    thread_id: Option<String>,
    model: Option<String>,
    assistant_started: HashSet<String>,
    /// Blocks added per turn, in order: the index a later `turn/plan/updated`
    /// rewrites wholesale (replace, never append).
    turn_blocks: HashMap<String, usize>,
    /// Turn id → the block index its plan card was added at, while the
    /// turn is open. One card per turn no matter how many updates refine
    /// it — the same shape Claude Code's TodoWrite produces.
    plan_cards: HashMap<String, usize>,
    /// The fallback lane: streamed `item/agentMessage/delta` text per
    /// `(turn_id, item_id)`, in first-seen order. Dropped when an
    /// `item/completed` closes the item; flushed at `turn/completed` when
    /// the turn was interrupted before any completed frame arrived.
    delta_text: HashMap<(String, String), String>,
    delta_order: Vec<(String, String)>,
    user_started: HashSet<String>,
    /// Full submitted text → the bubble text (`SubmitInput.display_text`):
    /// the provider echoes the whole input, so the user turn shows the
    /// short text instead. The turn-keyed map below wins when the echo
    /// names its turn; this one covers echoes that do not.
    display_by_text: HashMap<String, String>,
    /// Turn id from the submit ack → the bubble text: the join the submit
    /// creates. Recorded when `turn/start` answers, read when the turn's
    /// `userMessage` item completes (live and `thread/resume` alike).
    display_by_turn: HashMap<String, String>,
    usage: HashMap<(String, String), TokenCounts>,
    account: AccountSnapshot,
    current_turn: Option<String>,
    /// History turn ids already folded from a `thread/resume` response, so
    /// a second resume folds nothing twice. Live turns never land here —
    /// a resumed turn is completed, and the server never re-emits it.
    resumed_turns: HashSet<String>,
    /// Wire item id → where its pending approval card lives, so the
    /// item's own completion can settle the card: allowed when the item
    /// ran, denied when it completed declined. Deciding queues the
    /// answer through `DecideApproval`; only the completing item moves
    /// the card — never the press.
    approval_sites: HashMap<String, CodexApprovalSite>,
    /// `(turn, server, tool)` → the pending MCP tool-call elicitation
    /// gating that call (its card's approval id). The elicitation names
    /// no item id, so this is the join its `mcpToolCall` item settles
    /// through.
    elicitation_sites: HashMap<(String, String, String), ElicitationSite>,
}

impl CodexFold {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// The server-minted session id, once `thread/started` has been seen.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The latest turn id seen on `turn/started`, if any.
    pub fn current_turn(&self) -> Option<&str> {
        self.current_turn.as_deref()
    }

    /// Remember the effective model (from `model/list` or the turn params):
    /// it lands in the [`TurnMeta`] footer.
    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_owned());
    }

    /// Remember the bubble text for a submit carrying `display_text`.
    /// `turn_id` is the submit ack's turn — the join the submit creates;
    /// `full_text` is the model-visible input the echo carries back. An
    /// empty display, or one identical to the input, records nothing, so
    /// the typed text renders as before.
    pub fn record_display_text(&mut self, turn_id: Option<&str>, full_text: &str, display: &str) {
        if display.is_empty() || display == full_text {
            return;
        }
        if full_text.len() > 131_072 {
            return;
        }
        if let Some(turn_id) = turn_id.filter(|id| !id.is_empty()) {
            if self.display_by_turn.len() < 64 {
                self.display_by_turn.insert(turn_id.to_owned(), display.to_owned());
            }
        }
        if self.display_by_text.len() < 64 {
            self.display_by_text.insert(full_text.to_owned(), display.to_owned());
        }
    }

    /// The bubble text for an echoed user turn, if a submit recorded one:
    /// the turn join first, then the full input text.
    fn display_text_for<'a>(
        &'a self,
        turn_id: Option<&'a str>,
        full_text: &'a str,
    ) -> Option<&'a str> {
        turn_id
            .and_then(|id| self.display_by_turn.get(id))
            .or_else(|| self.display_by_text.get(full_text))
            .map(String::as_str)
    }

    /// The per-turn figure for `(thread_id, turn_id)`: the `last` bucket,
    /// never the cumulative `total`.
    pub fn usage_for(&self, thread_id: &str, turn_id: &str) -> Option<&TokenCounts> {
        self.usage.get(&(thread_id.to_owned(), turn_id.to_owned()))
    }

    /// The latest account reading (see [`AccountSnapshot`]).
    pub fn account(&self) -> &AccountSnapshot {
        &self.account
    }

    /// Fold an `account/rateLimits/read` answer into the account snapshot:
    /// the same object the `account/rateLimits/updated` push carries, so
    /// one observe path serves the lane's connect-time read and the live
    /// pushes alike.
    pub fn observe_rate_limits(&mut self, result: &Value) {
        self.account.observe(result);
    }

    /// Fold one resumed history turn's items plus its completion into
    /// render-ready deltas. Each history turn renders exactly like its
    /// live twin: every item through the same [`Self::apply_item`] lane
    /// an `item/completed` notification takes, then one [`Delta`] finish
    /// with the adapter's model in its footer (history turns carry no
    /// per-turn usage). A turn id already resumed folds to nothing, so
    /// repeating the resume never duplicates history — and a new turn
    /// afterwards appends after it, never inside it.
    ///
    /// Call with the adapter's model already set ([`Self::set_model`]):
    /// the finish footer reads it.
    pub fn apply_resume_turns(&mut self, thread_id: &str, turns: &[Value]) -> Vec<Delta> {
        let mut deltas = Vec::new();
        for turn in turns {
            let turn_id =
                turn.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            if turn_id.is_empty() || !self.resumed_turns.insert(turn_id.clone()) {
                continue;
            }
            let items = turn.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
            for item in &items {
                let item = decode_thread_item(item);
                deltas.extend(self.apply(&Frame::Notification(Notification::ItemCompleted {
                    thread_id: thread_id.to_owned(),
                    turn_id: turn_id.clone(),
                    item,
                })));
            }
            deltas.extend(self.apply(&Frame::Notification(Notification::TurnCompleted {
                thread_id: thread_id.to_owned(),
                turn_id: turn_id.clone(),
                status: turn
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("completed")
                    .to_owned(),
                duration_ms: turn.get("durationMs").and_then(Value::as_u64).unwrap_or(0),
            })));
        }
        deltas
    }

    /// Fold one decoded frame into render-ready deltas. Notifications
    /// render as the transcript; a server approval request cards its
    /// pending approval (the pump still routes the request itself — the
    /// answerable record and the tap — through [`crate::child`]).
    /// Anything else contributes nothing here.
    pub fn apply(&mut self, frame: &Frame) -> Vec<Delta> {
        match frame {
            Frame::Notification(notification) => self.apply_notification(notification),
            Frame::Request { id, method, params } => self.apply_request(method, params, id),
            Frame::Response { .. } | Frame::ResponseError { .. } => Vec::new(),
        }
    }

    /// Fold one server approval request into its pending approval card.
    /// Non-approval requests contribute nothing: our own calls are the
    /// pump's routing business, never the transcript's. An
    /// `mcp_tool_call` elicitation cards the same inline approval every
    /// other gate gets; any other elicitation cards nothing — it stays a
    /// question on the answerable surface, never a forged approval.
    pub fn apply_request(&mut self, method: &str, params: &Value, id: &Value) -> Vec<Delta> {
        if method == "mcpServer/elicitation/request" {
            if elicitation_is_tool_approval(params) {
                return self.apply_mcp_elicitation(params, id);
            }
            return Vec::new();
        }
        match ApprovalKind::from_method(method) {
            Some(ApprovalKind::Command) => self.apply_command_approval(params),
            Some(ApprovalKind::FileChange) => self.apply_file_change_approval(params),
            Some(ApprovalKind::Permissions) => self.apply_permissions_approval(params),
            Some(ApprovalKind::McpElicitation) | Some(ApprovalKind::Unknown) => {
                self.apply_unknown_approval(method, params)
            }
            None => Vec::new(),
        }
    }

    /// Card one approval request: ensure its turn, push the pending card,
    /// and record the site its completing item settles. A repeat request
    /// for an already-carded item cards nothing twice — the card moves
    /// only on the item's completion, never on a re-request.
    fn card_approval(&mut self, item_id: &str, turn_id: &str, card: Block, deltas: &mut Vec<Delta>) {
        if item_id.is_empty() || self.approval_sites.contains_key(item_id) {
            return;
        }
        self.ensure_assistant(turn_id, deltas);
        let block_index = self.turn_blocks.get(turn_id).copied().unwrap_or(0);
        self.push_block(turn_id, card.clone(), deltas);
        self.approval_sites.insert(
            item_id.to_owned(),
            CodexApprovalSite { turn_id: turn_id.to_owned(), block_index, card },
        );
    }

    /// Settle one carded approval to its decided state: the only thing that
    /// ever moves the card after the press. A repeat resolution for an
    /// already-settled item is a no-op.
    fn resolve_approval(&mut self, item_id: &str, state: ApprovalState, deltas: &mut Vec<Delta>) {
        let Some(site) = self.approval_sites.remove(item_id) else { return };
        let mut card = site.card;
        if let Block::Approval { state: slot, .. } = &mut card {
            *slot = state;
        }
        deltas.push(Delta::BlockUpdated {
            turn_id: site.turn_id,
            block_index: site.block_index,
            block: card,
        });
    }

    /// The owning turn of an approval request: the request's own turn, else
    /// the latest turn seen. `None` means the request names no turn and no
    /// turn ever opened — nothing to host the card.
    fn approval_turn(&self, params: &Value) -> Option<String> {
        let turn = params.get("turnId").and_then(Value::as_str).unwrap_or("");
        if !turn.is_empty() {
            return Some(turn.to_owned());
        }
        self.current_turn.clone()
    }

    /// `item/commandExecution/requestApproval`: the command with its cwd
    /// and the model's reason — the command card's face, never raw JSON.
    fn apply_command_approval(&mut self, params: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let item_id = params.get("itemId").and_then(Value::as_str).unwrap_or_default();
        let Some(turn_id) = self.approval_turn(params) else { return deltas };
        let command = params.get("command").and_then(Value::as_str).unwrap_or_default();
        let target =
            if command.trim().is_empty() { "a command".to_owned() } else { display_command(command) };
        let cwd = params.get("cwd").and_then(Value::as_str).unwrap_or_default().to_owned();
        let reason = params.get("reason").and_then(Value::as_str).unwrap_or_default();
        let reason =
            if reason.trim().is_empty() { "The model did not say why.".to_owned() } else { reason.to_owned() };
        self.card_approval(
            item_id,
            &turn_id,
            Block::Approval {
                id: item_id.to_owned(),
                tool: "Bash".to_owned(),
                command: target,
                reason,
                cwd,
                capabilities: Vec::new(),
                scope: ApprovalScope::ThisCommand,
                body_kind: ApprovalBodyKind::Command,
                state: ApprovalState::Pending,
                rule: None,
                choices: codex_choices(),
                stages: Vec::new(),
                current_stage: None,
                badges: ApprovalBadges::default(),
                feedback: None,
                resolved_by: None,
            },
            &mut deltas,
        );
        deltas
    }

    /// `item/fileChange/requestApproval`: the request itself names no path
    /// and no diff (both arrive on the completing item), so the pending
    /// card carries the model's reason — or says it has none — and the
    /// completion below it carries the path and the diff.
    fn apply_file_change_approval(&mut self, params: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let decoded = FileChangeApprovalParams::decode(params);
        let Some(turn_id) = self.approval_turn(params) else { return deltas };
        let (item_id, reason) = match &decoded {
            Some(decoded) => (
                decoded.item_id.clone(),
                decoded.reason.clone().unwrap_or_default(),
            ),
            None => (
                params.get("itemId").and_then(Value::as_str).unwrap_or_default().to_owned(),
                params.get("reason").and_then(Value::as_str).unwrap_or_default().to_owned(),
            ),
        };
        let grant = decoded
            .as_ref()
            .and_then(|decoded| decoded.grant_root.clone())
            .unwrap_or_default();
        let command =
            if grant.trim().is_empty() { "File change".to_owned() } else { grant.trim().to_owned() };
        let reason = if reason.trim().is_empty() {
            if grant.trim().is_empty() {
                "The model did not say why.".to_owned()
            } else {
                format!("Write access under {}", grant.trim())
            }
        } else {
            reason
        };
        self.card_approval(
            &item_id,
            &turn_id,
            Block::Approval {
                id: item_id.clone(),
                tool: "Edit".to_owned(),
                command,
                reason,
                cwd: String::new(),
                capabilities: Vec::new(),
                scope: ApprovalScope::ThisCommand,
                body_kind: ApprovalBodyKind::FileWrite,
                state: ApprovalState::Pending,
                rule: None,
                choices: codex_choices(),
                stages: Vec::new(),
                current_stage: None,
                badges: ApprovalBadges::default(),
                feedback: None,
                resolved_by: None,
            },
            &mut deltas,
        );
        deltas
    }

    /// `item/permissions/requestApproval`: the lane takes no `decision`
    /// token — only a JSON grant — so the card offers exactly the two
    /// grants the adapter can answer: what was requested, scoped to this
    /// turn or this session. The choice id IS the answer shape, riding
    /// `DecideApproval` verbatim.
    fn apply_permissions_approval(&mut self, params: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let Some(decoded) = PermissionsApprovalParams::decode(params) else { return deltas };
        let Some(turn_id) = self.approval_turn(params) else { return deltas };
        let mut lines = Vec::new();
        if let Some(reason) = decoded.reason.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            lines.push(reason.to_owned());
        }
        lines.push(format!("Applies to {}", decoded.cwd));
        if let Some(object) = decoded.permissions.as_object() {
            let mut keys: Vec<&str> =
                object.keys().take(5).map(String::as_str).collect();
            if object.len() > keys.len() {
                keys.push("…");
            }
            if !keys.is_empty() {
                lines.push(format!("Requests {}", keys.join(", ")));
            }
        }
        let grant = |scope: &str| {
            serde_json::json!({
                "permissions": decoded.permissions,
                "scope": scope,
            })
            .to_string()
        };
        self.card_approval(
            &decoded.item_id,
            &turn_id,
            Block::Approval {
                id: decoded.item_id.clone(),
                tool: "Permissions".to_owned(),
                command: "Permissions grant".to_owned(),
                reason: lines.join("\n"),
                cwd: decoded.cwd.clone(),
                capabilities: Vec::new(),
                scope: ApprovalScope::ThisCommand,
                body_kind: ApprovalBodyKind::Other,
                state: ApprovalState::Pending,
                rule: None,
                choices: vec![
                    ApprovalChoice {
                        id: grant("turn"),
                        label: "Allow for this turn".into(),
                        decision: ApprovalDecision::Once,
                        scope: ApprovalScope::ThisCommand,
                        rule_preview: None,
                        accepts_feedback: false,
                    },
                    ApprovalChoice {
                        id: grant("session"),
                        label: "Allow for this session".into(),
                        decision: ApprovalDecision::ApprovedForSession,
                        scope: ApprovalScope::ThisSession,
                        rule_preview: None,
                        accepts_feedback: false,
                    },
                ],
                stages: Vec::new(),
                current_stage: None,
                badges: ApprovalBadges::default(),
                feedback: None,
                resolved_by: None,
            },
            &mut deltas,
        );
        deltas
    }

    /// An `mcp_tool_call` elicitation: the gate Codex puts in front of the
    /// model's MCP tool calls. It cards the same inline approval every
    /// other gate gets — provider-aware title through the card's tool,
    /// the gated command, Allow/Deny number-key choices — and the
    /// completing `mcpToolCall` item settles it: allowed when the call
    /// ran, denied when the call reports it rejected. The approval id is
    /// the pump's prefixed elicitation id, so the press routes back
    /// through the adapter to this exact server request.
    fn apply_mcp_elicitation(&mut self, params: &Value, id: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let Some(turn_id) = self.approval_turn(params) else { return deltas };
        let approval_id = format!("{MCP_ELICITATION_ID_PREFIX}{id}");
        let server = params.get("serverName").and_then(Value::as_str).unwrap_or_default();
        let message = params.get("message").and_then(Value::as_str).unwrap_or_default();
        let meta = params.get("_meta");
        let tool_params =
            meta.and_then(|meta| meta.get("tool_params")).filter(|params| params.is_object());
        let (gated_server, gated_tool) =
            parse_tool_gate(server, message).unwrap_or((server.to_owned(), String::new()));
        // The terminal face needs the bare tool name: it is what the
        // approval title reads for its verb. Anything else rides
        // server-qualified, so the card never invents a tool it was not
        // told.
        let terminal =
            gated_server == crate::terminal::SERVER_NAME && is_terminal_tool(&gated_tool);
        let tool = if terminal {
            gated_tool.clone()
        } else if gated_tool.is_empty() {
            if server.is_empty() { "MCP tool".to_owned() } else { server.to_owned() }
        } else if server.is_empty() {
            gated_tool.clone()
        } else {
            format!("{server}/{gated_tool}")
        };
        let command = tool_params
            .and_then(|params| params.get("command"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|command| !command.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| tool.clone());
        let mut reason = message.to_owned();
        if reason.trim().is_empty() {
            reason = format!("Allow the {server} MCP tool call?");
        }
        let description = meta
            .and_then(|meta| meta.get("tool_description"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .filter(|description| *description != message);
        if let Some(description) = description {
            reason.push('\n');
            reason.push_str(description);
        }
        self.card_approval(
            &approval_id,
            &turn_id,
            Block::Approval {
                id: approval_id.clone(),
                tool,
                command,
                reason,
                cwd: String::new(),
                capabilities: Vec::new(),
                scope: ApprovalScope::ThisCommand,
                body_kind: ApprovalBodyKind::Other,
                state: ApprovalState::Pending,
                rule: None,
                choices: elicitation_choices(),
                stages: Vec::new(),
                current_stage: None,
                badges: ApprovalBadges::default(),
                feedback: None,
                resolved_by: None,
            },
            &mut deltas,
        );
        // The join its item settles through: first pending elicitation
        // for the triple wins, and a repeat request for an already-carded
        // id keeps the first — the card moves only on the item's
        // completion, never on a re-request.
        self.elicitation_sites
            .entry((turn_id, gated_server, gated_tool))
            .or_insert(approval_id);
        deltas
    }

    /// Settle one pending MCP tool-call elicitation from its call's
    /// outcome: denied when the call reports it rejected, allowed when
    /// the call ran (even when it then failed) — the same ran/refused
    /// rule the command lane settles by. The join is the
    /// `(turn, server, tool)` triple the elicitation indexed; a call
    /// whose triple names nothing pending settles the turn's oldest
    /// instead, so a gate the message parse could not name still clears.
    fn settle_mcp_elicitation(
        &mut self,
        turn_id: &str,
        server: &str,
        tool: &str,
        item: &Item,
        deltas: &mut Vec<Delta>,
    ) {
        let key = (turn_id.to_owned(), server.to_owned(), tool.to_owned());
        let approval_id = if let Some(approval_id) = self.elicitation_sites.remove(&key) {
            approval_id
        } else {
            let fallback = self.elicitation_sites.iter().find_map(|(key, approval_id)| {
                (key.0 == turn_id).then(|| (key.clone(), approval_id.clone()))
            });
            match fallback {
                Some((key, approval_id)) => {
                    self.elicitation_sites.remove(&key);
                    approval_id
                }
                None => return,
            }
        };
        let rejected = item.mcp_error().is_some_and(is_mcp_rejection);
        let state = if rejected {
            ApprovalState::Denied
        } else {
            ApprovalState::AllowedOnce {
                exit_code: mcp_exit_code(item).unwrap_or(0),
                duration_ms: item.mcp_duration_ms().unwrap_or(0),
            }
        };
        self.resolve_approval(&approval_id, state, deltas);
    }

    /// A future `*requestApproval` kind: surfaced as a pending marker, never
    /// a forged approval — no choice shape exists for it, and inventing one
    /// would answer blind. The pump's tap still records it answerable-side.
    fn apply_unknown_approval(&mut self, method: &str, params: &Value) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let Some(turn_id) = self.approval_turn(params) else { return deltas };
        self.ensure_assistant(&turn_id, &mut deltas);
        self.push_block(
            &turn_id,
            Block::Generic {
                kind: "codex-approval".into(),
                status: "pending".into(),
                text: format!(
                    "The server asked for an approval this client does not know how to answer ({method})."
                ),
            },
            &mut deltas,
        );
        deltas
    }

    fn apply_notification(&mut self, notification: &Notification) -> Vec<Delta> {
        match notification {
            Notification::ThreadStarted { .. } => {
                self.thread_id = notification.thread_id().map(str::to_owned);
                self.session_id = notification.session_id().map(str::to_owned);
                Vec::new()
            }
            Notification::TurnStarted { .. } => {
                self.current_turn = notification.turn_id().map(str::to_owned);
                // Open the assistant turn the moment Codex starts (X1),
                // not at its first completed item: the view goes running
                // on this start, so a long-silent turn reads as working,
                // never hung. Idempotent — the first item's
                // `ensure_assistant` is then a no-op — so the final
                // transcript keeps one assistant turn per user turn.
                let mut deltas = Vec::new();
                if let Some(turn_id) = notification.turn_id() {
                    self.ensure_assistant(turn_id, &mut deltas);
                }
                deltas
            }
            Notification::TokenUsage { .. } => {
                if let (Some(thread), Some(turn), Some(usage)) = (
                    notification.thread_id(),
                    notification.turn_id(),
                    notification.usage(),
                ) {
                    self.usage.insert(
                        (thread.to_owned(), turn.to_owned()),
                        usage.per_turn().clone(),
                    );
                }
                Vec::new()
            }
            Notification::RateLimitsUpdated { .. } => {
                if let Some(params) = notification.rate_limit_params() {
                    self.account.observe(params);
                }
                Vec::new()
            }
            Notification::ItemCompleted { .. } => self.apply_item(notification),
            Notification::PlanUpdated { .. } => self.apply_plan(notification),
            Notification::AgentMessageDelta { turn_id, item_id, delta, .. } => {
                self.push_delta(turn_id, item_id, delta);
                Vec::new()
            }
            Notification::TurnCompleted { .. } => self.finish_turn(notification),
            // Status, MCP, error, warning, resolved, and unknown kinds move
            // no transcript.
            _ => Vec::new(),
        }
    }

    fn ensure_assistant(&mut self, turn_id: &str, deltas: &mut Vec<Delta>) {
        if self.assistant_started.insert(turn_id.to_owned()) {
            deltas.push(Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: turn_id.to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp: None,
                },
            });
        }
    }

    /// Push one block onto `turn_id` and count it: the count is the block
    /// index a later `turn/plan/updated` addresses for its wholesale
    /// rewrite. Only additions consume indices — updates address them.
    fn push_block(&mut self, turn_id: &str, block: Block, deltas: &mut Vec<Delta>) {
        deltas.push(Delta::BlockAdded { turn_id: turn_id.to_owned(), block });
        *self.turn_blocks.entry(turn_id.to_owned()).or_default() += 1;
    }

    /// Refine the turn's plan card from one `turn/plan/updated`: the first
    /// update opens the card, every later update rewrites it wholesale —
    /// replace, never append — so four updates still render one card. An
    /// empty plan opens nothing (but still clears a card already open).
    fn apply_plan(&mut self, notification: &Notification) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let (Some(turn_id), Some(plan)) =
            (notification.turn_id(), notification.plan()) else { return deltas };
        let block = Block::Todo {
            items: plan
                .iter()
                .map(|step| TodoItem {
                    label: step.step.clone(),
                    state: map_plan_state(&step.status),
                    elapsed_ms: None,
                })
                .collect(),
        };
        if let Some(block_index) = self.plan_cards.get(turn_id).copied() {
            deltas.push(Delta::BlockUpdated {
                turn_id: turn_id.to_owned(),
                block_index,
                block,
            });
        } else if !plan.is_empty() {
            self.ensure_assistant(turn_id, &mut deltas);
            let block_index = self.turn_blocks.get(turn_id).copied().unwrap_or(0);
            self.push_block(turn_id, block, &mut deltas);
            self.plan_cards.insert(turn_id.to_owned(), block_index);
        }
        deltas
    }

    /// Accumulate one streamed chunk on the fallback lane. Nothing renders
    /// here: the text only survives if `turn/completed` finds it still open.
    fn push_delta(&mut self, turn_id: &str, item_id: &str, delta: &str) {
        if delta.is_empty() {
            return;
        }
        let key = (turn_id.to_owned(), item_id.to_owned());
        if !self.delta_text.contains_key(&key) {
            self.delta_order.push(key.clone());
        }
        self.delta_text.entry(key).or_default().push_str(delta);
    }

    /// Forget one item's buffered deltas: an `item/completed` closed it, so
    /// the completed text renders whole and the fallback must not repeat it.
    fn drop_delta(&mut self, turn_id: &str, item_id: &str) {
        self.delta_text.remove(&(turn_id.to_owned(), item_id.to_owned()));
        self.delta_order
            .retain(|key| !(key.0 == turn_id && key.1 == item_id));
    }

    /// Drain every still-open buffer for `turn_id`, oldest item first. Items
    /// a completed frame already closed were dropped, so this is exactly the
    /// text no completed frame ever carried.
    fn take_pending_delta_text(&mut self, turn_id: &str) -> Vec<String> {
        let mut pending = Vec::new();
        let mut kept = Vec::new();
        for key in std::mem::take(&mut self.delta_order) {
            if key.0 == turn_id {
                if let Some(text) = self.delta_text.remove(&key) {
                    if !text.is_empty() {
                        pending.push(text);
                    }
                }
            } else {
                kept.push(key);
            }
        }
        self.delta_order = kept;
        pending
    }

    fn apply_item(&mut self, notification: &Notification) -> Vec<Delta> {
        let mut deltas = Vec::new();
        let (Some(turn_id), Some(item)) = (notification.turn_id(), notification.item()) else {
            return deltas;
        };
        match item.kind() {
            "agentMessage" => {
                self.drop_delta(turn_id, item.id());
                if item.text().is_empty() {
                    return deltas;
                }
                self.ensure_assistant(turn_id, &mut deltas);
                self.push_block(
                    turn_id,
                    Block::Text { text: item.text().to_owned(), streaming: false },
                    &mut deltas,
                );
            }
            "userMessage" => {
                // The echo carries the whole submitted input; a submit
                // that recorded `display_text` shows the short text
                // instead, while the model still received the full text.
                let full = item.text().to_owned();
                let text =
                    self.display_text_for(notification.turn_id(), &full).unwrap_or(&full).to_owned();
                if self.user_started.insert(item.id().to_owned()) {
                    deltas.push(Delta::TurnStarted {
                        turn: Turn::User {
                            id: item.id().to_owned(),
                            text,
                            attachments: item
                                .attachments()
                                .iter()
                                .map(|attachment| Attachment {
                                    name: attachment.name.clone(),
                                    kind: AttachmentKind::Image,
                                    size_bytes: None,
                                    meta: Some(attachment.path.clone()),
                                    state: UploadState::Ready,
                                })
                                .collect(),
                            mentions: Vec::new(),
                            timestamp: None,
                        },
                    });
                }
            }
            "reasoning" => {
                if item.text().is_empty() {
                    return deltas;
                }
                self.ensure_assistant(turn_id, &mut deltas);
                self.push_block(
                    turn_id,
                    Block::Thinking {
                        text: item.text().to_owned(),
                        elapsed_ms: 0,
                        summary: None,
                        state: ThinkingState::Done,
                    },
                    &mut deltas,
                );
            }
            "commandExecution" => {
                self.ensure_assistant(turn_id, &mut deltas);
                // Every `CommandExecutionStatus` in the schema, named: the
                // wire status is a plain string, so a future CLI version can
                // send a fifth value no arm names — that unknown fails closed
                // to `Error`, never back to a spinner that never stops.
                let status = match (item.status(), item.exit_code()) {
                    ("inProgress", _) => ToolStatus::Running,
                    ("completed", Some(0)) => ToolStatus::Success,
                    ("completed", _) => ToolStatus::Error,
                    ("failed", _) => ToolStatus::Error,
                    ("declined", _) => ToolStatus::Cancelled,
                    (_unknown, _) => ToolStatus::Error,
                };
                // A still-open execution reads pending; only a finished one
                // reads done, and a declined one reads denied.
                let verb = match status {
                    ToolStatus::Running => "Run",
                    ToolStatus::Cancelled => "Denied",
                    _ => "Ran",
                };
                let exit_code = item.exit_code().and_then(|code| i32::try_from(code).ok());
                self.push_block(
                    turn_id,
                    Block::ToolCall {
                        id: item.id().to_owned(),
                        kind: ToolKind::Shell,
                        verb: verb.into(),
                        target: display_command(item.command()),
                        status,
                        duration_ms: None,
                        body: ToolBody::Shell {
                            output_lines: item
                                .aggregated_output()
                                .map(|output| {
                                    output.lines().map(str::to_owned).collect::<Vec<_>>()
                                })
                                .unwrap_or_default(),
                            exit_code,
                            live: false,
                        },
                        diff_stat: None,
                    },
                    &mut deltas,
                );
                // A terminal completion settles the approval that gated this
                // item: allowed when it ran (even when it then failed), and
                // denied when it completed declined without running.
                if !matches!(status, ToolStatus::Running) {
                    let state = match status {
                        ToolStatus::Cancelled => ApprovalState::Denied,
                        _ => ApprovalState::AllowedOnce {
                            exit_code: exit_code.unwrap_or(0),
                            duration_ms: 0,
                        },
                    };
                    self.resolve_approval(item.id(), state, &mut deltas);
                }
            }
            "fileChange" => {
                self.ensure_assistant(turn_id, &mut deltas);
                // Same fail-closed status rule as shell: only a clean
                // `completed` reads as success.
                let status = match item.status() {
                    "inProgress" => ToolStatus::Running,
                    "completed" => ToolStatus::Success,
                    "declined" => ToolStatus::Cancelled,
                    _unknown => ToolStatus::Error,
                };
                let changes = item.changes();
                let is_write = !changes.is_empty()
                    && changes.iter().all(|change| change.kind == "add");
                let kind = if is_write { ToolKind::Write } else { ToolKind::Edit };
                // Pending while open, done once finished, denied when the
                // approval refused it — the same pending-until-ran rule as
                // shell cards.
                let verb = match status {
                    ToolStatus::Running => {
                        if is_write {
                            "Write"
                        } else {
                            "Edit"
                        }
                    }
                    ToolStatus::Cancelled => "Denied",
                    _ => {
                        if is_write {
                            "Wrote"
                        } else {
                            "Edited"
                        }
                    }
                };
                let path = changes
                    .first()
                    .map(|change| change.path.clone())
                    .unwrap_or_default();
                let stat = file_change_stat(changes);
                self.push_block(
                    turn_id,
                    Block::ToolCall {
                        id: item.id().to_owned(),
                        kind,
                        verb: verb.into(),
                        target: path.clone(),
                        status,
                        duration_ms: None,
                        body: ToolBody::Edit { diff: file_change_diff(&path, changes) },
                        diff_stat: Some(stat),
                    },
                    &mut deltas,
                );
                // A terminal completion settles the approval that gated this
                // change, the way executions settle theirs.
                if !matches!(status, ToolStatus::Running) {
                    let state = match status {
                        ToolStatus::Cancelled => ApprovalState::Denied,
                        _ => ApprovalState::AllowedOnce { exit_code: 0, duration_ms: 0 },
                    };
                    self.resolve_approval(item.id(), state, &mut deltas);
                }
            }
            "subAgentActivity" => {
                self.ensure_assistant(turn_id, &mut deltas);
                // One marker, one card: markers carry distinct item ids
                // even for one delegation (`subagent.jsonl` shows a
                // `started` and an `interacted` under different ids), so
                // joining them by id would forge what the wire keeps
                // apart. The nested transcript itself is never delivered
                // on this turn's items, so the card carries the
                // delegation — never a forged transcript.
                let status = if item.activity_kind() == "started" {
                    ToolStatus::Running
                } else {
                    ToolStatus::Success
                };
                let verb = match status {
                    ToolStatus::Running => "Delegate",
                    _ => "Delegated",
                };
                self.push_block(
                    turn_id,
                    Block::ToolCall {
                        id: item.id().to_owned(),
                        kind: ToolKind::SubAgent,
                        verb: verb.into(),
                        target: item.agent_path().to_owned(),
                        status,
                        duration_ms: None,
                        body: ToolBody::SubAgent { turns: Vec::new() },
                        diff_stat: None,
                    },
                    &mut deltas,
                );
            }
            "mcpToolCall" => self.apply_mcp_tool_call(turn_id, item, &mut deltas),
            _ => {
                self.ensure_assistant(turn_id, &mut deltas);
                self.push_block(
                    turn_id,
                    Block::Generic {
                        kind: item.kind().to_owned(),
                        status: item.status().to_owned(),
                        text: item.text().to_owned(),
                    },
                    &mut deltas,
                );
            }
        }
        deltas
    }

    /// One completed `mcpToolCall`: a terminal shell card for a baaz
    /// terminal tool — the command from the call's arguments, the run's
    /// output as the body — and a carried generic card for anything
    /// else. A rejected call reads denied, a failed one failed: never a
    /// success tick for a call that never ran. A settled (non-running)
    /// call settles the elicitation that gated it, allowed when the
    /// call ran and denied when it reports it rejected.
    fn apply_mcp_tool_call(&mut self, turn_id: &str, item: &Item, deltas: &mut Vec<Delta>) {
        self.ensure_assistant(turn_id, deltas);
        let server = item.mcp_server();
        let tool = item.mcp_tool();
        if server == crate::terminal::SERVER_NAME && is_terminal_tool(tool) {
            let (status, card) = terminal_mcp_card(item);
            self.push_block(turn_id, card, deltas);
            if !matches!(status, ToolStatus::Running) {
                self.settle_mcp_elicitation(turn_id, server, tool, item, deltas);
            }
            return;
        }
        // Not a baaz terminal call (no other server reaches a strict
        // session, but the fold must not forge one): carried with its
        // wire status and its refusal or failure message — never the
        // raw result JSON, never a success the wire did not report.
        let status = match item.status() {
            "inProgress" => ToolStatus::Running,
            "completed" if item.mcp_error().is_none() => ToolStatus::Success,
            _unknown => ToolStatus::Error,
        };
        self.push_block(
            turn_id,
            Block::Generic {
                kind: "mcpToolCall".into(),
                status: match status {
                    ToolStatus::Running => "inProgress".into(),
                    ToolStatus::Success => "completed".into(),
                    _ => "failed".into(),
                },
                text: item.mcp_error().unwrap_or_default().to_owned(),
            },
            deltas,
        );
        if !matches!(status, ToolStatus::Running) {
            self.settle_mcp_elicitation(turn_id, server, tool, item, deltas);
        }
    }

    fn finish_turn(&mut self, notification: &Notification) -> Vec<Delta> {
        let Some(turn_id) = notification.turn_id() else { return Vec::new() };
        let mut deltas = Vec::new();
        // The interrupted path never sends `item/completed` for the streamed
        // message, so flush whatever the delta lane still holds: without this
        // the whole turn folds to zero deltas — not even a `TurnFinished`.
        // Items a completed frame already closed were dropped from the
        // buffer, so this cannot double-render the normal path.
        for text in self.take_pending_delta_text(turn_id) {
            self.ensure_assistant(turn_id, &mut deltas);
            self.push_block(
                turn_id,
                Block::Text { text, streaming: false },
                &mut deltas,
            );
        }
        if !self.assistant_started.contains(turn_id) {
            // No content lane ever opened this turn and no deltas survived
            // it: finishing it would mint an empty turn the transcript never
            // had.
            return deltas;
        }
        let thread_id = notification.thread_id().unwrap_or_default();
        let last = notification
            .turn_id()
            .and_then(|turn| self.usage_for(thread_id, turn))
            .cloned()
            .unwrap_or_default();
        let meta = TurnMeta {
            model: self.model.clone().unwrap_or_default(),
            duration_ms: notification.duration_ms().unwrap_or(0),
            tokens_in: last.input_tokens,
            tokens_out: last.output_tokens,
            reasoning_tokens: last.reasoning_output_tokens,
            // No cost field exists anywhere in this protocol: 0.0 is the
            // neutral unknown default, never a measurement.
            cost_usd: 0.0,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cached_tokens: last.cached_input_tokens,
        };
        deltas.push(Delta::TurnFinished { turn_id: turn_id.to_owned(), meta });
        deltas
    }
}

/// Fold one raw live line: skip blanks and torn JSON, decode, fold, emit.
///
/// Notifications render the transcript and server approval requests card
/// their pending approvals; responses are the pump's routing business and
/// contribute nothing. The per-line unit of the live pump, factored out so
/// the fixture tests exercise the same decode-and-fold.
pub fn step_line(fold: &mut CodexFold, line: &str, emit: &mut impl FnMut(ProviderEvent)) {
    if line.trim().is_empty() {
        return;
    }
    let Ok(frame) = crate::frame::decode_line(line) else { return };
    if !matches!(frame, Frame::Notification(_) | Frame::Request { .. }) {
        return;
    }
    let session_id = fold.session_id().map(str::to_owned).or_else(|| match &frame {
        Frame::Notification(notification) => notification.thread_id().map(str::to_owned),
        Frame::Request { params, .. } => {
            params.get("threadId").and_then(Value::as_str).map(str::to_owned)
        }
        _ => None,
    });
    let deltas = fold.apply(&frame);
    if !deltas.is_empty() {
        emit(ProviderEvent::Deltas { session_id, deltas });
    }
}

/// Whether a recorded envelope line is an `item/agentMessage/delta` frame:
/// the ignored lane the agreement test strips.
#[cfg(test)]
fn is_agent_delta(line: &str) -> bool {
    serde_json::from_str::<Value>(line)
        .ok()
        .and_then(|envelope| envelope.get("frame").cloned())
        .and_then(|frame| frame.get("method").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        == Some("item/agentMessage/delta")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::decode_envelope;

    fn fixture_lines(name: &str) -> Vec<String> {
        let path = format!("{}/../../fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture reads").lines().map(str::to_owned).collect()
    }

    /// Replay every frame in `name` — both directions, in order — and return
    /// the folded deltas. Client frames contribute nothing; the assertion is
    /// that the whole file replays without loss and folds to its transcript.
    fn replay(name: &str) -> (CodexFold, Vec<Delta>) {
        let mut fold = CodexFold::new();
        let mut deltas = Vec::new();
        for line in fixture_lines(name) {
            let (_, frame) = decode_envelope(&line).expect("fixture decodes");
            deltas.extend(fold.apply(&frame));
        }
        (fold, deltas)
    }

    fn texts(deltas: &[Delta]) -> Vec<&str> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Text { text, .. }, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn basic_replays_end_to_end() {
        let (fold, deltas) = replay("basic.jsonl");
        assert_eq!(
            fold.session_id(),
            Some("01a0d344-59b2-7513-a297-f3015556bce1"),
            "the server-minted session id is stored, never assumed"
        );
        assert_eq!(texts(&deltas), ["READY"], "the turn renders its whole message once");
        for delta in &deltas {
            if let Delta::BlockAdded { block: Block::Text { streaming, .. }, .. } = delta {
                assert!(!streaming, "completed-lane blocks are whole, never streaming");
            }
        }
        let started = deltas.iter().filter(|d| matches!(d, Delta::TurnStarted { .. })).count();
        assert_eq!(started, 2, "one user turn plus one assistant turn");
        let finished: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 1, "the completed turn finishes once");
        let meta = finished[0];
        assert_eq!(meta.tokens_in, 14918, "per-turn input comes from last, not total");
        assert_eq!(meta.tokens_out, 5);
        assert_eq!(meta.reasoning_tokens, 0);
        assert_eq!(meta.cached_tokens, 10624);
        assert_eq!(meta.duration_ms, 6048);
    }

    /// THE AGREEMENT TEST. The normal-path fixtures carry both lanes
    /// (incremental deltas plus completed repeats); folding one with its
    /// `item/agentMessage/delta` lines stripped must yield deltas identical
    /// to folding it whole — the fallback lane changes nothing there. That is
    /// "agree modulo streaming granularity": the two lanes describe the same
    /// content, and the fold renders it once.
    ///
    /// `interrupt.jsonl` is deliberately NOT in this loop: its streamed text
    /// exists ONLY on the delta lane (no `item/completed` ever closes the
    /// message), so stripping the deltas provably changes its transcript.
    /// The next test pins that survival instead.
    #[test]
    fn delta_and_completed_lanes_agree() {
        for name in [
            "basic.jsonl",
            "approval.jsonl",
            "edit.jsonl",
            "read-search.jsonl",
            "thinking.jsonl",
            "todo.jsonl",
            "subagent.jsonl",
            "approval-default.jsonl",
            "error.jsonl",
            "image.jsonl",
            "resume.jsonl",
        ] {
            let lines = fixture_lines(name);
            let (_, whole) = replay(name);
            let stripped: Vec<String> =
                lines.iter().filter(|line| !is_agent_delta(line)).cloned().collect();
            assert!(
                stripped.len() < lines.len(),
                "{name} must actually contain delta lines"
            );
            let mut fold = CodexFold::new();
            let mut without_deltas = Vec::new();
            for line in &stripped {
                let (_, frame) = decode_envelope(line).expect("fixture decodes");
                without_deltas.extend(fold.apply(&frame));
            }
            assert_eq!(
                whole, without_deltas,
                "{name}: the delta lane must not change the transcript"
            );
        }
    }

    /// NO DOUBLE RENDER. The completed text appears exactly once although the
    /// delta lane describes the same content a second time.
    #[test]
    fn each_completed_message_renders_exactly_once() {
        let (_, deltas) = replay("basic.jsonl");
        let readies = texts(&deltas).iter().filter(|text| **text == "READY").count();
        assert_eq!(readies, 1, "READY renders exactly once");
    }

    /// THE INTERRUPTION TEST. `interrupt.jsonl` streams 374 delta chunks of
    /// real text and then ends at `turn/completed` with `status:"interrupted"`
    /// — no `item/completed` ever closes the assistant message. The turn must
    /// still render what it produced: without the fallback lane this replay
    /// folds to a lone user turn, not even a `TurnFinished`.
    #[test]
    fn interrupted_turn_renders_streamed_text() {
        let (_, deltas) = replay("interrupt.jsonl");
        let rendered: String = texts(&deltas).join("");
        assert!(
            rendered.contains("1 — Starting gently."),
            "the first streamed chunk survives: {rendered:.120}…"
        );
        assert!(
            rendered.contains("69 — One"),
            "the last streamed chunk survives: …{tail}",
            tail = rendered.chars().rev().take(120).collect::<String>()
        );
        let finished =
            deltas.iter().filter(|d| matches!(d, Delta::TurnFinished { .. })).count();
        assert_eq!(finished, 1, "the interrupted turn still finishes once");
        let started =
            deltas.iter().filter(|d| matches!(d, Delta::TurnStarted { .. })).count();
        assert_eq!(started, 2, "one user turn plus one assistant turn");
    }

    /// Stripping the delta lane from the interrupted turn provably loses its
    /// only content — the mirror of the agreement test above.
    #[test]
    fn interrupted_turn_without_deltas_renders_nothing() {
        let lines = fixture_lines("interrupt.jsonl");
        let stripped: Vec<String> =
            lines.iter().filter(|line| !is_agent_delta(line)).cloned().collect();
        assert!(stripped.len() < lines.len(), "the fixture must carry delta lines");
        let mut fold = CodexFold::new();
        let mut without_deltas = Vec::new();
        for line in &stripped {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            without_deltas.extend(fold.apply(&frame));
        }
        assert!(
            texts(&without_deltas).is_empty(),
            "no completed lane, no content — the delta lane was the only copy"
        );
    }

    #[test]
    fn per_turn_tokens_use_last_not_total() {
        // approval.jsonl updates usage twice for one turn. The first update
        // has total == last; the second splits: total 28645, last 14352. A
        // fold that summed `total` would bill 42938 for one turn.
        let lines = fixture_lines("approval.jsonl");
        let mut fold = CodexFold::new();
        let mut seen = Vec::new();
        for line in &lines {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            if let Frame::Notification(notification) = &frame {
                if notification.usage().is_some() {
                    seen.push((
                        notification.thread_id().unwrap_or_default().to_owned(),
                        notification.turn_id().unwrap_or_default().to_owned(),
                    ));
                }
            }
            fold.apply(&frame);
        }
        assert_eq!(seen.len(), 2, "two usage updates pin the split");
        let (thread_id, turn_id) = &seen[1];
        let last = fold.usage_for(thread_id, turn_id).expect("usage recorded");
        assert_eq!(last.total_tokens, 14352, "the per-turn figure is last");
        // And the raw notification proves total differs: the trap is real.
        let raw_total = lines
            .iter()
            .filter_map(|line| decode_envelope(line).ok())
            .filter_map(|(_, frame)| match frame {
                Frame::Notification(notification) => {
                    notification.usage().map(|usage| usage.total.total_tokens)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(raw_total, [14293, 28645], "cumulative totals, never summed");
        assert_ne!(raw_total[1], last.total_tokens, "total != last on the second update");
    }

    /// THE RESUME TEST. `resume.jsonl` records a thread started, one
    /// turn, a fresh child, `thread/resume` by id, and one more turn. The
    /// adapter folds history ONLY from the resume response's
    /// `thread.turns[]` and live frames ONLY from the pump after it — the
    /// pre-resume stream belonged to a dead child. Replay the file the
    /// same way: skip everything up to the resume answer, fold the
    /// history turns from it, then fold the later live frames. Each turn
    /// renders once, the new turn appends after history, the finish
    /// footers name the resumed model, and repeating the resume folds
    /// nothing twice.
    #[test]
    fn resume_fixture_folds_history_once_then_appends_the_new_turn() {
        use crate::child::{resume_ids, resume_turns};

        let lines = fixture_lines("resume.jsonl");
        let mut fold = CodexFold::new();
        let mut deltas = Vec::new();
        let mut resumed = false;
        let mut history_turns = 0;
        for line in &lines {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            if !resumed {
                if let Frame::Response { id, result } = &frame {
                    if id.as_u64() == Some(102) {
                        let (thread_id, session_id, model) =
                            resume_ids(result).expect("resume answers thread ids");
                        assert_eq!(
                            thread_id, session_id,
                            "a fresh thread mints equal ids"
                        );
                        assert_eq!(model, "gpt-5.6-sol", "the resumed model is known");
                        fold.set_model(&model);
                        let turns = resume_turns(result);
                        history_turns = turns.len();
                        deltas.extend(fold.apply_resume_turns(&thread_id, &turns));
                        resumed = true;
                    }
                }
                continue;
            }
            deltas.extend(fold.apply(&frame));
        }
        assert!(resumed, "the fixture must carry the thread/resume answer");
        assert_eq!(history_turns, 1, "one history turn resumes");
        // History first: the R1 prompt, its answer, and its finish.
        let users: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnStarted { turn: Turn::User { text, .. } } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            users,
            ["Say R1 and nothing else", "Say R2 and nothing else"],
            "history prompt then the new prompt, each once: {users:?}"
        );
        assert_eq!(
            texts(&deltas),
            ["R1", "R2"],
            "history answer then the new answer, each once"
        );
        let finished: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2, "history turn and new turn both finish");
        for meta in &finished {
            assert_eq!(meta.model, "gpt-5.6-sol", "footers name the resumed model");
        }
        // Repeating the resume folds nothing twice.
        let mut replay = Vec::new();
        for line in &lines {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            if let Frame::Response { id, result } = &frame {
                if id.as_u64() == Some(102) {
                    let (thread_id, _, _) = resume_ids(result).expect("resume parses");
                    replay.extend(fold.apply_resume_turns(&thread_id, &resume_turns(result)));
                }
            }
        }
        assert!(replay.is_empty(), "a second resume replays nothing: {replay:?}");
    }

    #[test]
    fn rate_limit_pushes_feed_the_account_snapshot() {
        // Through the real fold: `CodexFold::apply_notification`'s
        // `RateLimitsUpdated` arm is what feeds the snapshot. Calling
        // `AccountSnapshot::observe` directly would pass even with that arm
        // deleted.
        assert!(
            CodexFold::new().account().label().is_none(),
            "no reading seen, no login claimed"
        );
        let (fold, _) = replay("basic.jsonl");
        let label = fold.account().label().expect("a push was seen");
        assert!(label.contains("19%"), "live meter label: {label}");
    }

    #[test]
    fn pushes_and_read_answers_feed_one_structured_report() {
        // The card's shape, not a label: window lengths label the
        // windows, percents scale to fractions, resets ride along — from
        // a push and from a bare `account/rateLimits/read` answer alike.
        let mut snapshot = AccountSnapshot::default();
        assert!(snapshot.usage_report().is_none(), "no reading, no report");
        snapshot.observe(&serde_json::json!({
            "rateLimits": {
                "planType": "prolite",
                "primary": {"usedPercent": 19, "windowDurationMins": 10080, "resetsAt": 1790588038},
                "secondary": {"usedPercent": 50, "windowDurationMins": 300, "resetsAt": 1790187000}
            }
        }));
        let report = snapshot.usage_report().expect("a push was seen");
        assert_eq!(report.plan.as_deref(), Some("prolite"));
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].label, "Weekly");
        assert!((report.windows[0].used_fraction - 0.19).abs() < 1e-9);
        assert_eq!(report.windows[1].label, "Session · 5h");
        assert!((report.windows[1].used_fraction - 0.50).abs() < 1e-9);
        // The read answer carries the same object bare, without the
        // push's `rateLimits` wrapping.
        let mut bare = AccountSnapshot::default();
        bare.observe(&serde_json::json!({
            "planType": "prolite",
            "primary": {"usedPercent": 85, "windowDurationMins": 10080, "resetsAt": 1790588038},
            "secondary": null
        }));
        let report = bare.usage_report().expect("a read was seen");
        assert_eq!(report.windows.len(), 1);
        assert_eq!(report.windows[0].label, "Weekly");
    }

    /// Every `CommandExecutionStatus` in the schema maps off the spinner:
    /// `failed` and `declined` must never render as still-executing.
    #[test]
    fn failed_and_declined_commands_do_not_spin() {
        fn fold_status(status: &str) -> ToolStatus {
            let line = format!(
                r#"{{"method":"item/completed","params":{{"threadId":"t","turnId":"u","item":{{"type":"commandExecution","id":"exec-1","command":"ls","status":"{status}","exitCode":1}}}}}}"#
            );
            let frame = crate::frame::decode_line(&line).expect("synthetic line decodes");
            let mut fold = CodexFold::new();
            let deltas = fold.apply(&frame);
            deltas
                .iter()
                .find_map(|delta| match delta {
                    Delta::BlockAdded { block: Block::ToolCall { status, .. }, .. } => {
                        Some(*status)
                    }
                    _ => None,
                })
                .expect("a command execution folds to a shell card")
        }
        assert_eq!(fold_status("failed"), ToolStatus::Error);
        assert_eq!(fold_status("declined"), ToolStatus::Cancelled);
        assert_eq!(fold_status("inProgress"), ToolStatus::Running);
        assert_eq!(fold_status("completed"), ToolStatus::Error, "exit 1 is not success");
    }

    /// Reasoning items in the schema's object shape fold to thinking blocks:
    /// with the extractor fixed, the reasoning arm is reachable again.
    #[test]
    fn reasoning_items_fold_to_thinking_blocks() {
        let line = r#"{"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"planned"}],"content":[{"type":"reasoning_text","text":"trace"}]}}}"#;
        let frame = crate::frame::decode_line(line).expect("synthetic line decodes");
        let mut fold = CodexFold::new();
        let deltas = fold.apply(&frame);
        let thinking: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Thinking { text, .. }, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(thinking, ["plannedtrace"], "summary plus content, joined");
    }

    fn shell_cards(deltas: &[Delta]) -> Vec<(ToolStatus, String, Vec<String>, Option<i32>)> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::Shell,
                            status,
                            target,
                            body: ToolBody::Shell { output_lines, exit_code, .. },
                            ..
                        },
                    ..
                } => Some((
                    *status,
                    target.clone(),
                    output_lines.clone(),
                    *exit_code,
                )),
                _ => None,
            })
            .collect()
    }

    fn assistant_texts(deltas: &[Delta]) -> Vec<String> {
        texts(deltas).iter().map(|text| (*text).to_owned()).collect()
    }

    /// Defects 2 and 3, pinned on `read-search.jsonl`: the card titles
    /// the model's inner command (no `/bin/zsh -lc` wrapper) and shows
    /// the run's captured output. Removing either fold arm fails this:
    /// the wrapper strip or the `aggregatedOutput` join.
    #[test]
    fn shell_card_shows_inner_command_with_captured_output() {
        let (_, deltas) = replay("read-search.jsonl");
        let shells = shell_cards(&deltas);
        assert_eq!(shells.len(), 1, "one executed command, one card");
        let (status, target, output, exit_code) = &shells[0];
        assert_eq!(*status, ToolStatus::Success);
        assert!(
            !target.contains("/bin/zsh"),
            "the wrapper stays out of the title: {target}"
        );
        assert!(target.contains("rg"), "the model's command survives: {target}");
        assert_eq!(output, &["hello", "world", "./data.txt:3:gamma"]);
        assert_eq!(*exit_code, Some(0));
    }

    /// The `-c` wrapper unwraps one layer with POSIX quote rules, across
    /// the shells the server uses — and nothing else. Removing the parser
    /// (or the shell list) fails this: the naive first/last-byte strip
    /// mangles the `'\''` idiom and `\"` escapes.
    #[test]
    fn display_command_unwraps_one_wrapper_layer_with_posix_quotes() {
        // Single-quoted inner, all known shells and both flags.
        for command in [
            "/bin/zsh -lc 'rg --files -g foo'",
            "/bin/bash -lc 'rg --files -g foo'",
            "bash -lc 'rg --files -g foo'",
            "sh -c 'rg --files -g foo'",
            "zsh -lc 'rg --files -g foo'",
            "/bin/zsh -c 'rg --files -g foo'",
        ] {
            assert_eq!(display_command(command), "rg --files -g foo", "{command}");
        }
        // The standard '\'' idiom: quote, end, escaped quote, quote.
        assert_eq!(
            display_command("/bin/zsh -lc 'echo it'\\''s fine'"),
            "echo it's fine"
        );
        // Double-quote escapes unescape; other backslashes survive.
        assert_eq!(display_command("/bin/zsh -lc \"rg \\\"quoted\\\"\""), "rg \"quoted\"");
        assert_eq!(
            display_command("/bin/zsh -lc \"rg --files -g '*'\""),
            "rg --files -g '*'"
        );
        // Anything else renders verbatim: a command that merely starts
        // with a shell path but is not a `-c` wrapper, an unknown shell,
        // a bare command, extra words after the single argument, and an
        // unterminated quote (not a word, never guessed).
        for verbatim in [
            "/bin/zsh script.sh",
            "/bin/zsh -l",
            "dash -c 'ls'",
            "ls /nonexistent-dir-xyz-123",
            "/bin/zsh -lc 'a' 'b'",
            "/bin/zsh -lc 'unterminated",
        ] {
            assert_eq!(display_command(verbatim), verbatim, "{verbatim}");
        }
    }

    /// The `error.jsonl` turn: a failed run folds to an `Error` card
    /// carrying the daemon's own stderr, and the agent's reaction still
    /// renders.
    #[test]
    fn failed_command_folds_to_error_card_with_output() {
        let (_, deltas) = replay("error.jsonl");
        let shells = shell_cards(&deltas);
        assert_eq!(shells.len(), 1);
        let (status, target, output, _) = &shells[0];
        assert_eq!(*status, ToolStatus::Error);
        assert_eq!(target, "ls /nonexistent-dir-xyz-123");
        assert!(output.iter().any(|line| line.contains("No such file")),
            "stderr survives: {output:?}");
        assert!(
            assistant_texts(&deltas).iter().any(|text| text == "DONE"),
            "the turn still answers"
        );
    }

    /// `edit.jsonl`: the `fileChange` item folds to a Wrote card with a
    /// `+2/−0` chip, and the wire shows the approval that preceded it —
    /// a `fileChange/requestApproval` the client accepted, then the
    /// completed change. The fold cards the change; the request itself is
    /// the pump's routing business (answered, never rendered).
    #[test]
    fn file_change_folds_to_write_card_with_diff_stat() {
        let (_, deltas) = replay("edit.jsonl");
        let writes: Vec<_> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::Write,
                            target,
                            status,
                            body: ToolBody::Edit { diff },
                            diff_stat,
                            ..
                        },
                    ..
                } => Some((target.clone(), *status, diff.clone(), *diff_stat)),
                _ => None,
            })
            .collect();
        assert_eq!(writes.len(), 1, "one file change, one card");
        let (target, status, diff, stat) = &writes[0];
        assert!(target.ends_with("greet-codex.txt"), "target: {target}");
        assert_eq!(*status, ToolStatus::Success);
        let stat = stat.expect("the chip counts ride the card");
        assert_eq!((stat.added, stat.removed, stat.files), (2, 0, 1));
        assert_eq!((diff.added, diff.removed), (2, 0));
        // The approval round trip is on the wire beside it.
        let lines = fixture_lines("edit.jsonl");
        let asked = lines.iter().filter(|line| {
            line.contains("item/fileChange/requestApproval")
                && line.contains("server->client")
        }).count();
        let accepted = lines.iter().filter(|line| {
            line.contains("client->server")
                && line.contains("\"decision\"")
                && line.contains("\"accept\"")
        }).count();
        assert_eq!((asked, accepted), (1, 1), "asked once, accepted once");
    }

    /// `thinking.jsonl`: the client turn carries the `effort: "max"`
    /// override, and the per-turn footer counts the reasoning the wire
    /// reports. The reasoning items themselves arrive empty — no thinking
    /// text exists on this wire even at max effort — so no `Thinking`
    /// block is forged from nothing.
    #[test]
    fn thinking_effort_recorded_and_reasoning_counted() {
        let lines = fixture_lines("thinking.jsonl");
        let effort = lines
            .iter()
            .filter_map(|line| decode_envelope(line).ok())
            .filter_map(|(_, frame)| match frame {
                Frame::Request { method, params, .. } => {
                    (method == "turn/start").then(|| {
                        params.get("effort").and_then(Value::as_str).map(str::to_owned)
                    }).flatten()
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(effort, ["max"], "the override rides turn/start");
        let (_, deltas) = replay("thinking.jsonl");
        let thinking =
            deltas.iter().filter(|d| matches!(d, Delta::BlockAdded { block: Block::Thinking { .. }, .. })).count();
        assert_eq!(thinking, 0, "empty reasoning items render nothing");
        let finished: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 1);
        assert!(
            finished[0].reasoning_tokens > 0,
            "the wire's reasoning count reaches the footer"
        );
    }

    /// `todo.jsonl`: four `turn/plan/updated` frames fold to exactly one
    /// `Todo` card — added once, rewritten wholesale on each later update
    /// — ending with the final statuses. Removing the plan arm fails this:
    /// the updates would render as prose alone with no structured block.
    #[test]
    fn todo_plan_updates_replace_one_structured_card() {
        let (_, deltas) = replay("todo.jsonl");
        let added: Vec<&Vec<TodoItem>> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Todo { items }, .. } => Some(items),
                _ => None,
            })
            .collect();
        assert_eq!(added.len(), 1, "four updates, one card: {deltas:?}");
        assert_eq!(
            added[0].iter().map(|item| item.label.as_str()).collect::<Vec<_>>(),
            ["List files", "Read notes.txt", "Summarize contents"]
        );
        // The added card is the FIRST update's snapshot: "List files" is
        // already running, the rest still pending.
        assert_eq!(
            added[0].iter().map(|item| item.state).collect::<Vec<_>>(),
            [TodoState::Running, TodoState::Pending, TodoState::Pending]
        );
        let updated: Vec<(&str, usize, &Vec<TodoItem>)> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated {
                    turn_id,
                    block_index,
                    block: Block::Todo { items },
                } => Some((turn_id.as_str(), *block_index, items)),
                _ => None,
            })
            .collect();
        assert_eq!(updated.len(), 3, "updates two through four rewrite: {updated:?}");
        // The LAST rewrite carries the latest statuses: every step done.
        // Replace, never append — the first update's `inProgress` is gone,
        // not stacked under a second card.
        assert!(
            updated[2].2.iter().all(|item| item.state == TodoState::Done),
            "the final update completes every step: {:?}",
            updated[2].2
        );
        // Every rewrite addresses the added card's own index on the same
        // turn: a rewrite anywhere else would fork the transcript.
        let added_at = deltas
            .iter()
            .position(|delta| {
                matches!(delta, Delta::BlockAdded { block: Block::Todo { .. }, .. })
            })
            .expect("the card is added");
        let (added_turn, added_index) = match &deltas[added_at] {
            Delta::BlockAdded { turn_id, .. } => (turn_id.clone(), {
                deltas[..added_at]
                    .iter()
                    .filter(|delta| {
                        matches!(delta, Delta::BlockAdded { turn_id: id, .. } if id == turn_id)
                    })
                    .count()
            }),
            _ => unreachable!(),
        };
        for (turn_id, block_index, _) in &updated {
            assert_eq!(*turn_id, added_turn, "same turn");
            assert_eq!(*block_index, added_index, "same card");
        }
        // The model's own progress prose still renders beside the card —
        // the card structures the plan, it does not silence the commentary.
        let rendered = assistant_texts(&deltas).join("\n");
        assert!(rendered.contains("DONE"), "the turn still answers");
    }

    /// `subagent.jsonl`: both delegation markers card as `SubAgent`
    /// (paths on the card), and the `wait` plumbing rides a Generic card
    /// — carried, never dropped.
    #[test]
    fn subagent_delegations_card_with_paths() {
        let (_, deltas) = replay("subagent.jsonl");
        let agents: Vec<_> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::SubAgent, target, status, ..
                        },
                    ..
                } => Some((target.clone(), *status)),
                _ => None,
            })
            .collect();
        assert_eq!(agents.len(), 2, "one card per marker: {agents:?}");
        assert!(agents.iter().any(|(target, status)| target == "/root/read_notes"
            && *status == ToolStatus::Running));
        assert!(agents.iter().any(|(target, status)| target == "/root"
            && *status == ToolStatus::Success));
        let generics = deltas
            .iter()
            .filter(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Generic { kind, .. }, .. }
                if kind == "collabAgentToolCall"
            ))
            .count();
        assert_eq!(generics, 1, "the wait call is carried generically");
    }

    /// `approval-default.jsonl`: the default profile asks before writing
    /// outside the workspace — the request carries the model's own
    /// reason — and after the accept the write completes and the turn
    /// answers.
    #[test]
    fn approval_default_flow_asks_then_runs() {
        let lines = fixture_lines("approval-default.jsonl");
        let reasons: Vec<String> = lines
            .iter()
            .filter_map(|line| decode_envelope(line).ok())
            .filter_map(|(_, frame)| match frame {
                Frame::Request { method, params, .. } => {
                    (method == "item/commandExecution/requestApproval").then(|| {
                        params.get("reason").and_then(Value::as_str).map(str::to_owned)
                    }).flatten()
                }
                _ => None,
            })
            .collect();
        assert_eq!(reasons.len(), 1);
        assert!(
            reasons[0].contains("outside-codex.txt"),
            "the model's own justification: {}",
            reasons[0]
        );
        let (_, deltas) = replay("approval-default.jsonl");
        let shells = shell_cards(&deltas);
        assert!(
            shells.iter().any(|(status, target, _, _)| {
                *status == ToolStatus::Success && target.contains("outside-codex.txt")
            }),
            "the approved write ran: {shells:?}"
        );
        assert!(
            assistant_texts(&deltas).iter().any(|text| text == "DONE"),
            "the turn still answers"
        );
    }

    /// `image.jsonl`: the echoed `localImage` reference folds into the
    /// user turn as an image attachment, and the turn answers.
    #[test]
    fn image_part_accepted_as_user_attachment() {
        let (_, deltas) = replay("image.jsonl");
        let users: Vec<_> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnStarted { turn: Turn::User { text, attachments, .. } } => {
                    Some((text.clone(), attachments.clone()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(users.len(), 1);
        assert!(users[0].0.contains("three words"));
        assert_eq!(users[0].1.len(), 1, "the attached PNG rides the turn");
        assert_eq!(users[0].1[0].name, "tiny.png");
        assert!(matches!(users[0].1[0].kind, AttachmentKind::Image));
        assert!(
            assistant_texts(&deltas).iter().any(|text| text.contains("DONE")),
            "the turn saw the image and answered"
        );
    }

    /// A submit carrying `display_text` bubbles the short text, not the
    /// whole echoed input; without one the echo renders as before.
    #[test]
    fn submit_display_text_replaces_the_echoed_bubble() {
        fn user_completed(turn_id: &str, item_id: &str, text: &str) -> String {
            serde_json::json!({
                "_dir": "server->client",
                "frame": {
                    "method": "item/completed",
                    "params": {
                        "item": {
                            "type": "userMessage",
                            "id": item_id,
                            "clientId": null,
                            "content": [{"type": "text", "text": text, "text_elements": []}],
                        },
                        "threadId": "thread-1",
                        "turnId": turn_id,
                        "completedAtMs": 1790250868,
                    },
                },
            })
            .to_string()
        }
        fn user_texts(deltas: &[Delta]) -> Vec<String> {
            deltas
                .iter()
                .filter_map(|delta| match delta {
                    Delta::TurnStarted { turn: Turn::User { text, .. } } => Some(text.clone()),
                    _ => None,
                })
                .collect()
        }
        let full = "Continuing a session handed off from Claude Code. Context follows.\n## Original goal\nRename Ledger to Journal";
        let short = "Handed off from Claude Code: Rename Ledger to Journal (1 recent turns, 0 open todos, 0 files touched)";
        // Keyed by the turn the submit creates: the ack's turn id wins.
        let mut fold = CodexFold::new();
        fold.record_display_text(Some("turn-1"), full, short);
        let (_, frame) =
            decode_envelope(&user_completed("turn-1", "item-1", full)).expect("envelope decodes");
        assert_eq!(user_texts(&fold.apply(&frame)), [short]);
        // Without a display text the echoed input renders whole, as before.
        let mut fold = CodexFold::new();
        let (_, frame) =
            decode_envelope(&user_completed("turn-9", "item-9", full)).expect("envelope decodes");
        assert_eq!(user_texts(&fold.apply(&frame)), [full]);
        // The text key covers an echo naming an unrecorded turn.
        let mut fold = CodexFold::new();
        fold.record_display_text(None, full, short);
        let (_, frame) =
            decode_envelope(&user_completed("turn-other", "item-2", full)).expect("envelope decodes");
        assert_eq!(user_texts(&fold.apply(&frame)), [short]);
        // An identical display records nothing: the typed text stands.
        let mut fold = CodexFold::new();
        fold.record_display_text(Some("turn-3"), full, full);
        let (_, frame) =
            decode_envelope(&user_completed("turn-3", "item-3", full)).expect("envelope decodes");
        assert_eq!(user_texts(&fold.apply(&frame)), [full]);
    }

    /// X1: `turn/started` alone opens the assistant turn, so the view
    /// goes running the moment Codex starts — not at its first completed
    /// item. Repeating the start opens nothing twice.
    #[test]
    fn turn_started_alone_opens_the_assistant_turn() {
        fn started(turn_id: &str) -> String {
            serde_json::json!({
                "_dir": "server->client",
                "frame": {
                    "method": "turn/started",
                    "params": {
                        "threadId": "thread-1",
                        "turn": {"id": turn_id, "status": "inProgress"},
                    },
                },
            })
            .to_string()
        }
        let mut fold = CodexFold::new();
        let (_, frame) = decode_envelope(&started("turn-1")).expect("envelope decodes");
        let deltas = fold.apply(&frame);
        assert_eq!(deltas.len(), 1, "the start opens one turn, drew {deltas:?}");
        match &deltas[0] {
            Delta::TurnStarted { turn: Turn::Assistant { id, .. } } => {
                assert_eq!(id, "turn-1", "the open turn owns the started turn id");
            }
            other => panic!("a start must open the assistant turn, opened {other:?}"),
        }
        assert_eq!(fold.current_turn(), Some("turn-1"), "the start names the current turn");
        let (_, frame) = decode_envelope(&started("turn-1")).expect("envelope decodes");
        assert!(fold.apply(&frame).is_empty(), "a re-delivered start opens nothing twice");
    }

    /// `turn/diff/updated` is carried, never rendered: folding those
    /// lines alone yields no deltas.
    #[test]
    fn turn_diff_moves_no_transcript() {
        let lines = fixture_lines("edit.jsonl");
        let mut fold = CodexFold::new();
        let mut deltas = Vec::new();
        let mut count = 0;
        for line in &lines {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            if matches!(
                frame,
                Frame::Notification(Notification::TurnDiff { .. })
            ) {
                count += 1;
                deltas.extend(fold.apply(&frame));
            }
        }
        assert!(count > 0, "the fixture must carry turn/diff/updated");
        assert!(deltas.is_empty(), "carried, never rendered");
    }

    /// Fold fixture lines up to (and including) the n-th server approval
    /// request: the point where the ask waits and nothing later has
    /// answered or run yet. Returns the fold, its deltas so far, and the
    /// index the rest of the replay continues from.
    fn fold_until_approval(lines: &[String], n: usize) -> (CodexFold, Vec<Delta>, usize) {
        let mut fold = CodexFold::new();
        let mut deltas = Vec::new();
        let mut seen = 0;
        for (index, line) in lines.iter().enumerate() {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            let is_ask = matches!(&frame, Frame::Request { method, .. } if method.contains("requestApproval"));
            deltas.extend(fold.apply(&frame));
            if is_ask {
                seen += 1;
                if seen == n {
                    return (fold, deltas, index + 1);
                }
            }
        }
        panic!("fewer than {n} approval requests");
    }

    /// One pending approval card from `deltas`, when exactly one was added.
    fn added_approval(deltas: &[Delta]) -> Block {
        let mut cards: Vec<Block> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: card @ Block::Approval { .. }, .. } => {
                    Some(card.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(cards.len(), 1, "one asked approval, one card: {deltas:?}");
        cards.pop().expect("counted")
    }

    /// `approval-default.jsonl`: the command approval cards as pending with
    /// the four decision tokens — command + cwd + the model's reason, never
    /// raw JSON, never an item id — and the accept settles it to allowed as
    /// the write runs. Deciding settles nothing: only the completing item
    /// moves the card.
    #[test]
    fn command_approval_cards_pending_face_then_settles_on_completion() {
        let lines = fixture_lines("approval-default.jsonl");
        let (mut fold, ask, rest_at) = fold_until_approval(&lines, 1);
        match added_approval(&ask) {
            Block::Approval { tool, command, reason, cwd, state, choices, body_kind, .. } => {
                assert_eq!(tool, "Bash");
                assert_eq!(body_kind, ApprovalBodyKind::Command, "a shell ask reads as a command");
                assert_eq!(state, ApprovalState::Pending);
                assert!(command.contains("outside-codex.txt"), "the inner command: {command}");
                assert!(!command.contains("/bin/zsh"), "the wrapper stays out: {command}");
                assert_eq!(cwd, "/tmp/w4b/work");
                assert!(reason.contains("outside-codex.txt"), "the model's reason: {reason}");
                assert!(!reason.contains("exec-"), "no item ids: {reason}");
                assert!(!reason.contains('{'), "no raw JSON: {reason}");
                let ids: Vec<&str> = choices.iter().map(|choice| choice.id.as_str()).collect();
                assert_eq!(
                    ids,
                    ["accept", "accept-for-session", "decline", "cancel"],
                    "the full choice set the lane answers"
                );
                let labels: Vec<&str> =
                    choices.iter().map(|choice| choice.label.as_str()).collect();
                assert_eq!(
                    labels,
                    ["Allow once", "Allow for this session", "Deny", "Deny and stop"]
                );
            }
            other => panic!("an approval card, got {other:?}"),
        }
        assert!(
            !ask.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { .. }, .. }
            )),
            "the ask moves no card"
        );
        let mut rest = Vec::new();
        for line in &lines[rest_at..] {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            rest.extend(fold.apply(&frame));
        }
        let settled: Vec<&Delta> = rest
            .iter()
            .filter(|delta| {
                matches!(
                    delta,
                    Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
                )
            })
            .collect();
        assert_eq!(settled.len(), 1, "the completion settles the card, once");
        let cards: Vec<&Delta> = ask
            .iter()
            .chain(rest.iter())
            .filter(|delta| matches!(delta, Delta::BlockAdded { block: Block::Approval { .. }, .. }))
            .collect();
        assert_eq!(cards.len(), 1, "settling updates the card, never cards twice");
        assert!(
            rest.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::ToolCall { kind: ToolKind::Shell, status: ToolStatus::Success, .. },
                    ..
                }
            )),
            "the approved write ran"
        );
    }

    /// `edit.jsonl`: the `fileChange` request names no path and no diff,
    /// so the pending card says so honestly — and the completing change
    /// settles it to allowed with the path and the diff on the card below.
    #[test]
    fn file_change_approval_cards_honestly_then_settles_with_diff() {
        let lines = fixture_lines("edit.jsonl");
        let (mut fold, ask, rest_at) = fold_until_approval(&lines, 1);
        match added_approval(&ask) {
            Block::Approval { tool, command, reason, state, choices, body_kind, .. } => {
                assert_eq!(tool, "Edit");
                assert_eq!(body_kind, ApprovalBodyKind::FileWrite, "a file-change ask reads as a file write");
                assert_eq!(state, ApprovalState::Pending);
                assert_eq!(command, "File change");
                assert!(
                    reason.contains("did not say why"),
                    "no path is forged from a reasonless ask: {reason}"
                );
                assert_eq!(choices.len(), 4, "the full choice set the lane answers");
            }
            other => panic!("an approval card, got {other:?}"),
        }
        let mut rest = Vec::new();
        for line in &lines[rest_at..] {
            let (_, frame) = decode_envelope(line).expect("fixture decodes");
            rest.extend(fold.apply(&frame));
        }
        assert!(
            rest.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
            )),
            "the completing change settles the card to allowed"
        );
        assert!(
            rest.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::ToolCall { kind: ToolKind::Write, status: ToolStatus::Success, .. },
                    ..
                }
            )),
            "the path and the diff land on the Wrote card below"
        );
    }

    /// The fold contributes exactly one approval surface: `step_line` over
    /// a `requestApproval` emits the card delta and never the tap — the
    /// tap is the pump's routing business, and emitting both would double
    /// every ask.
    #[test]
    fn folding_an_approval_request_emits_the_card_never_the_tap() {
        let envelope = fixture_lines("approval-default.jsonl")
            .into_iter()
            .find(|line| line.contains("item/commandExecution/requestApproval"))
            .expect("the ask on the wire");
        // The fixture records `{"_dir", "frame"}` envelopes; the live
        // `step_line` folds bare JSON-RPC frames, so the test hands it
        // the frame the envelope carries — feeding the envelope itself
        // decodes to nothing and cards nothing.
        let frame: Value = serde_json::from_str(&envelope).expect("fixture is JSON");
        let line = frame.get("frame").expect("envelope carries a frame").to_string();
        let mut fold = CodexFold::new();
        let mut events = Vec::new();
        step_line(&mut fold, &line, &mut |event| events.push(event));
        let cards = events
            .iter()
            .filter_map(|event| match event {
                ProviderEvent::Deltas { deltas, .. } => Some(deltas),
                _ => None,
            })
            .flatten()
            .filter(|delta| matches!(delta, Delta::BlockAdded { block: Block::Approval { .. }, .. }))
            .count();
        assert_eq!(cards, 1, "one ask, one card");
        assert!(
            !events.iter().any(|event| matches!(
                event,
                ProviderEvent::ApprovalRequested { .. }
            )),
            "the fold never taps: {events:?}"
        );
    }

    /// A gated execution reads pending ("Run") while open, done ("Ran")
    /// once finished, and denied ("Denied") when the approval refused it —
    /// and each terminal state settles the card the ask opened.
    #[test]
    fn gated_executions_read_pending_until_finished_or_denied() {
        fn request(item: &str) -> String {
            serde_json::json!({
                "id": 9,
                "method": "item/commandExecution/requestApproval",
                "params": {
                    "threadId": "th", "turnId": "t-1", "itemId": item,
                    "reason": "Run it?", "command": "make check", "cwd": "/tmp/work",
                },
            })
            .to_string()
        }
        fn completed(item: &str, status: &str) -> Frame {
            // The wire always carries `exitCode` on a finished execution
            // (see `approval-default.jsonl`); a completion without one is
            // not a clean finish, and the fold fails it closed to `Error`.
            let line = serde_json::json!({
                "method": "item/completed",
                "params": {
                    "threadId": "th", "turnId": "t-1",
                    "item": {"type": "commandExecution", "id": item, "command": "make check", "status": status, "exitCode": 0},
                },
            })
            .to_string();
            crate::frame::decode_line(&line).expect("synthetic completion decodes")
        }
        // Still open: the card reads pending and the approval waits.
        let mut fold = CodexFold::new();
        let frame = crate::frame::decode_line(&request("exec-1")).expect("ask decodes");
        let mut deltas = fold.apply(&frame);
        deltas.extend(fold.apply(&completed("exec-1", "inProgress")));
        let verbs: Vec<(&str, ToolStatus)> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block: Block::ToolCall { kind: ToolKind::Shell, verb, status, .. },
                    ..
                } => Some((verb.as_str(), *status)),
                _ => None,
            })
            .collect();
        assert_eq!(verbs, [("Run", ToolStatus::Running)], "pending while open: {verbs:?}");
        assert!(
            !deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { .. }, .. }
            )),
            "an open execution settles nothing"
        );
        // Finished: done verb, settled card.
        deltas.extend(fold.apply(&completed("exec-1", "completed")));
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::ToolCall { kind: ToolKind::Shell, verb, status: ToolStatus::Success, .. },
                    ..
                } if verb == "Ran"
            )),
            "done reads done"
        );
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
            )),
            "the finish settles the card to allowed"
        );
        // Declined: denied verb and card, no run.
        let mut fold = CodexFold::new();
        let frame = crate::frame::decode_line(&request("exec-2")).expect("ask decodes");
        let mut deltas = fold.apply(&frame);
        deltas.extend(fold.apply(&completed("exec-2", "declined")));
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::ToolCall { kind: ToolKind::Shell, verb, status: ToolStatus::Cancelled, .. },
                    ..
                } if verb == "Denied"
            )),
            "a refused execution reads denied"
        );
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::Denied, .. }, .. }
            )),
            "the decline settles the card to denied"
        );
    }

    /// T3b, `mcp-terminal.jsonl`: the live MCP tool-call gate. The
    /// `mcpServer/elicitation/request` cards the same inline approval
    /// every other gate gets — the gated command, the terminal tool
    /// face, exactly the three answers the wire carries — and the
    /// refused `mcpToolCall` settles it denied while the call itself
    /// reads denied under its own command. Remove the elicitation arm
    /// and no card lands; restore the forged-success fallback and the
    /// refused call reads success under `terminal`.
    #[test]
    fn mcp_tool_call_gate_cards_answerable_and_refusal_reads_denied() {
        let (_, deltas) = replay("mcp-terminal.jsonl");
        let cards: Vec<_> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: card @ Block::Approval { .. }, .. } => Some(card),
                _ => None,
            })
            .collect();
        assert_eq!(cards.len(), 1, "one gate, one card: {deltas:?}");
        match cards[0] {
            Block::Approval { id, tool, command, choices, state, .. } => {
                assert_eq!(
                    id, "mcp-elicitation-0",
                    "the pump's prefixed elicitation id, so the press routes back"
                );
                assert_eq!(tool, "terminal_run", "the terminal face, for the title verb");
                assert_eq!(
                    command, "echo hi-from-codex",
                    "the gated command, from the call's arguments"
                );
                assert_eq!(*state, ApprovalState::Pending);
                let ids: Vec<&str> =
                    choices.iter().map(|choice| choice.id.as_str()).collect();
                assert_eq!(
                    ids,
                    ["accept", "decline", "cancel"],
                    "exactly what the wire carries — no session scope"
                );
            }
            other => panic!("an approval card, got {other:?}"),
        }
        // The refused call: denied under its own command, never success
        // under `terminal`.
        let denied: Vec<_> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::Shell,
                            verb,
                            target,
                            status,
                            body,
                            ..
                        },
                    ..
                } => Some((verb.clone(), target.clone(), *status, body.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(denied.len(), 1, "one MCP call, one card");
        let (verb, target, status, body) = &denied[0];
        assert_eq!(verb, "Denied");
        assert_eq!(*status, ToolStatus::Cancelled);
        assert_eq!(target, "echo hi-from-codex");
        match body {
            ToolBody::Shell { output_lines, .. } => assert!(
                output_lines.iter().any(|line| line.contains("rejected")),
                "the refusal reads on the card: {output_lines:?}"
            ),
            body => panic!("a shell body, not {body:?}"),
        }
        // And the gate settles denied with it.
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated {
                    block: Block::Approval { id, state: ApprovalState::Denied, .. },
                    ..
                } if id == "mcp-elicitation-0"
            )),
            "the refusal settles the gate to denied"
        );
    }

    /// One synthetic `mcpToolCall` per outcome, through the real fold: a
    /// clean terminal run reads success with its output and exit code, a
    /// nonzero exit fails closed, a still-open call reads running, and a
    /// non-terminal call is carried generic with its wire status — never
    /// raw JSON, never a forged success.
    #[test]
    fn mcp_tool_call_outcomes_fold_honestly() {
        fn completed(
            status: &str,
            arguments: serde_json::Value,
            error: Option<&str>,
            result: Option<serde_json::Value>,
        ) -> Frame {
            let line = serde_json::json!({
                "method": "item/completed",
                "params": {
                    "threadId": "t",
                    "turnId": "u",
                    "item": {
                        "type": "mcpToolCall",
                        "id": "exec-1",
                        "server": "baaz",
                        "tool": "terminal_run",
                        "status": status,
                        "arguments": arguments,
                        "error": error.map(|message| serde_json::json!({"message": message})).unwrap_or(serde_json::Value::Null),
                        "result": result.unwrap_or(serde_json::Value::Null),
                        "durationMs": 7,
                    },
                },
            })
            .to_string();
            crate::frame::decode_line(&line).expect("synthetic line decodes")
        }
        fn terminal_result(output: &str, exit_code: i32) -> serde_json::Value {
            serde_json::json!({
                "content": [{
                    "type": "text",
                    "text": serde_json::json!({
                        "tab": "t1", "block": "t1:0", "status": "exited",
                        "exit_code": exit_code, "duration_ms": 42, "output": output,
                    })
                    .to_string(),
                }],
            })
        }
        fn shell_of(frame: &Frame) -> (String, String, ToolStatus, ToolBody) {
            let mut fold = CodexFold::new();
            let deltas = fold.apply(frame);
            deltas
                .iter()
                .find_map(|delta| match delta {
                    Delta::BlockAdded {
                        block:
                            Block::ToolCall { verb, target, status, body, .. },
                        ..
                    } => Some((verb.clone(), target.clone(), *status, body.clone())),
                    _ => None,
                })
                .expect("an mcpToolCall folds to a shell card")
        }
        let args = serde_json::json!({"command": "echo hi", "wait": "exit"});
        // Clean run: success, command plus tab, output and exit code.
        let (verb, target, status, body) =
            shell_of(&completed("completed", args.clone(), None, Some(terminal_result("hi\n", 0))));
        assert_eq!(verb, TERMINAL_RAN_VERB);
        assert_eq!(target, "echo hi · t1");
        assert_eq!(status, ToolStatus::Success);
        match body {
            ToolBody::Shell { output_lines, exit_code, live } => {
                assert_eq!(output_lines, ["hi"]);
                assert_eq!(exit_code, Some(0));
                assert!(!live);
            }
            body => panic!("a shell body, not {body:?}"),
        }
        // Nonzero exit: the same card, failed closed.
        let (_, _, status, _) =
            shell_of(&completed("completed", args.clone(), None, Some(terminal_result("", 3))));
        assert_eq!(status, ToolStatus::Error, "exit 3 is not success");
        // Still open: running, never done.
        let (verb, _, status, _) = shell_of(&completed("inProgress", args.clone(), None, None));
        assert_eq!(verb, TERMINAL_RUNNING_VERB);
        assert_eq!(status, ToolStatus::Running);
        // Failed without a refusal message: error, and the error is the body.
        let (verb, target, status, body) =
            shell_of(&completed("failed", args.clone(), Some("the bridge exploded"), None));
        assert_eq!(verb, TERMINAL_RAN_VERB);
        assert_eq!(target, "echo hi");
        assert_eq!(status, ToolStatus::Error);
        match body {
            ToolBody::Shell { output_lines, exit_code, .. } => {
                assert!(output_lines.iter().any(|line| line.contains("exploded")));
                assert_eq!(exit_code, None, "no exit code was reported");
            }
            body => panic!("a shell body, not {body:?}"),
        }
        // A call naming no command names its tool, never a guess.
        let (verb, target, status, _) = shell_of(&completed(
            "completed",
            serde_json::json!({"tab": "auto"}),
            None,
            None,
        ));
        assert_eq!((verb.as_str(), target.as_str(), status), ("Ran in terminal", "terminal_run", ToolStatus::Success));
    }

    /// A non-baaz `mcpToolCall` is carried, never forged: its wire status
    /// survives and its message rides the card — no raw result JSON, and
    /// a failure never reads success.
    #[test]
    fn foreign_mcp_tool_calls_are_carried_never_forged() {
        let line = serde_json::json!({
            "method": "item/completed",
            "params": {
                "threadId": "t",
                "turnId": "u",
                "item": {
                    "type": "mcpToolCall",
                    "id": "exec-9",
                    "server": "other",
                    "tool": "thing",
                    "status": "failed",
                    "arguments": {"x": 1},
                    "error": {"message": "boom"},
                    "result": null,
                    "durationMs": null,
                },
            },
        })
        .to_string();
        let frame = crate::frame::decode_line(&line).expect("synthetic line decodes");
        let mut fold = CodexFold::new();
        let deltas = fold.apply(&frame);
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::Generic { kind, status, text },
                    ..
                } if kind == "mcpToolCall" && status == "failed" && text == "boom"
            )),
            "carried with its status and message: {deltas:?}"
        );
        assert!(
            !deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded {
                    block: Block::ToolCall { status: ToolStatus::Success, .. },
                    ..
                }
            )),
            "no forged success"
        );
    }

    /// The elicitation gate parses Codex's own message strictly: the
    /// live shape names `(server, tool)`, anything else names nothing —
    /// and a server that disagrees with the request never mislabels.
    #[test]
    fn tool_gate_parses_strictly() {
        assert_eq!(
            parse_tool_gate("baaz", "Allow the baaz MCP server to run tool \"terminal_run\"?"),
            Some(("baaz".to_owned(), "terminal_run".to_owned()))
        );
        assert_eq!(parse_tool_gate("baaz", "Fill the form"), None);
        assert_eq!(
            parse_tool_gate("baaz", "Allow the other MCP server to run tool \"terminal_run\"?"),
            None,
            "a mismatched server never mislabels"
        );
        assert_eq!(
            parse_tool_gate("baaz", "Allow the baaz MCP server to run tool \"\"?"),
            None,
            "no empty tool"
        );
    }

    /// An elicitation that gates no tool call cards nothing: genuine
    /// input stays a question on the answerable surface, never a forged
    /// approval.
    #[test]
    fn non_tool_elicitations_card_nothing() {
        let params = serde_json::json!({
            "threadId": "t",
            "turnId": "u",
            "serverName": "s",
            "message": "Fill the form",
            "mode": "form",
            "requestedSchema": {},
        });
        let mut fold = CodexFold::new();
        assert!(
            fold.apply_request("mcpServer/elicitation/request", &params, &serde_json::json!(7)).is_empty()
        );
    }

    /// Approval fixture command executions fold to one shell card, completed
    /// with exit 0.
    #[test]
    fn command_executions_fold_to_shell_cards() {
        let (_, deltas) = replay("approval.jsonl");
        let shells: Vec<(&ToolStatus, _)> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded {
                    block:
                        Block::ToolCall {
                            kind: ToolKind::Shell,
                            status,
                            target,
                            body: ToolBody::Shell { exit_code, .. },
                            ..
                        },
                    ..
                } => Some((status, (target.clone(), *exit_code))),
                _ => None,
            })
            .collect();
        assert_eq!(shells.len(), 1, "one executed command, one card");
        assert_eq!(shells[0].0, &ToolStatus::Success);
        assert_eq!(shells[0].1.1, Some(0));
        assert!(shells[0].1.0.contains("baaz_probe_write.txt"), "target: {}", shells[0].1.0);
    }
}
