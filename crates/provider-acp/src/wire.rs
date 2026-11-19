//! ACP's JSON-RPC frames: what this client writes and how it reads what an
//! agent writes back.
//!
//! Written fresh from the protocol's published v1 shapes
//! (agentclientprotocol.com; the Apache-2.0 schema crate was read for the
//! field names, not depended on): `initialize`, `session/new`,
//! `session/load`, `session/prompt` and the `session/cancel` notification
//! from the client; `session/update` notifications and the
//! `session/request_permission` request from the agent. Every value stays a
//! `serde_json::Value` until a decoder below reads the fields this adapter
//! uses, so an agent that sends more than v1 (a newer field, an unknown
//! update kind) is read for what it shares with v1 and never refused for
//! the rest.
//!
//! Nothing outside this crate names an ACP method or field.

use serde_json::{json, Value};

/// The protocol version this client speaks.
pub const PROTOCOL_VERSION: u64 = 1;

/// The client→agent handshake.
pub const INITIALIZE: &str = "initialize";
/// Start a fresh agent session.
pub const SESSION_NEW: &str = "session/new";
/// Re-open a stored agent session; its history streams back as updates.
pub const SESSION_LOAD: &str = "session/load";
/// One prompt: a turn. Answered when the turn ends, with its stop reason.
pub const SESSION_PROMPT: &str = "session/prompt";
/// Stop the running turn (a notification).
pub const SESSION_CANCEL: &str = "session/cancel";
/// Everything the agent reports about a session (a notification).
pub const SESSION_UPDATE: &str = "session/update";
/// The agent asks before a tool call runs (a request).
pub const REQUEST_PERMISSION: &str = "session/request_permission";

/// JSON-RPC's "method not found": what this client answers an agent request
/// it does not serve (the file-system and terminal methods it never
/// advertised), never silence.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// ACP's "authentication required": a `session/new` the agent refuses
/// until it is signed in.
pub const AUTH_REQUIRED: i64 = -32000;

/// One line from the agent, by JSON-RPC shape.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A result for one of this client's requests.
    Response {
        /// The request's id.
        id: Value,
        /// The result.
        result: Value,
    },
    /// An error for one of this client's requests.
    Error {
        /// The request's id.
        id: Value,
        /// The JSON-RPC error code.
        code: i64,
        /// The agent's words.
        message: String,
    },
    /// A request from the agent: it waits for this client's answer.
    Request {
        /// Its id, echoed in the answer.
        id: Value,
        /// The method.
        method: String,
        /// Its params.
        params: Value,
    },
    /// A notification from the agent.
    Notification {
        /// The method.
        method: String,
        /// Its params.
        params: Value,
    },
}

/// Read one line. `None` for a line that is not a JSON-RPC frame.
pub fn decode(line: &str) -> Option<Frame> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    frame_of(value)
}

/// Read one already-parsed message.
pub fn frame_of(mut value: Value) -> Option<Frame> {
    let object = value.as_object_mut()?;
    let method = object.get("method").and_then(Value::as_str).map(str::to_owned);
    let id = object.remove("id").filter(|id| !id.is_null());
    let params = object.remove("params").unwrap_or(Value::Null);
    match (method, id) {
        (Some(method), Some(id)) => Some(Frame::Request { id, method, params }),
        (Some(method), None) => Some(Frame::Notification { method, params }),
        (None, Some(id)) => match object.remove("error") {
            Some(error) => Some(Frame::Error {
                id,
                code: error.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: error.get("message").and_then(Value::as_str).unwrap_or_default().to_owned(),
            }),
            None => Some(Frame::Response { id, result: object.remove("result").unwrap_or(Value::Null) }),
        },
        (None, None) => None,
    }
}

/// A request frame.
pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

/// The `initialize` params: protocol v1, no file-system or terminal
/// methods offered (this client serves neither, so an agent never asks).
pub fn initialize_params(client_name: &str, client_version: &str) -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false},
        "clientInfo": {"name": client_name, "version": client_version},
    })
}

/// One stdio MCP server a session gets: Baaz's terminal relay bridge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StdioServer {
    /// Its name in the agent's tool list.
    pub name: String,
    /// The program.
    pub command: String,
    /// Its arguments.
    pub args: Vec<String>,
}

fn servers(servers: &[StdioServer]) -> Value {
    Value::Array(
        servers
            .iter()
            .map(|server| json!({"name": server.name, "command": server.command, "args": server.args, "env": []}))
            .collect(),
    )
}

/// The `session/new` params.
pub fn session_new_params(cwd: &str, mcp: &[StdioServer]) -> Value {
    json!({"cwd": cwd, "mcpServers": servers(mcp)})
}

/// The `session/load` params.
pub fn session_load_params(session_id: &str, cwd: &str, mcp: &[StdioServer]) -> Value {
    json!({"sessionId": session_id, "cwd": cwd, "mcpServers": servers(mcp)})
}

/// The `session/prompt` params: the prompt as content blocks.
pub fn prompt_params(session_id: &str, prompt: Vec<Value>) -> Value {
    json!({"sessionId": session_id, "prompt": prompt})
}

/// A text content block.
pub fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}

/// An image content block.
pub fn image_block(base64: &str, media_type: &str) -> Value {
    json!({"type": "image", "data": base64, "mimeType": media_type})
}

/// The `session/cancel` notification.
pub fn cancel_notification(session_id: &str) -> Value {
    json!({"jsonrpc": "2.0", "method": SESSION_CANCEL, "params": {"sessionId": session_id}})
}

/// The answer to a permission request: the option the person picked.
pub fn permission_selected(id: &Value, option_id: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": {"outcome": "selected", "optionId": option_id}}})
}

/// The answer to a permission request the turn's cancel made moot.
pub fn permission_cancelled(id: &Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": {"outcome": {"outcome": "cancelled"}}})
}

/// The answer to an agent request this client does not serve.
pub fn method_not_found(id: &Value, method: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": METHOD_NOT_FOUND, "message": format!("baaz does not serve {method}")}})
}

/// What the agent's `initialize` answer says about it: the facts this
/// adapter's capabilities come from (docs/81 B S-4).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentHandshake {
    /// The protocol version it chose.
    pub protocol_version: u64,
    /// Its own name for itself (`agentInfo.title`, else `.name`), when it
    /// sent one.
    pub name: Option<String>,
    /// Its version, when it sent one.
    pub version: Option<String>,
    /// It can re-open a stored session (`session/load`).
    pub load_session: bool,
    /// It takes image content in a prompt.
    pub image: bool,
    /// The sign-in methods it offers, by name: non-empty when it can
    /// refuse a session until it is signed in.
    pub auth_methods: Vec<String>,
}

impl AgentHandshake {
    /// Read an `initialize` result.
    pub fn decode(result: &Value) -> Result<Self, String> {
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_u64)
            .ok_or_else(|| "the initialize answer names no protocol version".to_owned())?;
        let capabilities = result.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        let info = result.get("agentInfo");
        let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        Ok(Self {
            protocol_version,
            name: text(info.and_then(|info| info.get("title"))).or_else(|| text(info.and_then(|info| info.get("name")))),
            version: text(info.and_then(|info| info.get("version"))),
            load_session: capabilities.get("loadSession").and_then(Value::as_bool).unwrap_or(false),
            image: capabilities
                .get("promptCapabilities")
                .and_then(|prompt| prompt.get("image"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
            auth_methods: result
                .get("authMethods")
                .and_then(Value::as_array)
                .map(|methods| {
                    methods
                        .iter()
                        .filter_map(|method| text(method.get("name")).or_else(|| text(method.get("id"))))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }
}

/// Why a prompt turn ended (`PromptResponse.stopReason`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StopReason {
    /// The model finished.
    EndTurn,
    /// It ran out of output tokens.
    MaxTokens,
    /// It reached the agent's limit of model requests in one turn.
    MaxTurnRequests,
    /// It refused to go on.
    Refusal,
    /// The client cancelled it.
    Cancelled,
    /// A reason this client does not know, verbatim.
    Other(String),
}

impl StopReason {
    /// Read a `session/prompt` result.
    pub fn of(result: &Value) -> Self {
        match result.get("stopReason").and_then(Value::as_str).unwrap_or("end_turn") {
            "end_turn" => StopReason::EndTurn,
            "max_tokens" => StopReason::MaxTokens,
            "max_turn_requests" => StopReason::MaxTurnRequests,
            "refusal" => StopReason::Refusal,
            "cancelled" => StopReason::Cancelled,
            other => StopReason::Other(other.to_owned()),
        }
    }
}

/// What one permission option means (`PermissionOptionKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionKind {
    /// Allow this call.
    AllowOnce,
    /// Allow it and remember.
    AllowAlways,
    /// Refuse this call.
    RejectOnce,
    /// Refuse it and remember.
    RejectAlways,
}

impl OptionKind {
    fn of(raw: &str) -> Option<Self> {
        match raw {
            "allow_once" => Some(OptionKind::AllowOnce),
            "allow_always" => Some(OptionKind::AllowAlways),
            "reject_once" => Some(OptionKind::RejectOnce),
            "reject_always" => Some(OptionKind::RejectAlways),
            _ => None,
        }
    }

    /// Whether choosing it lets the call run.
    pub fn allows(self) -> bool {
        matches!(self, OptionKind::AllowOnce | OptionKind::AllowAlways)
    }
}

/// One option a permission request offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PermissionOption {
    /// The id the answer echoes.
    pub id: String,
    /// The agent's label for it.
    pub name: String,
    /// What it means; `None` for a kind this client does not know (never
    /// offered on the card).
    pub kind: Option<OptionKind>,
}

/// The options of a `session/request_permission` request.
pub fn permission_options(params: &Value) -> Vec<PermissionOption> {
    params
        .get("options")
        .and_then(Value::as_array)
        .map(|options| {
            options
                .iter()
                .filter_map(|option| {
                    Some(PermissionOption {
                        id: option.get("optionId")?.as_str()?.to_owned(),
                        name: option.get("name").and_then(Value::as_str).unwrap_or_default().to_owned(),
                        kind: option.get("kind").and_then(Value::as_str).and_then(OptionKind::of),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The session a `session/update` or a request names.
pub fn session_of(params: &Value) -> Option<&str> {
    params.get("sessionId").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_read_by_shape() {
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}"#),
            Some(Frame::Response { id: json!(3), result: json!({"stopReason": "end_turn"}) })
        );
        assert_eq!(
            decode(r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32000,"message":"Authentication required"}}"#),
            Some(Frame::Error { id: json!(4), code: AUTH_REQUIRED, message: "Authentication required".into() })
        );
        assert!(matches!(decode(r#"{"id":"p1","method":"session/request_permission","params":{}}"#), Some(Frame::Request { .. })));
        assert!(matches!(decode(r#"{"method":"session/update","params":{}}"#), Some(Frame::Notification { .. })));
        assert_eq!(decode("not json"), None);
        assert_eq!(decode(r#"{"jsonrpc":"2.0"}"#), None);
    }

    #[test]
    fn the_handshake_reads_what_the_capabilities_come_from() {
        let handshake = AgentHandshake::decode(&json!({
            "protocolVersion": 1,
            "agentCapabilities": {"loadSession": true, "promptCapabilities": {"image": true, "audio": false}},
            "agentInfo": {"name": "stand-in", "title": "Stand-in Agent", "version": "0.3.0"},
            "authMethods": [{"id": "login", "name": "Log in"}],
        }))
        .expect("decodes");
        assert_eq!(handshake.name.as_deref(), Some("Stand-in Agent"));
        assert_eq!(handshake.version.as_deref(), Some("0.3.0"));
        assert!(handshake.load_session && handshake.image);
        assert_eq!(handshake.auth_methods, vec!["Log in".to_owned()]);
        let bare = AgentHandshake::decode(&json!({"protocolVersion": 1})).expect("decodes");
        assert!(!bare.load_session && !bare.image && bare.name.is_none(), "absent is false, never assumed");
        assert!(AgentHandshake::decode(&json!({})).is_err(), "no version, no handshake");
    }

    #[test]
    fn permission_options_keep_every_known_kind() {
        let options = permission_options(&json!({"options": [
            {"optionId": "a", "name": "Allow", "kind": "allow_once"},
            {"optionId": "b", "name": "Always", "kind": "allow_always"},
            {"optionId": "c", "name": "No", "kind": "reject_once"},
            {"optionId": "d", "name": "Odd", "kind": "something_new"},
        ]}));
        assert_eq!(options.len(), 4);
        assert_eq!(options[1].kind, Some(OptionKind::AllowAlways));
        assert_eq!(options[3].kind, None, "an unknown kind is read, never offered");
        assert!(OptionKind::AllowOnce.allows() && !OptionKind::RejectAlways.allows());
    }

    #[test]
    fn stop_reasons_read_and_unknowns_keep_their_words() {
        assert_eq!(StopReason::of(&json!({"stopReason": "cancelled"})), StopReason::Cancelled);
        assert_eq!(StopReason::of(&json!({"stopReason": "max_tokens"})), StopReason::MaxTokens);
        assert_eq!(StopReason::of(&json!({"stopReason": "later"})), StopReason::Other("later".into()));
        assert_eq!(StopReason::of(&json!({})), StopReason::EndTurn);
    }
}
