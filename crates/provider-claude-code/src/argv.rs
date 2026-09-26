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
    /// The effort flag, when one was requested.
    pub effort: Option<String>,
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
    }
}

/// One text-only user turn as NDJSON for the child's stdin (doc §1
/// input frame). Text parts join in order; use [`user_content_line`] when
/// the turn carries images.
pub fn user_input_line(text: &str) -> String {
    user_content_line(text, &[])
}

/// One user turn carrying text plus image parts, as NDJSON for the
/// child's stdin. Probed live 2026-09-26 (`fixtures/claude-code/image.jsonl`):
/// an `image` content part with a base64 `source` is accepted and echoed
/// back by `--replay-user-messages` with its media type and length.
pub fn user_content_line(text: &str, images: &[ImageInput]) -> String {
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
    serde_json::json!({
        "type": "user",
        "message": {"role": "user", "content": content},
    })
    .to_string()
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
    fn mcp_config_always_brings_strict() {
        let launch = argv_for_open("req-1", None, None, Some("/tmp/mcp.json"), None);
        let argv = launch.argv;
        assert!(argv.contains(&"--mcp-config".to_owned()));
        assert!(argv.contains(&"--strict-mcp-config".to_owned()));
        let plain = argv_for_open("req-1", None, None, None, None);
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
        }]);
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
}
