//! Session argv: `OpenSession`, `ResumeSession` and `ForkSession` as
//! argument vectors, tested without running anything.
//!
//! One long-lived child per session (doc §1):
//!
//! ```text
//! claude --print --input-format stream-json --output-format stream-json
//!        --verbose [--model <id>] [--mcp-config <path> --strict-mcp-config]
//!        [--session-id <uuid> | --resume <uuid> [--fork-session]]
//! ```
//!
//! `--strict-mcp-config` rides every session (doc §4): without it the
//! session inherits the operator's unrelated connectors (Z5 — the stage-3
//! probe found five leaking). The opt-in strips it back off (see
//! [`without_strict_mcp`]).
//! User turns go to stdin as NDJSON ([`user_input_line`]); the prompt is
//! never passed positionally (a variadic flag would eat it — doc §1).

use aui_protocol::PermissionMode;

/// How permission decisions reach the child (addendum 2026-09-24):
/// `--permission-prompts host` routes every tool approval to the host, and
/// `--permission-prompt-tool stdio` — a sentinel, not a tool name — delivers
/// each `can_use_tool` request over the same stream-json control channel the
/// adapter already reads, answered with a `control_response`. Both flags are
/// required together: `host` alone auto-denies without ever asking.
///
/// How to start one child: its argv, its working directory, and the session
/// id both sides agree on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionLaunch {
    /// Arguments after the program name.
    pub argv: Vec<String>,
    /// Working directory for the child, from `OpenSession.workspace`.
    /// `None` means inherit.
    pub cwd: Option<String>,
    /// The session id Baaz and Claude Code share.
    pub session_id: String,
    /// The model flag, when one was requested.
    pub model: Option<String>,
    /// The effort flag, when one was requested.
    pub effort: Option<String>,
    /// The `--permission-mode` flag, when one was requested (see
    /// [`with_permission_mode`]).
    pub permission_mode: Option<PermissionMode>,
}

/// The `--permission-mode` spelling for each neutral approval mode.
/// `claude --help` (2.1.276) lists `acceptEdits, auto, bypassPermissions,
/// manual, dontAsk, plan`; `manual` is the asking mode the CLI reports
/// back as `default` (probed live 2026-10-16).
pub fn claude_permission_mode(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::AllowAll => "bypassPermissions",
        PermissionMode::OnRequest => "auto",
        PermissionMode::PromptUnmatched => "manual",
        PermissionMode::DenyUnmatched => "dontAsk",
    }
}

/// The neutral mode behind a mode the CLI reports (`system/init` and
/// `system/status` `permissionMode`, `initialize`'s
/// `current_permission_mode`, `set_permission_mode`'s `mode`). `default`
/// and `manual` are one mode. `acceptEdits` still asks for anything but
/// edits, so it reads as Auto. An unknown spelling folds to nothing rather
/// than a guess.
///
/// `plan` folds to nothing too, so the chip keeps the mode Baaz set: plan
/// mode is not Read-only's equal. It *asks* before a write rather than
/// refusing it, and an owner allow rule is matched before its write check
/// (2.1.276 source), so reading it as Read-only would claim a guarantee
/// that only [`read_only_settings`] gives. Baaz never requests `plan`;
/// when the model enters it from Read-only, the deny layer is still in
/// force and the chip rightly stays Read-only.
pub fn permission_mode_from_claude(wire: &str) -> Option<PermissionMode> {
    match wire {
        "bypassPermissions" => Some(PermissionMode::AllowAll),
        "auto" | "acceptEdits" => Some(PermissionMode::OnRequest),
        "default" | "manual" => Some(PermissionMode::PromptUnmatched),
        "dontAsk" => Some(PermissionMode::DenyUnmatched),
        _ => None,
    }
}

/// The tools Read-only denies outright: every built-in that writes or
/// runs a shell, and Baaz's own MCP tools that act (the bridge's reading
/// tools — `terminal_list/read/screen`, `browser_open/read/links/
/// screenshot` — stay usable).
///
/// `dontAsk` alone is not Read-only: it refuses only what no rule allows,
/// so an owner allow rule (`Bash(muse exec:*)` in the symlinked
/// `~/.claude/settings.json`) still ran. A deny rule is checked before any
/// allow rule, whatever its source — the 2.1.276 permission check returns
/// on a whole-tool or content deny before it consults allow rules — so
/// these hold against every owner rule. Bash is denied whole rather than
/// sandboxed: the settings sandbox (`sandbox.enabled` with
/// `filesystem.denyWrite`) is accepted by the CLI, but its write block
/// could not be shown without running a turn, and the CLI itself warns
/// that a live sandbox change "may not be fenced until the settings
/// change is picked up".
///
/// Enumerated from the 2.1.276 binary's own tool table (2026-10-16), not
/// from memory: every tool it flags `enablesCodeExecution` (Bash,
/// PowerShell, Monitor, AppifactRepl, Workflow, CronCreate, RemoteTrigger,
/// the two self-hosted-runner actions), the JavaScript `REPL`, and every
/// tool that writes a file, a worktree or remote state (Edit, MultiEdit,
/// Write, NotebookEdit, EnterWorktree, ExitWorktree, memory_write, the
/// Artifact tools, DesignSync, ClaudeDesign, SendFile,
/// ShareOnboardingGuide). The old names the CLI keeps as aliases
/// (`KillShell`/`BashOutput` → `TaskStop`/`TaskOutput`) only stop or read
/// a background task, so they stay. `Agent` (`Task`) stays too: a deny
/// rule is the session's, so a subagent meets the same list.
pub const READ_ONLY_DENIED_TOOLS: [&str; 25] = [
    // Runs commands or code.
    "Bash",
    "PowerShell",
    "Monitor",
    "REPL",
    "AppifactRepl",
    "Workflow",
    "CronCreate",
    "RemoteTrigger",
    "self_hosted_runner_spawn_local",
    "self_hosted_runner_requeue_session",
    // Writes files or a worktree.
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "EnterWorktree",
    "ExitWorktree",
    "memory_write",
    // Writes remote state.
    "Artifact",
    "ArtifactData",
    "ArtifactComments",
    "DesignSync",
    "ClaudeDesign",
    "SendFile",
    "ShareOnboardingGuide",
    "SendFeedback",
];

/// The Baaz bridge tools Read-only denies (served as
/// `mcp__baaz__<tool>`): those that type, run, open or close.
pub const READ_ONLY_DENIED_BRIDGE_TOOLS: [&str; 6] =
    ["terminal_open", "terminal_run", "terminal_send", "terminal_close", "browser_click", "browser_type"];

/// The settings layer that makes Read-only read-only: `permissions.deny`
/// for [`READ_ONLY_DENIED_TOOLS`] and [`READ_ONLY_DENIED_BRIDGE_TOOLS`],
/// plus `disableSkillShellExecution` so a skill's inline shell cannot run
/// around the Bash deny. It rides `--settings` on every Read-only launch
/// (see [`with_permission_mode`]) and `apply_flag_settings` on a live
/// switch into Read-only.
///
/// The two carriers are different layers (probed 2026-10-16 with
/// `list_permission_rules`/`get_settings`): an `apply_flag_settings`
/// replaces its own earlier keys — `{"permissions": null}` drops the deny
/// rules it set, and an `effortLevel` apply leaves them standing — but it
/// cannot remove rules that rode `--settings`. So a child launched
/// Read-only leaves it by relaunching, and one switched in live leaves it
/// live with [`read_only_settings_cleared`].
pub fn read_only_settings() -> serde_json::Value {
    read_only_settings_with(&[])
}

/// [`read_only_settings`] plus `mcp_denies` — the owner's own MCP tools
/// that can act ([`read_only_mcp_denies`]) — on the same deny list. This is
/// the shape every live (`apply_flag_settings`) Read-only layer takes: an
/// apply replaces the layer's whole `permissions` key, so the base rules
/// ride along with every refresh of the MCP ones.
pub fn read_only_settings_with(mcp_denies: &[String]) -> serde_json::Value {
    let mut deny: Vec<String> = READ_ONLY_DENIED_TOOLS.iter().map(|tool| (*tool).to_owned()).collect();
    deny.extend(
        READ_ONLY_DENIED_BRIDGE_TOOLS
            .iter()
            .map(|tool| format!("mcp__{}__{tool}", crate::terminal::SERVER_NAME)),
    );
    deny.extend(mcp_denies.iter().cloned());
    serde_json::json!({
        "permissions": {"deny": deny},
        "disableSkillShellExecution": true,
    })
}

/// One MCP server as the CLI's `mcp_status` control request reports it
/// (2.1.276 `McpServerStatus`, probed 2026-10-17 against a scratch stdio
/// server): `tools` is present only once the server connected, each tool
/// carrying the MCP annotations folded to `readOnly`/`destructive`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServerStatus {
    /// The server name as configured (not yet normalized for a rule).
    pub name: String,
    /// `connected`, `pending`, `failed`, `needs-auth` or `disabled`.
    pub status: String,
    /// The tools a connected server serves; `None` when the answer named
    /// none (any status but `connected`).
    pub tools: Option<Vec<McpToolStatus>>,
}

/// One tool of an [`McpServerStatus`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpToolStatus {
    /// The tool name as the server serves it.
    pub name: String,
    /// Whether the server marked it `readOnlyHint: true` (the CLI reports
    /// it as `annotations.readOnly`).
    pub read_only: bool,
}

/// A server or tool name as the CLI spells it inside an `mcp__…` tool
/// name and permission rule (2.1.276 `normalizeNameForMCP`): every
/// character outside `[A-Za-z0-9_-]` becomes `_`, and a `claude.ai `
/// connector name also collapses runs of `_` and trims them at the ends.
pub fn mcp_rule_segment(name: &str) -> String {
    let mut segment: String = name
        .chars()
        .map(|char| if char.is_ascii_alphanumeric() || char == '_' || char == '-' { char } else { '_' })
        .collect();
    if name.starts_with("claude.ai ") {
        let mut collapsed = String::with_capacity(segment.len());
        for char in segment.chars() {
            if !(char == '_' && collapsed.ends_with('_')) {
                collapsed.push(char);
            }
        }
        segment = collapsed.trim_matches('_').to_owned();
    }
    segment
}

/// The deny rules Read-only adds for the owner's own MCP servers: every
/// tool a connected server did NOT mark `readOnlyHint: true`
/// (`mcp__<server>__<tool>`), and every server whose tools are not known
/// yet — pending, failed, needing auth — whole (`mcp__<server>`, which the
/// CLI matches against every tool of that server). Baaz's own bridge is
/// left to [`READ_ONLY_DENIED_BRIDGE_TOOLS`]. A deny is checked before any
/// allow rule, whatever its source (see [`READ_ONLY_DENIED_TOOLS`]), so an
/// owner allow rule on one of these no longer runs it in Read-only.
///
/// The annotation is the server's own claim: a server that marks an
/// acting tool read-only is believed, exactly as Codex and Muse believe it.
pub fn read_only_mcp_denies(servers: &[McpServerStatus]) -> Vec<String> {
    let mut denies = Vec::new();
    for server in servers {
        if server.name == crate::terminal::SERVER_NAME {
            continue;
        }
        let segment = mcp_rule_segment(&server.name);
        if segment.is_empty() {
            continue;
        }
        match (&server.tools, server.status.as_str()) {
            (Some(tools), "connected") => denies.extend(
                tools
                    .iter()
                    .filter(|tool| !tool.read_only)
                    .map(|tool| format!("mcp__{segment}__{}", mcp_rule_segment(&tool.name))),
            ),
            _ => denies.push(format!("mcp__{segment}")),
        }
    }
    denies.sort();
    denies.dedup();
    denies
}

/// The `apply_flag_settings` payload that takes [`read_only_settings`]
/// back off a child it was applied to live: `null` removes each key from
/// the applied layer (the owner's own settings underneath stand).
pub fn read_only_settings_cleared() -> serde_json::Value {
    serde_json::json!({
        "permissions": null,
        "disableSkillShellExecution": null,
    })
}

/// Add `--permission-mode <mode>` to a built launch, replacing any the
/// launch already carried; `None` leaves the launch as built (the CLI's
/// own default). `bypassPermissions` needs no extra flag at launch — a
/// child spawned in it starts there (probed live 2026-10-16) — but only
/// such a child may switch INTO it later over the control channel; any
/// other refuses "the session was not launched with
/// --dangerously-skip-permissions", which is why a mid-session pick of
/// Full access relaunches instead.
///
/// Read-only also carries `--settings <read_only_settings>`, and every
/// other mode strips it: the deny layer follows the mode through every
/// open, resume, fork and relaunch built here.
pub fn with_permission_mode(mut launch: SessionLaunch, mode: Option<PermissionMode>) -> SessionLaunch {
    let Some(mode) = mode else { return launch };
    if let Some(at) = launch.argv.iter().position(|arg| arg == "--permission-mode") {
        launch.argv.drain(at..(at + 2).min(launch.argv.len()));
    }
    let layer = read_only_settings().to_string();
    if let Some(at) = launch.argv.windows(2).position(|pair| pair[0] == "--settings" && pair[1] == layer) {
        launch.argv.drain(at..at + 2);
    }
    launch.argv.push("--permission-mode".into());
    launch.argv.push(claude_permission_mode(mode).to_owned());
    if mode == PermissionMode::DenyUnmatched {
        launch.argv.push("--settings".into());
        launch.argv.push(layer);
    }
    launch.permission_mode = Some(mode);
    launch
}

/// Whether a built launch carries the Read-only deny layer on `--settings`
/// — a layer the child can only shed by relaunching.
pub fn carries_read_only_settings(launch: &SessionLaunch) -> bool {
    let layer = read_only_settings().to_string();
    launch.argv.windows(2).any(|pair| pair[0] == "--settings" && pair[1] == layer)
}

/// The flags every session child carries.
pub fn base_argv() -> Vec<String> {
    [
        "--print",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        // Permission routing (addendum 2026-09-24). Each flag takes exactly
        // one value — no variadic trap here — but they stay in this fixed
        // order with the rest of the lane so probes and argv agree.
        "--permission-prompts",
        "host",
        "--permission-prompt-tool",
        "stdio",
        // The prompt echo (W4b, live probe 2026-09-26): without this the
        // child never re-emits the submitted text and the transcript has
        // no user bubble — the turn shows the tool card and reply but not
        // what the person sent. The fold renders the echo as `Turn::User`.
        // Resume and fork keep the flag on purpose (W4d, live probe
        // 2026-09-26, `resume-replay.jsonl`): a resumed child emits only
        // the NEW turn's echo — no history replay — and a flagless resume
        // emits no echo at all, so the post-resume turn would lose its
        // bubble. History collisions are handled by uuid, not by dropping
        // the flag: see `ClaudeFold::mark_user_echo_seen`.
        "--replay-user-messages",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

fn with_model(mut argv: Vec<String>, model: Option<&str>) -> Vec<String> {
    if let Some(model) = model {
        argv.push("--model".into());
        argv.push(model.to_owned());
    }
    argv
}

/// The effort levels the installed CLI accepts (`claude --help`):
/// `--effort <level>`, one of these five. Effort is a launch flag, not a
/// per-turn channel — there is no effort surface over stream-json stdin —
/// so a mid-session change relaunches the child with `--resume` plus the
/// new flag (see the adapter's `SubmitInput` arm).
pub const CLAUDE_EFFORT_LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

fn with_effort(mut argv: Vec<String>, effort: Option<&str>) -> Vec<String> {
    if let Some(effort) = effort.filter(|effort| !effort.is_empty()) {
        argv.push("--effort".into());
        argv.push(effort.to_owned());
    }
    argv
}

fn with_mcp(mut argv: Vec<String>, mcp_config: Option<&str>) -> Vec<String> {
    if let Some(path) = mcp_config {
        argv.push("--mcp-config".into());
        argv.push(path.to_owned());
    }
    // Doc §4: without this the session sees the operator's connectors —
    // so it rides EVERY session, even when no bridge config named one
    // (Z5: the stage-3 probe found five leaking connectors). `claude
    // --help` lists `--strict-mcp-config` as a standalone boolean flag
    // ("Only use MCP servers from --mcp-config, ignoring all other MCP
    // configurations") with no required `--mcp-config` beside it, so no
    // empty config is passed: strict alone means no servers at all. The
    // opt-in strips it back off (see [`without_strict_mcp`]).
    argv.push("--strict-mcp-config".into());
    argv
}

/// Drop every `--strict-mcp-config` from a built launch: the Settings →
/// Providers "Use my own MCP servers" opt-in, applied by the adapter when
/// `use_own_mcp` is set. The `--mcp-config` bridge file (when one rode
/// the launch) stays — the session gains the owner's servers beside
/// Baaz's, exactly as it would without Baaz's scoping.
pub fn without_strict_mcp(mut launch: SessionLaunch) -> SessionLaunch {
    launch.argv.retain(|arg| arg != "--strict-mcp-config");
    launch
}

/// Open a new session. `--session-id` carries the caller-chosen id, so a
/// Baaz session id and a Claude Code session id are the same string and
/// `ReadSession` needs no map (doc §3). The request id doubles as that id:
/// retrying with the same id re-addrs the same session rather than opening
/// twice.
pub fn argv_for_open(
    request_id: &str,
    workspace: Option<&str>,
    model: Option<&str>,
    mcp_config: Option<&str>,
    effort: Option<&str>,
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--session-id".into());
    argv.push(request_id.to_owned());
    argv = with_model(argv, model);
    argv = with_effort(argv, effort);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: workspace.map(str::to_owned),
        session_id: request_id.to_owned(),
        model: model.map(str::to_owned),
        effort: effort.filter(|effort| !effort.is_empty()).map(str::to_owned),
        permission_mode: None,
    }
}

/// Re-attach to a stored session. `--resume` keeps the same id (doc §3).
/// `effort` re-applies the level: the adapter's `SubmitInput` arm builds
/// this shape when the picked effort differs from the running child's, so
/// the next turn relaunches with `--resume <id> --effort <new>`.
pub fn argv_for_resume(
    session_id: &str,
    model: Option<&str>,
    mcp_config: Option<&str>,
    effort: Option<&str>,
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--resume".into());
    argv.push(session_id.to_owned());
    argv = with_model(argv, model);
    argv = with_effort(argv, effort);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: None,
        session_id: session_id.to_owned(),
        model: model.map(str::to_owned),
        effort: effort.filter(|effort| !effort.is_empty()).map(str::to_owned),
        permission_mode: None,
    }
}

/// Branch a session. `--resume` plus `--fork-session` mints the new id;
/// `request_id` is the Baaz-side id for the branch. Whether the CLI honors
/// a `--session-id` alongside `--fork-session` was never probed live, so
/// the argv carries it as the caller's intent and the report says so.
pub fn argv_for_fork(
    request_id: &str,
    session_id: &str,
    model: Option<&str>,
    mcp_config: Option<&str>,
    effort: Option<&str>,
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--resume".into());
    argv.push(session_id.to_owned());
    argv.push("--fork-session".into());
    argv.push("--session-id".into());
    argv.push(request_id.to_owned());
    argv = with_model(argv, model);
    argv = with_effort(argv, effort);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: None,
        session_id: request_id.to_owned(),
        model: model.map(str::to_owned),
        effort: effort.filter(|effort| !effort.is_empty()).map(str::to_owned),
        permission_mode: None,
    }
}

/// One text-only user turn as NDJSON for the child's stdin (doc §1
/// input frame). Text parts join in order; use [`user_content_line`] when
/// the turn carries images.
pub fn user_input_line(text: &str) -> String {
    user_content_line(text, &[], None)
}

/// One user turn carrying text plus image parts, as NDJSON for the
/// child's stdin. Probed live 2026-09-26 (`fixtures/claude-code/image.jsonl`):
/// an `image` content part with a base64 `source` is accepted and echoed
/// back by `--replay-user-messages` with its media type and length.
///
/// `uuid`, when given, rides the line's top-level `uuid`: the CLI's echo
/// repeats it (probed live 2026-10-16, claude 2.1.276), which is how the
/// fold pairs each line with the turn that answers it.
pub fn user_content_line(text: &str, images: &[ImageInput], uuid: Option<&str>) -> String {
    let mut content = vec![serde_json::json!({"type": "text", "text": text})];
    for image in images {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": image.media_type,
                "data": image.base64_data,
            },
        }));
    }
    let mut line = serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": content},
    });
    if let Some(uuid) = uuid {
        line["uuid"] = serde_json::Value::String(uuid.to_owned());
    }
    line.to_string()
}

/// A fresh random (version 4) uuid for one stdin line. Hashed from the
/// standard library's per-instance random keys, so the crate needs no
/// uuid dependency; uniqueness within a session is all it must give.
pub fn line_uuid() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| age.as_nanos())
        .unwrap_or(0);
    let half = |salt: u8| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(nanos);
        hasher.write_u8(salt);
        hasher.finish()
    };
    let bits = (u128::from(half(0)) << 64) | u128::from(half(1));
    // Version 4, RFC 4122 variant.
    let bits = (bits & !(0xF << 76)) | (0x4 << 76);
    let bits = (bits & !(0x3 << 62)) | (0x2 << 62);
    let hex = format!("{bits:032x}");
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// One image part of an outgoing user turn: base64 bytes plus their media
/// type, exactly what the seam's `SubmissionPart::Image` carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInput {
    /// Base64-encoded bytes.
    pub base64_data: String,
    /// e.g. `"image/png"`.
    pub media_type: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(name: &str, read_only: bool) -> McpToolStatus {
        McpToolStatus { name: name.into(), read_only }
    }

    #[test]
    fn read_only_denies_the_owners_acting_mcp_tools_and_unknown_servers() {
        let servers = vec![
            McpServerStatus {
                name: "probe".into(),
                status: "connected".into(),
                tools: Some(vec![tool("peek", true), tool("poke", false), tool("zap", false)]),
            },
            McpServerStatus { name: "claude.ai Gmail".into(), status: "needs-auth".into(), tools: None },
            McpServerStatus { name: "slow.server".into(), status: "pending".into(), tools: None },
            // Baaz's own bridge rides its own list.
            McpServerStatus {
                name: crate::terminal::SERVER_NAME.into(),
                status: "connected".into(),
                tools: Some(vec![tool("terminal_run", false)]),
            },
            McpServerStatus { name: "quiet".into(), status: "connected".into(), tools: Some(Vec::new()) },
        ];
        assert_eq!(
            read_only_mcp_denies(&servers),
            vec!["mcp__claude_ai_Gmail", "mcp__probe__poke", "mcp__probe__zap", "mcp__slow_server"],
        );
    }

    #[test]
    fn mcp_rule_segments_match_the_clis_normalization() {
        assert_eq!(mcp_rule_segment("my-server_1"), "my-server_1");
        assert_eq!(mcp_rule_segment("plugin:foo:bar"), "plugin_foo_bar");
        assert_eq!(mcp_rule_segment("claude.ai  Google Drive"), "claude_ai_Google_Drive");
        assert_eq!(mcp_rule_segment("a.b  c"), "a_b__c", "only claude.ai names collapse");
    }

    #[test]
    fn the_live_layer_keeps_the_base_rules_beside_the_mcp_ones() {
        assert_eq!(read_only_settings_with(&[]), read_only_settings());
        let layer = read_only_settings_with(&["mcp__probe__poke".to_owned()]);
        let deny: Vec<&str> =
            layer["permissions"]["deny"].as_array().expect("deny").iter().filter_map(|rule| rule.as_str()).collect();
        assert!(deny.contains(&"Bash") && deny.contains(&"mcp__baaz__terminal_run"), "{deny:?}");
        assert_eq!(deny.last(), Some(&"mcp__probe__poke"));
        assert_eq!(layer["disableSkillShellExecution"], true);
    }

    #[test]
    fn the_chip_seed_never_reaches_the_argv() {
        // X4: the chip's display seed (env, `claude` settings, the last
        // reported model) is view-only. A session the person never picked
        // a model for opens with no `--model` flag on every launch shape,
        // letting the CLI resolve its own default exactly as before; only
        // an explicit pick adds the flag.
        for argv in [
            argv_for_open("req-1", Some("/work"), None, None, None).argv,
            argv_for_resume("sess-9", None, None, None).argv,
            argv_for_fork("branch-2", "sess-9", None, None, None).argv,
        ] {
            assert!(
                !argv.iter().any(|arg| arg == "--model"),
                "no model flag without a pick: {argv:?}"
            );
        }
        let picked = argv_for_open("req-1", Some("/work"), Some("opus"), None, None);
        let position = picked.argv.iter().position(|arg| arg == "--model").expect("--model");
        assert_eq!(picked.argv.get(position + 1).map(String::as_str), Some("opus"));
        assert_eq!(picked.model.as_deref(), Some("opus"));
    }

    #[test]
    fn open_chooses_the_session_id() {
        let launch = argv_for_open("req-1", Some("/work"), Some("haiku"), None, None);
        assert!(launch.argv.contains(&"--session-id".to_owned()));
        assert!(launch.argv.contains(&"req-1".to_owned()));
        assert!(!launch.argv.iter().any(|arg| arg == "--resume"));
        assert_eq!(launch.session_id, "req-1");
        assert_eq!(launch.cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn effort_rides_the_launch_flag_when_set_and_nowhere_when_not() {
        // `claude --help` lists `--effort <level>`; every launch shape
        // carries it when an effort is set and omits it when none is, so
        // the default session spawns exactly today's argv.
        for launch in [
            argv_for_open("req-1", Some("/work"), Some("haiku"), None, Some("high")),
            argv_for_resume("sess-9", None, None, Some("high")),
            argv_for_fork("branch-2", "sess-9", None, None, Some("high")),
        ] {
            let position =
                launch.argv.iter().position(|arg| arg == "--effort").expect("effort flag");
            assert_eq!(launch.argv.get(position + 1).map(String::as_str), Some("high"));
            assert_eq!(launch.effort.as_deref(), Some("high"));
        }
        for argv in [
            argv_for_open("req-1", Some("/work"), Some("haiku"), None, None).argv,
            argv_for_resume("sess-9", None, None, None).argv,
            argv_for_fork("branch-2", "sess-9", None, None, None).argv,
            base_argv(),
        ] {
            assert!(
                !argv.iter().any(|arg| arg == "--effort"),
                "no effort flag without a level: {argv:?}"
            );
        }
    }

    #[test]
    fn the_menu_lists_what_the_cli_accepts() {
        assert_eq!(CLAUDE_EFFORT_LEVELS, ["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn resume_keeps_the_id_and_fork_adds_the_flag() {
        let resume = argv_for_resume("sess-9", None, None, None);
        assert!(resume.argv.contains(&"--resume".to_owned()));
        assert!(resume.argv.contains(&"sess-9".to_owned()));
        assert!(!resume.argv.iter().any(|arg| arg == "--fork-session"));

        let fork = argv_for_fork("branch-2", "sess-9", None, None, None);
        assert!(fork.argv.contains(&"--resume".to_owned()));
        assert!(fork.argv.contains(&"--fork-session".to_owned()));
    }

    #[test]
    fn strict_rides_every_session_with_or_without_a_bridge() {
        // Z5: the stage-3 probe found five leaking owner connectors, so
        // strict no longer waits for a bridge config — and `claude --help`
        // lists it as a standalone flag, so no empty `--mcp-config` rides
        // beside it when there is no bridge.
        let launch = argv_for_open("req-1", None, None, Some("/tmp/mcp.json"), None);
        let argv = launch.argv;
        assert!(argv.contains(&"--mcp-config".to_owned()));
        assert!(argv.contains(&"--strict-mcp-config".to_owned()));
        for argv in [
            argv_for_open("req-1", None, None, None, None).argv,
            argv_for_resume("sess-9", None, None, None).argv,
            argv_for_fork("branch-2", "sess-9", None, None, None).argv,
        ] {
            assert!(
                argv.contains(&"--strict-mcp-config".to_owned()),
                "strict without a bridge too: {argv:?}"
            );
            assert!(
                !argv.iter().any(|arg| arg == "--mcp-config"),
                "but no config flag without a file: {argv:?}"
            );
        }
    }

    #[test]
    fn the_opt_in_drops_strict_and_keeps_the_bridge() {
        let launch = argv_for_open("req-1", None, None, Some("/tmp/mcp.json"), None);
        let stripped = without_strict_mcp(launch);
        assert!(stripped.argv.contains(&"--mcp-config".to_owned()));
        assert!(stripped.argv.contains(&"/tmp/mcp.json".to_owned()));
        assert!(
            !stripped.argv.iter().any(|arg| arg == "--strict-mcp-config"),
            "opted in means no strict: {:?}",
            stripped.argv
        );
        // Idempotent: stripping twice is stripping once.
        let twice = without_strict_mcp(stripped);
        assert!(!twice.argv.iter().any(|arg| arg == "--strict-mcp-config"));
    }

    #[test]
    fn base_lane_is_one_long_lived_stream_json_child() {
        let argv = base_argv();
        for flag in [
            "--print",
            "--input-format",
            "stream-json",
            "--output-format",
            "--verbose",
        ] {
            assert!(argv.contains(&flag.to_owned()), "missing {flag}");
        }
        // Permission routing (addendum 2026-09-24): both flags, each with
        // exactly one value, in lane order. `host` alone auto-denies, so a
        // missing `stdio` sentinel is a silent tool refusal, not a parse
        // error — assert the pair, not one half.
        let prompts = argv.iter().position(|arg| arg == "--permission-prompts");
        let tool = argv.iter().position(|arg| arg == "--permission-prompt-tool");
        let (prompts, tool) = (prompts.expect("host flag"), tool.expect("stdio flag"));
        assert_eq!(argv.get(prompts + 1).map(String::as_str), Some("host"));
        assert_eq!(argv.get(tool + 1).map(String::as_str), Some("stdio"));
        assert!(prompts < tool, "ordering discipline: prompts before prompt-tool");
        // Every launcher shares the lane, so open/resume/fork all route
        // permissions to the host.
        assert!(argv_for_open("req-1", None, None, None, None).argv.contains(&"stdio".to_owned()));
        assert!(argv_for_resume("sess-9", None, None, None).argv.contains(&"stdio".to_owned()));
        assert!(argv_for_fork("branch-2", "sess-9", None, None, None).argv.contains(&"stdio".to_owned()));
    }

    #[test]
    fn user_turns_are_single_ndjson_lines() {
        let line = user_input_line("Say A1");
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(value.get("type").and_then(|t| t.as_str()), Some("user"));
    }

    #[test]
    fn base_lane_replays_user_messages() {
        // Defect 1: without this flag the child never re-emits the
        // submitted text and the transcript has no user bubble. That
        // holds for resume too — a flagless resume emits no echo at all
        // (probed live: the R4 turn answered with no user frame) — so
        // resume and fork keep the flag; history collisions are handled
        // by echo uuid, not by dropping it.
        assert!(
            base_argv().contains(&"--replay-user-messages".to_owned()),
            "every launcher echoes the prompt"
        );
        for argv in [
            argv_for_open("req-1", None, None, None, None).argv,
            argv_for_resume("sess-9", None, None, None).argv,
            argv_for_fork("branch-2", "sess-9", None, None, None).argv,
        ] {
            assert!(
                argv.contains(&"--replay-user-messages".to_owned()),
                "open/resume/fork all echo: {argv:?}"
            );
        }
    }

    #[test]
    fn image_turns_carry_base64_source_parts() {
        // Probed live (`fixtures/claude-code/image.jsonl`): the `image`
        // part with a base64 `source` is what the CLI accepts.
        let line = user_content_line("see this", &[ImageInput {
            base64_data: "aGVsbG8=".into(),
            media_type: "image/png".into(),
        }], None);
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        let content = value
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(serde_json::Value::as_array)
            .expect("content array");
        assert_eq!(content.len(), 2);
        assert_eq!(
            content[1].get("type").and_then(serde_json::Value::as_str),
            Some("image")
        );
        let source = content[1].get("source").expect("image source");
        assert_eq!(
            source.get("media_type").and_then(serde_json::Value::as_str),
            Some("image/png")
        );
        assert_eq!(
            source.get("data").and_then(serde_json::Value::as_str),
            Some("aGVsbG8=")
        );
        // And the text-only spelling is unchanged: one text part.
        let plain: serde_json::Value =
            serde_json::from_str(&user_input_line("Say A1")).expect("one JSON object");
        assert_eq!(
            plain
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len),
            Some(1)
        );
    }

    #[test]
    fn each_mode_rides_every_launch_shape_and_none_rides_none() {
        let cases = [
            (PermissionMode::AllowAll, "bypassPermissions"),
            (PermissionMode::OnRequest, "auto"),
            (PermissionMode::PromptUnmatched, "manual"),
            (PermissionMode::DenyUnmatched, "dontAsk"),
        ];
        for (mode, wire) in cases {
            for launch in [
                argv_for_open("req-1", Some("/work"), None, None, None),
                argv_for_resume("sess-9", None, None, None),
                argv_for_fork("branch-2", "sess-9", None, None, None),
            ] {
                let launch = with_permission_mode(launch, Some(mode));
                let at = launch.argv.iter().position(|arg| arg == "--permission-mode").expect("flag");
                assert_eq!(launch.argv.get(at + 1).map(String::as_str), Some(wire));
                assert_eq!(launch.permission_mode, Some(mode));
                // No extra allow flag: a Full access launch starts there
                // on `--permission-mode` alone (probed live 2026-10-16).
                assert!(!launch.argv.iter().any(|arg| arg.contains("dangerously")), "{:?}", launch.argv);
            }
            // What the CLI reports back folds to the same mode.
            assert_eq!(permission_mode_from_claude(wire), Some(mode));
        }
        let bare = with_permission_mode(argv_for_open("req-1", None, None, None, None), None);
        assert!(!bare.argv.iter().any(|arg| arg == "--permission-mode"), "no mode, no flag");
        assert_eq!(bare.permission_mode, None);
    }

    /// The `--settings` value on a launch, parsed.
    fn settings_of(launch: &SessionLaunch) -> Option<serde_json::Value> {
        let at = launch.argv.iter().position(|arg| arg == "--settings")?;
        Some(serde_json::from_str(launch.argv.get(at + 1)?).expect("settings JSON"))
    }

    #[test]
    fn read_only_carries_the_deny_layer_on_every_launch_shape_and_no_other_mode_does() {
        for launch in [
            argv_for_open("req-1", Some("/work"), None, None, None),
            argv_for_resume("sess-9", None, None, None),
            argv_for_fork("branch-2", "sess-9", None, None, None),
        ] {
            let read_only = with_permission_mode(launch.clone(), Some(PermissionMode::DenyUnmatched));
            let settings = settings_of(&read_only).expect("Read-only rides --settings");
            let deny: Vec<&str> = settings["permissions"]["deny"]
                .as_array()
                .expect("deny list")
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect();
            for tool in ["Bash", "Edit", "MultiEdit", "Write", "NotebookEdit", "PowerShell"] {
                assert!(deny.contains(&tool), "{tool} denied: {deny:?}");
            }
            for tool in ["terminal_run", "terminal_send", "terminal_open", "terminal_close", "browser_click", "browser_type"] {
                assert!(deny.contains(&format!("mcp__baaz__{tool}").as_str()), "{tool} denied: {deny:?}");
            }
            // The reading bridge tools stay usable.
            for tool in ["terminal_list", "terminal_read", "terminal_screen", "browser_read", "browser_screenshot"] {
                assert!(!deny.contains(&format!("mcp__baaz__{tool}").as_str()), "{tool} stays allowed");
            }
            assert_eq!(settings["disableSkillShellExecution"], true);
            // No allow list rides along: the layer only ever narrows.
            assert!(settings["permissions"].get("allow").is_none());
            assert!(carries_read_only_settings(&read_only));
            // Still `dontAsk`, so whatever no rule names is refused unasked.
            let at = read_only.argv.iter().position(|arg| arg == "--permission-mode").expect("flag");
            assert_eq!(read_only.argv[at + 1], "dontAsk");
            for mode in [PermissionMode::AllowAll, PermissionMode::OnRequest, PermissionMode::PromptUnmatched] {
                let other = with_permission_mode(launch.clone(), Some(mode));
                assert_eq!(settings_of(&other), None, "{mode:?} carries no deny layer");
                assert!(!carries_read_only_settings(&other));
            }
        }
    }

    #[test]
    fn leaving_read_only_strips_the_layer_and_reentering_adds_it_once() {
        let launch = with_permission_mode(
            argv_for_resume("sess-9", None, None, None),
            Some(PermissionMode::DenyUnmatched),
        );
        let left = with_permission_mode(launch, Some(PermissionMode::PromptUnmatched));
        assert!(!left.argv.iter().any(|arg| arg == "--settings"), "{:?}", left.argv);
        let back = with_permission_mode(left, Some(PermissionMode::DenyUnmatched));
        let back = with_permission_mode(back, Some(PermissionMode::DenyUnmatched));
        assert_eq!(back.argv.iter().filter(|arg| *arg == "--settings").count(), 1, "{:?}", back.argv);
    }

    #[test]
    fn the_live_clear_nulls_exactly_the_keys_the_layer_sets() {
        let layer = read_only_settings();
        let cleared = read_only_settings_cleared();
        let keys = |value: &serde_json::Value| {
            let mut keys: Vec<String> = value.as_object().expect("object").keys().cloned().collect();
            keys.sort();
            keys
        };
        assert_eq!(keys(&layer), keys(&cleared));
        assert!(cleared.as_object().expect("object").values().all(serde_json::Value::is_null));
    }

    #[test]
    fn a_new_mode_replaces_the_old_flag() {
        let launch = with_permission_mode(
            argv_for_resume("sess-9", None, None, None),
            Some(PermissionMode::OnRequest),
        );
        let launch = with_permission_mode(launch, Some(PermissionMode::AllowAll));
        let flags: Vec<_> = launch.argv.iter().filter(|arg| *arg == "--permission-mode").collect();
        assert_eq!(flags.len(), 1, "one flag: {:?}", launch.argv);
        assert!(launch.argv.contains(&"bypassPermissions".to_owned()));
        assert!(!launch.argv.contains(&"auto".to_owned()));
    }

    #[test]
    fn reported_spellings_fold_to_the_closest_mode() {
        // The CLI reports the asking mode as `default` even when launched
        // with `manual` (probed live 2026-10-16).
        assert_eq!(permission_mode_from_claude("default"), Some(PermissionMode::PromptUnmatched));
        assert_eq!(permission_mode_from_claude("acceptEdits"), Some(PermissionMode::OnRequest));
        // Plan asks rather than refuses, and matches allow rules first: it
        // folds to nothing so the chip never claims Read-only for it.
        assert_eq!(permission_mode_from_claude("plan"), None);
        assert_eq!(permission_mode_from_claude("someday"), None, "unknown folds to nothing");
    }
}
