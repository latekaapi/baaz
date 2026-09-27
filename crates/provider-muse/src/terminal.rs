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

use muse_client::schema::{
    SessionConfig, SessionMcpServerConfig, SessionMcpServerMode, SessionResumeParams,
    SessionStartParams,
};

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
    ///
    /// The schema spelling of the host's neutral bridge spec (baaz's
    /// `terminal::relay::bridge_spec`): same command, same args, same
    /// required route, in muse's `config.mcpServers`.
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

/// The bridge `config` from the host's neutral spec fields: `command` is
/// the bridge binary, `args` its argv tail for the session (which names
/// the session id). The host builds those with its
/// `terminal::relay::bridge_spec`; every muse-schema spelling stays here,
/// behind the seam.
pub fn session_config_for(command: &str, args: &[String]) -> SessionConfig {
    SessionConfig {
        mcp_servers: Some(
            [(
                SERVER_NAME.to_owned(),
                SessionMcpServerConfig::Stdio {
                    args: Some(args.to_vec()),
                    command: command.to_owned(),
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

/// Attach the terminal route to a `session/start`'s params: the
/// client-minted `session_id` (so the id the bridge carries is known before
/// the start runs and can be registered first) plus the bridge config.
/// Callers register `session_id` with the service before sending.
pub fn attach_start(
    params: &mut SessionStartParams,
    session_id: &str,
    command: &str,
    args: &[String],
) {
    params.session_id = Some(session_id.to_owned());
    params.config = Some(session_config_for(command, args));
}

/// Attach the terminal route to a `session/resume`'s params: the session
/// already exists, so only the bridge config rides along. `session_id`
/// must be the resume's own — a resume never renames the session, and the
/// bridge args name that same id.
pub fn attach_resume(
    params: &mut SessionResumeParams,
    session_id: &str,
    command: &str,
    args: &[String],
) {
    debug_assert_eq!(params.session_id, session_id, "resume never renames the session");
    params.config = Some(session_config_for(command, args));
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

    #[test]
    fn start_takes_the_minted_session_id() {
        let mut params = muse_client::schema::SessionStartParams {
            command_id: "cmd-1".into(),
            ..Default::default()
        };
        attach_start(
            &mut params,
            "s-minted",
            "/tmp/baaz/bin/mcp-bridge",
            &[
                "--terminal".to_owned(),
                "--socket".to_owned(),
                "/tmp/baaz/run/terminal-1.sock".to_owned(),
                "--session".to_owned(),
                "s-minted".to_owned(),
            ],
        );
        assert_eq!(params.session_id.as_deref(), Some("s-minted"));
        assert!(params.config.is_some(), "the start carries the bridge");
    }

    #[test]
    fn resume_keeps_its_session_and_gains_the_bridge() {
        let mut params = muse_client::schema::SessionResumeParams {
            command_id: "cmd-2".into(),
            session_id: "s-known".into(),
            ..Default::default()
        };
        attach_resume(
            &mut params,
            "s-known",
            "/tmp/baaz/bin/mcp-bridge",
            &[
                "--terminal".to_owned(),
                "--socket".to_owned(),
                "/tmp/baaz/run/terminal-1.sock".to_owned(),
                "--session".to_owned(),
                "s-known".to_owned(),
            ],
        );
        assert_eq!(params.session_id, "s-known", "resume never renames the session");
        let config = params.config.expect("the resume carries the bridge");
        let args = match &config.mcp_servers.expect("one server")[SERVER_NAME] {
            SessionMcpServerConfig::Stdio { args, .. } => args.clone(),
            other => panic!("the bridge is a stdio server, not {other:?}"),
        };
        assert!(
            args.expect("args").contains(&"s-known".to_owned()),
            "the bridge answers for the resumed session"
        );
    }
}
