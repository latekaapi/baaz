//! The terminal relay's Codex leg: the spawn argv carries the bridge.
//!
//! All offline: `spawn_args_for` builds the `app-server` argv fragment
//! without spawning — nothing here spends the owner's money. The live
//! question the gate cannot answer (which servers a Baaz-started child
//! actually has, and whether inherited ones stay silencable) is probed by
//! hand and recorded in docs/19-codex.md, not here.

use std::path::PathBuf;

use provider_codex::{CodexAdapter, TerminalRelay};

fn relay() -> TerminalRelay {
    TerminalRelay {
        bridge: PathBuf::from("/tmp/baaz/bin/mcp-bridge"),
        socket: PathBuf::from("/tmp/baaz/run/terminal-9.sock"),
    }
}

#[test]
fn the_spawn_argv_carries_the_bridge_for_the_session() {
    let adapter = CodexAdapter::new("codex-must-never-spawn");
    adapter.set_terminal_relay(relay());
    // A fresh open answers for the request id: Codex mints the thread id
    // itself, so the request id is the identity the app registers before
    // the send.
    let extra = adapter.spawn_args_for("cmd-open");
    assert!(extra.len() >= 4, "bridge plus disables: {extra:?}");
    assert_eq!(extra[0], "-c");
    assert!(
        extra[1].starts_with("mcp_servers.baaz.command="),
        "the command rides a dotted path: {}",
        extra[1]
    );
    assert!(extra[1].contains("/tmp/baaz/bin/mcp-bridge"));
    assert_eq!(extra[2], "-c");
    assert!(extra[3].contains("\"--session\",\"cmd-open\""), "the open id: {}", extra[3]);
    // Inherited servers ride along as full-table disables (the bundled
    // set at least — the file-configured rest depends on the machine),
    // plus one plugin disable per owner-enabled plugin.
    let disables = extra.iter().filter(|arg| arg.contains("enabled=false")).count();
    assert!(disables > 0, "inherited servers are silenced: {extra:?}");
    assert!(
        extra.iter().any(|arg| arg.contains("mcp_servers.codex_apps={")),
        "the probe's 97-tool server stays covered: {extra:?}"
    );
    assert!(
        !extra.iter().any(|arg| arg.contains("mcp_servers.baaz={")),
        "the bridge itself is never disabled: {extra:?}"
    );
    // A resume names its stored session directly.
    let extra = adapter.spawn_args_for("s-stored");
    assert!(extra[3].contains("\"--session\",\"s-stored\""), "the resume id: {}", extra[3]);
    for arg in &extra {
        assert!(!arg.contains(".codex/config.toml"), "never the owner's file: {arg}");
    }
}

#[test]
fn without_a_relay_the_child_still_spawns_silenced() {
    // Z5: the disables ride EVERY session, relay or not — the live probe
    // (docs/19-codex.md) listed a bare `app-server` at 5 servers and 104
    // owner tools. No bridge without a relay, but the silences stay.
    let adapter = CodexAdapter::new("codex-must-never-spawn");
    let extra = adapter.spawn_args_for("cmd-open");
    assert!(
        !extra.iter().any(|arg| arg.contains("mcp_servers.baaz.")),
        "no relay means no bridge: {extra:?}"
    );
    let disables = extra.iter().filter(|arg| arg.contains("enabled=false")).count();
    assert!(disables > 0, "inherited servers are silenced without a relay: {extra:?}");
}

#[test]
fn with_the_switch_on_no_disables_but_the_bridge() {
    // The Settings → Providers opt-in restores the owner's servers: no
    // silence rows at all, while the bridge still rides when set.
    let adapter = CodexAdapter::new("codex-must-never-spawn");
    adapter.set_terminal_relay(relay());
    adapter.set_use_own_mcp(true);
    let extra = adapter.spawn_args_for("cmd-open");
    assert_eq!(extra.len(), 4, "bridge only: {extra:?}");
    assert!(extra[1].starts_with("mcp_servers.baaz.command="));
    assert!(extra[3].contains("\"--session\",\"cmd-open\""));
    assert!(
        !extra.iter().any(|arg| arg.contains("enabled=false")),
        "opted in means nothing silenced: {extra:?}"
    );
    // And without a relay the opt-in is exactly bare `app-server`.
    let bare = CodexAdapter::new("codex-must-never-spawn");
    bare.set_use_own_mcp(true);
    assert!(bare.spawn_args_for("cmd-open").is_empty(), "opted in, no relay: bare");
}
