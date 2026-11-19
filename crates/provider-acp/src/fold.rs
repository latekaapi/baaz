//! ACP's session updates, folded into the transcript through the shared
//! builder (docs/82 5.3, 5.7).
//!
//! An ACP agent names no turns: a turn is one `session/prompt`, from the
//! request to its answer. The adapter mints the turn id when it sends the
//! prompt ([`AcpFold::submitted`]) and every `session/update` until the
//! answer lands in that turn. A `session/load` replays a stored session as
//! the same updates with the person's own words among them
//! (`user_message_chunk`); while one plays ([`AcpFold::begin_history`]) the
//! fold cuts the stream into turns at each run of the person's words.
//!
//! Everything else is the builder's: the agent's text and thoughts stream
//! into one block each until something else speaks, a tool call is one card
//! filed under its `toolCallId` and refaced by every update, the plan is one
//! todo card per turn rewritten whole by every `plan` update, and a
//! permission request is an approval card in the one vocabulary (docs/82
//! 5.5).

use std::collections::HashMap;
use std::time::Instant;

use aui_protocol::{
    ApprovalBodyKind, ApprovalState, Block, Delta, Diff, Hunk, Provider, ThinkingState, TodoItem, TodoState, ToolBody,
    ToolKind, ToolStatus, Turn, TurnMeta,
};
use provider::facts::{ContextReading, ProviderSkill, SessionSide, TurnOutcome};
use provider::{ApprovalAnswer, ApprovalOption, ProviderEvent};
use serde_json::Value;
use transcript_core::builder::{approval_key, tool_key};
use transcript_core::failure::{self, FailureKind};
use transcript_core::{wall_ms_since, ToolEvent, TranscriptBuilder};

use crate::wire::{OptionKind, PermissionOption, StopReason};

/// The mark an ACP session's transcript header names: the icon set's
/// neutral block. No agent's own mark fits every ACP agent, and none is
/// worn by another registered provider (docs/82 5.7).
pub const MARK: Provider = Provider::Cursor;

/// Why a turn ended, as the adapter learned it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ending {
    /// The prompt's answer, with its stop reason.
    Stopped(StopReason),
    /// The prompt's error answer: the JSON-RPC code and the agent's words.
    Failed {
        /// The JSON-RPC error code.
        code: i64,
        /// The agent's words.
        message: String,
    },
}

/// One tool call as the updates so far describe it: every field the wire
/// sent, merged.
#[derive(Clone, Debug)]
struct Tool {
    title: String,
    kind: String,
    status: String,
    content: Vec<Value>,
    locations: Vec<Value>,
    raw_input: Option<Value>,
    raw_output: Option<Value>,
    started: Instant,
}

impl Tool {
    fn new() -> Self {
        Self {
            title: String::new(),
            kind: String::new(),
            status: "pending".into(),
            content: Vec::new(),
            locations: Vec::new(),
            raw_input: None,
            raw_output: None,
            started: Instant::now(),
        }
    }

    /// Merge one `tool_call` / `tool_call_update` (or a permission
    /// request's `toolCall`): a field the update carries replaces it, an
    /// absent one keeps it.
    fn merge(&mut self, update: &Value) {
        if let Some(title) = update.get("title").and_then(Value::as_str) {
            self.title = title.to_owned();
        }
        if let Some(kind) = update.get("kind").and_then(Value::as_str) {
            self.kind = kind.to_owned();
        }
        if let Some(status) = update.get("status").and_then(Value::as_str) {
            self.status = status.to_owned();
        }
        if let Some(content) = update.get("content").and_then(Value::as_array) {
            self.content = content.clone();
        }
        if let Some(locations) = update.get("locations").and_then(Value::as_array) {
            self.locations = locations.clone();
        }
        if let Some(input) = update.get("rawInput").filter(|input| !input.is_null()) {
            self.raw_input = Some(input.clone());
        }
        if let Some(output) = update.get("rawOutput").filter(|output| !output.is_null()) {
            self.raw_output = Some(output.clone());
        }
    }

    fn status(&self) -> ToolStatus {
        match self.status.as_str() {
            "pending" => ToolStatus::Pending,
            "completed" => ToolStatus::Success,
            "failed" => ToolStatus::Error,
            _ => ToolStatus::Running,
        }
    }

    fn finished(&self) -> bool {
        matches!(self.status(), ToolStatus::Success | ToolStatus::Error)
    }

    /// The text its content blocks carry, as lines.
    fn output_lines(&self) -> Vec<String> {
        self.content
            .iter()
            .filter(|item| item.get("type").and_then(Value::as_str) == Some("content"))
            .filter_map(|item| item.get("content")?.get("text")?.as_str())
            .flat_map(transcript_core::tool::split_lines)
            .collect()
    }

    /// The first file change its content carries: `(path, old, new)`.
    fn diff(&self) -> Option<(String, Option<String>, String)> {
        self.content.iter().find(|item| item.get("type").and_then(Value::as_str) == Some("diff")).map(|item| {
            (
                item.get("path").and_then(Value::as_str).unwrap_or_default().to_owned(),
                item.get("oldText").and_then(Value::as_str).map(str::to_owned),
                item.get("newText").and_then(Value::as_str).unwrap_or_default().to_owned(),
            )
        })
    }

    /// The command an `execute` call runs, as its raw input spells it.
    fn command(&self) -> Option<String> {
        let command = self.raw_input.as_ref()?.get("command")?;
        match command {
            Value::String(line) => Some(line.clone()),
            Value::Array(words) => Some(words.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
            _ => None,
        }
    }

    /// The first file it touches, relative to `cwd` when under it.
    fn path(&self, cwd: &str) -> Option<String> {
        let path = self
            .locations
            .first()
            .and_then(|location| location.get("path"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.diff().map(|(path, _, _)| path))
            .filter(|path| !path.is_empty())?;
        Some(relative(&path, cwd))
    }

    /// The card's noun for the approval's definition list.
    fn noun(&self) -> &'static str {
        match self.kind.as_str() {
            "read" => "Read",
            "edit" => "Edit",
            "delete" => "Delete",
            "move" => "Move",
            "search" => "Search",
            "execute" => "Shell",
            "fetch" => "Fetch",
            "think" => "Think",
            _ => "Tool",
        }
    }

    /// The card, as [`ToolEvent`] presents it.
    fn event(&self, id: &str, cwd: &str) -> ToolEvent {
        let status = self.status();
        let title = if self.title.trim().is_empty() { self.noun().to_owned() } else { self.title.clone() };
        let target = |fallback: &str| self.path(cwd).unwrap_or_else(|| fallback.to_owned());
        let event = match self.kind.as_str() {
            "read" => {
                let output_lines = self.output_lines();
                ToolEvent::new(id, ToolKind::Read, target(&title), status)
                    .body(ToolBody::Read { lines: output_lines.len(), output_lines })
            }
            "edit" | "delete" | "move" => match self.diff() {
                Some((path, old, new)) => {
                    let path = relative(&path, cwd);
                    let rows = match &old {
                        Some(old) => changed_rows(old, &new),
                        None => transcript_core::diff::addition_lines(&new),
                    };
                    let (added, removed) = transcript_core::diff::counts(&rows);
                    let kind = if old.is_none() { ToolKind::Write } else { ToolKind::Edit };
                    let stat = transcript_core::diff::stat([rows.as_slice()], 1);
                    let diff = Diff {
                        path: path.clone(),
                        hunks: vec![Hunk { header: String::new(), lines: rows }],
                        added: u32::try_from(added).unwrap_or(u32::MAX),
                        removed: u32::try_from(removed).unwrap_or(u32::MAX),
                    };
                    ToolEvent::new(id, kind, path, status).body(ToolBody::Edit { diff }).diff_stat(Some(stat))
                }
                None => ToolEvent::new(id, ToolKind::Edit, target(&title), status),
            },
            "search" => ToolEvent::new(id, ToolKind::Search, title, status),
            "fetch" => ToolEvent::new(id, ToolKind::Web, title, status),
            "execute" => ToolEvent::new(id, ToolKind::Shell, self.command().unwrap_or(title), status).body(ToolBody::Shell {
                output_lines: self.output_lines(),
                exit_code: self.exit_code(),
                live: status == ToolStatus::Running,
            }),
            _ => ToolEvent::new(id, ToolKind::Mcp { server: String::new(), tool: title.clone() }, title, status).body(
                ToolBody::Mcp {
                    params: params_of(self.raw_input.as_ref()),
                    result_json: self
                        .raw_output
                        .as_ref()
                        .map(|output| serde_json::to_string_pretty(output).unwrap_or_default())
                        .unwrap_or_default(),
                },
            ),
        };
        let duration = self.finished().then(|| wall_ms_since(self.started));
        event.duration(duration)
    }

    /// A shell call's exit code, when its raw output names one.
    fn exit_code(&self) -> Option<i32> {
        let output = self.raw_output.as_ref()?;
        let code = output.get("exitCode").or_else(|| output.get("exit_code"))?.as_i64()?;
        i32::try_from(code).ok()
    }
}

/// A path relative to `cwd` when it lies under it.
fn relative(path: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    match path.strip_prefix(cwd).and_then(|rest| rest.strip_prefix('/')) {
        Some(rest) if !cwd.is_empty() && !rest.is_empty() => rest.to_owned(),
        _ => path.to_owned(),
    }
}

/// `old` → `new` as display rows: the unchanged head and tail as context,
/// the middle as one deletion and one addition run. An ACP diff carries
/// whole texts, not hunks; this is the honest minimum without a diff
/// library.
fn changed_rows(old: &str, new: &str) -> Vec<aui_protocol::DiffLine> {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    let head = old_lines.iter().zip(&new_lines).take_while(|(a, b)| a == b).count();
    let tail = old_lines[head..].iter().rev().zip(new_lines[head..].iter().rev()).take_while(|(a, b)| a == b).count();
    let mut lines: Vec<String> = Vec::new();
    lines.extend(old_lines[..head].iter().map(|line| format!(" {line}")));
    lines.extend(old_lines[head..old_lines.len() - tail].iter().map(|line| format!("-{line}")));
    lines.extend(new_lines[head..new_lines.len() - tail].iter().map(|line| format!("+{line}")));
    lines.extend(old_lines[old_lines.len() - tail..].iter().map(|line| format!(" {line}")));
    transcript_core::diff::hunk_lines(lines.iter().map(String::as_str), 1, 1, true)
}

/// An object's fields as the MCP card's definition list.
fn params_of(input: Option<&Value>) -> Vec<(String, String)> {
    let Some(Value::Object(fields)) = input else { return Vec::new() };
    fields
        .iter()
        .map(|(name, value)| {
            let shown = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (name.clone(), shown)
        })
        .collect()
}

/// The assistant turn updates land in.
#[derive(Clone, Debug)]
struct Open {
    turn: String,
    /// A live prompt's turn (else a stored session's, replayed by a load).
    live: bool,
    started: Instant,
    /// The streaming text block's key, while the agent is still writing it.
    text: Option<String>,
    /// The streaming thinking block's key, and when it began.
    thought: Option<(String, Instant)>,
    /// Blocks keyed so far: names the next text or thought block.
    keyed: u64,
}

/// One permission request: the card it raised and what it offered.
#[derive(Clone, Debug)]
struct Ask {
    tool: String,
    options: Vec<PermissionOption>,
    raised: Instant,
}

/// One session's updates, folded.
#[derive(Clone, Debug)]
pub struct AcpFold {
    builder: TranscriptBuilder,
    session_id: String,
    cwd: String,
    open: Option<Open>,
    /// A load is replaying a stored session.
    history: bool,
    /// The person's words a load streamed, not yet a turn.
    ask: Option<String>,
    /// Turns a load replayed: names the next.
    replayed: u64,
    tools: HashMap<String, Tool>,
    /// Permission requests by approval id, until their call finishes.
    asks: HashMap<String, Ask>,
}

impl AcpFold {
    /// A fresh fold for session `session_id` running in `cwd`.
    pub fn new(session_id: &str, cwd: &str) -> Self {
        Self {
            builder: TranscriptBuilder::with_session(aui_protocol::Session::new(session_id, MARK, "", cwd)),
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            open: None,
            history: false,
            ask: None,
            replayed: 0,
            tools: HashMap::new(),
            asks: HashMap::new(),
        }
    }

    /// The session the events name.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The deltas folded since the last take.
    pub fn take(&mut self) -> Vec<Delta> {
        self.builder.take()
    }

    /// The live turn, while one runs.
    pub fn running_turn(&self) -> Option<&str> {
        self.open.as_ref().filter(|open| open.live).map(|open| open.turn.as_str())
    }

    /// The transcript folded so far.
    pub fn session(&self) -> &aui_protocol::Session {
        self.builder.session()
    }

    // ------------------------------------------------------------ turns

    /// A prompt went out as `turn_id`: the person's bubble with `words`,
    /// then the agent's turn, open. `timestamp` is the send time (Unix
    /// ms), `None` on a replay.
    pub fn submitted(&mut self, turn_id: &str, words: &str, timestamp: Option<u64>) {
        self.end_history();
        self.builder.push(Delta::TurnStarted {
            turn: Turn::User {
                id: format!("{turn_id}:ask"),
                text: words.to_owned(),
                attachments: Vec::new(),
                mentions: Vec::new(),
                timestamp,
            },
        });
        self.builder.open_turn(turn_id, timestamp);
        self.open = Some(Open { turn: turn_id.to_owned(), live: true, started: Instant::now(), text: None, thought: None, keyed: 0 });
    }

    /// A load starts replaying the stored session.
    pub fn begin_history(&mut self) {
        self.history = true;
    }

    /// The load answered: its last turn closes.
    pub fn end_history(&mut self) {
        if !self.history {
            return;
        }
        self.history = false;
        self.flush_ask();
        if self.open.as_ref().is_some_and(|open| !open.live) {
            self.close_open(TurnMeta::default());
        }
    }

    /// The turn the next update lands in: the live one, or — while a load
    /// replays — the stored turn the person's last words opened.
    fn turn(&mut self) -> Option<String> {
        if self.history {
            self.flush_ask();
            if self.open.is_none() {
                self.replayed += 1;
                let turn = format!("{}:stored-{}", self.session_id, self.replayed);
                self.builder.open_turn(&turn, None);
                self.open = Some(Open { turn: turn.clone(), live: false, started: Instant::now(), text: None, thought: None, keyed: 0 });
            }
        }
        self.open.as_ref().map(|open| open.turn.clone())
    }

    /// The person's words a load streamed become their bubble, after the
    /// stored turn before them closes.
    fn flush_ask(&mut self) {
        let Some(words) = self.ask.take() else { return };
        self.replayed += 1;
        self.builder.push(Delta::TurnStarted {
            turn: Turn::User {
                id: format!("{}:asked-{}", self.session_id, self.replayed),
                text: words,
                attachments: Vec::new(),
                mentions: Vec::new(),
                timestamp: None,
            },
        });
    }

    /// Close the open turn: its streaming blocks settle, and it finishes
    /// with `meta`.
    fn close_open(&mut self, meta: TurnMeta) -> Option<String> {
        self.settle_streams();
        let open = self.open.take()?;
        self.builder.finish_turn(&open.turn, meta);
        Some(open.turn)
    }

    /// The text and thought blocks stop streaming.
    fn settle_streams(&mut self) {
        let Some(open) = self.open.as_mut() else { return };
        let text = open.text.take();
        let thought = open.thought.take();
        if let Some(key) = text {
            self.builder.modify(&key, |block| {
                if let Block::Text { streaming, .. } = block {
                    *streaming = false;
                }
            });
        }
        if let Some((key, began)) = thought {
            self.builder.modify(&key, |block| {
                if let Block::Thinking { state, elapsed_ms, .. } = block {
                    *state = ThinkingState::Done;
                    *elapsed_ms = wall_ms_since(began);
                }
            });
        }
    }

    // ---------------------------------------------------------- updates

    /// One `session/update`'s `update`. Facts beside the transcript (the
    /// agent's commands, its context reading) land in `events`.
    pub fn update(&mut self, update: &Value, events: &mut Vec<ProviderEvent>) {
        match update.get("sessionUpdate").and_then(Value::as_str).unwrap_or_default() {
            "user_message_chunk" => {
                // Live, the bubble is already up from the prompt; a load
                // streams the person's words, which open the next turn.
                if !self.history {
                    return;
                }
                if self.open.is_some() {
                    self.close_open(TurnMeta::default());
                }
                let chunk = chunk_text(update);
                self.ask.get_or_insert_with(String::new).push_str(&chunk);
            }
            "agent_message_chunk" => self.text(&chunk_text(update)),
            "agent_thought_chunk" => self.thought(&chunk_text(update)),
            "tool_call" | "tool_call_update" => {
                let Some(id) = update.get("toolCallId").and_then(Value::as_str) else { return };
                self.tool(id, update);
            }
            "plan" => self.plan(update),
            "available_commands_update" => events.push(ProviderEvent::SkillsListed {
                session_id: self.session_id.clone(),
                skills: update
                    .get("availableCommands")
                    .and_then(Value::as_array)
                    .map(|commands| {
                        commands
                            .iter()
                            .filter_map(|command| {
                                Some(ProviderSkill {
                                    name: command.get("name")?.as_str()?.to_owned(),
                                    description: command
                                        .get("description")
                                        .and_then(Value::as_str)
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            }),
            "usage_update" => {
                let (Some(used), Some(size)) =
                    (update.get("used").and_then(Value::as_u64), update.get("size").and_then(Value::as_u64))
                else {
                    return;
                };
                events.push(ProviderEvent::SessionFacts {
                    session_id: self.session_id.clone(),
                    side: SessionSide { context: Some(ContextReading::measured(used, Some(size))), ..SessionSide::default() },
                });
            }
            // Modes, config options and session info have no place in the
            // seam yet: read, not shown.
            _ => {}
        }
    }

    /// The agent's words: appended to its streaming text block, or a new
    /// one when something else spoke since.
    fn text(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let Some(turn) = self.turn() else { return };
        self.end_thought();
        let open = self.open.as_mut().expect("a turn is open");
        if let Some(key) = open.text.clone() {
            if self.builder.is_last(&key) {
                self.builder.append(&key, transcript_core::builder::Stream::Text, chunk);
                return;
            }
        }
        open.keyed += 1;
        let key = format!("text:{turn}:{}", open.keyed);
        open.text = Some(key.clone());
        self.builder.add(&turn, Some(&key), Block::Text { text: chunk.to_owned(), streaming: true });
    }

    /// The agent's reasoning, the same way.
    fn thought(&mut self, chunk: &str) {
        if chunk.is_empty() {
            return;
        }
        let Some(turn) = self.turn() else { return };
        self.end_text();
        let open = self.open.as_mut().expect("a turn is open");
        if let Some((key, _)) = open.thought.clone() {
            if self.builder.is_last(&key) {
                self.builder.append(&key, transcript_core::builder::Stream::Thinking, chunk);
                return;
            }
        }
        open.keyed += 1;
        let key = format!("thought:{turn}:{}", open.keyed);
        open.thought = Some((key.clone(), Instant::now()));
        self.builder.add(
            &turn,
            Some(&key),
            Block::Thinking { text: chunk.to_owned(), elapsed_ms: 0, summary: None, state: ThinkingState::Thinking },
        );
    }

    fn end_text(&mut self) {
        let Some(key) = self.open.as_mut().and_then(|open| open.text.take()) else { return };
        self.builder.modify(&key, |block| {
            if let Block::Text { streaming, .. } = block {
                *streaming = false;
            }
        });
    }

    fn end_thought(&mut self) {
        let Some((key, began)) = self.open.as_mut().and_then(|open| open.thought.take()) else { return };
        self.builder.modify(&key, |block| {
            if let Block::Thinking { state, elapsed_ms, .. } = block {
                *state = ThinkingState::Done;
                *elapsed_ms = wall_ms_since(began);
            }
        });
    }

    /// A tool call opened or moved: its card, filed under its id.
    fn tool(&mut self, id: &str, update: &Value) {
        let Some(turn) = self.turn() else { return };
        self.end_text();
        self.end_thought();
        let tool = self.tools.entry(id.to_owned()).or_insert_with(Tool::new);
        tool.merge(update);
        let tool = tool.clone();
        self.builder.upsert(&turn, &tool_key(id), tool.event(id, &self.cwd).block());
        if tool.finished() {
            // An approved call that ran: its card settles with the result.
            let approved: Vec<String> = self
                .asks
                .iter()
                .filter(|(approval, ask)| {
                    ask.tool == id
                        && matches!(
                            self.builder.block(&approval_key(approval)),
                            Some(Block::Approval { state: ApprovalState::Approving, .. })
                        )
                })
                .map(|(approval, _)| approval.clone())
                .collect();
            for approval in approved {
                if let Some(ask) = self.asks.remove(&approval) {
                    let state = ApprovalState::AllowedOnce {
                        exit_code: tool.exit_code().unwrap_or(i32::from(tool.status() == ToolStatus::Error)),
                        duration_ms: wall_ms_since(ask.raised),
                    };
                    self.builder.approval_resolved(&approval, state);
                }
            }
        }
    }

    /// The agent's plan: one todo card per turn, rewritten whole.
    fn plan(&mut self, update: &Value) {
        let Some(turn) = self.turn() else { return };
        self.end_text();
        self.end_thought();
        let items = update
            .get("entries")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .map(|entry| TodoItem {
                        label: entry.get("content").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        state: match entry.get("status").and_then(Value::as_str) {
                            Some("completed") => TodoState::Done,
                            Some("in_progress") => TodoState::Running,
                            _ => TodoState::Pending,
                        },
                        elapsed_ms: None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.builder.upsert(&turn, &format!("plan:{turn}"), Block::Todo { items });
    }

    // -------------------------------------------------------- approvals

    /// A `session/request_permission` under `approval_id`: the call it
    /// gates is refaced from the request's `toolCall`, and a pending card
    /// offers the request's options in the one vocabulary. Returns the
    /// tap's headline and options; `None` when no turn is open to hold
    /// the card (the caller answers the request cancelled).
    pub fn permission(&mut self, approval_id: &str, params: &Value) -> Option<(String, Vec<ApprovalOption>)> {
        let options = crate::wire::permission_options(params);
        let offered = offered(&options);
        let call = params.get("toolCall").cloned().unwrap_or(Value::Null);
        let tool_id = call.get("toolCallId").and_then(Value::as_str).unwrap_or(approval_id).to_owned();
        let turn = self.turn()?;
        self.end_text();
        self.end_thought();
        let tool = self.tools.entry(tool_id.clone()).or_insert_with(Tool::new);
        tool.merge(&call);
        let tool = tool.clone();
        self.builder.upsert(&turn, &tool_key(&tool_id), tool.event(&tool_id, &self.cwd).block());
        let title = if tool.title.trim().is_empty() { tool.noun().to_owned() } else { tool.title.clone() };
        let (command, body_kind) = match tool.kind.as_str() {
            "execute" => (tool.command().unwrap_or_else(|| title.clone()), ApprovalBodyKind::Command),
            "edit" | "delete" | "move" => (tool.path(&self.cwd).unwrap_or_else(|| title.clone()), ApprovalBodyKind::FileWrite),
            _ => (tool.path(&self.cwd).unwrap_or_else(|| title.clone()), ApprovalBodyKind::Other),
        };
        let reason = if command == title { String::new() } else { title.clone() };
        let choices = offered.iter().map(|option| option.answer.choice(option.choice.clone())).collect();
        let card = transcript_core::approval::card(approval_id, tool.noun(), command, reason, self.cwd.clone(), body_kind, choices);
        self.builder.approval_raised(&turn, card);
        self.asks.insert(approval_id.to_owned(), Ask { tool: tool_id, options, raised: Instant::now() });
        Some((title, offered))
    }

    /// The kind of `option` on `approval_id`'s request, when it offered
    /// that option.
    pub fn option_kind(&self, approval_id: &str, option: &str) -> Option<OptionKind> {
        self.asks.get(approval_id)?.options.iter().find(|offered| offered.id == option)?.kind
    }

    /// The person decided `approval_id` with an option of `kind`: an allow
    /// reads approved until the call it gates finishes; a refusal settles
    /// the card at once.
    pub fn decided(&mut self, approval_id: &str, kind: OptionKind) {
        if kind.allows() {
            self.builder.approval_restated(approval_id, ApprovalState::Approving);
        } else {
            self.asks.remove(approval_id);
            self.builder.approval_resolved(approval_id, ApprovalState::Denied);
        }
    }

    /// `approval_id` will never be answered (the turn was cancelled, the
    /// child died): its card settles as cancelled with `reason`.
    pub fn abandoned(&mut self, approval_id: &str, reason: &str) {
        self.asks.remove(approval_id);
        self.builder.approval_resolved(approval_id, ApprovalState::Cancelled { reason: Some(reason.to_owned()) });
    }

    // ----------------------------------------------------------- endings

    /// The live turn ended: its cards settle, a failure leaves its error
    /// card, and it finishes. `None` when no live turn was open; else the
    /// turn and how it ended.
    pub fn finish(&mut self, ending: &Ending) -> Option<(String, TurnOutcome)> {
        let open = self.open.as_ref().filter(|open| open.live)?;
        let (turn, started) = (open.turn.clone(), open.started);
        let (outcome, failure) = match ending {
            Ending::Stopped(StopReason::EndTurn) => (TurnOutcome::Completed, None),
            Ending::Stopped(StopReason::Cancelled) => (TurnOutcome::Interrupted, None),
            Ending::Stopped(reason) => {
                let words = match reason {
                    StopReason::MaxTokens => "The agent stopped: it reached its output limit.".to_owned(),
                    StopReason::MaxTurnRequests => "The agent stopped: it reached its limit of model requests for one turn.".to_owned(),
                    StopReason::Refusal => "The agent refused to go on.".to_owned(),
                    StopReason::Other(other) => format!("The agent stopped: {other}."),
                    StopReason::EndTurn | StopReason::Cancelled => unreachable!("matched above"),
                };
                (TurnOutcome::Failed { message: Some(words.clone()), signed_out: false }, Some((FailureKind::Other, words)))
            }
            Ending::Failed { code, message } => (
                TurnOutcome::Failed { message: Some(message.clone()), signed_out: *code == crate::wire::AUTH_REQUIRED },
                Some((FailureKind::from_words(message), message.clone())),
            ),
        };
        let stopped = matches!(outcome, TurnOutcome::Interrupted | TurnOutcome::Failed { .. });
        self.settle_tools(&turn, stopped);
        self.settle_asks("The turn ended before it was answered.");
        if let Some((kind, words)) = failure {
            self.settle_streams();
            self.builder.add(&turn, None, failure::card(kind, &words));
        }
        let meta = TurnMeta { duration_ms: wall_ms_since(started), ..TurnMeta::default() };
        self.close_open(meta);
        Some((turn, outcome))
    }

    /// The child died: every approval settles cancelled, and the turn it
    /// was running closes with a card saying so (docs/71 §2). The deltas
    /// come first; the caller reports the loss after them.
    pub fn lost(&mut self, reason: &str) {
        self.history = false;
        self.ask = None;
        let open: Vec<String> = self.builder.open_approvals().to_vec();
        for approval in open {
            self.abandoned(&approval, reason);
        }
        self.asks.clear();
        let Some(turn) = self.open.as_ref().map(|open| open.turn.clone()) else { return };
        self.settle_tools(&turn, true);
        self.settle_streams();
        self.builder.add(
            &turn,
            None,
            Block::Error { title: "The agent stopped unexpectedly".into(), detail: reason.to_owned(), retryable: false },
        );
        let started = self.open.as_ref().map_or_else(Instant::now, |open| open.started);
        self.close_open(TurnMeta { duration_ms: wall_ms_since(started), ..TurnMeta::default() });
    }

    /// Calls the turn left pending or running settle: cancelled when the
    /// turn was stopped, else as the agent last said.
    fn settle_tools(&mut self, turn: &str, stopped: bool) {
        if !stopped {
            return;
        }
        let mut unfinished: Vec<String> = self
            .tools
            .iter()
            .filter(|(id, tool)| !tool.finished() && self.builder.turn_of(&tool_key(id)) == Some(turn))
            .map(|(id, _)| id.clone())
            .collect();
        unfinished.sort();
        for id in unfinished {
            let Some(tool) = self.tools.get(&id) else { continue };
            let mut event = tool.event(&id, &self.cwd);
            event.status = ToolStatus::Cancelled;
            event.body = match event.body {
                ToolBody::Shell { output_lines, exit_code, .. } => ToolBody::Shell { output_lines, exit_code, live: false },
                other => other,
            };
            self.builder.update(&tool_key(&id), event.block());
        }
    }

    /// Requests still open when the turn ended: an approved one ran (its
    /// call never said it finished), an unanswered one never will be.
    fn settle_asks(&mut self, reason: &str) {
        let open: Vec<String> = self.builder.open_approvals().to_vec();
        for approval in open {
            let approved = matches!(self.builder.block(&approval_key(&approval)), Some(Block::Approval { state: ApprovalState::Approving, .. }));
            if approved {
                let raised = self.asks.remove(&approval).map_or_else(Instant::now, |ask| ask.raised);
                self.builder.approval_resolved(&approval, ApprovalState::AllowedOnce { exit_code: 0, duration_ms: wall_ms_since(raised) });
            } else {
                self.abandoned(&approval, reason);
            }
        }
    }
}

/// A content chunk's text (`update.content` a text block); empty for any
/// other content.
fn chunk_text(update: &Value) -> String {
    let content = update.get("content");
    match content.and_then(|content| content.get("type")).and_then(Value::as_str) {
        Some("text") | None => {
            content.and_then(|content| content.get("text")).and_then(Value::as_str).unwrap_or_default().to_owned()
        }
        _ => String::new(),
    }
}

/// A request's options in the one vocabulary, in card order: allow once,
/// always allow, refuse (the first refusal: "refuse and remember" has no
/// answer of its own and stands in only when it is the only refusal). An
/// option of a kind this client does not know is not offered.
pub fn offered(options: &[PermissionOption]) -> Vec<ApprovalOption> {
    let first = |kinds: &[OptionKind]| {
        kinds.iter().find_map(|kind| options.iter().find(|option| option.kind == Some(*kind))).map(|option| option.id.clone())
    };
    [
        (ApprovalAnswer::Once, first(&[OptionKind::AllowOnce])),
        (ApprovalAnswer::AlwaysHere, first(&[OptionKind::AllowAlways])),
        (ApprovalAnswer::Deny, first(&[OptionKind::RejectOnce, OptionKind::RejectAlways])),
    ]
    .into_iter()
    .filter_map(|(answer, choice)| Some(ApprovalOption { answer, choice: choice? }))
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn replayed(fold: &mut AcpFold) -> aui_protocol::Session {
        let mut session = aui_protocol::Session::new("s", MARK, "", "/w");
        for delta in fold.take() {
            assert!(session.apply(delta.clone()), "every delta lands: {delta:?}");
        }
        session
    }

    fn chunk(kind: &str, text: &str) -> Value {
        json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
    }

    #[test]
    fn a_turn_streams_its_words_thoughts_tools_and_plan_into_one_assistant_turn() {
        let mut fold = AcpFold::new("s", "/w");
        let mut events = Vec::new();
        fold.submitted("t1", "fix it", None);
        fold.update(&chunk("agent_thought_chunk", "Looking"), &mut events);
        fold.update(&chunk("agent_thought_chunk", " around."), &mut events);
        fold.update(&chunk("agent_message_chunk", "I will "), &mut events);
        fold.update(&chunk("agent_message_chunk", "read it."), &mut events);
        fold.update(
            &json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "Read main.rs", "kind": "read", "status": "pending",
                    "locations": [{"path": "/w/src/main.rs"}]}),
            &mut events,
        );
        fold.update(
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
                    "content": [{"type": "content", "content": {"type": "text", "text": "fn main() {}\n"}}]}),
            &mut events,
        );
        fold.update(
            &json!({"sessionUpdate": "plan", "entries": [
                {"content": "Read", "priority": "high", "status": "completed"},
                {"content": "Fix", "priority": "high", "status": "in_progress"}]}),
            &mut events,
        );
        fold.update(&chunk("agent_message_chunk", "Done."), &mut events);
        let ended = fold.finish(&Ending::Stopped(StopReason::EndTurn));
        assert_eq!(ended, Some(("t1".to_owned(), TurnOutcome::Completed)));
        let session = replayed(&mut fold);
        assert_eq!(session.turns.len(), 2, "the person's bubble and the agent's turn");
        assert!(matches!(&session.turns[0], Turn::User { text, .. } if text == "fix it"));
        let blocks = session.turns[1].blocks();
        assert!(matches!(&blocks[0], Block::Thinking { text, state: ThinkingState::Done, .. } if text == "Looking around."));
        assert!(matches!(&blocks[1], Block::Text { text, streaming: false } if text == "I will read it."));
        assert!(
            matches!(&blocks[2], Block::ToolCall { kind: ToolKind::Read, target, status: ToolStatus::Success, body: ToolBody::Read { lines: 1, .. }, .. } if target == "src/main.rs")
        );
        assert!(matches!(&blocks[3], Block::Todo { items } if items.len() == 2 && items[1].state == TodoState::Running));
        assert!(matches!(&blocks[4], Block::Text { text, .. } if text == "Done."));
        assert!(events.is_empty());
    }

    #[test]
    fn a_permission_raises_one_card_in_the_vocabulary_and_settles_with_the_call() {
        let mut fold = AcpFold::new("s", "/w");
        fold.submitted("t1", "run it", None);
        let params = json!({"sessionId": "s",
            "toolCall": {"toolCallId": "c1", "title": "Run tests", "kind": "execute", "status": "pending", "rawInput": {"command": "cargo test"}},
            "options": [
                {"optionId": "yes", "name": "Allow", "kind": "allow_once"},
                {"optionId": "always", "name": "Always allow", "kind": "allow_always"},
                {"optionId": "no", "name": "Reject", "kind": "reject_once"}]});
        let (headline, options) = fold.permission("c1", &params).expect("a turn holds the card");
        assert_eq!(headline, "Run tests");
        let answers: Vec<ApprovalAnswer> = options.iter().map(|option| option.answer).collect();
        assert_eq!(answers, vec![ApprovalAnswer::Once, ApprovalAnswer::AlwaysHere, ApprovalAnswer::Deny]);
        assert_eq!(fold.option_kind("c1", "no"), Some(OptionKind::RejectOnce));
        fold.decided("c1", OptionKind::AllowOnce);
        fold.update(
            &json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed", "rawOutput": {"exitCode": 0}}),
            &mut Vec::new(),
        );
        fold.finish(&Ending::Stopped(StopReason::EndTurn));
        let session = replayed(&mut fold);
        let blocks = session.turns[1].blocks();
        assert!(matches!(&blocks[0], Block::ToolCall { kind: ToolKind::Shell, target, status: ToolStatus::Success, .. } if target == "cargo test"));
        let Block::Approval { command, choices, state, .. } = &blocks[1] else { panic!("an approval card: {blocks:?}") };
        assert_eq!(command, "cargo test");
        assert_eq!(choices.iter().map(|choice| choice.id.as_str()).collect::<Vec<_>>(), vec!["yes", "always", "no"]);
        assert!(matches!(state, ApprovalState::AllowedOnce { exit_code: 0, .. }), "{state:?}");
    }

    #[test]
    fn a_refusal_and_a_cancel_settle_their_cards() {
        let mut fold = AcpFold::new("s", "/w");
        fold.submitted("t1", "edit", None);
        let ask = |id: &str| {
            json!({"toolCall": {"toolCallId": id, "title": "Edit", "kind": "edit"},
                   "options": [{"optionId": "ok", "name": "Ok", "kind": "allow_once"}, {"optionId": "never", "name": "Never", "kind": "reject_always"}]})
        };
        let (_, options) = fold.permission("a", &ask("a")).expect("a turn holds the card");
        assert_eq!(options.last().map(|option| (option.answer, option.choice.as_str())), Some((ApprovalAnswer::Deny, "never")));
        fold.permission("b", &ask("b"));
        fold.decided("a", OptionKind::RejectAlways);
        fold.finish(&Ending::Stopped(StopReason::Cancelled));
        let session = replayed(&mut fold);
        let states: Vec<&ApprovalState> = session.turns[1]
            .blocks()
            .iter()
            .filter_map(|block| match block {
                Block::Approval { state, .. } => Some(state),
                _ => None,
            })
            .collect();
        assert_eq!(states[0], &ApprovalState::Denied);
        assert!(matches!(states[1], ApprovalState::Cancelled { .. }), "an unanswered ask is cancelled, never denied");
        assert!(fold.builder.open_approvals().is_empty());
    }

    #[test]
    fn a_failed_turn_leaves_an_error_card_and_a_lost_child_closes_the_turn() {
        let mut fold = AcpFold::new("s", "/w");
        fold.submitted("t1", "go", None);
        let ended = fold.finish(&Ending::Failed { code: -32603, message: "rate limit exceeded".into() });
        assert!(matches!(ended, Some((_, TurnOutcome::Failed { .. }))));
        fold.submitted("t2", "again", None);
        fold.update(
            &json!({"sessionUpdate": "tool_call", "toolCallId": "c", "title": "ls", "kind": "execute", "status": "in_progress"}),
            &mut Vec::new(),
        );
        fold.lost("the agent process exited");
        assert_eq!(fold.running_turn(), None);
        let session = replayed(&mut fold);
        assert!(matches!(session.turns[1].blocks().last(), Some(Block::Error { title, retryable: true, .. }) if title == "Rate limit reached"));
        let lost = session.turns[3].blocks();
        assert!(matches!(&lost[0], Block::ToolCall { status: ToolStatus::Cancelled, .. }));
        assert!(matches!(lost.last(), Some(Block::Error { detail, .. }) if detail == "the agent process exited"));
    }

    #[test]
    fn a_load_cuts_the_stored_stream_into_turns_at_the_persons_words() {
        let mut fold = AcpFold::new("s", "/w");
        let mut events = Vec::new();
        fold.begin_history();
        fold.update(&chunk("user_message_chunk", "first"), &mut events);
        fold.update(&chunk("agent_message_chunk", "one"), &mut events);
        fold.update(&chunk("user_message_chunk", "sec"), &mut events);
        fold.update(&chunk("user_message_chunk", "ond"), &mut events);
        fold.update(&chunk("agent_message_chunk", "two"), &mut events);
        fold.end_history();
        let session = replayed(&mut fold);
        let shape: Vec<String> = session
            .turns
            .iter()
            .map(|turn| match turn {
                Turn::User { text, .. } => format!("user:{text}"),
                Turn::Assistant { blocks, .. } => format!("agent:{}", blocks.len()),
            })
            .collect();
        assert_eq!(shape, vec!["user:first", "agent:1", "user:second", "agent:1"]);
        assert_eq!(fold.running_turn(), None, "a stored turn is never running");
    }

    #[test]
    fn commands_and_context_ride_beside_the_transcript() {
        let mut fold = AcpFold::new("s", "/w");
        let mut events = Vec::new();
        fold.update(
            &json!({"sessionUpdate": "available_commands_update", "availableCommands": [{"name": "review", "description": "Review the diff"}]}),
            &mut events,
        );
        fold.update(&json!({"sessionUpdate": "usage_update", "used": 1200, "size": 200000}), &mut events);
        assert!(matches!(&events[0], ProviderEvent::SkillsListed { skills, .. } if skills[0].name == "review"));
        assert!(
            matches!(&events[1], ProviderEvent::SessionFacts { side, .. } if side.context.as_ref().is_some_and(|context| context.window_tokens == Some(200000)))
        );
    }

    #[test]
    fn an_edit_with_both_texts_draws_its_change_and_a_new_file_its_lines() {
        let tool = |content: Value| {
            let mut tool = Tool::new();
            tool.merge(&json!({"kind": "edit", "status": "completed", "content": [content]}));
            tool.event("e", "/w").block()
        };
        let edit = tool(json!({"type": "diff", "path": "/w/a.rs", "oldText": "a\nb\nc\n", "newText": "a\nB\nc\n"}));
        assert!(
            matches!(&edit, Block::ToolCall { kind: ToolKind::Edit, target, diff_stat: Some(stat), .. } if target == "a.rs" && stat.added == 1 && stat.removed == 1),
            "{edit:?}"
        );
        let new = tool(json!({"type": "diff", "path": "/w/n.rs", "newText": "x\ny\n"}));
        assert!(matches!(&new, Block::ToolCall { kind: ToolKind::Write, diff_stat: Some(stat), .. } if stat.added == 2));
    }
}
