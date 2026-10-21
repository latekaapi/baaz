//! The terminal relay's per-session wiring (T2): how each provider's agent
//! reaches the socket service from [`super::service`].
//!
//! The app serves the `docs/14-terminal.md` §4 contract on
//! `<support_dir>/run/terminal-<pid>.sock`; every route below names the
//! same `mcp-bridge` relay beside it, so the tools, the tab rules and the
//! refusal shapes do not change with the route:
//!
//! * **Muse (route 1, session MCP):** `session/start` and `session/resume`
//!   carry `config.mcpServers.baaz`, built by `provider-muse`'s
//!   `TerminalRelay` (which owns every muse-schema spelling) from the bridge
//!   path and this socket. Only when `initialize` granted `sessionMcp` —
//!   otherwise no route.
//! * **Claude Code:** a per-session MCP config JSON under
//!   `<support_dir>/mcp/<session>.json` naming the same bridge, passed as
//!   `--mcp-config <path> --strict-mcp-config` on open/resume/fork.
//! * **Codex:** the bridge as a per-session MCP server through `codex
//!   app-server` config overrides (`-c mcp_servers.baaz.…`), which are
//!   process-scoped and never touch the owner's `~/.codex/config.toml`.
//!
//! The bridge binary is the `mcp-bridge` executable beside the running baaz
//! binary (in the app bundle that is `Contents/MacOS/mcp-bridge` —
//! [`bridge_path`]). When the socket is gone the relay answers every tool
//! with "Baaz isn't running…", so a stale path degrades to a sentence, not
//! a failure.

use std::path::{Path, PathBuf};

/// Where a session's Claude Code MCP config file lives:
/// `<support_dir>/mcp/<session>.json`, written by
/// [`write_claude_mcp_config`]. Beside the socket's `run/` directory, never
/// inside it — the relay never scans that directory for anything but the
/// one socket it serves.
pub fn claude_config_dir(support_dir: &Path) -> PathBuf {
    support_dir.join("mcp")
}

/// The absolute path of the `mcp-bridge` binary beside the running baaz
/// binary (or inside the app bundle next to it — same rule, since the
/// bundle's executable lives in `Contents/MacOS/`).
///
/// `BAAZ_MCP_BRIDGE` overrides it outright, for tests and one-off probes.
/// Otherwise it is the sibling of [`std::env::current_exe`]; with no
/// usable exe path it falls back to the bare name and lets the spawn fail
/// loud at the route, rather than inventing a path.
pub fn bridge_path() -> PathBuf {
    if let Some(path) = std::env::var_os("BAAZ_MCP_BRIDGE").map(PathBuf::from) {
        return path;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("mcp-bridge");
        }
    }
    PathBuf::from("mcp-bridge")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bridge_sits_beside_the_running_binary() {
        // `BAAZ_MCP_BRIDGE` overrides outright (this is also how the
        // integration tests point the route at a stub).
        let dir = std::env::temp_dir().join("baaz-relay-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_MCP_BRIDGE", dir.join("stub-bridge"));
        assert_eq!(bridge_path(), dir.join("stub-bridge"));
        std::env::remove_var("BAAZ_MCP_BRIDGE");
        // Otherwise the sibling of the test binary: absolute, named
        // `mcp-bridge`.
        let path = bridge_path();
        assert!(path.is_absolute(), "the bridge path is absolute: {}", path.display());
        assert_eq!(path.file_name().and_then(|name| name.to_str()), Some("mcp-bridge"));
    }

    #[test]
    fn the_claude_config_dir_sits_beside_the_socket_dir() {
        let support = PathBuf::from("/tmp/baaz-state");
        assert_eq!(claude_config_dir(&support), support.join("mcp"));
    }

}
