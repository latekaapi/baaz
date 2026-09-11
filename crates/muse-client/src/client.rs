//! The `muse serve` child, its two I/O threads, and the request plane.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossbeam_channel::{bounded, unbounded, Receiver, Sender};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::error::{MuseError, Result};
use crate::frame::{notification_line, parse_line, request_line, Frame};
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
///   cargo run -p harness -- --provider meta --workspace /tmp/ws
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

    /// One line, with the arrow that says which way it went.
    fn write(&self, arrow: &str, line: &str) {
        let Ok(mut file) = self.file.lock() else { return };
        let _ = writeln!(file, "{arrow} {}", line.trim_end());
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
            program: PathBuf::from("muse"),
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

struct Pending {
    tx: Sender<Result<Value>>,
}

struct GapState {
    /// Sessions currently backfilling, with the events buffered meanwhile.
    buffering: HashMap<String, Vec<MuseEvent>>,
}

struct Inner {
    next_id: AtomicI64,
    /// The wire capture, when `MUSE_CAPTURE` named a file.
    capture: Option<Arc<Capture>>,
    pending: Mutex<HashMap<i64, Pending>>,
    /// The writer thread's inbox. It is an `Option` so [`MuseClient::shutdown`]
    /// can *drop* it: the writer parks in `recv()`, and dropping the last sender
    /// is the only thing that wakes it, so without this the drop glue would
    /// join a thread that never returns.
    writes: Mutex<Option<Sender<String>>>,
    events: Sender<MuseEvent>,
    gap: Mutex<GapState>,
    closed: AtomicBool,
}

impl Inner {
    /// Hand one framed line to the writer thread.
    fn write_line(&self, line: String) -> Result<()> {
        if let Some(capture) = &self.capture {
            capture.write("-->", &line);
        }
        let writes = self.writes.lock().expect("writes mutex");
        let Some(writes) = writes.as_ref() else { return Err(MuseError::Closed) };
        writes.send(line).map_err(|_| MuseError::Closed)
    }

    fn request_value(&self, method: &str, params: Option<Value>) -> Result<Value> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(MuseError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = bounded(1);
        self.pending.lock().expect("pending mutex").insert(id, Pending { tx });
        let line = request_line(id, method, params.as_ref());
        if self.write_line(line).is_err() {
            self.pending.lock().expect("pending mutex").remove(&id);
            return Err(MuseError::Closed);
        }
        match rx.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                self.pending.lock().expect("pending mutex").remove(&id);
                Err(MuseError::Timeout(method.to_owned()))
            }
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => Err(MuseError::Closed),
        }
    }

    fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        self.write_line(notification_line(method, params.as_ref()))
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
                buffer.push(event);
                return;
            }
        }
        let _ = self.events.send(event);
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
    child: Arc<Mutex<Child>>,
    threads: Vec<JoinHandle<()>>,
}

impl MuseClient {
    /// Spawn `muse serve` and start the reader and writer threads.
    pub fn spawn(config: &MuseConfig) -> Result<Self> {
        let mut command = Command::new(&config.program);
        command.arg("serve");
        if config.trust_workspace {
            command.arg("--trust-workspace");
        }
        if config.no_session_log {
            command.arg("--no-session-log");
        }
        command.args(&config.extra_args);
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        Ok(Self::from_pipes(child, stdin, stdout))
    }

    fn from_pipes(child: Child, stdin: ChildStdin, stdout: ChildStdout) -> Self {
        let (writes_tx, writes_rx) = unbounded::<String>();
        let (events_tx, events_rx) = unbounded::<MuseEvent>();
        let inner = Arc::new(Inner {
            next_id: AtomicI64::new(1),
            capture: Capture::from_env(),
            pending: Mutex::new(HashMap::new()),
            writes: Mutex::new(Some(writes_tx)),
            events: events_tx,
            gap: Mutex::new(GapState { buffering: HashMap::new() }),
            closed: AtomicBool::new(false),
        });
        let child = Arc::new(Mutex::new(child));

        let writer = std::thread::Builder::new()
            .name("muse-writer".into())
            .spawn(move || {
                let mut stdin = stdin;
                while let Ok(line) = writes_rx.recv() {
                    if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
                        break;
                    }
                }
            })
            .expect("spawn muse-writer");

        let reader_inner = Arc::clone(&inner);
        let reader_child = Arc::clone(&child);
        let reader = std::thread::Builder::new()
            .name("muse-reader".into())
            .spawn(move || read_loop(reader_inner, reader_child, stdout))
            .expect("spawn muse-reader");

        Self { inner, events: events_rx, child, threads: vec![reader, writer] }
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

    /// Kill the child and stop the I/O threads.
    ///
    /// Dropping the writer's inbox is what stops the writer; killing the child
    /// closes its stdout, which is what stops the reader. Both are idempotent,
    /// so `Drop` can call this after an explicit `shutdown`.
    pub fn shutdown(&mut self) {
        self.inner.closed.store(true, Ordering::SeqCst);
        drop(self.inner.writes.lock().expect("writes mutex").take());
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
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
    pub fn view_subscribe(&self, params: &ViewSubscribeParams) -> Result<ViewSubscribeResult> {
        self.call("view/subscribe", params)
    }

    /// `view/unsubscribe` — stop following a session. Idempotent; does not
    /// unload the session.
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
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

fn read_loop(inner: Arc<Inner>, child: Arc<Mutex<Child>>, stdout: ChildStdout) {
    let reader = BufReader::new(stdout);
    for line in reader.lines() {
        let Ok(line) = line else { break };
        if let Some(capture) = &inner.capture {
            capture.write("<--", &line);
        }
        match parse_line(&line) {
            Ok(None) => {}
            Ok(Some(frame)) => handle_frame(&inner, frame),
            Err(err) => {
                // A line we cannot frame is a protocol fault, not a reason to
                // drop the connection silently: surface it and keep reading.
                let _ = inner.events.send(MuseEvent::Notification {
                    method: "client/protocolError".into(),
                    params: serde_json::json!({ "message": err.to_string() }),
                    cursor: None,
                    session_id: None,
                });
            }
        }
    }
    inner.closed.store(true, Ordering::SeqCst);
    let code = child.lock().ok().and_then(|mut c| c.wait().ok()).and_then(|s| s.code());
    // Fail every in-flight request so no caller waits on a dead child.
    let pending: Vec<_> = inner.pending.lock().expect("pending mutex").drain().collect();
    for (_, waiter) in pending {
        let _ = waiter.tx.send(Err(MuseError::Closed));
    }
    let _ = inner.events.send(MuseEvent::Closed(code));
}

fn handle_frame(inner: &Arc<Inner>, frame: Frame) {
    match frame {
        Frame::Response { id, result } => {
            match inner.pending.lock().expect("pending mutex").remove(&id) {
                Some(waiter) => {
                    let _ = waiter.tx.send(Ok(result));
                }
                // No waiter means either the id is unknown (a protocol
                // fault) or `request_value` already gave up after
                // `REQUEST_TIMEOUT` — a response that arrives after that is
                // otherwise silently dropped, leaving no trace to
                // distinguish "the server never answered" from "the server
                // answered late" (finding `client-adapter-5`).
                None => {
                    let _ = inner.events.send(MuseEvent::Notification {
                        method: "client/protocolError".into(),
                        params: serde_json::json!({
                            "message": format!("response for unknown or timed-out request id {id}"),
                            "id": id,
                        }),
                        cursor: None,
                        session_id: None,
                    });
                }
            }
        }
        Frame::ErrorResponse { id, error } => {
            let waiter = id.and_then(|id| inner.pending.lock().expect("pending mutex").remove(&id));
            match waiter {
                Some(waiter) => {
                    let _ = waiter.tx.send(Err(MuseError::Rpc(error)));
                }
                // A `null` id means an unrecoverable parse error: no request
                // owns it, so it goes to the app as an event.
                None => {
                    let _ = inner.events.send(MuseEvent::Notification {
                        method: "client/protocolError".into(),
                        params: serde_json::to_value(&*error).unwrap_or(Value::Null),
                        cursor: None,
                        session_id: None,
                    });
                }
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
    let inner = Arc::clone(inner);
    // The page request cannot run on the reader thread: the reader is what
    // completes it.
    let _ = std::thread::Builder::new().name("muse-gapfill".into()).spawn(move || {
        let mut filled: Vec<MuseEvent> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut cursor = after;
        loop {
            let mut page = serde_json::Map::new();
            page.insert("sessionId".into(), Value::String(session_id.clone()));
            page.insert("limit".into(), Value::from(GAP_PAGE_LIMIT));
            page.insert("direction".into(), Value::String("forward".into()));
            if let Some(cursor) = &cursor {
                page.insert("cursor".into(), Value::String(cursor.clone()));
            }
            let Ok(result) = inner.request_value("view/page", Some(Value::Object(page))) else {
                break;
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
        let buffered = inner
            .gap
            .lock()
            .expect("gap mutex")
            .buffering
            .remove(&session_id)
            .unwrap_or_default();
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
}
