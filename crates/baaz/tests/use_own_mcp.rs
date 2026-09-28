//! Z5: provider sessions never inherit the owner's own MCP servers
//! unless asked — the host-side argv contract for both scoped lanes.
//!
//! These drive the real adapters offline (no spawn, no model): the
//! Claude Code launch builders write their per-session config into a
//! temp dir, and the Codex builder only assembles `-c` overrides, so
//! nothing here spends the owner's money.

use std::path::PathBuf;

fn claude_relay(dir: &std::path::Path) -> provider_claude_code::TerminalRelay {
    provider_claude_code::TerminalRelay {
        bridge: PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
        socket: PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
        config_dir: dir.to_path_buf(),
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("baaz-z5-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn has(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

/// Claude Code without a bridge: strict rides anyway (Z5), with no
/// `--mcp-config` beside it — strict alone means no servers at all.
#[test]
fn claude_without_a_bridge_is_still_strict() {
    let adapter = provider_claude_code::ClaudeCodeAdapter::new("claude-must-never-spawn");
    for argv in [
        adapter.launch_for_open("req-1", None, None).expect("open").argv,
        adapter.launch_for_resume("sess-9").expect("resume").argv,
        adapter.launch_for_fork("branch-2", "sess-9").expect("fork").argv,
    ] {
        assert!(has(&argv, "--strict-mcp-config"), "strict without a bridge: {argv:?}");
        assert!(!has(&argv, "--mcp-config"), "no config flag without a file: {argv:?}");
    }
}

/// Claude Code with a bridge: the per-session config plus strict.
#[test]
fn claude_with_a_bridge_carries_config_and_strict() {
    let dir = temp_dir("bridge");
    let adapter = provider_claude_code::ClaudeCodeAdapter::new("claude-must-never-spawn");
    adapter.set_terminal_relay(claude_relay(&dir));
    let open = adapter.launch_for_open("req-1", Some("/work"), None).expect("open");
    assert!(has(&open.argv, "--mcp-config"), "bridge config: {:?}", open.argv);
    assert!(has(&open.argv, "--strict-mcp-config"), "strict: {:?}", open.argv);
}

/// Claude Code with the switch on: no strict anywhere, bridge kept.
#[test]
fn claude_with_the_switch_on_drops_strict_and_keeps_the_bridge() {
    let dir = temp_dir("own");
    let adapter = provider_claude_code::ClaudeCodeAdapter::new("claude-must-never-spawn");
    adapter.set_terminal_relay(claude_relay(&dir));
    adapter.set_use_own_mcp(true);
    let open = adapter.launch_for_open("req-1", None, None).expect("open");
    assert!(has(&open.argv, "--mcp-config"), "bridge still rides: {:?}", open.argv);
    assert!(!has(&open.argv, "--strict-mcp-config"), "no strict opted in: {:?}", open.argv);
    let resume = adapter.launch_for_resume("sess-9").expect("resume");
    assert!(!has(&resume.argv, "--strict-mcp-config"), "resume too: {:?}", resume.argv);
    // And without a relay the opt-in is exactly today's argv: nothing.
    let bare = provider_claude_code::ClaudeCodeAdapter::new("claude-must-never-spawn");
    bare.set_use_own_mcp(true);
    for argv in [
        bare.launch_for_open("req-1", None, None).expect("open").argv,
        bare.launch_for_resume("sess-9").expect("resume").argv,
    ] {
        assert!(!has(&argv, "--strict-mcp-config"), "opted in, no relay: {argv:?}");
        assert!(!has(&argv, "--mcp-config"), "opted in, no relay: {argv:?}");
    }
}

/// Codex without a relay still spawns silenced (Z5 probe: bare
/// `app-server` listed 5 servers and 104 owner tools).
#[test]
fn codex_without_a_relay_still_spawns_silenced() {
    let adapter = provider_codex::CodexAdapter::new("codex-must-never-spawn");
    let extra = adapter.spawn_args_for("cmd-open");
    assert!(
        !extra.iter().any(|arg| arg.contains("mcp_servers.baaz.")),
        "no relay means no bridge: {extra:?}"
    );
    assert!(
        extra.iter().any(|arg| arg.contains("mcp_servers.codex_apps={")),
        "the probe's 97-tool server stays covered: {extra:?}"
    );
}

/// Codex with the switch on: bridge only, no disables.
#[test]
fn codex_with_the_switch_on_builds_bridge_only() {
    let adapter = provider_codex::CodexAdapter::new("codex-must-never-spawn");
    adapter.set_terminal_relay(provider_codex::TerminalRelay {
        bridge: PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
        socket: PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
    });
    adapter.set_use_own_mcp(true);
    let extra = adapter.spawn_args_for("cmd-open");
    assert_eq!(extra.len(), 4, "bridge only: {extra:?}");
    assert!(
        !extra.iter().any(|arg| arg.contains("enabled=false")),
        "opted in means nothing silenced: {extra:?}"
    );
}
