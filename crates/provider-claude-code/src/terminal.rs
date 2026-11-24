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
//! With Settings › MCP's "Use my own MCP servers" on, the strict flag goes
//! and a second file rides beside the bridge's: the person's own servers
//! from their `~/.claude.json` ([`own_mcp_servers`], `<session>-own.json`),
//! which Baaz's own Claude home would otherwise never see.
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
/// "command": <bridge>, "args": [...], "env": {BAAZ_BRIDGE_TOKEN: …}}}}`
/// (the token: docs/85 10.0) — the shape the CLI's
/// `--mcp-config` accepts (see docs/18-claude-code.md §4).
pub fn mcp_config_value(bridge: &Path, socket: &Path, session_id: &str) -> serde_json::Value {
    serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": bridge.to_string_lossy(),
                "args": bridge_args(socket, session_id),
                "env": { provider::bridge_token::ENV: provider::bridge_token::token_for(session_id) },
            }
        }
    })
}

/// The person's own MCP servers a session in `workspace` loads when Settings ›
/// MCP's "Use my own MCP servers" is on, from the text of their
/// `~/.claude.json`: user scope (the top-level `mcpServers`, where `claude
/// mcp add -s user` writes) plus local scope for that folder
/// (`projects[<workspace>].mcpServers`, where a plain `claude mcp add` run
/// in it writes), the folder's winning a name both define as the CLI's own
/// precedence does — never one named `baaz`, which is Baaz's own. Baaz runs
/// Claude Code with its own `CLAUDE_CONFIG_DIR` ([`crate::home`]), whose
/// `.claude.json` is not that file, so these reach a session only through
/// [`write_own_mcp_config`]. Text that does not parse holds none.
pub fn own_mcp_servers(claude_json: &str, workspace: Option<&str>) -> serde_json::Map<String, serde_json::Value> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(claude_json) else {
        return serde_json::Map::new();
    };
    let servers = |value: Option<&serde_json::Value>| value.and_then(|v| v.get("mcpServers")).and_then(|v| v.as_object()).cloned().unwrap_or_default();
    let mut out = servers(Some(&value));
    if let Some(folder) = workspace.map(|w| w.trim_end_matches('/')).filter(|w| !w.is_empty()) {
        out.extend(servers(value.get("projects").and_then(|p| p.get(folder))));
    }
    out.remove(SERVER_NAME);
    out
}

/// Write the person's own servers (`servers`, from [`own_mcp_servers`]) for
/// `session_id` into `dir` as `<session>-own.json`, `{"mcpServers": …}` —
/// a second `--mcp-config` beside the bridge's (the CLI takes several) —
/// and return its path. Atomic like [`write_mcp_config`]. Written fresh at
/// every launch, so a server the person added since loads on the next one.
pub fn write_own_mcp_config(dir: &Path, session_id: &str, servers: &serde_json::Map<String, serde_json::Value>) -> io::Result<PathBuf> {
    if session_id.is_empty() || session_id.contains('/') || session_id.contains('\\') || session_id.contains('\0') {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "refusing a session id that is a path"));
    }
    std::fs::create_dir_all(dir)?;
    let path = own_mcp_config_path(dir, session_id);
    let text = serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers })).map_err(io::Error::other)?;
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    // Owner-only, like the `~/.claude.json` it copies: a server's `env` and
    // `headers` often hold tokens.
    let written = {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .and_then(|mut file| file.write_all(text.as_bytes()))
    };
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    match std::fs::rename(&temporary, &path) {
        Ok(()) => Ok(path),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Where [`write_own_mcp_config`] puts `session_id`'s file in `dir`.
pub fn own_mcp_config_path(dir: &Path, session_id: &str) -> PathBuf {
    dir.join(format!("{session_id}-own.json"))
}

/// What Claude Code's sessions connect besides Baaz's own server, for
/// Settings › MCP (docs/82 8.8, 8.9), read from the files they load: the
/// person's own user-scope servers from `~/.claude.json` (what "Use my own
/// MCP servers" passes to every launch, [`own_mcp_servers`]), the servers
/// in Baaz's own Claude home's `.claude.json` (the file `CLAUDE_CONFIG_DIR`
/// points the CLI at, [`crate::home`]), and the plugins `settings.json`
/// turns on (linked from the person's `~/.claude`). `scripted` reads the
/// recorded `fixtures/claude-code/mcp-*.json` instead (a capture never
/// reads the person's files). An unreadable file lists nothing.
pub fn mcp_listing(scripted: bool) -> provider::McpListing {
    const CONFIG: &str = include_str!("../../../fixtures/claude-code/mcp-claude.json");
    const SETTINGS: &str = include_str!("../../../fixtures/claude-code/mcp-settings.json");
    let home = crate::home::default_home();
    let owner = std::env::var_os("HOME").map(PathBuf::from).map(|home| home.join(".claude.json"));
    let (config_path, settings_path) = (home.join(".claude.json"), home.join("settings.json"));
    let (owner_text, config, settings) = if scripted {
        (CONFIG.to_owned(), String::new(), SETTINGS.to_owned())
    } else {
        (
            owner.as_ref().and_then(|path| std::fs::read_to_string(path).ok()).unwrap_or_default(),
            std::fs::read_to_string(&config_path).unwrap_or_default(),
            std::fs::read_to_string(&settings_path).unwrap_or_default(),
        )
    };
    let sources = if scripted {
        vec![PathBuf::from("~/.claude.json"), PathBuf::from("~/.claude/settings.json")]
    } else {
        owner.into_iter().chain([config_path, settings_path]).collect()
    };
    provider::McpListing {
        sources,
        entries: listed_mcp(&[&owner_text, &config], &settings),
        unlisted: "Servers added for one folder load in that folder's sessions; a project's .mcp.json and connectors on your claude.ai account load too. None of these is listed.",
    }
}

/// [`mcp_listing`]'s entries from the files' text: the user-scope servers
/// of each `.claude.json` (never `baaz`, never a project's own; a name in
/// both once), then the plugins `enabledPlugins` sets true, each by name
/// (a JSON object has no order to keep). Text that does not parse lists
/// nothing.
pub fn listed_mcp(claude_jsons: &[&str], settings_json: &str) -> Vec<provider::ConfiguredMcp> {
    let keys = |text: &str, field: &str, keep: &dyn Fn(&serde_json::Value) -> bool| -> Vec<String> {
        serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| value.get(field).and_then(|v| v.as_object()).cloned())
            .map(|map| map.into_iter().filter(|(_, v)| keep(v)).map(|(k, _)| k).collect())
            .unwrap_or_default()
    };
    let mut servers: Vec<String> = claude_jsons.iter().flat_map(|text| keys(text, "mcpServers", &|_| true)).filter(|name| name != SERVER_NAME).collect();
    servers.sort();
    servers.dedup();
    let mut plugins = keys(settings_json, "enabledPlugins", &|v| v.as_bool() == Some(true));
    plugins.sort();
    servers
        .into_iter()
        .map(|name| provider::ConfiguredMcp { name, plugin: false })
        .chain(plugins.into_iter().map(|name| provider::ConfiguredMcp { name, plugin: true }))
        .collect()
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
        assert_eq!(listing.sources[0], PathBuf::from("~/.claude.json"), "the person's own file: what the launch passes");
        assert!(listed_mcp(&["", "x"], "not json").is_empty(), "unreadable files list nothing");
        // Both files' servers, a name in both once, never `baaz`.
        let both = listed_mcp(&[r#"{"mcpServers":{"a":{},"baaz":{}}}"#, r#"{"mcpServers":{"a":{},"b":{}}}"#], "{}");
        assert_eq!(both.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    }

    /// docs/82 8.9: the person's own servers for a launch — user scope plus
    /// the launch folder's local scope (which wins a shared name), never
    /// `baaz`, never another folder's; unreadable text holds none.
    #[test]
    fn own_servers_are_user_scope_plus_the_folders_local_scope() {
        const OWNER: &str = include_str!("../../../fixtures/claude-code/mcp-claude.json");
        let names = |map: &serde_json::Map<String, serde_json::Value>| {
            let mut names = map.keys().cloned().collect::<Vec<_>>();
            names.sort();
            names
        };
        assert_eq!(names(&own_mcp_servers(OWNER, None)), ["filesystem", "linear"]);
        assert_eq!(names(&own_mcp_servers(OWNER, Some("/work"))), ["filesystem", "linear", "local-only"]);
        assert_eq!(names(&own_mcp_servers(OWNER, Some("/work/"))), ["filesystem", "linear", "local-only"], "a trailing slash is the same folder");
        assert_eq!(names(&own_mcp_servers(OWNER, Some("/elsewhere"))), ["filesystem", "linear"]);
        let local_wins = own_mcp_servers(r#"{"mcpServers":{"x":{"url":"user"}},"projects":{"/w":{"mcpServers":{"x":{"url":"local"}}}}}"#, Some("/w"));
        assert_eq!(local_wins["x"]["url"], "local");
        assert!(own_mcp_servers("not json", Some("/work")).is_empty());
        // The file a launch carries: `{"mcpServers": …}`, beside the bridge's.
        let dir = std::env::temp_dir().join(format!("baaz-own-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_own_mcp_config(&dir, "s-1", &own_mcp_servers(OWNER, None)).expect("written");
        assert_eq!(path, own_mcp_config_path(&dir, "s-1"));
        let back: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        assert_eq!(back["mcpServers"]["linear"]["type"], "http");
        assert!(back["mcpServers"].get("baaz").is_none());
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "the copy is owner-only, like ~/.claude.json");
        }
        assert!(write_own_mcp_config(&dir, "../x", &serde_json::Map::new()).is_err(), "a pathy id refuses");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(
            value["mcpServers"]["baaz"]["env"]["BAAZ_BRIDGE_TOKEN"],
            serde_json::json!(provider::bridge_token::token_for("s-9")),
            "the token rides the env (docs/85 10.0)"
        );
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
