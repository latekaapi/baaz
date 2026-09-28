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
    pub fn spawn(
        program: &str,
        launch: &SessionLaunch,
        hub: &std::sync::Arc<ControlHub>,
    ) -> std::io::Result<Self> {
        // The child's `PATH` is the login-shell `PATH` with the program's
        // own directory first, so a Dock launch still runs a home install
        // and its `env`-shebang neighbours. No other env var is changed.
        let mut command = Command::new(program);
        command
            .args(&launch.argv)
            .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
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
