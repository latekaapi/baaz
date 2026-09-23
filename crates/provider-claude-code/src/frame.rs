//! One CLI stdout line in, one decoded frame out. Pure and IO-free.
//!
//! Every shape in `fixtures/claude-code/` must decode without error,
//! including the operator's own `system/hook_started` and
//! `system/hook_response` frames, which are noise. An unknown `type` (or an
//! unknown `system` subtype, `stream_event` kind, or assistant content kind)
//! decodes to [`Frame::Ignored`], never an error: the CLI will add frames
//! and Baaz must not die on an upgrade.

use serde_json::Value;

/// A content block inside an `assistant` frame's message.
#[derive(Clone, Debug, PartialEq)]
pub enum ContentBlock {
    /// A `thinking` block: reasoning trace plus its signature.
    Thinking {
        /// The trace text (empty in every checked-in fixture).
        text: String,
    },
    /// A `text` block: finished prose.
    Text {
        /// The completed text.
        text: String,
    },
    /// A `tool_use` block: one tool invocation with its whole input.
    ToolUse {
        /// The wire tool-use id (`toolu_…`), for joining tool results.
        id: String,
        /// Tool name (`Bash`, `ToolSearch`, `mcp__<server>__<tool>`, …).
        name: String,
        /// The complete input object.
        input: Value,
    },
    /// A content kind this decoder does not know. Carried, not dropped, so
    /// a count of frames still balances; the fold renders nothing for it.
    Other {
        /// The wire `type` string.
        kind: String,
    },
}

/// One tool result inside a `user` frame's message.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResult {
    /// The `tool_use_id` this answers.
    pub tool_use_id: String,
    /// Human rendering of the result content (joined text parts).
    pub text: String,
    /// The wire `is_error` flag, defaulting to false when absent.
    pub is_error: bool,
}

/// The `init` frame: session identity plus the turn's tool surface.
#[derive(Clone, Debug, PartialEq)]
pub struct InitFrame {
    /// The session id, repeated on every frame thereafter.
    pub session_id: String,
    /// Model id as reported, e.g. `claude-haiku-4-5-20251001`.
    pub model: String,
    /// Absolute cwd the child runs in.
    pub cwd: String,
    /// Tool names the turn may call, including `mcp__*` entries.
    pub tools: Vec<String>,
}

/// One decoded CLI stdout line.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// `system/init`: identity, model, cwd, tools.
    Init(InitFrame),
    /// An `assistant` frame: one or more completed content blocks of the
    /// message `message_id`. Several frames may share one message id; each
    /// carries distinct blocks.
    Assistant {
        /// Session this belongs to.
        session_id: String,
        /// The message id (`msg_…`); the fold's turn key.
        message_id: String,
        /// The frame's own uuid, for exactly-once counting.
        uuid: String,
        /// The completed blocks, in order.
        blocks: Vec<ContentBlock>,
    },
    /// A `user` frame: tool results (plus the sibling `tool_use_result`
    /// payload, kept raw — its shape varies by tool).
    UserResult {
        /// Session this belongs to.
        session_id: String,
        /// The results, in order.
        results: Vec<ToolResult>,
        /// The raw `tool_use_result` object, when present.
        raw_detail: Option<Value>,
    },
    /// A `stream_event` frame (only with `--include-partial-messages`).
    /// Carried minimally: the fold renders nothing from it (see the lane
    /// choice in [`crate::fold`]), but decoding it proves the shape still
    /// parses and keeps frame counts honest.
    Stream {
        /// Session this belongs to.
        session_id: String,
        /// The inner event type (`content_block_delta`, …).
        event_type: String,
    },
    /// A `rate_limit_event` frame: the account meter (doc §5).
    RateLimit(crate::account::RateLimitInfo),
    /// A `result` frame: the turn's final text, usage, and cost.
    TurnResult {
        /// Session this belongs to.
        session_id: String,
        /// The final text (`result` string).
        text: String,
        /// Billed input tokens (`usage.input_tokens`).
        input_tokens: u64,
        /// Billed completion tokens (`usage.output_tokens`).
        output_tokens: u64,
        /// Cache-read tokens, when reported.
        cache_read_tokens: Option<u64>,
        /// Cache-creation tokens, when reported.
        cache_write_tokens: Option<u64>,
        /// Reasoning tokens, when reported.
        reasoning_tokens: u64,
        /// Turn cost in USD (`total_cost_usd`).
        total_cost_usd: f64,
        /// Wall-clock time in milliseconds (`duration_ms`).
        duration_ms: u64,
        /// Model id, from the first `modelUsage` key, when present.
        model: Option<String>,
    },
    /// Noise or the not-yet-known: hooks, spinner status, token estimates,
    /// turn summaries, and any unknown `type`. Ignored, not fatal.
    Ignored {
        /// Why this frame carries no transcript (e.g. `hook_started`,
        /// `status`, `thinking_tokens`, `unknown type "frobnicate"`).
        note: String,
    },
}

impl Frame {
    /// The session this frame belongs to, when it names one.
    /// `rate_limit_event` carries none on the wire.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Frame::Init(init) => Some(&init.session_id),
            Frame::Assistant { session_id, .. }
            | Frame::UserResult { session_id, .. }
            | Frame::Stream { session_id, .. }
            | Frame::TurnResult { session_id, .. } => Some(session_id),
            Frame::RateLimit(_) | Frame::Ignored { .. } => None,
        }
    }
}

/// A line that is not JSON at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// What failed to parse.
    pub reason: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "undecodable claude-code line: {}", self.reason)
    }
}

impl std::error::Error for DecodeError {}

fn text_parts(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str).unwrap_or_default().to_owned()
                } else {
                    // `tool_reference` and friends name a tool rather than
                    // carrying prose; keep the name so the text still says
                    // what happened.
                    part
                        .get("tool_name")
                        .and_then(Value::as_str)
                        .map(|name| format!("<{name}>"))
                        .unwrap_or_default()
                }
            })
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

fn decode_assistant(value: &Value) -> Frame {
    let session_id = value.get("session_id").and_then(Value::as_str).unwrap_or_default();
    let message = value.get("message");
    let message_id = message
        .and_then(|message| message.get("id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let uuid = value.get("uuid").and_then(Value::as_str).unwrap_or_default();
    let mut blocks = Vec::new();
    if let Some(items) = message.and_then(|message| message.get("content")).and_then(Value::as_array)
    {
        for item in items {
            let kind = item.get("type").and_then(Value::as_str).unwrap_or("");
            match kind {
                "thinking" => blocks.push(ContentBlock::Thinking {
                    text: item
                        .get("thinking")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                }),
                "text" => blocks.push(ContentBlock::Text {
                    text: item.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
                }),
                "tool_use" => blocks.push(ContentBlock::ToolUse {
                    id: item.get("id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    name: item.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                    input: item.get("input").cloned().unwrap_or(Value::Null),
                }),
                other => blocks.push(ContentBlock::Other { kind: other.to_owned() }),
            }
        }
    }
    Frame::Assistant {
        session_id: session_id.to_owned(),
        message_id: message_id.to_owned(),
        uuid: uuid.to_owned(),
        blocks,
    }
}

fn decode_user(value: &Value) -> Frame {
    let session_id = value.get("session_id").and_then(Value::as_str).unwrap_or_default();
    let mut results = Vec::new();
    if let Some(items) =
        value.get("message").and_then(|message| message.get("content")).and_then(Value::as_array)
    {
        for item in items {
            if item.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            results.push(ToolResult {
                tool_use_id: item
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                text: text_parts(item.get("content").unwrap_or(&Value::Null)),
                is_error: item.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            });
        }
    }
    Frame::UserResult {
        session_id: session_id.to_owned(),
        results,
        raw_detail: value.get("tool_use_result").cloned(),
    }
}

fn decode_turn_result(value: &Value) -> Frame {
    let usage = value.get("usage");
    let uint = |key: &str| usage.and_then(|usage| usage.get(key)).and_then(Value::as_u64);
    let model = value
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.keys().next().cloned());
    Frame::TurnResult {
        session_id: value.get("session_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
        text: value.get("result").and_then(Value::as_str).unwrap_or_default().to_owned(),
        input_tokens: uint("input_tokens").unwrap_or(0),
        output_tokens: uint("output_tokens").unwrap_or(0),
        cache_read_tokens: uint("cache_read_input_tokens"),
        cache_write_tokens: uint("cache_creation_input_tokens"),
        reasoning_tokens: usage
            .and_then(|usage| usage.get("output_tokens_details"))
            .and_then(|details| details.get("thinking_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        total_cost_usd: value.get("total_cost_usd").and_then(Value::as_f64).unwrap_or(0.0),
        duration_ms: value.get("duration_ms").and_then(Value::as_u64).unwrap_or(0),
        model,
    }
}

/// Decode one CLI stdout line. Pure and IO-free.
///
/// Unknown `type` values, unknown `system` subtypes, and unknown assistant
/// content kinds decode to [`Frame::Ignored`] (or [`ContentBlock::Other`]);
/// only non-JSON input is an error.
pub fn decode_line(line: &str) -> Result<Frame, DecodeError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| DecodeError { reason: error.to_string() })?;
    let frame_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    match frame_type {
        "system" => {
            let subtype = value.get("subtype").and_then(Value::as_str).unwrap_or("");
            match subtype {
                "init" => {
                    let tools = value
                        .get("tools")
                        .and_then(Value::as_array)
                        .map(|tools| {
                            tools
                                .iter()
                                .filter_map(Value::as_str)
                                .map(str::to_owned)
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    Ok(Frame::Init(InitFrame {
                        session_id: value
                            .get("session_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        model: value
                            .get("model")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        cwd: value.get("cwd").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        tools,
                    }))
                }
                // The operator's own hooks are noise; so are the spinner,
                // the token estimates, and the turn summaries.
                other => Ok(Frame::Ignored { note: other.to_owned() }),
            }
        }
        "assistant" => Ok(decode_assistant(&value)),
        "user" => Ok(decode_user(&value)),
        "stream_event" => Ok(Frame::Stream {
            session_id: value.get("session_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
            event_type: value
                .get("event")
                .and_then(|event| event.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
        }),
        "rate_limit_event" => value
            .get("rate_limit_info")
            .cloned()
            .map(|info| Frame::RateLimit(crate::account::RateLimitInfo::decode(&info)))
            .ok_or(DecodeError { reason: "rate_limit_event without rate_limit_info".into() }),
        "result" => Ok(decode_turn_result(&value)),
        // Forward compatibility: the CLI will add frames; Baaz ignores them.
        other => Ok(Frame::Ignored { note: format!("unknown type {other:?}") }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<String> {
        let path = format!(
            "{}/../../fixtures/claude-code/{name}",
            env!("CARGO_MANIFEST_DIR"),
            name = name
        );
        std::fs::read_to_string(path)
            .expect("fixture reads")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn every_fixture_line_decodes_without_error() {
        for name in ["basic.jsonl", "partial.jsonl", "mcp.jsonl", "resume.jsonl", "bidi.jsonl"] {
            for (index, line) in fixture(name).iter().enumerate() {
                decode_line(line)
                    .unwrap_or_else(|error| panic!("{name}:{index}: {error}"));
            }
        }
    }

    #[test]
    fn hooks_are_noise_and_unknown_types_are_ignored() {
        let hook = decode_line(
            r#"{"type":"system","subtype":"hook_started","session_id":"s"}"#,
        )
        .expect("decodes");
        assert!(matches!(hook, Frame::Ignored { .. }));
        let response = decode_line(
            r#"{"type":"system","subtype":"hook_response","session_id":"s"}"#,
        )
        .expect("decodes");
        assert!(matches!(response, Frame::Ignored { .. }));
        let future = decode_line(r#"{"type":"frobnicate","session_id":"s"}"#).expect("decodes");
        assert!(matches!(future, Frame::Ignored { .. }));
        let missing = decode_line(r#"{"session_id":"s"}"#).expect("decodes");
        assert!(matches!(missing, Frame::Ignored { .. }));
    }

    #[test]
    fn non_json_is_the_only_error() {
        assert!(decode_line("not json at all").is_err());
    }
}
