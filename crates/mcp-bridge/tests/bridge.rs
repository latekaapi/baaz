//! Bridge tests, always through the real `serve_loop` path on in-memory
//! buffers: what the provider would send on a pipe, answered the same way.

use std::io::{BufReader, Cursor};

use mcp_bridge::{
    PROTOCOL_VERSION, ToolOutcome, ToolRegistry, mcp_config_value, serve_loop,
};
use serde_json::{Value, json};

fn test_registry() -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry
        .register(
            "ping",
            "Replies PONG.",
            json!({"type": "object", "properties": {}}),
            |_| Ok(ToolOutcome::text("PONG")),
        )
        .unwrap();
    registry
        .register(
            "echo",
            "Echoes its message argument.",
            json!({
                "type": "object",
                "required": ["message"],
                "properties": {"message": {"type": "string"}},
            }),
            |args| {
                let message = args
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Ok(ToolOutcome::text(message))
            },
        )
        .unwrap();
    registry
        .register(
            "boom",
            "Panics, so the loop can prove it survives.",
            json!({"type": "object", "properties": {}}),
            |_| panic!("boom for the survival test"),
        )
        .unwrap();
    registry
}

/// Drive `lines` through one `serve_loop` session; one parsed reply per line
/// the server owed a response to.
fn run_session(lines: &[&str], registry: &ToolRegistry) -> Vec<Value> {
    let input = lines.join("\n") + "\n";
    let mut output = Vec::new();
    serve_loop(BufReader::new(Cursor::new(input)), &mut output, registry)
        .expect("in-memory loop runs");
    let text = String::from_utf8(output).expect("replies are UTF-8");
    text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("reply line is JSON"))
        .collect()
}

fn id_of(reply: &Value) -> &Value {
    reply.get("id").expect("reply carries an id")
}

fn result_of(reply: &Value) -> &Value {
    reply.get("result").expect("reply is a success")
}

fn error_of(reply: &Value) -> &Value {
    reply.get("error").expect("reply is an error")
}

#[test]
fn initialize_advertises_protocol_and_tools() {
    let registry = test_registry();
    let replies = run_session(
        &[r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#],
        &registry,
    );
    assert_eq!(replies.len(), 1);
    let result = result_of(&replies[0]);
    assert_eq!(
        result.get("protocolVersion").and_then(Value::as_str),
        Some(PROTOCOL_VERSION)
    );
    assert!(result.pointer("/capabilities/tools").is_some());
    assert!(result.get("serverInfo").is_some());
}

#[test]
fn tools_list_reports_every_tool_with_schema() {
    let registry = test_registry();
    let replies = run_session(
        &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#],
        &registry,
    );
    assert_eq!(replies.len(), 1);
    let tools =
        result_of(&replies[0]).get("tools").and_then(Value::as_array).unwrap();
    let names: Vec<&str> =
        tools.iter().filter_map(|t| t.get("name").and_then(Value::as_str)).collect();
    assert_eq!(names, vec!["ping", "echo", "boom"]);
    let echo = tools.iter().find(|t| t.get("name") == Some(&json!("echo"))).unwrap();
    assert_eq!(
        echo.get("inputSchema"),
        Some(&json!({
            "type": "object",
            "required": ["message"],
            "properties": {"message": {"type": "string"}},
        }))
    );
    assert!(echo.get("description").and_then(Value::as_str).is_some());
}

#[test]
fn tools_call_returns_tool_content() {
    let registry = test_registry();
    let replies = run_session(
        &[r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#],
        &registry,
    );
    assert_eq!(replies.len(), 1);
    assert_eq!(*id_of(&replies[0]), json!(7));
    assert_eq!(
        result_of(&replies[0]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn full_provider_sequence_on_one_loop() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"echo","arguments":{"message":"hi"}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 3);
    assert_eq!(
        result_of(&replies[0]).get("protocolVersion").and_then(Value::as_str),
        Some(PROTOCOL_VERSION)
    );
    assert!(
        result_of(&replies[1]).get("tools").and_then(Value::as_array).unwrap().len()
            == 3
    );
    assert_eq!(
        result_of(&replies[2]).get("content"),
        Some(&json!([{"type": "text", "text": "hi"}]))
    );
}

#[test]
fn non_json_line_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            "this is not json",
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(error_of(&replies[0]).get("code"), Some(&json!(-32700)));
    assert_eq!(
        result_of(&replies[1]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn request_without_method_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":4,"params":{}}"#,
            r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(*id_of(&replies[0]), json!(4));
    assert!(error_of(&replies[0]).get("code").is_some());
    assert_eq!(
        result_of(&replies[1]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn unknown_method_with_id_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":9,"method":"nope/nothing","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":10,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(error_of(&replies[0]).get("code"), Some(&json!(-32601)));
    assert_eq!(
        result_of(&replies[1]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn unknown_tool_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":11,"method":"tools/call","params":{"name":"ghost","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2);
    assert!(error_of(&replies[0]).get("message").is_some());
    assert_eq!(
        result_of(&replies[1]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn schema_mismatch_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":13,"method":"tools/call","params":{"name":"echo","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":14,"method":"tools/call","params":{"name":"echo","arguments":{"message":42}}}"#,
            r#"{"jsonrpc":"2.0","id":15,"method":"tools/call","params":{"name":"echo","arguments":{"message":"back"}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 3);
    assert!(error_of(&replies[0]).get("message").is_some());
    assert!(error_of(&replies[1]).get("message").is_some());
    assert_eq!(
        result_of(&replies[2]).get("content"),
        Some(&json!([{"type": "text", "text": "back"}]))
    );
}

#[test]
fn notification_gets_no_reply_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":16,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 1);
    assert_eq!(*id_of(&replies[0]), json!(16));
    assert_eq!(
        result_of(&replies[0]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn an_explicit_null_id_is_a_request_and_gets_a_reply() {
    // JSON-RPC 2.0 makes a request a notification by the ABSENCE of `id`,
    // not by a null value. Treating `"id": null` as a notification strands
    // the caller: it waits on a reply that is never written, and because
    // one process serves the whole provider session, the session waits too.
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":null,"method":"tools/list","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":19,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2, "an explicit null id is a request, not a notification");
    assert_eq!(*id_of(&replies[0]), json!(null), "the null id is echoed as null");
    assert!(result_of(&replies[0]).get("tools").is_some());
    assert_eq!(*id_of(&replies[1]), json!(19));
}

#[test]
fn panicking_handler_errors_and_loop_survives() {
    let registry = test_registry();
    let replies = run_session(
        &[
            r#"{"jsonrpc":"2.0","id":17,"method":"tools/call","params":{"name":"boom","arguments":{}}}"#,
            r#"{"jsonrpc":"2.0","id":18,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#,
        ],
        &registry,
    );
    assert_eq!(replies.len(), 2);
    assert_eq!(error_of(&replies[0]).get("code"), Some(&json!(-32603)));
    assert_eq!(
        result_of(&replies[1]).get("content"),
        Some(&json!([{"type": "text", "text": "PONG"}]))
    );
}

#[test]
fn duplicate_registration_is_refused() {
    let mut registry = ToolRegistry::new();
    registry
        .register(
            "ping",
            "First.",
            json!({"type": "object", "properties": {}}),
            |_| Ok(ToolOutcome::text("one")),
        )
        .unwrap();
    let refused = registry.register(
        "ping",
        "Second.",
        json!({"type": "object", "properties": {}}),
        |_| Ok(ToolOutcome::text("two")),
    );
    assert!(refused.is_err());
    // The original stays: no silent last-wins.
    let replies = run_session(
        &[r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"ping","arguments":{}}}"#],
        &registry,
    );
    assert_eq!(
        result_of(&replies[0]).get("content"),
        Some(&json!([{"type": "text", "text": "one"}]))
    );
}

#[test]
fn config_value_matches_probe_shape() {
    let value = mcp_config_value(
        "baazprobe",
        "python3",
        &["/tmp/ccprobe/mcpsrv.py".to_string()],
    );
    assert_eq!(
        value,
        json!({"mcpServers": {"baazprobe": {
            "type": "stdio",
            "command": "python3",
            "args": ["/tmp/ccprobe/mcpsrv.py"],
        }}})
    );
}

#[test]
fn config_value_does_not_hardcode_the_name() {
    let value =
        mcp_config_value("baaz", "/usr/local/bin/mcp-bridge", &["--quiet".to_string()]);
    assert_eq!(
        value.pointer("/mcpServers/baaz/command"),
        Some(&json!("/usr/local/bin/mcp-bridge"))
    );
    assert!(value.pointer("/mcpServers/baazprobe").is_none());
}
