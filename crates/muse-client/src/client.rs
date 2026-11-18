//! The `muse serve` child, its I/O threads, and the request plane.
//!
//! The child runs on the shared process runtime ([`provider_runtime`]):
//! spawned in a new session and process group, recorded in the pid
//! ledger, hung up as a group (SIGTERM, then SIGKILL), its stdout read
//! lossily with a line cap, its stdin owned by one writer thread, its
//! requests held in a [`RequestMap`] whose close no insert can slip past.

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use provider_runtime::pump::{spawn_pump, Flow, PumpEnd};
use provider_runtime::{ChildProcess, RequestMap, Spawned, StderrTail, StdinWriter, Unanswered, TERM_GRACE};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{MuseError, Result};
use crate::frame::{notification_line, parse_line, request_line, Frame};
use crate::program::{
    child_path_needs_repair, child_path_value, muse_fallback_dirs, resolve_muse_program,
    resolve_spawn_program, MUSE_BINARY,
};
use crate::schema::{
    AccountLoginCancelResult, AccountLoginStartParams, AccountLoginStartResult, AccountState,
    ApprovalDecideParams, ApprovalDecideResult,
    ApprovalListPendingParams, ApprovalListPendingResult, ClientCapabilities, ClientInfo,
    InitializeParams, InitializeResult,
    ItemReadOutputParams, ItemReadOutputResult, ModelListParams, ModelListResult,
    SessionCompactParams, SessionCompactResult, SessionForkParams, SessionForkResult,
    SessionListParams, SessionListResult, SessionReadParams, SessionReadResult, SessionResumeParams,
    SessionResumeResult, SessionSetApprovalModeParams, SessionSetApprovalModeResult,
    SessionSetModelParams, SessionSetModelResult, SessionStartParams, SessionStartResult,
    SessionUserShellParams, SessionUserShellResult, TurnCancelParams, TurnCancelResult,
    TurnInterruptParams, TurnInterruptResult, TurnStartParams, TurnStartResult, TurnSteerParams,
    TurnSteerResult, TurnUnqueueParams, TurnUnqueueResult, UserInputAnswerParams,
    UserInputAnswerResult, UserInputCancelParams, UserInputCancelResult, UserInputClarifyParams,
    UserInputClarifyResult, ViewPageParams, ViewPageResult, ViewSubscribeParams, ViewSubscribeResult,
    ViewUnsubscribeParams, ViewUnsubscribeResult, SCHEMA_FINGERPRINT,
};

/// How long a request waits before giving up on a silent server.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(180);

/// The page size used to fill a [`view/gap`](MuseEvent) hole. The wire caps
/// `view/page.limit` at 1000.
const GAP_PAGE_LIMIT: u64 = 1000;

/// How many `view/page` requests one backfill may make before giving up.
///
/// The loop used to be unbounded and could block up to [`REQUEST_TIMEOUT`]
/// per page, so a server that kept handing back a `nextCursor` held a
/// session's live events parked indefinitely (finding `client-adapter-7`).
/// At [`GAP_PAGE_LIMIT`] events a page this is 64 000 events, far more than
/// any retained window, and it is a bound rather than a policy: reaching it
/// aborts the fill loudly.
const GAP_MAX_PAGES: usize = 64;

/// How many live events one session may park while its backfill runs.
///
/// Parking was unbounded, so a busy session behind a slow backfill grew a
/// `Vec` with nothing to stop it (finding `client-adapter-7`). Past this the
/// fill is abandoned, the parked events are released in order, and the app is
/// told — a hole the reader can see beats memory it cannot.
const GAP_MAX_BUFFERED: usize = 10_000;

/// The synthesized notification a wire fault raises.
///
/// `read_loop` turns an unframable line, an id-less error and a response with
/// no waiter into one of these. It carries a `message` and, when the fault is
/// a response, its `id`; it has no `sessionId`, because a fault at the frame
/// level belongs to no session — the app files it against the view that is
/// open (finding `client-adapter-4`).
pub const PROTOCOL_ERROR: &str = "client/protocolError";

/// The synthesized notification an abandoned backfill raises.
///
/// Carries `sessionId` and a `reason`, so the fold can turn it into the row
/// that says the transcript above may be missing events.
pub const GAP_ABORTED: &str = "client/gapAborted";

/// The environment variable that turns on a wire capture.
///
/// Set it to a path and every line, both directions, is appended there in the
/// `--> ` / `<-- ` format the fixtures under `fixtures/msp/` use — which is the
/// format the fold's own tests and the app's `--replay` read. That is the whole
/// point: a session driven by hand once becomes a fixture the tests replay
/// forever, and a bug that needed a real provider to reach needs it only once.
///
/// ```text
/// MUSE_CAPTURE=fixtures/msp/transcript-userinput-answer.jsonl \
///   cargo run -p baaz -- --provider meta --workspace /tmp/ws
/// ```
///
/// Nothing is redacted. A capture holds the prompts, the replies and the
/// workspace paths of whoever made it, so a capture that is going to be checked
/// in wants reading first.
pub const CAPTURE_ENV: &str = "MUSE_CAPTURE";

/// The sink a capture is written to: one file, one mutex, both threads.
struct Capture {
    file: Mutex<std::fs::File>,
}

impl Capture {
    /// Open the capture named by [`CAPTURE_ENV`], if it is set.
    fn from_env() -> Option<Arc<Capture>> {
        let path = std::env::var_os(CAPTURE_ENV)?;
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Some(Arc::new(Capture { file: Mutex::new(file) })),
            Err(error) => {
                eprintln!("muse-client: cannot open {}: {error}", path.to_string_lossy());
                None
            }
        }
    }

    /// One line, with the arrow that says which way it went. Secrets are
    /// redacted first: captures become fixtures in a public repository,
    /// and `account/loginStart` carries the API key (docs/81 B PM-17).
    fn write(&self, arrow: &str, line: &str) {
        let line = redact_secrets(line.trim_end());
        let Ok(mut file) = self.file.lock() else { return };
        let _ = writeln!(file, "{arrow} {line}");
    }
}

/// The keys whose values never reach a capture file.
const SECRET_KEYS: [&str; 4] = ["apiKey", "api_key", "token", "password"];

/// `line` with every [`SECRET_KEYS`] value replaced by `"[redacted]"`, at
/// any depth. A line that is not JSON is kept only when it names none of
/// the keys; otherwise it becomes one redaction marker.
fn redact_secrets(line: &str) -> String {
    fn scrub(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, inner) in map.iter_mut() {
                    if SECRET_KEYS.contains(&key.as_str()) && !inner.is_null() {
                        *inner = serde_json::Value::String("[redacted]".into());
                    } else {
                        scrub(inner);
                    }
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(scrub),
            _ => {}
        }
    }
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(mut value) => {
            scrub(&mut value);
            value.to_string()
        }
        Err(_) if SECRET_KEYS.iter().any(|key| line.contains(key)) => "[redacted line]".to_owned(),
        Err(_) => line.to_owned(),
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    /// docs/81 B PM-17: an API key typed into the login sheet never lands
    /// in a capture file.
    #[test]
    fn a_login_api_key_is_redacted_in_captures() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"account/loginStart","params":{"apiKey":"sk-live-123","nested":{"token":"t"}}}"#;
        let out = redact_secrets(line);
        assert!(!out.contains("sk-live-123") && !out.contains("\"t\""), "{out}");
        assert!(out.contains("account/loginStart"), "the rest stays: {out}");
        assert_eq!(redact_secrets("not json apiKey=sk"), "[redacted line]");
        assert_eq!(redact_secrets(r#"{"method":"turn/start"}"#), r#"{"method":"turn/start"}"#);
    }
}

/// Mint a fresh `commandId`.
///
/// Every MSP command carries a **client-minted UUIDv7** and the server enforces
/// the version — a v4 uuid is rejected with `invalidParams`. The id is also the
/// idempotency handle, it *equals* the `turnId` of a fresh `turn/start`, and
/// `turn/unqueued` / `turn/retracted` echo it back so the composer can restore
/// the prompt text (research §1.4, §1.5).
pub fn new_command_id() -> String {
    Uuid::now_v7().to_string()
}

/// Everything the client hands to the app as it arrives.
#[derive(Clone, Debug, PartialEq)]
pub enum MuseEvent {
    /// A server→client notification, already ordered by the wire.
    Notification {
        /// Method name, e.g. `"turn/completed"`.
        method: String,
        /// The params object, untouched.
        params: Value,
        /// `params.viewCursor` when the notification carries one. Opaque and
        /// strictly monotonic — compare for equality, never parse.
        cursor: Option<String>,
        /// `params.sessionId` when the notification carries one.
        session_id: Option<String>,
    },
    /// A server→client request: `approval/request` or `userInput/request`.
    ///
    /// It is surfaced, never answered with a JSON-RPC result. Settle it with
    /// `approval/decide` or `userInput/answer|cancel|clarify`, and de-duplicate
    /// it against the matching `…/requested` notification on `approvalId` /
    /// `userInputId`.
    ServerRequest {
        /// The server's own id, in the server's own id space.
        id: Value,
        /// Method name.
        method: String,
        /// The params object, identical to the sibling notification's.
        params: Value,
    },
    /// The child exited. The app should respawn, `initialize`, and
    /// `session/resume` from the last observed `viewCursor`.
    Closed(Option<i32>),
}

impl MuseEvent {
    /// The event a server→client [`Frame`] becomes, or `None` for a response
    /// frame, which belongs to a pending request rather than to the app.
    ///
    /// This is the same conversion the reader thread does, exposed so a captured
    /// transcript can be replayed through the fold without a child process.
    pub fn from_frame(frame: Frame) -> Option<MuseEvent> {
        match frame {
            Frame::Notification { method, params } => {
                let (cursor, session_id) = event_cursor_session(&params);
                Some(MuseEvent::Notification { method, params, cursor, session_id })
            }
            Frame::ServerRequest { id, method, params } => {
                Some(MuseEvent::ServerRequest { id, method, params })
            }
            Frame::Response { .. } | Frame::ErrorResponse { .. } => None,
        }
    }
}

/// `params.viewCursor` and `params.sessionId`, read the one way every caller
/// needs them (finding `client-adapter-9`): [`MuseEvent::from_frame`] reads
/// them off a freshly parsed notification, and `handle_frame` reads the same
/// two fields off a live one arriving on the reader thread. Both used to
/// re-spell the same two `Value::get` calls.
fn event_cursor_session(params: &Value) -> (Option<String>, Option<String>) {
    let cursor = params.get("viewCursor").and_then(Value::as_str).map(str::to_owned);
    let session_id = params.get("sessionId").and_then(Value::as_str).map(str::to_owned);
    (cursor, session_id)
}

/// How to spawn the child.
#[derive(Clone, Debug)]
pub struct MuseConfig {
    /// Path to the `muse` binary.
    pub program: PathBuf,
    /// Load each session workspace's skills and rules. The app always wants
    /// this; it is a host-level posture and is not negotiable on the wire.
    pub trust_workspace: bool,
    /// Memory-only sessions (`sessionDurability: "ephemeral"`).
    ///
    /// ⚠️ **An ephemeral host emits no view events.** Verified live against
    /// 1.0.3 with this client and independently with the reference Python
    /// probe: under `--no-session-log` a `turn/start` is accepted and then
    /// `session/started` is the *only* notification that ever arrives — no
    /// `turn/started`, no items, no `turn/completed`. So anything that needs to
    /// see a transcript must run durable. The flag stays because a host that
    /// only issues commands still works, and because the finding is worth
    /// keeping expressible.
    pub no_session_log: bool,
    /// Extra arguments appended verbatim after the flags above.
    pub extra_args: Vec<String>,
}

impl Default for MuseConfig {
    fn default() -> Self {
        Self {
            // The resolved install when one is found, else the bare name —
            // unchanged behaviour for machines where `muse` is on `PATH`,
            // and a working spawn from a Dock launch where it is not.
            program: resolve_muse_program().unwrap_or_else(|| PathBuf::from(MUSE_BINARY)),
            trust_workspace: true,
            no_session_log: false,
            extra_args: Vec::new(),
        }
    }
}

/// A warning raised by [`MuseClient::initialize`], never an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SchemaWarning {
    /// The server's stable-surface fingerprint differs from the bundle these
    /// types were generated from. SS1.4.1 makes this a warning condition, so the
    /// app logs it and carries on.
    FingerprintMismatch {
        /// The fingerprint the types were generated from.
        expected: String,
        /// What the server reported.
        found: String,
    },
}

struct GapState {
    /// Sessions currently backfilling, with the events buffered meanwhile.
    buffering: HashMap<String, Vec<MuseEvent>>,
    /// Sessions whose backfill was abandoned while its thread still runs: its
    /// pages are dropped rather than spliced in behind live events that have
    /// already gone through (finding `client-adapter-7`).
    abandoned: HashSet<String>,
    /// The live `muse-gapfill` threads, so [`MuseClient`] joins them on the
    /// way out instead of leaving them detached.
    fills: Vec<JoinHandle<()>>,
}

struct Inner {
    next_id: AtomicI64,
    /// The wire capture, when `MUSE_CAPTURE` named a file.
    capture: Option<Arc<Capture>>,
    /// Requests in flight by id. Closed when the child's stdout ends or the
    /// client shuts down: every open request fails then, and a request made
    /// after fails at once instead of waiting [`REQUEST_TIMEOUT`] on a dead
    /// child (docs/81 B PM-9).
    requests: RequestMap<i64, Result<Value>>,
    /// The child's stdin writer. An `Option` so [`MuseClient::shutdown`] can
    /// drop it: the writer thread ends once its last handle is gone.
    writes: Mutex<Option<StdinWriter>>,
    events: Sender<MuseEvent>,
    gap: Mutex<GapState>,
    /// The child's last stderr lines, drained on their own thread so the
    /// child can never block on a full pipe.
    stderr: StderrTail,
}

impl Inner {
    /// Hand one framed line to the writer thread. `id` tags a request's
    /// line, so a failed write fails that request at once.
    fn write_line(&self, line: String, id: Option<i64>) -> Result<()> {
        if let Some(capture) = &self.capture {
            capture.write("-->", &line);
        }
        let writes = self.writes.lock().expect("writes mutex");
        let Some(writes) = writes.as_ref() else { return Err(MuseError::Closed) };
        // The writer adds the newline the framed line already ends with.
        let line = line.strip_suffix('\n').map(str::to_owned).unwrap_or(line);
        writes.enqueue(line, id.map(|id| id.to_string())).map_err(|_| MuseError::Closed)
    }

    fn request_value(&self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let waiter = self.requests.insert(id, REQUEST_TIMEOUT).map_err(|_| MuseError::Closed)?;
        let line = request_line(id, method, params.as_ref());
        if self.write_line(line, Some(id)).is_err() {
            self.requests.forget(&id);
            return Err(MuseError::Closed);
        }
        match self.requests.wait(waiter) {
            Ok(result) => result,
            Err(Unanswered::Expired) => Err(MuseError::Timeout(method.to_owned())),
            Err(Unanswered::Forgotten) => Err(MuseError::Closed),
        }
    }

    fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        self.write_line(notification_line(method, params.as_ref()), None)
    }

    /// Emit an event, or park it if its session is mid-backfill.
    fn dispatch(&self, event: MuseEvent) {
        // `approval/request` / `userInput/request` carry `sessionId` in
        // `params`, exactly like their sibling `…/requested` notification.
        // Both must park during that session's `view/gap` backfill (finding
        // `client-adapter-2`) or a request arriving mid-fill folds its card
        // immediately while the item it gates is still queued behind the
        // parked notifications, landing the approval/question before (or
        // instead of alongside) the tool card it belongs to.
        let session = match &event {
            MuseEvent::Notification { session_id, .. } => session_id.clone(),
            MuseEvent::ServerRequest { params, .. } => {
                params.get("sessionId").and_then(Value::as_str).map(str::to_owned)
            }
            MuseEvent::Closed(_) => None,
        };
        if let Some(session) = session {
            let mut gap = self.gap.lock().expect("gap mutex");
            if let Some(buffer) = gap.buffering.get_mut(&session) {
                if buffer.len() < GAP_MAX_BUFFERED {
                    buffer.push(event);
                    return;
                }
                // The buffer is full: give up on the splice rather than grow
                // (finding `client-adapter-7`). Everything parked goes out in
                // order, this event behind it, and the fill thread's pages
                // are dropped when it finishes.
                let parked = gap.buffering.remove(&session).unwrap_or_default();
                gap.abandoned.insert(session.clone());
                drop(gap);
                self.gap_aborted(
                    &session,
                    &format!("more than {GAP_MAX_BUFFERED} events arrived while backfilling"),
                );
                for parked in parked {
                    let _ = self.events.send(parked);
                }
                let _ = self.events.send(event);
                return;
            }
        }
        let _ = self.events.send(event);
    }

    /// Say out loud that a backfill gave up, so the `view/gap` row can stop
    /// promising a transcript that is about to have a hole in it.
    fn gap_aborted(&self, session_id: &str, reason: &str) {
        let _ = self.events.send(MuseEvent::Notification {
            method: GAP_ABORTED.into(),
            params: serde_json::json!({ "sessionId": session_id, "reason": reason }),
            cursor: None,
            session_id: Some(session_id.to_owned()),
        });
    }
}

/// One `muse serve` child. Owns the pipes; all I/O runs on background threads so
/// the UI thread never blocks on a pipe.
///
/// Sessions are multiplexed on a single child: one process per app, many
/// sessions. The transport carries **no policy** — it frames, routes and
/// surfaces; interpreting the stream is the fold's job.
pub struct MuseClient {
    inner: Arc<Inner>,
    events: Receiver<MuseEvent>,
    /// The child, shared with the reader (which reads its exit code). A
    /// hang-up needs no lock: [`MuseClient::shutdown`] takes `&self`, so a
    /// client shared behind an `Arc` is still killed (docs/81 B PM-12).
    child: Arc<ChildProcess>,
}

impl MuseClient {
    /// Spawn `muse serve` and start the reader and writer threads.
    ///
    /// A bare program name is looked up on `PATH` and then the home-install
    /// fallbacks (see [`resolve_muse_program`](crate::resolve_muse_program)),
    /// so a Dock launch with launchd's minimal `PATH` still finds a
    /// `~/.local/bin/muse`. When nothing is found the error says where it
    /// looked. The child's `PATH` is the current `PATH` plus the program's
    /// directory plus the existing fallback dirs, so the `muse` wrapper and
    /// the tools muse runs work from that same minimal launch.
    pub fn spawn(config: &MuseConfig) -> Result<Self> {
        Self::spawn_with_env(config, &[], &[])
    }

    /// Spawn `muse serve` with per-child environment removals and
    /// overrides: one `env_remove` per `env_remove` entry (applied first,
    /// so an override below can still set a removed name on purpose), then
    /// one `env` per `env_set` entry. Additive: [`spawn`](Self::spawn)
    /// delegates with both lists empty. The provider host passes the
    /// desktop-agent scrub as `env_remove`, so the child never inherits
    /// Baaz's launch session — without touching this process's env.
    pub fn spawn_with_env(
        config: &MuseConfig,
        env_remove: &[String],
        env_set: &[(String, String)],
    ) -> Result<Self> {
        Self::spawn_command(&Self::serve_command(config, env_remove, env_set)?)
    }

    /// Spawn a prepared `muse serve` command ([`Self::serve_command`], plus
    /// whatever the host sets on it — its `PATH`, its working directory)
    /// through the shared runtime: a new session and process group, in the
    /// pid ledger while it runs, stdio piped.
    pub fn spawn_command(command: &Command) -> Result<Self> {
        let Spawned { process, stdin, stdout, stderr } = ChildProcess::spawn(command)?;
        Self::from_parts(process, stdin, stdout, stderr)
    }

    /// The `muse serve` [`Command`] for `config`, before the stdio pipes:
    /// the same PATH repair [`spawn`](Self::spawn) always did, plus the
    /// per-child removals and overrides. Pure builder apart from the PATH
    /// probes — no process spawned, no process env touched — so tests
    /// assert the removals on the built command via `get_envs`.
    pub fn serve_command(
        config: &MuseConfig,
        env_remove: &[String],
        env_set: &[(String, String)],
    ) -> Result<Command> {
        let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|paths| std::env::split_paths(&paths).collect())
            .unwrap_or_default();
        let fallbacks = muse_fallback_dirs();
        let program = resolve_spawn_program(&config.program, &path_dirs, &fallbacks)?;
        let mut command = Command::new(&program);
        if child_path_needs_repair(&program, &path_dirs, &fallbacks) {
            let existing: Vec<PathBuf> =
                fallbacks.iter().filter(|dir| dir.is_dir()).cloned().collect();
            command.env("PATH", child_path_value(&path_dirs, &program, &existing));
        }
        for name in env_remove {
            command.env_remove(name);
        }
        for (name, value) in env_set {
            command.env(name, value);
        }
        command.arg("serve");
        if config.trust_workspace {
            command.arg("--trust-workspace");
        }
        if config.no_session_log {
            command.arg("--no-session-log");
        }
        command.args(&config.extra_args);
        Ok(command)
    }

    fn from_parts(
        child: ChildProcess,
        stdin: std::fs::File,
        stdout: std::fs::File,
        stderr: std::fs::File,
    ) -> Result<Self> {
        let (events_tx, events_rx) = unbounded::<MuseEvent>();
        let inner = Arc::new(Inner {
            next_id: AtomicI64::new(1),
            capture: Capture::from_env(),
            requests: RequestMap::new(),
            writes: Mutex::new(None),
            events: events_tx,
            gap: Mutex::new(GapState {
                buffering: HashMap::new(),
                abandoned: HashSet::new(),
                fills: Vec::new(),
            }),
            stderr: StderrTail::new(),
        });
        let child = Arc::new(child);

        // A request whose line could not be written fails now, not at its
        // deadline. Weak: the writer must not keep the client alive.
        let failed = Arc::downgrade(&inner);
        let writer = StdinWriter::spawn(stdin, "muse-writer", move |id, _| {
            if let (Some(inner), Ok(id)) = (failed.upgrade(), id.parse::<i64>()) {
                inner.requests.complete(&id, Err(MuseError::Closed));
            }
        })?;
        *inner.writes.lock().expect("writes mutex") = Some(writer);

        // The child's stderr is diagnostics, never protocol — but when the
        // child dies unexpectedly it is the only record of why. Drained
        // into a bounded tail so a chatty child cannot grow the client
        // without bound and a dead one cannot block on a full pipe.
        inner.stderr.drain(stderr, "muse-stderr", None)?;

        // The reader is detached, never joined: it ends when stdout closes,
        // and a grandchild still holding the pipe must not hang a teardown
        // (docs/81 B PM-5).
        let (reader_inner, end_inner, end_child) = (Arc::clone(&inner), Arc::clone(&inner), Arc::clone(&child));
        spawn_pump(
            "muse-reader",
            stdout,
            move |line| {
                read_line(&reader_inner, line);
                Flow::Continue
            },
            move |end| reader_ended(&end_inner, &end_child, end),
        )?;

        Ok(Self { inner, events: events_rx, child })
    }

    /// The child's pid (its session and process group too), for the log.
    pub fn pid(&self) -> u32 {
        self.child.pid()
    }

    /// The child's stderr, line-capped at the tail: what the child was saying
    /// when it exited. Diagnostics for the next reconnect — never secrets:
    /// `muse serve` carries no device-code flow on stderr, and the tail is
    /// bounded either way.
    pub fn stderr_tail(&self) -> String {
        self.inner.stderr.lines().join("\n")
    }

    /// The event stream, in wire order.
    ///
    /// `crossbeam_channel::Receiver::clone` is a **competing** consumer, not a
    /// broadcast: each event goes to exactly one clone, so two live clones
    /// split the transcript between them rather than each seeing it whole
    /// (finding `client-adapter-14`). Keep exactly one consumer — the pump in
    /// `conn.rs` — and fan out from there.
    pub fn events(&self) -> Receiver<MuseEvent> {
        self.events.clone()
    }

    /// Send any MSP method and get the raw result object back.
    ///
    /// This is the escape hatch for methods the typed wrappers do not cover
    /// (the `subagent/*` family). MSP results are always objects, possibly `{}`.
    pub fn request(&self, method: &str, params: Option<Value>) -> Result<Value> {
        self.inner.request_value(method, params)
    }

    fn call<P: Serialize, R: DeserializeOwned>(&self, method: &str, params: &P) -> Result<R> {
        let params = serde_json::to_value(params)?;
        let result = self.inner.request_value(method, Some(params))?;
        serde_json::from_value(result).map_err(MuseError::Json)
    }

    /// [`MuseClient::call`]'s sibling for a method that takes no params
    /// (finding `client-adapter-17`): `account/read`, `account/loginCancel`
    /// and `account/logout` each hand-rolled this `request_value` +
    /// `from_value` + `map_err` sequence because they have no params struct.
    fn call_no_params<R: DeserializeOwned>(&self, method: &str) -> Result<R> {
        let result = self.inner.request_value(method, None)?;
        serde_json::from_value(result).map_err(MuseError::Json)
    }

    /// Hang up: fail every request in flight, stop the writer, and end
    /// the child's whole process group (SIGTERM, then SIGKILL after
    /// [`TERM_GRACE`]). `&self`, so a client shared behind an `Arc` is
    /// killed too. Idempotent, so `Drop` can call this after an explicit
    /// `shutdown`. The reader and stderr threads are never joined: they end
    /// when their pipes close (docs/81 B PM-5).
    pub fn shutdown(&self) {
        self.inner.requests.close("the client shut down", |_| Err(MuseError::Closed));
        drop(self.inner.writes.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take());
        self.child.terminate(TERM_GRACE);
        // A gap fill parks in `request_value`, which fails the moment the
        // request map closes, so these return rather than outliving the
        // client (finding `client-adapter-7`). Taken under the lock, joined
        // outside it: the thread takes the same lock on its way out.
        let fills: Vec<JoinHandle<()>> =
            std::mem::take(&mut self.inner.gap.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).fills);
        for handle in fills {
            let _ = handle.join();
        }
    }

    // ---------------------------------------------------------------- handshake

    /// `initialize`, then the `initialized` notification.
    ///
    /// Returns the server's result and, when the server's stable-surface
    /// fingerprint differs from the bundle these types were generated from, a
    /// [`SchemaWarning`]. A mismatch is **a warning, never a failure**.
    ///
    /// `clientInfo.name` must match `[a-z0-9_]+`.
    pub fn initialize(
        &self,
        name: &str,
        version: &str,
        capabilities: ClientCapabilities,
    ) -> Result<(InitializeResult, Option<SchemaWarning>)> {
        let params = InitializeParams {
            client_info: ClientInfo {
                name: name.to_owned(),
                title: None,
                version: version.to_owned(),
            },
            capabilities: Some(capabilities),
        };
        let result: InitializeResult = self.call("initialize", &params)?;
        let warning = (result.schema.fingerprint != SCHEMA_FINGERPRINT).then(|| {
            SchemaWarning::FingerprintMismatch {
                expected: SCHEMA_FINGERPRINT.to_owned(),
                found: result.schema.fingerprint.clone(),
            }
        });
        self.inner.notify("initialized", None)?;
        Ok((result, warning))
    }

    // ------------------------------------------------------------------ sessions

    /// `session/start` — mint the session and subscribe at its cursor.
    pub fn session_start(&self, params: &SessionStartParams) -> Result<SessionStartResult> {
        self.call("session/start", params)
    }

    /// `session/resume` — load a session, subscribe, and get history back.
    pub fn session_resume(&self, params: &SessionResumeParams) -> Result<SessionResumeResult> {
        self.call("session/resume", params)
    }

    /// `session/fork` — branch a session at a turn boundary.
    pub fn session_fork(&self, params: &SessionForkParams) -> Result<SessionForkResult> {
        self.call("session/fork", params)
    }

    /// `session/list` — the sidebar's index. Read-only; never takes a lease.
    pub fn session_list(&self, params: &SessionListParams) -> Result<SessionListResult> {
        self.call("session/list", params)
    }

    /// `session/read` — read a stored session without attaching to it.
    /// Note `excludeItems` defaults to `true` here, the opposite of resume/fork.
    pub fn session_read(&self, params: &SessionReadParams) -> Result<SessionReadResult> {
        self.call("session/read", params)
    }

    /// `session/compact` — the `/compact` gesture. The one method that may ack
    /// `"noop"`, which is a success and not an error.
    pub fn session_compact(&self, params: &SessionCompactParams) -> Result<SessionCompactResult> {
        self.call("session/compact", params)
    }

    /// `session/setModel`. The result is admission only; the authority is the
    /// `session/modelChanged` notification.
    pub fn session_set_model(&self, params: &SessionSetModelParams) -> Result<SessionSetModelResult> {
        self.call("session/setModel", params)
    }

    /// `session/setApprovalMode`. Applies next-action; an in-flight approval is
    /// not decided retroactively.
    pub fn session_set_approval_mode(
        &self,
        params: &SessionSetApprovalModeParams,
    ) -> Result<SessionSetApprovalModeResult> {
        self.call("session/setApprovalMode", params)
    }

    /// `session/userShell` — the `!` escape hatch. Capability-gated on
    /// `userShell` at `initialize`, outside any turn, and still subject to the
    /// approval policy.
    pub fn session_user_shell(
        &self,
        params: &SessionUserShellParams,
    ) -> Result<SessionUserShellResult> {
        self.call("session/userShell", params)
    }

    // --------------------------------------------------------------------- turns

    /// `turn/start`. The ack's `turnId` is authoritative — never derive it.
    pub fn turn_start(&self, params: &TurnStartParams) -> Result<TurnStartResult> {
        self.call("turn/start", params)
    }

    /// `turn/steer` — inject input into the running turn. `expectedTurnId` is
    /// required and closes the race where the turn changes under you.
    pub fn turn_steer(&self, params: &TurnSteerParams) -> Result<TurnSteerResult> {
        self.call("turn/steer", params)
    }

    /// `turn/interrupt` — the stop button, on the runtime's priority lane. With
    /// `retract: true` an un-started turn is durably retracted and its prompt
    /// can be restored to the composer.
    pub fn turn_interrupt(&self, params: &TurnInterruptParams) -> Result<TurnInterruptResult> {
        self.call("turn/interrupt", params)
    }

    /// `turn/cancel` — the non-urgent cancel, on the normal command lane.
    ///
    /// No caller in `crates/baaz` yet (the shell uses `turn_interrupt`
    /// instead); kept as wire surface covered by `schema_roundtrip.rs`, not
    /// dead code.
    pub fn turn_cancel(&self, params: &TurnCancelParams) -> Result<TurnCancelResult> {
        self.call("turn/cancel", params)
    }

    /// `turn/unqueue` — reclaim a queued submission. Not a stop: a reclaim that
    /// arrives after its target launched is rejected.
    pub fn turn_unqueue(&self, params: &TurnUnqueueParams) -> Result<TurnUnqueueResult> {
        self.call("turn/unqueue", params)
    }

    // -------------------------------------------------------------------- models

    /// `model/list` — a query, not a command: no `commandId`, no view event.
    pub fn model_list(&self, params: &ModelListParams) -> Result<ModelListResult> {
        self.call("model/list", params)
    }

    // ------------------------------------------------------------------- account
    //
    // Sign-in over the wire (experimental surface). Every method here
    // requires `experimentalApi: true` at `initialize`; without the opt-in
    // each one answers `-32601` with `data.kind: "experimentalRequired"`.

    /// `account/read` — which credential lane is in effect. The only probe:
    /// `loggedOut` means the login screen, anything else means signed in.
    ///
    /// Requires `experimentalApi: true` at `initialize`, else `-32601` /
    /// `data.kind: "experimentalRequired"`.
    pub fn account_read(&self) -> Result<AccountState> {
        self.call_no_params("account/read")
    }

    /// `account/loginStart` — run the device-code flow (`type:
    /// "deviceCode"`, whose URL and code come back in the result) or store
    /// and validate an API key (`type: "apiKey"`, synchronous, `{}`).
    ///
    /// The key travels in `params` and must never reach a log; the
    /// hand-written `Debug` on [`AccountLoginStartParams`] redacts it.
    /// Requires `experimentalApi: true` at `initialize`, else `-32601` /
    /// `data.kind: "experimentalRequired"`.
    pub fn account_login_start(
        &self,
        params: &AccountLoginStartParams,
    ) -> Result<AccountLoginStartResult> {
        self.call("account/loginStart", params)
    }

    /// `account/loginCancel` — abandon the pending device-code flow. The
    /// `account/loginCompleted {cancelled}` notification arrives before this
    /// result.
    ///
    /// Requires `experimentalApi: true` at `initialize`, else `-32601` /
    /// `data.kind: "experimentalRequired"`.
    pub fn account_login_cancel(&self) -> Result<AccountLoginCancelResult> {
        self.call_no_params("account/loginCancel")
    }

    /// `account/logout` — clear the stored credential. The result is the new
    /// [`AccountState`]; apply it like `account/changed`. Note an `envKey`
    /// lane survives this (the environment still holds the key), so the
    /// caller must say so.
    ///
    /// Requires `experimentalApi: true` at `initialize`, else `-32601` /
    /// `data.kind: "experimentalRequired"`.
    pub fn account_logout(&self) -> Result<AccountState> {
        self.call_no_params("account/logout")
    }

    // ---------------------------------------------------------------------- view

    /// `view/page` — the backfill path. Pages are always ascending by
    /// `viewCursor`, contiguous, and never replay `item/delta`.
    pub fn view_page(&self, params: &ViewPageParams) -> Result<ViewPageResult> {
        self.call("view/page", params)
    }

    /// `view/subscribe` — attach this connection's live view subscription at an
    /// explicit cursor. The re-attach path after `view/unsubscribe`.
    ///
    /// No caller in `crates/baaz` yet (the app never detaches its
    /// subscription); kept as wire surface covered by `schema_roundtrip.rs`,
    /// not dead code.
    pub fn view_subscribe(&self, params: &ViewSubscribeParams) -> Result<ViewSubscribeResult> {
        self.call("view/subscribe", params)
    }

    /// `view/unsubscribe` — stop following a session. Idempotent; does not
    /// unload the session.
    ///
    /// No caller in `crates/baaz` yet (nothing detaches); kept as wire
    /// surface covered by `schema_roundtrip.rs`, not dead code.
    pub fn view_unsubscribe(
        &self,
        params: &ViewUnsubscribeParams,
    ) -> Result<ViewUnsubscribeResult> {
        self.call("view/unsubscribe", params)
    }

    // ------------------------------------------------------------------------ item

    /// `item/readOutput` — byte-ranged fetch of stored full output the view
    /// truncated. Read-only; works on loaded and unloaded sessions.
    pub fn item_read_output(
        &self,
        params: &ItemReadOutputParams,
    ) -> Result<ItemReadOutputResult> {
        self.call("item/readOutput", params)
    }

    // ----------------------------------------------------------------- approvals

    /// `approval/decide`. `requirementId` must equal the request's
    /// `currentRequirementId` — that is the multi-stage race guard. A `terminal:
    /// false` result means the stage was satisfied but more remain.
    pub fn approval_decide(&self, params: &ApprovalDecideParams) -> Result<ApprovalDecideResult> {
        self.call("approval/decide", params)
    }

    /// `approval/listPending` — the pull dual of the re-issued server requests.
    pub fn approval_list_pending(
        &self,
        params: &ApprovalListPendingParams,
    ) -> Result<ApprovalListPendingResult> {
        self.call("approval/listPending", params)
    }

    // ---------------------------------------------------------------- user input

    /// `userInput/answer` — one answer per question, keyed on the option
    /// **label**, not an index.
    pub fn user_input_answer(
        &self,
        params: &UserInputAnswerParams,
    ) -> Result<UserInputAnswerResult> {
        self.call("userInput/answer", params)
    }

    /// `userInput/cancel` — the tool call resolves cancelled and the model sees
    /// that.
    pub fn user_input_cancel(
        &self,
        params: &UserInputCancelParams,
    ) -> Result<UserInputCancelResult> {
        self.call("userInput/cancel", params)
    }

    /// `userInput/clarify` — "let me explain instead of picking". The model
    /// receives the text and re-decides.
    pub fn user_input_clarify(
        &self,
        params: &UserInputClarifyParams,
    ) -> Result<UserInputClarifyResult> {
        self.call("userInput/clarify", params)
    }
}

impl Drop for MuseClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// A line we cannot frame (or one dropped for its length) is a protocol
/// fault, not a reason to drop the connection silently: surface it and keep
/// reading.
fn protocol_fault(inner: &Inner, message: String) {
    let _ = inner.events.send(MuseEvent::Notification {
        method: PROTOCOL_ERROR.into(),
        params: serde_json::json!({ "message": message }),
        cursor: None,
        session_id: None,
    });
}

/// One line of the child's stdout, through the shared pump.
fn read_line(inner: &Arc<Inner>, line: provider_runtime::Line) {
    if let Some(note) = line.note() {
        protocol_fault(inner, note);
        return;
    }
    if let Some(capture) = &inner.capture {
        capture.write("<--", &line.text);
    }
    match parse_line(&line.text) {
        Ok(None) => {}
        Ok(Some(frame)) => handle_frame(inner, frame),
        Err(err) => protocol_fault(inner, err.to_string()),
    }
}

/// The child's stdout ended (or the reader failed): fail every request in
/// flight — and every one made after — so no caller waits on a dead child,
/// then say the child is gone, with its exit code when it has one. The
/// wait for that code is bounded and takes no lock: a child that closed
/// its stdout but lives on never wedges the reader (docs/81 B PM-6).
fn reader_ended(inner: &Inner, child: &ChildProcess, end: PumpEnd) {
    if let PumpEnd::Panicked(message) = end {
        protocol_fault(inner, format!("the reader failed and stopped: {message}"));
    }
    inner.requests.close("the child exited", |_| Err(MuseError::Closed));
    let code = child.wait_exit(Duration::from_millis(500)).and_then(provider_runtime::Exit::code);
    let _ = inner.events.send(MuseEvent::Closed(code));
}

fn handle_frame(inner: &Arc<Inner>, frame: Frame) {
    match frame {
        Frame::Response { id, result } => {
            // No waiter means either the id is unknown (a protocol fault) or
            // `request_value` already gave up after `REQUEST_TIMEOUT` — a
            // response that arrives after that is otherwise silently
            // dropped, leaving no trace to distinguish "the server never
            // answered" from "the server answered late" (finding
            // `client-adapter-5`).
            if !inner.requests.complete(&id, Ok(result)) {
                let _ = inner.events.send(MuseEvent::Notification {
                    method: PROTOCOL_ERROR.into(),
                    params: serde_json::json!({
                        "message": format!("response for unknown or timed-out request id {id}"),
                        "id": id,
                    }),
                    cursor: None,
                    session_id: None,
                });
            }
        }
        Frame::ErrorResponse { id, error } => {
            let unowned = match id {
                Some(id) => inner.requests.deliver(&id, Err(MuseError::Rpc(error))).err(),
                None => Some(Err(MuseError::Rpc(error))),
            };
            // A `null` id means an unrecoverable parse error: no request
            // owns it, so it goes to the app as an event (as does an error
            // for a request nobody waits on any more).
            if let Some(Err(MuseError::Rpc(error))) = unowned {
                let _ = inner.events.send(MuseEvent::Notification {
                    method: PROTOCOL_ERROR.into(),
                    params: serde_json::to_value(&*error).unwrap_or(Value::Null),
                    cursor: None,
                    session_id: None,
                });
            }
        }
        Frame::ServerRequest { id, method, params } => {
            inner.dispatch(MuseEvent::ServerRequest { id, method, params });
        }
        Frame::Notification { method, params } => {
            let (cursor, session_id) = event_cursor_session(&params);
            if method == "view/gap" {
                start_gap_fill(inner, &params);
            }
            inner.dispatch(MuseEvent::Notification { method, params, cursor, session_id });
        }
    }
}

/// Handle `view/gap` by the sanctioned splice-fill recovery: park live events
/// for that session, page the `(after, next)` range forward, emit the paged
/// events, then release the parked ones, discarding cursors the page already
/// covered.
fn start_gap_fill(inner: &Arc<Inner>, params: &Value) {
    let Some(session_id) = params.get("sessionId").and_then(Value::as_str).map(str::to_owned)
    else {
        return;
    };
    let after = params.get("after").and_then(Value::as_str).map(str::to_owned);
    {
        let mut gap = inner.gap.lock().expect("gap mutex");
        if gap.buffering.contains_key(&session_id) {
            return;
        }
        gap.buffering.insert(session_id.clone(), Vec::new());
    }
    let tracker = Arc::clone(inner);
    let inner = Arc::clone(inner);
    // The page request cannot run on the reader thread: the reader is what
    // completes it.
    let handle = std::thread::Builder::new().name("muse-gapfill".into()).spawn(move || {
        let mut filled: Vec<MuseEvent> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor = after;
        // Why the fill stopped, when it stopped early. `None` is a fill that
        // reached the end of the range the way it is supposed to.
        let mut aborted: Option<String> = None;
        for page_number in 0..=GAP_MAX_PAGES {
            if page_number == GAP_MAX_PAGES {
                aborted = Some(format!("the backfill did not end within {GAP_MAX_PAGES} pages"));
                break;
            }
            let mut page = serde_json::Map::new();
            page.insert("sessionId".into(), Value::String(session_id.clone()));
            page.insert("limit".into(), Value::from(GAP_PAGE_LIMIT));
            page.insert("direction".into(), Value::String("forward".into()));
            if let Some(cursor) = &cursor {
                page.insert("cursor".into(), Value::String(cursor.clone()));
            }
            let result = match inner.request_value("view/page", Some(Value::Object(page))) {
                Ok(result) => result,
                Err(error) => {
                    // Silent `break` was the old behaviour: the parked events
                    // were released with nothing said, so a failed backfill
                    // and a complete one looked the same (finding
                    // `client-adapter-7`).
                    aborted = Some(format!("view/page failed: {error}"));
                    break;
                }
            };
            let events = result.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
            for event in &events {
                let Some(method) = event.get("method").and_then(Value::as_str) else { continue };
                let params = event.get("params").cloned().unwrap_or(Value::Null);
                let event_cursor =
                    params.get("viewCursor").and_then(Value::as_str).map(str::to_owned);
                if let Some(event_cursor) = &event_cursor {
                    seen.insert(event_cursor.clone());
                }
                filled.push(MuseEvent::Notification {
                    method: method.to_owned(),
                    params,
                    cursor: event_cursor,
                    session_id: Some(session_id.clone()),
                });
            }
            match result.get("nextCursor").and_then(Value::as_str) {
                Some(next) if !events.is_empty() => cursor = Some(next.to_owned()),
                _ => break,
            }
        }
        let buffered = {
            let mut gap = inner.gap.lock().expect("gap mutex");
            if gap.abandoned.remove(&session_id) {
                // The parking was given up on while this ran: its events are
                // already out, and splicing pages in behind them would put
                // the transcript out of order.
                return;
            }
            gap.buffering.remove(&session_id).unwrap_or_default()
        };
        if let Some(reason) = aborted {
            inner.gap_aborted(&session_id, &reason);
        }
        for event in filled {
            let _ = inner.events.send(event);
        }
        for event in buffered {
            // The same cursor-dedup pass covers a parked `ServerRequest`: its
            // `params.viewCursor` is compared against the page's `seen` set
            // exactly like a parked notification's `cursor` field.
            let cursor = match &event {
                MuseEvent::Notification { cursor, .. } => cursor.clone(),
                MuseEvent::ServerRequest { params, .. } => {
                    params.get("viewCursor").and_then(Value::as_str).map(str::to_owned)
                }
                MuseEvent::Closed(_) => None,
            };
            if let Some(cursor) = &cursor {
                if seen.contains(cursor) {
                    continue;
                }
            }
            let _ = inner.events.send(event);
        }
    });
    // Tracked, not detached: `shutdown` joins these on the way out
    // (finding `client-adapter-7`).
    if let Ok(handle) = handle {
        tracker.gap.lock().expect("gap mutex").fills.push(handle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_serve_command_carries_removals_and_overrides() {
        let dir = std::env::temp_dir().join(format!(
            "muse-client-serve-cmd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let program = dir.join("muse");
        std::fs::write(&program, "#!/bin/sh\nexit 0\n").expect("fake muse");
        let config = MuseConfig {
            program: program.clone(),
            trust_workspace: true,
            no_session_log: false,
            extra_args: Vec::new(),
        };
        let command = MuseClient::serve_command(
            &config,
            &["CLAUDECODE".to_owned(), "ANTHROPIC_BASE_URL".to_owned()],
            &[("MUSE_PROBE".to_owned(), "1".to_owned())],
        )
        .expect("explicit program file resolves");
        for name in ["CLAUDECODE", "ANTHROPIC_BASE_URL"] {
            assert!(
                command.get_envs().any(|(key, value)| key == name && value.is_none()),
                "the serve command removes {name}: {:?}",
                command.get_envs().collect::<Vec<_>>()
            );
        }
        assert!(
            command
                .get_envs()
                .any(|(key, value)| key == "MUSE_PROBE" && value == Some(std::ffi::OsStr::new("1"))),
            "overrides ride the same command"
        );
        assert_eq!(command.get_program(), program.as_os_str(), "the resolved program spawns");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
