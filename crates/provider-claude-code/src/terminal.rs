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

/// What the Claude home Baaz's sessions run under connects besides Baaz's
/// own server, for Settings › MCP (docs/82 8.8), read from its files —
/// never from `claude` itself: the user-scope `mcpServers` of
/// `<home>/.claude.json` (the file `CLAUDE_CONFIG_DIR` points the CLI at,
/// [`crate::home`]) and the plugins `settings.json` turns on (linked from
/// the person's `~/.claude`). `scripted` reads the recorded
/// `fixtures/claude-code/mcp-*.json` instead (a capture never reads the
/// person's files). An unreadable file lists nothing.
pub fn mcp_listing(scripted: bool) -> provider::McpListing {
    const CONFIG: &str = include_str!("../../../fixtures/claude-code/mcp-claude.json");
    const SETTINGS: &str = include_str!("../../../fixtures/claude-code/mcp-settings.json");
    let home = crate::home::default_home();
    let (config_path, settings_path) = (home.join(".claude.json"), home.join("settings.json"));
    let (config, settings) = if scripted {
        (CONFIG.to_owned(), SETTINGS.to_owned())
    } else {
        (
            std::fs::read_to_string(&config_path).unwrap_or_default(),
            std::fs::read_to_string(&settings_path).unwrap_or_default(),
        )
    };
    let sources = if scripted {
        vec![PathBuf::from("~/Library/Application Support/baaz/claude-home/.claude.json"), PathBuf::from("~/.claude/settings.json")]
    } else {
        vec![config_path, settings_path]
    };
    provider::McpListing {
        sources,
        entries: listed_mcp(&config, &settings),
        unlisted: "Connectors on your claude.ai account and a project's .mcp.json load too and are not listed.",
    }
}

/// [`mcp_listing`]'s entries from the two files' text: the user-scope
/// servers (never `baaz`, never a project's own), then the plugins
/// `enabledPlugins` sets true, each by name (a JSON object has no order to
/// keep). Text that does not parse lists nothing.
pub fn listed_mcp(claude_json: &str, settings_json: &str) -> Vec<provider::ConfiguredMcp> {
    let keys = |text: &str, field: &str, keep: &dyn Fn(&serde_json::Value) -> bool| -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get(field).and_then(|v| v.as_object()).cloned())
            .map(|map| {
                let mut names: Vec<String> = map.into_iter().filter(|(_, v)| keep(v)).map(|(k, _)| k).collect();
                names.sort();
                names
            })
            .unwrap_or_default()
    };
    let servers = keys(claude_json, "mcpServers", &|_| true)
        .into_iter()
        .filter(|name| name != SERVER_NAME)
        .map(|name| provider::ConfiguredMcp { name, plugin: false });
    let plugins = keys(settings_json, "enabledPlugins", &|v| v.as_bool() == Some(true))
        .into_iter()
        .map(|name| provider::ConfiguredMcp { name, plugin: true });
    servers.chain(plugins).collect()
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

    /// Settings › MCP's Claude Code list: the recorded home's two
    /// user-scope servers (Baaz's own and a project's left out), then the
    /// one plugin turned on; the scripted read never touches the person's
    /// files.
    #[test]
    fn the_mcp_listing_reads_user_servers_and_plugins_turned_on() {
        let listing = mcp_listing(true);
        let names: Vec<(&str, bool)> = listing.entries.iter().map(|e| (e.name.as_str(), e.plugin)).collect();
        assert_eq!(names, [("filesystem", false), ("linear", false), ("figma@claude-plugins", true)]);
        assert_eq!(listing.sources.len(), 2);
        assert!(listed_mcp("", "not json").is_empty(), "unreadable files list nothing");
    }

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
