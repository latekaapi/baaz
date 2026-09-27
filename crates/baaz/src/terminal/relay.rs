//! The terminal relay's per-session wiring (T2): how each provider's agent
//! reaches the socket service from [`super::service`].
//!
//! The app serves the `docs/14-terminal.md` §4 contract on
//! `<support_dir>/run/terminal-<pid>.sock`; every route below names the
//! same `mcp-bridge` relay beside it, so the tools, the tab rules and the
//! refusal shapes do not change with the route:
//!
//! * **Muse (route 1, session MCP):** `session/start` and `session/resume`
//!   carry `config.mcpServers.baaz`, a stdio server whose command is the
//!   bridge and whose args name the socket and the session. Only when
//!   `initialize` granted `sessionMcp` — otherwise no route.
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
use std::sync::atomic::{AtomicBool, Ordering};

/// The MCP server name every route registers the bridge under. One name
/// everywhere, so the tool surface (`terminal_*` — the bridge's own names,
/// unprefixed over session MCP) reads the same whatever opened the session.
pub const SERVER_NAME: &str = "baaz";

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

/// The `config.mcpServers` entry for Muse route 1: the bridge as a stdio
/// server on this session only. `mode` is `required`: a bridge that cannot
/// start fails the session open loudly rather than leaving a session whose
/// terminal tools silently never arrive.
pub fn muse_server_config(
    bridge: &Path,
    socket: &Path,
    session_id: &str,
) -> muse_client::schema::SessionMcpServerConfig {
    muse_client::schema::SessionMcpServerConfig::Stdio {
        args: Some(bridge_args(socket, session_id)),
        command: bridge.to_string_lossy().into_owned(),
        env: None,
        framing: None,
        mode: Some(muse_client::schema::SessionMcpServerMode::Required),
    }
}

/// The whole `config` object for a Muse `session/start` or
/// `session/resume`: just the bridge, under [`SERVER_NAME`].
pub fn muse_session_config(
    bridge: &Path,
    socket: &Path,
    session_id: &str,
) -> muse_client::schema::SessionConfig {
    muse_client::schema::SessionConfig {
        mcp_servers: Some(
            [(SERVER_NAME.to_owned(), muse_server_config(bridge, socket, session_id))]
                .into_iter()
                .collect(),
        ),
    }
}

/// Attach the terminal route to a `session/start`'s params: the
/// client-minted `session_id` (so the id the bridge carries is known before
/// the start runs and can be registered first) plus the bridge config.
/// Callers register `session_id` with the service before sending.
pub fn attach_muse_start(
    params: &mut muse_client::schema::SessionStartParams,
    session_id: &str,
    bridge: &Path,
    socket: &Path,
) {
    params.session_id = Some(session_id.to_owned());
    params.config = Some(muse_session_config(bridge, socket, session_id));
}

/// Attach the terminal route to a `session/resume`'s params: the session
/// already exists, so only the bridge config rides along.
pub fn attach_muse_resume(
    params: &mut muse_client::schema::SessionResumeParams,
    session_id: &str,
    bridge: &Path,
    socket: &Path,
) {
    params.config = Some(muse_session_config(bridge, socket, session_id));
}

/// The Muse route's one refusal: `initialize` did not grant `sessionMcp`,
/// so there is no route — logged once per process, then silent, so every
/// later session open does not re-announce a settled fact.
pub fn note_muse_route_ungranted() {
    static LOGGED: AtomicBool = AtomicBool::new(false);
    if !LOGGED.swap(true, Ordering::Relaxed) {
        crate::baaz_log!("terminal: sessionMcp not granted; agent sessions run without terminal tools");
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
    fn muse_config_names_the_bridge_socket_and_session() {
        let config = muse_session_config(&bridge(), &socket(), "s-1");
        let servers = config.mcp_servers.expect("one server");
        assert_eq!(servers.len(), 1);
        match &servers[SERVER_NAME] {
            muse_client::schema::SessionMcpServerConfig::Stdio { command, args, mode, .. } => {
                assert_eq!(command, &bridge().to_string_lossy());
                let args = args.clone().expect("args");
                assert_eq!(
                    args,
                    vec!["--terminal", "--socket", "/tmp/baaz/run/terminal-1.sock", "--session", "s-1"]
                );
                assert_eq!(*mode, Some(muse_client::schema::SessionMcpServerMode::Required));
            }
            other => panic!("the bridge is a stdio server, not {other:?}"),
        }
    }

    #[test]
    fn muse_start_takes_the_minted_session_id() {
        let mut params = muse_client::schema::SessionStartParams {
            command_id: "cmd-1".into(),
            ..Default::default()
        };
        attach_muse_start(&mut params, "s-minted", &bridge(), &socket());
        assert_eq!(params.session_id.as_deref(), Some("s-minted"));
        assert!(params.config.is_some(), "the start carries the bridge");
    }

    #[test]
    fn muse_resume_keeps_its_session_and_gains_the_bridge() {
        let mut params = muse_client::schema::SessionResumeParams {
            command_id: "cmd-2".into(),
            session_id: "s-known".into(),
            ..Default::default()
        };
        attach_muse_resume(&mut params, "s-known", &bridge(), &socket());
        assert_eq!(params.session_id, "s-known", "resume never renames the session");
        let config = params.config.expect("the resume carries the bridge");
        let args = match &config.mcp_servers.expect("one server")[SERVER_NAME] {
            muse_client::schema::SessionMcpServerConfig::Stdio { args, .. } => args.clone(),
            other => panic!("the bridge is a stdio server, not {other:?}"),
        };
        assert!(
            args.expect("args").contains(&"s-known".to_owned()),
            "the bridge answers for the resumed session"
        );
    }

    #[test]
    fn bridge_args_name_the_socket_and_session() {
        assert_eq!(
            bridge_args(&socket(), "s-9"),
            vec!["--terminal", "--socket", "/tmp/baaz/run/terminal-1.sock", "--session", "s-9"]
        );
    }

    #[test]
    fn the_claude_config_dir_sits_beside_the_socket_dir() {
        let support = PathBuf::from("/tmp/baaz-state");
        assert_eq!(claude_config_dir(&support), support.join("mcp"));
    }

    /// The order the Muse open path runs in, against a real socket: mint
    /// the id, register it, then build the start — so by the time the
    /// start (with the bridge) could run, the socket already serves the id
    /// the bridge will name. An unregistered id is refused, which is what
    /// would answer if the registration ever moved after the send.
    #[gpui::test]
    fn the_minted_session_is_registered_before_the_start(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::UnixStream;
        use std::time::Duration;

        let tmp = std::env::temp_dir().join(format!("baaz-relay-order-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let root = tmp.join("work");
        std::fs::create_dir_all(&root).expect("work root");
        let host = cx.new(|_| crate::terminal::TerminalHost::new());
        let service = crate::terminal::service::TerminalService::start_at(
            host,
            &tmp,
            std::process::id() % 40000 + 20000,
        );

        // The open path's three statements, in its order.
        let session_id = muse_client::new_command_id();
        service.register_session(&session_id, root.clone());
        let mut params = muse_client::schema::SessionStartParams {
            command_id: "cmd-1".into(),
            workspace_root: Some(root.to_string_lossy().into_owned()),
            ..Default::default()
        };
        attach_muse_start(&mut params, &session_id, &bridge(), service.socket_path());

        // The start carries the registered id, and the bridge names it.
        assert_eq!(params.session_id.as_deref(), Some(session_id.as_str()));
        let servers = params.config.expect("bridge config").mcp_servers.expect("one server");
        match &servers[SERVER_NAME] {
            muse_client::schema::SessionMcpServerConfig::Stdio { args, .. } => {
                assert!(args.clone().expect("args").contains(&session_id));
            }
            other => panic!("the bridge is a stdio server, not {other:?}"),
        }

        // And the socket serves that id right now — no start has run, so
        // only the prior registration can be answering.
        let request = serde_json::to_string(&serde_json::json!({
            "id": 1,
            "session": session_id,
            "tool": "terminal_list",
            "params": {},
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(service.socket_path()).expect("socket answers");
        // Short reads: the socket thread queues the job, but only the
        // drain below answers it — a long block here would starve the
        // very drain the reply waits on.
        stream.set_read_timeout(Some(Duration::from_millis(50))).expect("timeout");
        stream.write_all(request.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("flush");
        let mut reader = BufReader::new(stream);
        for _ in 0..400 {
            cx.update(|cx| service.drain(cx));
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => panic!("server closed the connection"),
                Ok(_) => {}
                Err(error)
                    if error.kind() == std::io::ErrorKind::TimedOut
                        || error.kind() == std::io::ErrorKind::WouldBlock =>
                {
                    continue;
                }
                Err(error) => panic!("socket reads: {error}"),
            }
            if line.trim().is_empty() {
                continue;
            }
            let reply: serde_json::Value = serde_json::from_str(&line).expect("reply parses");
            assert_eq!(
                reply.get("ok"),
                Some(&serde_json::Value::Bool(true)),
                "the registered session is served: {reply}"
            );
            let _ = std::fs::remove_dir_all(&tmp);
            return;
        }
        panic!("no reply after draining");
    }
}
