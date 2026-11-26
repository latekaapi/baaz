//! The terminal relay's Codex leg: the bridge as a per-session MCP server.
//!
//! `codex app-server` takes process-scoped config overrides (`-c
//! dotted.path=value`, where the value parses as TOML) that beat
//! `~/.codex/config.toml` for that child only — the owner's file is never
//! read differently, let alone written. The bridge rides
//! `mcp_servers.baaz.command` / `mcp_servers.baaz.args`, so the session the
//! child serves sees Baaz's terminal tools beside whatever the owner
//! configured. Whether those inherited servers can be silenced per session
//! (without editing the owner's config) is a live question — see
//! [`disable_override`] and the T2 probe notes in docs/19-codex.md.
//!
//! The bridge's `--session` is a Baaz-minted id the app registers with the
//! service before the open runs: the `OpenSession` request id for a fresh
//! thread (Codex mints the thread id itself, so it cannot be known at
//! spawn), the stored session id for a resume.

/// The MCP server name the bridge registers under on every route.
pub const SERVER_NAME: &str = "baaz";

/// The bridge's argv tail for one session: `--terminal --socket <sock>
/// --session <id>`, exactly what `mcp-bridge --terminal` parses.
pub fn bridge_args(socket: &std::path::Path, session_id: &str) -> Vec<String> {
    vec![
        "--terminal".to_owned(),
        "--socket".to_owned(),
        socket.to_string_lossy().into_owned(),
        "--session".to_owned(),
        session_id.to_owned(),
    ]
}

/// Quote one `-c` value as TOML: the `value` portion of `key=value` parses
/// as TOML, falling back to a raw string — so a path with spaces must
/// arrive quoted, with its quotes and backslashes escaped.
pub fn toml_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for char in value.chars() {
        match char {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    quoted
}

/// The `codex app-server` argv fragment carrying the bridge for
/// `session_id`: `-c mcp_servers.baaz.command=… -c mcp_servers.baaz.args=…
/// -c mcp_servers.baaz.env={BAAZ_BRIDGE_TOKEN=…}` (the token: docs/85 10.0).
/// Spliced after `app-server`, before stdio takes over.
pub fn server_overrides(
    bridge: &std::path::Path,
    socket: &std::path::Path,
    session_id: &str,
) -> Vec<String> {
    let command = bridge.to_string_lossy();
    let args: Vec<String> =
        bridge_args(socket, session_id).into_iter().map(|arg| toml_string(&arg)).collect();
    vec![
        "-c".to_owned(),
        format!("mcp_servers.{SERVER_NAME}.command={}", toml_string(&command)),
        "-c".to_owned(),
        format!("mcp_servers.{SERVER_NAME}.args=[{}]", args.join(",")),
        "-c".to_owned(),
        format!(
            "mcp_servers.{SERVER_NAME}.env={{{}={}}}",
            provider::bridge_token::ENV,
            toml_string(&provider::bridge_token::token_for(session_id))
        ),
    ]
}

/// Silence inherited MCP servers for this child only: one full-table
/// `-c 'mcp_servers.<name>={enabled=false,command="/bin/true"}'` per name.
/// Full-table, because a single-key `-c mcp_servers.<name>.enabled=false`
/// does NOT merge — it replaces the table with that one key, the transport
/// goes invalid, and the child refuses to start at all (probed live
/// 2026-09-27: `error loading default config after config error: invalid
/// transport in mcp_servers.cua_repl`). The complete table validates, the
/// server never spawns, and the owner's config file is never touched.
/// A name that is not one dotted-path segment is refused rather than
/// smuggled into the override key, and the bridge itself is never
/// silencable here.
pub fn disable_override(server: &str) -> Option<Vec<String>> {
    if server.is_empty()
        || server == SERVER_NAME
        || server.contains(|char: char| !(char.is_ascii_alphanumeric() || char == '_' || char == '-'))
    {
        return None;
    }
    Some(vec![
        "-c".to_owned(),
        format!("mcp_servers.{server}={{enabled=false,command=\"/bin/true\"}}"),
    ])
}

/// The disable overrides for every name in `servers`, in order, skipping
/// what [`disable_override`] refuses. Empty in, empty out.
pub fn disable_overrides<'a>(servers: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    servers.into_iter().filter_map(disable_override).flatten().collect()
}

/// Codex-bundled servers that are not `[mcp_servers]` config-file tables
/// but still start in every session: seen live 2026-09-27 on codex-cli
/// 0.144.6 (`mcpServerStatus/list` beside the bridge: `codex_app`,
/// `codex_apps` with 97 tools, `cua_repl`, plus file-configured
/// `computer-use` and `node_repl`). The Z5 probe (2026-09-28,
/// docs/19-codex.md) re-listed bare `app-server` at 5 servers and 104
/// tools — `node_repl` (`js`, `js_add_node_module_dir`, `js_reset`,
/// `turn_ended`) is file-configured on this box but rides the floor
/// anyway, so a box where it is bundled-but-unconfigured stays covered.
/// Overriding an absent name creates a disabled no-op stub, so covering
/// a name this machine does not have costs one inert row — while missing
/// a name this machine DOES have leaks its tools into the session.
pub const BUNDLED_SERVERS: &[&str] =
    &["codex_app", "codex_apps", "computer-use", "cua_repl", "node_repl"];

/// Every plugin off for this child: `-c features.plugins=false`. One key
/// for the owner's config-named plugins AND the ones installed remotely
/// (under `plugins/cache`, absent from `config.toml`), which a per-plugin
/// `plugins."<name>".enabled=false` could never name. The per-plugin key
/// was also not enough on its own: Codex 0.153.2 still listed the plugin's
/// skills with it set. The offline probe (docs/88 C.2,
/// `scripts/bench/prompt_probe.py`) shows this key leaves 0 plugin skills,
/// no `<plugins_instructions>` and no `<recommended_plugins>`; the five
/// bundled system skills stay.
pub fn plugins_off() -> [String; 2] {
    ["-c".to_owned(), "features.plugins=false".to_owned()]
}

/// Plugin install suggestions off for this child: `-c
/// features.tool_suggest=false` drops the `<recommended_plugins>` block
/// (about 876 characters a request, probed in docs/88 C.2). Baaz has no
/// handler for an install request, so the suggestions are dead weight
/// whether or not the person uses their own MCP servers.
pub fn tool_suggest_off() -> [String; 2] {
    ["-c".to_owned(), "features.tool_suggest=false".to_owned()]
}

/// Owner-enabled `[plugins."<name>@<marketplace>"]` tables named by
/// `config.toml` text, read-only — the same heading scan as
/// [`file_server_names`]. Sub-tables (`[plugins."x".env]`) are not
/// plugins; a `"` inside the name never reaches an override key.
pub fn plugin_names(config_toml: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in config_toml.lines() {
        let line = line.trim();
        let Some(body) = line.strip_prefix("[plugins.") else { continue };
        let Some(name) = body.strip_suffix(']') else { continue };
        let name = name.trim().trim_matches('"').trim_matches('\'').to_owned();
        // A sub-table (`[plugins."browser@openai-bundled".env]`) keeps an
        // inner quote after the outer trim; a plugin header never does.
        // Exact duplicates ride once.
        if name.is_empty() || name.contains('"') || names.contains(&name) {
            continue;
        }
        names.push(name);
    }
    names
}

/// File-configured `[mcp_servers.<name>]` tables named by `config.toml`
/// text, read-only: line headers, nothing more — this crate gains no TOML
/// dependency for one heading scan. Quoted and dotted headers that are not
/// one path segment never reach an override ([`disable_override`] refuses
/// them); the bridge's own table is never named for silencing.
pub fn file_server_names(config_toml: &str) -> Vec<String> {
    let mut names = Vec::new();
    for line in config_toml.lines() {
        let line = line.trim();
        let Some(body) = line.strip_prefix("[mcp_servers.") else { continue };
        let Some(name) = body.strip_suffix(']') else { continue };
        let name = name.trim().trim_matches('"').trim_matches('\'').to_owned();
        // Sub-tables (`[mcp_servers.node_repl.env]`) are not servers, and
        // dotted leftovers never reach an override key anyway.
        if name.is_empty() || name == SERVER_NAME || name.contains('.') || names.contains(&name) {
            continue;
        }
        names.push(name);
    }
    names
}

/// Where Codex reads its config: `$CODEX_HOME/config.toml`, defaulting to
/// `~/.codex/config.toml`. Read, never written — the disable scan below
/// only learns table names from it.
pub fn codex_config_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".codex"))
        })?;
    Some(home.join("config.toml"))
}

/// What `~/.codex/config.toml` connects besides Baaz's own server, for
/// Settings › MCP (docs/82 8.8): its `[mcp_servers.<name>]` tables and its
/// plugins that are not `enabled = false`, read from the file — never from
/// `codex` itself. `scripted` reads the recorded
/// `fixtures/codex/mcp-config.toml` instead (a capture never reads the
/// person's files). An unreadable file lists nothing.
pub fn mcp_listing(scripted: bool) -> provider::McpListing {
    const FIXTURE: &str = include_str!("../../../fixtures/codex/mcp-config.toml");
    let path = codex_config_path().unwrap_or_else(|| std::path::PathBuf::from("~/.codex/config.toml"));
    let text = if scripted {
        FIXTURE.to_owned()
    } else {
        std::fs::read_to_string(&path).unwrap_or_default()
    };
    provider::McpListing {
        sources: vec![if scripted { std::path::PathBuf::from("~/.codex/config.toml") } else { path }],
        entries: listed_mcp(&text),
        unlisted: "Codex's built-in servers load too and are not listed.",
    }
}

/// [`mcp_listing`]'s entries from `config.toml` text: the file's servers
/// ([`file_server_names`], so never `baaz`), then its plugins left on.
pub fn listed_mcp(config_toml: &str) -> Vec<provider::ConfiguredMcp> {
    let off = disabled_plugins(config_toml);
    let servers = file_server_names(config_toml).into_iter().map(|name| provider::ConfiguredMcp { name, plugin: false });
    let plugins = plugin_names(config_toml)
        .into_iter()
        .filter(|name| !off.contains(name))
        .map(|name| provider::ConfiguredMcp { name, plugin: true });
    servers.chain(plugins).collect()
}

/// The plugins whose table says `enabled = false`: a line scan of each
/// `[plugins."<name>"]` table's own keys, up to the next header.
fn disabled_plugins(config_toml: &str) -> Vec<String> {
    let mut off = Vec::new();
    let mut current: Option<String> = None;
    for line in config_toml.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            current = line
                .strip_prefix("[plugins.")
                .and_then(|body| body.strip_suffix(']'))
                .map(|name| name.trim().trim_matches('"').trim_matches('\'').to_owned())
                .filter(|name| !name.contains('"'));
            continue;
        }
        let Some(name) = current.as_ref() else { continue };
        let Some((key, value)) = line.split_once('=') else { continue };
        if key.trim() == "enabled" && value.split('#').next().map(str::trim) == Some("false") {
            off.push(name.clone());
        }
    }
    off
}

/// Every inherited server to silence for one child: the bundled set plus
/// whatever `[mcp_servers]` tables the owner's config file names today.
/// Best-effort throughout — an unreadable file silences only the bundled
/// set, and the bridge override (which never depends on this) is
/// unaffected.
pub fn inherited_servers() -> Vec<String> {
    let mut names: Vec<String> =
        BUNDLED_SERVERS.iter().map(|name| name.to_string()).collect();
    if let Some(path) = codex_config_path() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for name in file_server_names(&text) {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Settings › MCP's Codex list: the recorded config's two servers (the
    /// sub-table and Baaz's own table left out), then its one plugin still
    /// on; the scripted read never touches the person's file.
    #[test]
    fn the_mcp_listing_reads_servers_and_plugins_left_on() {
        let listing = mcp_listing(true);
        let names: Vec<(&str, bool)> = listing.entries.iter().map(|e| (e.name.as_str(), e.plugin)).collect();
        assert_eq!(names, [("github", false), ("postgres", false), ("pdf@openai-primary-runtime", true)]);
        assert_eq!(listing.sources, [PathBuf::from("~/.codex/config.toml")]);
        assert!(listed_mcp("").is_empty(), "an empty or unreadable file lists nothing");
        assert!(listed_mcp("[plugins.\"x@y\"]\nenabled = false # off\n").is_empty(), "a trailing comment still reads off");
    }

    fn bridge() -> PathBuf {
        PathBuf::from("/Applications/Baaz.app/Contents/MacOS/mcp-bridge")
    }

    fn socket() -> PathBuf {
        PathBuf::from("/tmp/baaz/run/terminal-1.sock")
    }

    #[test]
    fn the_overrides_name_the_bridge_socket_and_session() {
        let extra = server_overrides(&bridge(), &socket(), "s-7");
        assert_eq!(extra.len(), 6);
        assert_eq!(extra[0], "-c");
        assert!(
            extra[1].starts_with("mcp_servers.baaz.command="),
            "the command rides a dotted path: {}",
            extra[1]
        );
        assert!(extra[1].contains("/Applications/Baaz.app/Contents/MacOS/mcp-bridge"));
        assert_eq!(extra[2], "-c");
        assert!(
            extra[3].starts_with("mcp_servers.baaz.args=["),
            "the args ride a TOML array: {}",
            extra[3]
        );
        assert!(extra[3].contains("\"--session\",\"s-7\""));
        // The session's token rides the server's env, never its argv tail
        // (docs/85 10.0).
        assert_eq!(extra[4], "-c");
        assert_eq!(
            extra[5],
            format!("mcp_servers.baaz.env={{BAAZ_BRIDGE_TOKEN=\"{}\"}}", provider::bridge_token::token_for("s-7"))
        );
        for arg in &extra {
            assert!(!arg.contains(".codex/config.toml"), "never the owner's file: {arg}");
            assert!(!arg.contains('~'), "no home-relative writes: {arg}");
        }
    }

    #[test]
    fn toml_quoting_survives_spaces_and_quotes() {
        assert_eq!(toml_string("/plain/path"), "\"/plain/path\"");
        assert_eq!(toml_string("/a b/c"), "\"/a b/c\"");
        assert_eq!(toml_string("say \"hi\""), "\"say \\\"hi\\\"\"");
        assert_eq!(toml_string("back\\slash"), "\"back\\\\slash\"");
    }

    #[test]
    fn disable_names_plain_servers_only() {
        // Full-table: the single-key form replaces the table and breaks
        // startup (probed live: `invalid transport in mcp_servers.…`).
        assert_eq!(
            disable_override("node_repl"),
            Some(vec![
                "-c".to_owned(),
                "mcp_servers.node_repl={enabled=false,command=\"/bin/true\"}".to_owned()
            ])
        );
        assert_eq!(disable_override("baaz"), None, "the bridge is never silenced");
        assert_eq!(disable_override("a.b"), None, "no smuggled path segments");
        assert_eq!(disable_override(""), None);
        // Batched, in order, refusing what the single form refuses.
        assert_eq!(
            disable_overrides(["cua_repl", "a.b", "baaz"]),
            vec![
                "-c".to_owned(),
                "mcp_servers.cua_repl={enabled=false,command=\"/bin/true\"}".to_owned()
            ]
        );
        assert!(disable_overrides(Vec::<&str>::new()).is_empty());
    }

    #[test]
    fn the_config_scan_names_tables_not_subtables() {
        let text = "[mcp_servers.node_repl]\ncommand = \"x\"\n\
            [mcp_servers.node_repl.env]\nA = \"1\"\n\
            [mcp_servers.baaz]\ncommand = \"y\"\n\
            [other]\n";
        assert_eq!(file_server_names(text), vec!["node_repl".to_owned()]);
        assert!(file_server_names("no tables here").is_empty());
    }

    #[test]
    fn the_bundled_set_names_what_the_probe_saw() {
        // `codex_apps` (97 tools), `cua_repl`, `codex_app`: bundled, not
        // file-configured, still starting in every session. `node_repl`
        // joins the floor (Z5 probe 2026-09-28: file-configured here,
        // floor-covered everywhere).
        for name in ["codex_app", "codex_apps", "cua_repl", "computer-use", "node_repl"] {
            assert!(BUNDLED_SERVERS.contains(&name), "{name} stays covered");
        }
    }

    #[test]
    fn plugins_and_install_suggestions_go_off_by_feature_key() {
        assert_eq!(plugins_off(), ["-c".to_owned(), "features.plugins=false".to_owned()]);
        assert_eq!(tool_suggest_off(), ["-c".to_owned(), "features.tool_suggest=false".to_owned()]);
    }

    #[test]
    fn the_plugin_scan_names_tables_not_subtables() {
        let text = "[plugins.\"browser@openai-bundled\"]\nenabled = true\n\
            [plugins.\"browser@openai-bundled\".env]\nA = \"1\"\n\
            [plugins.\"pdf@openai-primary-runtime\"]\nenabled = true\n\
            [mcp_servers.node_repl]\ncommand = \"x\"\n";
        assert_eq!(
            plugin_names(text),
            vec!["browser@openai-bundled".to_owned(), "pdf@openai-primary-runtime".to_owned()]
        );
        assert!(plugin_names("no tables here").is_empty());
    }
}
