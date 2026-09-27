//! The terminal relay's Muse leg (route 1, session MCP).
//!
//! When `initialize` granted `sessionMcp` (muse ≥ 1.3), `session/start`
//! and `session/resume` carry `config.mcpServers.baaz`: the `mcp-bridge`
//! relay beside the app, so the agent's `terminal_*` tools reach the
//! socket service from [`crate`] docs (`docs/14-terminal.md` D46). When the
//! grant is absent there is no route — the session opens exactly as it
//! always did, with no `config` key at all.
//!
//! The relay's address (bridge binary, socket path) is adapter state, set
//! by the host with [`MuseAdapter::set_terminal_relay`] before the session
//! commands run. The session id the bridge answers for is the command's
//! own: a fresh open mints it client-side (`request_id` doubles as the new
//! session's exact identity, which the host registers with the service
//! before sending), a resume names the stored session.

use std::path::PathBuf;

use muse_client::schema::{SessionConfig, SessionMcpServerConfig, SessionMcpServerMode};

/// The MCP server name the bridge registers under on every route.
pub const SERVER_NAME: &str = "baaz";

/// Where the relay lives: the bridge binary to spawn and the socket to
/// point it at. Set once per connection by the host; read on every session
/// open and resume.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRelay {
    /// Absolute path of the `mcp-bridge` binary beside the running baaz
    /// binary (or inside the app bundle next to it).
    pub bridge: PathBuf,
    /// `<support_dir>/run/terminal-<pid>.sock`: what the bridge's
    /// `--socket` names.
    pub socket: PathBuf,
}

impl TerminalRelay {
    /// The bridge's argv tail for `session_id`: `--terminal --socket
    /// <sock> --session <id>`, exactly what `mcp-bridge --terminal`
    /// parses.
    pub fn bridge_args(&self, session_id: &str) -> Vec<String> {
        vec![
            "--terminal".to_owned(),
            "--socket".to_owned(),
            self.socket.to_string_lossy().into_owned(),
            "--session".to_owned(),
            session_id.to_owned(),
        ]
    }

    /// The whole `config` object for `session/start` and `session/resume`:
    /// just the bridge, under [`SERVER_NAME`]. `mode` is `required`: a
    /// bridge that cannot start fails the open loudly rather than leaving
    /// a session whose terminal tools silently never arrive.
    pub fn session_config(&self, session_id: &str) -> SessionConfig {
        SessionConfig {
            mcp_servers: Some(
                [(
                    SERVER_NAME.to_owned(),
                    SessionMcpServerConfig::Stdio {
                        args: Some(self.bridge_args(session_id)),
                        command: self.bridge.to_string_lossy().into_owned(),
                        env: None,
                        framing: None,
                        mode: Some(SessionMcpServerMode::Required),
                    },
                )]
                .into_iter()
                .collect(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relay() -> TerminalRelay {
        TerminalRelay {
            bridge: PathBuf::from("/Applications/Baaz.app/Contents/MacOS/mcp-bridge"),
            socket: PathBuf::from("/tmp/baaz/run/terminal-1.sock"),
        }
    }

    #[test]
    fn the_config_names_the_bridge_socket_and_session() {
        let config = relay().session_config("s-1");
        let servers = config.mcp_servers.expect("one server");
        assert_eq!(servers.len(), 1);
        match &servers[SERVER_NAME] {
            SessionMcpServerConfig::Stdio { command, args, mode, .. } => {
                assert_eq!(command, "/Applications/Baaz.app/Contents/MacOS/mcp-bridge");
                assert_eq!(
                    args.clone().expect("args"),
                    vec![
                        "--terminal",
                        "--socket",
                        "/tmp/baaz/run/terminal-1.sock",
                        "--session",
                        "s-1"
                    ]
                );
                assert_eq!(*mode, Some(SessionMcpServerMode::Required));
            }
            other => panic!("the bridge is a stdio server, not {other:?}"),
        }
    }
}
