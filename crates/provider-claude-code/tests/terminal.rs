//! The terminal relay's Claude Code leg: every launch carries the bridge.
//!
//! All offline: the launch builders write the per-session MCP config file
//! and mint the argv, but never spawn — `claude` is never executed. Each
//! test points the relay at a temp dir and asserts the argv names the
//! written file (with `--strict-mcp-config` beside it, so the session sees
//! Baaz's tools and nothing else) and the file names the bridge, the
//! socket and the session.

use std::path::PathBuf;

use provider_claude_code::{ClaudeCodeAdapter, TerminalRelay};

fn relay(dir: &PathBuf) -> TerminalRelay {
    TerminalRelay {
        bridge: PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
        socket: PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
        config_dir: dir.clone(),
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cc-terminal-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn mcp_config_path(argv: &[String]) -> &String {
    let position = argv.iter().position(|arg| arg == "--mcp-config").expect("--mcp-config");
    assert!(
        argv.iter().any(|arg| arg == "--strict-mcp-config"),
        "strict rides with the config, or the session inherits the operator's connectors: {argv:?}"
    );
    &argv[position + 1]
}

#[test]
fn open_resume_and_fork_all_carry_the_bridge() {
    let dir = temp_dir("launches");
    let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
    adapter.set_terminal_relay(relay(&dir));

    let open = adapter.launch_for_open("req-1", Some("/work"), None).expect("open launch");
    assert_eq!(open.session_id, "req-1");
    let path = mcp_config_path(&open.argv);
    assert_eq!(*path, dir.join("req-1.json").to_string_lossy(), "one file per session: {path}");

    let resume = adapter.launch_for_resume("sess-9").expect("resume launch");
    let path = mcp_config_path(&resume.argv);
    assert!(resume.argv.contains(&"--resume".to_owned()));
    assert_eq!(*path, dir.join("sess-9.json").to_string_lossy());

    let fork = adapter.launch_for_fork("branch-2", "sess-9").expect("fork launch");
    let path = mcp_config_path(&fork.argv);
    assert!(fork.argv.contains(&"--fork-session".to_owned()));
    assert_eq!(
        *path,
        dir.join("branch-2.json").to_string_lossy(),
        "the fork's bridge answers for the NEW session: {path}"
    );

    // Every written file names the bridge, the socket and its own session.
    for (file, session) in [("req-1", "req-1"), ("sess-9", "sess-9"), ("branch-2", "branch-2")] {
        let text = std::fs::read_to_string(dir.join(format!("{file}.json"))).expect("config reads");
        let value: serde_json::Value = serde_json::from_str(&text).expect("config parses");
        let server = &value["mcpServers"]["baaz"];
        assert_eq!(server["type"], "stdio", "{file}: stdio server");
        assert_eq!(server["command"], "/tmp/baaz/bin/mcp-bridge", "{file}: the bridge");
        let args: Vec<String> = serde_json::from_value(server["args"].clone()).expect("args");
        assert_eq!(
            args,
            vec![
                "--terminal",
                "--socket",
                "/tmp/baaz/run/terminal-9.sock",
                "--session",
                session
            ],
            "{file}: socket and session"
        );
    }
}

#[test]
fn without_a_relay_the_argv_is_what_it_always_was() {
    let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
    for argv in [
        adapter.launch_for_open("req-1", None, None).expect("open").argv,
        adapter.launch_for_resume("sess-9").expect("resume").argv,
        adapter.launch_for_fork("branch-2", "sess-9").expect("fork").argv,
    ] {
        assert!(
            !argv.iter().any(|arg| arg == "--mcp-config"),
            "no relay means no config flag: {argv:?}"
        );
        assert!(
            !argv.iter().any(|arg| arg == "--strict-mcp-config"),
            "no relay means no strict flag either: {argv:?}"
        );
    }
}
