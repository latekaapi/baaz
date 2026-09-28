//! The browser relay over a test socket: the six tools forward like the
//! terminal ones, and a screenshot answer renders as an MCP image block.

use std::io::{BufRead, BufReader, Cursor, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcp_bridge::{
    ToolOutcome, ToolRegistry, browser as browser_relay, serve_loop,
    terminal::TerminalTarget,
};
use mcp_bridge::browser::{BROWSER_TOOL_NAMES, register_browser_tools};
use serde_json::{Value, json};

static NEXT: AtomicU64 = AtomicU64::new(1_000);

/// What the test socket heard: one entry per forwarded call.
#[derive(Debug, Clone)]
struct Heard {
    tool: String,
    session: String,
    params: Value,
}

/// A scripted Baaz: records what arrived and answers from a canned
/// per-tool result — image bytes for the screenshot, an echo otherwise.
struct Script {
    heard: Arc<Mutex<Vec<Heard>>>,
    shutdown: Arc<AtomicBool>,
}

fn socket_path() -> std::path::PathBuf {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    std::path::PathBuf::from(format!("/tmp/bmb-{}-{n}.sock", std::process::id()))
}

fn serve(path: &std::path::Path, script: &Script) -> std::thread::JoinHandle<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).expect("test socket binds");
    listener.set_nonblocking(true).expect("nonblocking accept");
    let heard = script.heard.clone();
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
                        let result = if tool == "browser_screenshot" {
                            // base64("fake-png"): the service's PNG bytes,
                            // stood in for by eight ASCII bytes here.
                            json!({"image_base64": "ZmFrZS1wbmc="})
                        } else {
                            json!({"tool": tool, "params": params})
                        };
                        let reply = json!({"id": id, "ok": true, "result": result});
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
    registry.set_instructions(browser_relay::INSTRUCTIONS);
    register_browser_tools(
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

#[test]
fn the_six_tools_are_described_and_steered() {
    assert_eq!(BROWSER_TOOL_NAMES.len(), 6);
    let defs = mcp_bridge::browser::tool_defs();
    assert_eq!(defs.len(), 6);
    for (name, description, schema) in &defs {
        assert!(
            BROWSER_TOOL_NAMES.contains(&name.as_str()),
            "defined tool is advertised: {name}"
        );
        assert!(
            description.contains("person sees this browser"),
            "{name} steers the model: {description}"
        );
        assert!(schema.get("type").is_some(), "{name} has a schema");
    }
    let open = defs.iter().find(|(name, _, _)| name == "browser_open").expect("open");
    assert!(
        open.2.pointer("/required").and_then(Value::as_array).is_some_and(|required| required
            .iter()
            .any(|key| key == "url")),
        "open requires a url: {}",
        open.2
    );
    let click = defs.iter().find(|(name, _, _)| name == "browser_click").expect("click");
    assert!(
        click.2.pointer("/required").and_then(Value::as_array).is_some_and(|required| required
            .iter()
            .any(|key| key == "selector")),
        "click requires a selector: {}",
        click.2
    );
}

#[test]
fn browser_calls_forward_session_tool_and_params() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s7");
    let replies = run_session(
        &[rpc(
            1,
            "tools/call",
            json!({"name": "browser_open", "arguments": {"url": "https://example.com"}}),
        )],
        &registry,
    );
    let heard = script.heard.lock().expect("heard");
    assert_eq!(heard.len(), 1, "one socket call: {heard:?}");
    assert_eq!(heard[0].tool, "browser_open");
    assert_eq!(heard[0].session, "s7", "the calling session travels");
    assert_eq!(heard[0].params, json!({"url": "https://example.com"}));
    drop(heard);
    let text = result_of(&replies[0])
        .pointer("/content/0/text")
        .and_then(Value::as_str)
        .expect("one text block");
    let echoed: Value = serde_json::from_str(text).expect("result is JSON text");
    assert_eq!(echoed.get("tool"), Some(&json!("browser_open")));
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn the_screenshot_answer_renders_as_an_mcp_image_block() {
    let path = socket_path();
    let script =
        Script { heard: Arc::default(), shutdown: Arc::default() };
    let worker = serve(&path, &script);
    let registry = registry_for(&path, "s7");
    let replies = run_session(
        &[rpc(1, "tools/call", json!({"name": "browser_screenshot", "arguments": {}}))],
        &registry,
    );
    let block = result_of(&replies[0]).pointer("/content/0").expect("one block");
    assert_eq!(block.get("type"), Some(&json!("image")), "an image block: {block}");
    assert_eq!(block.get("mimeType"), Some(&json!("image/png")));
    assert_eq!(
        block.get("data"),
        Some(&json!("ZmFrZS1wbmc=")),
        "the service's PNG bytes travel untouched: {block}"
    );
    script.shutdown.store(true, Ordering::Release);
    worker.join().expect("server stops");
}

#[test]
fn a_gone_socket_answers_unavailable_for_browser_tools() {
    use mcp_bridge::terminal::BROWSER_UNAVAILABLE;
    let missing = socket_path();
    assert!(UnixStream::connect(&missing).is_err(), "nothing serves this path");
    let registry = registry_for(&missing, "s1");
    let args_for = |name: &str| match name {
        "browser_open" => json!({"url": "https://example.com"}),
        "browser_click" => json!({"selector": "h1"}),
        "browser_type" => json!({"selector": "input", "text": "hi"}),
        _ => json!({}),
    };
    let lines: Vec<String> = BROWSER_TOOL_NAMES
        .iter()
        .enumerate()
        .map(|(i, name)| {
            rpc(i as u64 + 1, "tools/call", json!({"name": name, "arguments": args_for(name)}))
        })
        .collect();
    let replies = run_session(&lines, &registry);
    for (i, name) in BROWSER_TOOL_NAMES.iter().enumerate() {
        let text = result_of(&replies[i])
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .expect("one text block");
        assert_eq!(text, BROWSER_UNAVAILABLE, "{name} answers the browser is unavailable, not a crash");
    }
}
