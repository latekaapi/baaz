//! The child process lane: one long-lived child per session.
//!
//! The child is `claude` with the [`crate::argv`] flags; user turns go to
//! its stdin as NDJSON and frames come from its stdout through
//! [`pump_reader`](crate::fold::pump_reader) — the SAME function the fixture
//! tests use, so the live path and the tested path cannot drift apart.
//!
//! Nothing here runs in tests: spawning `claude` spends the owner's money
//! and every test stays offline against the checked-in fixtures.

use std::io::{Read, Write};
use std::process::Command;
use std::sync::Arc;

use provider::runtime::pump::{spawn_pump as spawn_line_pump, Flow, PumpEnd};
use provider::runtime::{ChildProcess, Spawned, StdinWriter, TERM_GRACE};

use crate::argv::SessionLaunch;
use crate::controls::ControlHub;

/// The child's stdin writer ([`StdinWriter`], shared by every adapter) for
/// the child of `epoch`: a hub control line whose write fails is refused
/// through [`ControlHub::hub_write_failed`] at once, not at its timeout.
pub(crate) fn stdin_writer(
    stdin: impl Write + Send + 'static,
    hub: &Arc<ControlHub>,
    epoch: u64,
) -> std::io::Result<StdinWriter> {
    let hub = Arc::clone(hub);
    StdinWriter::spawn(stdin, "provider-claude-code-stdin", move |request_id, reason| {
        hub.hub_write_failed(epoch, request_id, reason);
    })
}

/// Whether `BAAZ_TRACE=1` is set: the child's argv and stderr are echoed
/// to Baaz's own stderr. Read once.
pub(crate) fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BAAZ_TRACE").is_ok_and(|v| v == "1"))
}

/// How many of the child's last stderr lines an exit card quotes (the
/// adapter keeps [`provider::STDERR_TAIL_LINES`] for the failure view).
const EXIT_CARD_LINES: usize = 6;

/// The exit card's words from a settled stderr tail: the last
/// [`EXIT_CARD_LINES`] lines joined into one, `None` when the child said
/// nothing there.
fn exit_card_text(lines: &[String]) -> Option<String> {
    let start = lines.len().saturating_sub(EXIT_CARD_LINES);
    let said: Vec<&str> = lines[start..].iter().map(|line| line.trim()).collect();
    (!said.is_empty()).then(|| said.join(" "))
}

/// How every end-of-stream reason opens: the app's failure sentence
/// already says the process exited, so it strips this lead (baaz-core
/// `provider_status::failure_detail`) rather than naming the CLI itself.
pub const EXITED: &str = "the Claude Code process exited";

/// The end-of-stream reason for a child whose stderr said `stderr`: the
/// CLI's own words lead, and the resume hint follows.
pub(crate) fn exit_reason(stderr: Option<&str>) -> String {
    match stderr {
        Some(said) => format!("{EXITED}: {said}; send again to resume the session"),
        None => CHILD_EXITED_REASON.to_owned(),
    }
}

/// How long the pump waits, after stdout closes, for the child's stderr to
/// close too before it cards the exit with whatever tail it has.
const STDERR_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// Pump one child's stdout into the hub, gated to its epoch, until the
/// stream ends: the same per-line ingest the scripted-frame tests drive,
/// through the shared pump ([`provider::runtime::pump`]): lines read
/// lossily with a cap, each one guarded. The end-of-stream card quotes
/// `stderr`'s tail when the child left one.
pub(crate) fn spawn_pump(
    stdout: impl Read + Send + 'static,
    hub: &Arc<ControlHub>,
    epoch: u64,
    stderr: Option<provider::StderrTail>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let (pump_hub, end_hub) = (Arc::clone(hub), Arc::clone(hub));
    spawn_line_pump(
        "provider-claude-code-pump",
        stdout,
        move |line| {
            match line.note() {
                Some(note) => eprintln!("[Claude Code] {note}"),
                None => pump_hub.ingest_line_of(epoch, &line.text),
            }
            Flow::Continue
        },
        move |end| {
            // A turn the child never finished ends here, carded, and the
            // lane learns the connection is gone — unless Baaz hung up on
            // purpose (`shutdown` retired the epoch). A fold that panicked
            // ends the session like a lost child, with the poison it left
            // cleared (docs/81 B PM-3).
            let reason = match end {
                PumpEnd::Panicked(message) => {
                    end_hub.recover_from_panic();
                    provider::guard::panic_reason("Claude Code", &message)
                }
                PumpEnd::Closed => {
                    let said = stderr.and_then(|tail| exit_card_text(&tail.settled(STDERR_SETTLE)));
                    exit_reason(said.as_deref())
                }
            };
            end_hub.ingest_end_of_stream(epoch, &reason);
        },
    )
}

/// A running session child: stdin for turns, a pump thread for stdout.
/// The child leads its own process group ([`ChildProcess`]), so a hang-up
/// ends the tools and MCP servers it started too.
pub struct RunningChild {
    child: ChildProcess,
    /// The single writer of the child's stdin, shared with the hub
    /// ([`ControlHub::attach_stdin`]), which queues the Read-only MCP
    /// refresh from the pump.
    stdin: StdinWriter,
    /// The hub and this child's epoch on it: a deliberate hang-up retires
    /// the epoch so the pump's end of stream reads as expected, not lost.
    hub: std::sync::Arc<ControlHub>,
    epoch: u64,
}

/// What the end-of-stream card says when the current child's stdout
/// closes on its own: the process exited (or closed its output) without
/// Baaz hanging up, so any turn it was running stopped there.
pub const CHILD_EXITED_REASON: &str =
    "the Claude Code process exited before the turn finished; send again to resume the session";

impl RunningChild {
    /// Spawn `program` with `launch.argv` in `launch.cwd`, holding stdin
    /// open (a closed stdin is not the same as no stdin — doc §1) and
    /// pumping stdout through the shared ingest path
    /// ([`ControlHub::ingest_line_of`], gated to this child's epoch, over the
    /// same [`ControlHub::ingest_line`] the scripted-frame
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
            .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)));
        provider::child_env::scrub_command(&mut command);
        if let Some(home) = claude_home {
            for (key, value) in crate::home::child_env(home) {
                command.env(key, value);
            }
        }
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &launch.env {
            command.env(key, value);
        }
        if trace_enabled() {
            eprintln!(
                "baaz: claude spawn: {program} {} (cwd {:?}, CLAUDE_CONFIG_DIR {})",
                launch.argv.join(" "),
                launch.cwd,
                claude_home.map_or_else(|| "inherited".to_owned(), |home| home.display().to_string())
            );
        }
        // A new session and process group, in the pid ledger while it runs.
        let Spawned { process: child, stdin, stdout, stderr } = ChildProcess::spawn(&command)?;
        // The adapter's one ring (on the hub): it outlives this child, so
        // a child that dies on its first breath still leaves its words for
        // the failure view.
        hub.stderr.drain(stderr, "provider-claude-code-stderr", Some("claude"))?;
        let stderr = hub.stderr.clone();
        let epoch = hub.begin_child();
        let stdin = stdin_writer(stdin, hub, epoch)?;
        hub.attach_stdin(epoch, stdin.clone());
        // Detached, never joined: the pump ends when stdout closes.
        spawn_pump(stdout, hub, epoch, Some(stderr))?;
        Ok(Self { child, stdin, hub: std::sync::Arc::clone(hub), epoch })
    }

    /// Write one NDJSON turn to the child's stdin, through its writer
    /// thread, waiting for the write.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_line(line)
    }

    /// Whether the child process has exited (observed without reaping:
    /// its pid stays reserved for the hang-up's group signal).
    pub fn has_exited(&mut self) -> bool {
        self.child.has_exited()
    }

    /// Hang up: the whole process group, SIGTERM then SIGKILL after
    /// [`TERM_GRACE`]. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        // Deliberate: the pump's end of stream that follows is expected.
        self.hub.retire_child(self.epoch);
        // The hub's handle keeps the writer thread (and the pipe) alive:
        // drop it, so the writer ends once its queue drains.
        self.hub.detach_stdin(self.epoch);
        self.child.terminate(TERM_GRACE);
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A scripted child stdin for tests: records each whole line, runs a hook
/// on it (the pump landing a line "now"), and can block or fail writes.
#[cfg(test)]
pub(crate) mod testing {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    /// What a hook sees: each whole line the writer delivered.
    pub(crate) type LineHook = Box<dyn FnMut(&str) + Send>;

    /// A child stdin that keeps what it is given.
    pub(crate) struct ScriptedStdin {
        pending: Vec<u8>,
        /// Every whole line written, in order.
        pub(crate) lines: Arc<Mutex<Vec<String>>>,
        /// While set, every write fails (a broken pipe).
        pub(crate) fail: Arc<AtomicBool>,
        /// While unset, a write spins until it is set (a child not
        /// reading its stdin).
        pub(crate) open: Arc<AtomicBool>,
        hook: Option<LineHook>,
    }

    impl ScriptedStdin {
        pub(crate) fn new(hook: Option<LineHook>) -> Self {
            Self {
                pending: Vec::new(),
                lines: Arc::new(Mutex::new(Vec::new())),
                fail: Arc::new(AtomicBool::new(false)),
                open: Arc::new(AtomicBool::new(true)),
                hook,
            }
        }
    }

    impl std::io::Write for ScriptedStdin {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            while !self.open.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            if self.fail.load(Ordering::SeqCst) {
                return Err(std::io::Error::new(std::io::ErrorKind::BrokenPipe, "broken pipe"));
            }
            self.pending.extend_from_slice(buf);
            while let Some(at) = self.pending.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8_lossy(&self.pending[..at]).into_owned();
                self.pending.drain(..=at);
                self.lines.lock().expect("lines").push(line.clone());
                if let Some(hook) = self.hook.as_mut() {
                    hook(&line);
                }
            }
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Wait (bounded) until `done` holds.
    pub(crate) fn wait_until(mut done: impl FnMut() -> bool) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        done()
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{wait_until, ScriptedStdin};
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    /// Every end-of-stream reason opens with [`EXITED`], the lead the
    /// app's failure detail strips.
    #[test]
    fn every_exit_reason_opens_with_the_exited_lead() {
        assert!(CHILD_EXITED_REASON.starts_with(EXITED));
        assert!(exit_reason(Some("boom")).starts_with(EXITED));
        assert!(exit_reason(None).starts_with(EXITED));
    }

    /// A child's stdout that says when it has been read to the end.
    struct DrainedStdout {
        bytes: std::io::Cursor<Vec<u8>>,
        drained: Arc<AtomicBool>,
    }

    impl Read for DrainedStdout {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let read = self.bytes.read(buf)?;
            if read == 0 {
                self.drained.store(true, Ordering::SeqCst);
            }
            Ok(read)
        }
    }

    /// THE STDIN DEADLOCK TEST. A child that will not read its stdin until
    /// its stdout drains, sent a prompt too big for the pipe while its
    /// stdout carries an `mcp_status` answer that makes the pump write the
    /// Read-only MCP refresh: the pump only queues that write, reads the
    /// stdout to its end, the child then takes its stdin, and both the
    /// prompt and the refresh land, each whole. With one stdin mutex shared
    /// by the prompt and the pump, the pump blocked on the refresh and the
    /// stdout never drained.
    #[test]
    fn a_blocked_prompt_never_stops_the_pump_draining_stdout() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let hub = Arc::new(ControlHub::new(
            Arc::new(Mutex::new(crate::fold::ClaudeFold::new())),
            tx,
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(None)),
            Arc::new(Mutex::new(Some("sess-1".to_owned()))),
        ));
        let epoch = hub.begin_child();
        hub.note_launched_layer(true, false);
        let stdin = ScriptedStdin::new(None);
        let (lines, open) = (Arc::clone(&stdin.lines), Arc::clone(&stdin.open));
        open.store(false, Ordering::SeqCst);
        let writer = stdin_writer(stdin, &hub, epoch).expect("writer");
        hub.attach_stdin(epoch, writer.clone());
        hub.query_mcp_status();
        let query: serde_json::Value =
            serde_json::from_str(hub.mcp_sent.lock().expect("sent").last().expect("query")).expect("json");

        // The prompt blocks in its write: the child is not reading.
        let prompt = "x".repeat(1 << 20);
        let sender = {
            let writer = writer.clone();
            let prompt = prompt.clone();
            std::thread::spawn(move || writer.write_line(&prompt))
        };
        let drained = Arc::new(AtomicBool::new(false));
        let answer = serde_json::json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": query["request_id"], "response": {"mcpServers": [
                {"name": "probe", "status": "connected", "tools": [{"name": "poke", "annotations": {}}]},
            ]}},
        });
        let stdout = DrainedStdout {
            bytes: std::io::Cursor::new(format!("{answer}\n").into_bytes()),
            drained: Arc::clone(&drained),
        };
        let pump = spawn_pump(stdout, &hub, epoch, None).expect("pump");
        assert!(wait_until(|| drained.load(Ordering::SeqCst)), "the pump drained stdout while the prompt was blocked");
        // Only now does the child read its stdin.
        open.store(true, Ordering::SeqCst);
        sender.join().expect("sender").expect("the prompt is written");
        pump.join().expect("pump ends");
        assert!(wait_until(|| lines.lock().expect("lines").len() == 3), "{:?}", lines.lock().expect("lines").len());
        let lines = lines.lock().expect("lines");
        assert!(lines[0].contains("mcp_status"));
        assert!(lines.contains(&prompt), "the prompt landed whole");
        assert!(lines.iter().any(|line| line.contains("mcp__probe__poke")), "the refresh landed");
    }
}
