//! The child process lane: one long-lived child per session.
//!
//! The child is `claude` with the [`crate::argv`] flags; user turns go to
//! its stdin as NDJSON and frames come from its stdout through
//! [`pump_reader`](crate::fold::pump_reader) — the SAME function the fixture
//! tests use, so the live path and the tested path cannot drift apart.
//!
//! Nothing here runs in tests: spawning `claude` spends the owner's money
//! and every test stays offline against the checked-in fixtures.

use std::io::{BufRead as _, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};

use crate::argv::SessionLaunch;
use crate::controls::ControlHub;

/// One line for the child's stdin: the writer thread writes it whole, then
/// answers `reply` (when the sender waits) or, for a hub-initiated control
/// line that failed, tells the hub which `request_id` can never be
/// answered.
struct WriteJob {
    line: String,
    reply: Option<Sender<std::io::Result<()>>>,
    request_id: Option<String>,
}

/// The child's stdin, owned by ONE writer thread fed by a channel. Nobody
/// else touches the pipe: the pump and the UI only enqueue, so a large
/// prompt blocked in `write_all` (the child not reading stdin until its
/// stdout drains) can never stall the pump that drains that stdout.
/// Lines go out whole and in enqueue order.
#[derive(Clone)]
pub(crate) struct StdinWriter {
    tx: Sender<WriteJob>,
}

impl StdinWriter {
    /// Hand `stdin` to a fresh writer thread. A failed write fails that
    /// line (its reply, or the hub's [`ControlHub::hub_write_failed`] for
    /// the child of `epoch`) and every line after it: a pipe that broke
    /// once stays broken.
    pub(crate) fn spawn(
        mut stdin: impl Write + Send + 'static,
        hub: &Arc<ControlHub>,
        epoch: u64,
    ) -> std::io::Result<Self> {
        let (tx, rx) = unbounded::<WriteJob>();
        let hub = Arc::clone(hub);
        std::thread::Builder::new()
            .name("provider-claude-code-stdin".into())
            .spawn(move || {
                let mut broken: Option<String> = None;
                for job in rx {
                    let written = match &broken {
                        Some(reason) => Err(std::io::Error::other(reason.clone())),
                        None => stdin
                            .write_all(job.line.as_bytes())
                            .and_then(|()| stdin.write_all(b"\n"))
                            .and_then(|()| stdin.flush()),
                    };
                    if let Err(error) = &written {
                        broken.get_or_insert_with(|| error.to_string());
                        if let Some(request_id) = &job.request_id {
                            hub.hub_write_failed(epoch, request_id, &error.to_string());
                        }
                    }
                    if let Some(reply) = job.reply {
                        let _ = reply.send(written);
                    }
                }
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(Self { tx })
    }

    /// Queue one line and return at once: the writer thread delivers it.
    /// `request_id` names the hub control request it carries, so a failed
    /// write refuses that request instead of leaving it to its timeout.
    pub(crate) fn enqueue(&self, line: String, request_id: Option<String>) -> std::io::Result<()> {
        self.tx
            .send(WriteJob { line, reply: None, request_id })
            .map_err(|_| std::io::Error::other("the child's stdin writer has stopped"))
    }

    /// Queue one line and hand back where its write result lands, without
    /// waiting for it: a caller holding a lock the pump needs waits only
    /// after releasing it.
    pub(crate) fn enqueue_with_reply(&self, line: String) -> Receiver<std::io::Result<()>> {
        let (reply, result) = bounded(1);
        if let Err(error) = self.tx.send(WriteJob { line, reply: Some(reply), request_id: None }) {
            if let Some(reply) = error.into_inner().reply {
                let _ = reply.send(Err(std::io::Error::other("the child's stdin writer has stopped")));
            }
        }
        result
    }

    /// Queue one line and wait for its write: the UI side, which may block
    /// (never the pump, which never waits on a write).
    pub(crate) fn write_line(&self, line: &str) -> std::io::Result<()> {
        self.enqueue_with_reply(line.to_owned())
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("the child's stdin writer has stopped")))
    }
}

/// Whether `BAAZ_TRACE=1` is set: the child's argv and stderr are echoed
/// to Baaz's own stderr. Read once.
fn trace_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("BAAZ_TRACE").is_ok_and(|v| v == "1"))
}

/// How many of the child's last stderr lines an exit card quotes.
const STDERR_TAIL_LINES: usize = 6;

/// The child's stderr, drained by its own thread into a short tail: the
/// CLI's own reason when it exits ("No conversation found…", a refused
/// flag), which the exit card quotes. Draining also keeps a chatty child
/// from blocking on a full stderr pipe.
#[derive(Clone, Default)]
pub(crate) struct StderrTail {
    lines: Arc<std::sync::Mutex<std::collections::VecDeque<String>>>,
    done: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

impl StderrTail {
    /// Drain `stderr` on a fresh thread until it closes.
    pub(crate) fn spawn(stderr: impl Read + Send + 'static) -> std::io::Result<Self> {
        let tail = Self::default();
        let sink = tail.clone();
        std::thread::Builder::new()
            .name("provider-claude-code-stderr".into())
            .spawn(move || {
                for line in BufReader::new(stderr).lines() {
                    let Ok(line) = line else { break };
                    sink.push(&line);
                }
                sink.close();
            })
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Ok(tail)
    }

    /// Keep one stderr line (blank ones are dropped).
    pub(crate) fn push(&self, line: &str) {
        if trace_enabled() {
            eprintln!("baaz: claude stderr: {line}");
        }
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        let mut lines = self.lines.lock().expect("stderr tail mutex");
        if lines.len() == STDERR_TAIL_LINES {
            lines.pop_front();
        }
        lines.push_back(line.to_owned());
    }

    /// The stream closed: nothing more will land.
    pub(crate) fn close(&self) {
        let (closed, ready) = &*self.done;
        *closed.lock().expect("stderr done mutex") = true;
        ready.notify_all();
    }

    /// The tail joined into one line, once stderr has closed or `wait` has
    /// passed (stdout and stderr close in either order); `None` when the
    /// child said nothing there.
    pub(crate) fn settled_text(&self, wait: std::time::Duration) -> Option<String> {
        let (closed, ready) = &*self.done;
        let guard = closed.lock().expect("stderr done mutex");
        drop(ready.wait_timeout_while(guard, wait, |closed| !*closed).expect("stderr done mutex"));
        let lines = self.lines.lock().expect("stderr tail mutex");
        (!lines.is_empty()).then(|| lines.iter().cloned().collect::<Vec<_>>().join(" "))
    }
}

/// The end-of-stream reason for a child whose stderr said `stderr`: the
/// CLI's own words lead, and the resume hint follows.
pub(crate) fn exit_reason(stderr: Option<&str>) -> String {
    match stderr {
        Some(said) => format!("the Claude Code process exited: {said}; send again to resume the session"),
        None => CHILD_EXITED_REASON.to_owned(),
    }
}

/// How long the pump waits, after stdout closes, for the child's stderr to
/// close too before it cards the exit with whatever tail it has.
const STDERR_SETTLE: std::time::Duration = std::time::Duration::from_millis(500);

/// Pump one child's stdout into the hub, gated to its epoch, until the
/// stream ends: the same per-line ingest the scripted-frame tests drive.
/// The end-of-stream card quotes `stderr`'s tail when the child left one.
pub(crate) fn spawn_pump(
    stdout: impl Read + Send + 'static,
    hub: &Arc<ControlHub>,
    epoch: u64,
    stderr: Option<StderrTail>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let pump_hub = Arc::clone(hub);
    std::thread::Builder::new()
        .name("provider-claude-code-pump".into())
        .spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                pump_hub.ingest_line_of(epoch, &line);
            }
            // A turn the child never finished ends here, carded, and
            // the lane learns the connection is gone — unless Baaz
            // hung up on purpose (`shutdown` retired the epoch).
            let said = stderr.and_then(|tail| tail.settled_text(STDERR_SETTLE));
            pump_hub.ingest_end_of_stream(epoch, &exit_reason(said.as_deref()));
        })
        .map_err(|error| std::io::Error::other(error.to_string()))
}

/// A running session child: stdin for turns, a pump thread for stdout.
pub struct RunningChild {
    child: Child,
    /// The single writer of the child's stdin, shared with the hub
    /// ([`ControlHub::attach_stdin`]), which queues the Read-only MCP
    /// refresh from the pump.
    stdin: StdinWriter,
    pump: Option<std::thread::JoinHandle<()>>,
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
            .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        provider::child_env::scrub_command(&mut command);
        if let Some(home) = claude_home {
            for (key, value) in crate::home::child_env(home) {
                command.env(key, value);
            }
        }
        if let Some(cwd) = &launch.cwd {
            command.current_dir(cwd);
        }
        if trace_enabled() {
            eprintln!("baaz: claude spawn: {program} {} (cwd {:?})", launch.argv.join(" "), launch.cwd);
        }
        let mut child = command.spawn()?;
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = StderrTail::spawn(child.stderr.take().expect("stderr piped"))?;
        let epoch = hub.begin_child();
        let stdin = StdinWriter::spawn(child.stdin.take().expect("stdin piped"), hub, epoch)?;
        hub.attach_stdin(epoch, stdin.clone());
        let pump = spawn_pump(stdout, hub, epoch, Some(stderr))?;
        Ok(Self { child, stdin, pump: Some(pump), hub: std::sync::Arc::clone(hub), epoch })
    }

    /// Write one NDJSON turn to the child's stdin, through its writer
    /// thread, waiting for the write.
    pub fn send_line(&mut self, line: &str) -> std::io::Result<()> {
        self.stdin.write_line(line)
    }

    /// Whether the child process has exited (reaped here when it has).
    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Hang up. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        // Deliberate: the pump's end of stream that follows is expected.
        self.hub.retire_child(self.epoch);
        // The hub's handle keeps the writer thread (and the pipe) alive:
        // drop it, so the writer ends once its queue drains.
        self.hub.detach_stdin(self.epoch);
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
        let writer = StdinWriter::spawn(stdin, &hub, epoch).expect("writer");
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
