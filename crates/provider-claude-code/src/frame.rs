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
        /// Turn cost in USD (`total_cost_usd`). Real on this provider —
        /// never the muse `0.0` literal (addendum 2026-09-24).
        total_cost_usd: f64,
        /// Wall-clock time in milliseconds (`duration_ms`).
        duration_ms: u64,
        /// Model id, from the first `modelUsage` key, when present.
        model: Option<String>,
        /// After-the-fact denials (`permission_denials[]`): useful for the
        /// transcript, never a substitute for answering the request.
        permission_denials: Vec<PermissionDenial>,
    },
    /// A `control_request` with subtype `can_use_tool`: the child is
    /// suspended on a permission decision and waits silently — no timeout
    /// was observed. The adapter must surface this and answer it; folding
    /// it away hangs the turn forever.
    ControlRequest(ApprovalRequest),
    /// A `control_request` whose subtype this decoder does not know.
    /// Surfaced, never dropped and never panicked on: the probe only ever
    /// saw `can_use_tool`, and the next subtype must be visible when it
    /// arrives. The fold queues these as answerable
    /// ([`crate::fold::UnknownControlRequest`]), so visibility is not
    /// where handling ends.
    ControlUnknown {
        /// The top-level `request_id`, for joining a later answer.
        request_id: String,
        /// The unrecognised `request.subtype`.
        subtype: String,
        /// The raw `request` object, kept for inspection.
        raw: Value,
    },
    /// A child→host `control_response` (e.g. the answer to the host's
    /// `initialize` handshake). Carried so the frame counts balance;
    /// host-initiated, so nothing about it needs answering.
    ControlResponse {
        /// The `response.request_id` this answers.
        request_id: String,
        /// The `response.subtype` (e.g. `success`).
        subtype: String,
    },
    /// Noise or the not-yet-known: hooks, spinner status, token estimates,
    /// turn summaries, and any unknown `type`. Ignored, not fatal.
    Ignored {
        /// Why this frame carries no transcript (e.g. `hook_started`,
        /// `status`, `thinking_tokens`, `unknown type "frobnicate"`).
        note: String,
    },
}

/// One `addRules`-style permission suggestion inside a `can_use_tool`
/// request: the provider-authored "don't ask again" affordance.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionSuggestion {
    /// The suggestion `type` (e.g. `addRules`).
    pub suggestion_type: String,
    /// The suggested behavior (e.g. `allow`).
    pub behavior: String,
    /// Tool names the rule would cover.
    pub tool_names: Vec<String>,
    /// Where the rule would persist (e.g. `localSettings`).
    pub destination: String,
}

/// A typed `can_use_tool` approval request: what the child wants to call,
/// exposed for a human to decide — never answered inside the adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct ApprovalRequest {
    /// The top-level `request_id`: what the `control_response` echoes.
    pub request_id: String,
    /// The tool the child wants (`mcp__baaz__ping`, `Bash`, `Edit`, …).
    pub tool_name: String,
    /// The human label (`Ping`), when the child supplies one.
    pub display_name: String,
    /// The MCP server name, when the tool is an MCP tool.
    pub mcp_server: Option<String>,
    /// The proposed tool input.
    pub input: Value,
    /// The wire tool-use id (`toolu_…`) the result will join to.
    pub tool_use_id: String,
    /// Provider-authored persistence suggestions (`addRules`, …).
    pub suggestions: Vec<PermissionSuggestion>,
}

/// One entry of a `result` frame's `permission_denials[]`: a refusal that
/// already happened, decoded for the transcript.
#[derive(Clone, Debug, PartialEq)]
pub struct PermissionDenial {
    /// The refused tool.
    pub tool_name: String,
    /// The wire tool-use id (`toolu_…`).
    pub tool_use_id: String,
    /// The input that was refused.
    pub tool_input: Value,
}

/// The one-line human summary of an [`ApprovalRequest`]: what is being
/// approved, on the provider seam (`ApprovalRequested` headline,
/// `PendingApproval` headline).
pub fn approval_headline(request: &ApprovalRequest) -> String {
    if request.display_name.is_empty() {
        request.tool_name.clone()
    } else {
        format!("{} ({})", request.display_name, request.tool_name)
    }
}

/// One host→child `control_response` line answering `request_id` with an
/// allow. The `updatedInput` rides along as `{}`: the decision here is the
/// behavior, never a rewritten call.
pub fn encode_control_allow(request_id: &str) -> String {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {"behavior": "allow", "updatedInput": {}},
        },
    })
    .to_string()
}

/// One host→child `control_response` line answering `request_id` with a
/// deny, carrying the human's reason.
pub fn encode_control_deny(request_id: &str, message: &str) -> String {
    serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {"behavior": "deny", "message": message},
        },
    })
    .to_string()
}

impl Frame {
    /// The session this frame belongs to, when it names one.
    /// `rate_limit_event` carries none on the wire, and neither do the
    /// control frames — the caller falls back to the session it opened.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Frame::Init(init) => Some(&init.session_id),
            Frame::Assistant { session_id, .. }
            | Frame::UserResult { session_id, .. }
            | Frame::Stream { session_id, .. }
            | Frame::TurnResult { session_id, .. } => Some(session_id),
            Frame::RateLimit(_)
            | Frame::Ignored { .. }
            | Frame::ControlRequest(_)
            | Frame::ControlUnknown { .. }
            | Frame::ControlResponse { .. } => None,
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

fn decode_denial(item: &Value) -> PermissionDenial {
    PermissionDenial {
        tool_name: item.get("tool_name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        tool_use_id: item
            .get("tool_use_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        tool_input: item.get("tool_input").cloned().unwrap_or(Value::Null),
    }
}

fn decode_suggestion(item: &Value) -> PermissionSuggestion {
    let tool_names = item
        .get("rules")
        .and_then(Value::as_array)
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule.get("toolName").and_then(Value::as_str))
                .map(str::to_owned)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    PermissionSuggestion {
        suggestion_type: item.get("type").and_then(Value::as_str).unwrap_or_default().to_owned(),
        behavior: item.get("behavior").and_then(Value::as_str).unwrap_or_default().to_owned(),
        tool_names,
        destination: item.get("destination").and_then(Value::as_str).unwrap_or_default().to_owned(),
    }
}

fn decode_approval(request_id: &str, request: Option<&Value>) -> ApprovalRequest {
    let get = |key: &str| request.and_then(|request| request.get(key));
    let suggestions = get("permission_suggestions")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(decode_suggestion).collect::<Vec<_>>())
        .unwrap_or_default();
    ApprovalRequest {
        request_id: request_id.to_owned(),
        tool_name: get("tool_name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        display_name: get("display_name").and_then(Value::as_str).unwrap_or_default().to_owned(),
        mcp_server: get("mcp_server")
            .and_then(|server| server.get("name"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        input: get("input").cloned().unwrap_or(Value::Null),
        tool_use_id: get("tool_use_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
        suggestions,
    }
}

fn decode_turn_result(value: &Value) -> Frame {
    let usage = value.get("usage");
    let uint = |key: &str| usage.and_then(|usage| usage.get(key)).and_then(Value::as_u64);
    let model = value
        .get("modelUsage")
        .and_then(Value::as_object)
        .and_then(|usage| usage.keys().next().cloned());
    let permission_denials = value
        .get("permission_denials")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(decode_denial).collect::<Vec<_>>())
        .unwrap_or_default();
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
        permission_denials,
    }
}

/// Decode one CLI stdout line. Pure and IO-free.
///
/// Unknown `type` values, unknown `system` subtypes, and unknown assistant
/// content kinds decode to [`Frame::Ignored`] (or [`ContentBlock::Other`]);
/// only non-JSON input is an error. `control_request` decodes to
/// [`Frame::ControlRequest`] for `can_use_tool` and to
/// [`Frame::ControlUnknown`] for any other subtype — never `Ignored`, so an
/// unanswered decision cannot hide inside the noise.
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
        "control_request" => {
            let request_id =
                value.get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned();
            let request = value.get("request");
            let subtype =
                request.and_then(|request| request.get("subtype")).and_then(Value::as_str).unwrap_or("");
            match subtype {
                // The one subtype Baaz answers: a permission decision the
                // child waits on.
                "can_use_tool" => Ok(Frame::ControlRequest(decode_approval(&request_id, request))),
                // Any other subtype is carried, not dropped and never
                // panicked on — the next one must be visible when it lands.
                other => Ok(Frame::ControlUnknown {
                    request_id,
                    subtype: other.to_owned(),
                    raw: request.cloned().unwrap_or(Value::Null),
                }),
            }
        }
        "control_response" => {
            let response = value.get("response");
            let get = |key: &str| response.and_then(|response| response.get(key));
            Ok(Frame::ControlResponse {
                request_id: get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                subtype: get("subtype").and_then(Value::as_str).unwrap_or_default().to_owned(),
            })
        }
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
        // permission.jsonl is bidirectional: host->cli lines carry an
        // envelope, so the inner frame is what decodes.
        for (index, line) in fixture("permission.jsonl").iter().enumerate() {
            let value: Value = serde_json::from_str(line).expect("fixture is JSON");
            let owned;
            let frame_line = match value.get("_dir").and_then(Value::as_str) {
                Some("host->cli") => {
                    owned = value.get("frame").expect("envelope carries a frame").to_string();
                    owned.as_str()
                }
                _ => line.as_str(),
            };
            decode_line(frame_line)
                .unwrap_or_else(|error| panic!("permission.jsonl:{index}: {error}"));
        }
    }

    fn permission_fixture() -> Vec<Value> {
        fixture("permission.jsonl")
            .iter()
            .map(|line| serde_json::from_str(line).expect("fixture is JSON"))
            .collect()
    }

    /// The probe's payoff: the `can_use_tool` request decodes with every
    /// field the seam needs — tool, label, input, tool-use id, suggestions.
    #[test]
    fn can_use_tool_decodes_with_the_whole_request() {
        let lines = permission_fixture();
        let request = lines
            .iter()
            .filter(|line| line.get("_dir").is_none())
            .map(|line| decode_line(&line.to_string()).expect("child line decodes"))
            .find_map(|frame| match frame {
                Frame::ControlRequest(request) => Some(request),
                _ => None,
            })
            .expect("one can_use_tool request in the fixture");
        assert_eq!(request.request_id, "b4554ab2-30c2-4271-8451-dd9d6f5e226d");
        assert_eq!(request.tool_name, "mcp__baaz__ping");
        assert_eq!(request.display_name, "Ping");
        assert_eq!(request.input, serde_json::json!({}));
        assert_eq!(request.tool_use_id, "toolu_01CPKoR3sS6ZvHfqgQtJWZU5");
        assert_eq!(request.mcp_server.as_deref(), Some("baaz"));
        assert_eq!(request.suggestions.len(), 1);
        let suggestion = &request.suggestions[0];
        assert_eq!(suggestion.suggestion_type, "addRules");
        assert_eq!(suggestion.behavior, "allow");
        assert_eq!(suggestion.tool_names, ["mcp__baaz__ping"]);
        assert_eq!(suggestion.destination, "localSettings");
        assert_eq!(approval_headline(&request), "Ping (mcp__baaz__ping)");
    }

    /// The allow answer the test's explicit decision produces must match the
    /// human's answer on the wire: same request id, same behavior.
    #[test]
    fn allow_answer_matches_the_fixture_host_line() {
        let lines = permission_fixture();
        let host_answer = lines
            .iter()
            .filter_map(|line| line.get("frame"))
            .find(|frame| {
                frame
                    .get("response")
                    .and_then(|response| response.get("request_id"))
                    .and_then(Value::as_str)
                    == Some("b4554ab2-30c2-4271-8451-dd9d6f5e226d")
            })
            .expect("the human answered the request in the fixture");
        let written: Value =
            serde_json::from_str(&encode_control_allow("b4554ab2-30c2-4271-8451-dd9d6f5e226d"))
                .expect("encodes JSON");
        assert_eq!(&written, host_answer);
    }

    #[test]
    fn deny_answer_carries_the_human_reason() {
        let value: Value =
            serde_json::from_str(&encode_control_deny("req-7", "not now")).expect("encodes JSON");
        assert_eq!(
            value
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(Value::as_str),
            Some("deny")
        );
        assert_eq!(
            value
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("message"))
                .and_then(Value::as_str),
            Some("not now")
        );
        assert_eq!(
            value
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(Value::as_str),
            Some("req-7")
        );
    }

    /// `can_use_tool` is the one subtype handled; anything else is carried,
    /// not dropped and never panicked on — including the host's own
    /// `initialize` handshake in the fixture.
    #[test]
    fn unknown_control_subtypes_are_surfaced() {
        let lines = permission_fixture();
        let host_init = lines
            .iter()
            .filter_map(|line| line.get("frame"))
            .find(|frame| {
                frame
                    .get("request")
                    .and_then(|request| request.get("subtype"))
                    .and_then(Value::as_str)
                    == Some("initialize")
            })
            .expect("the initialize handshake is in the fixture");
        match decode_line(&host_init.to_string()).expect("decodes") {
            Frame::ControlUnknown { request_id, subtype, .. } => {
                assert_eq!(request_id, "req_init_1");
                assert_eq!(subtype, "initialize");
            }
            other => panic!("initialize must surface as unknown, got {other:?}"),
        }
        match decode_line(
            r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#,
        )
        .expect("decodes")
        {
            Frame::ControlUnknown { request_id, subtype, .. } => {
                assert_eq!(request_id, "req-x");
                assert_eq!(subtype, "frobnicate");
            }
            other => panic!("future subtypes must surface, got {other:?}"),
        }
    }

    /// Unlike muse, this provider's `result` frame carries a real cost and a
    /// structured denial list: read both, never the `0.0` literal.
    #[test]
    fn result_carries_real_cost_and_denials() {
        let lines = permission_fixture();
        let result = lines
            .iter()
            .filter(|line| line.get("_dir").is_none())
            .map(|line| decode_line(&line.to_string()).expect("child line decodes"))
            .find_map(|frame| match frame {
                Frame::TurnResult {
                    total_cost_usd,
                    model,
                    permission_denials,
                    text,
                    ..
                } => Some((total_cost_usd, model, permission_denials, text)),
                _ => None,
            })
            .expect("one result frame in the fixture");
        assert!((result.0 - 0.0188967).abs() < 1e-9, "real cost, not 0.0: {}", result.0);
        assert_eq!(result.1.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert!(result.2.is_empty(), "nothing was denied in the fixture");
        assert_eq!(result.3, "PONG");
        // And a denial decodes against real bytes: the deny fixture's
        // `result` frame carries the refusal the owner actually saw. A
        // hand-written literal here would pass whether or not its field
        // names match the wire, so it pins nothing.
        let denied = fixture("permission-deny.jsonl")
            .iter()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|line| line.get("_dir").is_none())
            .map(|line| decode_line(&line.to_string()).expect("child line decodes"))
            .find_map(|frame| match frame {
                Frame::TurnResult { permission_denials, total_cost_usd, .. } => {
                    Some((permission_denials, total_cost_usd))
                }
                _ => None,
            })
            .expect("one denial-carrying result in the deny fixture");
        assert!((denied.1 - 0.0696295).abs() < 1e-9, "real cost, not 0.0: {}", denied.1);
        assert_eq!(
            denied.0,
            vec![PermissionDenial {
                tool_name: "mcp__baaz__ping".into(),
                tool_use_id: "toolu_01XqHeZeKksDmhf4miPM8P5C".into(),
                tool_input: serde_json::json!({}),
            }]
        );
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
