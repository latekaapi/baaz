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
//!   cost in the footer meta.
//! * everything else: no deltas (`init` records identity; `rate_limit`
//!   updates the account snapshot).

use std::collections::HashMap;
use std::io::BufRead;

use aui_protocol::{Block, Delta, ThinkingState, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta};
use provider::ProviderEvent;

use crate::account::AccountSnapshot;
use crate::frame::{ContentBlock, Frame};

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
                let open = std::mem::take(&mut self.open);
                open.into_iter()
                    .map(|turn_id| Delta::TurnFinished { turn_id, meta: meta.clone() })
                    .collect()
            }
            Frame::RateLimit(info) => {
                self.account.observe(info.clone());
                Vec::new()
            }
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
    let deltas = fold.apply(&frame);
    if !deltas.is_empty() {
        emit(ProviderEvent::Deltas { session_id, deltas });
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
}
