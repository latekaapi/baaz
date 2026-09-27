//! The terminal relay's per-session wiring (T2): how each provider's agent
//! reaches the socket service from [`super::service`].
//!
//! The app serves the `docs/14-terminal.md` §4 contract on
//! `<support_dir>/run/terminal-<pid>.sock`; every route below names the
//! same `mcp-bridge` relay beside it, so the tools, the tab rules and the
//! refusal shapes do not change with the route:
//!
//! * **Muse (route 1, session MCP):** `session/start` and `session/resume`
//!   carry `config.mcpServers.baaz`, built from the [`BridgeSpec`] below by
//!   `provider-muse` (which owns every muse-schema spelling). Only when
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

/// The provider-neutral bridge spec: what every route names, before any
/// provider turns it into its own schema. `command` is the bridge binary,
/// `args` its argv tail for `session_id`, and `required` means a bridge
/// that cannot start fails the session open loudly rather than leaving a
/// session whose terminal tools silently never arrive.
///
/// The muse-schema spelling of this spec lives in `provider-muse` (which
/// owns every wire spelling); the Claude Code JSON and the Codex `-c`
/// overrides live in their own provider crates. This module never names a
/// wire type, so the seam ratchet (`seam_ratchet.rs`) keeps counting this
/// file out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BridgeSpec {
    /// The bridge binary to spawn, under the `baaz` server name every
    /// route shares.
    pub command: String,
    /// `--terminal --socket <sock> --session <id>`, for this session.
    pub args: Vec<String>,
    /// The route is mandatory: fail the open, never run bridgeless.
    pub required: bool,
}

/// The bridge spec for one session: the bridge binary, its argv tail for
/// `session_id`, and the required route.
pub fn bridge_spec(bridge: &Path, socket: &Path, session_id: &str) -> BridgeSpec {
    BridgeSpec {
        command: bridge.to_string_lossy().into_owned(),
        args: bridge_args(socket, session_id),
        required: true,
    }
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
    fn bridge_spec_names_the_bridge_socket_and_session() {
        let spec = bridge_spec(&bridge(), &socket(), "s-1");
        assert_eq!(spec.command, bridge().to_string_lossy());
        assert_eq!(
            spec.args,
            vec!["--terminal", "--socket", "/tmp/baaz/run/terminal-1.sock", "--session", "s-1"]
        );
        assert!(spec.required, "a broken bridge fails the open loud");
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
    /// the id, register it, then build the bridge spec — so by the time
    /// the start (with the bridge) could run, the socket already serves
    /// the id the bridge will name. An unregistered id is refused, which
    /// is what would answer if the registration ever moved after the
    /// send. Neutral on purpose: the muse-schema spelling lives in
    /// `provider-muse`, so this file never names the wire crate.
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

        // The open path's three statements, in its order: mint the id
        // (what `muse_terminal_session` mints client-side), register it,
        // then name it in the bridge spec the start will carry.
        let session_id = format!("s-{}", std::process::id());
        service.register_session(&session_id, root.clone());
        let spec = bridge_spec(&bridge(), service.socket_path(), &session_id);

        // The spec carries the registered id.
        assert!(spec.args.contains(&session_id));
        assert!(spec.args.contains(&service.socket_path().to_string_lossy().into_owned()));

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
