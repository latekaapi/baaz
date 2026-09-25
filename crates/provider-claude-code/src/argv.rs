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
//! `--strict-mcp-config` rides along whenever `--mcp-config` does (doc §4):
//! without it the session inherits the operator's unrelated connectors.
//! User turns go to stdin as NDJSON ([`user_input_line`]); the prompt is
//! never passed positionally (a variadic flag would eat it — doc §1).

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

/// Why a Claude Code session offers no reasoning-effort picker: the launch
/// argv carries `--model` but no effort flag, no per-turn channel was ever
/// probed over stream-json stdin, and no captured transcript
/// (`fixtures/claude-code/*.jsonl`) shows an effort surface — the one
/// `effort` string on record is a slash-command name in the init payload,
/// not a level list. Guessing a menu from that would invent support, so
/// the picker renders this reason instead.
pub fn reasoning_effort_unavailable_reason() -> &'static str {
    "Claude Code sessions expose no reasoning-effort control: the launch argv carries \
     `--model` but no effort flag, and no captured transcript shows an effort surface"
}

fn with_mcp(mut argv: Vec<String>, mcp_config: Option<&str>) -> Vec<String> {
    if let Some(path) = mcp_config {
        argv.push("--mcp-config".into());
        argv.push(path.to_owned());
        // Doc §4: without this the session sees the operator's connectors.
        argv.push("--strict-mcp-config".into());
    }
    argv
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
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--session-id".into());
    argv.push(request_id.to_owned());
    argv = with_model(argv, model);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: workspace.map(str::to_owned),
        session_id: request_id.to_owned(),
        model: model.map(str::to_owned),
    }
}

/// Re-attach to a stored session. `--resume` keeps the same id (doc §3).
pub fn argv_for_resume(
    session_id: &str,
    model: Option<&str>,
    mcp_config: Option<&str>,
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--resume".into());
    argv.push(session_id.to_owned());
    argv = with_model(argv, model);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: None,
        session_id: session_id.to_owned(),
        model: model.map(str::to_owned),
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
) -> SessionLaunch {
    let mut argv = base_argv();
    argv.push("--resume".into());
    argv.push(session_id.to_owned());
    argv.push("--fork-session".into());
    argv.push("--session-id".into());
    argv.push(request_id.to_owned());
    argv = with_model(argv, model);
    argv = with_mcp(argv, mcp_config);
    SessionLaunch {
        argv,
        cwd: None,
        session_id: request_id.to_owned(),
        model: model.map(str::to_owned),
    }
}

/// One user turn as NDJSON for the child's stdin (doc §1 input frame).
/// Text parts join in order; images have no probed input shape and are
/// refused by dispatch before they reach here.
pub fn user_input_line(text: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": [{"type": "text", "text": text}],
        },
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_chooses_the_session_id() {
        let launch = argv_for_open("req-1", Some("/work"), Some("haiku"), None);
        assert!(launch.argv.contains(&"--session-id".to_owned()));
        assert!(launch.argv.contains(&"req-1".to_owned()));
        assert!(!launch.argv.iter().any(|arg| arg == "--resume"));
        assert_eq!(launch.session_id, "req-1");
        assert_eq!(launch.cwd.as_deref(), Some("/work"));
    }

    #[test]
    fn no_effort_flag_anywhere_on_the_lane() {
        // The evidence behind `reasoning_effort_unavailable_reason`: every
        // launch shape carries `--model` and none carries an effort flag,
        // so Baaz must not offer effort levels it cannot send.
        for argv in [
            argv_for_open("req-1", Some("/work"), Some("haiku"), None).argv,
            argv_for_resume("sess-9", None, None).argv,
            argv_for_fork("branch-2", "sess-9", None, None).argv,
            base_argv(),
        ] {
            assert!(argv.contains(&"--model".to_owned()) || !argv.contains(&"haiku".to_owned()));
            assert!(
                !argv.iter().any(|arg| arg.contains("effort")),
                "no effort flag on this lane: {argv:?}"
            );
        }
        assert!(!reasoning_effort_unavailable_reason().is_empty());
    }

    #[test]
    fn resume_keeps_the_id_and_fork_adds_the_flag() {
        let resume = argv_for_resume("sess-9", None, None);
        assert!(resume.argv.contains(&"--resume".to_owned()));
        assert!(resume.argv.contains(&"sess-9".to_owned()));
        assert!(!resume.argv.iter().any(|arg| arg == "--fork-session"));

        let fork = argv_for_fork("branch-2", "sess-9", None, None);
        assert!(fork.argv.contains(&"--resume".to_owned()));
        assert!(fork.argv.contains(&"--fork-session".to_owned()));
    }

    #[test]
    fn mcp_config_always_brings_strict() {
        let launch = argv_for_open("req-1", None, None, Some("/tmp/mcp.json"));
        let argv = launch.argv;
        assert!(argv.contains(&"--mcp-config".to_owned()));
        assert!(argv.contains(&"--strict-mcp-config".to_owned()));
        let plain = argv_for_open("req-1", None, None, None);
        assert!(!plain.argv.iter().any(|arg| arg == "--strict-mcp-config"));
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
        assert!(argv_for_open("req-1", None, None, None).argv.contains(&"stdio".to_owned()));
        assert!(argv_for_resume("sess-9", None, None).argv.contains(&"stdio".to_owned()));
        assert!(argv_for_fork("branch-2", "sess-9", None, None).argv.contains(&"stdio".to_owned()));
    }

    #[test]
    fn user_turns_are_single_ndjson_lines() {
        let line = user_input_line("Say A1");
        assert!(!line.contains('\n'));
        let value: serde_json::Value = serde_json::from_str(&line).expect("one JSON object");
        assert_eq!(value.get("type").and_then(|t| t.as_str()), Some("user"));
    }
}
