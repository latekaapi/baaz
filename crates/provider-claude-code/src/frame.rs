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
    /// The frame-level `tool_use_result` payload, when the frame carries
    /// exactly one result so the detail unambiguously belongs to it: the
    /// structured diff for Edit/Write, the file for Read, the streams for
    /// Bash. `None` on multi-result frames, where one detail cannot be
    /// dealt to several results without guessing.
    pub detail: Option<serde_json::Value>,
}

/// One image part of a `user` text echo (`--replay-user-messages`): what
/// the person attached, echoed back without a path — the wire carries the
/// bytes' media type and length, never a filename.
#[derive(Clone, Debug, PartialEq)]
pub struct UserImage {
    /// The part's media type, e.g. `image/png`.
    pub media_type: String,
    /// Length of the base64 payload in bytes (the chip's size hint).
    pub data_len: usize,
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

/// Token usage inside a stored `assistant` line's message: the same keys
/// the stream `result` frame carries (`input_tokens`,
/// `cache_creation_input_tokens`, `cache_read_input_tokens`,
/// `output_tokens`, `output_tokens_details.thinking_tokens`). The stored
/// transcript carries no `result` frame, so this is what closes a replayed
/// turn's footer.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AssistantUsage {
    /// Billed input tokens (`usage.input_tokens`).
    pub input_tokens: u64,
    /// Billed completion tokens (`usage.output_tokens`).
    pub output_tokens: u64,
    /// Cache-read tokens, when reported.
    pub cache_read_tokens: Option<u64>,
    /// Cache-creation tokens, when reported.
    pub cache_write_tokens: Option<u64>,
    /// Reasoning tokens, when reported.
    pub reasoning_tokens: u64,
}

/// One row of the `initialize` answer's model catalog
/// (`response.response.models[]`, recorded in
/// `fixtures/claude-code/permission.jsonl`): what `--model` takes as
/// `value`, resolved to the full id the stream reports, with the human
/// label and the effort levels the row supports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogModel {
    /// What `--model` (and `set_model`) carries: an alias (`sonnet`),
    /// a bracketed id (`opus[1m]`, `claude-fable-5-1[1m]`) or `default`.
    pub value: String,
    /// The full id the stream reports for this row (`init`'s `model` and
    /// the assistant messages), when the answer names one.
    pub resolved_model: Option<String>,
    /// The human label the picker shows (`displayName`).
    pub display_name: String,
    /// The answer's one-line description, when it sends one.
    pub description: Option<String>,
    /// The effort levels this row supports (`supportedEffortLevels`):
    /// empty when the answer names none (Haiku carries no effort fields).
    pub efforts: Vec<String>,
}

/// Normalize a Claude Code model id for catalog matching: lowercase, strip
/// a trailing `[...]` context suffix (`opus[1m]`, `claude-opus-5[1m]`),
/// strip a trailing `-YYYYMMDD` build-date stamp
/// (`claude-haiku-4-5-20251001`), strip a leading `claude-`. The stream
/// reports resolved full ids (`init`'s `model`) while the catalog keys on
/// `value` aliases, so both sides normalize before comparing; callers match
/// on the normalized form first, then on the family (the first `-`
/// segment), so `claude-opus-5[1m]` finds an `opus[1m]` row.
pub fn normalize_model_id(id: &str) -> String {
    let lower = id.to_lowercase();
    let no_context = match lower.strip_suffix(']') {
        Some(inner) => match inner.split_once('[') {
            Some((base, _)) => base.to_owned(),
            None => lower.clone(),
        },
        None => lower,
    };
    let no_date = if no_context.len() > 9 {
        let (head, tail) = no_context.split_at(no_context.len() - 8);
        if tail.bytes().all(|byte| byte.is_ascii_digit()) && head.ends_with('-') {
            head[..head.len() - 1].to_owned()
        } else {
            no_context
        }
    } else {
        no_context
    };
    no_date.strip_prefix("claude-").unwrap_or(&no_date).to_owned()
}

/// The catalog rows out of an `initialize` answer's `response.response`:
/// one [`CatalogModel`] per `models[]` entry, in answer order. Rows
/// without a `value` are skipped, never fabricated; a missing or
/// misshapen `models[]` is an empty catalog, not an error.
pub fn decode_catalog_models(response: &Value) -> Vec<CatalogModel> {
    response
        .get("models")
        .and_then(Value::as_array)
        .map(|models| {
            models
                .iter()
                .filter_map(|row| {
                    let value = row.get("value").and_then(Value::as_str)?;
                    if value.is_empty() {
                        return None;
                    }
                    Some(CatalogModel {
                        value: value.to_owned(),
                        resolved_model: row
                            .get("resolvedModel")
                            .and_then(Value::as_str)
                            .filter(|model| !model.is_empty())
                            .map(str::to_owned),
                        display_name: row
                            .get("displayName")
                            .and_then(Value::as_str)
                            .filter(|label| !label.is_empty())
                            .unwrap_or(value)
                            .to_owned(),
                        description: row
                            .get("description")
                            .and_then(Value::as_str)
                            .filter(|description| !description.is_empty())
                            .map(str::to_owned),
                        efforts: row
                            .get("supportedEffortLevels")
                            .and_then(Value::as_array)
                            .map(|levels| {
                                levels
                                    .iter()
                                    .filter_map(Value::as_str)
                                    .filter(|level| !level.is_empty())
                                    .map(str::to_owned)
                                    .collect()
                            })
                            .unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
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
        /// The model (`message.model`), present on stored lines only: the
        /// stream carries the model on `init` instead.
        model: Option<String>,
        /// The usage (`message.usage`), present on stored lines only: the
        /// stored transcript carries no `result` frame, so this closes a
        /// replayed turn's footer.
        usage: Option<AssistantUsage>,
        /// The enclosing tool call when this message comes from a
        /// sub-agent (`Agent`/`Task`): the parent `tool_use` id whose card
        /// nests these blocks. Empty on the main thread.
        parent_tool_use_id: String,
        /// B12: the line's `timestamp` (RFC3339) as Unix milliseconds, for
        /// thinking durations on replay. `None` on lines without one
        /// (every live stream line) — unknown, never the epoch.
        at_ms: Option<u64>,
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
        /// The enclosing tool call when this result comes from a
        /// sub-agent: the parent `tool_use` id. Empty on the main thread.
        parent_tool_use_id: String,
    },
    /// A `user` frame echoing the submitted prompt
    /// (`--replay-user-messages`): the person's own text, which the
    /// transcript renders as their bubble. The pre-replay CLI never sends
    /// this — no echo, no bubble — which is exactly the defect that hid
    /// every Claude Code prompt.
    UserText {
        /// Session this belongs to.
        session_id: String,
        /// The frame's own uuid, for exactly-once counting.
        uuid: String,
        /// The echoed prompt text (joined text parts).
        text: String,
        /// The echoed image parts, in order.
        images: Vec<UserImage>,
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
        /// The model that answered the turn: the frame's own `model` when it
        /// names one, else the `modelUsage` entry with the most output
        /// tokens — never a Haiku entry while a non-Haiku entry exists,
        /// since Haiku rides along for the CLI's own internal sub-calls.
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
    /// host-initiated, so nothing about it needs answering. An `initialize`
    /// answer carries the model catalog (`response.response.models[]`),
    /// which the adapter keeps for `ListModels` once it confirms the reply
    /// matches its own `initialize` request id. A rejection carries the
    /// CLI's reason in `error` (see [`decode_control_error`]).
    ControlResponse {
        /// The `response.request_id` this answers.
        request_id: String,
        /// The `response.subtype` (`"success"` or `"error"`).
        subtype: String,
        /// The catalog rows, when this answer carries them: `initialize`
        /// only, empty otherwise.
        models: Vec<CatalogModel>,
        /// The CLI's rejection text, when this answer refuses: what the
        /// session banner shows. `None` on success.
        error: Option<String>,
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
    /// The child's own short description (`description`), e.g. a path —
    /// the human handle when the input is machine-shaped.
    pub description: String,
    /// Why the child is asking (`decision_reason`), e.g. which policy
    /// tripped — the card's reason when nothing else says more.
    pub decision_reason: String,
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
            | Frame::UserText { session_id, .. }
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

/// The session a line belongs to: the stream spells `session_id`, the
/// stored transcript `sessionId` (captured live 2026-09-26,
/// `fixtures/claude-code/stored-history.jsonl`).
fn session_str(value: &Value) -> String {
    value
        .get("session_id")
        .or_else(|| value.get("sessionId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The enclosing tool call (`Agent`/`Task`): `parent_tool_use_id`, empty
/// on the main thread. Stored lines carry `parentUuid` instead, but that
/// is the message chain (user → attachments → assistant), not a tool-use
/// link — mapping it here routed every stored answer into a sub-agent
/// card (captured live 2026-09-26, `stored-history.jsonl`: the whole file
/// is `isSidechain: false`). So this reads the stream key only; stored
/// sub-agent threads render inline, unverified.
fn parent_str(value: &Value) -> String {
    value
        .get("parent_tool_use_id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The usage inside a stored `assistant` line's message, under the same
/// keys the stream `result` frame carries. `None` when the message
/// carries no usage object at all (every stream line).
fn assistant_usage(message: Option<&Value>) -> Option<AssistantUsage> {
    let usage = message?.get("usage")?.as_object()?;
    let uint = |key: &str| usage.get(key).and_then(Value::as_u64);
    Some(AssistantUsage {
        input_tokens: uint("input_tokens").unwrap_or(0),
        output_tokens: uint("output_tokens").unwrap_or(0),
        cache_read_tokens: uint("cache_read_input_tokens"),
        cache_write_tokens: uint("cache_creation_input_tokens"),
        reasoning_tokens: usage
            .get("output_tokens_details")
            .and_then(|details| details.get("thinking_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn decode_assistant(value: &Value) -> Frame {
    let session_id = session_str(value);
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
        parent_tool_use_id: parent_str(value),
        model: message
            .and_then(|message| message.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        usage: assistant_usage(message),
        at_ms: timestamp_ms(value),
    }
}

/// B12: Unix milliseconds from a line's `timestamp` (RFC3339), for
/// thinking durations on replay. `None` when absent or unparseable —
/// unknown, never the epoch. Parsed by hand (no date dependency on this
/// lane): `YYYY-MM-DDTHH:MM:SS[.frac][Z|±HH:MM]`.
pub fn timestamp_ms(value: &Value) -> Option<u64> {
    value.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_ms)
}

/// Days from civil date (Howard Hinnant's algorithm), for
/// [`parse_rfc3339_ms`]: days since 1970-01-01.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Two ASCII digits as a number, for [`parse_rfc3339_ms`].
fn two_digits(bytes: &[u8]) -> Option<i64> {
    if bytes.len() == 2 && bytes.iter().all(|byte| byte.is_ascii_digit()) {
        Some(((bytes[0] - b'0') * 10 + (bytes[1] - b'0')) as i64)
    } else {
        None
    }
}

fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20 {
        return None;
    }
    if bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }
    let year = text[0..4].parse::<i64>().ok()?;
    let month = two_digits(&bytes[5..7])?;
    let day = two_digits(&bytes[8..10])?;
    let hour = two_digits(&bytes[11..13])?;
    let minute = two_digits(&bytes[14..16])?;
    let second = two_digits(&bytes[17..19])?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    // Byte 19 is the zone when there is no fraction (`...:00Z`), or the
    // fraction's dot when there is one (`...:14.384Z`).
    let mut rest = &bytes[19..];
    // Optional fractional seconds.
    let mut millis = 0i64;
    if rest.first() == Some(&b'.') {
        let digits: Vec<u8> = rest[1..].iter().copied().take_while(u8::is_ascii_digit).collect();
        if digits.is_empty() || digits.len() > 9 {
            return None;
        }
        let mut nanos = 0i64;
        for digit in digits.iter() {
            nanos = nanos * 10 + i64::from(digit - b'0');
        }
        for _ in digits.len()..9 {
            nanos *= 10;
        }
        millis = nanos.div_euclid(1_000_000);
        rest = &rest[1 + digits.len()..];
    }
    // Zone: `Z` or `±HH:MM`.
    let offset_seconds = if rest == b"Z" {
        0
    } else if rest.len() == 6 && (rest[0] == b'+' || rest[0] == b'-') && rest[3] == b':' {
        let hours = two_digits(&rest[1..3])?;
        let minutes = two_digits(&rest[4..6])?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let sign = if rest[0] == b'+' { 1 } else { -1 };
        sign * (hours * 3600 + minutes * 60)
    } else {
        return None;
    };
    let days = days_from_civil(year, month, day);
    let stamp = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    u64::try_from(stamp * 1000 + millis).ok()
}

fn decode_user(value: &Value) -> Frame {
    let session_id = session_str(value);
    let uuid = value.get("uuid").and_then(Value::as_str).unwrap_or_default();
    let raw_detail = value.get("tool_use_result").cloned();
    let mut results = Vec::new();
    let mut text = String::new();
    let mut images = Vec::new();
    if let Some(items) =
        value.get("message").and_then(|message| message.get("content")).and_then(Value::as_array)
    {
        for item in items {
            match item.get("type").and_then(Value::as_str) {
                Some("tool_result") => {
                    results.push(ToolResult {
                        tool_use_id: item
                            .get("tool_use_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        text: text_parts(item.get("content").unwrap_or(&Value::Null)),
                        is_error: item.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        // Set below, once the frame's result count is known.
                        detail: None,
                    });
                }
                Some("text") => {
                    text.push_str(item.get("text").and_then(Value::as_str).unwrap_or_default());
                }
                Some("image") => {
                    let source = item.get("source").unwrap_or(&Value::Null);
                    images.push(UserImage {
                        media_type: source
                            .get("media_type")
                            .and_then(Value::as_str)
                            .unwrap_or("image/png")
                            .to_owned(),
                        data_len: source
                            .get("data")
                            .and_then(Value::as_str)
                            .map(str::len)
                            .unwrap_or(0),
                    });
                }
                _ => {}
            }
        }
    }
    let parent_tool_use_id = parent_str(value);
    // B12: a skill body the CLI replays as a `user` line (`isMeta: true`,
    // the SKILL.md text keyed by its tool use): never the person's bubble
    // and never a turn boundary — the `Skill` tool's own "Loaded skill"
    // row already says it. It folds to an empty result, which the fold
    // drops without a bubble or a split. A line that also carries tool
    // results keeps its results: suppression never eats data.
    if value.get("isMeta").and_then(Value::as_bool).unwrap_or(false) && results.is_empty() {
        return Frame::UserResult {
            session_id: session_id.clone(),
            results: Vec::new(),
            raw_detail,
            parent_tool_use_id,
        };
    }
    if results.is_empty() {
        // No tool result: this is the `--replay-user-messages` echo of the
        // submitted prompt (or an empty frame) — the person's bubble.
        if text.is_empty() && images.is_empty() {
            return Frame::UserResult {
                session_id: session_id.to_owned(),
                results,
                raw_detail,
                parent_tool_use_id,
            };
        }
        return Frame::UserText {
            session_id: session_id.to_owned(),
            uuid: uuid.to_owned(),
            text,
            images,
        };
    }
    // One frame-level detail belongs to exactly one result; on
    // multi-result frames no arm deals it out, or parallel calls would
    // wear each other's diffs.
    if results.len() == 1 {
        results[0].detail = raw_detail.clone();
    }
    Frame::UserResult {
        session_id: session_id.to_owned(),
        results,
        raw_detail,
        parent_tool_use_id,
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
        description: get("description").and_then(Value::as_str).unwrap_or_default().to_owned(),
        decision_reason: get("decision_reason").and_then(Value::as_str).unwrap_or_default().to_owned(),
        tool_use_id: get("tool_use_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
        suggestions,
    }
}

/// Whether this model id names the Haiku family: the CLI runs its own
/// internal sub-calls on Haiku and lists them in `modelUsage` beside the
/// answering model, so a Haiku entry must never claim the turn while a
/// non-Haiku entry exists.
fn is_haiku_model(id: &str) -> bool {
    id.to_ascii_lowercase().contains("haiku")
}

/// Whether `id` can name a model at all: non-empty and not a `<...>`
/// placeholder such as the `<synthetic>` the CLI emits on
/// error/synthetic messages. Placeholders never claim the turn; the next
/// source answers instead.
pub fn is_model_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && !id.starts_with('<')
}

/// The turn's answering model out of a `result` frame: the frame's own
/// `model` when it names a real model id, else the `modelUsage` entry
/// with the most output tokens — never a Haiku entry while a non-Haiku
/// entry exists. Placeholder ids (`<synthetic>`, anything starting with
/// `<`) are skipped at both steps, falling back to the next source.
/// Wire order breaks output-token ties. `None` when the frame names no
/// model at all.
fn decode_result_model(value: &Value) -> Option<String> {
    if let Some(model) = value.get("model").and_then(Value::as_str).filter(|model| is_model_id(model))
    {
        return Some(model.trim().to_owned());
    }
    let usage = value.get("modelUsage")?.as_object()?;
    let output_tokens = |entry: &serde_json::Map<String, Value>| {
        entry
            .get("outputTokens")
            .or_else(|| entry.get("output_tokens"))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    };
    let mut best: Option<(&String, u64)> = None;
    let mut best_main: Option<(&String, u64)> = None;
    for (id, entry) in usage {
        if !is_model_id(id) {
            continue;
        }
        let tokens = entry.as_object().map(output_tokens).unwrap_or(0);
        if best.map_or(true, |(_, leader)| tokens > leader) {
            best = Some((id, tokens));
        }
        if !is_haiku_model(id) && best_main.map_or(true, |(_, leader)| tokens > leader) {
            best_main = Some((id, tokens));
        }
    }
    best_main.or(best).map(|(id, _)| id.clone())
}

fn decode_turn_result(value: &Value) -> Frame {
    let usage = value.get("usage");
    let uint = |key: &str| usage.and_then(|usage| usage.get(key)).and_then(Value::as_u64);
    let model = decode_result_model(value);
    let permission_denials = value
        .get("permission_denials")
        .and_then(Value::as_array)
        .map(|items| items.iter().map(decode_denial).collect::<Vec<_>>())
        .unwrap_or_default();
    Frame::TurnResult {
        session_id: session_str(value),
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

/// The CLI's rejection text out of a `control_response`'s `response`
/// object: the first non-empty string among `error`,
/// `response.error` and `response.message`, in that order. The first
/// placement is the probed one — a live `set_model` to a bogus id answers
/// `{"subtype":"error","request_id":"…","error":"Model '…' not found"}`
/// (probed 2026-09-28, free control probe, no turn spent) — and the other
/// two are defensive reads of the success shape's nestings. `None` when no
/// placement names a reason — a bare `{"subtype":"error"}` still counts as
/// a refusal, with a fallback reason minted at the confirmation site,
/// never here.
pub fn decode_control_error(response: Option<&Value>) -> Option<String> {
    let response = response?;
    let text = |value: Option<&Value>| {
        value.and_then(Value::as_str).filter(|text| !text.trim().is_empty()).map(str::to_owned)
    };
    text(response.get("error"))
        .or_else(|| text(response.get("response").and_then(|inner| inner.get("error"))))
        .or_else(|| text(response.get("response").and_then(|inner| inner.get("message"))))
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
            let models = response
                .and_then(|response| response.get("response"))
                .map(decode_catalog_models)
                .unwrap_or_default();
            Ok(Frame::ControlResponse {
                request_id: get("request_id").and_then(Value::as_str).unwrap_or_default().to_owned(),
                subtype: get("subtype").and_then(Value::as_str).unwrap_or_default().to_owned(),
                models,
                error: decode_control_error(response),
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
        for name in [
            "basic.jsonl",
            "partial.jsonl",
            "mcp.jsonl",
            "resume.jsonl",
            "resume-replay.jsonl",
            "bidi.jsonl",
            "edit.jsonl",
            "read-search.jsonl",
            "thinking.jsonl",
            "todo.jsonl",
            "subagent.jsonl",
            "approval-default.jsonl",
            "error.jsonl",
            "image.jsonl",
            "stored-history.jsonl",
        ] {
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

    /// Q2: a result whose `modelUsage` lists the Haiku sub-call first and
    /// the answering model second decodes to the answering model — even
    /// when the sub-call wrote more tokens. The frame's own `model` still
    /// wins when it names one, and a lone Haiku entry still reads Haiku:
    /// a real Haiku turn is Haiku.
    #[test]
    fn result_names_the_answering_model_not_the_haiku_subcall() {
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"done","usage":{"input_tokens":10,"output_tokens":510},"total_cost_usd":0.01,"duration_ms":100,"model":null,"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":5,"outputTokens":500},"claude-opus-4-1-20250822":{"inputTokens":5,"outputTokens":10}}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-opus-4-1-20250822"));
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
        }
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"done","usage":{"input_tokens":10,"output_tokens":10},"total_cost_usd":0.01,"duration_ms":100,"model":"claude-opus-4-1-20250822","modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":5,"outputTokens":500}}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-opus-4-1-20250822"));
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
        }
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"done","usage":{"input_tokens":10,"output_tokens":10},"total_cost_usd":0.01,"duration_ms":100,"model":null,"modelUsage":{"claude-haiku-4-5-20251001":{"inputTokens":5,"outputTokens":10}}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-haiku-4-5-20251001"));
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
        }
    }

    /// Q2b: a `<...>` placeholder never claims the turn — the frame's own
    /// `<synthetic>` model falls back to `modelUsage`, a placeholder
    /// `modelUsage` key falls back to the next entry, and placeholders
    /// everywhere decode to no model at all.
    #[test]
    fn result_placeholders_fall_back_to_the_next_source() {
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"done","usage":{"input_tokens":10,"output_tokens":10},"total_cost_usd":0.01,"duration_ms":100,"model":"<synthetic>","modelUsage":{"claude-opus-4-1-20250822":{"inputTokens":5,"outputTokens":10}}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-opus-4-1-20250822"));
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
        }
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"done","usage":{"input_tokens":10,"output_tokens":10},"total_cost_usd":0.01,"duration_ms":100,"model":null,"modelUsage":{"<synthetic>":{"inputTokens":5,"outputTokens":500},"claude-opus-4-1-20250822":{"inputTokens":5,"outputTokens":10}}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model.as_deref(), Some("claude-opus-4-1-20250822"));
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
        }
        let frame = decode_line(
            r#"{"type":"result","session_id":"s","result":"oops","usage":{"input_tokens":10,"output_tokens":0},"total_cost_usd":0.0,"duration_ms":100,"model":"<synthetic>"}"#,
        )
        .expect("decodes");
        match frame {
            Frame::TurnResult { model, .. } => {
                assert_eq!(model, None);
            }
            other => panic!("a result decodes to TurnResult, got {other:?}"),
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

    #[test]
    fn the_initialize_answer_carries_the_model_catalog() {
        // `fixtures/claude-code/permission.jsonl` already holds an
        // `initialize` answer with five `models[]` rows: the `default`
        // alias, `opus[1m]`, `claude-fable-5-1[1m]` ("Fable"),
        // `sonnet`, and `haiku` — which carries no effort fields at all.
        // A decoder that drops the payload serves an empty `ListModels`.
        let lines = permission_fixture();
        let frame = lines
            .iter()
            .filter(|line| line.get("_dir").is_none())
            .map(|line| decode_line(&line.to_string()).expect("child line decodes"))
            .find_map(|frame| match frame {
                Frame::ControlResponse { models, .. } if !models.is_empty() => Some(models),
                _ => None,
            })
            .expect("one catalog-carrying control_response in the fixture");
        let values: Vec<&str> = frame.iter().map(|row| row.value.as_str()).collect();
        assert_eq!(
            values,
            ["default", "opus[1m]", "claude-fable-5-1[1m]", "sonnet", "haiku"],
            "every row, in answer order: {values:?}"
        );
        let fable = frame.iter().find(|row| row.value == "claude-fable-5-1[1m]").expect("fable");
        assert_eq!(fable.display_name, "Fable");
        assert_eq!(fable.resolved_model.as_deref(), Some("claude-fable-5-1"));
        assert_eq!(fable.efforts, ["low", "medium", "high", "xhigh", "max"]);
        let sonnet = frame.iter().find(|row| row.value == "sonnet").expect("sonnet");
        assert_eq!(sonnet.display_name, "Sonnet");
        assert_eq!(sonnet.resolved_model.as_deref(), Some("claude-sonnet-5"));
        assert!(!sonnet.efforts.is_empty(), "sonnet takes an effort");
        let haiku = frame.iter().find(|row| row.value == "haiku").expect("haiku");
        assert_eq!(haiku.resolved_model.as_deref(), Some("claude-haiku-4-5-20251001"));
        assert!(
            haiku.efforts.is_empty(),
            "haiku names no effort levels: the menu must say so, not list the flag's"
        );
        // And a response without a payload still decodes: other control
        // answers carry no catalog.
        let bare = decode_line(
            r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-x"}}"#,
        )
        .expect("decodes");
        assert!(
            matches!(bare, Frame::ControlResponse { ref models, .. } if models.is_empty()),
            "no payload, no rows: {bare:?}"
        );
    }

    /// A rejection decodes with the CLI's reason and no catalog rows: the
    /// confirmation site rolls back on the subtype, and banners the error.
    #[test]
    fn control_error_decodes_with_the_cli_reason() {
        // The probed rejection shape (live CLI, bogus `set_model`):
        // `{"subtype":"error","request_id":"…","error":"Model '…' not found"}`.
        let frame = decode_line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"baaz-ctl-3","error":"Model 'bogus-model-xyz-123' not found"}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::ControlResponse { request_id, subtype, models, error } => {
                assert_eq!(request_id, "baaz-ctl-3");
                assert_eq!(subtype, "error");
                assert!(models.is_empty(), "a refusal carries no catalog: {models:?}");
                assert_eq!(error.as_deref(), Some("Model 'bogus-model-xyz-123' not found"));
            }
            other => panic!("expected ControlResponse, got {other:?}"),
        }
        // The nested placement reads too: the probe has not named the
        // shape yet, so every suggested placement decodes.
        let nested = decode_line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"r","response":{"error":"nested reason"}}}"#,
        )
        .expect("decodes");
        assert!(
            matches!(nested, Frame::ControlResponse { error: Some(_), .. }),
            "nested error reads: {nested:?}"
        );
        // A bare refusal still refuses: the reason falls back at the
        // confirmation site, never here.
        let bare = decode_line(
            r#"{"type":"control_response","response":{"subtype":"error","request_id":"r"}}"#,
        )
        .expect("decodes");
        assert!(
            matches!(bare, Frame::ControlResponse { error: None, .. }),
            "no reason named, still an error subtype: {bare:?}"
        );
    }

    fn child_lines(name: &str) -> Vec<String> {
        // Child frames only: `host->cli` envelopes carry the submission
        // and answers, which decode as noise (`Ignored`) rather than
        // frames — the fold tests rely on the same split.
        fixture(name)
            .into_iter()
            .filter(|line| {
                serde_json::from_str::<Value>(line)
                    .ok()
                    .and_then(|value| value.get("_dir").and_then(Value::as_str).map(str::to_owned))
                    .is_none()
            })
            .collect()
    }

    /// The `--replay-user-messages` echo decodes to the person's own
    /// text: the bubble the pre-replay CLI never sent (defect 1).
    #[test]
    fn user_echo_decodes_to_user_text() {
        let echoes: Vec<Frame> = child_lines("edit.jsonl")
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .filter(|frame| matches!(frame, Frame::UserText { .. }))
            .collect();
        assert_eq!(echoes.len(), 1, "one submitted prompt, one echo");
        match &echoes[0] {
            Frame::UserText { uuid, text, images, .. } => {
                assert!(!uuid.is_empty(), "the echo carries its uuid");
                assert!(text.contains("greet.txt"), "the prompt text: {text}");
                assert!(images.is_empty(), "no images on this turn");
            }
            other => panic!("expected UserText, got {other:?}"),
        }
        // And the pre-replay captures genuinely have no echo: `basic`
        // was recorded before the flag, so no bubble can fold from it.
        let old: Vec<Frame> = child_lines("basic.jsonl")
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .filter(|frame| matches!(frame, Frame::UserText { .. }))
            .collect();
        assert!(old.is_empty(), "pre-replay captures echo nothing");
    }

    /// The image turn echoes its image part with media type and length —
    /// the attachment chip's whole input (never a filename: the wire
    /// sends none).
    #[test]
    fn image_echo_carries_image_part() {
        let echoes: Vec<Frame> = child_lines("image.jsonl")
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .filter(|frame| matches!(frame, Frame::UserText { .. }))
            .collect();
        assert_eq!(echoes.len(), 1);
        match &echoes[0] {
            Frame::UserText { text, images, .. } => {
                assert!(text.contains("three words"), "the prompt text: {text}");
                assert_eq!(images.len(), 1);
                assert_eq!(images[0].media_type, "image/png");
                assert!(images[0].data_len > 0, "bytes were attached");
            }
            other => panic!("expected UserText, got {other:?}"),
        }
    }

    /// Tool results carry their frame's structured detail 1:1 — the
    /// Edit's `structuredPatch`, the Write's created content — so the
    /// fold can chip the diff. Multi-result frames deal nothing out.
    #[test]
    fn tool_results_carry_detail_1_to_1() {
        let results: Vec<ToolResult> = child_lines("edit.jsonl")
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .filter_map(|frame| match frame {
                Frame::UserResult { results, .. } => Some(results),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(results.len(), 2, "Write result plus Edit result");
        let write = results.iter().find(|result| {
            result.detail.as_ref().and_then(|detail| detail.get("type")).and_then(Value::as_str)
                == Some("create")
        }).expect("the Write result carries its created content");
        assert!(write.text.contains("created successfully"));
        let edit = results.iter().find(|result| {
            result
                .detail
                .as_ref()
                .and_then(|detail| detail.get("structuredPatch"))
                .and_then(serde_json::Value::as_array)
                .map(|patch| !patch.is_empty())
                .unwrap_or(false)
        }).expect("the Edit result carries its structured patch");
        assert!(edit.text.contains("updated successfully"));
    }

    /// Sub-agent frames arrive linked: assistant frames and tool results
    /// with `parent_tool_use_id` set route into the `Agent` card, while
    /// main-thread frames leave it empty.
    #[test]
    fn subagent_frames_carry_parent_link() {
        let frames: Vec<Frame> = child_lines("subagent.jsonl")
            .iter()
            .filter_map(|line| decode_line(line).ok())
            .collect();
        let linked_assistant =
            frames.iter().filter(|frame| matches!(
                frame, Frame::Assistant { parent_tool_use_id, .. }
                if !parent_tool_use_id.is_empty()
            )).count();
        assert!(linked_assistant >= 2, "the agent's thinking and Read link up");
        let linked_results =
            frames.iter().filter(|frame| matches!(
                frame, Frame::UserResult { parent_tool_use_id, .. }
                if !parent_tool_use_id.is_empty()
            )).count();
        assert_eq!(linked_results, 1, "the nested Read result links up");
        let main_results =
            frames.iter().filter(|frame| matches!(
                frame, Frame::UserResult { parent_tool_use_id, results, .. }
                if parent_tool_use_id.is_empty() && !results.is_empty()
            )).count();
        assert_eq!(main_results, 1, "the agent's own answer stays main-thread");
    }
}
