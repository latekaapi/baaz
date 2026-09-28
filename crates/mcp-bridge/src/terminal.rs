//! The terminal relay: Baaz's seven terminal tools as MCP, forwarded to
//! the app's unix socket.
//!
//! The relay owns no terminal state: each `tools/call` opens the socket,
//! sends one `{id, session, tool, params}` line, and renders the
//! `{id, ok, result | error}` reply as MCP content. When Baaz is not
//! serving its socket — the app is closed, or this session was never
//! registered — every tool answers with [`UNAVAILABLE`] rather than
//! failing: the model can report that and move on.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{ToolOutcome, ToolRegistry};

/// What every tool reports when the socket is gone.
pub const UNAVAILABLE: &str = "Baaz isn't running; the terminal is unavailable";

/// What a `browser_*` tool reports when the socket is gone: the same answer,
/// naming the browser — seen live, a browser tool told the model "the
/// terminal is unavailable" and it apologised for the wrong thing.
pub const BROWSER_UNAVAILABLE: &str = "Baaz isn't running; the browser is unavailable";

/// The unavailable answer for `tool`.
pub fn unavailable_for(tool: &str) -> &'static str {
    if tool.starts_with("browser_") { BROWSER_UNAVAILABLE } else { UNAVAILABLE }
}

/// The `instructions` string: steering per D50, shared with the tool
/// descriptions below.
pub const INSTRUCTIONS: &str = "Baaz's terminal tools drive the person's visible terminal: \
    every command the agent runs there the person can watch. Use the terminal when the user \
    says terminal, for long-running or interactive commands, or anything the user should \
    watch; use the shell tool for quick captured checks. If a terminal tool reports that \
    Baaz isn't running, the terminal is unavailable — say so and carry on without it.";

/// The seven tools, in contract order (`docs/14-terminal.md` §4).
pub const TOOL_NAMES: [&str; 7] = [
    "terminal_list",
    "terminal_open",
    "terminal_run",
    "terminal_read",
    "terminal_screen",
    "terminal_send",
    "terminal_close",
];

/// Where to forward calls: the socket path and the session id Baaz
/// registered for this agent.
#[derive(Debug, Clone)]
pub struct TerminalTarget {
    /// `<support_dir>/run/terminal-<pid>.sock`.
    pub socket: PathBuf,
    /// The session id the app registered (`register_session`).
    pub session: String,
}

/// One tool's advertisement: name, description, JSON Schema.
pub fn tool_defs() -> Vec<(String, String, Value)> {
    vec![
        (
            "terminal_list".to_owned(),
            "List this project's terminal tabs: id, title, owner, busy state. Read-only.".to_owned(),
            json!({"type": "object", "properties": {}}),
        ),
        (
            "terminal_open".to_owned(),
            "Open a new agent-owned terminal tab. It belongs to the project and the person can see it. \
            `cwd` may name any directory the user could open themselves (same privilege as the user); \
            it defaults to the project root."
                .to_owned(),
            json!({"type": "object", "properties": {
                "cwd": {"type": "string", "description": "Starting directory, any directory; defaults to the project root."},
                "title": {"type": "string", "description": "Tab title; defaults to shell."},
            }}),
        ),
        (
            "terminal_run".to_owned(),
            "Run a command in the terminal the person can watch. Use this when the user says \
            terminal, for long-running or interactive commands, or anything the user should watch; \
            use the shell tool for quick captured checks. `tab` is auto (the idle active tab, else \
            a new agent tab), new, or a tab id. `wait: exit` waits for the command (a timeout leaves \
            it running; follow with terminal_read); `wait: none` returns at once."
                .to_owned(),
            json!({"type": "object", "required": ["command"], "properties": {
                "command": {"type": "string"},
                "tab": {"type": "string", "description": "auto (default), new, or a tab id."},
                "wait": {"type": "string", "enum": ["exit", "none"]},
                "timeout_ms": {"type": "integer", "description": "Wait at most this long (default 30000, max 600000)."},
            }}),
        ),
        (
            "terminal_read".to_owned(),
            "Read a tab's output: a finished or running block, or what is new since a cursor. Read-only."
                .to_owned(),
            json!({"type": "object", "required": ["tab"], "properties": {
                "tab": {"type": "string"},
                "block": {"type": "string", "description": "Block id (t1:7) or index; omit for new output."},
                "since": {"type": "integer", "description": "Cursor from an earlier read or run; omit for all retained output."},
                "max_bytes": {"type": "integer", "description": "Cap (default 4096, max 32768)."},
            }}),
        ),
        (
            "terminal_screen".to_owned(),
            "The tab's visible grid as text, for prompts and TUIs. Read-only.".to_owned(),
            json!({"type": "object", "required": ["tab"], "properties": {
                "tab": {"type": "string"},
            }}),
        ),
        (
            "terminal_send".to_owned(),
            "Type into a tab: pasted text and/or named keys (answer prompts, interrupt with ctrl-c). \
            Tabs the person opened take only answers to a prompt of a command the agent ran there."
                .to_owned(),
            json!({"type": "object", "required": ["tab"], "properties": {
                "tab": {"type": "string"},
                "text": {"type": "string"},
                "keys": {"description": "A key name or list of key names: enter, tab, esc, up, down, left, right, backspace, ctrl-c, ctrl-d, ctrl-z, ctrl-l."},
            }}),
        ),
        (
            "terminal_close".to_owned(),
            "Close an agent-opened tab. Refused for tabs the person opened.".to_owned(),
            json!({"type": "object", "required": ["tab"], "properties": {
                "tab": {"type": "string"},
            }}),
        ),
    ]
}

/// Register the seven tools, each forwarding to `target`.
pub fn register_terminal_tools(registry: &mut ToolRegistry, target: &TerminalTarget) {
    for (name, description, schema) in tool_defs() {
        let target = target.clone();
        let tool = name.clone();
        registry
            .register(name, description, schema, move |args| {
                forward_call(&target.socket, &target.session, &tool, &args)
            })
            .expect("fresh registry takes the terminal tools");
    }
}

/// Forward one call to the socket and render the reply as MCP content.
///
/// The socket being gone is an answer, not an error ([`UNAVAILABLE`]): the
/// model reports it and carries on. A served refusal (unknown session, a
/// busy tab, a user-owned close) is an error, like any failed tool.
pub fn forward_call(
    socket: &Path,
    session: &str,
    tool: &str,
    args: &Value,
) -> Result<ToolOutcome, String> {
    let stream = match UnixStream::connect(socket) {
        Ok(stream) => stream,
        Err(_) => return Ok(ToolOutcome::text(unavailable_for(tool))),
    };
    let request = serde_json::to_string(&json!({
        "id": 1,
        "session": session,
        "tool": tool,
        "params": args,
    }))
    .expect("request serializes");
    if stream.set_write_timeout(Some(Duration::from_secs(30))).is_err()
        || stream.set_read_timeout(Some(Duration::from_millis(660_000))).is_err()
    {
        return Ok(ToolOutcome::text(unavailable_for(tool)));
    }
    let mut stream = stream;
    if stream.write_all(request.as_bytes()).is_err()
        || stream.write_all(b"\n").is_err()
        || stream.flush().is_err()
    {
        return Ok(ToolOutcome::text(unavailable_for(tool)));
    }
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() || line.trim().is_empty() {
        return Ok(ToolOutcome::text(unavailable_for(tool)));
    }
    let reply: Value = serde_json::from_str(&line).map_err(|e| format!("bad terminal reply: {e}"))?;
    if reply.get("ok") == Some(&Value::Bool(true)) {
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        let text = serde_json::to_string(&result).unwrap_or_else(|_| "null".to_owned());
        Ok(ToolOutcome::text(text))
    } else {
        Err(reply
            .pointer("/error/error")
            .and_then(Value::as_str)
            .or_else(|| reply.get("error").and_then(Value::as_str))
            .unwrap_or("the terminal refused the call")
            .to_owned())
    }
}
