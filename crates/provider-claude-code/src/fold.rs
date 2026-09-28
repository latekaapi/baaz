//! Decoded frames into render-ready deltas: one lane, and the reason.
//!
//! RENDERING LANE: **`assistant` frames only.** `stream_event` frames are
//! decoded (so a shape change still parses) and then ignored for transcript
//! purposes; they emit no deltas.
//!
//! Why the `assistant` lane won:
//!
//! * `basic.jsonl` — a turn run *without* `--include-partial-messages` —
//!   contains zero `stream_event` frames. The deltas lane is absent from a
//!   turn the CLI demonstrably produces, so a deltas-only renderer prints
//!   nothing for it. The `assistant` lane is the only lane present in all
//!   five fixtures.
//! * `assistant` frames carry completed blocks (whole text, whole tool
//!   input), so the fold needs no cross-frame assembly state: no partial
//!   JSON stitching, no chunk bookkeeping, nothing to disagree about.
//! * The trap this task exists to avoid is folding both lanes and
//!   double-rendering every message. Ignoring one lane entirely — rather
//!   than "deduplicating" two lanes with a heuristic — leaves no seam where
//!   the two paths can disagree. The agreement test proves it: folding
//!   `partial.jsonl` with its `stream_event` lines stripped yields deltas
//!   identical to folding it whole.
//!
//! What the fold emits, per frame:
//!
//! * `assistant`: blocks join the turn's ONE assistant turn — opened on the
//!   turn's first message (turn id IS that message's id, so the live fold
//!   and the history replay agree on the key) and closed by the `result`
//!   frame — then one [`Delta::BlockAdded`] per content block: `text` →
//!   [`Block::Text`] (complete, `streaming: false`), `thinking` →
//!   [`Block::Thinking`], `tool_use` → [`Block::ToolCall`] (`Bash` is shell,
//!   `mcp__<server>__<tool>` is MCP, anything else is [`Block::Generic`]).
//!   A turn's several messages share the turn (W8: folding one per message
//!   rendered one meta row, one token count and one ledger row per message
//!   instead of per turn).
//! * `user` (tool results): one [`Delta::BlockUpdated`] per answered tool
//!   call, completing its card (`Success`/`Error` plus output).
//! * `user` (prompt echo): the person's bubble, then the previous turn's
//!   close when one is still open (stored history carries no `result`
//!   frame, so the echo is the only boundary between two replayed turns).
//! * `result`: one [`Delta::TurnFinished`] for the turn, with usage and
//!   cost in the footer meta, preceded by one generic card per
//!   `permission_denials` entry (useful for the transcript; never a
//!   substitute for answering the request).
//! * `control_request/can_use_tool`: the request is queued as pending (see
//!   [`ClaudeFold::pending_approvals`]) and rendered as a pending approval
//!   card; the tap on the shoulder rides
//!   [`ProviderEvent::ApprovalRequested`] (see [`step_line`]). Nothing here
//!   answers it — answering is an explicit `DecideApproval`, because the
//!   child waits silently (no timeout was observed) and an auto-answer
//!   would bypass the person.
//! * unknown control subtypes: queued as answerable pending requests and
//!   rendered as generic cards — surfaced, never dropped, never
//!   auto-answered, and never left hanging: `DecideApproval` answers one
//!   with an explicit `"deny"` refusal (or a deliberate `"allow"`).
//! * `control_response` (child→host): host-initiated, so no deltas.
//! * everything else: no deltas (`init` records identity; `rate_limit`
//!   updates the account snapshot).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;

use aui_protocol::{
    ApprovalBadges, ApprovalBodyKind, ApprovalChoice, ApprovalDecision, ApprovalScope,
    ApprovalState, Attachment, AttachmentKind, Block, Delta, Diff, DiffKind, DiffLine, Hunk,
    ThinkingState, TodoItem, TodoState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta,
    UploadState,
};
use provider::{ProviderError, ProviderEvent};

use crate::account::AccountSnapshot;
use crate::frame::{approval_headline, ApprovalRequest, ContentBlock, Frame, ToolResult};

/// Where a pending approval card lives, so its tool's result or the turn's
/// denials can settle it to its decided state.
#[derive(Clone, Debug)]
struct ApprovalSite {
    /// The turn hosting the card.
    turn_id: String,
    /// The card's block index in that turn, for wholesale updates.
    block_index: usize,
    /// The wire tool-use id the request gates (empty when the request
    /// names none — the card still settles, through denials alone).
    tool_use_id: String,
}

/// Where a tool call lives, and what it was, so its result can complete it.
#[derive(Clone, Debug)]
struct ToolSite {
    turn_id: String,
    block_index: usize,
    kind: ToolKind,
    verb: String,
    target: String,
    name: String,
    params: Vec<(String, String)>,
}

/// Where a nested (sub-agent) tool call lives inside its `Agent` card's
/// buffer, and what it was, so the nested result can complete it there.
#[derive(Clone, Debug)]
struct NestedSite {
    /// Index in the agent card's buffered blocks.
    index: usize,
    kind: ToolKind,
    verb: String,
    target: String,
    name: String,
    params: Vec<(String, String)>,
}

/// The tool names that manage the agent's task list rather than doing
/// work: TaskCreate/TaskUpdate (live on this wire) and the legacy
/// TodoWrite (same list shape, older spelling).
fn is_todo_tool(name: &str) -> bool {
    matches!(name, "TaskCreate" | "TaskUpdate" | "TodoWrite")
}

/// Whether `tool` is one of the baaz terminal tools (`docs/14-terminal.md`
/// §4), served on this lane as `mcp__baaz__terminal_*`. A terminal call
/// folds to a shell card with terminal verbs (D51) — the existing
/// tool-card rendering carries the command, the exit pill, the duration
/// and the output — never the raw request/response JSON.
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

/// The opening verb every terminal card folds with; the result maps it to
/// past tense, the same present-until-ran rule shell cards follow.
pub const TERMINAL_RUNNING_VERB: &str = "Running in terminal";

/// The settled verb of a terminal card.
pub const TERMINAL_RAN_VERB: &str = "Ran in terminal";

/// Whether `name` is a baaz terminal tool call on this lane, and its tool
/// short of the `mcp__baaz__` prefix when it is.
fn terminal_tool_name(name: &str) -> Option<String> {
    let (server, tool) = mcp_split(name)?;
    (server == "baaz" && is_terminal_tool(&tool)).then_some(tool)
}

/// The provider's task status vocabulary onto the transcript's.
fn map_todo_state(status: &str) -> TodoState {
    match status {
        "in_progress" => TodoState::Running,
        "completed" => TodoState::Done,
        _ => TodoState::Pending,
    }
}

/// The task number out of a creation/update confirmation
/// (`Task #1 created successfully: …`, `Updated task #1 status`): what
/// the result names when its detail is absent.
fn task_number_from(text: &str) -> Option<&str> {
    let hash = text.find('#')?;
    let rest = &text[hash + 1..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 { None } else { Some(&rest[..end]) }
}

/// Fold one todo-call's input into the card's list: TaskCreate appends a
/// pending row, TaskUpdate moves a row's state, legacy TodoWrite replaces
/// the whole list. Unknown shapes leave the list alone — a call the fold
/// does not understand must not rewrite the transcript.
fn apply_todo_input(
    items: &mut Vec<TodoEntry>,
    tool_id: &str,
    name: &str,
    input: &serde_json::Value,
) {
    let str_field = |key: &str| input.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
    match name {
        "TaskCreate" => {
            let label = str_field("subject");
            let label = if label.is_empty() { str_field("description") } else { label };
            if !items.iter().any(|entry| entry.key == tool_id) {
                items.push(TodoEntry {
                    key: tool_id.to_owned(),
                    label: label.to_owned(),
                    state: TodoState::Pending,
                });
            }
        }
        "TaskUpdate" => {
            let id = str_field("taskId");
            if let Some(entry) = items.iter_mut().find(|entry| entry.key == id) {
                entry.state = map_todo_state(str_field("status"));
            }
        }
        _ => {
            // Legacy TodoWrite: the whole list, in order.
            if let Some(todos) = input.get("todos").and_then(serde_json::Value::as_array) {
                items.clear();
                for (index, todo) in todos.iter().enumerate() {
                    let label = todo
                        .get("content")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");
                    let state = todo
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .map(map_todo_state)
                        .unwrap_or(TodoState::Pending);
                    items.push(TodoEntry {
                        key: format!("legacy-{index}"),
                        label: label.to_owned(),
                        state,
                    });
                }
            }
        }
    }
}

/// The completed `Agent` card: the delegation header plus one nested
/// assistant turn carrying every block the sub-agent produced.
fn agent_block(id: &str, site: &ToolSite, status: ToolStatus, blocks: &[Block]) -> Block {
    // A still-running delegation keeps its pending verb; only a finished
    // one reads done — the same pending-until-ran rule as every tool card.
    let verb = match status {
        ToolStatus::Pending | ToolStatus::Running => site.verb.clone(),
        _ => past_verb(&site.kind, &site.verb),
    };
    Block::ToolCall {
        id: id.to_owned(),
        kind: ToolKind::SubAgent,
        verb,
        target: site.target.clone(),
        status,
        duration_ms: None,
        body: ToolBody::SubAgent {
            turns: vec![Turn::Assistant {
                id: format!("{id}/subagent"),
                blocks: blocks.to_vec(),
                meta: TurnMeta::default(),
                timestamp: None,
            }],
        },
        diff_stat: None,
    }
}

/// The CLI's `Exit code N` result prefix onto the card's exit code. The
/// success path carries no prefix (its detail is bare streams), so
/// `None` there means "ran, code unreported" — never 0 by assumption.
fn parse_exit_code(text: &str) -> Option<i32> {
    let first = text.lines().next()?;
    let rest = first.strip_prefix("Exit code ")?;
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// The opening present-tense verb onto its completed past tense: a card
/// opens as "Write" (it has not run) and only reads "Wrote" once the
/// result lands. `Read` reads the same either way; anything unrecognised
/// passes through rather than inventing a tense.
fn past_verb(kind: &ToolKind, open: &str) -> String {
    match (kind, open) {
        (ToolKind::Shell, "Run") => "Ran",
        (ToolKind::Write, "Write") => "Wrote",
        (ToolKind::Edit, "Edit") => "Edited",
        (ToolKind::Mcp { .. }, "Run") => "Ran",
        (ToolKind::SubAgent, "Delegate") => "Delegated",
        _ => open,
    }
    .to_owned()
}

/// Complete any tool card from its result: the one place shell output,
/// MCP payloads, file diffs, read counts and the generic fallback are
/// decided, shared by the main transcript and nested sub-agent buffers
/// so the two cannot disagree.
fn finish_tool_block(
    kind: &ToolKind,
    verb: &str,
    target: &str,
    name: &str,
    params: &[(String, String)],
    tool_use_id: &str,
    result: &ToolResult,
) -> Block {
    // A terminal call completes from its result JSON, never as raw text:
    // the tab joins the command in the header, the run's output is the
    // body, and the exit code is the status (D51).
    if verb == TERMINAL_RUNNING_VERB {
        return finish_terminal_block(target, params, tool_use_id, result);
    }
    let status = if result.is_error { ToolStatus::Error } else { ToolStatus::Success };
    let done = past_verb(kind, verb);
    match kind {
        ToolKind::Shell => Block::ToolCall {
            id: tool_use_id.to_owned(),
            kind: kind.clone(),
            verb: done,
            target: target.to_owned(),
            status,
            duration_ms: None,
            body: ToolBody::Shell {
                output_lines: result.text.lines().map(str::to_owned).collect(),
                exit_code: parse_exit_code(&result.text),
                live: false,
            },
            diff_stat: None,
        },
        ToolKind::Mcp { .. } => Block::ToolCall {
            id: tool_use_id.to_owned(),
            kind: kind.clone(),
            verb: done,
            target: target.to_owned(),
            status,
            duration_ms: None,
            body: ToolBody::Mcp { params: params.to_vec(), result_json: result.text.clone() },
            diff_stat: None,
        },
        ToolKind::Edit | ToolKind::Write => {
            let (body, diff_stat) = edit_body(target, result);
            Block::ToolCall {
                id: tool_use_id.to_owned(),
                kind: kind.clone(),
                verb: done,
                target: target.to_owned(),
                status,
                duration_ms: None,
                body,
                diff_stat,
            }
        }
        ToolKind::Read => Block::ToolCall {
            id: tool_use_id.to_owned(),
            kind: kind.clone(),
            verb: done,
            target: target.to_owned(),
            status,
            duration_ms: None,
            body: ToolBody::Read { lines: read_line_count(result) },
            diff_stat: None,
        },
        _ => Block::Generic {
            kind: name.to_owned(),
            status: if result.is_error { "error".into() } else { "completed".into() },
            text: result.text.clone(),
        },
    }
}

/// Complete a terminal tool card from its result JSON (`docs/14-terminal.md`
/// §4): `{tab, block, status, exit_code?, duration_ms, output, …}`. The
/// header names the command plus the tab, the body is the run's output —
/// a result without an `output` field (list, open, send, close) bodies
/// the result text itself — and the exit code is the status and the pill.
fn finish_terminal_block(
    target: &str,
    params: &[(String, String)],
    tool_use_id: &str,
    result: &ToolResult,
) -> Block {
    let object: Option<serde_json::Map<String, serde_json::Value>> =
        serde_json::from_str(result.text.trim())
            .ok()
            .filter(|value: &serde_json::Value| value.is_object())
            .map(|value: serde_json::Value| value.as_object().cloned().unwrap_or_default());
    let param = |key: &str| params.iter().find(|(name, _)| name == key).map(|(_, value)| value.clone());
    let command =
        param("command").filter(|command| !command.trim().is_empty()).unwrap_or_else(|| target.to_owned());
    let tab = object
        .as_ref()
        .and_then(|object| object.get("tab"))
        .and_then(serde_json::Value::as_str)
        .filter(|tab| !tab.trim().is_empty());
    let target = match tab {
        Some(tab) if command.trim() != tab => format!("{command} · {tab}"),
        _ => command,
    };
    let output_lines = match object
        .as_ref()
        .and_then(|object| object.get("output"))
        .and_then(serde_json::Value::as_str)
    {
        Some(output) => output.lines().map(str::to_owned).collect(),
        None => result.text.lines().map(str::to_owned).collect(),
    };
    let exit_code = object
        .as_ref()
        .and_then(|object| object.get("exit_code"))
        .and_then(serde_json::Value::as_i64)
        .and_then(|code| i32::try_from(code).ok());
    let duration_ms = object
        .as_ref()
        .and_then(|object| object.get("duration_ms"))
        .and_then(serde_json::Value::as_u64);
    let status = if result.is_error {
        ToolStatus::Error
    } else {
        match exit_code {
            Some(0) | None => ToolStatus::Success,
            Some(_) => ToolStatus::Error,
        }
    };
    Block::ToolCall {
        id: tool_use_id.to_owned(),
        kind: ToolKind::Shell,
        verb: TERMINAL_RAN_VERB.into(),
        target,
        status,
        duration_ms,
        body: ToolBody::Shell { output_lines, exit_code, live: false },
        diff_stat: None,
    }
}

/// The Read card's line count: the wire's own `numLines` when the detail
/// carries it, else the echoed text's lines. Never 0 for a non-empty
/// read — but 0 when there is genuinely nothing to count.
fn read_line_count(result: &ToolResult) -> usize {
    result
        .detail
        .as_ref()
        .and_then(|detail| detail.get("file"))
        .and_then(|file| file.get("numLines"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|lines| usize::try_from(lines).ok())
        .unwrap_or_else(|| result.text.lines().count())
}

/// The Edit/Write card body from the result detail: the wire's own
/// `structuredPatch` hunks for an edit, the created content as an
/// all-addition diff for a write. `None` detail (or an unrecognised one)
/// leaves the card header-only rather than forging a diff.
fn edit_body(target: &str, result: &ToolResult) -> (ToolBody, Option<aui_protocol::DiffStat>) {
    let detail = match result.detail.as_ref() {
        Some(detail) => detail,
        None => return (ToolBody::None, None),
    };
    let path = detail
        .get("filePath")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(target)
        .to_owned();
    if let Some(patch) = detail.get("structuredPatch").and_then(serde_json::Value::as_array) {
        if !patch.is_empty() {
            let (diff, stat) = diff_from_structured_patch(&path, patch);
            return (ToolBody::Edit { diff }, Some(stat));
        }
    }
    if let Some(content) = detail.get("content").and_then(serde_json::Value::as_str) {
        let lines: Vec<&str> = content.lines().collect();
        let added = lines.len();
        let diff = Diff {
            path,
            hunks: vec![Hunk {
                header: format!("@@ -0,0 +1,{added} @@"),
                lines: lines
                    .iter()
                    .enumerate()
                    .map(|(index, line)| DiffLine {
                        kind: DiffKind::Add,
                        old_no: None,
                        new_no: Some(index as u32 + 1),
                        text: (*line).to_owned(),
                    })
                    .collect(),
            }],
            added: added as u32,
            removed: 0,
        };
        let stat =
            aui_protocol::DiffStat { added: added as u64, removed: 0, files: 1 };
        return (ToolBody::Edit { diff }, Some(stat));
    }
    (ToolBody::None, None)
}

/// The wire's `structuredPatch` entries onto one [`Diff`] plus its chip
/// counts: `+`/`-` lines counted, `\ No newline` markers and `+++`/`---`
/// headers skipped, anything unprefixed read as context.
fn diff_from_structured_patch(
    path: &str,
    patch: &[serde_json::Value],
) -> (Diff, aui_protocol::DiffStat) {
    let mut hunks = Vec::new();
    let mut added: u64 = 0;
    let mut removed: u64 = 0;
    for entry in patch {
        let old_start = entry.get("oldStart").and_then(serde_json::Value::as_u64).unwrap_or(1);
        let old_lines = entry.get("oldLines").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let new_start = entry.get("newStart").and_then(serde_json::Value::as_u64).unwrap_or(1);
        let new_lines = entry.get("newLines").and_then(serde_json::Value::as_u64).unwrap_or(0);
        let mut old_no = old_start as u32;
        let mut new_no = new_start as u32;
        let mut lines = Vec::new();
        for line in entry
            .get("lines")
            .and_then(serde_json::Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            let text = line.as_str().unwrap_or("");
            if text.starts_with('\\') {
                continue;
            }
            if let Some(stripped) = text.strip_prefix('+') {
                if stripped.starts_with('+') {
                    continue;
                }
                added += 1;
                lines.push(DiffLine { kind: DiffKind::Add, old_no: None, new_no: Some(new_no), text: stripped.to_owned() });
                new_no += 1;
            } else if let Some(stripped) = text.strip_prefix('-') {
                if stripped.starts_with('-') {
                    continue;
                }
                removed += 1;
                lines.push(DiffLine { kind: DiffKind::Del, old_no: Some(old_no), new_no: None, text: stripped.to_owned() });
                old_no += 1;
            } else {
                let text = text.strip_prefix(' ').unwrap_or(text);
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
        hunks.push(Hunk {
            header: format!("@@ -{old_start},{old_lines} +{new_start},{new_lines} @@"),
            lines,
        });
    }
    let files = u64::from(!patch.is_empty());
    let diff = Diff {
        path: path.to_owned(),
        hunks,
        added: added as u32,
        removed: removed as u32,
    };
    (diff, aui_protocol::DiffStat { added, removed, files })
}

/// A `control_request` whose subtype this adapter does not know, held for
/// an explicit human decision exactly like a `can_use_tool` request: a
/// received request must always be answerable, or the child hangs.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownControlRequest {
    /// The top-level `request_id`: what the `control_response` echoes.
    pub request_id: String,
    /// The unrecognised `request.subtype`.
    pub subtype: String,
    /// The raw `request` object, kept for inspection.
    pub raw: serde_json::Value,
}

/// One row of the per-turn todo card: a task the agent tracks, keyed by
/// the provider's task id (`Task #N`) once the creation result names it,
/// by the `tool_use` id before that.
#[derive(Clone, Debug)]
struct TodoEntry {
    /// The provider task id when known, else the creating `tool_use` id.
    key: String,
    /// The human task text (`subject`, or the legacy `content`).
    label: String,
    /// The latest status seen for this task.
    state: TodoState,
}

/// The per-turn todo card: one card per turn no matter how many
/// TaskCreate/TaskUpdate/TodoWrite calls refine it, so the transcript
/// shows a list, not a call log.
#[derive(Clone, Debug)]
struct TodoCard {
    /// The turn hosting the card.
    turn_id: String,
    /// The card's block index in that turn, for wholesale updates.
    block_index: usize,
    /// The list as last rendered.
    items: Vec<TodoEntry>,
}

/// One sub-agent invocation: the `Agent` card plus the blocks its own
/// messages (`parent_tool_use_id` set) accumulate, flushed into the card
/// when the agent's result lands — or at turn end, when the agent is still
/// running async and the turn finished first.
#[derive(Clone, Debug)]
struct AgentCard {
    /// The turn hosting the card.
    turn_id: String,
    /// The card's block index in that turn, for wholesale updates.
    block_index: usize,
    /// The card's current status (Running until the agent answers).
    status: ToolStatus,
    /// The nested blocks, in arrival order.
    blocks: Vec<Block>,
    /// Nested tool-use id → site in `blocks`, so nested results complete
    /// their own cards.
    nested_tools: HashMap<String, NestedSite>,
    /// Whether the card already carries the current buffer (set when the
    /// agent's own result lands; the turn-end flush covers the async rest).
    flushed: bool,
}

/// The fold: decoded frames in, [`Delta`]s out.
///
/// Stateful only where the wire is relational: one assistant turn per CLI
/// turn (many `assistant` messages share it; the `result` frame closes
/// it), tool-use id → card (results arrive on later `user` frames).
#[derive(Clone, Debug, Default)]
pub struct ClaudeFold {
    started: HashMap<String, ()>,
    open: Vec<String>,
    /// The turn this CLI turn's assistant messages share: `Some` from the
    /// first message's `TurnStarted` until the `result` frame (or the next
    /// prompt echo) finishes it. `None` between turns.
    assistant_turn: Option<String>,
    tools: HashMap<String, ToolSite>,
    /// Echo uuids already rendered as user turns, so a replayed fixture
    /// never mints the same bubble twice.
    user_echoes: HashSet<String>,
    /// Full submitted text → the bubble text (`SubmitInput.display_text`):
    /// the replayed echo carries the whole input, so the user turn shows
    /// the short text instead. The model still received the full text.
    display_by_text: HashMap<String, String>,
    /// Replayed user message uuid → the bubble text, learned when an echo
    /// first folds: what a stored history shows after a restart, when the
    /// submit's full text is gone but the uuid survives in the transcript.
    display_by_uuid: HashMap<String, String>,
    /// `tool_use` ids of todo-list calls (TaskCreate/TaskUpdate/TodoWrite):
    /// their results refine the turn's todo card, never a tool card.
    todo_tool_ids: HashSet<String>,
    /// Turn id → its todo card, while the turn is open. Ordered, so two
    /// concurrent todo cards resolve deterministically: a `HashMap` here
    /// made the fallback lookup (and the result fan-out below) pick
    /// whichever card hashed first.
    todos: BTreeMap<String, TodoCard>,
    /// `Agent` tool-use id → its nesting card, while the turn is open.
    /// Ordered, so the turn-end flush renders concurrent subagents in
    /// id order rather than hash order.
    agents: BTreeMap<String, AgentCard>,
    /// Blocks already emitted per turn, so a second `assistant` frame with
    /// the same message id continues the index sequence (and a later
    /// `BlockUpdated` for a tool result addresses the right card).
    emitted: HashMap<String, usize>,
    /// Footer facts per turn from `assistant` lines (`message.model`,
    /// `message.usage`): what closes a replayed turn, since the stored
    /// transcript carries no `result` frame. Every line carries them, so
    /// the last line of the turn wins; the `result` frame's usage stays
    /// authoritative on the live path and this is only ever a fallback.
    stored_meta: HashMap<String, TurnMeta>,
    /// The last stored line's model, in file order: the session's model
    /// when history exists, `None` on a stream-only fold.
    stored_model: Option<String>,
    model: Option<String>,
    session_id: Option<String>,
    account: AccountSnapshot,
    /// `can_use_tool` requests waiting on a human decision, oldest first. A
    /// request stays here until `DecideApproval` answers it: nothing in the
    /// adapter answers on its own, because an unanswered child waits
    /// silently and an auto-answered one would bypass the person.
    pending: Vec<ApprovalRequest>,
    /// Requests already answered through `DecideApproval`, by `request_id`.
    /// The card stays up until the settling frames arrive, and settling
    /// rebuilds the card from the request — so the request lives here
    /// between the press and the resolution. Cleared as each card settles.
    decided: HashMap<String, ApprovalRequest>,
    /// Where each carded approval lives (`request_id` → site), so the
    /// tool's own result (allowed, it ran) or the turn's `permission_denials`
    /// (denied, it never ran) can settle the card. Survives `take_approval`:
    /// the decision queues the answer line, the frames settle the card —
    /// deciding never moves it, only resolving does.
    approval_sites: HashMap<String, ApprovalSite>,
    /// `tool_use` id → `request_id`, the join a result needs to settle the
    /// approval whose tool just answered. Same lifetime as `approval_sites`.
    tool_approvals: HashMap<String, String>,
    /// The session cwd from the `init` frame: what a pending approval card
    /// names as where the tool would run. Empty until `init` arrives.
    session_cwd: String,
    /// Unknown `control_request` subtypes seen so far, as
    /// `(request_id, subtype)` in arrival order. Surfaced in the transcript,
    /// never dropped.
    unknown_control: Vec<(String, String)>,
    /// Unknown-subtype requests waiting on a human decision, oldest first.
    /// Answered through `DecideApproval` with `"deny"` (a refusal) or
    /// `"allow"`; never auto-answered, never unanswerable.
    pending_unknown: Vec<UnknownControlRequest>,
    /// The model catalog from the last `initialize` answer, in answer
    /// order: what `ListModels` serves. Replaced wholesale on every
    /// `initialize` answer — the child restates the whole list — and empty
    /// until the first one lands.
    catalog: Vec<crate::frame::CatalogModel>,
}

impl ClaudeFold {
    /// An empty fold.
    pub fn new() -> Self {
        Self::default()
    }

    /// The latest account reading (see [`crate::account`]).
    pub fn account(&self) -> &AccountSnapshot {
        &self.account
    }

    /// The session id from the last `init` frame, when seen.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// The session's effective model: the last `init` frame's, or the last
    /// `SelectModel` admission.
    pub fn model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// The model catalog from the last `initialize` answer, in answer
    /// order. Empty until the first answer lands — `ListModels` refuses
    /// honestly rather than serving Baaz's fallback as the child's own.
    pub fn catalog_models(&self) -> &[crate::frame::CatalogModel] {
        &self.catalog
    }

    /// Remember the effective model from a `SelectModel` admission: the next
    /// turn's footer reads it, the way `init`'s model lands in the first.
    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_owned());
    }

    /// Forget the recorded model: a `SelectModel` admission the child never
    /// took is un-recorded, so the chip stops claiming it.
    pub fn clear_model(&mut self) {
        self.model = None;
    }

    /// `can_use_tool` requests waiting on a human decision, oldest first.
    pub fn pending_approvals(&self) -> &[ApprovalRequest] {
        &self.pending
    }

    /// Unknown `control_request` subtypes seen so far, as
    /// `(request_id, subtype)` in arrival order.
    pub fn unknown_control(&self) -> &[(String, String)] {
        &self.unknown_control
    }

    /// Take a pending request for deciding. Moves it off the answerable
    /// queue (so a second decision cannot answer the same request twice)
    /// into the decided shelf, where the settling frames still find it:
    /// deciding queues the answer line, resolving settles the card, and
    /// the card rebuild needs the request either way. `None` means nobody
    /// asked for that id — an unknown approval decides nothing.
    pub fn take_approval(&mut self, request_id: &str) -> Option<ApprovalRequest> {
        let position = self.pending.iter().position(|queued| queued.request_id == request_id)?;
        let request = self.pending.remove(position);
        self.decided.insert(request_id.to_owned(), request.clone());
        Some(request)
    }

    /// Unknown-subtype control requests waiting on a human decision, oldest
    /// first.
    pub fn pending_unknown(&self) -> &[UnknownControlRequest] {
        &self.pending_unknown
    }

    /// Take an unknown-subtype request for deciding. Removes it, so a
    /// second decision cannot answer the same request twice.
    pub fn take_unknown(&mut self, request_id: &str) -> Option<UnknownControlRequest> {
        let position =
            self.pending_unknown.iter().position(|queued| queued.request_id == request_id)?;
        Some(self.pending_unknown.remove(position))
    }

    /// Put a claimed request back after an undeliverable answer, without
    /// duplicating ids. Leaves the decided shelf alone when the request
    /// never left it through this path (defensive; same id both places
    /// would answer twice).
    pub fn requeue_approval(&mut self, request: ApprovalRequest) {
        if !self.pending.iter().any(|queued| queued.request_id == request.request_id) {
            self.pending.push(request.clone());
        }
        self.decided.remove(&request.request_id);
    }

    /// Put a claimed unknown-subtype request back, without duplicating ids.
    pub fn requeue_unknown(&mut self, request: UnknownControlRequest) {
        if !self.pending_unknown.iter().any(|queued| queued.request_id == request.request_id) {
            self.pending_unknown.push(request);
        }
    }

    /// Fold one decoded frame into render-ready deltas.
    pub fn apply(&mut self, frame: &Frame) -> Vec<Delta> {
        match frame {
            Frame::Init(init) => {
                self.session_id = Some(init.session_id.clone());
                self.model = Some(init.model.clone());
                self.session_cwd = init.cwd.clone();
                Vec::new()
            }
            Frame::Assistant { message_id, blocks, parent_tool_use_id, model, usage, .. } => {
                // Nested (sub-agent) blocks buffer into their `Agent` card,
                // never the turn: they open nothing and footer nothing (as
                // before — their usage is the sub-agent's request, not the
                // turn's).
                if !parent_tool_use_id.is_empty() {
                    return self.apply_subagent_blocks(parent_tool_use_id, blocks);
                }
                // The turn this message belongs to: opened on the turn's
                // first message, shared by the rest.
                let mut deltas = Vec::new();
                let turn_id = self.assistant_turn_id(message_id, &mut deltas);
                // Each line carries footer facts on the message itself:
                // remembered per turn so the history path can close it
                // (the stored transcript has no `result` frame). The
                // per-message usage is one API request, not the turn, so
                // the last line wins and the `result` frame stays
                // authoritative wherever one arrives.
                if model.is_some() || usage.is_some() {
                    let usage = usage.clone().unwrap_or_default();
                    self.stored_meta.insert(turn_id.clone(), TurnMeta {
                        model: model.clone().unwrap_or_default(),
                        duration_ms: 0,
                        tokens_in: usage.input_tokens.saturating_add(
                            usage.cache_read_tokens.unwrap_or(0),
                        ).saturating_add(
                            usage.cache_write_tokens.unwrap_or(0),
                        ),
                        tokens_out: usage.output_tokens,
                        reasoning_tokens: usage.reasoning_tokens,
                        // The stored file carries no cost: 0.0 is the
                        // neutral unknown default, never a measurement.
                        cost_usd: 0.0,
                        cache_read_tokens: usage.cache_read_tokens,
                        cache_write_tokens: usage.cache_write_tokens,
                        cached_tokens: 0,
                    });
                    if let Some(model) = model {
                        if !model.is_empty() {
                            self.stored_model = Some(model.clone());
                        }
                    }
                }
                deltas.extend(self.apply_blocks(&turn_id, blocks));
                deltas
            }
            Frame::UserResult { results, parent_tool_use_id, .. } => {
                if parent_tool_use_id.is_empty() {
                    results.iter().flat_map(|result| self.apply_result(result)).collect()
                } else {
                    results.iter().for_each(|result| self.apply_nested_result(parent_tool_use_id, result));
                    Vec::new()
                }
            }
            Frame::UserText { uuid, text, images, .. } => self.apply_user_text(uuid, text, images),
            Frame::TurnResult {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
                reasoning_tokens,
                total_cost_usd,
                duration_ms,
                model,
                permission_denials,
                ..
            } => {
                // `tokens_in` is the whole billed prompt: the wire splits
                // it into bare input plus cache creation and cache read,
                // and the muse lane's `promptTokens` arrives whole, so a
                // bare `input_tokens` here would show 93 for a 30k prompt.
                // The cache fields stay informational (never add them on
                // top — that double-counts); the ledger reads `tokens_in`.
                let meta = TurnMeta {
                    model: model.clone().or_else(|| self.model.clone()).unwrap_or_default(),
                    duration_ms: *duration_ms,
                    tokens_in: input_tokens
                        .saturating_add(cache_read_tokens.unwrap_or(0))
                        .saturating_add(cache_write_tokens.unwrap_or(0)),
                    tokens_out: *output_tokens,
                    reasoning_tokens: *reasoning_tokens,
                    cost_usd: *total_cost_usd,
                    cache_read_tokens: *cache_read_tokens,
                    cache_write_tokens: *cache_write_tokens,
                    cached_tokens: 0,
                };
                let mut deltas = Vec::new();
                // Async sub-agents may still be streaming when the turn
                // ends: flush whatever they buffered into their cards
                // before the finish, or their work vanishes silently.
                self.flush_agents(&mut deltas);
                // After-the-fact refusals, on the transcript before the
                // finish: decoded, visible, and never a substitute for
                // answering the request itself. A denial that joins to a
                // carded approval settles that card (and its tool card) to
                // denied instead of minting a second card beside it; a
                // denial with no card — a bare result, no request seen —
                // still cards generically rather than vanishing.
                if !permission_denials.is_empty() {
                    // The card rides the running turn; with no turn open (a
                    // bare result carrying denials) a turn keyed by the
                    // first denial opens so the card is never dangling —
                    // the same mechanism control-channel cards use.
                    let turn_id = match self.open.last().cloned() {
                        Some(open) => open,
                        None => {
                            let key = permission_denials
                                .first()
                                .map(|denial| format!("denial-{}", denial.tool_use_id))
                                .unwrap_or_else(|| "denial".to_owned());
                            self.control_turn(&key, &mut deltas)
                        }
                    };
                    for denial in permission_denials {
                        let request_id =
                            self.tool_approvals.get(&denial.tool_use_id).cloned();
                        match request_id {
                            Some(request_id) => {
                                self.resolve_approval_card(
                                    &request_id,
                                    ApprovalState::Denied,
                                    &mut deltas,
                                );
                                if let Some(site) = self.tools.get(&denial.tool_use_id).cloned() {
                                    if let Some(block) = gated_tool_block(
                                        &denial.tool_use_id,
                                        &site,
                                        ToolStatus::Cancelled,
                                        "Denied",
                                    ) {
                                        deltas.push(Delta::BlockUpdated {
                                            turn_id: site.turn_id,
                                            block_index: site.block_index,
                                            block,
                                        });
                                    }
                                }
                            }
                            None => deltas.push(Delta::BlockAdded {
                                turn_id: turn_id.clone(),
                                block: Block::Generic {
                                    kind: "permission-denial".into(),
                                    status: "denied".into(),
                                    text: format!("{} refused", denial.tool_name),
                                },
                            }),
                        }
                    }
                }
                // One user turn, one finished turn: the assistant turn owns
                // the `result` frame's totals, so the footer, the token
                // chip and the ledger each count them once (W8: finishing
                // every open message turn copied the whole turn's totals
                // into each). A stale control-channel turn with no
                // assistant behind it still closes, with a default meta —
                // never the turn's totals twice. A bare denial turn opened
                // above owns the frame the same way.
                let finishing = std::mem::take(&mut self.open);
                let owner = self
                    .assistant_turn
                    .take()
                    .or_else(|| finishing.last().cloned());
                for turn_id in finishing {
                    self.stored_meta.remove(&turn_id);
                    let finished = if Some(&turn_id) == owner.as_ref() {
                        meta.clone()
                    } else {
                        TurnMeta::default()
                    };
                    deltas.push(Delta::TurnFinished { turn_id, meta: finished });
                }
                deltas
            }
            Frame::RateLimit(info) => {
                self.account.observe(info.clone());
                Vec::new()
            }
            // A permission decision the child is suspended on. Queued as
            // pending and carded — never folded away, or the turn hangs
            // forever with no timeout to save it.
            Frame::ControlRequest(request) => self.apply_approval(request),
            // An unrecognised control subtype: recorded and carded, never
            // dropped and never panicked on. The next subtype must be
            // visible when it arrives.
            Frame::ControlUnknown { request_id, subtype, raw } => {
                self.apply_unknown_control(request_id, subtype, raw)
            }
            // A child→host `control_response` (e.g. the answer to our
            // `initialize` handshake). Host-initiated, so nothing about it
            // needs answering — but an `initialize` answer restates the
            // model catalog, which `ListModels` serves from here.
            Frame::ControlResponse { models, .. } => {
                if !models.is_empty() {
                    self.catalog = models.clone();
                }
                Vec::new()
            }
            // The other lane, by choice (see module docs): parsed, ignored.
            Frame::Stream { .. } | Frame::Ignored { .. } => Vec::new(),
        }
    }

    /// The turn a main-thread `assistant` message belongs to: the open one
    /// when a turn is running, else a fresh turn keyed by this first
    /// message's id (announced in `deltas`). The id is content, not
    /// timing — the same first message opens the same turn on the live
    /// fold and on the history replay, so the ledger key agrees and a
    /// reopen adds no rows.
    ///
    /// A message seen before with no turn open is a repeat after its turn
    /// closed: it addresses its old turn without announcing a second one,
    /// exactly as the per-message fold did.
    fn assistant_turn_id(&mut self, message_id: &str, deltas: &mut Vec<Delta>) -> String {
        if let Some(open) = self.assistant_turn.clone() {
            return open;
        }
        if self.started.contains_key(message_id) {
            return message_id.to_owned();
        }
        self.started.insert(message_id.to_owned(), ());
        self.open.push(message_id.to_owned());
        self.assistant_turn = Some(message_id.to_owned());
        deltas.push(Delta::TurnStarted {
            turn: Turn::Assistant {
                id: message_id.to_owned(),
                blocks: Vec::new(),
                meta: TurnMeta::default(),
                timestamp: None,
            },
        });
        message_id.to_owned()
    }

    fn apply_blocks(&mut self, turn_id: &str, blocks: &[ContentBlock]) -> Vec<Delta> {
        let mut deltas = Vec::new();
        for block in blocks.iter() {
            let block_index = self.emitted.get(turn_id).copied().unwrap_or(0);
            let emitted = match block {
                // An empty thinking block is the redacted kind: the child
                // billed the thought (its signature) without uttering it.
                // Carding it renders an empty "Thought for 0.0 s" shell, so
                // it folds to nothing — the billed count still reaches the
                // footer through `reasoning_tokens`.
                ContentBlock::Thinking { text } if text.trim().is_empty() => None,
                ContentBlock::Thinking { text } => Some(Block::Thinking {
                    text: text.clone(),
                    elapsed_ms: 0,
                    summary: None,
                    state: ThinkingState::Done,
                }),
                ContentBlock::Text { text } => {
                    Some(Block::Text { text: text.clone(), streaming: false })
                }
                ContentBlock::ToolUse { .. } | ContentBlock::Other { .. } => None,
            };
            match block {
                ContentBlock::Thinking { .. } | ContentBlock::Text { .. } => {
                    // A redacted thinking block folds to no card at all:
                    // nothing is pushed and no index is consumed, so the
                    // cards around the hole keep their addresses.
                    if let Some(block) = emitted {
                        deltas.push(Delta::BlockAdded {
                            turn_id: turn_id.to_owned(),
                            block,
                        });
                        self.emitted.insert(turn_id.to_owned(), block_index + 1);
                    }
                }
                ContentBlock::ToolUse { id, name, input } => {
                    if is_todo_tool(name) {
                        // The agent's task list, not a tool card: one card
                        // per turn, refined by every call (see
                        // `apply_todo_call`). Never a `ToolSite`, so its
                        // result routes to the card, not to a card update.
                        self.todo_tool_ids.insert(id.clone());
                        self.apply_todo_call(turn_id, block_index, id, name, input, &mut deltas);
                    } else {
                        let site = tool_card(id, name, input);
                        if site.kind == ToolKind::SubAgent {
                            self.agents.insert(id.clone(), AgentCard {
                                turn_id: turn_id.to_owned(),
                                block_index,
                                status: ToolStatus::Running,
                                blocks: Vec::new(),
                                nested_tools: HashMap::new(),
                                flushed: false,
                            });
                        }
                        self.tools.insert(id.clone(), ToolSite {
                            turn_id: turn_id.to_owned(),
                            block_index,
                            kind: site.kind.clone(),
                            verb: site.verb.clone(),
                            target: site.target.clone(),
                            name: name.clone(),
                            params: site.params.clone(),
                        });
                        deltas.push(Delta::BlockAdded {
                            turn_id: turn_id.to_owned(),
                            block: site.block,
                        });
                        self.emitted.insert(turn_id.to_owned(), block_index + 1);
                    }
                }
                ContentBlock::Other { .. } => {}
            }
        }
        deltas
    }

    /// The session's model from stored history: the last stored
    /// `assistant` line's `message.model`, in file order. `None` when no
    /// stored line carried one — a stream-only fold, or a history of
    /// user turns alone.
    pub fn stored_model(&self) -> Option<&str> {
        self.stored_model.as_deref()
    }

    /// Close every still-open turn with its stored footer, oldest first.
    /// STORED-HISTORY ONLY: the adapter calls this once on the throwaway
    /// fold that replayed the stored file, never on the live fold — a
    /// live turn is still running, and finishing it here would settle the
    /// stop button mid-turn. A turn with no stored footer still closes
    /// (with a default meta) so a reopened view never shows a stuck
    /// "working" indicator for a turn that settled before the restart.
    pub fn finish_stored_turns(&mut self) -> Vec<Delta> {
        let mut deltas = Vec::new();
        self.flush_agents(&mut deltas);
        self.assistant_turn = None;
        let open = std::mem::take(&mut self.open);
        deltas.extend(open.into_iter().map(|turn_id| {
            let meta = self.stored_meta.remove(&turn_id).unwrap_or_default();
            Delta::TurnFinished { turn_id, meta }
        }));
        deltas
    }

    /// Remember the bubble text for a submit carrying `display_text`.
    /// An empty display, or one identical to the input, records nothing,
    /// so the typed text renders as before.
    pub fn record_display_text(&mut self, full_text: &str, display: &str) {
        if display.is_empty() || display == full_text {
            return;
        }
        if full_text.len() > 131_072 || self.display_by_text.len() >= 64 {
            return;
        }
        self.display_by_text.insert(full_text.to_owned(), display.to_owned());
    }

    /// The submit display map, for seeding a fresh fold that replays
    /// stored history in place of this one.
    pub fn display_overrides(&self) -> HashMap<String, String> {
        self.display_by_text.clone()
    }

    /// Seed the submit display map on a fresh fold before it replays
    /// stored history.
    pub fn set_display_overrides(&mut self, overrides: HashMap<String, String>) {
        for (full, display) in overrides {
            self.record_display_text(&full, &display);
        }
    }

    /// Pre-mark one echo uuid as already rendered, without rendering it:
    /// the resume path. History the adapter already showed carries the
    /// same uuids the resumed child's stream repeats (proven live by
    /// `resume-replay.jsonl`: every stored user entry shares its echo's
    /// uuid), so a resumed session seeds these before the stream runs and
    /// each user turn bubbles exactly once — history plus the new turn.
    /// An empty uuid marks nothing: it could never match an echo.
    pub fn mark_user_echo_seen(&mut self, uuid: &str) {
        if !uuid.is_empty() {
            self.user_echoes.insert(uuid.to_owned());
        }
    }

    /// The echoed prompt (`--replay-user-messages`) as the person's own
    /// turn: the bubble the pre-replay CLI never sent. Exactly once per
    /// echo uuid — a replayed fixture must not mint the bubble twice.
    fn apply_user_text(
        &mut self,
        uuid: &str,
        text: &str,
        images: &[crate::frame::UserImage],
    ) -> Vec<Delta> {
        if uuid.is_empty() || !self.user_echoes.insert(uuid.to_owned()) {
            return Vec::new();
        }
        // The echo carries the whole submitted input; a submit that
        // recorded `display_text` shows the short text instead. The uuid
        // join wins (it names the replayed message exactly); the full
        // text is the fallback, and its hit teaches the uuid map so a
        // later replay keyed by uuid alone still shows the short text.
        let bubble = self
            .display_by_uuid
            .get(uuid)
            .cloned()
            .or_else(|| self.display_by_text.get(text).cloned());
        if let Some(display) = &bubble {
            self.display_by_uuid.insert(uuid.to_owned(), display.clone());
        }
        let bubble = bubble.unwrap_or_else(|| text.to_owned());
        let mut deltas = Vec::new();
        // A fresh prompt ends the previous turn when one is still open.
        // Live that turn already closed on its `result` frame, so this is
        // a no-op; stored history carries no `result` frame, so the echo
        // is the only boundary between two replayed turns — without it a
        // reopen stacks the whole session into one turn (one footer, one
        // ledger row for many turns, keyed wrong). The stored footer
        // closes it, so a reopened view settles instead of hanging.
        if let Some(turn_id) = self.assistant_turn.take() {
            self.open.retain(|open| open != &turn_id);
            let meta = self.stored_meta.remove(&turn_id).unwrap_or_default();
            deltas.push(Delta::TurnFinished { turn_id, meta });
        }
        let attachments = images
            .iter()
            .enumerate()
            .map(|(index, image)| Attachment {
                name: format!("image-{}", index + 1),
                kind: AttachmentKind::Image,
                size_bytes: Some(image.data_len as u64),
                meta: Some(image.media_type.clone()),
                state: UploadState::Ready,
            })
            .collect();
        deltas.push(Delta::TurnStarted {
            turn: Turn::User {
                id: uuid.to_owned(),
                text: bubble,
                attachments,
                mentions: Vec::new(),
                timestamp: None,
            },
        });
        deltas
    }

    /// Complete one tool card from its result, settling the linked approval
    /// card alongside it when the result is a success: a tool that ran and
    /// answered was allowed, full stop. An error result completes only the
    /// tool card — allowed-but-failed and denied look identical here, and
    /// the turn's `permission_denials` tells them apart below.
    fn apply_result(&mut self, result: &crate::frame::ToolResult) -> Vec<Delta> {
        if self.todo_tool_ids.contains(&result.tool_use_id) {
            return self.apply_todo_result(result).into_iter().collect();
        }
        let Some(site) = self.tools.get(&result.tool_use_id).cloned() else { return Vec::new() };
        let status = if result.is_error { ToolStatus::Error } else { ToolStatus::Success };
        if site.kind == ToolKind::SubAgent {
            return self.apply_agent_result(&result.tool_use_id, &site, status, result).into_iter().collect();
        }
        let block = finish_tool_block(
            &site.kind,
            &site.verb,
            &site.target,
            &site.name,
            &site.params,
            &result.tool_use_id,
            result,
        );
        let mut deltas =
            vec![Delta::BlockUpdated { turn_id: site.turn_id, block_index: site.block_index, block }];
        if !result.is_error {
            if let Some(request_id) = self.tool_approvals.get(&result.tool_use_id).cloned() {
                let exit_code = match &site.kind {
                    ToolKind::Shell => parse_exit_code(&result.text).unwrap_or(0),
                    _ => 0,
                };
                self.resolve_approval_card(
                    &request_id,
                    ApprovalState::AllowedOnce { exit_code, duration_ms: 0 },
                    &mut deltas,
                );
            }
        }
        deltas
    }

    /// Refine the turn's todo card from one TaskCreate/TaskUpdate/TodoWrite
    /// call: the first call opens the card, every later call rewrites it
    /// wholesale. Only the opening call consumes a block index — updates
    /// address the recorded one.
    ///
    /// The card spans the CLI turn's many assistant messages: one card per
    /// turn, refined by every call (a `BlockUpdated` to an earlier turn is
    /// exactly how tool results already address older cards). A new CLI
    /// turn has no open turns left — its first call always opens a fresh
    /// card.
    fn apply_todo_call(
        &mut self,
        turn_id: &str,
        block_index: usize,
        tool_id: &str,
        name: &str,
        input: &serde_json::Value,
        deltas: &mut Vec<Delta>,
    ) {
        // The card to refine: this turn's own, else any card on a
        // still-open turn (a `BlockUpdated` to an earlier turn is exactly
        // how tool results already address older cards). `None` means no
        // card is open anywhere — this call opens one.
        let key = if self.todos.contains_key(turn_id) {
            Some(turn_id.to_owned())
        } else {
            self.todos
                .iter()
                .find(|(_, card)| self.open.iter().any(|turn| turn == &card.turn_id))
                .map(|(key, _)| key.clone())
        };
        let (key, is_new) = match key {
            Some(key) => (key, false),
            None => {
                self.todos.insert(turn_id.to_owned(), TodoCard {
                    turn_id: turn_id.to_owned(),
                    block_index,
                    items: Vec::new(),
                });
                (turn_id.to_owned(), true)
            }
        };
        if let Some(card) = self.todos.get_mut(&key) {
            apply_todo_input(&mut card.items, tool_id, name, input);
        }
        let card = self.todos.get(&key).expect("inserted above");
        let block = Block::Todo {
            items: card
                .items
                .iter()
                .map(|entry| TodoItem {
                    label: entry.label.clone(),
                    state: entry.state,
                    elapsed_ms: None,
                })
                .collect(),
        };
        if is_new {
            deltas.push(Delta::BlockAdded { turn_id: card.turn_id.clone(), block });
            self.emitted.insert(turn_id.to_owned(), block_index + 1);
        } else {
            deltas.push(Delta::BlockUpdated {
                turn_id: card.turn_id.clone(),
                block_index: card.block_index,
                block,
            });
        }
    }

    /// Refine the turn's todo card from one todo-call result: TaskCreate
    /// names the task's real id, TaskUpdate moves its state. A result for
    /// a call the fold never saw refines nothing.
    fn apply_todo_result(&mut self, result: &ToolResult) -> Option<Delta> {
        let detail = result.detail.as_ref();
        // The card this call belongs to: the one still holding its
        // provisional tool-use key, or the one already holding its task id.
        let task_id = detail
            .and_then(|detail| detail.get("task"))
            .and_then(|task| task.get("id"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .or_else(|| {
                detail
                    .and_then(|detail| detail.get("taskId"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| task_number_from(&result.text).map(|number| number.to_owned()));
        let mut found = None;
        for card in self.todos.values_mut() {
            if let Some(entry) =
                card.items.iter_mut().find(|entry| Some(entry.key.as_str()) == task_id.as_deref())
            {
                if entry.key == result.tool_use_id {
                    if let Some(id) = task_id.clone() {
                        entry.key = id;
                    }
                }
                if let Some(change) =
                    detail.and_then(|detail| detail.get("statusChange")).and_then(|change| {
                        change.get("to").and_then(serde_json::Value::as_str)
                    })
                {
                    entry.state = map_todo_state(change);
                }
                found = Some((card.turn_id.clone(), card.block_index));
                break;
            }
            // A creation result whose provisional key is still the
            // `tool_use` id: claim it for this task id.
            if let Some(entry) =
                card.items.iter_mut().find(|entry| entry.key == result.tool_use_id)
            {
                if let Some(id) = task_id.clone() {
                    entry.key = id;
                }
                found = Some((card.turn_id.clone(), card.block_index));
                break;
            }
        }
        let (turn_id, block_index) = found?;
        let card = self.todos.values().find(|card| card.turn_id == turn_id)?;
        Some(Delta::BlockUpdated {
            turn_id,
            block_index,
            block: Block::Todo {
                items: card
                    .items
                    .iter()
                    .map(|entry| TodoItem {
                        label: entry.label.clone(),
                        state: entry.state,
                        elapsed_ms: None,
                    })
                    .collect(),
            },
        })
    }

    /// Buffer one sub-agent message's blocks into its `Agent` card: no
    /// deltas yet — the card refreshes when the agent answers, or at turn
    /// end when the agent is still running async.
    fn apply_subagent_blocks(
        &mut self,
        agent_id: &str,
        blocks: &[ContentBlock],
    ) -> Vec<Delta> {
        if !self.agents.contains_key(agent_id) {
            // Frames out of order (or a fixture starting mid-agent): a
            // card shell so the blocks land somewhere owned.
            self.agents.insert(agent_id.to_owned(), AgentCard {
                turn_id: agent_id.to_owned(),
                block_index: 0,
                status: ToolStatus::Running,
                blocks: Vec::new(),
                nested_tools: HashMap::new(),
                flushed: false,
            });
        }
        if let Some(card) = self.agents.get_mut(agent_id) {
            for block in blocks {
                match block {
                    ContentBlock::Thinking { text } => {
                        // Redacted thinking stays out of the nested card
                        // too: an empty trace with a spinner is noise
                        // wherever it renders.
                        if !text.trim().is_empty() {
                            card.blocks.push(Block::Thinking {
                                text: text.clone(),
                                elapsed_ms: 0,
                                summary: None,
                                state: ThinkingState::Done,
                            });
                        }
                    }
                    ContentBlock::Text { text } => {
                        card.blocks.push(Block::Text { text: text.clone(), streaming: false });
                    }
                    ContentBlock::ToolUse { id, name, input } => {
                        let site = tool_card(id, name, input);
                        card.nested_tools.insert(id.clone(), NestedSite {
                            index: card.blocks.len(),
                            kind: site.kind.clone(),
                            verb: site.verb.clone(),
                            target: site.target.clone(),
                            name: name.clone(),
                            params: site.params.clone(),
                        });
                        card.blocks.push(site.block);
                    }
                    ContentBlock::Other { .. } => {}
                }
            }
            card.flushed = false;
        }
        Vec::new()
    }

    /// Complete one nested tool card inside its `Agent` card's buffer. An
    /// id the buffer never saw completes nothing — it is not promoted to
    /// the main transcript, or a stray id would forge a card there.
    fn apply_nested_result(&mut self, agent_id: &str, result: &ToolResult) {
        let Some(card) = self.agents.get_mut(agent_id) else { return };
        let Some(site) = card.nested_tools.get(&result.tool_use_id) else { return };
        let block = finish_tool_block(
            &site.kind,
            &site.verb,
            &site.target,
            &site.name,
            &site.params,
            &result.tool_use_id,
            result,
        );
        if let Some(slot) = card.blocks.get_mut(site.index) {
            *slot = block;
        }
        card.flushed = false;
    }

    /// Complete the `Agent` card with its buffered nested transcript: one
    /// nested assistant turn carrying every block the sub-agent produced.
    /// Marks the buffer flushed, so the turn-end pass leaves it alone.
    fn apply_agent_result(
        &mut self,
        tool_id: &str,
        site: &ToolSite,
        status: ToolStatus,
        _result: &ToolResult,
    ) -> Option<Delta> {
        let card = self.agents.get_mut(tool_id)?;
        card.status = status;
        card.flushed = true;
        let block = agent_block(tool_id, site, status, &card.blocks);
        Some(Delta::BlockUpdated {
            turn_id: card.turn_id.clone(),
            block_index: card.block_index,
            block,
        })
    }

    /// Flush every unflushed non-empty agent buffer into its card, ahead
    /// of the turn finish: the async agent outlives the turn, and without
    /// this its buffered work would never render.
    fn flush_agents(&mut self, deltas: &mut Vec<Delta>) {
        let mut pending = Vec::new();
        for (id, card) in self.agents.iter_mut() {
            if !card.flushed && !card.blocks.is_empty() {
                card.flushed = true;
                pending.push((
                    id.clone(),
                    card.turn_id.clone(),
                    card.block_index,
                    card.status,
                    card.blocks.clone(),
                ));
            }
        }
        for (id, turn_id, block_index, status, blocks) in pending {
            if let Some(site) = self.tools.get(&id) {
                let site = site.clone();
                deltas.push(Delta::BlockUpdated {
                    turn_id,
                    block_index,
                    block: agent_block(&id, &site, status, &blocks),
                });
            }
        }
    }

    /// Queue a `can_use_tool` request as pending and card it. The card rides
    /// the running turn; with no turn open (a bare control exchange) a turn
    /// keyed by the request itself opens so the card is never dangling.
    ///
    /// A repeat request for an already-carded id cards nothing twice: the
    /// card moves only when the tool's result or the turn's denials
    /// resolve it, never on a re-request.
    fn apply_approval(&mut self, request: &ApprovalRequest) -> Vec<Delta> {
        if !self.pending.iter().any(|queued| queued.request_id == request.request_id) {
            self.pending.push(request.clone());
        }
        if self.approval_sites.contains_key(&request.request_id) {
            return Vec::new();
        }
        let mut deltas = Vec::new();
        let turn_id = self.control_turn(&request.request_id, &mut deltas);
        let block_index = self.emitted.get(&turn_id).copied().unwrap_or(0);
        deltas.push(Delta::BlockAdded {
            turn_id: turn_id.clone(),
            block: approval_card(request, &self.session_cwd),
        });
        self.emitted.insert(turn_id.clone(), block_index + 1);
        self.approval_sites.insert(
            request.request_id.clone(),
            ApprovalSite {
                turn_id: turn_id.clone(),
                block_index,
                tool_use_id: request.tool_use_id.clone(),
            },
        );
        if !request.tool_use_id.is_empty() {
            self.tool_approvals.insert(request.tool_use_id.clone(), request.request_id.clone());
        }
        // The gated call has not run: its open card (when the `tool_use`
        // arrived first) drops from Running to Pending, so no spinner ever
        // reads done beside the waiting approval.
        if let Some(site) = self.tools.get(&request.tool_use_id).cloned() {
            if let Some(block) = waiting_tool_block(&request.tool_use_id, &site) {
                deltas.push(Delta::BlockUpdated {
                    turn_id: site.turn_id,
                    block_index: site.block_index,
                    block,
                });
            }
        }
        deltas
    }

    /// Settle one carded approval to its decided state: the only thing that
    /// ever moves the card after the press. A repeat resolution for an
    /// already-settled id is a no-op.
    fn resolve_approval_card(&mut self, request_id: &str, state: ApprovalState, deltas: &mut Vec<Delta>) {
        let Some(site) = self.approval_sites.remove(request_id) else { return };
        if !site.tool_use_id.is_empty() {
            self.tool_approvals.remove(&site.tool_use_id);
        }
        // The request is on the decided shelf whenever a press preceded
        // the settling frames (the ordinary order), or still queued when
        // the frames outran the decision bookkeeping. Either way the card
        // rebuilds from it; a repeated resolution finds no site and stops
        // above.
        let request = self
            .decided
            .remove(request_id)
            .or_else(|| {
                self.pending
                    .iter()
                    .position(|queued| queued.request_id == request_id)
                    .map(|position| self.pending.remove(position))
            });
        let Some(request) = request else { return };
        let mut card = approval_card(&request, &self.session_cwd);
        if let Block::Approval { state: slot, .. } = &mut card {
            *slot = state;
        }
        deltas.push(Delta::BlockUpdated { turn_id: site.turn_id, block_index: site.block_index, block: card });
    }

    /// Record an unrecognised control subtype, queue it for an explicit
    /// decision, and card it. The card is the surfacing: without it the
    /// decision would vanish inside the noise. The queue is the handling:
    /// without it no code path could answer the request and the child
    /// would hang.
    fn apply_unknown_control(
        &mut self,
        request_id: &str,
        subtype: &str,
        raw: &serde_json::Value,
    ) -> Vec<Delta> {
        if !self.unknown_control.iter().any(|(known, _)| known == request_id) {
            self.unknown_control.push((request_id.to_owned(), subtype.to_owned()));
        }
        if !self.pending_unknown.iter().any(|queued| queued.request_id == request_id) {
            self.pending_unknown.push(UnknownControlRequest {
                request_id: request_id.to_owned(),
                subtype: subtype.to_owned(),
                raw: raw.clone(),
            });
        }
        let mut deltas = Vec::new();
        let turn_id = self.control_turn(request_id, &mut deltas);
        deltas.push(Delta::BlockAdded {
            turn_id,
            block: Block::Generic {
                kind: format!("control-request:{subtype}"),
                status: "pending".into(),
                text: format!("control_request {request_id} subtype {subtype:?}: {raw}"),
            },
        });
        deltas
    }

    /// The turn hosting a control-channel card: the running assistant turn
    /// when one is open, else a fresh turn keyed by `key` (announced in
    /// `deltas`) so the card lands somewhere the transcript owns.
    fn control_turn(&mut self, key: &str, deltas: &mut Vec<Delta>) -> String {
        if let Some(open) = self.assistant_turn.clone() {
            return open;
        }
        self.started.insert(key.to_owned(), ());
        self.open.push(key.to_owned());
        deltas.push(Delta::TurnStarted {
            turn: Turn::Assistant {
                id: key.to_owned(),
                blocks: Vec::new(),
                meta: TurnMeta::default(),
                timestamp: None,
            },
        });
        key.to_owned()
    }
}

/// Render one explicit human decision as the `control_response` line
/// answering `request`. Pure: the caller writes the line to the child's
/// stdin.
///
/// Only the card's own choices decide: `"allow"` answers
/// `{"behavior":"allow","updatedInput":{}}`, `"deny"` answers
/// `{"behavior":"deny","message":…}` with `feedback` as the reason (or a
/// default when none was given). Anything else is rejected with the offered
/// choices named. There is no default — a decision nobody made produces no
/// answer line at all.
pub fn decide_approval(
    request: &ApprovalRequest,
    choice: &str,
    feedback: Option<&str>,
) -> Result<String, ProviderError> {
    match choice {
        "allow" => Ok(crate::frame::encode_control_allow(&request.request_id)),
        "deny" => Ok(crate::frame::encode_control_deny(
            &request.request_id,
            feedback.unwrap_or("denied by the operator"),
        )),
        other => Err(ProviderError::Rejected {
            reason: format!(
                "unknown approval choice {other:?} for {}: offer \"allow\" or \"deny\"",
                request.request_id
            ),
        }),
    }
}

/// Render one explicit human decision answering an unknown-subtype control
/// request. Same wire shape as [`decide_approval`] — a refusal (`"deny"`
/// with the human's reason) is the expected default, `"allow"` the
/// deliberate override — because the child waits on a `control_response`
/// naming the `request_id`, whatever the subtype was.
pub fn decide_unknown_approval(
    request: &UnknownControlRequest,
    choice: &str,
    feedback: Option<&str>,
) -> Result<String, ProviderError> {
    match choice {
        "allow" => Ok(crate::frame::encode_control_allow(&request.request_id)),
        "deny" => Ok(crate::frame::encode_control_deny(
            &request.request_id,
            feedback.unwrap_or("denied by the operator"),
        )),
        other => Err(ProviderError::Rejected {
            reason: format!(
                "unknown approval choice {other:?} for {}: offer \"allow\" or \"deny\"",
                request.request_id
            ),
        }),
    }
}

/// One input field as plain text, for faces that name a single value.
fn approval_field(request: &ApprovalRequest, key: &str) -> String {
    request
        .input
        .get(key)
        .and_then(|value| match value {
            serde_json::Value::String(text) => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The input's remaining scalar fields as `k=v` pairs, capped: the compact
/// argument list for tools with no richer face. Keys already surfaced by
/// the caller stay out, so the card never says the path twice.
fn compact_args(request: &ApprovalRequest, skip: &[&str]) -> String {
    let mut pairs = Vec::new();
    if let Some(object) = request.input.as_object() {
        for (key, value) in object {
            if skip.contains(&key.as_str()) {
                continue;
            }
            let rendered = match value {
                serde_json::Value::String(text) => {
                    if text.chars().count() > 80 {
                        format!("{}…", text.chars().take(79).collect::<String>())
                    } else {
                        text.clone()
                    }
                }
                serde_json::Value::Null => continue,
                other => other.to_string(),
            };
            pairs.push(format!("{key}={rendered}"));
            if pairs.len() >= 4 {
                break;
            }
        }
    }
    pairs.join(", ")
}

/// A content preview: the first lines of a `Write`, capped so a big file
/// does not become a big card.
fn content_preview(content: &str) -> String {
    const MAX_LINES: usize = 5;
    const MAX_CHARS: usize = 300;
    let lines: Vec<&str> = content.lines().take(MAX_LINES).collect();
    let mut preview = lines.join("\n");
    if content.lines().count() > MAX_LINES || preview.chars().count() > MAX_CHARS {
        preview = preview.chars().take(MAX_CHARS).collect::<String>();
        preview.push('…');
    }
    preview
}

/// A small line diff of one edit: common leading/trailing lines trimmed,
/// the changed middle shown as `-`/`+` lines, capped. Plain text, not a
/// patch object — the card's reason line, not a file.
fn mini_diff(old: &str, new: &str) -> String {
    const MAX_EACH: usize = 8;
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let mut prefix = 0;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_lines.len() - prefix
        && suffix < new_lines.len() - prefix
        && old_lines[old_lines.len() - 1 - suffix] == new_lines[new_lines.len() - 1 - suffix]
    {
        suffix += 1;
    }
    let removed = &old_lines[prefix..old_lines.len() - suffix];
    let added = &new_lines[prefix..new_lines.len() - suffix];
    let mut out = Vec::new();
    for line in removed.iter().take(MAX_EACH) {
        out.push(format!("- {line}"));
    }
    if removed.len() > MAX_EACH {
        out.push(format!("… {} more removed", removed.len() - MAX_EACH));
    }
    for line in added.iter().take(MAX_EACH) {
        out.push(format!("+ {line}"));
    }
    if added.len() > MAX_EACH {
        out.push(format!("… {} more added", added.len() - MAX_EACH));
    }
    if out.is_empty() {
        out.push("(no visible change)".to_owned());
    }
    out.join("\n")
}

/// The child's ask (`decision_reason`) appended under a face that already
/// says what: why the child is asking, when it said why.
fn with_ask(mut face: String, request: &ApprovalRequest) -> String {
    if !request.decision_reason.trim().is_empty() {
        if !face.is_empty() {
            face.push('\n');
        }
        face.push_str(request.decision_reason.trim());
    }
    face
}

/// The `(command, reason, cwd)` face of a `can_use_tool` request, per tool:
/// Bash names the command and where it would run; Write names the path
/// with a content preview; Edit names the path with the diff; fetchers
/// name the URL; MCP tools name server/tool with compact args. The
/// fallback names the child's own description with compact args — still
/// never the raw input JSON, and never a wire id.
fn approval_face(request: &ApprovalRequest, session_cwd: &str) -> (String, String, String) {
    let cwd = session_cwd.to_owned();
    let fallback_command =
        if !request.description.trim().is_empty() {
            request.description.trim().to_owned()
        } else {
            approval_headline(request)
        };
    match request.tool_name.as_str() {
        "Bash" => {
            let command = approval_field(request, "command");
            let command = if command.trim().is_empty() { fallback_command } else { command };
            let reason = with_ask(command.clone(), request);
            (command, reason, cwd)
        }
        "Write" => {
            let path = approval_field(request, "file_path");
            let path = if path.trim().is_empty() { fallback_command.clone() } else { path };
            let content = approval_field(request, "content");
            let reason = if content.is_empty() {
                with_ask(path.clone(), request)
            } else {
                with_ask(format!("{path}\n{}", content_preview(&content)), request)
            };
            (path, reason, cwd)
        }
        "Edit" | "MultiEdit" => {
            let path = approval_field(request, "file_path");
            let path = if path.trim().is_empty() { fallback_command.clone() } else { path };
            let mut diffs = Vec::new();
            if request.tool_name == "MultiEdit" {
                if let Some(edits) = request.input.get("edits").and_then(|edits| edits.as_array()) {
                    for edit in edits.iter().take(3) {
                        let old = edit.get("old_string").and_then(|v| v.as_str()).unwrap_or("");
                        let new = edit.get("new_string").and_then(|v| v.as_str()).unwrap_or("");
                        if !old.is_empty() || !new.is_empty() {
                            diffs.push(mini_diff(old, new));
                        }
                    }
                }
            } else {
                let old = approval_field(request, "old_string");
                let new = approval_field(request, "new_string");
                if !old.is_empty() || !new.is_empty() {
                    diffs.push(mini_diff(&old, &new));
                }
            }
            let reason = if diffs.is_empty() {
                with_ask(path.clone(), request)
            } else {
                with_ask(format!("{path}\n{}", diffs.join("\n")), request)
            };
            (path, reason, cwd)
        }
        "Read" => {
            let path = approval_field(request, "file_path");
            let path = if path.trim().is_empty() { fallback_command.clone() } else { path };
            (path.clone(), with_ask(path, request), cwd)
        }
        "WebFetch" => {
            let url = approval_field(request, "url");
            let url = if url.trim().is_empty() { fallback_command.clone() } else { url };
            (url.clone(), with_ask(url, request), cwd)
        }
        "WebSearch" => {
            let query = approval_field(request, "query");
            let query = if query.trim().is_empty() { fallback_command.clone() } else { query };
            (query.clone(), with_ask(query, request), cwd)
        }
        _ => {
            // A terminal run asks in its own words — "Run in terminal ·
            // `<command>`" — never as the raw `baaz · Terminal Run
            // (mcp__baaz__terminal_run)` request the generic MCP face
            // would print (D51).
            if terminal_tool_name(&request.tool_name).as_deref() == Some("terminal_run") {
                let command = approval_field(request, "command");
                let command = if command.trim().is_empty() {
                    fallback_command.clone()
                } else {
                    format!("Run in terminal · `{}`", command.trim())
                };
                return (command.clone(), with_ask(command, request), cwd);
            }
            if request.mcp_server.is_some() || request.tool_name.starts_with("mcp__") {
                let server = request.mcp_server.clone().unwrap_or_default();
                let head = if server.is_empty() {
                    approval_headline(request)
                } else {
                    format!("{server} · {}", approval_headline(request))
                };
                let args = compact_args(request, &[]);
                // No bare duplication: the head already rides the command
                // line, so an arg-less call says what it allows in words.
                let reason = if args.is_empty() {
                    if server.is_empty() {
                        format!("Allow {}", approval_headline(request))
                    } else {
                        format!("Allow {} via MCP server {server}", approval_headline(request))
                    }
                } else {
                    format!("{head}\n{args}")
                };
                (head, with_ask(reason, request), cwd)
            } else {
                let args = compact_args(request, &[]);
                let reason = if !request.decision_reason.trim().is_empty() {
                    request.decision_reason.trim().to_owned()
                } else {
                    args
                };
                (fallback_command, reason, cwd)
            }
        }
    }
}

/// The card's body kind, from the tool on the wire: Bash (and the terminal
/// executor) reads as a command, the write/edit family as a file write,
/// and everything else — reads, fetchers, MCP tools — as other.
fn approval_body_kind(tool_name: &str) -> ApprovalBodyKind {
    match tool_name {
        "Bash" => ApprovalBodyKind::Command,
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => ApprovalBodyKind::FileWrite,
        _ if terminal_tool_name(tool_name).is_some() => ApprovalBodyKind::Command,
        _ => ApprovalBodyKind::Other,
    }
}

/// The transcript card for a `can_use_tool` request: a pending approval the
/// person resolves through `DecideApproval` with the card's `"allow"` /
/// `"deny"` choices.
///
/// The face is per tool, never the raw input JSON: the command line names
/// what would run, the reason says it in the tool's own terms, and no
/// wire id (`toolu_…`, `request_id`) ever reaches user-facing text.
fn approval_card(request: &ApprovalRequest, session_cwd: &str) -> Block {
    let (command, reason, cwd) = approval_face(request, session_cwd);
    let mut capabilities = Vec::new();
    for suggestion in &request.suggestions {
        for name in &suggestion.tool_names {
            if !capabilities.contains(name) {
                capabilities.push(name.clone());
            }
        }
    }
    let rule = capabilities.first().cloned();
    Block::Approval {
        id: request.request_id.clone(),
        tool: request.tool_name.clone(),
        command,
        reason,
        cwd,
        capabilities,
        scope: ApprovalScope::ThisCommand,
        body_kind: approval_body_kind(&request.tool_name),
        state: ApprovalState::Pending,
        rule,
        choices: vec![
            ApprovalChoice {
                id: "allow".into(),
                label: "Allow".into(),
                decision: ApprovalDecision::Once,
                scope: ApprovalScope::ThisCommand,
                rule_preview: None,
                accepts_feedback: false,
            },
            ApprovalChoice {
                id: "deny".into(),
                label: "Deny".into(),
                decision: ApprovalDecision::Deny,
                scope: ApprovalScope::ThisCommand,
                rule_preview: None,
                accepts_feedback: true,
            },
        ],
        stages: Vec::new(),
        current_stage: None,
        badges: ApprovalBadges::default(),
        feedback: None,
        resolved_by: None,
    }
}

struct ToolCard {
    kind: ToolKind,
    verb: String,
    target: String,
    params: Vec<(String, String)>,
    block: Block,
}

/// The opening card for a `tool_use` block: shell for `Bash`, MCP for
/// `mcp__<server>__<tool>`, and the mandated [`Block::Generic`] fallback
/// for anything else (a richer card would be a guess).
fn tool_card(id: &str, name: &str, input: &serde_json::Value) -> ToolCard {
    let params = input
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| {
                    let rendered = match value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    (key.clone(), rendered)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if name == "Bash" {
        let target =
            input.get("command").and_then(serde_json::Value::as_str).unwrap_or(name).to_owned();
        // The opening verb is present tense: the call has not run yet, and
        // past tense beside a spinner reads done while an approval may
        // still be gating it. The result maps it back (see `past_verb`).
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Shell,
            verb: "Run".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: true },
            diff_stat: None,
        };
        ToolCard { kind: ToolKind::Shell, verb: "Run".into(), target, params, block }
    } else if matches!(name, "Write" | "Edit" | "Read") {
        // File cards: the header names the path; the body (diff, line
        // count) lands when the result's structured detail arrives. Verbs
        // stay present until the result lands, for the same reason.
        let (kind, verb) = match name {
            "Write" => (ToolKind::Write, "Write"),
            "Edit" => (ToolKind::Edit, "Edit"),
            _ => (ToolKind::Read, "Read"),
        };
        let target = input
            .get("file_path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(name)
            .to_owned();
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: kind.clone(),
            verb: verb.into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::None,
            diff_stat: None,
        };
        ToolCard { kind, verb: verb.into(), target, params, block }
    } else if name == "Skill" {
        // A skill invocation: the quiet "Loaded skill `name`" row, the same
        // one `read_skill` folds to on the muse lane (D63). The tool's input
        // names the skill under `skill`; without one the card names the tool
        // rather than guessing.
        let target =
            input.get("skill").and_then(serde_json::Value::as_str).unwrap_or(name).to_owned();
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Read,
            verb: "Loaded skill".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::None,
            diff_stat: None,
        };
        ToolCard { kind: ToolKind::Read, verb: "Loaded skill".into(), target, params, block }
    } else if name == "Agent" {
        // A sub-agent delegation: the card nests the agent's own blocks
        // (routed by `parent_tool_use_id`) once they arrive.
        let agent_type = input
            .get("subagent_type")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let description = input
            .get("description")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let target = match (agent_type.is_empty(), description.is_empty()) {
            (false, false) => format!("{agent_type}: {description}"),
            (false, true) => agent_type.to_owned(),
            (true, false) => description.to_owned(),
            (true, true) => name.to_owned(),
        };
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::SubAgent,
            verb: "Delegate".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::SubAgent { turns: Vec::new() },
            diff_stat: None,
        };
        ToolCard { kind: ToolKind::SubAgent, verb: "Delegate".into(), target, params, block }
    } else if terminal_tool_name(name).is_some() {
        // A baaz terminal tool call opens as a shell card with the
        // command in the header (D51): the result completes it to "Ran
        // in terminal" with the tab, the exit and the output.
        let target = input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .or_else(|| input.get("tab").and_then(serde_json::Value::as_str))
            .filter(|text| !text.trim().is_empty())
            .unwrap_or(name)
            .to_owned();
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Shell,
            verb: TERMINAL_RUNNING_VERB.into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: true },
            diff_stat: None,
        };
        ToolCard { kind: ToolKind::Shell, verb: TERMINAL_RUNNING_VERB.into(), target, params, block }
    } else if let Some((server, tool)) = mcp_split(name) {
        let target = format!("{server} · {tool}");
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Mcp { server: server.clone(), tool: tool.clone() },
            verb: "Run".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::Mcp { params: params.clone(), result_json: String::new() },
            diff_stat: None,
        };
        ToolCard {
            kind: ToolKind::Mcp { server, tool },
            verb: "Run".into(),
            target,
            params,
            block,
        }
    } else {
        let text = if params.is_empty() {
            format!("{name} called")
        } else {
            let pairs =
                params.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>();
            format!("{name} called ({})", pairs.join(", "))
        };
        ToolCard {
            kind: ToolKind::Search,
            verb: String::new(),
            target: String::new(),
            params: Vec::new(),
            block: Block::Generic { kind: name.to_owned(), status: "running".into(), text },
        }
    }
}

/// Rebuild an open tool card in its gated state: `Pending` with its opening
/// verb while the approval waits, `Cancelled` reading "Denied" once the
/// turn's denials confirm it never ran. Nothing is invented — the shape is
/// the opening card's, minus any live output (nothing ran, so nothing
/// streams). SubAgent cards are left alone (`None`): their nested
/// transcript is the truth about them, not the gate.
fn gated_tool_block(
    tool_use_id: &str,
    site: &ToolSite,
    status: ToolStatus,
    verb: &str,
) -> Option<Block> {
    let body = match &site.kind {
        ToolKind::Shell => ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: false },
        ToolKind::Edit | ToolKind::Write | ToolKind::Read => ToolBody::None,
        ToolKind::Mcp { .. } => {
            ToolBody::Mcp { params: site.params.clone(), result_json: String::new() }
        }
        _ => return None,
    };
    Some(Block::ToolCall {
        id: tool_use_id.to_owned(),
        kind: site.kind.clone(),
        verb: verb.to_owned(),
        target: site.target.clone(),
        status,
        duration_ms: None,
        body,
        diff_stat: None,
    })
}

/// The gated call has not run: its open card drops to waiting.
fn waiting_tool_block(tool_use_id: &str, site: &ToolSite) -> Option<Block> {
    gated_tool_block(tool_use_id, site, ToolStatus::Pending, &site.verb)
}

/// Split `mcp__<server>__<tool>`; anything else is not an MCP name.
fn mcp_split(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server.to_owned(), tool.to_owned()))
}

/// Read NDJSON frames from `reader` — the child's stdout in production, a
/// fixture file in tests — and call `emit` for every [`ProviderEvent`).
///
/// This is the ONE stream path: the child lane and the fixture tests both
/// come through here, so a test that folds a fixture exercises the same
/// decode-and-fold the live child does. Blank lines are skipped; a line
/// that is not JSON is skipped rather than killing the pump (a torn final
/// line from a dying child must not lose the turn before it).
/// Fold one raw line: skip blanks and torn JSON, decode, fold, emit.
/// The per-line unit of [`pump_reader`], factored out so the live child —
/// which shares its fold under a mutex for `ReadAccount` — runs the same
/// decode-and-fold per line while locking only around that line.
pub fn step_line(fold: &mut ClaudeFold, line: &str, emit: &mut impl FnMut(ProviderEvent)) {
    if line.trim().is_empty() {
        return;
    }
    let Ok(frame) = crate::frame::decode_line(line) else { return };
    let session_id =
        frame.session_id().map(str::to_owned).or_else(|| fold.session_id().map(str::to_owned));
    // A permission request the child waits on: the transcript card arrives
    // through the fold below, and this tap tells the app a human decision is
    // owed. Nothing here answers it — answering is an explicit
    // `DecideApproval` carrying one of the card's own choices.
    let tap = match &frame {
        Frame::ControlRequest(request) => Some(ProviderEvent::ApprovalRequested {
            session_id: session_id.clone().unwrap_or_default(),
            approval_id: request.request_id.clone(),
            headline: approval_headline(request),
        }),
        _ => None,
    };
    let deltas = fold.apply(&frame);
    if !deltas.is_empty() {
        emit(ProviderEvent::Deltas { session_id: session_id.clone(), deltas });
    }
    if let Some(tap) = tap {
        emit(tap);
    }
}

/// Fold a whole stream: [`step_line`] per line until EOF or a broken pipe.
pub fn pump_reader<R: BufRead>(reader: R, fold: &mut ClaudeFold, mut emit: impl FnMut(ProviderEvent)) {
    for line in reader.lines() {
        let Ok(line) = line else { break };
        step_line(fold, &line, &mut emit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::decode_line;

    fn fixture_lines(name: &str) -> Vec<String> {
        let path = format!("{}/../../fixtures/claude-code/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture reads").lines().map(str::to_owned).collect()
    }

    fn fold_lines(lines: &[String]) -> (ClaudeFold, Vec<Delta>) {
        let mut fold = ClaudeFold::new();
        let mut deltas = Vec::new();
        for line in lines {
            deltas.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        (fold, deltas)
    }

    fn text_of(block: &Block) -> Option<&str> {
        match block {
            Block::Text { text, .. } => Some(text),
            _ => None,
        }
    }

    fn replay(name: &str) -> (ClaudeFold, Vec<Delta>) {
        fold_lines(&fixture_lines(name))
    }

    /// The line index just past the n-th `can_use_tool` control request
    /// (1-based): the point in the replay where n asks wait and nothing
    /// later has answered or run yet.
    fn nth_control_request(lines: &[String], n: usize) -> Option<usize> {
        let mut seen = 0;
        lines
            .iter()
            .position(|line| {
                let is_ask = serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .map(|value| {
                        value.get("type").and_then(|kind| kind.as_str()) == Some("control_request")
                            && value
                                .get("request")
                                .and_then(|request| request.get("subtype"))
                                .and_then(|subtype| subtype.as_str())
                                == Some("can_use_tool")
                    })
                    .unwrap_or(false);
                if is_ask {
                    seen += 1;
                    seen == n
                } else {
                    false
                }
            })
            .map(|index| index + 1)
    }

    fn user_turns(deltas: &[Delta]) -> Vec<(&str, usize)> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnStarted {
                    turn: Turn::User { text, attachments, .. },
                    ..
                } => Some((text.as_str(), attachments.len())),
                _ => None,
            })
            .collect()
    }

    fn finished_metas(deltas: &[Delta]) -> Vec<TurnMeta> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta.clone()),
                _ => None,
            })
            .collect()
    }

    fn tool_updates(deltas: &[Delta]) -> Vec<Block> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated { block, .. } => Some(block.clone()),
                _ => None,
            })
            .collect()
    }

    /// A submit carrying `display_text` bubbles the short text, not the
    /// whole replayed echo; without one the echo renders as before. The
    /// first fold teaches the replayed message's uuid, so a stored
    /// history replayed through a seeded fold still shows the short text.
    #[test]
    fn submit_display_text_replaces_the_replayed_bubble() {
        fn echo(uuid: &str, text: &str) -> Frame {
            Frame::UserText {
                session_id: "s".to_owned(),
                uuid: uuid.to_owned(),
                text: text.to_owned(),
                images: Vec::new(),
            }
        }
        let full = "Continuing a session handed off from Muse. Context follows.\n## Original goal\nRename Ledger to Journal";
        let short = "Handed off from Muse: Rename Ledger to Journal (1 recent turns, 0 open todos, 0 files touched)";
        // With display_text the replayed echo bubbles the short text, and
        // the uuid join learns it.
        let mut fold = ClaudeFold::new();
        fold.record_display_text(full, short);
        let deltas = fold.apply(&echo("u-1", full));
        assert_eq!(user_texts(&deltas), [short]);
        assert_eq!(
            fold.display_by_uuid.get("u-1").map(String::as_str),
            Some(short),
            "the replayed message's uuid keeps the short text"
        );
        // Without display_text the echo renders whole, as before.
        let mut fold = ClaudeFold::new();
        let deltas = fold.apply(&echo("u-2", full));
        assert_eq!(user_texts(&deltas), [full]);
        // A seeded replay fold (what `stored_history` builds after a
        // restart) still bubbles the short text for the same echo.
        let mut live = ClaudeFold::new();
        live.record_display_text(full, short);
        let mut replay = ClaudeFold::new();
        replay.set_display_overrides(live.display_overrides());
        let deltas = replay.apply(&echo("u-1", full));
        assert_eq!(user_texts(&deltas), [short]);
        // An identical display records nothing: the typed text stands.
        let mut fold = ClaudeFold::new();
        fold.record_display_text(full, full);
        let deltas = fold.apply(&echo("u-3", full));
        assert_eq!(user_texts(&deltas), [full]);
    }

    /// Defect 1, pinned on `edit.jsonl`: the replayed prompt echo folds
    /// to exactly one user turn carrying the submitted text. Removing the
    /// `UserText` arm (or the `--replay-user-messages` flag that produces
    /// the echo) fails this — the turn would show tools and reply with no
    /// bubble for what the person sent.
    #[test]
    fn user_echo_folds_to_exactly_one_user_turn() {
        let (_, deltas) = replay("edit.jsonl");
        let users = user_turns(&deltas);
        assert_eq!(users.len(), 1, "one submitted prompt, one bubble");
        assert!(users[0].0.contains("greet.txt"), "the prompt text: {}", users[0].0);
        assert_eq!(users[0].1, 0, "no attachments on this turn");
    }

    fn resume_segments() -> (Vec<String>, Vec<String>) {
        // `resume-replay.jsonl` is two child runs concatenated: two turns,
        // quit, resume, one more turn. The split is the resume's
        // `SessionStart:resume` hook — everything before is history,
        // everything after is the resumed child's stream.
        let lines = fixture_lines("resume-replay.jsonl");
        let at = lines
            .iter()
            .position(|line| line.contains("\"hook_name\":\"SessionStart:resume\""))
            .expect("the resume run starts with its own SessionStart hook");
        (lines[..at].to_vec(), lines[at..].to_vec())
    }

    fn user_texts(deltas: &[Delta]) -> Vec<&str> {
        user_turns(deltas).iter().map(|(text, _)| *text).collect()
    }

    /// `resume-replay.jsonl` (captured live: two turns, quit, resume, one
    /// more turn, all with `--replay-user-messages`): the folded session
    /// shows each user turn exactly once — history plus the new turn.
    #[test]
    fn resume_replay_folds_each_user_turn_exactly_once() {
        let (history, resumed) = resume_segments();
        // The capture's own finding, pinned: the resumed child re-emits
        // only the new turn. If a future CLI replays history on resume,
        // this fails first — revisit the seeding, not the assertion.
        let resumed_folded = fold_lines(&resumed);
        let resumed_users = user_texts(&resumed_folded.1);
        assert_eq!(resumed_users, ["Say R3"], "no history re-emitted: {resumed_users:?}");
        // And the whole session — history plus the new turn — bubbles
        // each prompt exactly once.
        let mut all = history.clone();
        all.extend(resumed.clone());
        let folded = fold_lines(&all);
        let users = user_texts(&folded.1);
        assert_eq!(users, ["Say R1", "Say R2", "Say R3"], "each turn once: {users:?}");
    }

    /// The seeded resume: history the adapter already showed (the stored
    /// transcript carries the same uuids the stream repeats — proven by
    /// the capture) is pre-marked, so the resumed stream cannot bubble it
    /// again, while the new turn still bubbles. Without the seeding the
    /// repeated echo would mint a second bubble for the same uuid.
    #[test]
    fn resume_seed_drops_repeated_history_echo_keeps_new_turn() {
        let (history, resumed) = resume_segments();
        let history_uuids: Vec<String> = history
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .filter_map(|frame| match frame {
                Frame::UserText { uuid, .. } => Some(uuid),
                _ => None,
            })
            .collect();
        assert_eq!(history_uuids.len(), 2, "history holds R1 and R2");
        // Unseeded control: the resume stream bubbles its new turn — the
        // flag is load-bearing here (a flagless resume emits no echo at
        // all, probed live as R4). Seeding must not break this.
        let unseeded_folded = fold_lines(&resumed);
        let unseeded = user_texts(&unseeded_folded.1);
        assert_eq!(unseeded, ["Say R3"], "the new turn bubbles: {unseeded:?}");
        // Seeded with history: the repeated R1/R2 echoes (same uuids
        // the stored transcript carries) render nothing — history was
        // already shown elsewhere — while R3, never seen, still bubbles
        // exactly once.
        let mut fold = ClaudeFold::new();
        for uuid in &history_uuids {
            fold.mark_user_echo_seen(uuid);
        }
        fold.mark_user_echo_seen("");
        let mut deltas = Vec::new();
        for line in history.iter().chain(resumed.iter()) {
            deltas.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        assert_eq!(user_texts(&deltas), ["Say R3"], "seeded history stays silent");
        // And when history already covers everything (the stored file
        // written after the new turn landed), the repeated stream echoes
        // bubble nothing at all — but the assistant's reply still renders.
        let mut covered = ClaudeFold::new();
        for line in resumed.iter() {
            if let Ok(Frame::UserText { uuid, .. }) = decode_line(line) {
                covered.mark_user_echo_seen(&uuid);
            }
        }
        let mut redeltas = Vec::new();
        for line in &resumed {
            redeltas.extend(covered.apply(&decode_line(line).expect("decodes")));
        }
        assert!(
            user_texts(&redeltas).is_empty(),
            "covered history never re-bubbles"
        );
        let texts: Vec<&str> = redeltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block, .. } => text_of(block),
                _ => None,
            })
            .collect();
        assert!(texts.contains(&"R3"), "the reply survives the seeding: {texts:?}");
    }

    /// Defect 4, pinned on `basic.jsonl` (a pre-replay capture, so no
    /// bubble is involved): the billed prompt is bare input PLUS both
    /// cache legs. `input_tokens` alone reads 10 for a ~30k prompt; the
    /// fold sums all three into `tokens_in` while keeping the cache legs
    /// informational (never added on top — that double-counts).
    #[test]
    fn tokens_in_sums_bare_input_and_both_cache_legs() {
        let (_, deltas) = replay("basic.jsonl");
        let metas = finished_metas(&deltas);
        assert!(!metas.is_empty(), "the turn finishes with a footer");
        for meta in &metas {
            assert_eq!(meta.tokens_in, 10 + 21448 + 8165, "the whole billed prompt");
            assert_eq!(meta.cache_read_tokens, Some(21448));
            assert_eq!(meta.cache_write_tokens, Some(8165));
            assert_eq!(meta.tokens_out, 57);
        }
    }

    /// `edit.jsonl`: Write then Edit fold to file cards with server-honest
    /// diff chips — `+1/−0` for the creation, `+2/−1` for the edit — and
    /// both approvals on the wire precede their runs: mid-replay both asks
    /// wait, and the full replay settles both cards to allowed as their
    /// tools run.
    #[test]
    fn edit_fixture_folds_write_and_edit_cards_with_diff_stats() {
        let lines = fixture_lines("edit.jsonl");
        let write_asked_at = nth_control_request(&lines, 1).expect("the Write ask on the wire");
        let (fold, _) = fold_lines(&lines[..write_asked_at]);
        assert_eq!(fold.pending_approvals().len(), 1, "Write asked");
        let edit_asked_at = nth_control_request(&lines, 2).expect("two asks on the wire");
        let (fold, _) = fold_lines(&lines[..edit_asked_at]);
        // The Write already ran, so only the Edit still waits: settling
        // follows each run, never the end of the turn.
        assert_eq!(fold.pending_approvals().len(), 1, "Write settled, Edit asked");
        let (fold, deltas) = replay("edit.jsonl");
        assert!(fold.pending_approvals().is_empty(), "both asks settled as their tools ran");
        let allowed = deltas
            .iter()
            .filter(|delta| {
                matches!(
                    delta,
                    Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
                )
            })
            .count();
        assert_eq!(allowed, 2, "Write allowed, Edit allowed");
        let updated = tool_updates(&deltas);
        // The waiting flip lands before each completion, so the last
        // update per card is the one that ran.
        let write = updated.iter().rev().find_map(|block| match block {
            Block::ToolCall { kind: ToolKind::Write, target, status, body, diff_stat, .. } => {
                Some((target.clone(), *status, body.clone(), *diff_stat))
            }
            _ => None,
        }).expect("the Write card completes");
        assert!(write.0.ends_with("greet.txt"), "target: {}", write.0);
        assert_eq!(write.1, ToolStatus::Success);
        assert!(matches!(write.2, ToolBody::Edit { .. }), "created content as a diff");
        assert_eq!(
            write.3.map(|stat| (stat.added, stat.removed, stat.files)),
            Some((1, 0, 1)),
            "one line created"
        );
        let edit = updated.iter().rev().find_map(|block| match block {
            Block::ToolCall { kind: ToolKind::Edit, target, status, body, diff_stat, .. } => {
                Some((target.clone(), *status, body.clone(), *diff_stat))
            }
            _ => None,
        }).expect("the Edit card completes");
        assert!(edit.0.ends_with("greet.txt"), "target: {}", edit.0);
        assert_eq!(edit.1, ToolStatus::Success);
        match &edit.2 {
            ToolBody::Edit { diff } => {
                let rows: Vec<String> = diff
                    .hunks
                    .iter()
                    .flat_map(|hunk| hunk.lines.iter())
                    .map(|line| {
                        let prefix = match line.kind {
                            DiffKind::Add => "+",
                            DiffKind::Del => "-",
                            DiffKind::Context => " ",
                        };
                        format!("{prefix}{}", line.text)
                    })
                    .collect();
                assert!(rows.contains(&"-hi there".to_owned()), "rows: {rows:?}");
                assert!(rows.contains(&"+hello there".to_owned()), "rows: {rows:?}");
                assert!(rows.contains(&"+bye".to_owned()), "rows: {rows:?}");
            }
            other => panic!("the Edit body is a diff, got {other:?}"),
        }
        assert_eq!(
            edit.3.map(|stat| (stat.added, stat.removed, stat.files)),
            Some((2, 1, 1)),
            "+2/−1 chip"
        );
    }

    /// `read-search.jsonl`: the Read card counts the wire's own `numLines`
    /// and the grep run keeps its output on a shell card.
    #[test]
    fn read_search_fixture_folds_read_card_and_shell_search() {
        let (_, deltas) = replay("read-search.jsonl");
        let updated = tool_updates(&deltas);
        let read = updated.iter().find_map(|block| match block {
            Block::ToolCall { kind: ToolKind::Read, target, body, .. } => {
                Some((target.clone(), body.clone()))
            }
            _ => None,
        }).expect("the Read card completes");
        assert!(read.0.ends_with("notes.txt"), "target: {}", read.0);
        assert!(
            matches!(read.1, ToolBody::Read { lines: 3 }),
            "the wire's own line count: {read:?}"
        );
        let shell = updated.iter().find_map(|block| match block {
            Block::ToolCall {
                kind: ToolKind::Shell, target, status, body, ..
            } => Some((target.clone(), *status, body.clone())),
            _ => None,
        }).expect("the grep run completes");
        assert!(shell.0.contains("grep"), "the model's command: {}", shell.0);
        assert_eq!(shell.1, ToolStatus::Success);
        match &shell.2 {
            ToolBody::Shell { output_lines, exit_code, .. } => {
                assert_eq!(output_lines, &["3:gamma"]);
                assert_eq!(*exit_code, None, "success carries no exit prefix");
            }
            other => panic!("a shell body, got {other:?}"),
        }
    }

    /// `error.jsonl`: the failing run folds to an `Error` card with the
    /// CLI's own exit code parsed off its `Exit code N` prefix, and the
    /// turn still finishes. The wire asks first, so the card waits as
    /// `Pending` beside the approval before the error completes it —
    /// pending while gated, error only once it ran and failed.
    #[test]
    fn error_fixture_marks_shell_error_with_exit_code() {
        let (_, deltas) = replay("error.jsonl");
        let updated = tool_updates(&deltas);
        let shells: Vec<_> = updated
            .iter()
            .filter_map(|block| match block {
                Block::ToolCall {
                    kind: ToolKind::Shell, target, status, body, ..
                } => Some((target.clone(), *status, body.clone())),
                _ => None,
            })
            .collect();
        assert!(
            shells.iter().any(|(_, status, _)| *status == ToolStatus::Pending),
            "the gated run waits before it fails: {shells:?}"
        );
        let shell = shells.last().expect("the failed run completes its card");
        assert_eq!(shell.0, "ls /nonexistent-dir-xyz-123");
        assert_eq!(shell.1, ToolStatus::Error);
        match &shell.2 {
            ToolBody::Shell { output_lines, exit_code, .. } => {
                assert!(output_lines.iter().any(|line| line.contains("No such file")),
                    "stderr survives: {output_lines:?}");
                assert_eq!(*exit_code, Some(1), "parsed off the Exit code prefix");
            }
            other => panic!("a shell body, got {other:?}"),
        }
        assert!(!finished_metas(&deltas).is_empty(), "the turn still finishes");
    }

    /// `error.jsonl`: the Bash `can_use_tool` ask cards as a command — the
    /// `$ ` prompt face — while the Write ask in `approval-default.jsonl`
    /// cards as a file write (asserted in
    /// `approval_default_fixture_cards_pending_approval_then_runs`).
    #[test]
    fn bash_approval_cards_command_body_kind() {
        let (_, deltas) = replay("error.jsonl");
        let cards: Vec<Block> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: card @ Block::Approval { .. }, .. } => {
                    Some(card.clone())
                }
                _ => None,
            })
            .collect();
        assert!(!cards.is_empty(), "the Bash ask cards on the transcript");
        match &cards[0] {
            Block::Approval { tool, body_kind, .. } => {
                assert_eq!(tool, "Bash");
                assert_eq!(*body_kind, ApprovalBodyKind::Command, "a Bash ask reads as a command");
            }
            other => panic!("an approval card, got {other:?}"),
        }
    }

    /// `todo.jsonl`: three TaskCreate calls plus their status moves fold
    /// to exactly one `Todo` card — opened once, rewritten after — ending
    /// with all three rows done.
    #[test]
    fn todo_fixture_keeps_one_todo_card_with_three_done_items() {
        let (_, deltas) = replay("todo.jsonl");
        let added = deltas
            .iter()
            .filter(|delta| matches!(
                delta, Delta::BlockAdded { block: Block::Todo { .. }, .. }
            ))
            .count();
        assert_eq!(added, 1, "one card no matter how many calls refine it");
        let todos: Vec<Vec<TodoItem>> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Todo { items }, .. }
                | Delta::BlockUpdated { block: Block::Todo { items }, .. } => {
                    Some(items.clone())
                }
                _ => None,
            })
            .collect();
        assert!(!todos.is_empty(), "the card renders");
        let last = todos.last().expect("a card rendered");
        assert_eq!(last.len(), 3, "three tracked tasks");
        for item in last {
            assert_eq!(item.state, TodoState::Done, "row done: {}", item.label);
        }
        let labels: Vec<&str> = last.iter().map(|item| item.label.as_str()).collect();
        for want in ["List files", "Read notes.txt", "Summarize contents"] {
            assert!(labels.contains(&want), "row present: {labels:?}");
        }
        // And the work itself still cards: the list-files Bash run and
        // the notes Read completed beside the plan.
        assert!(
            tool_updates(&deltas).iter().any(|block| matches!(
                block, Block::ToolCall { kind: ToolKind::Shell, status: ToolStatus::Success, .. }
            )),
            "the plan's work still renders"
        );
    }

    /// `subagent.jsonl`: the `Agent` delegation nests the sub-agent's own
    /// blocks — its Read of notes.txt, completed — inside one `SubAgent`
    /// card, flushed before the turn finishes (the agent still runs
    /// async when the turn ends).
    #[test]
    fn subagent_fixture_nests_transcript_in_agent_card() {
        let (_, deltas) = replay("subagent.jsonl");
        let agents: Vec<Block> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated {
                    block: nested @ Block::ToolCall { kind: ToolKind::SubAgent, .. },
                    ..
                } => Some(nested.clone()),
                _ => None,
            })
            .collect();
        assert!(!agents.is_empty(), "the delegation card completes");
        let last = agents.last().expect("a card completed");
        match last {
            Block::ToolCall { verb, target, body: ToolBody::SubAgent { turns }, .. } => {
                assert_eq!(verb, "Delegated");
                assert!(target.contains("Explore"), "target: {target}");
                assert_eq!(turns.len(), 1, "one nested turn");
                let nested_read = turns[0].blocks().iter().any(|block| matches!(
                    block,
                    Block::ToolCall { kind: ToolKind::Read, status: ToolStatus::Success, .. }
                ));
                assert!(nested_read, "the nested Read completed inside the card");
            }
            other => panic!("a SubAgent card, got {other:?}"),
        }
        // The flush lands before the finish: without it the async
        // agent's buffered work would never render.
        let finish_at = deltas
            .iter()
            .position(|delta| matches!(delta, Delta::TurnFinished { .. }))
            .expect("the turn finishes");
        let agent_update_at = deltas
            .iter()
            .position(|delta| matches!(
                delta,
                Delta::BlockUpdated {
                    block: Block::ToolCall { kind: ToolKind::SubAgent, .. },
                    ..
                }
            ))
            .expect("the card flushed");
        assert!(agent_update_at < finish_at, "nested work renders before the finish");
    }

    fn agent_tool_frame(message: &str, tool: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {
                "id": message,
                "content": [{
                    "type": "tool_use",
                    "id": tool,
                    "name": "Agent",
                    "input": {"subagent_type": "Explore", "description": tool},
                }],
            },
            "session_id": "s",
            "uuid": format!("u-{message}"),
            "parent_tool_use_id": null,
        })
        .to_string()
    }

    fn nested_text_frame(tool: &str, text: &str) -> String {
        serde_json::json!({
            "type": "assistant",
            "message": {"id": format!("{tool}-nested"), "content": [{"type": "text", "text": text}]},
            "session_id": "s",
            "uuid": format!("u-{tool}-nested"),
            "parent_tool_use_id": tool,
        })
        .to_string()
    }

    /// Two (and three) concurrent subagents flush in tool-use id order,
    /// not hash order: the turn-end pass used to iterate a `HashMap`, so
    /// the card sequence depended on the hasher. Each id set below runs
    /// in one fresh fold and must come out sorted — over many distinct
    /// id sets a hash-ordered flush could not stay sorted every time.
    #[test]
    fn concurrent_subagents_flush_in_id_order() {
        for tools in [
            vec!["toolu_02", "toolu_01"],
            vec!["toolu_zz", "toolu_aa"],
            vec!["toolu_c", "toolu_a", "toolu_b"],
            vec!["toolu_10", "toolu_9"],
        ] {
            let mut fold = ClaudeFold::new();
            let mut deltas = Vec::new();
            // One message opens every delegation; each agent buffers one
            // nested text block; the turn ends with all agents still
            // running, so the flush — not an agent result — renders them.
            for &tool in &tools {
                let line = agent_tool_frame("msg-1", tool);
                deltas.extend(fold.apply(&decode_line(&line).expect("decodes")));
            }
            for &tool in &tools {
                let line = nested_text_frame(tool, &format!("work from {tool}"));
                deltas.extend(fold.apply(&decode_line(&line).expect("decodes")));
            }
            let end = decode_line(r#"{"type":"result","session_id":"s"}"#).expect("decodes");
            deltas.extend(fold.apply(&end));
            let flushed: Vec<&str> = deltas
                .iter()
                .filter_map(|delta| match delta {
                    Delta::BlockUpdated {
                        block: Block::ToolCall { id, kind: ToolKind::SubAgent, .. },
                        ..
                    } => Some(id.as_str()),
                    _ => None,
                })
                .collect();
            let mut sorted = tools.clone();
            sorted.sort_unstable();
            assert_eq!(flushed, sorted, "flush order is id order for {tools:?}");
        }
    }

    /// The todo-result fan-out resolves deterministically: two cards from
    /// two turns hold the same provisional key (a replayed `tool_use` id),
    /// and the creation result renames exactly the card whose message
    /// sorts first. Over many message-id pairs a hash-ordered scan could
    /// not pick the first-sorted card every time.
    #[test]
    fn todo_result_fanout_resolves_first_sorted_card() {
        for (later, earlier) in [
            ("msg-b", "msg-a"),
            ("msg-2", "msg-10"),
            ("msg-zzz", "msg-aaa"),
            ("msg-k", "msg-c"),
        ] {
            let mut fold = ClaudeFold::new();
            let mut deltas = Vec::new();
            for message in [later, earlier] {
                let line = serde_json::json!({
                    "type": "assistant",
                    "message": {
                        "id": message,
                        "content": [{
                            "type": "tool_use",
                            "id": "toolu_shared",
                            "name": "TaskCreate",
                            "input": {"subject": message},
                        }],
                    },
                    "session_id": "s",
                    "uuid": format!("u-{message}"),
                    "parent_tool_use_id": null,
                })
                .to_string();
                deltas.extend(fold.apply(&decode_line(&line).expect("decodes")));
                // Close the turn so the next call opens a second card
                // rather than refining the first.
                let end =
                    decode_line(r#"{"type":"result","session_id":"s"}"#).expect("decodes");
                deltas.extend(fold.apply(&end));
            }
            let result = serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result",
                        "tool_use_id": "toolu_shared",
                        "content": "Task #7 created successfully: shared",
                    }],
                },
                "session_id": "s",
                "uuid": "u-result",
                "parent_tool_use_id": null,
            })
            .to_string();
            deltas.extend(fold.apply(&decode_line(&result).expect("decodes")));
            let updated: Vec<&str> = deltas
                .iter()
                .filter_map(|delta| match delta {
                    Delta::BlockUpdated { turn_id, block: Block::Todo { .. }, .. } => {
                        Some(turn_id.as_str())
                    }
                    _ => None,
                })
                .collect();
            assert_eq!(updated, [earlier], "the first-sorted card wins for {later}/{earlier}");
        }
    }

    /// `thinking.jsonl` (sonnet, 8-puzzle): the thinking *text* is
    /// signature-only on this wire (empty), so no thinking block folds —
    /// an empty "Thought for 0.0 s" shell is noise — and the footer counts
    /// the reasoning the wire reports: 262 thinking tokens, with the whole
    /// billed prompt in `tokens_in`. The count carries the evidence the
    /// redacted trace cannot. Remove the empty-text skip and an empty
    /// thinking card folds again.
    #[test]
    fn thinking_fixture_skips_empty_thinking_but_counts_reasoning() {
        let (_, deltas) = replay("thinking.jsonl");
        let thinking =
            deltas.iter().filter(|delta| matches!(
                delta, Delta::BlockAdded { block: Block::Thinking { .. }, .. }
            )).count();
        assert_eq!(thinking, 0, "redacted thinking folds to no block");
        let metas = finished_metas(&deltas);
        assert!(!metas.is_empty());
        for meta in &metas {
            assert_eq!(meta.reasoning_tokens, 262, "the wire's thinking count");
            assert_eq!(meta.tokens_in, 2 + 44200, "bare input plus cache creation");
            assert_eq!(meta.cache_write_tokens, Some(44200));
        }
    }

    /// `approval-default.jsonl`: the Write approval cards as pending with
    /// its own allow/deny choices, and after the allow the write runs to
    /// a completed card — the transcript shows ask-then-run, not just run.
    /// The decision itself settles nothing: the card moves only when the
    /// write's result lands.
    #[test]
    fn approval_default_fixture_cards_pending_approval_then_runs() {
        let lines = fixture_lines("approval-default.jsonl");
        let asked_at = nth_control_request(&lines, 1).expect("the Write ask on the wire");
        let (mut fold, ask_deltas) = fold_lines(&lines[..asked_at]);
        // The ask waits, exactly as the live child waits for its answer.
        // Deciding it must mint the same answer the capture sent.
        assert_eq!(fold.pending_approvals().len(), 1, "the Write ask waits");
        let request = fold.pending_approvals()[0].clone();
        assert_eq!(request.tool_name, "Write");
        let minted = crate::fold::decide_approval(&request, "allow", None)
            .expect("allow is the card's own choice");
        let sent = fixture_lines("approval-default.jsonl")
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter_map(|line| line.get("frame").cloned())
            .find(|frame| {
                frame
                    .get("response")
                    .and_then(|response| response.get("request_id"))
                    .and_then(serde_json::Value::as_str)
                    == Some(request.request_id.as_str())
            })
            .expect("the capture answered this request");
        let minted_value: serde_json::Value =
            serde_json::from_str(&minted).expect("the minted answer is JSON");
        assert_eq!(&minted_value, &sent, "the decision matches the live answer");
        let approvals: Vec<Block> = ask_deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: card @ Block::Approval { .. }, .. } => {
                    Some(card.clone())
                }
                _ => None,
            })
            .collect();
        assert_eq!(approvals.len(), 1, "one asked approval, one card");
        match &approvals[0] {
            Block::Approval { tool, state, choices, command, reason, cwd, body_kind, .. } => {
                assert_eq!(tool, "Write");
                assert_eq!(*body_kind, ApprovalBodyKind::FileWrite, "a Write ask reads as a file write");
                assert_eq!(*state, ApprovalState::Pending);
                assert!(
                    choices.iter().any(|choice| choice.id == "allow")
                        && choices.iter().any(|choice| choice.id == "deny"),
                    "the card's own choices"
                );
                // The Write face names the path with a content preview —
                // never the raw input JSON, never a wire id — and where it
                // would run.
                assert_eq!(command, "/tmp/w4b/outside-claude.txt", "the path, not JSON");
                assert!(reason.contains("APPROVED"), "the content preview: {reason}");
                assert!(!reason.contains("toolu_"), "no wire ids: {reason}");
                assert!(!reason.contains('{'), "no raw JSON: {reason}");
                assert!(!cwd.is_empty(), "the card names where the tool would run");
            }
            other => panic!("an approval card, got {other:?}"),
        }
        // The press itself settles nothing: take the decision, then run
        // the rest of the capture — the write's result settles the card.
        fold.take_approval(&request.request_id).expect("the ask is answerable");
        let mut rest = Vec::new();
        for line in &lines[asked_at..] {
            rest.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        assert!(
            rest.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
            )),
            "the write's result settles the decided card to allowed"
        );
        assert!(fold.pending_approvals().is_empty(), "nothing waits after the run");
        let write_done = rest
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated { block, .. } => Some(block),
                _ => None,
            })
            .chain(tool_updates(&rest).iter())
            .any(|block| matches!(
                block,
                Block::ToolCall {
                    kind: ToolKind::Write, status: ToolStatus::Success, diff_stat: Some(_), ..
                }
            ));
        assert!(write_done, "the allowed write completed with its chip");
    }

    /// `image.jsonl`: the echoed image part folds into the user turn as
    /// an image attachment, and the turn answers what it saw.
    #[test]
    fn image_fixture_accepts_image_part_on_user_turn() {
        let (_, deltas) = replay("image.jsonl");
        let users = user_turns(&deltas);
        assert_eq!(users.len(), 1);
        assert!(users[0].0.contains("three words"), "the prompt text: {}", users[0].0);
        let attachments: Vec<Attachment> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnStarted { turn: Turn::User { attachments, .. } } => {
                    Some(attachments.clone())
                }
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(attachments.len(), 1, "the attached PNG rides the turn");
        assert_eq!(attachments[0].name, "image-1");
        assert!(matches!(attachments[0].kind, AttachmentKind::Image));
        let rendered: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block, .. } => text_of(block),
                _ => None,
            })
            .collect();
        assert!(
            rendered.iter().any(|text| text.contains("Bright green square")),
            "the turn saw the image: {rendered:?}"
        );
    }

    /// THE AGREEMENT TEST. `partial.jsonl` carries both lanes (deltas plus
    /// `assistant` repeats); `basic.jsonl` carries only `assistant`. Folding
    /// `partial.jsonl` with its `stream_event` lines stripped must yield
    /// deltas identical to folding it whole — the ignored lane changes
    /// nothing — and `basic.jsonl` must fold to its complete message through
    /// the same lane. That is "agree modulo streaming granularity": the two
    /// lanes describe the same content, and the fold renders it once.
    #[test]
    fn partial_and_basic_agree_modulo_streaming_granularity() {
        let partial = fixture_lines("partial.jsonl");
        let (_, whole) = fold_lines(&partial);
        let stripped: Vec<String> = partial
            .iter()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|value| {
                        value.get("type").and_then(serde_json::Value::as_str).map(str::to_owned)
                    })
                    .as_deref()
                    != Some("stream_event")
            })
            .cloned()
            .collect();
        assert!(
            stripped.len() < partial.len(),
            "partial.jsonl must actually contain stream_event lines"
        );
        let (_, without_stream) = fold_lines(&stripped);
        assert_eq!(
            whole, without_stream,
            "the stream_event lane must not change the transcript"
        );

        let (_, basic) = fold_lines(&fixture_lines("basic.jsonl"));
        let texts: Vec<&str> = basic
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block, .. } => text_of(block),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["PROBE_OK"], "basic renders its whole message, no deltas needed");
        for delta in &basic {
            if let Delta::BlockAdded { block: Block::Text { streaming, .. }, .. } = delta {
                assert!(!streaming, "assistant-lane blocks are complete, never streaming");
            }
        }
    }

    /// NO DOUBLE RENDER. Every `assistant` frame's content appears exactly
    /// once in the folded transcript, although `stream_event` frames
    /// describe the same content a second time. Redacted (empty) thinking
    /// folds to no block at all, so those holes leave the count.
    #[test]
    fn partial_renders_each_assistant_message_exactly_once() {
        let partial = fixture_lines("partial.jsonl");
        let is_empty_thinking = |block: &serde_json::Value| {
            block.get("type").and_then(|kind| kind.as_str()) == Some("thinking")
                && block.get("thinking").and_then(|text| text.as_str()).is_some_and(|text| text.trim().is_empty())
        };
        let assistant_blocks: usize = partial
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
            })
            .map(|value| {
                value
                    .get("message")
                    .and_then(|message| message.get("content"))
                    .and_then(serde_json::Value::as_array)
                    .map(|content| content.iter().filter(|block| !is_empty_thinking(block)).count())
                    .unwrap_or(0)
            })
            .sum();
        let (_, deltas) = fold_lines(&partial);
        let added = deltas.iter().filter(|d| matches!(d, Delta::BlockAdded { .. })).count();
        assert_eq!(
            added, assistant_blocks,
            "one rendered block per assistant content block — no lane doubles it"
        );

        let dones = deltas
            .iter()
            .filter(|delta| match delta {
                Delta::BlockAdded { block: Block::Text { text, .. }, .. } => text == "DONE",
                _ => false,
            })
            .count();
        assert_eq!(dones, 1, "the DONE message renders exactly once");
        let bashes = deltas
            .iter()
            .filter(|delta| {
                matches!(
                    delta,
                    Delta::BlockAdded {
                        block: Block::ToolCall { kind: ToolKind::Shell, .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(bashes, 1, "the Bash call renders exactly once");
    }

    #[test]
    fn tool_results_complete_their_cards() {
        let (_, deltas) = fold_lines(&fixture_lines("partial.jsonl"));
        let completed = deltas
            .iter()
            .filter(|delta| {
                matches!(
                    delta,
                    Delta::BlockUpdated {
                        block: Block::ToolCall { status: ToolStatus::Success, .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(completed, 1, "the Bash result completes its card");
        let outputs: Vec<String> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated {
                    block:
                        Block::ToolCall {
                            body: ToolBody::Shell { output_lines, .. },
                            ..
                        },
                    ..
                } => Some(output_lines.join("\n")),
                _ => None,
            })
            .collect();
        assert_eq!(outputs, ["HELLO_FROM_TOOL"]);
    }

    #[test]
    fn mcp_tool_names_split_into_server_and_tool() {
        let (_, deltas) = fold_lines(&fixture_lines("mcp.jsonl"));
        let mut mcps = Vec::new();
        for delta in &deltas {
            if let Delta::BlockAdded {
                block: Block::ToolCall { kind: ToolKind::Mcp { server, tool }, .. },
                ..
            } = delta
            {
                mcps.push(format!("{server}::{tool}"));
            }
        }
        assert_eq!(mcps, ["baazprobe::baaz_ping"]);
        // ToolSearch is not a code search: it renders through the Generic
        // fallback, never as a richer card.
        let generics = deltas
            .iter()
            .filter(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Generic { kind, .. }, .. }
                if kind == "ToolSearch"
            ))
            .count();
        assert_eq!(generics, 1);
    }

    /// A `terminal_run` tool_use opens a shell card naming the command —
    /// never the raw `mcp__baaz__terminal_run` request — and its result
    /// completes it to "Ran in terminal · command · tab" with the run's
    /// output, exit and duration (D51).
    #[test]
    fn a_baaz_terminal_run_folds_to_a_shell_card_not_raw_json() {
        let session = "terminal-1";
        let lines = vec![
            serde_json::json!({
                "type": "assistant",
                "message": {
                    "model": "m", "id": "msg_1", "type": "message", "role": "assistant",
                    "content": [{
                        "type": "tool_use", "id": "toolu_term1",
                        "name": "mcp__baaz__terminal_run",
                        "input": {"command": "echo hi", "tab": "auto", "wait": "exit"},
                    }],
                },
                "parent_tool_use_id": null, "session_id": session,
                "uuid": "u-1", "timestamp": "2026-09-27T00:00:00.000Z", "request_id": "req_1",
            })
            .to_string(),
            serde_json::json!({
                "type": "user",
                "message": {
                    "role": "user",
                    "content": [{
                        "type": "tool_result", "tool_use_id": "toolu_term1",
                        "content": [{
                            "type": "text",
                            "text": "{\"tab\":\"t1\",\"block\":\"t1:0\",\"status\":\"exited\",\"exit_code\":0,\"duration_ms\":42,\"output\":\"hi\\n\",\"truncated_bytes\":0,\"cursor\":\"c:9\"}",
                        }],
                    }],
                },
                "parent_tool_use_id": null, "session_id": session,
                "uuid": "u-2", "timestamp": "2026-09-27T00:00:01.000Z",
            })
            .to_string(),
        ];
        let (_, deltas) = fold_lines(&lines);
        let opened = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::ToolCall { kind, verb, target, status, .. }, .. } => {
                    Some((kind.clone(), verb.clone(), target.clone(), *status))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            opened,
            [(
                ToolKind::Shell,
                TERMINAL_RUNNING_VERB.to_owned(),
                "echo hi".to_owned(),
                ToolStatus::Running
            )],
            "the terminal call opens as a running shell card naming the command"
        );
        let completed = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockUpdated { block: Block::ToolCall { verb, target, status, duration_ms, body, .. }, .. } => {
                    Some((verb.clone(), target.clone(), *status, *duration_ms, body.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(completed.len(), 1, "the result completes the card");
        let (verb, target, status, duration_ms, body) = &completed[0];
        assert_eq!(verb, TERMINAL_RAN_VERB);
        assert_eq!(target, "echo hi · t1", "the command plus the tab label");
        assert_eq!(*status, ToolStatus::Success);
        assert_eq!(*duration_ms, Some(42));
        match body {
            ToolBody::Shell { output_lines, exit_code, live } => {
                assert_eq!(output_lines, &["hi".to_owned()]);
                assert_eq!(*exit_code, Some(0));
                assert!(!live);
            }
            body => panic!("a shell body, not {body:?}"),
        }
    }

    /// The `terminal_run` approval asks "Run in terminal · `<command>`" —
    /// never "baaz · Terminal Run (mcp__baaz__terminal_run)" (D51).
    #[test]
    fn a_terminal_run_approval_names_the_command() {
        let lines = vec![
            serde_json::json!({
                "type": "control_request",
                "request_id": "req_ask_1",
                "request": {
                    "subtype": "can_use_tool",
                    "tool_name": "mcp__baaz__terminal_run",
                    "mcp_server": {"name": "baaz", "source": "dynamic"},
                    "display_name": "Terminal Run",
                    "input": {"command": "echo hi", "tab": "auto"},
                    "permission_suggestions": [],
                    "tool_use_id": "toolu_term1",
                },
            })
            .to_string(),
        ];
        let (fold, deltas) = fold_lines(&lines);
        assert_eq!(fold.pending_approvals().len(), 1, "the ask waits");
        let commands = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { block: Block::Approval { command, .. }, .. } => Some(command.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(commands, ["Run in terminal · `echo hi`".to_owned()]);
    }

    #[test]
    fn bidi_finishes_two_turns_with_costed_metas() {
        let (_, deltas) = fold_lines(&fixture_lines("bidi.jsonl"));
        let finished: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert_eq!(finished.len(), 2, "two result frames finish two turns");
        for meta in finished {
            assert!(meta.cost_usd > 0.0);
            assert!(meta.tokens_out > 0);
        }
    }

    #[test]
    fn pump_reader_is_the_shared_stream_path() {
        // The fixture tests must exercise the same code path the child
        // does: fold the file through `pump_reader` and compare against the
        // direct fold above.
        let path = format!("{}/../../fixtures/claude-code/partial.jsonl", env!("CARGO_MANIFEST_DIR"));
        let file = std::fs::File::open(path).expect("fixture reads");
        let mut fold = ClaudeFold::new();
        let mut events = Vec::new();
        pump_reader(std::io::BufReader::new(file), &mut fold, |event| events.push(event));
        let via_pump: Vec<Delta> = events
            .into_iter()
            .flat_map(|event| match event {
                ProviderEvent::Deltas { deltas, .. } => deltas,
                _ => Vec::new(),
            })
            .collect();
        let (_, direct) = fold_lines(&fixture_lines("partial.jsonl"));
        assert_eq!(via_pump, direct);
        // Every emitted event names the fixture's session.
        let file = std::fs::File::open(format!(
            "{}/../../fixtures/claude-code/partial.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture reads");
        let mut fold = ClaudeFold::new();
        pump_reader(std::io::BufReader::new(file), &mut fold, |event| {
            if let ProviderEvent::Deltas { session_id, .. } = event {
                assert_eq!(session_id.as_deref(), Some("96b540fe-c0a9-4347-9106-d1eb0e88ac49"));
            }
        });
    }

    fn permission_child_lines() -> Vec<String> {
        let path = format!(
            "{}/../../fixtures/claude-code/permission.jsonl",
            env!("CARGO_MANIFEST_DIR")
        );
        std::fs::read_to_string(path)
            .expect("fixture reads")
            .lines()
            .filter_map(|line| {
                let value: serde_json::Value =
                    serde_json::from_str(line).expect("fixture is JSON");
                // Host-sent envelope lines are answers, not child output.
                if value.get("_dir").is_some() { None } else { Some(line.to_owned()) }
            })
            .collect()
    }

    /// THE HANG TEST, fold half. Replaying the child's side of the fixture
    /// must leave the permission request pending with a card on the
    /// transcript: a reader that only folded `assistant`/`stream_event`
    /// would leave nothing pending, and the child would wait forever.
    /// Deciding moves the request off the answerable queue exactly once
    /// without moving the card; the PONG result then settles the card to
    /// allowed — decided never means resolved.
    #[test]
    fn permission_request_stays_pending_until_decided() {
        let lines = permission_child_lines();
        let asked_at = nth_control_request(&lines, 1).expect("the ask on the wire");
        let (mut fold, _) = fold_lines(&lines[..asked_at]);
        let request_id = {
            let pending = fold.pending_approvals();
            assert_eq!(pending.len(), 1, "the can_use_tool request must surface, not fold away");
            let request = &pending[0];
            assert_eq!(request.tool_name, "mcp__baaz__ping");
            assert_eq!(request.tool_use_id, "toolu_01CPKoR3sS6ZvHfqgQtJWZU5");
            request.request_id.clone()
        };
        // The card is on the transcript too: a pending approval, not prose.
        let (_, ask_deltas) = fold_lines(&lines[..asked_at]);
        let cards = ask_deltas
            .iter()
            .filter(|delta| {
                matches!(
                    delta,
                    Delta::BlockAdded {
                        block: Block::Approval { state: aui_protocol::ApprovalState::Pending, .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(cards, 1, "one pending approval card on the transcript");
        // Deciding takes it off the pending set exactly once — and moves
        // no card: the decision queues the answer line, nothing more.
        let taken = fold.take_approval(&request_id);
        assert!(taken.is_some());
        assert!(fold.pending_approvals().is_empty());
        assert!(fold.take_approval(&request_id).is_none());
        let mut settled = Vec::new();
        for line in &lines[asked_at..] {
            settled.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        assert!(
            settled.iter().any(|delta| matches!(
                delta,
                Delta::BlockUpdated { block: Block::Approval { state: ApprovalState::AllowedOnce { .. }, .. }, .. }
            )),
            "the PONG result settles the decided card to allowed: {settled:?}"
        );
        assert!(
            !settled.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Approval { .. }, .. }
            )),
            "settling updates the card, never cards twice"
        );
    }

    /// Decisions are explicit and typed: allow and deny render their wire
    /// lines, anything else is rejected with the offered choices named.
    #[test]
    fn decisions_are_explicit_allow_or_deny() {
        let lines = permission_child_lines();
        let asked_at = nth_control_request(&lines, 1).expect("the ask on the wire");
        let (fold, _) = fold_lines(&lines[..asked_at]);
        let request = fold.pending_approvals()[0].clone();
        let allow = decide_approval(&request, "allow", None).expect("allow decides");
        let written: serde_json::Value = serde_json::from_str(&allow).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("allow")
        );
        let deny = decide_approval(&request, "deny", Some("too risky")).expect("deny decides");
        let written: serde_json::Value = serde_json::from_str(&deny).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("message"))
                .and_then(serde_json::Value::as_str),
            Some("too risky")
        );
        let error = decide_approval(&request, "maybe", None).expect_err("no third choice");
        assert!(error.to_string().contains("allow"));
    }

    /// An unrecognised control subtype is recorded and carded — surfaced in
    /// the transcript, never dropped, never panicked on.
    #[test]
    fn unknown_control_subtype_is_surfaced_not_dropped() {
        let mut fold = ClaudeFold::new();
        let frame = decode_line(
            r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#,
        )
        .expect("decodes");
        let deltas = fold.apply(&frame);
        assert_eq!(fold.unknown_control(), &[("req-x".to_owned(), "frobnicate".to_owned())]);
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Generic { kind, .. }, .. }
                if kind.contains("frobnicate")
            )),
            "the unknown subtype must appear on the transcript: {deltas:?}"
        );
        // …and it never lands in the pending set: nothing known to decide.
        assert!(fold.pending_approvals().is_empty());
    }

    /// The permission turn's `result` frame finishes with the provider's
    /// real cost in the footer meta — never the muse `0.0` literal.
    #[test]
    fn permission_turn_finishes_with_real_cost() {
        let (_, deltas) = fold_lines(&permission_child_lines());
        let metas: Vec<&TurnMeta> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { meta, .. } => Some(meta),
                _ => None,
            })
            .collect();
        assert!(!metas.is_empty(), "the result frame finishes turns");
        let last = metas.last().expect("a finish");
        assert!(
            (last.cost_usd - 0.0188967).abs() < 1e-9,
            "real total_cost_usd in the meta: {}",
            last.cost_usd
        );
        // The PONG tool result still completes its MCP card.
        let pongs = deltas
            .iter()
            .filter(|delta| match delta {
                Delta::BlockUpdated {
                    block:
                        Block::ToolCall {
                            body: ToolBody::Mcp { result_json, .. },
                            status: ToolStatus::Success,
                            ..
                        },
                    ..
                } => result_json.contains("PONG"),
                _ => false,
            })
            .count();
        assert_eq!(pongs, 1, "the allowed tool's PONG completes its card");
    }

    /// A `result` carrying denials with no turn open still cards them: the
    /// card opens its own turn rather than vanishing. Pinned to the real
    /// deny capture — the refusal the owner actually saw — never a
    /// hand-written literal whose names could drift off the wire.
    #[test]
    fn result_denials_card_when_no_turn_open() {
        let result_line = fixture_lines("permission-deny.jsonl")
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| value.get("_dir").is_none())
            .find(|value| {
                value.get("type").and_then(serde_json::Value::as_str) == Some("result")
            })
            .expect("the deny fixture carries a child result frame")
            .to_string();
        let frame = decode_line(&result_line).expect("decodes");
        match &frame {
            crate::frame::Frame::TurnResult { permission_denials, total_cost_usd, .. } => {
                assert!(
                    (total_cost_usd - 0.0696295).abs() < 1e-9,
                    "real cost, not 0.0: {total_cost_usd}"
                );
                assert_eq!(
                    permission_denials,
                    &vec![crate::frame::PermissionDenial {
                        tool_name: "mcp__baaz__ping".into(),
                        tool_use_id: "toolu_01XqHeZeKksDmhf4miPM8P5C".into(),
                        tool_input: serde_json::json!({}),
                    }]
                );
            }
            other => panic!("result must decode, got {other:?}"),
        }
        // No turn open: a bare result, the path the old code dropped.
        let mut fold = ClaudeFold::new();
        let deltas = fold.apply(&frame);
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Generic { kind, status, .. }, .. }
                if kind == "permission-denial" && status == "denied"
            )),
            "the denial must card even with no turn open: {deltas:?}"
        );
        // No wire ids in user-facing text: the card names the refused
        // tool, never its `toolu_…` id.
        assert!(
            deltas.iter().any(|delta| match delta {
                Delta::BlockAdded {
                    block: Block::Generic { text, .. },
                    ..
                } => text.contains("mcp__baaz__ping") && !text.contains("toolu_"),
                _ => false,
            }),
            "the card names the refused tool, never its id: {deltas:?}"
        );
        assert!(
            deltas.iter().any(|delta| matches!(delta, Delta::TurnStarted { .. })),
            "a synthetic turn opens so the card is never dangling: {deltas:?}"
        );
        assert!(
            deltas.iter().any(|delta| matches!(delta, Delta::TurnFinished { .. })),
            "the synthetic turn still finishes with its costed meta: {deltas:?}"
        );
    }

    /// An unknown control subtype is answerable: it queues for a decision,
    /// the caller decides, a `control_response` line is minted, and the
    /// claim runs exactly once.
    #[test]
    fn unknown_control_request_can_be_answered() {
        let mut fold = ClaudeFold::new();
        let frame = decode_line(
            r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#,
        )
        .expect("decodes");
        let deltas = fold.apply(&frame);
        assert!(
            deltas.iter().any(|delta| matches!(
                delta,
                Delta::BlockAdded { block: Block::Generic { kind, .. }, .. }
                if kind.contains("frobnicate")
            )),
            "the unknown subtype must appear on the transcript: {deltas:?}"
        );
        assert_eq!(fold.pending_unknown().len(), 1, "the request must queue, not just card");
        assert_eq!(fold.pending_unknown()[0].request_id, "req-x");
        assert_eq!(fold.pending_unknown()[0].subtype, "frobnicate");
        let request = fold.take_unknown("req-x").expect("a received request is answerable");
        let line =
            decide_unknown_approval(&request, "deny", Some("no such tool")).expect("refusal mints");
        let written: serde_json::Value = serde_json::from_str(&line).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(serde_json::Value::as_str),
            Some("req-x")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("deny")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("message"))
                .and_then(serde_json::Value::as_str),
            Some("no such tool")
        );
        assert!(fold.take_unknown("req-x").is_none(), "claimed exactly once");
        assert!(fold.pending_unknown().is_empty());
        let error = decide_unknown_approval(&request, "maybe", None).expect_err("no third choice");
        assert!(error.to_string().contains("allow"));
    }

    /// W8 helpers: the assistant turns a replay opened, and how each one
    /// finished.
    fn assistant_ids(deltas: &[Delta]) -> Vec<&str> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnStarted { turn: Turn::Assistant { id, .. } } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    fn finishes(deltas: &[Delta]) -> Vec<(&str, &TurnMeta)> {
        deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::TurnFinished { turn_id, meta } => Some((turn_id.as_str(), meta)),
                _ => None,
            })
            .collect()
    }

    /// The first `assistant` message id in a fixture: the turn id a
    /// single-turn fold must open, live and on replay alike.
    fn first_message_id(lines: &[String]) -> String {
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("type").and_then(serde_json::Value::as_str) == Some("assistant")
            })
            .filter_map(|value| {
                value
                    .get("message")
                    .and_then(|message| message.get("id"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .next()
            .expect("the fixture carries an assistant message")
    }

    /// The `result` frames in a fixture, in order, as the totals the fold
    /// must report exactly once per turn: `(tokens_in, tokens_out,
    /// reasoning, cost, model, duration_ms)`.
    fn result_totals(lines: &[String]) -> Vec<(u64, u64, u64, f64, String, u64)> {
        lines
            .iter()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|value| {
                value.get("type").and_then(serde_json::Value::as_str) == Some("result")
            })
            .map(|value| {
                let usage = value.get("usage").cloned().unwrap_or(serde_json::Value::Null);
                let uint = |key: &str| usage.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
                let tokens_in = uint("input_tokens")
                    + uint("cache_read_input_tokens")
                    + uint("cache_creation_input_tokens");
                let reasoning = usage
                    .get("output_tokens_details")
                    .and_then(|details| details.get("thinking_tokens"))
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let model = value
                    .get("modelUsage")
                    .and_then(serde_json::Value::as_object)
                    .and_then(|usage| usage.keys().next().cloned())
                    .unwrap_or_default();
                (
                    tokens_in,
                    uint("output_tokens"),
                    reasoning,
                    value.get("total_cost_usd").and_then(serde_json::Value::as_f64).unwrap_or(0.0),
                    model,
                    value.get("duration_ms").and_then(serde_json::Value::as_u64).unwrap_or(0),
                )
            })
            .collect()
    }

    /// W8, defect 1: one user turn is one assistant turn. Each
    /// multi-message fixture opens exactly one assistant turn — keyed by
    /// its first message — and finishes exactly that turn. Folding one
    /// turn per message (the old arms) opens three on `edit.jsonl`.
    #[test]
    fn one_user_turn_is_one_assistant_turn() {
        for name in ["edit.jsonl", "approval-default.jsonl", "todo.jsonl", "subagent.jsonl"] {
            let lines = fixture_lines(name);
            let (_, deltas) = fold_lines(&lines);
            let starts = assistant_ids(&deltas);
            let done = finishes(&deltas);
            assert_eq!(starts.len(), 1, "{name}: one assistant turn opens");
            assert_eq!(done.len(), 1, "{name}: one assistant turn finishes");
            assert_eq!(starts[0], done[0].0, "{name}: the finish closes the open turn");
            assert_eq!(
                starts[0],
                first_message_id(&lines),
                "{name}: the turn id is the first message, live and on replay"
            );
        }
    }

    /// W8, defect 2: the turn's totals land once. Each finished meta equals
    /// its `result` frame's usage — never copied into several turns, never
    /// summed across messages.
    #[test]
    fn turn_totals_equal_the_result_frame_once() {
        for name in ["edit.jsonl", "approval-default.jsonl", "todo.jsonl", "subagent.jsonl"] {
            let lines = fixture_lines(name);
            let (_, deltas) = fold_lines(&lines);
            let done = finishes(&deltas);
            let want = result_totals(&lines);
            assert_eq!(want.len(), 1, "{name}: one result frame");
            assert_eq!(done.len(), 1, "{name}: one finish carries it");
            let meta = done[0].1;
            assert_eq!(meta.tokens_in, want[0].0, "{name}: whole billed prompt, once");
            assert_eq!(meta.tokens_out, want[0].1, "{name}: completions, once");
            assert_eq!(meta.reasoning_tokens, want[0].2, "{name}: reasoning, once");
            assert!(
                (meta.cost_usd - want[0].3).abs() < 1e-9,
                "{name}: real cost {}",
                meta.cost_usd
            );
            assert_eq!(meta.model, want[0].4, "{name}: result model");
            assert_eq!(meta.duration_ms, want[0].5, "{name}: wall clock");
        }
    }

    /// W8, defect 3 (fold half): history replays under the live turn's id.
    /// Folding a turn's user+assistant lines and closing with
    /// `finish_stored_turns` finishes the same id the live fold (with the
    /// `result` frame) finished — so the ledger's no-op-on-replay insert
    /// sees one key, not two.
    #[test]
    fn history_replay_reuses_the_live_turn_id() {
        let lines = fixture_lines("edit.jsonl");
        let (_, live) = fold_lines(&lines);
        let live_done = finishes(&live);
        assert_eq!(live_done.len(), 1);
        let stored: Vec<String> = lines
            .iter()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .ok()
                    .and_then(|value| {
                        value.get("type").and_then(serde_json::Value::as_str).map(str::to_owned)
                    })
                    .as_deref()
                    != Some("result")
            })
            .cloned()
            .collect();
        let mut fold = ClaudeFold::new();
        let mut replayed = Vec::new();
        for line in &stored {
            replayed.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        replayed.extend(fold.finish_stored_turns());
        let replay_done = finishes(&replayed);
        assert_eq!(replay_done.len(), 1, "the replayed turn closes once");
        assert_eq!(
            replay_done[0].0, live_done[0].0,
            "replay reuses the live turn id or the ledger doubles"
        );
    }

    /// W8, stored-history boundary: a fresh prompt echo closes the still
    /// open turn, so replaying two turns without `result` frames still
    /// folds to two turns. Without the echo close the second turn's blocks
    /// join the first and only `finish_stored_turns` ever finishes.
    #[test]
    fn a_new_prompt_closes_the_previous_turn() {
        fn echo(uuid: &str, text: &str) -> String {
            serde_json::json!({
                "type": "user",
                "message": {"role": "user", "content": [{"type": "text", "text": text}]},
                "session_id": "s",
                "uuid": uuid,
            })
            .to_string()
        }
        fn reply(message: &str, text: &str) -> String {
            serde_json::json!({
                "type": "assistant",
                "message": {"id": message, "content": [{"type": "text", "text": text}]},
                "session_id": "s",
                "uuid": format!("u-{message}"),
                "parent_tool_use_id": null,
            })
            .to_string()
        }
        let mut fold = ClaudeFold::new();
        let mut deltas = Vec::new();
        for line in [echo("u-1", "first"), reply("msg-1", "one"), echo("u-2", "second"), reply("msg-2", "two")] {
            deltas.extend(fold.apply(&decode_line(&line).expect("decodes")));
        }
        // The second echo already closed the first turn, before any
        // `finish_stored_turns`.
        let done = finishes(&deltas);
        assert_eq!(done.len(), 1, "the echo closes the previous turn: {deltas:?}");
        assert_eq!(done[0].0, "msg-1");
        let echo_at = deltas
            .iter()
            .position(|delta| matches!(
                delta,
                Delta::TurnStarted { turn: Turn::User { id, .. } } if id == "u-2"
            ))
            .expect("the second bubble");
        let finish_at = deltas
            .iter()
            .position(|delta| matches!(delta, Delta::TurnFinished { .. }))
            .expect("the close");
        assert!(finish_at < echo_at, "close lands before the next bubble");
        deltas.extend(fold.finish_stored_turns());
        let done = finishes(&deltas);
        assert_eq!(done.len(), 2, "both turns close");
        assert_ne!(done[0].0, done[1].0, "distinct turns, distinct ids");
    }

    /// W8, three turns live (`resume-replay.jsonl`): three echoes, three
    /// assistant turns, three finishes — each finish carrying its own
    /// `result` frame's totals, in order.
    #[test]
    fn three_live_turns_finish_three_times_with_own_totals() {
        let lines = fixture_lines("resume-replay.jsonl");
        let (_, deltas) = fold_lines(&lines);
        assert_eq!(user_turns(&deltas).len(), 3, "three prompts bubble");
        let starts = assistant_ids(&deltas);
        assert_eq!(starts.len(), 3, "three assistant turns open");
        let done = finishes(&deltas);
        assert_eq!(done.len(), 3, "three assistant turns finish");
        for (start, (id, _)) in starts.iter().zip(done.iter()) {
            assert_eq!(start, id, "each finish closes its own turn");
        }
        let want = result_totals(&lines);
        assert_eq!(want.len(), 3, "three result frames");
        for ((_, meta), total) in done.iter().zip(want.iter()) {
            assert_eq!(meta.tokens_in, total.0, "each turn billed once");
            assert_eq!(meta.tokens_out, total.1);
            assert!((meta.cost_usd - total.3).abs() < 1e-9);
        }
    }

    /// W8, defect 5 (structural half): a turn whose thinking rendered keeps
    /// its trace and its count on the same turn — the condition that
    /// selects the library's plain `N reasoning` cell over the silent one.
    /// (Redacted empty thinking still folds to nothing; see
    /// `thinking_fixture_skips_empty_thinking_but_counts_reasoning`.)
    #[test]
    fn visible_thinking_and_reasoning_share_one_turn() {
        let thinking = serde_json::json!({
            "type": "assistant",
            "message": {"id": "msg-1", "content": [
                {"type": "thinking", "thinking": "let me think"},
                {"type": "tool_use", "id": "toolu_1", "name": "Bash", "input": {"command": "true"}},
            ]},
            "session_id": "s",
            "uuid": "u-1",
            "parent_tool_use_id": null,
        })
        .to_string();
        let answered = serde_json::json!({
            "type": "user",
            "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"},
            ]},
            "session_id": "s",
            "uuid": "u-2",
            "parent_tool_use_id": null,
        })
        .to_string();
        let said = serde_json::json!({
            "type": "assistant",
            "message": {"id": "msg-2", "content": [{"type": "text", "text": "DONE"}]},
            "session_id": "s",
            "uuid": "u-3",
            "parent_tool_use_id": null,
        })
        .to_string();
        let end = serde_json::json!({
            "type": "result",
            "session_id": "s",
            "result": "DONE",
            "usage": {"input_tokens": 5, "output_tokens": 7,
                      "output_tokens_details": {"thinking_tokens": 87}},
            "total_cost_usd": 0.01,
            "duration_ms": 1000,
            "modelUsage": {"m": {}},
        })
        .to_string();
        let mut fold = ClaudeFold::new();
        let mut deltas = Vec::new();
        for line in [&thinking, &answered, &said, &end] {
            deltas.extend(fold.apply(&decode_line(line).expect("decodes")));
        }
        let starts = assistant_ids(&deltas);
        assert_eq!(starts, ["msg-1"], "one turn across both messages");
        let thinking_on_turn: Vec<&str> = deltas
            .iter()
            .filter_map(|delta| match delta {
                Delta::BlockAdded { turn_id, block: Block::Thinking { .. } } => Some(turn_id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(thinking_on_turn, ["msg-1"], "the trace renders on the turn");
        let done = finishes(&deltas);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].0, "msg-1");
        assert_eq!(done[0].1.reasoning_tokens, 87, "the count lands on the same turn");
    }

    #[test]
    fn a_skill_call_folds_to_the_quiet_row() {
        // Claude Code's `Skill` tool invokes a skill by name under `skill`:
        // it folds to the same quiet row `read_skill` folds to (D63).
        let card = tool_card("t-1", "Skill", &serde_json::json!({"skill": "decoction"}));
        assert_eq!(card.kind, ToolKind::Read);
        assert_eq!(card.verb, "Loaded skill");
        assert_eq!(card.target, "decoction");
        match card.block {
            Block::ToolCall { kind: ToolKind::Read, verb, target, .. } => {
                assert_eq!(verb, "Loaded skill");
                assert_eq!(target, "decoction");
            }
            block => panic!("a Skill call folded to {block:?}"),
        }
        // Without a skill name the card names the tool, never a guess.
        let card = tool_card("t-2", "Skill", &serde_json::json!({}));
        assert_eq!(card.target, "Skill");
    }
}
