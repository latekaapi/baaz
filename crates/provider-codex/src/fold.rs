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
    Attachment, AttachmentKind, Block, Delta, Diff, DiffKind, DiffLine, DiffStat, Hunk,
    ThinkingState, TodoItem, TodoState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta,
    UploadState,
};
use provider::ProviderEvent;
use serde_json::Value;

use crate::frame::{FileChangeEntry, Frame, Notification, TokenCounts};

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
/// beside the seam's account shape.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSnapshot {
    /// Primary window usage percent, when reported.
    pub used_percent: Option<u64>,
    /// Plan name (`prolite`, …), when reported.
    pub plan: Option<String>,
    /// Unix time the window resets, when reported.
    pub resets_at: Option<u64>,
}

impl AccountSnapshot {
    /// Record a push. Unknown fields are ignored; a push that names no
    /// window at all still counts as "a reading was seen".
    pub fn observe(&mut self, params: &Value) {
        let limits = params.get("rateLimits");
        let primary = limits.and_then(|limits| limits.get("primary"));
        if let Some(used) =
            primary.and_then(|primary| primary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.used_percent = Some(used);
        }
        if let Some(plan) = limits
            .and_then(|limits| limits.get("planType"))
            .and_then(Value::as_str)
        {
            self.plan = Some(plan.to_owned());
        }
        if let Some(resets) = primary
            .and_then(|primary| primary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.resets_at = Some(resets);
        }
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
    usage: HashMap<(String, String), TokenCounts>,
    account: AccountSnapshot,
    current_turn: Option<String>,
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

    /// The per-turn figure for `(thread_id, turn_id)`: the `last` bucket,
    /// never the cumulative `total`.
    pub fn usage_for(&self, thread_id: &str, turn_id: &str) -> Option<&TokenCounts> {
        self.usage.get(&(thread_id.to_owned(), turn_id.to_owned()))
    }

    /// The latest account reading (see [`AccountSnapshot`]).
    pub fn account(&self) -> &AccountSnapshot {
        &self.account
    }

    /// Fold one decoded frame into render-ready deltas. Only notifications
    /// render: requests and responses are the pump's routing business (see
    /// [`crate::child`]), and contribute nothing here.
    pub fn apply(&mut self, frame: &Frame) -> Vec<Delta> {
        match frame {
            Frame::Notification(notification) => self.apply_notification(notification),
            Frame::Request { .. } | Frame::Response { .. } | Frame::ResponseError { .. } => {
                Vec::new()
            }
        }
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
                Vec::new()
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
                if self.user_started.insert(item.id().to_owned()) {
                    deltas.push(Delta::TurnStarted {
                        turn: Turn::User {
                            id: item.id().to_owned(),
                            text: item.text().to_owned(),
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
                let exit_code = item.exit_code().and_then(|code| i32::try_from(code).ok());
                self.push_block(
                    turn_id,
                    Block::ToolCall {
                        id: item.id().to_owned(),
                        kind: ToolKind::Shell,
                        verb: "Ran".into(),
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
                let (kind, verb) = if is_write {
                    (ToolKind::Write, "Wrote")
                } else {
                    (ToolKind::Edit, "Edited")
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
                self.push_block(
                    turn_id,
                    Block::ToolCall {
                        id: item.id().to_owned(),
                        kind: ToolKind::SubAgent,
                        verb: "Delegated".into(),
                        target: item.agent_path().to_owned(),
                        status,
                        duration_ms: None,
                        body: ToolBody::SubAgent { turns: Vec::new() },
                        diff_stat: None,
                    },
                    &mut deltas,
                );
            }
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
/// Only notifications reach the fold here — the pump routes requests and
/// responses before this runs. The per-line unit of the live pump, factored
/// out so the fixture tests exercise the same decode-and-fold.
pub fn step_line(fold: &mut CodexFold, line: &str, emit: &mut impl FnMut(ProviderEvent)) {
    if line.trim().is_empty() {
        return;
    }
    let Ok(frame) = crate::frame::decode_line(line) else { return };
    if !matches!(frame, Frame::Notification(_)) {
        return;
    }
    let session_id = fold.session_id().map(str::to_owned).or_else(|| {
        if let Frame::Notification(notification) = &frame {
            notification.thread_id().map(str::to_owned)
        } else {
            None
        }
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
