//! The terminal relay over a test socket: tools listed with schemas and
//! steering, calls forwarded to Baaz's socket, absence answered — all
//! through the real `serve_loop` path on in-memory buffers, against a
//! background thread speaking the service's `{id, session, tool, params}`
//! protocol. `ping` keeps working beside the seven tools.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcp_bridge::{
    ToolOutcome, ToolRegistry, serve_loop,
    terminal::{INSTRUCTIONS, TOOL_NAMES, TerminalTarget, UNAVAILABLE, register_terminal_tools},
};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(1);

/// What the test socket heard: one entry per forwarded call.
#[derive(Debug, Clone)]
struct Heard {
    tool: String,
    session: String,
    params: Value,
}

/// A scripted Baaz: answers every request, records what arrived, refuses
/// the configured tool like the service refuses user-owned closes.
struct Script {
    heard: Arc<Mutex<Vec<Heard>>>,
    refuse_tool: Option<String>,
    shutdown: Arc<AtomicBool>,
}

fn socket_path() -> std::path::PathBuf {
    // Short on purpose: unix socket paths die past ~104 bytes (SUN_LEN).
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::path::PathBuf::from(format!("/tmp/bmr-{}-{n}.sock", std::process::id()))
}

fn serve(path: &std::path::Path, script: &Script) -> std::thread::JoinHandle<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("test socket binds");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let heard = script.heard.clone();
    let refuse_tool = script.refuse_tool.clone();
    let shutdown = script.shutdown.clone();
    std::thread::spawn(move || {
        while !shutdown.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let mut reader =
                        BufReader::new(stream.try_clone().expect("socket clones"));
                    let mut stream = stream;
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                        if line.trim().is_empty() {
                            continue;
                        }
                        let request: Value = serde_json::from_str(&line).expect("request parses");
                        let id = request.get("id").cloned().unwrap_or(Value::Null);
                        let tool =
                            request.get("tool").and_then(Value::as_str).unwrap_or("").to_owned();
                        let session =
                            request.get("session").and_then(Value::as_str).unwrap_or("").to_owned();
                        let params = request.get("params").cloned().unwrap_or(Value::Null);
                        heard.lock().expect("heard").push(Heard {
                            tool: tool.clone(),
                            session,
                            params: params.clone(),
                        });
                        let reply = if refuse_tool.as_deref() == Some(tool.as_str()) {
                            json!({"id": id, "ok": false, "error": {"error": "tab t1 is owned by the user"}})
                        } else {
                            json!({"id": id, "ok": true, "result": {"tool": tool, "params": params}})
                        };
                        let line = serde_json::to_string(&reply).expect("reply serializes");
                        if stream.write_all(line.as_bytes()).is_err()
                            || stream.write_all(b"\n").is_err()
                        {
                            break;
                        }
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
    })
}

fn registry_for(socket: &std::path::Path, session: &str) -> ToolRegistry {
    let mut registry = ToolRegistry::new();
    registry.set_instructions(INSTRUCTIONS);
    register_terminal_tools(
        &mut registry,
        &TerminalTarget { socket: socket.to_owned(), session: session.to_owned() },
    );
    registry
        .register(
            "ping",
            "Diagnostic ping: replies PONG.",
            json!({"type": "object", "properties": {}}),
            |_| Ok(ToolOutcome::text("PONG")),
        )
        .expect("fresh registry takes ping");
    registry
}

/// Drive `lines` through one `serve_loop` session; one parsed reply per
/// line the server owed a response to.
fn run_session(lines: &[String], registry: &ToolRegistry) -> Vec<Value> {
    let input = lines.join("\n") + "\n";
    let mut output = Vec::new();
    serve_loop(BufReader::new(Cursor::new(input)), &mut output, registry)
        .expect("in-memory loop runs");
    String::from_utf8(output)
        .expect("replies are UTF-8")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("reply line is JSON"))
        .collect()
}

fn rpc(id: u64, method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0", "id": id, "method": method, "params": params,
    }))
    .expect("request serializes")
}

fn result_of(reply: &Value) -> &Value {
    reply.get("result").unwrap_or_else(|| panic!("reply is a success: {reply}"))
}

fn error_of(reply: &Value) -> &Value {
    reply.get("error").unwrap_or_else(|| panic!("reply is an error: {reply}"))
}

fn content_text(result: &Value) -> &str {
    result
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .expect("one text block")
}

#[test]
fn initialize_carries_the_steering_instructions() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), refuse_tool: None, shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s1");
    let replies = run_session(&[rpc(1, "initialize", json!({}))], &registry);
    let instructions =
        result_of(&replies[0]).get("instructions").and_then(Value::as_str).expect("instructions");
    assert_eq!(instructions, INSTRUCTIONS);
    assert!(
        instructions.contains("Use the terminal"),
        "the D50 steering travels with initialize: {instructions}"
    );
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn tools_list_advertises_the_seven_tools_with_schemas() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), refuse_tool: None, shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s1");
    let replies = run_session(&[rpc(1, "tools/list", json!({}))], &registry);
    let tools = result_of(&replies[0]).get("tools").and_then(Value::as_array).expect("tools");
    let names: Vec<&str> =
        tools.iter().filter_map(|tool| tool.get("name").and_then(Value::as_str)).collect();
    for name in TOOL_NAMES {
        assert!(names.contains(&name), "listed: {names:?}");
    }
    assert!(names.contains(&"ping"), "ping stays: {names:?}");
    assert_eq!(TOOL_NAMES.len(), 7);
    for tool in tools {
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
        assert!(
            tool.get("description").and_then(Value::as_str).is_some_and(|d| !d.is_empty()),
            "{name} is described"
        );
        assert!(tool.get("inputSchema").is_some(), "{name} has a schema");
    }
    let run = tools.iter().find(|tool| tool.get("name") == Some(&json!("terminal_run"))).expect("run");
    assert!(
        run.pointer("/inputSchema/required").and_then(Value::as_array).is_some_and(|required| required
            .iter()
            .any(|key| key == "command")),
        "run requires a command: {run}"
    );
    assert!(
        run.get("description")
            .and_then(Value::as_str)
            .is_some_and(|d| d.contains("long-running")),
        "run steers per D50: {}",
        run.get("description").unwrap_or(&Value::Null)
    );
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn tools_call_forwards_session_tool_and_params() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), refuse_tool: None, shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s9");
    let replies = run_session(
        &[rpc(
            1,
            "tools/call",
            json!({"name": "terminal_run", "arguments": {"command": "echo hi", "wait": "none"}}),
        )],
        &registry,
    );
    let heard = script.heard.lock().expect("heard");
    assert_eq!(heard.len(), 1, "one socket call: {heard:?}");
    assert_eq!(heard[0].tool, "terminal_run");
    assert_eq!(heard[0].session, "s9", "the registered session travels");
    assert_eq!(heard[0].params, json!({"command": "echo hi", "wait": "none"}), "args pass through");
    drop(heard);
    let text = content_text(result_of(&replies[0]));
    let echoed: Value = serde_json::from_str(text).expect("result is JSON text");
    assert_eq!(echoed.get("tool"), Some(&json!("terminal_run")));
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn a_served_refusal_is_a_tool_error() {
    let path = socket_path();
    let script = Script {
        heard: Arc::default(),
        refuse_tool: Some("terminal_close".to_owned()),
        shutdown: Arc::default(),
    };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s1");
    let replies = run_session(
        &[rpc(1, "tools/call", json!({"name": "terminal_close", "arguments": {"tab": "t1"}}))],
        &registry,
    );
    let message = error_of(&replies[0]).to_string();
    assert!(message.contains("owned by the user"), "the refusal surfaces: {message}");
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn a_gone_socket_answers_unavailable_not_a_crash() {
    let missing = socket_path();
    assert!(UnixStream::connect(&missing).is_err(), "nothing serves this path");
    let registry = registry_for(&missing, "s1");
    // Valid args throughout: schema validation runs before the handler,
    // so invalid args would fail without ever touching the socket.
    let args_for = |name: &str| match name {
        "terminal_run" => json!({"command": "true"}),
        "terminal_read" | "terminal_screen" | "terminal_send" | "terminal_close" => {
            json!({"tab": "t1"})
        }
        _ => json!({}),
    };
    let mut lines: Vec<String> = TOOL_NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            rpc(i as u64 + 1, "tools/call", json!({"name": name, "arguments": args_for(name)}))
        })
        .collect();
    lines.push(rpc(100, "tools/call", json!({"name": "ping", "arguments": {}})));
    let replies = run_session(&lines, &registry);
    for (i, name) in TOOL_NAMES.iter().enumerate() {
        assert_eq!(
            content_text(result_of(&replies[i])),
            UNAVAILABLE,
            "{name} answers unavailable, not a crash"
        );
    }
    // `ping` is the bridge's own diagnostic, not a forwarded tool: it
    // answers even with Baaz gone.
    assert_eq!(content_text(result_of(&replies[TOOL_NAMES.len()])), "PONG");
}

#[test]
fn ping_answers_beside_the_terminal_tools() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), refuse_tool: None, shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let mut registry = ToolRegistry::new();
    register_terminal_tools(
        &mut registry,
        &TerminalTarget { socket: path.clone(), session: "s1".to_owned() },
    );
    registry
        .register(
            "ping",
            "Diagnostic ping: replies PONG.",
            json!({"type": "object", "properties": {}}),
            |_| Ok(ToolOutcome::text("PONG")),
        )
        .expect("fresh registry takes ping");
    let replies =
        run_session(&[rpc(1, "tools/call", json!({"name": "ping", "arguments": {}}))], &registry);
    assert_eq!(content_text(result_of(&replies[0])), "PONG");
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}
