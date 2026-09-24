//! One stdio line in, one decoded frame out. Pure and IO-free.
//!
//! `codex app-server` speaks newline-delimited JSON-RPC 2.0 over stdio, and
//! it is bidirectional: the server sends requests to us that we must answer.
//! Every shape here was captured live in `fixtures/codex/` (see
//! `docs/19-codex.md`); nothing is designed from memory of other protocols.
//!
//! Two wire facts the decoder leans on:
//!
//! * There is **no `"jsonrpc":"2.0"` field** on the wire in the captured
//!   frames. Frames are classified by which of `id`/`method`/`result`/
//!   `error` they carry, never by a version tag.
//! * 68 notification kinds exist and this decoder knows a subset. An unknown
//!   `method` decodes to [`Notification::Unknown`], never an error: a future
//!   CLI version degrades instead of failing.

use serde_json::Value;

/// A line that is not JSON at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// What failed to parse.
    pub reason: String,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "undecodable codex line: {}", self.reason)
    }
}

impl std::error::Error for DecodeError {}

/// Which way a recorded fixture line traveled. The live wire carries bare
/// JSON-RPC objects; the fixtures wrap each in `{"_dir", "frame"}` so a
/// bidirectional protocol can record both directions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    /// `client->server`: our requests and notifications.
    ClientToServer,
    /// `server->client`: responses, notifications, and requests to us.
    ServerToClient,
}

/// Token counts in one `thread/tokenUsage/updated` bucket.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenCounts {
    /// All tokens in the bucket.
    pub total_tokens: u64,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Prompt tokens served from cache.
    pub cached_input_tokens: u64,
    /// Completion tokens.
    pub output_tokens: u64,
    /// Completion tokens spent reasoning.
    pub reasoning_output_tokens: u64,
}

/// The `tokenUsage` of `thread/tokenUsage/updated`.
///
/// `total` is cumulative over the thread; `last` covers only the named turn.
/// There is deliberately **no cost field**: the protocol reports tokens,
/// never money, and a `cost_usd` reading `0.0` would present absence as a
/// measurement.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    /// Cumulative over the thread. Never sum this per turn.
    pub total: TokenCounts,
    /// This turn only. The per-turn figure.
    pub last: TokenCounts,
    /// The model's context window.
    pub model_context_window: u64,
}

impl TokenUsage {
    /// The per-turn figure: `last`, not `total`.
    pub fn per_turn(&self) -> &TokenCounts {
        &self.last
    }
}

/// One transcript item carried by `item/started` / `item/completed`.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// An `agentMessage`: finished prose in `text`.
    AgentMessage {
        /// The wire item id (`msg_…`).
        id: String,
        /// The complete text (empty while still streaming).
        text: String,
    },
    /// A `userMessage`: the submitted prompt, as joined text parts.
    UserMessage {
        /// The wire item id.
        id: String,
        /// The prompt text.
        text: String,
    },
    /// A `reasoning` trace: summaries joined (empty in every fixture).
    Reasoning {
        /// The wire item id (`rs_…`).
        id: String,
        /// The trace text.
        text: String,
    },
    /// A `commandExecution`: one shell invocation and its outcome.
    CommandExecution {
        /// The wire item id (`exec-…`).
        id: String,
        /// The exact command that ran.
        command: String,
        /// Wire status (`inProgress`, `completed`, …).
        status: String,
        /// Process exit code, when reported.
        exit_code: Option<i64>,
    },
    /// An item kind this decoder does not know. Carried, not dropped.
    Other {
        /// The wire `type` string.
        item_type: String,
        /// The wire item id, when present.
        id: String,
    },
}

/// One server-to-us notification.
#[derive(Clone, Debug, PartialEq)]
pub enum Notification {
    /// `thread/started`: the server minted the thread (and session) id.
    ThreadStarted {
        /// The minted thread id.
        thread_id: String,
        /// The minted session id (equal to the thread id on a fresh thread).
        session_id: String,
    },
    /// `thread/status/changed`: `active` or `idle`.
    ThreadStatus {
        /// The thread whose status moved.
        thread_id: String,
        /// The wire status type string.
        status: String,
    },
    /// `turn/started`: a turn began.
    TurnStarted {
        /// The owning thread.
        thread_id: String,
        /// The started turn.
        turn_id: String,
    },
    /// `turn/completed`: a turn ended (`completed`, `interrupted`, …).
    TurnCompleted {
        /// The owning thread.
        thread_id: String,
        /// The finished turn.
        turn_id: String,
        /// The wire status string.
        status: String,
        /// Wall-clock time in milliseconds, when reported.
        duration_ms: u64,
    },
    /// `item/started`: an item began (carried; the fold renders nothing —
    /// see the lane choice in [`crate::fold`]).
    ItemStarted {
        /// The owning thread.
        thread_id: String,
        /// The owning turn.
        turn_id: String,
        /// The started item.
        item: Item,
    },
    /// `item/completed`: an item finished, carrying its full content.
    ItemCompleted {
        /// The owning thread.
        thread_id: String,
        /// The owning turn.
        turn_id: String,
        /// The completed item.
        item: Item,
    },
    /// `item/agentMessage/delta`: one incremental text chunk. Decoded (so a
    /// shape change still parses) and then ignored for transcript purposes;
    /// the fold renders nothing from it (see the lane choice in
    /// [`crate::fold`]).
    AgentMessageDelta {
        /// The owning thread.
        thread_id: String,
        /// The owning turn.
        turn_id: String,
        /// The message item this chunk belongs to (`msg_…`).
        item_id: String,
        /// The incremental text.
        delta: String,
    },
    /// `thread/tokenUsage/updated`: per-turn tokens keyed on
    /// `(thread_id, turn_id)`.
    TokenUsage {
        /// The owning thread.
        thread_id: String,
        /// The turn the `last` bucket covers.
        turn_id: String,
        /// Total vs last buckets plus the context window.
        usage: TokenUsage,
    },
    /// `account/rateLimits/updated`: the money guard's feed, pushed
    /// unprompted. Kept raw: the fold only needs presence, not fields.
    RateLimitsUpdated {
        /// The raw `params`.
        params: Value,
    },
    /// `mcpServer/startupStatus/updated`: one MCP server's startup movement.
    McpStatus {
        /// The owning thread, when named.
        thread_id: Option<String>,
        /// Server name (`cua_repl`, `node_repl`, `codex_apps`, …).
        name: String,
        /// Wire status (`starting`, …).
        status: String,
    },
    /// An `error` notification: the server complaining, not answering.
    ErrorNotice {
        /// Human text, or the raw params when no message field exists.
        message: String,
    },
    /// A `warning` notification.
    WarningNotice {
        /// Human text, or the raw params when no message field exists.
        message: String,
    },
    /// `serverRequest/resolved`: the server withdrew request `request_id`
    /// (our answer, or lack of one, was accepted).
    ServerRequestResolved {
        /// The owning thread, when named.
        thread_id: Option<String>,
        /// The resolved server request id.
        request_id: Value,
    },
    /// Any other method: 68 kinds exist and we decode a subset. A future CLI
    /// version degrades instead of failing.
    Unknown {
        /// The wire method.
        method: String,
        /// The raw `params`.
        params: Value,
    },
}

/// One decoded wire object.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A JSON-RPC request (`id` plus `method`): our calls, or the server
    /// asking us (approvals, tool calls, questions).
    Request {
        /// The request id (number on every captured frame).
        id: Value,
        /// The method (`thread/start`, `item/commandExecution/requestApproval`, …).
        method: String,
        /// The raw `params`.
        params: Value,
    },
    /// A JSON-RPC success response (`id` plus `result`).
    Response {
        /// The id being answered.
        id: Value,
        /// The raw `result`.
        result: Value,
    },
    /// A JSON-RPC error response (`id` plus `error`).
    ResponseError {
        /// The id being answered.
        id: Value,
        /// The wire error code.
        code: i64,
        /// The wire error message.
        message: String,
    },
    /// A JSON-RPC notification (`method`, no `id`).
    Notification(Notification),
}

fn str_field(value: &Value, key: &str) -> String {
    value.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn counts_from(value: &Value) -> TokenCounts {
    let uint = |key: &str| value.get(key).and_then(Value::as_u64).unwrap_or(0);
    TokenCounts {
        total_tokens: uint("totalTokens"),
        input_tokens: uint("inputTokens"),
        cached_input_tokens: uint("cachedInputTokens"),
        output_tokens: uint("outputTokens"),
        reasoning_output_tokens: uint("reasoningOutputTokens"),
    }
}

fn usage_from(value: &Value) -> TokenUsage {
    TokenUsage {
        total: value.get("total").map(counts_from).unwrap_or_default(),
        last: value.get("last").map(counts_from).unwrap_or_default(),
        model_context_window: value
            .get("modelContextWindow")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

fn text_parts(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| {
                if part.get("type").and_then(Value::as_str) == Some("text") {
                    part.get("text").and_then(Value::as_str)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .concat(),
        _ => String::new(),
    }
}

fn strings_joined(value: &Value) -> String {
    match value {
        // Per the schema (`ReasoningItemContent`, `ReasoningItemReasoningSummary`)
        // every element is an object carrying its text under `"text"` — never
        // a bare string. `Value::as_str` on an object is `None`, so filtering
        // by it drops every element unconditionally and reasoning always
        // decodes to `""`. Extract `.text` from each object instead.
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => text.clone(),
                _ => item.get("text").and_then(Value::as_str).unwrap_or_default().to_owned(),
            })
            .collect::<Vec<_>>()
            .concat(),
        Value::String(text) => text.clone(),
        _ => String::new(),
    }
}

fn decode_item(item: &Value) -> Item {
    let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");
    let id = str_field(item, "id");
    match item_type {
        "agentMessage" => Item::AgentMessage { id, text: str_field(item, "text") },
        "userMessage" => Item::UserMessage {
            id,
            text: item.get("content").map(text_parts).unwrap_or_default(),
        },
        "reasoning" => {
            let mut text = item.get("summary").map(strings_joined).unwrap_or_default();
            text.push_str(&item.get("content").map(strings_joined).unwrap_or_default());
            Item::Reasoning { id, text }
        }
        "commandExecution" => Item::CommandExecution {
            id,
            command: str_field(item, "command"),
            status: str_field(item, "status"),
            exit_code: item.get("exitCode").and_then(Value::as_i64),
        },
        other => Item::Other { item_type: other.to_owned(), id },
    }
}

fn notice_message(params: &Value) -> String {
    if let Some(text) = params.as_str() {
        return text.to_owned();
    }
    if let Some(message) = params.get("message").and_then(Value::as_str) {
        return message.to_owned();
    }
    if let Some(text) = params.get("text").and_then(Value::as_str) {
        return text.to_owned();
    }
    params.to_string()
}

fn decode_notification(method: &str, params: &Value) -> Notification {
    let thread = || str_field(params, "threadId");
    let turn = || str_field(params, "turnId");
    match method {
        "thread/started" => {
            let thread_obj = params.get("thread").unwrap_or(&Value::Null);
            Notification::ThreadStarted {
                thread_id: str_field(thread_obj, "id"),
                session_id: str_field(thread_obj, "sessionId"),
            }
        }
        "thread/status/changed" => Notification::ThreadStatus {
            thread_id: thread(),
            status: params
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        },
        "turn/started" => Notification::TurnStarted {
            thread_id: thread(),
            turn_id: params
                .get("turn")
                .map(|turn| str_field(turn, "id"))
                .unwrap_or_default(),
        },
        "turn/completed" => {
            let turn_obj = params.get("turn").unwrap_or(&Value::Null);
            Notification::TurnCompleted {
                thread_id: thread(),
                turn_id: str_field(turn_obj, "id"),
                status: str_field(turn_obj, "status"),
                duration_ms: turn_obj.get("durationMs").and_then(Value::as_u64).unwrap_or(0),
            }
        }
        "item/started" => Notification::ItemStarted {
            thread_id: thread(),
            turn_id: turn(),
            item: params.get("item").map(decode_item).unwrap_or(Item::Other {
                item_type: String::new(),
                id: String::new(),
            }),
        },
        "item/completed" => Notification::ItemCompleted {
            thread_id: thread(),
            turn_id: turn(),
            item: params.get("item").map(decode_item).unwrap_or(Item::Other {
                item_type: String::new(),
                id: String::new(),
            }),
        },
        "item/agentMessage/delta" => Notification::AgentMessageDelta {
            thread_id: thread(),
            turn_id: turn(),
            item_id: str_field(params, "itemId"),
            delta: str_field(params, "delta"),
        },
        "thread/tokenUsage/updated" => Notification::TokenUsage {
            thread_id: thread(),
            turn_id: turn(),
            usage: params.get("tokenUsage").map(usage_from).unwrap_or_default(),
        },
        "account/rateLimits/updated" => {
            Notification::RateLimitsUpdated { params: params.clone() }
        }
        "mcpServer/startupStatus/updated" => Notification::McpStatus {
            thread_id: params.get("threadId").and_then(Value::as_str).map(str::to_owned),
            name: str_field(params, "name"),
            status: str_field(params, "status"),
        },
        "error" => Notification::ErrorNotice { message: notice_message(params) },
        "warning" => Notification::WarningNotice { message: notice_message(params) },
        "serverRequest/resolved" => Notification::ServerRequestResolved {
            thread_id: params.get("threadId").and_then(Value::as_str).map(str::to_owned),
            request_id: params.get("requestId").cloned().unwrap_or(Value::Null),
        },
        other => Notification::Unknown { method: other.to_owned(), params: params.clone() },
    }
}

/// Decode one wire object. Pure and IO-free.
///
/// Requests, responses, and every known notification decode to their arm;
/// unknown notification methods decode to [`Notification::Unknown`]; only
/// non-object input is an error.
pub fn decode_value(frame: &Value) -> Result<Frame, DecodeError> {
    let object = frame.as_object().ok_or_else(|| DecodeError {
        reason: format!("expected a JSON-RPC object, got {frame}"),
    })?;
    let has_id = object.contains_key("id");
    let method = object.get("method").and_then(Value::as_str);
    match (has_id, method) {
        (true, Some(method)) => Ok(Frame::Request {
            id: object.get("id").cloned().unwrap_or(Value::Null),
            method: method.to_owned(),
            params: object.get("params").cloned().unwrap_or(Value::Null),
        }),
        (true, None) => {
            let id = object.get("id").cloned().unwrap_or(Value::Null);
            if let Some(error) = object.get("error") {
                Ok(Frame::ResponseError {
                    id,
                    code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                })
            } else {
                Ok(Frame::Response {
                    id,
                    result: object.get("result").cloned().unwrap_or(Value::Null),
                })
            }
        }
        (false, Some(method)) => Ok(Frame::Notification(decode_notification(
            method,
            object.get("params").unwrap_or(&Value::Null),
        ))),
        (false, None) => Err(DecodeError {
            reason: format!("object is neither request, response, nor notification: {frame}"),
        }),
    }
}

/// Decode one live stdio line: a bare JSON-RPC object.
pub fn decode_line(line: &str) -> Result<Frame, DecodeError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| DecodeError { reason: error.to_string() })?;
    decode_value(&value)
}

/// Decode one recorded fixture line: `{"_dir", "frame"}`.
pub fn decode_envelope(line: &str) -> Result<(Direction, Frame), DecodeError> {
    let value: Value =
        serde_json::from_str(line).map_err(|error| DecodeError { reason: error.to_string() })?;
    let dir = value.get("_dir").and_then(Value::as_str).ok_or_else(|| DecodeError {
        reason: format!("envelope without _dir: {line}"),
    })?;
    let direction = match dir {
        "client->server" => Direction::ClientToServer,
        "server->client" => Direction::ServerToClient,
        other => {
            return Err(DecodeError { reason: format!("unknown envelope direction {other:?}") });
        }
    };
    let frame = value.get("frame").ok_or_else(|| DecodeError {
        reason: format!("envelope without frame: {line}"),
    })?;
    Ok((direction, decode_value(frame)?))
}

impl Item {
    /// The wire item id (`msg_…`, `exec-…`, …).
    pub fn id(&self) -> &str {
        match self {
            Item::AgentMessage { id, .. }
            | Item::UserMessage { id, .. }
            | Item::Reasoning { id, .. }
            | Item::CommandExecution { id, .. }
            | Item::Other { id, .. } => id,
        }
    }

    /// The wire `type` string (`agentMessage`, `userMessage`, …).
    pub fn kind(&self) -> &'static str {
        match self {
            Item::AgentMessage { .. } => "agentMessage",
            Item::UserMessage { .. } => "userMessage",
            Item::Reasoning { .. } => "reasoning",
            Item::CommandExecution { .. } => "commandExecution",
            Item::Other { .. } => "other",
        }
    }

    /// Renderable text: full `text` for agent messages, joined parts for
    /// user messages, the trace for reasoning; empty for anything else.
    pub fn text(&self) -> &str {
        match self {
            Item::AgentMessage { text, .. }
            | Item::UserMessage { text, .. }
            | Item::Reasoning { text, .. } => text,
            Item::CommandExecution { .. } | Item::Other { .. } => "",
        }
    }

    /// The exact command, for command executions; empty otherwise.
    pub fn command(&self) -> &str {
        match self {
            Item::CommandExecution { command, .. } => command,
            _ => "",
        }
    }

    /// The wire `status` for command executions; empty otherwise.
    pub fn status(&self) -> &str {
        match self {
            Item::CommandExecution { status, .. } => status,
            _ => "",
        }
    }

    /// The process exit code, when reported.
    pub fn exit_code(&self) -> Option<i64> {
        match self {
            Item::CommandExecution { exit_code, .. } => *exit_code,
            _ => None,
        }
    }
}

impl Notification {
    /// The owning thread, when the notification names one.
    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Notification::ThreadStarted { thread_id, .. }
            | Notification::ThreadStatus { thread_id, .. }
            | Notification::TurnStarted { thread_id, .. }
            | Notification::TurnCompleted { thread_id, .. }
            | Notification::ItemStarted { thread_id, .. }
            | Notification::ItemCompleted { thread_id, .. }
            | Notification::AgentMessageDelta { thread_id, .. }
            | Notification::TokenUsage { thread_id, .. } => Some(thread_id),
            Notification::McpStatus { thread_id, .. }
            | Notification::ServerRequestResolved { thread_id, .. } => thread_id.as_deref(),
            Notification::RateLimitsUpdated { .. }
            | Notification::ErrorNotice { .. }
            | Notification::WarningNotice { .. }
            | Notification::Unknown { .. } => None,
        }
    }

    /// The owning turn, when the notification names one.
    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Notification::TurnStarted { turn_id, .. }
            | Notification::TurnCompleted { turn_id, .. }
            | Notification::ItemStarted { turn_id, .. }
            | Notification::ItemCompleted { turn_id, .. }
            | Notification::AgentMessageDelta { turn_id, .. }
            | Notification::TokenUsage { turn_id, .. } => Some(turn_id),
            _ => None,
        }
    }

    /// The carried item, for `item/started` and `item/completed`.
    pub fn item(&self) -> Option<&Item> {
        match self {
            Notification::ItemStarted { item, .. } | Notification::ItemCompleted { item, .. } => {
                Some(item)
            }
            _ => None,
        }
    }

    /// The token buckets, for `thread/tokenUsage/updated`.
    pub fn usage(&self) -> Option<&TokenUsage> {
        match self {
            Notification::TokenUsage { usage, .. } => Some(usage),
            _ => None,
        }
    }

    /// The terminal status string, for `turn/completed`.
    pub fn completed_status(&self) -> Option<&str> {
        match self {
            Notification::TurnCompleted { status, .. } => Some(status),
            _ => None,
        }
    }

    /// Wall-clock milliseconds, for `turn/completed`.
    pub fn duration_ms(&self) -> Option<u64> {
        match self {
            Notification::TurnCompleted { duration_ms, .. } => Some(*duration_ms),
            _ => None,
        }
    }

    /// Human text, for `error` and `warning` notices.
    pub fn notice_text(&self) -> Option<&str> {
        match self {
            Notification::ErrorNotice { message } | Notification::WarningNotice { message } => {
                Some(message)
            }
            _ => None,
        }
    }

    /// The minted session id, for `thread/started`.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Notification::ThreadStarted { session_id, .. } => Some(session_id),
            _ => None,
        }
    }

    /// The raw params, for `account/rateLimits/updated`.
    pub fn rate_limit_params(&self) -> Option<&Value> {
        match self {
            Notification::RateLimitsUpdated { params } => Some(params),
            _ => None,
        }
    }
}

impl Frame {
    /// The method, for requests and notifications.
    pub fn method(&self) -> Option<&str> {
        match self {
            Frame::Request { method, .. } => Some(method),
            Frame::Notification(notification) => match notification {
                Notification::Unknown { method, .. } => Some(method),
                _ => None,
            },
            Frame::Response { .. } | Frame::ResponseError { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_lines(name: &str) -> Vec<String> {
        let path = format!("{}/../../fixtures/codex/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture reads").lines().map(str::to_owned).collect()
    }

    #[test]
    fn every_fixture_frame_decodes_without_error() {
        for name in ["basic.jsonl", "approval.jsonl", "interrupt.jsonl"] {
            for (index, line) in fixture_lines(name).iter().enumerate() {
                decode_envelope(line)
                    .unwrap_or_else(|error| panic!("{name}:{index}: {error}"));
            }
        }
    }

    #[test]
    fn no_jsonrpc_tag_is_required() {
        // The captured frames carry no "jsonrpc":"2.0" field; classification
        // keys on id/method/result/error alone.
        let notification = decode_line(r#"{"method":"turn/started","params":{}}"#).expect("decodes");
        assert!(matches!(notification, Frame::Notification(Notification::TurnStarted { .. })));
        let request = decode_line(r#"{"id":0,"method":"thread/start","params":{}}"#).expect("decodes");
        assert!(matches!(request, Frame::Request { .. }));
        let response = decode_line(r#"{"id":2,"result":{"thread":{"id":"t"}}}"#).expect("decodes");
        assert!(matches!(response, Frame::Response { .. }));
    }

    #[test]
    fn unknown_notification_method_degrades_rather_than_erroring() {
        // 68 notification kinds exist and we decode a subset: anything else
        // must survive the decode so a future CLI version degrades.
        let frame = decode_line(
            r#"{"method":"frobnicate/didSomething","params":{"novel":true}}"#,
        )
        .expect("unknown method decodes");
        match frame {
            Frame::Notification(Notification::Unknown { method, params }) => {
                assert_eq!(method, "frobnicate/didSomething");
                assert_eq!(params.get("novel"), Some(&Value::Bool(true)));
            }
            other => panic!("expected Unknown, got {other:?}"),
        }
        // The fixtures' own not-yet-decoded kinds degrade the same way.
        let remote = decode_line(
            r#"{"method":"remoteControl/status/changed","params":{"status":"disabled"}}"#,
        )
        .expect("decodes");
        assert!(matches!(
            remote,
            Frame::Notification(Notification::Unknown { method, .. })
            if method == "remoteControl/status/changed"
        ));
        let skills =
            decode_line(r#"{"method":"skills/changed","params":{}}"#).expect("decodes");
        assert!(matches!(skills, Frame::Notification(Notification::Unknown { .. })));
    }

    #[test]
    fn error_responses_decode_without_losing_the_id() {
        let frame = decode_line(
            r#"{"id":7,"error":{"code":-32000,"message":"boom"}}"#,
        )
        .expect("decodes");
        match frame {
            Frame::ResponseError { id, code, message } => {
                assert_eq!(id, Value::from(7));
                assert_eq!(code, -32000);
                assert_eq!(message, "boom");
            }
            other => panic!("expected ResponseError, got {other:?}"),
        }
    }

    #[test]
    fn non_json_is_the_only_error() {
        assert!(decode_line("not json at all").is_err());
    }

    #[test]
    fn reasoning_text_extracts_from_schema_object_shape() {
        // `ReasoningItemContent` / `ReasoningItemReasoningSummary` are arrays
        // of objects (`{"type":…, "text":…}`), never bare strings: decoding
        // must read `.text` out of each object, not drop every element.
        let frame = decode_line(
            r#"{"method":"item/completed","params":{"threadId":"t","turnId":"u","item":{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":"planned"}],"content":[{"type":"reasoning_text","text":"trace"},{"type":"text","text":"!"}]}}}"#,
        )
        .expect("schema-shaped reasoning decodes");
        match frame {
            Frame::Notification(Notification::ItemCompleted { item: Item::Reasoning { text, .. }, .. }) => {
                assert_eq!(text, "plannedtrace!", "summary plus content, joined");
            }
            other => panic!("expected a reasoning item, got {other:?}"),
        }
    }
}
