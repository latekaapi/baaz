//! One ACP agent process on the shared runtime (docs/82 5.1): spawned in
//! its own session and process group, its stdin owned by one writer
//! thread, its stdout read line by line by a guarded pump, its requests in
//! flight in a [`RequestMap`] with deadlines.
//!
//! The pump does three things with what the agent writes:
//!
//! * an answer completes the request waiting on it — except a
//!   `session/prompt`'s, which ends a turn: the fold settles it there and
//!   then, so nothing ever blocks on a turn;
//! * a `session/update` folds into the transcript;
//! * a `session/request_permission` raises its card and its tap and is
//!   parked until the person decides; any other request (the file-system
//!   and terminal methods this client never offered) is refused with
//!   "method not found", never left hanging.
//!
//! When stdout ends without a hang-up the child died under the session
//! (docs/71 §2): every request in flight fails at once, every approval
//! settles cancelled, the running turn closes with a card naming the exit,
//! and only then is the connection reported lost.

use std::collections::HashMap;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crossbeam_channel::Sender;
use provider::runtime::pump::{spawn_pump, Flow, PumpEnd};
use provider::runtime::{ChildProcess, RequestMap, Spawned, StdinWriter};
use provider::ProviderEvent;
use serde_json::Value;

use crate::fold::{AcpFold, Ending};
use crate::wire::{self, Frame, StopReason};

/// What the pump says when the child died under the session, with the
/// child's last stderr line when it left one.
pub const CHILD_EXITED_REASON: &str = "the agent process exited";

/// Why a request in flight fails once the child's stdout has ended.
const EXITED_BEFORE_ANSWERING: &str = "the agent process exited before answering";

/// How long the pump waits, after stdout closes, for stderr to close too
/// before it names the exit with whatever tail it has.
const STDERR_SETTLE: Duration = Duration::from_millis(400);

/// A request that got no result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RpcError {
    /// The agent's JSON-RPC error code; `None` when the request never got
    /// an answer at all (the child is gone, the deadline passed).
    pub code: Option<i64>,
    /// What went wrong, in the agent's words when it answered.
    pub message: String,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// One permission request waiting on the person.
#[derive(Clone, Debug)]
pub struct Parked {
    /// The approval's id: the card's and the tap's.
    pub approval: String,
    /// The JSON-RPC id the answer echoes.
    pub rpc: Value,
    /// The tap's headline.
    pub headline: String,
}

/// What the pump and the adapter share, under one lock, one line at a
/// time.
pub struct Shared {
    /// The session's transcript fold.
    pub fold: AcpFold,
    /// The agent's own id for the session the child serves, once open.
    pub wire: Option<String>,
    /// Prompts in flight, by request id: each one's answer ends its turn.
    pub prompts: HashMap<u64, String>,
    /// Permission requests waiting on the person, oldest first.
    pub parked: Vec<Parked>,
    /// A revive's `session/load` is replaying what the transcript already
    /// shows: its updates are not folded again.
    pub muted: bool,
    /// A `session/new` is in flight: its answer names the session, and the
    /// agent may speak for it (its commands) before the open returns, so
    /// the pump adopts the id the moment the answer passes.
    pub opening: Option<Opening>,
}

/// What a `session/new` in flight is for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// A fresh session in this folder: a fresh fold.
    Fresh(String),
    /// A revived child's stand-in for a session it could not re-open: the
    /// transcript stays, only the agent's id moves.
    Rewire,
}

impl Shared {
    /// Nothing open yet.
    pub fn new() -> Self {
        Self { fold: AcpFold::new("", ""), wire: None, prompts: HashMap::new(), parked: Vec::new(), muted: false, opening: None }
    }

    /// A `session/new` answer passed: the session it names is the one the
    /// pump folds from here on.
    fn adopt(&mut self, result: &Value) {
        let Some(id) = result.get("sessionId").and_then(Value::as_str).filter(|id| !id.is_empty()) else { return };
        match self.opening.take() {
            Some(Opening::Fresh(cwd)) => {
                self.fold = AcpFold::new(id, &cwd);
                self.prompts.clear();
                self.parked.clear();
            }
            Some(Opening::Rewire) => {}
            None => return,
        }
        self.wire = Some(id.to_owned());
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

/// The shared state, locked; a panic elsewhere never wedges it (docs/81 B
/// PM-3).
pub fn lock(shared: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The fold's deltas so far, as one event for its session.
pub fn emit_deltas(shared: &mut Shared, events: &Sender<ProviderEvent>) {
    let deltas = shared.fold.take();
    if !deltas.is_empty() {
        let _ = events.send(ProviderEvent::Deltas { session_id: Some(shared.fold.session_id().to_owned()), deltas });
    }
}

/// The live turn ended (`ending`): every permission request still parked
/// is answered cancelled (the agent stopped waiting), the fold settles the
/// turn, and its deltas go out before its `TurnSettled`.
pub fn end_turn(shared: &mut Shared, ending: &Ending, events: &Sender<ProviderEvent>, write: &impl Fn(Value)) {
    for parked in shared.parked.drain(..) {
        write(wire::permission_cancelled(&parked.rpc));
    }
    let ended = shared.fold.finish(ending);
    emit_deltas(shared, events);
    if let Some((turn_id, outcome)) = ended {
        let _ = events.send(ProviderEvent::TurnSettled { session_id: shared.fold.session_id().to_owned(), turn_id, outcome });
    }
}

/// Requests in flight by request id.
type Requests = RequestMap<u64, Result<Value, RpcError>>;

/// A running agent.
pub struct RunningChild {
    child: ChildProcess,
    stdin: StdinWriter,
    next_id: AtomicU64,
    requests: Arc<Requests>,
    /// Set by [`Self::shutdown`] before the child is stopped: the pump's end
    /// of stream that follows is a hang-up, not a loss.
    retired: Arc<AtomicBool>,
    /// Set by the pump when stdout ended on its own: the child died.
    lost: Arc<AtomicBool>,
}

fn write_value(stdin: &StdinWriter, value: &Value, tag: Option<u64>) -> std::io::Result<()> {
    let line = serde_json::to_string(value).map_err(|error| std::io::Error::other(error.to_string()))?;
    stdin.enqueue(line, tag.map(|id| id.to_string()))
}

impl RunningChild {
    /// Spawn `program args…` and start its pump. The child's `PATH` is the
    /// login-shell `PATH` with the program's own directory first (a Dock
    /// launch still runs a home install), and the inherited desktop-agent
    /// env is scrubbed ([`provider::child_env`]). Its stderr drains into
    /// `stderr`, the adapter's one ring.
    pub fn spawn(
        program: &str,
        args: &[String],
        shared: Arc<Mutex<Shared>>,
        events: Sender<ProviderEvent>,
        stderr: provider::StderrTail,
    ) -> std::io::Result<Self> {
        let mut command = Command::new(program);
        command.args(args).env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)));
        provider::child_env::scrub_command(&mut command);
        let Spawned { process: child, stdin, stdout, stderr: stderr_pipe } = ChildProcess::spawn(&command)?;
        stderr.drain(stderr_pipe, "provider-acp-stderr", Some("acp"))?;
        let retired = Arc::new(AtomicBool::new(false));
        let lost = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Requests::new());
        // A request whose line never reached the child fails now, not at
        // its deadline.
        let failed = Arc::clone(&requests);
        let stdin = StdinWriter::spawn(stdin, "provider-acp-stdin", move |id, reason| {
            if let Ok(id) = id.parse::<u64>() {
                failed.complete(&id, Err(RpcError { code: None, message: format!("the agent is unreachable: {reason}") }));
            }
        })?;
        let (pump_stdin, pump_shared, pump_requests, pump_events) =
            (stdin.clone(), Arc::clone(&shared), Arc::clone(&requests), events.clone());
        let step = move |line: provider::runtime::Line| {
            if let Some(note) = line.note() {
                eprintln!("[acp] {note}");
                return Flow::Continue;
            }
            let Some(frame) = wire::decode(&line.text) else { return Flow::Continue };
            // Queued, never written here: the pump never waits on the
            // child's stdin.
            let write = |value: Value| {
                let _ = write_value(&pump_stdin, &value, None);
            };
            step(frame, &pump_shared, &pump_requests, &pump_events, &write);
            Flow::Continue
        };
        let (end_requests, end_retired, end_lost) = (Arc::clone(&requests), Arc::clone(&retired), Arc::clone(&lost));
        let end = move |end: PumpEnd| {
            let failed = match end {
                PumpEnd::Panicked(message) => {
                    shared.clear_poison();
                    Some(provider::guard::panic_reason("acp", &message))
                }
                PumpEnd::Closed => None,
            };
            end_requests.close(EXITED_BEFORE_ANSWERING, |reason| Err(RpcError { code: None, message: reason.to_owned() }));
            if end_retired.load(Ordering::SeqCst) {
                return;
            }
            let reason = failed.unwrap_or_else(|| match stderr.settled(STDERR_SETTLE).last() {
                Some(line) => format!("{CHILD_EXITED_REASON}: {}", line.trim()),
                None => CHILD_EXITED_REASON.to_owned(),
            });
            {
                let mut shared = lock(&shared);
                shared.parked.clear();
                shared.prompts.clear();
                shared.muted = false;
                shared.fold.lost(&reason);
                emit_deltas(&mut shared, &events);
            }
            end_lost.store(true, Ordering::SeqCst);
            let _ = events.send(ProviderEvent::ConnectionLost { reason });
        };
        // Detached, never joined: the pump ends when stdout closes.
        spawn_pump("provider-acp-pump", stdout, step, end)?;
        Ok(Self { child, stdin, next_id: AtomicU64::new(1), requests, retired, lost })
    }

    /// Whether this child's stdout ended on its own: it died.
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    /// The child's pid.
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Call `method` and wait up to `timeout` for its result.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, RpcError> {
        let id = self.next_id();
        let unreachable = |reason: String| RpcError { code: None, message: format!("the agent is unreachable: {reason}") };
        // A closed map (the child already exited) refuses at once.
        let waiter = self.requests.insert(id, timeout).map_err(unreachable)?;
        if let Err(error) = write_value(&self.stdin, &wire::request(id, method, params), Some(id)) {
            self.requests.forget(&id);
            return Err(unreachable(error.to_string()));
        }
        match self.requests.wait(waiter) {
            Ok(answer) => answer,
            Err(_) => Err(RpcError { code: None, message: format!("{method} timed out") }),
        }
    }

    /// Send a prompt as `turn`: registered with the pump before it is
    /// written, so an answer that beats this call still ends the turn.
    /// Never waits for the answer — the turn ends when it lands.
    pub fn prompt(&self, shared: &Mutex<Shared>, turn: &str, params: Value) -> std::io::Result<()> {
        let id = self.next_id();
        lock(shared).prompts.insert(id, turn.to_owned());
        let written = write_value(&self.stdin, &wire::request(id, wire::SESSION_PROMPT, params), None);
        if written.is_err() {
            lock(shared).prompts.remove(&id);
        }
        written
    }

    /// Send a notification or an answer: queued, never waiting on the
    /// child reading its stdin.
    pub fn send(&self, value: &Value) -> std::io::Result<()> {
        write_value(&self.stdin, value, None)
    }

    /// Hang up: the pump's end of stream that follows reports nothing.
    pub fn shutdown(&self) {
        self.retired.store(true, Ordering::SeqCst);
        self.child.terminate(provider::runtime::TERM_GRACE);
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// One frame from the agent.
fn step(frame: Frame, shared: &Mutex<Shared>, requests: &Requests, events: &Sender<ProviderEvent>, write: &impl Fn(Value)) {
    match frame {
        Frame::Response { id, result } => {
            let Some(id) = id.as_u64() else { return };
            let mut held = lock(shared);
            if held.prompts.remove(&id).is_some() {
                end_turn(&mut held, &Ending::Stopped(StopReason::of(&result)), events, write);
                return;
            }
            if held.opening.is_some() {
                held.adopt(&result);
            }
            drop(held);
            requests.complete(&id, Ok(result));
        }
        Frame::Error { id, code, message } => {
            let Some(id) = id.as_u64() else { return };
            let mut held = lock(shared);
            if held.prompts.remove(&id).is_some() {
                end_turn(&mut held, &Ending::Failed { code, message }, events, write);
                return;
            }
            drop(held);
            requests.complete(&id, Err(RpcError { code: Some(code), message }));
        }
        Frame::Request { id, method, params } => {
            let mut held = lock(shared);
            let ours = held.wire.as_deref().is_some_and(|wire| wire::session_of(&params) == Some(wire));
            if method != wire::REQUEST_PERMISSION || !ours {
                write(wire::method_not_found(&id, &method));
                return;
            }
            let approval = params
                .get("toolCall")
                .and_then(|call| call.get("toolCallId"))
                .and_then(Value::as_str)
                .filter(|call| !call.is_empty() && !held.parked.iter().any(|parked| parked.approval == *call))
                .map_or_else(|| format!("rpc-{id}"), str::to_owned);
            match held.fold.permission(&approval, &params) {
                Some((headline, options)) => {
                    held.parked.push(Parked { approval: approval.clone(), rpc: id, headline: headline.clone() });
                    emit_deltas(&mut held, events);
                    let session_id = held.fold.session_id().to_owned();
                    let _ = events.send(ProviderEvent::ApprovalRequested { session_id, approval_id: approval, headline, options });
                }
                // No turn to hold the card: the request is moot.
                None => write(wire::permission_cancelled(&id)),
            }
        }
        Frame::Notification { method, params } => {
            if method != wire::SESSION_UPDATE {
                return;
            }
            let mut held = lock(shared);
            let ours = held.wire.as_deref().is_some_and(|wire| wire::session_of(&params) == Some(wire));
            if !ours || held.muted {
                return;
            }
            let mut beside = Vec::new();
            held.fold.update(params.get("update").unwrap_or(&Value::Null), &mut beside);
            emit_deltas(&mut held, events);
            for event in beside {
                let _ = events.send(event);
            }
        }
    }
}
