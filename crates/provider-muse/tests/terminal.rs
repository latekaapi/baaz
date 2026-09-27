//! The terminal relay's Muse leg (route 1, session MCP) against the
//! recording puppet.
//!
//! The puppet speaks real JSON-RPC over real stdio, so the adapter's whole
//! send path genuinely runs; `--grant-mcp` makes it behave like muse ≥ 1.3
//! (`grantedCapabilities: ["userShell", "sessionMcp"]`), without the flag
//! it behaves like the older servers that never granted the route. Each
//! test asserts on what the puppet recorded — the method AND the full
//! params object — for `session/start` and `session/resume`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use muse_client::{MuseClient, MuseConfig};
use provider::{Ack, Command, ConnectInfo, Provider};
use provider_muse::MuseAdapter;
use serde_json::Value;

fn puppet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py")
}

static SEQ: AtomicU64 = AtomicU64::new(0);

struct Record {
    method: String,
    params: Value,
}

fn read_records(path: &PathBuf) -> Vec<Record> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let value: Value = serde_json::from_str(line).expect("record line is JSON");
            Record {
                method: value.get("method").and_then(Value::as_str).unwrap_or_default().to_owned(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
            }
        })
        .collect()
}

/// Connect through the puppet, waiting out the handshake's fire-and-forget
/// `initialized` tail so nothing `connect` sent is charged to the first
/// command. `grant_mcp` behaves like muse ≥ 1.3; `relay` sets the terminal
/// relay (bridge + socket under `/tmp`) before any session command runs.
fn connect(grant_mcp: bool, relay: bool) -> (Provider, PathBuf) {
    let seq = SEQ.fetch_add(1, Ordering::SeqCst);
    let records = std::env::temp_dir().join(format!(
        "provider-muse-terminal-{seq}-{}-{}.jsonl",
        std::process::id(),
        if grant_mcp { "granted" } else { "plain" }
    ));
    let mut extra = vec!["--record".to_owned(), records.to_string_lossy().into_owned()];
    if grant_mcp {
        extra.push("--grant-mcp".to_owned());
    }
    let client = MuseClient::spawn(&MuseConfig {
        program: puppet(),
        trust_workspace: false,
        no_session_log: false,
        extra_args: extra,
    })
    .expect("fake server spawns — is python3 on PATH?");
    let adapter = MuseAdapter::new(client);
    if relay {
        adapter.set_terminal_relay(
            PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
            PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
        );
    }
    let mut provider = Provider::new(adapter);
    provider.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let all = read_records(&records);
        if all.iter().any(|record| record.method == "initialized") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "connect's `initialized` notification never reached the puppet"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    (provider, records)
}

fn last_params(records: &PathBuf, method: &str) -> Value {
    read_records(records)
        .into_iter()
        .rev()
        .find(|record| record.method == method)
        .unwrap_or_else(|| panic!("puppet recorded no {method}"))
        .params
}

fn open_session(provider: &Provider) -> Ack {
    provider
        .send(Command::OpenSession {
            request_id: "cmd-open".into(),
            workspace: Some("/tmp/w".into()),
            model: None,
            model_provider: None,
        })
        .expect("open lands")
}

fn resume_session(provider: &Provider) -> Ack {
    provider
        .send(Command::ResumeSession {
            request_id: "cmd-resume".into(),
            session_id: "s-1".into(),
            cursor: None,
            metadata_only: false,
        })
        .expect("resume lands")
}

#[test]
fn granted_open_and_resume_carry_the_bridge() {
    let (provider, records) = connect(true, true);
    match open_session(&provider) {
        Ack::Session { session_id, .. } => assert_eq!(session_id, "s-1"),
        other => panic!("open must ack a session, got {other:?}"),
    }
    let params = last_params(&records, "session/start");
    // The open mints its session id client-side from the request id, so
    // the host could register it with the service before the send.
    assert_eq!(
        params.get("sessionId").and_then(Value::as_str),
        Some("cmd-open"),
        "granted open mints the session id: {params}"
    );
    let server = &params["config"]["mcpServers"]["baaz"];
    assert_eq!(server["transport"], "stdio", "the bridge is a stdio server: {params}");
    assert_eq!(server["command"], "/tmp/baaz/bin/mcp-bridge", "the bridge binary: {params}");
    let args: Vec<String> = serde_json::from_value(server["args"].clone()).expect("args array");
    assert_eq!(
        args,
        vec!["--terminal", "--socket", "/tmp/baaz/run/terminal-9.sock", "--session", "cmd-open"],
        "the bridge answers for the minted session: {params}"
    );
    assert_eq!(server["mode"], "required", "a broken bridge fails loud: {params}");

    match resume_session(&provider) {
        Ack::Session { session_id, .. } => assert_eq!(session_id, "s-1"),
        other => panic!("resume must ack a session, got {other:?}"),
    }
    let params = last_params(&records, "session/resume");
    assert_eq!(
        params.get("sessionId").and_then(Value::as_str),
        Some("s-1"),
        "resume never renames the session: {params}"
    );
    let server = &params["config"]["mcpServers"]["baaz"];
    assert_eq!(server["command"], "/tmp/baaz/bin/mcp-bridge", "the resume carries the bridge");
    let args: Vec<String> = serde_json::from_value(server["args"].clone()).expect("args array");
    assert!(args.contains(&"s-1".to_owned()), "the bridge answers for the resumed session");
}

#[test]
fn ungranted_sessions_send_what_they_always_sent() {
    // The relay is set, but the old server never granted the route: no
    // minted id, no `config` key — byte-identical to the pre-route shape.
    let (provider, records) = connect(false, true);
    match open_session(&provider) {
        Ack::Session { .. } => {}
        other => panic!("open must ack a session, got {other:?}"),
    }
    let params = last_params(&records, "session/start");
    assert!(
        params.get("sessionId").is_none_or(Value::is_null),
        "ungranted open mints nothing: {params}"
    );
    assert!(
        params.get("config").is_none_or(Value::is_null),
        "ungranted open carries no bridge: {params}"
    );
    match resume_session(&provider) {
        Ack::Session { .. } => {}
        other => panic!("resume must ack a session, got {other:?}"),
    }
    let params = last_params(&records, "session/resume");
    assert!(
        params.get("config").is_none_or(Value::is_null),
        "ungranted resume carries no bridge: {params}"
    );
}

#[test]
fn granted_without_a_relay_sends_what_it_always_sent() {
    // The grant alone opens no route: with no relay set there is nothing
    // to point the bridge at, so the params stay bare.
    let (provider, records) = connect(true, false);
    match open_session(&provider) {
        Ack::Session { .. } => {}
        other => panic!("open must ack a session, got {other:?}"),
    }
    let params = last_params(&records, "session/start");
    assert!(params.get("config").is_none_or(Value::is_null));
    assert!(params.get("sessionId").is_none_or(Value::is_null));
}
