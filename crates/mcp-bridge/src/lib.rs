//! Baaz's tools spoken as MCP: a stdio JSON-RPC bridge.
//!
//! The bridge answers exactly three methods over newline-delimited JSON
//! (`initialize`, `tools/list`, `tools/call`), mirroring the throwaway probe
//! server evidenced in `docs/18-claude-code.md` §4. It is provider-agnostic:
//! the Claude-Code-specific part (writing the `--mcp-config` file) lives in
//! the adapter lane, not here.

use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Protocol version the probe server answered with, and this bridge keeps.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Name reported in `initialize`'s `serverInfo`.
pub const SERVER_NAME: &str = "mcp-bridge";

/// One content block in a `tools/call` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentBlock {
    /// Always `"text"` for now; kept as a string so richer blocks fit later.
    #[serde(rename = "type")]
    pub kind: String,
    /// The text payload.
    pub text: String,
}

impl ContentBlock {
    /// A single text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: text.into(),
        }
    }
}

/// What a successful tool call hands back to the provider.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolOutcome {
    /// Content blocks of the result; never empty in practice.
    pub content: Vec<ContentBlock>,
    /// False on success; serialized so the shape matches MCP exactly.
    #[serde(rename = "isError", default, skip_serializing_if = "is_false")]
    pub is_error: bool,
}

fn is_false(b: &bool) -> bool {
    !b
}

impl ToolOutcome {
    /// A single text block, the common case for diagnostic tools.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(text)],
            is_error: false,
        }
    }
}

/// A tool handler: JSON arguments in, outcome or a human message out.
pub type Handler =
    Box<dyn Fn(Value) -> Result<ToolOutcome, String> + Send + Sync>;

/// One registered tool: its advertisement plus its handler.
pub struct ToolDef {
    /// Tool name, unique within the registry.
    pub name: String,
    /// Human description, advertised via `tools/list`.
    pub description: String,
    /// JSON Schema for the tool's arguments, advertised via `tools/list`
    /// and checked before the handler runs.
    pub input_schema: Value,
    handler: Handler,
}

impl fmt::Debug for ToolDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolDef")
            .field("name", &self.name)
            .field("description", &self.description)
            .field("input_schema", &self.input_schema)
            .finish_non_exhaustive()
    }
}

/// Refusal to register a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// A tool with this name is already registered. Shadowing would let the
    /// model call a tool the operator cannot see, so this is an error,
    /// never last-wins.
    Duplicate { name: String },
    /// The name is empty.
    EmptyName,
}

impl fmt::Display for RegistryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Duplicate { name } => {
                write!(f, "tool already registered: {name}")
            }
            Self::EmptyName => write!(f, "tool name must not be empty"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// The set of tools the bridge serves. Registration order is preserved in
/// `tools/list` output.
#[derive(Default)]
pub struct ToolRegistry {
    tools: Vec<ToolDef>,
    index: HashMap<String, usize>,
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ToolRegistry")
            .field(
                "tools",
                &self.tools.iter().map(|t| &t.name).collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl ToolRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one tool. A repeated name is an error, not a silent replace.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        description: impl Into<String>,
        input_schema: Value,
        handler: impl Fn(Value) -> Result<ToolOutcome, String>
            + Send
            + Sync
            + 'static,
    ) -> Result<(), RegistryError> {
        let name = name.into();
        if name.is_empty() {
            return Err(RegistryError::EmptyName);
        }
        if self.index.contains_key(&name) {
            return Err(RegistryError::Duplicate { name });
        }
        self.index.insert(name.clone(), self.tools.len());
        self.tools.push(ToolDef {
            name,
            description: description.into(),
            input_schema,
            handler: Box::new(handler),
        });
        Ok(())
    }

    /// Look a tool up by name.
    pub fn get(&self, name: &str) -> Option<&ToolDef> {
        self.index.get(name).and_then(|i| self.tools.get(*i))
    }

    /// Number of registered tools.
    ///
    /// Nothing in this crate calls it; it is here for the host that will
    /// build the registry and want to report what it exposed.
    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tools are registered. See [`ToolRegistry::len`].
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Registered tools in registration order.
    pub fn tools(&self) -> &[ToolDef] {
        &self.tools
    }
}

/// Build the `{"mcpServers":{...}}` config value for this server.
///
/// `server_name` is the provider-side name (the probe used `"baazprobe"`),
/// `command` the bridge binary to spawn, and `args` its argv. Nothing is
/// hardcoded: the name travels with the value so the adapter lane can pass
/// it straight into the child's `--mcp-config` file.
pub fn mcp_config_value(
    server_name: &str,
    command: &str,
    args: &[impl AsRef<str>],
) -> Value {
    let args: Vec<Value> =
        args.iter().map(|a| Value::String(a.as_ref().to_string())).collect();
    let mut servers = serde_json::Map::new();
    servers.insert(
        server_name.to_string(),
        json!({"type": "stdio", "command": command, "args": args}),
    );
    let mut root = serde_json::Map::new();
    root.insert("mcpServers".to_string(), Value::Object(servers));
    Value::Object(root)
}

/// Run the JSON-RPC loop: one NDJSON request per line on `reader`, one
/// NDJSON response per reply owed on `writer`.
///
/// The loop never fails on bad input: malformed lines, unknown methods,
/// unknown tools, schema mismatches and panicking handlers each produce an
/// error response where one is owed, and the loop continues. Only an
/// underlying I/O failure ends it with `Err`.
pub fn serve_loop<R: BufRead, W: Write>(
    reader: R,
    writer: &mut W,
    registry: &ToolRegistry,
) -> std::io::Result<()> {
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = handle_line(&line, registry) {
            writer.write_all(reply.as_bytes())?;
            writer.write_all(b"\n")?;
        }
    }
    writer.flush()?;
    Ok(())
}

/// Handle one input line. `None` means no reply is owed (a notification).
fn handle_line(line: &str, registry: &ToolRegistry) -> Option<String> {
    let request: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => {
            return Some(error_reply(
                &Value::Null,
                -32700,
                &format!("parse error: {e}"),
            ));
        }
    };
    let object = match request.as_object() {
        Some(o) => o,
        None => {
            return Some(error_reply(
                &Value::Null,
                -32600,
                "invalid request: expected a JSON object",
            ));
        }
    };
    // JSON-RPC 2.0: a request is a notification when the `id` MEMBER IS
    // ABSENT, not when its value is null. Conflating the two strands a
    // client that sends an explicit `"id": null` — it waits for a reply
    // that never comes, and the provider's whole session waits with it.
    // Absent -> no reply, ever. Present-but-null -> a reply, echoing null.
    let Some(id) = object.get("id") else {
        // A notification: never a reply, even if malformed.
        return None;
    };
    let method = object.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        return Some(error_reply(id, -32600, "invalid request: missing method"));
    };
    let params = object.get("params").unwrap_or(&Value::Null);

    match method {
        "initialize" => Some(success_reply(id, &initialize_result())),
        "tools/list" => Some(success_reply(id, &tools_list_result(registry))),
        "tools/call" => Some(call_tool(id, params, registry)),
        _ => Some(error_reply(id, -32601, &format!("method not found: {method}"))),
    }
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    })
}

fn tools_list_result(registry: &ToolRegistry) -> Value {
    let tools: Vec<Value> = registry
        .tools()
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            })
        })
        .collect();
    json!({"tools": tools})
}

fn call_tool(id: &Value, params: &Value, registry: &ToolRegistry) -> String {
    let name = params.get("name").and_then(Value::as_str);
    let Some(name) = name else {
        return error_reply(id, -32602, "invalid params: missing tool name");
    };
    let Some(tool) = registry.get(name) else {
        return error_reply(id, -32602, &format!("unknown tool: {name}"));
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Err(detail) = validate_against_schema(&tool.input_schema, &arguments) {
        return error_reply(id, -32602, &format!("invalid params: {detail}"));
    }
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (tool.handler)(arguments)
    }));
    match outcome {
        Err(_) => error_reply(id, -32603, "internal error: tool handler panicked"),
        Ok(Err(message)) => error_reply(id, -32603, &format!("tool failed: {message}")),
        Ok(Ok(outcome)) => success_reply(id, &serde_json::to_value(&outcome).unwrap_or_else(|_| json!({"content": []}))),
    }
}

/// Check `args` against a JSON-Schema-shaped `schema`, covering the subset
/// bridges actually emit: `type`, `required`, `properties`, `items`, `enum`.
///
/// Unknown keywords are ignored so richer schemas still validate their
/// checkable parts. Anything outside this subset that matters should grow
/// this function, not bypass it.
fn validate_against_schema(schema: &Value, args: &Value) -> Result<(), String> {
    if let Some(false) = schema.as_bool() {
        return Err("arguments forbidden by schema".to_string());
    }
    if schema.as_bool() == Some(true) {
        return Ok(());
    }
    let Some(schema) = schema.as_object() else {
        return Ok(());
    };

    if let Some(expected) = schema.get("type").and_then(Value::as_str) {
        if !matches_json_type(expected, args) {
            return Err(format!(
                "expected type {expected}, got {}",
                json_type_name(args)
            ));
        }
    }

    if let Some(expected) = schema.get("enum").and_then(Value::as_array) {
        if !expected.contains(args) {
            return Err("value is not one of the allowed enum values".to_string());
        }
    }

    if args.is_object() {
        let object =
            args.as_object().expect("checked as object just above");
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("missing required property: {key}"));
                }
            }
        }
        if let Some(properties) =
            schema.get("properties").and_then(Value::as_object)
        {
            for (key, subschema) in properties {
                if let Some(value) = object.get(key) {
                    validate_against_schema(subschema, value).map_err(|e| {
                        format!("property {key}: {e}")
                    })?;
                }
            }
        }
    }

    if let (Some(items), Some(elements)) = (
        schema.get("items"),
        args.as_array(),
    ) {
        // Tuple-form `items` (an array) is out of subset; object-form applies
        // to every element.
        if !items.is_array() {
            for (i, element) in elements.iter().enumerate() {
                validate_against_schema(items, element)
                    .map_err(|e| format!("item {i}: {e}"))?;
            }
        }
    }

    Ok(())
}

fn matches_json_type(expected: &str, value: &Value) -> bool {
    match expected {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => true,
    }
}

fn json_type_name(value: &Value) -> &'static str {
    if value.is_null() {
        "null"
    } else if value.is_boolean() {
        "boolean"
    } else if value.is_string() {
        "string"
    } else if value.is_i64() || value.is_u64() {
        "integer"
    } else if value.is_number() {
        "number"
    } else if value.is_array() {
        "array"
    } else {
        "object"
    }
}

fn success_reply(id: &Value, result: &Value) -> String {
    serde_json::to_string(&json!({"jsonrpc": "2.0", "id": id, "result": result}))
        .expect("reply serializes")
}

fn error_reply(id: &Value, code: i64, message: &str) -> String {
    serde_json::to_string(
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    )
    .expect("reply serializes")
}
