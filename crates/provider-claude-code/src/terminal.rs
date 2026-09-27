//! The terminal relay's Claude Code leg: one MCP config file per session.
//!
//! The adapter already speaks `--mcp-config <path> --strict-mcp-config`
//! ([`crate::argv`]); this module owns what that file contains for the
//! terminal route: the `mcp-bridge` relay as a stdio server named `baaz`,
//! pointed at the socket service for exactly one session. `--strict-mcp-config`
//! stays on every launch that carries the file, so a Baaz session sees
//! Baaz's tools and nothing else (docs/18-claude-code.md §4) — in
//! particular never the operator's own connectors.
//!
//! Files live under `<support_dir>/mcp/<session>.json`, written atomically:
//! a crash mid-write leaves the previous config rather than half of the
//! next one. Session ids are UUIDs; anything shaped like a path is refused
//! rather than served.

use std::io;
use std::path::{Path, PathBuf};

/// The MCP server name the bridge registers under on every route.
pub const SERVER_NAME: &str = "baaz";

/// The bridge's argv tail for one session: `--terminal --socket <sock>
/// --session <id>`, exactly what `mcp-bridge --terminal` parses.
pub fn bridge_args(socket: &Path, session_id: &str) -> Vec<String> {
    vec![
        "--terminal".to_owned(),
        "--socket".to_owned(),
        socket.to_string_lossy().into_owned(),
        "--session".to_owned(),
        session_id.to_owned(),
    ]
}

/// The file's whole content: `{"mcpServers": {"baaz": {"type": "stdio",
/// "command": <bridge>, "args": [...]}}}` — the shape the CLI's
/// `--mcp-config` accepts (see docs/18-claude-code.md §4).
pub fn mcp_config_value(bridge: &Path, socket: &Path, session_id: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": bridge.to_string_lossy(),
                "args": bridge_args(socket, session_id),
            }
        }
    })
}

/// Write the config file for `session_id` into `dir` (`<support_dir>/mcp`)
/// and return its path, for `--mcp-config`. Atomic: written beside itself
/// and renamed, so a crash leaves the previous config.
pub fn write_mcp_config(
    dir: &Path,
    session_id: &str,
    bridge: &Path,
    socket: &Path,
) -> io::Result<PathBuf> {
    if session_id.is_empty()
        || session_id.contains('/')
        || session_id.contains('\\')
        || session_id.contains('\0')
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing a session id that is a path"));
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{session_id}.json"));
    let text = serde_json::to_string_pretty(&mcp_config_value(bridge, socket, session_id))
        .map_err(io::Error::other)?;
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, text.as_bytes())?;
    match std::fs::rename(&temporary, &path) {
        Ok(()) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bridge() -> PathBuf {
        PathBuf::from("/Applications/Baaz.app/Contents/MacOS/mcp-bridge")
    }

    fn socket() -> PathBuf {
        PathBuf::from("/tmp/baaz/run/terminal-1.sock")
    }

    #[test]
    fn the_file_is_a_stdio_server_for_the_session() {
        let value = mcp_config_value(&bridge(), &socket(), "s-9");
        let server = &value["mcpServers"]["baaz"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], bridge().to_string_lossy().as_ref());
        let args: Vec<String> = serde_json::from_value(server["args"].clone()).expect("args");
        assert_eq!(
            args,
            vec!["--terminal", "--socket", "/tmp/baaz/run/terminal-1.sock", "--session", "s-9"]
        );
        assert_eq!(value.as_object().expect("object").len(), 1, "nothing but mcpServers");
    }

    #[test]
    fn the_file_round_trips_and_pathy_ids_refuse() {
        let dir = std::env::temp_dir().join(format!("cc-terminal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_mcp_config(&dir, "s-9", &bridge(), &socket()).expect("writes");
        assert_eq!(path, dir.join("s-9.json"));
        let back: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("reads")).expect("parses");
        assert_eq!(back, mcp_config_value(&bridge(), &socket(), "s-9"));
        assert!(write_mcp_config(&dir, "../escape", &bridge(), &socket()).is_err());
        assert!(write_mcp_config(&dir, "", &bridge(), &socket()).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
