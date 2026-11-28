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

/// The terminal relay: the seven tools as MCP, forwarded to Baaz's socket.
pub mod terminal;
/// The browser relay: the browser tools as MCP, over the same socket.
pub mod browser;
/// The memory relay: past sessions and memory notes, over the same socket.
pub mod memory;
/// The pane relay: show things in the right pane, read and act on Changes.
pub mod pane;
/// The orchestration relay: delegate to other agents; workers report back.
pub mod orchestra;

/// Protocol version the probe server answered with, and this bridge keeps.
pub const PROTOCOL_VERSION: &str = "2024-11-05";

/// Name reported in `initialize`'s `serverInfo`.
pub const SERVER_NAME: &str = "mcp-bridge";

/// One content block in a `tools/call` result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentBlock {
    /// `"text"` or `"image"`; kept as a string so richer blocks fit later.
    #[serde(rename = "type")]
    pub kind: String,
    /// The text payload. Absent on image blocks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Base64-encoded bytes. Present on image blocks only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
    /// The image's media type (`image/png`). Present on image blocks only.
    #[serde(default, rename = "mimeType", skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl ContentBlock {
    /// A single text block.
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            kind: "text".to_string(),
            text: Some(text.into()),
            data: None,
            mime_type: None,
        }
    }

    /// A single image block: base64 bytes with their media type.
    pub fn image(base64: impl Into<String>, mime_type: impl Into<String>) -> Self {
        Self {
            kind: "image".to_string(),
            text: None,
            data: Some(base64.into()),
            mime_type: Some(mime_type.into()),
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
    instructions: Option<String>,
    /// docs/85 13.7: the tool-search mode's always-on tools. `Some`, the
    /// list shows only these plus [`TOOL_SEARCH`]; every tool still runs.
    always_on: Option<Vec<String>>,
}

/// docs/85 13.7 (T3's pattern): the one tool that finds and runs the rest
/// when a provider loads every tool up front and the list costs too much.
pub const TOOL_SEARCH: &str = "tool_search";

/// The tools a tool-search bridge always lists: what a person asks for most.
pub const ALWAYS_ON_TOOL_NAMES: [&str; 5] = ["pane_show", "terminal_run", "session_search", "memory_save", "browser_navigate"];

/// What a `tool_search` query answers at most.
const TOOL_SEARCH_HITS: usize = 6;

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

    /// Steering for the model, advertised in `initialize` as
    /// `instructions` (the terminal relay's D50 steering). Unset by
    /// default: diagnostic bridges say nothing.
    pub fn set_instructions(&mut self, instructions: impl Into<String>) {
        self.instructions = Some(instructions.into());
    }

    /// docs/85 13.7: list only `always_on` (those registered) and
    /// [`TOOL_SEARCH`], which finds the rest by words and runs any by name.
    pub fn set_tool_search(&mut self, always_on: &[&str]) {
        self.always_on = Some(always_on.iter().map(|name| (*name).to_owned()).collect());
    }

    /// The tools a `tools/list` names, in registration order.
    fn listed(&self) -> impl Iterator<Item = &ToolDef> {
        self.tools.iter().filter(|t| self.always_on.as_ref().is_none_or(|on| on.contains(&t.name)))
    }

    /// The `tool_search` matches for `query`: tools ranked by the query's
    /// words found in the name and the description (name hits count
    /// double), the always-on ones left out.
    fn search(&self, query: &str) -> Vec<&ToolDef> {
        let words: Vec<String> = query.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() > 2).map(str::to_owned).collect();
        let mut scored: Vec<(usize, &ToolDef)> = self
            .tools
            .iter()
            .filter(|t| self.always_on.as_ref().is_none_or(|on| !on.contains(&t.name)))
            .map(|t| {
                let (name, text) = (t.name.replace('_', " "), t.description.to_lowercase());
                let score = words.iter().map(|w| usize::from(text.contains(w.as_str())) + 2 * usize::from(name.contains(w.as_str()))).sum();
                (score, t)
            })
            .filter(|(score, _)| *score > 0)
            .collect();
        scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
        scored.into_iter().take(TOOL_SEARCH_HITS).map(|(_, t)| t).collect()
    }
}

/// A tool name as an agent may spell it, back to the bare name the bridge
/// registers: agents carry their own host's prefix over (Claude's
/// `mcp__baaz__pane_show`; Muse once called `mcp__baaz.terminal_open`, owner
/// 2026-11-28), and a call by the prefixed name must still reach the tool.
pub fn bare_tool_name(name: &str) -> &str {
    for prefix in ["mcp__baaz__", "mcp__baaz.", "baaz__", "baaz."] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return rest;
        }
    }
    name
}

/// `tool_search`'s own advertisement.
fn tool_search_def() -> Value {
    json!({
        "name": TOOL_SEARCH,
        "description": "Find and run Baaz's other tools. Showing things in the right pane (the terminal, a file at a \
            line, a diff, what changed, the browser, the pull request) is pane_show, listed on its own: call it directly. \
            This finds the rest: reading or acting on changes (what changed, revert, comment, commit, the pull request's \
            checks), the terminal (open a tab, read the screen, send keys, list or close tabs), the browser (read, click, \
            type, screenshot), past sessions (read one), memory notes (edit), code search (where something is defined or \
            used), and other agents (delegate a task, open a new session with a prompt). Pass query to list matching tools \
            with their arguments; pass call (a tool's name) and arguments to run it.",
        "inputSchema": {"type": "object", "properties": {
            "query": {"type": "string", "description": "What you want to do, in a few words."},
            "call": {"type": "string", "description": "A tool's name to run."},
            "arguments": {"type": "object", "description": "That tool's arguments."},
        }},
    })
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
        "initialize" => Some(success_reply(id, &initialize_result(registry))),
        "tools/list" => Some(success_reply(id, &tools_list_result(registry))),
        "tools/call" => Some(call_tool(id, params, registry)),
        _ => Some(error_reply(id, -32601, &format!("method not found: {method}"))),
    }
}

fn initialize_result(registry: &ToolRegistry) -> Value {
    let mut result = json!({
        "protocolVersion": PROTOCOL_VERSION,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION")},
    });
    if let Some(instructions) = &registry.instructions {
        result["instructions"] = Value::String(instructions.clone());
    }
    result
}

/// The bridge tools that only look — list, read, screen, open a page,
/// read the console and network, screenshot — advertised with the MCP `readOnlyHint`
/// annotation. Codex runs an annotated tool without an approval and Muse
/// allows it by policy (`allow:mcp_read_only_hint`), so in Read-only these
/// keep working while every other bridge tool — unannotated — is refused.
/// The same split as Claude Code's Read-only deny list
/// (`provider_claude_code::argv::READ_ONLY_DENIED_BRIDGE_TOOLS`). Named,
/// never defaulted: a new tool is acting until it is listed here.
pub const READ_ONLY_TOOL_NAMES: [&str; 21] = [
    "terminal_list",
    "terminal_read",
    "terminal_screen",
    "browser_tabs",
    "browser_navigate",
    "browser_snapshot",
    "browser_read",
    "browser_screenshot",
    "browser_wait",
    "browser_fetch",
    "browser_console",
    "browser_network",
    "browser_errors",
    // docs/85 13.1, 13.2, 13.4: show in the pane, read Changes, read the PR.
    "pane_show",
    "changes_read",
    "pr_status",
    // docs/85 14.2: the reads, and a worker's own report and escalation
    // (a reviewer in Read-only must still be able to finish its task).
    "agents_available",
    "task_status",
    "task_wait",
    "report",
    "escalate",
];

/// The bridge tools that can change something beyond the page's view, advertised
/// with `destructiveHint`.
pub const DESTRUCTIVE_TOOL_NAMES: [&str; 3] = ["browser_act", "browser_run", "browser_upload"];

fn tools_list_result(registry: &ToolRegistry) -> Value {
    let mut tools: Vec<Value> = registry
        .listed()
        .map(|t| {
            let mut tool = json!({
                "name": t.name,
                "description": t.description,
                "inputSchema": t.input_schema,
            });
            if READ_ONLY_TOOL_NAMES.contains(&t.name.as_str()) {
                tool["annotations"] = json!({"readOnlyHint": true});
            }
            if DESTRUCTIVE_TOOL_NAMES.contains(&t.name.as_str()) {
                tool["annotations"] = json!({"destructiveHint": true});
            }
            tool
        })
        .collect();
    if registry.always_on.is_some() {
        tools.push(tool_search_def());
    }
    json!({"tools": tools})
}

fn call_tool(id: &Value, params: &Value, registry: &ToolRegistry) -> String {
    let name = params.get("name").and_then(Value::as_str);
    let Some(name) = name else {
        return error_reply(id, -32602, "invalid params: missing tool name");
    };
    let mut arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut name = bare_tool_name(name).to_owned();
    if registry.always_on.is_some() && name == TOOL_SEARCH {
        match arguments.get("call").and_then(Value::as_str).map(str::to_owned) {
            Some(target) => {
                arguments = arguments.get("arguments").cloned().unwrap_or_else(|| json!({}));
                name = bare_tool_name(&target).to_owned();
            }
            None => {
                let query = arguments.get("query").and_then(Value::as_str).unwrap_or("");
                let found: Vec<Value> = registry
                    .search(query)
                    .into_iter()
                    .map(|t| json!({"name": t.name, "description": t.description, "arguments": t.input_schema}))
                    .collect();
                let text = if found.is_empty() {
                    "No tool matches; try other words.".to_owned()
                } else {
                    format!("Run one with tool_search {{call: <name>, arguments: {{...}}}}.\n{}", serde_json::to_string(&found).unwrap_or_default())
                };
                return success_reply(id, &serde_json::to_value(ToolOutcome::text(text)).unwrap_or_else(|_| json!({"content": []})));
            }
        }
    }
    let name = name.as_str();
    let Some(tool) = registry.get(name) else {
        return error_reply(id, -32602, &format!("unknown tool: {name}"));
    };
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

#[cfg(test)]
mod tool_search_tests {
    use super::*;

    /// A registry of every tool family's names, each answering its own name.
    fn full() -> ToolRegistry {
        let mut registry = ToolRegistry::new();
        let any = json!({"type": "object", "properties": {}});
        for (name, description, schema) in crate::pane::tool_defs().into_iter().chain(crate::memory::tool_defs()) {
            let answer = name.clone();
            registry.register(name, description, schema, move |_| Ok(ToolOutcome::text(answer.clone()))).unwrap();
        }
        for name in ["terminal_run", "terminal_read", "browser_navigate", "browser_act"] {
            let answer = name.to_owned();
            registry.register(name, format!("{name} tool"), any.clone(), move |_| Ok(ToolOutcome::text(answer.clone()))).unwrap();
        }
        registry
    }

    fn call(registry: &ToolRegistry, request: Value) -> Value {
        serde_json::from_str(&handle_line(&request.to_string(), registry).expect("a reply")).unwrap()
    }

    /// docs/85 13.7: in tool-search mode the list is the five always-on
    /// tools plus tool_search; a query finds the rest by the words a person
    /// says; a call by name runs a tool the list does not show, through its
    /// own schema check. Without the mode the list is everything.
    #[test]
    fn tool_search_lists_five_finds_the_rest_and_runs_them() {
        let mut registry = full();
        let all = call(&registry, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        assert_eq!(all["result"]["tools"].as_array().unwrap().len(), registry.len());
        registry.set_tool_search(&ALWAYS_ON_TOOL_NAMES);
        let listed = call(&registry, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let names: Vec<&str> = listed["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["pane_show", "session_search", "memory_save", "terminal_run", "browser_navigate", "tool_search"]);
        let found = call(&registry, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "tool_search", "arguments": {"query": "commit and push"}}}));
        let text = found["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("\"changes_commit\""), "{text}");
        assert!(!text.contains("\"pane_show\""), "always-on tools are not search results");
        let ran = call(&registry, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "tool_search", "arguments": {"call": "pr_status", "arguments": {}}}}));
        assert_eq!(ran["result"]["content"][0]["text"], "pr_status");
        let checked = call(&registry, json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "tool_search", "arguments": {"call": "changes_comment", "arguments": {"path": "a"}}}}));
        assert!(checked["error"]["message"].as_str().unwrap().contains("invalid params"), "the hidden tool's own schema holds: {checked}");
        let direct = call(&registry, json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "changes_read", "arguments": {}}}));
        assert_eq!(direct["result"]["content"][0]["text"], "changes_read", "a name the agent already knows still runs");
        let none = call(&registry, json!({"jsonrpc": "2.0", "id": 6, "method": "tools/call", "params": {"name": "tool_search", "arguments": {"query": "zzz qqq"}}}));
        assert!(none["result"]["content"][0]["text"].as_str().unwrap().contains("No tool matches"));
    }

    /// Owner 2026-11-28: a Muse session called `mcp__baaz.terminal_open`
    /// and was refused. A tool reached by its host-prefixed name runs, by
    /// name or through tool_search; an unknown bare name is still refused.
    #[test]
    fn a_prefixed_tool_name_reaches_the_tool() {
        for name in ["mcp__baaz__pane_show", "mcp__baaz.pane_show", "baaz.pane_show", "baaz__pane_show", "pane_show"] {
            assert_eq!(bare_tool_name(name), "pane_show", "{name}");
        }
        let mut registry = full();
        registry.set_tool_search(&ALWAYS_ON_TOOL_NAMES);
        let direct = call(&registry, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "mcp__baaz.terminal_read", "arguments": {}}}));
        assert_eq!(direct["result"]["content"][0]["text"], "terminal_read", "{direct}");
        let searched = call(&registry, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "tool_search", "arguments": {"call": "mcp__baaz__pr_status", "arguments": {}}}}));
        assert_eq!(searched["result"]["content"][0]["text"], "pr_status", "{searched}");
        let unknown = call(&registry, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "mcp__baaz.terminal_teleport", "arguments": {}}}));
        assert!(unknown["error"]["message"].as_str().unwrap().contains("unknown tool: terminal_teleport"), "{unknown}");
    }

    /// Owner 2026-11-28: Muse answered "I have no pane control" while
    /// pane_show was in its list. tool_search's description sends the
    /// right pane to pane_show by name, and pane_show says it is the right
    /// pane; the bridge's instructions tie "open the terminal in the right
    /// pane" to it.
    #[test]
    fn the_right_pane_is_named_on_pane_show_not_hidden_behind_search() {
        let search = tool_search_def()["description"].as_str().unwrap().to_owned();
        assert!(search.contains("pane_show, listed on its own: call it directly"), "{search}");
        let pane = crate::pane::tool_defs().into_iter().find(|(name, _, _)| name == "pane_show").unwrap().1;
        assert!(pane.contains("Baaz's right pane"), "{pane}");
        assert!(crate::pane::INSTRUCTIONS.contains("pane_show view=terminal"));
        assert!(ALWAYS_ON_TOOL_NAMES.contains(&"pane_show"));
    }
}
