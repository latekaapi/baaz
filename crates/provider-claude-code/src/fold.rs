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
//! * `assistant`: one [`Delta::TurnStarted`] per unseen message id (turn id
//!   IS the message id), then one [`Delta::BlockAdded`] per content block:
//!   `text` → [`Block::Text`] (complete, `streaming: false`), `thinking` →
//!   [`Block::Thinking`], `tool_use` → [`Block::ToolCall`] (`Bash` is shell,
//!   `mcp__<server>__<tool>` is MCP, anything else is [`Block::Generic`]).
//! * `user` (tool results): one [`Delta::BlockUpdated`] per answered tool
//!   call, completing its card (`Success`/`Error` plus output).
//! * `result`: one [`Delta::TurnFinished`] per open turn, with usage and
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

use std::collections::HashMap;
use std::io::BufRead;

use aui_protocol::{
    ApprovalBadges, ApprovalChoice, ApprovalDecision, ApprovalScope, ApprovalState, Block, Delta,
    ThinkingState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta,
};
use provider::{ProviderError, ProviderEvent};

use crate::account::AccountSnapshot;
use crate::frame::{approval_headline, ApprovalRequest, ContentBlock, Frame};

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

/// The fold: decoded frames in, [`Delta`]s out.
///
/// Stateful only where the wire is relational: message id → turn (many
/// `assistant` frames share one message), tool-use id → card (results
/// arrive on later `user` frames), open turns → finished by `result`.
#[derive(Clone, Debug, Default)]
pub struct ClaudeFold {
    started: HashMap<String, ()>,
    open: Vec<String>,
    tools: HashMap<String, ToolSite>,
    /// Blocks already emitted per turn, so a second `assistant` frame with
    /// the same message id continues the index sequence (and a later
    /// `BlockUpdated` for a tool result addresses the right card).
    emitted: HashMap<String, usize>,
    model: Option<String>,
    session_id: Option<String>,
    account: AccountSnapshot,
    /// `can_use_tool` requests waiting on a human decision, oldest first. A
    /// request stays here until `DecideApproval` answers it: nothing in the
    /// adapter answers on its own, because an unanswered child waits
    /// silently and an auto-answered one would bypass the person.
    pending: Vec<ApprovalRequest>,
    /// Unknown `control_request` subtypes seen so far, as
    /// `(request_id, subtype)` in arrival order. Surfaced in the transcript,
    /// never dropped.
    unknown_control: Vec<(String, String)>,
    /// Unknown-subtype requests waiting on a human decision, oldest first.
    /// Answered through `DecideApproval` with `"deny"` (a refusal) or
    /// `"allow"`; never auto-answered, never unanswerable.
    pending_unknown: Vec<UnknownControlRequest>,
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

    /// Remember the effective model from a `SelectModel` admission: the next
    /// turn's footer reads it, the way `init`'s model lands in the first.
    pub fn set_model(&mut self, model: &str) {
        self.model = Some(model.to_owned());
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

    /// Take a pending request for deciding. Removes it, so a second decision
    /// cannot answer the same request twice. `None` means nobody asked for
    /// that id — an unknown approval decides nothing.
    pub fn take_approval(&mut self, request_id: &str) -> Option<ApprovalRequest> {
        let position = self.pending.iter().position(|queued| queued.request_id == request_id)?;
        Some(self.pending.remove(position))
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
    /// duplicating ids.
    pub fn requeue_approval(&mut self, request: ApprovalRequest) {
        if !self.pending.iter().any(|queued| queued.request_id == request.request_id) {
            self.pending.push(request);
        }
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
                Vec::new()
            }
            Frame::Assistant { message_id, blocks, .. } => self.apply_blocks(message_id, blocks),
            Frame::UserResult { results, .. } => {
                results.iter().filter_map(|result| self.apply_result(result)).collect()
            }
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
                let meta = TurnMeta {
                    model: model.clone().or_else(|| self.model.clone()).unwrap_or_default(),
                    duration_ms: *duration_ms,
                    tokens_in: *input_tokens,
                    tokens_out: *output_tokens,
                    reasoning_tokens: *reasoning_tokens,
                    cost_usd: *total_cost_usd,
                    cache_read_tokens: *cache_read_tokens,
                    cache_write_tokens: *cache_write_tokens,
                    cached_tokens: 0,
                };
                let mut deltas = Vec::new();
                // After-the-fact refusals, on the transcript before the
                // finish: decoded, visible, and never a substitute for
                // answering the request itself.
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
                        deltas.push(Delta::BlockAdded {
                            turn_id: turn_id.clone(),
                            block: Block::Generic {
                                kind: "permission-denial".into(),
                                status: "denied".into(),
                                text: format!(
                                    "{} ({}) refused",
                                    denial.tool_name, denial.tool_use_id
                                ),
                            },
                        });
                    }
                }
                let open = std::mem::take(&mut self.open);
                deltas.extend(
                    open.into_iter()
                        .map(|turn_id| Delta::TurnFinished { turn_id, meta: meta.clone() }),
                );
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
            // needs answering.
            Frame::ControlResponse { .. } => Vec::new(),
            // The other lane, by choice (see module docs): parsed, ignored.
            Frame::Stream { .. } | Frame::Ignored { .. } => Vec::new(),
        }
    }

    fn apply_blocks(&mut self, message_id: &str, blocks: &[ContentBlock]) -> Vec<Delta> {
        let mut deltas = Vec::new();
        if !self.started.contains_key(message_id) {
            self.started.insert(message_id.to_owned(), ());
            self.open.push(message_id.to_owned());
            deltas.push(Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: message_id.to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp: None,
                },
            });
        }
        for block in blocks.iter() {
            let block_index = self.emitted.get(message_id).copied().unwrap_or(0);
            let emitted = match block {
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
                    deltas.push(Delta::BlockAdded {
                        turn_id: message_id.to_owned(),
                        block: emitted.expect("covered above"),
                    });
                    self.emitted.insert(message_id.to_owned(), block_index + 1);
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let site = tool_card(id, name, input);
                    self.tools.insert(id.clone(), ToolSite {
                        turn_id: message_id.to_owned(),
                        block_index,
                        kind: site.kind.clone(),
                        verb: site.verb.clone(),
                        target: site.target.clone(),
                        name: name.clone(),
                        params: site.params.clone(),
                    });
                    deltas.push(Delta::BlockAdded {
                        turn_id: message_id.to_owned(),
                        block: site.block,
                    });
                    self.emitted.insert(message_id.to_owned(), block_index + 1);
                }
                ContentBlock::Other { .. } => {}
            }
        }
        deltas
    }

    fn apply_result(&mut self, result: &crate::frame::ToolResult) -> Option<Delta> {
        let site = self.tools.get(&result.tool_use_id)?.clone();
        let status = if result.is_error { ToolStatus::Error } else { ToolStatus::Success };
        let block = match &site.kind {
            ToolKind::Shell => Block::ToolCall {
                id: result.tool_use_id.clone(),
                kind: site.kind.clone(),
                verb: site.verb.clone(),
                target: site.target.clone(),
                status,
                duration_ms: None,
                body: ToolBody::Shell {
                    output_lines: result.text.lines().map(str::to_owned).collect(),
                    exit_code: None,
                    live: false,
                },
                diff_stat: None,
            },
            ToolKind::Mcp { .. } => Block::ToolCall {
                id: result.tool_use_id.clone(),
                kind: site.kind.clone(),
                verb: site.verb.clone(),
                target: site.target.clone(),
                status,
                duration_ms: None,
                body: ToolBody::Mcp { params: site.params.clone(), result_json: result.text.clone() },
                diff_stat: None,
            },
            _ => Block::Generic {
                kind: site.name.clone(),
                status: if result.is_error { "error".into() } else { "completed".into() },
                text: result.text.clone(),
            },
        };
        Some(Delta::BlockUpdated { turn_id: site.turn_id, block_index: site.block_index, block })
    }

    /// Queue a `can_use_tool` request as pending and card it. The card rides
    /// the running turn; with no turn open (a bare control exchange) a turn
    /// keyed by the request itself opens so the card is never dangling.
    fn apply_approval(&mut self, request: &ApprovalRequest) -> Vec<Delta> {
        if !self.pending.iter().any(|queued| queued.request_id == request.request_id) {
            self.pending.push(request.clone());
        }
        let mut deltas = Vec::new();
        let turn_id = self.control_turn(&request.request_id, &mut deltas);
        deltas.push(Delta::BlockAdded { turn_id, block: approval_card(request) });
        deltas
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

    /// The turn hosting a control-channel card: the running turn when one is
    /// open, else a fresh turn keyed by `key` (announced in `deltas`) so the
    /// card lands somewhere the transcript owns.
    fn control_turn(&mut self, key: &str, deltas: &mut Vec<Delta>) -> String {
        if let Some(open) = self.open.last().cloned() {
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

/// The transcript card for a `can_use_tool` request: a pending approval the
/// person resolves through `DecideApproval` with the card's `"allow"` /
/// `"deny"` choices.
fn approval_card(request: &ApprovalRequest) -> Block {
    let input = match &request.input {
        serde_json::Value::Null => String::new(),
        serde_json::Value::Object(map) if map.is_empty() => String::new(),
        other => other.to_string(),
    };
    let command = if input.is_empty() {
        request.tool_name.clone()
    } else {
        format!("{} {input}", request.tool_name)
    };
    let mut reason =
        format!("tool_use {} requests {}", request.tool_use_id, approval_headline(request));
    if let Some(server) = &request.mcp_server {
        reason.push_str(&format!(" via MCP server {server}"));
    }
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
        cwd: String::new(),
        capabilities,
        scope: ApprovalScope::ThisCommand,
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
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Shell,
            verb: "Ran".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::Shell { output_lines: Vec::new(), exit_code: None, live: true },
            diff_stat: None,
        };
        ToolCard { kind: ToolKind::Shell, verb: "Ran".into(), target, params, block }
    } else if let Some((server, tool)) = mcp_split(name) {
        let target = format!("{server} · {tool}");
        let block = Block::ToolCall {
            id: id.to_owned(),
            kind: ToolKind::Mcp { server: server.clone(), tool: tool.clone() },
            verb: "Ran".into(),
            target: target.clone(),
            status: ToolStatus::Running,
            duration_ms: None,
            body: ToolBody::Mcp { params: params.clone(), result_json: String::new() },
            diff_stat: None,
        };
        ToolCard {
            kind: ToolKind::Mcp { server, tool },
            verb: "Ran".into(),
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
    /// describe the same content a second time.
    #[test]
    fn partial_renders_each_assistant_message_exactly_once() {
        let partial = fixture_lines("partial.jsonl");
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
                    .map(Vec::len)
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
    #[test]
    fn permission_request_stays_pending_until_decided() {
        let (mut fold, _) = fold_lines(&permission_child_lines());
        let request_id = {
            let pending = fold.pending_approvals();
            assert_eq!(pending.len(), 1, "the can_use_tool request must surface, not fold away");
            let request = &pending[0];
            assert_eq!(request.tool_name, "mcp__baaz__ping");
            assert_eq!(request.tool_use_id, "toolu_01CPKoR3sS6ZvHfqgQtJWZU5");
            request.request_id.clone()
        };
        // The card is on the transcript too: a pending approval, not prose.
        let (_, deltas) = fold_lines(&permission_child_lines());
        let cards = deltas
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
        // Deciding takes it off the pending set exactly once.
        let taken = fold.take_approval(&request_id);
        assert!(taken.is_some());
        assert!(fold.pending_approvals().is_empty());
        assert!(fold.take_approval(&request_id).is_none());
    }

    /// Decisions are explicit and typed: allow and deny render their wire
    /// lines, anything else is rejected with the offered choices named.
    #[test]
    fn decisions_are_explicit_allow_or_deny() {
        let (fold, _) = fold_lines(&permission_child_lines());
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
        assert!(
            deltas.iter().any(|delta| match delta {
                Delta::BlockAdded {
                    block: Block::Generic { text, .. },
                    ..
                } => text.contains("toolu_01XqHeZeKksDmhf4miPM8P5C"),
                _ => false,
            }),
            "the card names the refused tool use: {deltas:?}"
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
}
