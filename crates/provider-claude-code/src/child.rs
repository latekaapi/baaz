//! The child process lane: one long-lived child per session.
//!
//! The child is `claude` with the [`crate::argv`] flags; user turns go to
//! its stdin as NDJSON and frames come from its stdout through
//! [`pump_reader`](crate::fold::pump_reader) — the SAME function the fixture
//! tests use, so the live path and the tested path cannot drift apart.
//!
//! Nothing here runs in tests: spawning `claude` spends the owner's money
//! and every test stays offline against the checked-in fixtures.

use std::io::{BufReader, BufRead as _, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use provider::ProviderEvent;

use crate::argv::SessionLaunch;
use crate::controls::ControlHub;

/// A running session child: stdin for turns, a pump thread for stdout.
pub struct RunningChild {
    child: Child,
    stdin: ChildStdin,
    pump: Option<std::thread::JoinHandle<()>>,
}

impl RunningChild {
    /// Spawn `program` with `launch.argv` in `launch.cwd`, holding stdin
    /// open (a closed stdin is not the same as no stdin — doc §1) and
    /// pumping stdout through the shared ingest path
    /// ([`ControlHub::ingest_line`], the same function the scripted-frame
    /// tests drive) into `events`. The fold is shared with the adapter
    /// (for `ReadAccount`) and locked one line at a time, never for the
    /// whole stream.
    /// Blocking: run it on the background executor.
    /// Spawn a pre-consent legacy child: the owner's real home, i.e. no
    /// `CLAUDE_CONFIG_DIR` override at all. The inherited desktop-agent
    /// env is still scrubbed ([`provider::child_env`]); only the home
    /// stays the owner's, so a `--resume` finds the transcript where it
    /// still lives. Blocking: run it on the background executor.
    pub fn spawn_legacy(
        program: &str,
        launch: &SessionLaunch,
        hub: &std::sync::Arc<ControlHub>,
    ) -> std::io::Result<Self> {
        Self::spawn_inner(program, launch, hub, None)
    }

    /// Spawn a Baaz-homed child: `CLAUDE_CONFIG_DIR` names `claude_home`
    /// (see [`spawn_legacy`](Self::spawn_legacy) for the pre-consent
    /// resume without the override). Blocking: run it on the background
    /// executor.
    pub fn spawn(
        program: &str,
        launch: &SessionLaunch,
        hub: &std::sync::Arc<ControlHub>,
        claude_home: &std::path::Path,
    ) -> std::io::Result<Self> {
        Self::spawn_inner(program, launch, hub, Some(claude_home))
    }

    fn spawn_inner(
        program: &str,
        launch: &SessionLaunch,
        hub: &std::sync::Arc<ControlHub>,
        claude_home: Option<&std::path::Path>,
    ) -> std::io::Result<Self> {
        // The child's `PATH` is the login-shell `PATH` with the program's
        // own directory first, so a Dock launch still runs a home install
        // and its `env`-shebang neighbours. The inherited desktop-agent
        // env is scrubbed ([`provider::child_env`]) and, unless this is a
        // pre-consent legacy resume (`None`: no override, the owner's
        // real home), the child writes to the Baaz-owned home
        // ([`crate::home`]), never the owner's `~/.claude` — that is
        // what keeps Baaz sessions out of the desktop app's listing. No
        // filesystem work happens here: the adapter re-ensures the
        // absent-only owner links before every Baaz-homed spawn, so
        // late-created owner dirs are linked without a restart.
        let mut command = Command::new(program);
        command
            .args(&launch.argv)
            .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        provider::child_env::scrub_command(&mut command);
        if let Some(home) = claude_home {
            for (key, value) in crate::home::child_env(home) {
                command.env(key, value);
            }
        }
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stdin = child.stdin.take().expect("stdin piped");
        let hub = std::sync::Arc::clone(hub);
        let pump = std::thread::Builder::new()
            .name("provider-claude-code-pump".into())
            .spawn(move || {
                for line in BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    hub.ingest_line(&line);
                }
                let _ = hub.tx.send(ProviderEvent::ConnectionLost {
                    reason: "the agent process exited".into(),
                });
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { child, stdin, pump: Some(pump) })
    }

    /// Write one NDJSON turn to the child's stdin.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    /// Hang up. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        let _ = self.stdin.flush();
        // Detach, never join: the pump ends when stdout closes, which is
        // when the child exits after stdin closes.
        self.pump.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}
