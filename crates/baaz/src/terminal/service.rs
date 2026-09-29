//! The terminal service: the agent's seven terminal tools plus its six
//! browser tools over a unix socket (D46, Z7b).
//!
//! The app serves the `docs/14-terminal.md` §4 contract on
//! `<support_dir>/run/terminal-<pid>.sock` (dir `0700`, socket `0600`).
//! Every route — the session-MCP relay, the plugin relay, the shell CLI —
//! is a thin client of this socket, so the tools and the tab rules do not
//! change with the route.
//!
//! # Thread model
//!
//! One listener thread accepts connections; one thread per connection reads
//! line-delimited JSON requests (`{id, session, tool, params}`) and blocks
//! on a reply channel. No socket thread ever touches gpui: each request is
//! queued, and [`TerminalService::drain`] — called on the UI thread, every
//! 15 ms by the harness's pump task — executes the queue against
//! [`TerminalHost`] (and the browser registry, for `browser_*`) and
//! answers. A `terminal_run` with `wait: exit` stays a [`PendingRun`]
//! across drains: each drain pumps the tab once and checks the block, so a
//! long run never stalls a frame and the UI never blocks. A `browser_*`
//! evaluation likewise stays a [`PendingBrowser`] across drains: each drain
//! sends at most one script and collects what the page answered, so a slow
//! page never stalls a frame either.
//!
//! Only session ids the app registered ([`register_session`][TerminalService::register_session],
//! D53) are served; anything else is refused. The socket file is removed
//! when the service drops (quit).
//!
//! Terminal output never reaches a log line: this module has no logging at
//! all, by construction (pinned by `crates/baaz/tests/terminal_service.rs`).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aui_terminal::{BlockAuthor, TextCursor};
use aui_webview::{WebviewState, agent_js};
use base64::Engine as _;
use gpui::{App, Entity};
use serde_json::{Value, json};

use super::{TabOwner, TerminalHost, title_from_command};

/// The seven tools, in contract order.
pub const TOOL_NAMES: [&str; 7] = [
    "terminal_list",
    "terminal_open",
    "terminal_run",
    "terminal_read",
    "terminal_screen",
    "terminal_send",
    "terminal_close",
];

/// The six browser tools, in contract order (`docs/15-browser-tools.md`).
pub const BROWSER_TOOL_NAMES: [&str; 6] = [
    "browser_open",
    "browser_read",
    "browser_links",
    "browser_click",
    "browser_type",
    "browser_screenshot",
];

/// How long a `browser_*` evaluation waits for the page's answer before
/// the tool reports [`BROWSER_NO_ANSWER`].
pub const BROWSER_TIMEOUT: Duration = Duration::from_secs(15);
/// What a `browser_*` tool reports when the page never answered.
pub const BROWSER_NO_ANSWER: &str = "the page did not answer";
/// `browser_read`'s default `max_chars`.
pub const BROWSER_READ_DEFAULT_MAX: usize = 8_000;
/// `browser_links`'s default `max`.
pub const BROWSER_LINKS_DEFAULT_MAX: usize = 100;

/// `terminal_run` output is head+tail capped at this many bytes.
pub const RUN_OUTPUT_CAP: usize = 4_096;
/// `terminal_read`'s default `max_bytes`.
pub const READ_DEFAULT_MAX: usize = 4_096;
/// `terminal_read`'s maximum `max_bytes`.
pub const READ_MAX: usize = 32_768;

/// At most this many early/late browser answers wait in the stash.
const EVAL_STASH_MAX: usize = 64;

/// How long `browser_open` waits for a page title after its navigation
/// before answering with whatever the page has (often a title-less page).
const OPEN_TITLE_GRACE: Duration = Duration::from_secs(3);
/// `terminal_run`'s default `timeout_ms`.
pub const RUN_DEFAULT_TIMEOUT_MS: u64 = 30_000;
/// `terminal_run`'s maximum `timeout_ms`.
pub const RUN_MAX_TIMEOUT_MS: u64 = 600_000;
/// How often the listener notices the shutdown flag.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// What every relay tool reports when Baaz is not serving its socket.
pub const UNAVAILABLE: &str = "Baaz isn't running; the terminal is unavailable";

/// The socket path for a support dir and pid:
/// `<support_dir>/run/terminal-<pid>.sock`.
pub fn socket_path_for(support_dir: &Path, pid: u32) -> PathBuf {
    support_dir.join("run").join(format!("terminal-{pid}.sock"))
}

/// A restrictive process umask held while the run dir and socket are
/// created, restored on drop. `umask` is process-wide, so a static lock
/// serialises holders: without it two concurrent starts could restore in
/// the wrong order and leak one caller's wider mask into the other's
/// window.
struct UmaskGuard {
    previous: libc::mode_t,
    _lock: std::sync::MutexGuard<'static, ()>,
}

static UMASK_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

impl UmaskGuard {
    fn restrict() -> Self {
        let lock = UMASK_LOCK.lock().expect("umask lock");
        // SAFETY: `umask` always succeeds and is async-signal-safe; the
        // previous mask is restored in `drop`, and the static lock keeps
        // concurrent holders from interleaving set/restore pairs.
        let previous = unsafe { libc::umask(0o077) };
        Self { previous, _lock: lock }
    }
}

impl Drop for UmaskGuard {
    fn drop(&mut self) {
        // SAFETY: restoring the mask this guard replaced.
        unsafe {
            libc::umask(self.previous);
        }
    }
}

/// The service: session registry, request queue, pending runs, and the
/// socket's lifecycle. One service per window owns its socket; it is not
/// `Clone`, so a drop always means quit and always removes the socket.
pub struct TerminalService {
    shared: Arc<Shared>,
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
    /// Whether this window bound the socket (false when another window kept
    /// the name): only the owner removes it on quit.
    owned: bool,
    /// The listener thread, joined on drop so removal never races a still
    /// bound socket.
    accept: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

struct Shared {
    host: Entity<TerminalHost>,
    sessions: Mutex<HashMap<String, PathBuf>>,
    queue: Mutex<VecDeque<Job>>,
    pending: Mutex<Vec<PendingRun>>,
    /// Blocks a `wait: none` run (or a timed-out `wait: exit`) left behind:
    /// each drain stamps what has appeared so far agent-owned, until the
    /// run's block closes or the tab goes away.
    marks: Mutex<Vec<MarkWatch>>,
    activity: Mutex<Option<ActivityHook>>,
    /// One live webview per session id, registered by the harness when the
    /// pane creates it. A `browser_*` tool acts only on its calling
    /// session's entry here — never another session's — which is what
    /// keeps sessions' pages apart.
    browsers: Mutex<HashMap<String, Entity<WebviewState>>>,
    /// `browser_*` evaluations still waiting for their page's answer.
    pending_browser: Mutex<Vec<PendingBrowser>>,
    /// Evaluation answers that arrived for a request still queued: the
    /// page answers every outstanding script at once, so a drain that
    /// collects for one pending evaluation stashes the others' here.
    eval_stash: Mutex<HashMap<(String, u64), Result<String, String>>>,
    /// The next evaluation request id. Unique per service, so concurrent
    /// evaluations from one session never share an answer.
    next_request: AtomicU64,
    /// How long a `browser_*` evaluation waits for the page. Production is
    /// [`BROWSER_TIMEOUT`]; tests shorten it.
    browser_timeout: Mutex<Duration>,
    /// What the harness does when the agent opens a URL: open the Browser
    /// pane, which the harness defers like [`ActivityHook`] (same
    /// re-entrancy rule — `drain` runs inside a Harness update).
    browser_open: Mutex<Option<BrowserOpenHook>>,
    /// Spawn-time request ids a provider lane outgrew: Codex answers
    /// `OpenSession` with a server-minted thread id, while its bridge keeps
    /// calling with `--session <request_id>`. Maps request id → thread id so
    /// those calls still reach the lane's tabs and browser.
    session_aliases: Mutex<HashMap<String, String>>,
}

/// What the harness does when the agent runs something: open the dock (not
/// focused) so the person can watch. Runs on the UI thread, inside `drain` —
/// which the harness calls from inside its own update — so the hook must
/// never synchronously update the entity that is draining: defer that work
/// with `cx.defer`, which runs at the end of the effect cycle with the
/// entity off the stack. A hook that updates the draining entity re-enters
/// it and aborts the app.
type ActivityHook = Box<dyn Fn(&mut App) + Send + Sync>;

/// What the harness does when the agent opens a URL: open the right pane
/// on the Browser kind for that session. Same re-entrancy rule as
/// [`ActivityHook`]: runs inside `drain`, so the harness defers its own
/// update with `cx.defer`.
type BrowserOpenHook = Box<dyn Fn(&mut App, String) + Send + Sync>;

struct Job {
    id: Value,
    session: String,
    tool: String,
    params: Value,
    reply: mpsc::Sender<String>,
}

/// A `terminal_run` with `wait: exit` still in flight: the command is
/// pasted, `before` counts the blocks that predate it, and each drain pumps
/// the tab until the new block closes or the deadline passes. The command
/// keeps running either way — `timeout` only stops waiting.
struct PendingRun {
    id: Value,
    tab: String,
    before: usize,
    started: Instant,
    deadline: Instant,
    reply: mpsc::Sender<String>,
}

/// A `browser_*` call still waiting on its page: the script is sent once,
/// and each drain collects what arrived until the matching answer lands or
/// the deadline passes. The page keeps loading either way — the deadline
/// only stops waiting, and reports [`BROWSER_NO_ANSWER`].
struct PendingBrowser {
    id: Value,
    session: String,
    deadline: Instant,
    reply: mpsc::Sender<String>,
    wait: BrowserWait,
}

/// What a [`PendingBrowser`] is waiting for.
enum BrowserWait {
    /// `browser_open`: navigate once the session's webview exists, then
    /// answer once the page settles — with its FINAL url and title, never
    /// the requested URL, which WebKit normalises and redirects. The
    /// webview may not exist yet — the agent can open a URL before the
    /// person ever opened the pane — so the navigation waits for the
    /// harness's registration, not just the load.
    /// `before` is the pane's (url, title) just before this call navigated.
    Open { url: String, navigated: bool, before: Option<(String, String)> },
    /// `browser_read/links/click/type`: the script goes out once (`sent`),
    /// then the drain matches the answer by request id.
    Eval { request_id: u64, sent: bool, js: String, render: BrowserRender },
    /// `browser_screenshot`: ask once (`captured`), then read the bytes.
    Shot { captured: bool },
}

/// How a [`BrowserWait::Eval`] answer is shaped into the tool's result.
#[derive(Clone, Copy)]
enum BrowserRender {
    /// `browser_read`: the page's `{title, url, text}` object, as answered.
    Read,
    /// `browser_links`: the answered array, capped at `max` entries.
    Links { max: usize },
    /// `browser_click` / `browser_type`: the answered element text.
    Text,
}

/// A run the service already answered (`wait: none`, or a `wait: exit` that
/// timed out) whose blocks still want the agent mark: `before` counts the
/// blocks that predate the run, and each drain stamps what has appeared
/// since. Dropped when the run's block closes or the tab goes away — later
/// blocks are someone else's.
struct MarkWatch {
    tab: String,
    before: usize,
}

impl TerminalService {
    /// Serve on `<support_dir>/run/terminal-<pid>.sock` for this process.
    /// Best-effort: when the path is already live (a second window in this
    /// process) the service still registers sessions and drains, but owns
    /// no listener — the relay then reports the terminal unavailable
    /// rather than stealing the first window's socket.
    pub fn start(host: Entity<TerminalHost>, support_dir: &Path) -> Self {
        Self::start_at(host, support_dir, std::process::id())
    }

    /// Serve on [`socket_path_for`]`(support_dir, pid)`: the seam the tests
    /// drive with a temp dir standing in for the support dir.
    pub fn start_at(host: Entity<TerminalHost>, support_dir: &Path, pid: u32) -> Self {
        let path = socket_path_for(support_dir, pid);
        // No world-readable window: a restrictive umask holds from the run
        // dir's creation through the bind and its chmod, so neither the dir
        // nor the socket ever carries group/other bits. `DirBuilder::mode`
        // is umask-proof on its own (`mkdir` applies `mode & !umask`), and
        // the umask covers the bind, whose mode the process would otherwise
        // choose. The trailing `set_permissions` tightens pre-existing
        // dirs; the guard restores the mask on every return below.
        let _umask = UmaskGuard::restrict();
        if let Some(run_dir) = path.parent() {
            let _ = std::fs::DirBuilder::new().mode(0o700).create(run_dir);
            let _ = std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700));
        }
        let shared = Arc::new(Shared {
            host,
            sessions: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            pending: Mutex::new(Vec::new()),
            marks: Mutex::new(Vec::new()),
            activity: Mutex::new(None),
            browsers: Mutex::new(HashMap::new()),
            pending_browser: Mutex::new(Vec::new()),
            eval_stash: Mutex::new(HashMap::new()),
            next_request: AtomicU64::new(1),
            browser_timeout: Mutex::new(BROWSER_TIMEOUT),
            browser_open: Mutex::new(None),
            session_aliases: Mutex::new(HashMap::new()),
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        // A leftover file from a crashed run binds fine once removed; a
        // live socket means another window owns the name, so keep hands off.
        if path.exists() && UnixStream::connect(&path).is_ok() {
            return Self { shared, socket_path: path, shutdown, owned: false, accept: Mutex::new(None) };
        }
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                let _ = listener.set_nonblocking(true);
                let worker = shared.clone();
                let done = shutdown.clone();
                let accept =
                    std::thread::spawn(move || accept_loop(listener, worker, done));
                Self {
                    shared,
                    socket_path: path,
                    shutdown,
                    owned: true,
                    accept: Mutex::new(Some(accept)),
                }
            }
            Err(_) => {
                // No listener (permissions, a second window that raced us):
                // the object still works, the socket just is not ours.
                Self { shared, socket_path: path, shutdown, owned: false, accept: Mutex::new(None) }
            }
        }
    }

    /// Where this service listens (or would, when another window owns it):
    /// what a relay beside the app is pointed at.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Trust `id` (a session the app opened) for `project_root` (D53).
    /// Re-registering moves the session to the new root.
    pub fn register_session(&self, id: &str, project_root: PathBuf) {
        self.shared.sessions.lock().expect("session registry").insert(id.to_owned(), project_root);
    }

    /// What the harness runs on the UI thread when the agent starts
    /// something (open the dock, unfocused). Test hooks observe it instead.
    ///
    /// The hook fires inside [`drain`][TerminalService::drain], inside the
    /// draining entity's update: it must defer entity work with `cx.defer`
    /// rather than updating the draining entity inline (see [`ActivityHook`]).
    pub fn set_activity_hook(&self, hook: impl Fn(&mut App) + Send + Sync + 'static) {
        *self.shared.activity.lock().expect("activity hook") = Some(Box::new(hook));
    }

    /// Trust `state` as `session`'s webview for `browser_*` tools. Called
    /// on the UI thread when the pane creates (or reuses) the session's
    /// webview; re-registering moves the session to the new state.
    pub fn register_browser(&self, session: &str, state: Entity<WebviewState>) {
        self.shared.browsers.lock().expect("browser registry").insert(session.to_owned(), state);
    }

    /// Remember that `from` (a provider open's spawn-time request id) is now
    /// served as `to` (the lane's thread id): later tool calls naming `from`
    /// resolve to `to`. Called on the UI thread when the lane's thread id
    /// lands. Idempotent; `from == to` stores nothing.
    pub fn alias_session(&self, from: &str, to: &str) {
        if from == to {
            return;
        }
        self.shared
            .session_aliases
            .lock()
            .expect("session aliases")
            .insert(from.to_owned(), to.to_owned());
    }

    /// The lane id `session` now answers as: one alias chain, cycle-guarded.
    /// Unaliased ids answer as themselves, so this is safe to call blindly.
    fn resolve_session(&self, session: &str) -> String {
        let aliases = self.shared.session_aliases.lock().expect("session aliases");
        let mut current = session.to_owned();
        for _ in 0..8 {
            match aliases.get(&current) {
                Some(next) if next != &current => current = next.clone(),
                _ => break,
            }
        }
        current
    }

    /// What the harness runs on the UI thread when the agent opens a URL
    /// (open the Browser pane for that session). Test hooks observe it
    /// instead. Same re-entrancy rule as
    /// [`set_activity_hook`][TerminalService::set_activity_hook]: the hook
    /// fires inside [`drain`][TerminalService::drain], so it must defer
    /// entity work with `cx.defer`.
    pub fn set_browser_open_hook(&self, hook: impl Fn(&mut App, String) + Send + Sync + 'static) {
        *self.shared.browser_open.lock().expect("browser open hook") = Some(Box::new(hook));
    }

    /// How long a `browser_*` evaluation waits for the page. Tests shorten
    /// it so the timeout path does not take [`BROWSER_TIMEOUT`].
    #[cfg(test)]
    pub(crate) fn set_browser_timeout(&self, timeout: Duration) {
        *self.shared.browser_timeout.lock().expect("browser timeout") = timeout;
    }

    /// Run every queued request and poll every pending run. Call on the UI
    /// thread only — everything here touches [`TerminalHost`] or a
    /// [`WebviewState`].
    pub fn drain(&self, cx: &mut App) {
        let jobs: Vec<Job> = self.shared.queue.lock().expect("job queue").drain(..).collect();
        for job in jobs {
            self.execute(job, cx);
        }
        self.poll_runs(cx);
        self.poll_marks(cx);
        self.poll_browser(cx);
    }

    /// How many runs are still waiting on their blocks. Tests read this;
    /// the app never needs it.
    #[cfg(test)]
    pub(crate) fn pending_runs(&self) -> usize {
        self.shared.pending.lock().expect("pending runs").len()
    }

    fn execute(&self, job: Job, cx: &mut App) {
        // A provider lane outgrows its spawn-time request id (Codex serves
        // the server-minted thread id from then on): route the call under
        // the id the lane answers as now. Terminal tools key tabs by project
        // root so either id would find the same tabs; resolving keeps the
        // recorded origin on the lane's current id, like the browser tools.
        let session = self.resolve_session(&job.session);
        let root = match self.shared.sessions.lock().expect("session registry").get(&session) {
            Some(root) => root.clone(),
            None => {
                send(&job.reply, &job.id, false, json!({"error": format!("unknown session: no session the app opened is named {}", job.session)}));
                return;
            }
        };
        // `terminal_run` sends its own replies: refusals and `wait: none`
        // answer at once, while `wait: exit` queues and answers later from
        // `poll_runs` — its reply channel rides the pending run, not this.
        if job.tool == "terminal_run" {
            self.run(&root, Some(session.clone()), &job.params, &job.id, job.reply, cx);
            return;
        }
        // `browser_*` likewise sends its own replies: refusals answer at
        // once, while navigations and evaluations queue for
        // `poll_browser` — their reply channels ride the pending item.
        if BROWSER_TOOL_NAMES.contains(&job.tool.as_str()) {
            self.browser_tool(&session, &job.tool, &job.params, &job.id, job.reply, cx);
            return;
        }
        let outcome: Result<Value, String> = match job.tool.as_str() {
            "terminal_list" => self.list(&root, cx),
            "terminal_open" => self.open(&root, Some(session.clone()), &job.params, cx),
            "terminal_read" => self.read(&root, &job.params, cx),
            "terminal_screen" => self.screen(&root, &job.params, cx),
            "terminal_send" => self.send(&root, &job.params, cx),
            "terminal_close" => self.close(&root, &job.params, cx),
            other => {
                let mut names = TOOL_NAMES.to_vec();
                names.extend(BROWSER_TOOL_NAMES);
                Err(format!("unknown tool: {other} ({})", names.join(", ")))
            }
        };
        match outcome {
            Ok(result) => send(&job.reply, &job.id, true, result),
            Err(error) => send(&job.reply, &job.id, false, json!({"error": error})),
        }
    }
}

impl Drop for TerminalService {
    fn drop(&mut self) {
        // Quit removes the socket — but only when this window bound it. A
        // second window's service never owned the name, so it leaves the
        // first window's socket alone. Joining the listener first makes the
        // removal deterministic: no still-bound socket can answer a probe
        // and no fresh bind after a crash can be mistaken for ours.
        self.shutdown.store(true, Ordering::Release);
        if let Some(accept) = self.accept.lock().expect("listener thread").take() {
            let _ = accept.join();
        }
        if self.owned {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}

fn send(reply: &mpsc::Sender<String>, id: &Value, ok: bool, payload: Value) {
    let mut envelope = serde_json::Map::with_capacity(3);
    envelope.insert("id".to_owned(), id.clone());
    envelope.insert("ok".to_owned(), Value::Bool(ok));
    envelope.insert(if ok { "result" } else { "error" }.to_owned(), payload);
    let _ = reply.send(serde_json::to_string(&Value::Object(envelope)).expect("reply serializes"));
}

fn accept_loop(listener: UnixListener, shared: Arc<Shared>, shutdown: Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit the listener's nonblocking mode
                // on some platforms: without this, any reply past the
                // kernel buffer (~8 KB — every large `terminal_read`) fails
                // mid-write and drops the connection.
                if stream.set_nonblocking(false).is_err() {
                    continue;
                }
                let worker = shared.clone();
                std::thread::spawn(move || serve_conn(stream, worker));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(_) => {
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

fn serve_conn(stream: UnixStream, shared: Arc<Shared>) {
    let mut reader = BufReader::new(stream.try_clone().expect("socket clones"));
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let (id, session, tool, params) = match serde_json::from_str::<Value>(&line) {
            Ok(Value::Object(mut map)) => {
                let id = map.remove("id").unwrap_or(Value::Null);
                let session = map.remove("session").and_then(|v| v.as_str().map(str::to_owned));
                let tool = map.remove("tool").and_then(|v| v.as_str().map(str::to_owned));
                let params = map.remove("params").unwrap_or_else(|| json!({}));
                match (session, tool) {
                    (Some(session), Some(tool)) => (id, session, tool, params),
                    _ => {
                        let _ = write_reply(
                            reader.get_mut(),
                            &id,
                            false,
                            json!({"error": "malformed request: need {id, session, tool, params}"}),
                        );
                        continue;
                    }
                }
            }
            _ => {
                let _ = write_reply(
                    reader.get_mut(),
                    &Value::Null,
                    false,
                    json!({"error": "malformed request: need {id, session, tool, params}"}),
                );
                continue;
            }
        };
        let (tx, rx) = mpsc::channel();
        shared.queue.lock().expect("job queue").push_back(Job { id: id.clone(), session, tool, params, reply: tx });
        // The socket thread waits; the UI thread answers in `drain`. A
        // dead UI (quit mid-request) drops the sender and unblocks this.
        // The reply is already the full `{id, ok, result | error}` line,
        // built once in `send` so ids echo exactly.
        let reply = rx.recv().unwrap_or_else(|_| {
            serde_json::to_string(&json!({"id": id, "ok": false, "error": {"error": UNAVAILABLE}}))
                .expect("reply serializes")
        });
        let stream = reader.get_mut();
        if stream.write_all(reply.as_bytes()).is_err() || stream.write_all(b"\n").is_err() {
            return;
        }
        if stream.flush().is_err() {
            return;
        }
    }
}

/// Write one malformed-request reply line.
fn write_reply(stream: &mut UnixStream, id: &Value, ok: bool, payload: Value) -> std::io::Result<()> {
    let mut envelope = serde_json::Map::with_capacity(3);
    envelope.insert("id".to_owned(), id.clone());
    envelope.insert("ok".to_owned(), Value::Bool(ok));
    envelope.insert(if ok { "result" } else { "error" }.to_owned(), payload);
    let line = serde_json::to_string(&Value::Object(envelope)).expect("envelope serializes");
    stream.write_all(line.as_bytes())?;
    stream.write_all(b"\n")?;
    stream.flush()
}

/// The seven tools. Each runs on the UI thread, inside `drain`, and reads
/// only the calling session's project: `root` is the registered root from
/// [`TerminalService::register_session`].
impl TerminalService {
    fn host(&self) -> &Entity<TerminalHost> {
        &self.shared.host
    }

    fn list(&self, root: &Path, cx: &mut App) -> Result<Value, String> {
        let tabs = self.host().read(cx).tabs_for(root).into_iter().map(|tab| {
            let session = tab.session.read(cx);
            let blocks = session.blocks();
            let last = blocks.len().checked_sub(1).map(|i| format!("{}:{i}", tab.id));
            json!({
                "tab": tab.id,
                "title": tab.title,
                "cwd": tab.cwd.to_string_lossy(),
                "owner": match tab.owner {
                    TabOwner::User => "user",
                    // The contract names the agent `muse` (§4); the host's
                    // enum stays product-free, the label is the wire's.
                    TabOwner::Agent => "muse",
                },
                "busy": session.alt_screen() || blocks.iter().any(|block| block.running()),
                "alt_screen": session.alt_screen(),
                "last_block": last,
            })
        });
        Ok(json!({"tabs": tabs.collect::<Vec<_>>()}))
    }

    fn open(
        &self,
        root: &Path,
        origin: Option<String>,
        params: &Value,
        cx: &mut App,
    ) -> Result<Value, String> {
        // Any existing directory is accepted, deliberately: the agent runs
        // with the user's own privileges, so a tab starting elsewhere is no
        // wider than the person opening it there themselves — and a narrower
        // rule would add no boundary anyway, since any command can `cd`.
        // The tool description says as much.
        let cwd = match params.get("cwd").and_then(Value::as_str) {
            Some(dir) => {
                let path = PathBuf::from(dir);
                if !path.is_dir() {
                    return Err(format!("cwd does not exist: {dir}"));
                }
                path
            }
            None => root.to_owned(),
        };
        let title = params
            .get("title")
            .and_then(Value::as_str)
            .filter(|title| !title.trim().is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| "shell".to_owned());
        let id = self.host().update(cx, |host, cx| {
            host.open_with_cwd(root, &cwd, title, TabOwner::Agent, origin, cx)
        });
        Ok(json!({"tab": id}))
    }

    /// Run `command` per D43 and answer on `reply`, unless `wait: exit`
    /// queues the run for [`poll_runs`][TerminalService::poll_runs].
    /// Every arm sends exactly one reply: refusals and `wait: none` here,
    /// `wait: exit` later.
    fn run(
        &self,
        root: &Path,
        origin: Option<String>,
        params: &Value,
        job_id: &Value,
        reply: mpsc::Sender<String>,
        cx: &mut App,
    ) {
        let fail = |message: String| send(&reply, job_id, false, json!({"error": message}));
        let command = params.get("command").and_then(Value::as_str).unwrap_or("").to_owned();
        if command.trim().is_empty() {
            fail("command is empty: there is nothing to run".to_owned());
            return;
        }
        let tab_arg = params.get("tab").and_then(Value::as_str).unwrap_or("auto");
        let wait = params.get("wait").and_then(Value::as_str).unwrap_or("exit");
        if wait != "exit" && wait != "none" {
            fail(format!("wait must be exit|none, not {wait}"));
            return;
        }
        let timeout_ms = params
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .unwrap_or(RUN_DEFAULT_TIMEOUT_MS)
            .min(RUN_MAX_TIMEOUT_MS);

        // Resolve the tab: `auto` is D43 (the project's idle active tab,
        // else a fresh agent tab), `new` always opens one, an id names one.
        let new_tab = |host: &mut TerminalHost, cx: &mut gpui::Context<TerminalHost>| {
            host.open(root, title_from_command(&command), TabOwner::Agent, origin.clone(), cx)
        };
        let id = match tab_arg {
            "new" => self.host().update(cx, |host, cx| new_tab(host, cx)),
            "auto" => match self.host().read(cx).pick(cx, root, true) {
                super::Pick::Existing(id) => id,
                super::Pick::New => self.host().update(cx, |host, cx| new_tab(host, cx)),
            },
            named => {
                let known = self.host().read(cx).get(named).is_some();
                if !known {
                    fail(format!("unknown tab: {named}"));
                    return;
                }
                let same_project =
                    self.host().read(cx).get(named).is_some_and(|tab| tab.project_root == root);
                if !same_project {
                    fail(format!("tab {named} belongs to another project"));
                    return;
                }
                // D43's sharing rule: the agent may run in the person's tab
                // only when that tab is idle and not in alt-screen — exactly
                // `!busy()`, which covers a running block and the alternate
                // screen alike. A busy user-owned tab is refused by id,
                // naming the owner and what runs there; `auto` never picks
                // a busy tab ([`pick_tab`]) and opens an agent-owned tab
                // instead. Agent-owned tabs take the same idle rule: nobody
                // pastes into a running command.
                if self.host().read(cx).busy(cx, named) {
                    let running = self
                        .host()
                        .read(cx)
                        .running_command(cx, named)
                        .unwrap_or_else(|| "an interactive program".to_owned());
                    if self.host().read(cx).get(named).is_some_and(|tab| tab.owner == TabOwner::User) {
                        fail(format!(
                            "tab {named} is owned by the user and busy (running: {running}); run with tab auto or new instead"
                        ));
                    } else {
                        fail(format!("tab {named} is busy (running: {running})"));
                    }
                    return;
                }
                named.to_owned()
            }
        };

        let before = self
            .host()
            .read(cx)
            .get(&id)
            .map(|tab| tab.session.read(cx).blocks().len())
            .unwrap_or(0);
        let entity = self.host().read(cx).get(&id).map(|tab| tab.session.clone());
        if let Some(entity) = entity {
            entity.update(cx, |session, _| {
                session.paste(&command);
                session.write(b"\r");
            });
        }
        // The person watches the agent work: the dock opens, unfocused.
        if let Some(hook) = self.shared.activity.lock().expect("activity hook").as_ref() {
            hook(cx);
        }

        let cursor = self
            .host()
            .read(cx)
            .get(&id)
            .map(|tab| tab.session.read(cx).tail_cursor().line)
            .unwrap_or(TextCursor::start().line);
        if wait == "none" {
            send(
                &reply,
                job_id,
                true,
                json!({
                    "tab": id,
                    "block": format!("{id}:{before}"),
                    "status": "running",
                    "duration_ms": 0,
                    "output": "",
                    "truncated_bytes": 0,
                    "cursor": cursor,
                }),
            );
            // The block forms after this reply; watch it so it still earns
            // the agent mark.
            self.shared.marks.lock().expect("mark watches").push(MarkWatch { tab: id, before });
            return;
        }
        self.shared.pending.lock().expect("pending runs").push(PendingRun {
            id: job_id.clone(),
            tab: id,
            before,
            started: Instant::now(),
            deadline: Instant::now() + Duration::from_millis(timeout_ms),
            reply,
        });
    }

    /// Poll every run `wait: exit` left behind: pump the tab once, stamp
    /// new blocks agent-owned, and answer when the block closes or the
    /// deadline passes. `timeout` stops waiting, never the command.
    fn poll_runs(&self, cx: &mut App) {
        let mut pending = self.shared.pending.lock().expect("pending runs");
        let mut done: Vec<usize> = Vec::new();
        for (index, run) in pending.iter().enumerate() {
            let Some(entity) =
                self.host().read(cx).get(&run.tab).map(|tab| tab.session.clone())
            else {
                // The tab died mid-run: say so rather than hanging the tool.
                send(&run.reply, &run.id, false, json!({"error": format!("tab {} was closed", run.tab)}));
                done.push(index);
                continue;
            };
            entity.read(cx).pump();
            let blocks = entity.read(cx).blocks();
            // New blocks are the agent's: upgrade their authors now that
            // they exist (the assembler leaves every block human).
            stamp_agent(&entity, cx, run.before, blocks.len());
            let finished = blocks.get(run.before).is_some_and(|block| !block.running());
            if finished {
                let block = &blocks[run.before];
                let (output, truncated_bytes) =
                    cap_bytes(&strip_ansi(&block_text_of(cx, self.host(), &run.tab, run.before)), RUN_OUTPUT_CAP);
                let duration_ms = block
                    .ended
                    .map(|ended| ended.saturating_duration_since(block.started).as_millis() as u64)
                    .unwrap_or(0);
                let cursor = tail_of(cx, self.host(), &run.tab);
                send(
                    &run.reply,
                    &run.id,
                    true,
                    json!({
                        "tab": run.tab,
                        "block": format!("{}:{}", run.tab, run.before),
                        "status": "exited",
                        "exit_code": block.exit,
                        "duration_ms": duration_ms,
                        "output": output,
                        "truncated_bytes": truncated_bytes,
                        "cursor": cursor,
                    }),
                );
                done.push(index);
            } else if Instant::now() >= run.deadline {
                let output = blocks
                    .get(run.before)
                    .map(|_| strip_ansi(&block_text_of(cx, self.host(), &run.tab, run.before)))
                    .unwrap_or_default();
                let (output, truncated_bytes) = cap_bytes(&output, RUN_OUTPUT_CAP);
                send(
                    &run.reply,
                    &run.id,
                    true,
                    json!({
                        "tab": run.tab,
                        "block": format!("{}:{}", run.tab, run.before),
                        "status": "timeout",
                        "duration_ms": run.started.elapsed().as_millis() as u64,
                        "output": output,
                        "truncated_bytes": truncated_bytes,
                        "cursor": tail_of(cx, self.host(), &run.tab),
                    }),
                );
                // `timeout` stops waiting, never the command: watch the
                // block so its eventual close still earns the agent mark.
                self.shared.marks.lock().expect("mark watches").push(MarkWatch {
                    tab: run.tab.clone(),
                    before: run.before,
                });
                done.push(index);
            }
        }
        for index in done.into_iter().rev() {
            pending.remove(index);
        }
    }

    /// Pump and stamp what answered-but-unwaited runs left behind: the tab
    /// is pumped once (nothing else drives it now that no run waits on it)
    /// and every block since `before` becomes agent-owned. A watch retires
    /// when its block closes (later blocks are someone else's) or when the
    /// tab goes away.
    fn poll_marks(&self, cx: &mut App) {
        let mut marks = self.shared.marks.lock().expect("mark watches");
        let mut done: Vec<usize> = Vec::new();
        for (index, watch) in marks.iter().enumerate() {
            let Some(entity) =
                self.host().read(cx).get(&watch.tab).map(|tab| tab.session.clone())
            else {
                done.push(index);
                continue;
            };
            entity.read(cx).pump();
            let blocks = entity.read(cx).blocks();
            stamp_agent(&entity, cx, watch.before, blocks.len());
            if blocks.len() <= watch.before || blocks[watch.before].running() {
                continue;
            }
            done.push(index);
        }
        for index in done.into_iter().rev() {
            marks.remove(index);
        }
    }

    fn read(&self, root: &Path, params: &Value, cx: &mut App) -> Result<Value, String> {
        let tab_arg = params.get("tab").and_then(Value::as_str).ok_or("terminal_read needs tab")?;
        let tab = self.tab_in_project(root, tab_arg, cx)?;
        let max_bytes =
            params.get("max_bytes").and_then(Value::as_u64).unwrap_or(READ_DEFAULT_MAX as u64).min(READ_MAX as u64)
                as usize;
        let session = self.host().read(cx).get(&tab).expect("checked").session.clone();
        let running = {
            let view = session.read(cx);
            view.alt_screen() || view.blocks().iter().any(|block| block.running())
        };
        if let Some(block_arg) = params.get("block").and_then(Value::as_str) {
            let index = self.block_index(&tab, block_arg)?;
            let view = session.read(cx);
            let blocks = view.blocks();
            let block = blocks.get(index).ok_or_else(|| format!("unknown block: {block_arg}"))?;
            let (output, truncated_bytes) =
                cap_bytes(&strip_ansi(&view.block_text(index).unwrap_or_default()), max_bytes);
            return Ok(json!({
                "output": output,
                "truncated_bytes": truncated_bytes,
                "cursor": view.tail_cursor().line,
                "running": block.running(),
            }));
        }
        let since = params
            .get("since")
            .and_then(Value::as_i64)
            .map(|line| TextCursor { line: line.clamp(i32::MIN as i64, i32::MAX as i64) as i32 })
            .unwrap_or_else(TextCursor::start);
        let view = session.read(cx);
        let (output, truncated_bytes) = cap_bytes(&strip_ansi(&view.range_text(since)), max_bytes);
        Ok(json!({
            "output": output,
            "truncated_bytes": truncated_bytes,
            "cursor": view.tail_cursor().line,
            "running": running,
        }))
    }

    fn screen(&self, root: &Path, params: &Value, cx: &mut App) -> Result<Value, String> {
        let tab_arg = params.get("tab").and_then(Value::as_str).ok_or("terminal_screen needs tab")?;
        let tab = self.tab_in_project(root, tab_arg, cx)?;
        let session = self.host().read(cx).get(&tab).expect("checked").session.clone();
        let view = session.read(cx);
        let (row, col) = view.with_term(|term| {
            let point = term.grid().cursor.point;
            (point.line.0 + term.grid().display_offset() as i32, point.column.0)
        });
        let (cols, rows) = view.cells();
        Ok(json!({
            "lines": strip_ansi(&view.screen_text()),
            "cursor": {"row": row, "col": col},
            "alt_screen": view.alt_screen(),
            "rows": rows,
            "cols": cols,
        }))
    }

    fn send(&self, root: &Path, params: &Value, cx: &mut App) -> Result<Value, String> {
        let tab_arg = params.get("tab").and_then(Value::as_str).ok_or("terminal_send needs tab")?;
        let tab = self.tab_in_project(root, tab_arg, cx)?;
        // The person's tab is theirs: the agent may only answer a prompt of
        // a command it ran there — the tab's currently running block, when
        // that block is agent-authored — and never type into the person's
        // own ssh/less/password prompt. Tabs the agent opened take anything.
        let owner = self.host().read(cx).get(&tab).expect("checked").owner;
        if owner == TabOwner::User {
            let session = self.host().read(cx).get(&tab).expect("checked").session.clone();
            let agent_running = session
                .read(cx)
                .blocks()
                .iter()
                .find(|block| block.running())
                .is_some_and(|block| block.author == BlockAuthor::Agent);
            if !agent_running {
                return Err(format!(
                    "tab {tab} is owned by the user and is not running a command the agent started; terminal_send reaches only a prompt of a command the agent ran"
                ));
            }
        }
        let text = params.get("text").and_then(Value::as_str).unwrap_or("");
        let keys = match params.get("keys") {
            None => Vec::new(),
            Some(Value::String(key)) => vec![key.clone()],
            Some(Value::Array(keys)) => keys
                .iter()
                .map(|key| {
                    key.as_str().map(str::to_owned).ok_or_else(|| "keys must be key names".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err("keys must be a key name or a list of key names".to_owned()),
        };
        if text.is_empty() && keys.is_empty() {
            return Err("terminal_send needs text or keys".to_owned());
        }
        let mut key_bytes: Vec<Vec<u8>> = Vec::with_capacity(keys.len());
        for key in &keys {
            key_bytes.push(key_bytes_for(key)?);
        }
        let session = self.host().read(cx).get(&tab).expect("checked").session.clone();
        session.update(cx, |session, _| {
            if !text.is_empty() {
                session.paste(text);
            }
            for bytes in &key_bytes {
                session.write(bytes);
            }
        });
        Ok(json!({"ok": true}))
    }

    fn close(&self, root: &Path, params: &Value, cx: &mut App) -> Result<Value, String> {
        let tab_arg = params.get("tab").and_then(Value::as_str).ok_or("terminal_close needs tab")?;
        let tab = self.tab_in_project(root, tab_arg, cx)?;
        let owner = self.host().read(cx).get(&tab).expect("checked").owner;
        if owner == TabOwner::User {
            return Err(format!(
                "tab {tab} is owned by the user; the agent may close only tabs it opened"
            ));
        }
        // Dropping a pending run's tab answers the run instead of hanging
        // it; `poll_runs` sends the closed-tab error on its next pass.
        self.host().update(cx, |host, _| host.close(&tab));
        Ok(json!({"ok": true}))
    }

    /// `tab_arg` names a tab of this project, else a refusal.
    fn tab_in_project(&self, root: &Path, tab_arg: &str, cx: &mut App) -> Result<String, String> {
        let known = self.host().read(cx).get(tab_arg).is_some();
        if !known {
            return Err(format!("unknown tab: {tab_arg}"));
        }
        let same = self.host().read(cx).get(tab_arg).is_some_and(|tab| tab.project_root == root);
        if !same {
            return Err(format!("tab {tab_arg} belongs to another project"));
        }
        Ok(tab_arg.to_owned())
    }

    /// `t1:7` or `7`, for a tab already resolved to `tab`: the block index.
    fn block_index(&self, tab: &str, block_arg: &str) -> Result<usize, String> {
        let index = block_arg.split(':').next_back().unwrap_or(block_arg);
        // A qualified id must qualify with this tab, not another's.
        match block_arg.split_once(':') {
            Some((head, _)) if head != tab => {
                return Err(format!("block {block_arg} is not in tab {tab}"));
            }
            _ => {}
        }
        index.parse::<usize>().map_err(|_| format!("bad block id: {block_arg}"))
    }
}

/// The six browser tools (Z7b). Each runs on the UI thread, inside `drain`,
/// and acts only on the calling session's own webview: `session` is the
/// registered id from [`TerminalService::register_session`], and the
/// webview is the harness's [`register_browser`][TerminalService::register_browser]
/// entry for that same id — never another session's.
impl TerminalService {
    /// Dispatch one `browser_*` job. Refusals (a bad URL, a missing
    /// argument, no webview yet) answer at once; navigations, evaluations
    /// and screenshots queue a [`PendingBrowser`] for [`poll_browser`].
    /// Every arm sends exactly one reply.
    fn browser_tool(
        &self,
        session: &str,
        tool: &str,
        params: &Value,
        job_id: &Value,
        reply: mpsc::Sender<String>,
        cx: &mut App,
    ) {
        // `execute` already resolved once; resolve again so a lane whose
        // thread id landed between queueing and this drain still reaches its
        // browser — and so the pane hook below sees the lane's current id.
        let session = self.resolve_session(session);
        let session = session.as_str();
        let timeout = *self.shared.browser_timeout.lock().expect("browser timeout");
        let pend = |reply: mpsc::Sender<String>, wait: BrowserWait| {
            self.shared.pending_browser.lock().expect("pending browser").push(PendingBrowser {
                id: job_id.clone(),
                session: session.to_owned(),
                deadline: Instant::now() + timeout,
                reply,
                wait,
            });
        };
        match tool {
            "browser_open" => {
                let url = params.get("url").and_then(Value::as_str).unwrap_or("").to_owned();
                if url.trim().is_empty() {
                    send(&reply, job_id, false, json!({"error": "browser_open needs url"}));
                    return;
                }
                if !browser_url_allowed(&url) {
                    send(
                        &reply,
                        job_id,
                        false,
                        json!({"error": format!(
                            "refused URL: {url} (browser_open takes http, https, file, or about:blank)"
                        )}),
                    );
                    return;
                }
                // The person watches the agent browse: the pane opens on
                // Browser, unfocused. Deferred like the dock hook — `drain`
                // runs inside a Harness update.
                if let Some(hook) = self.shared.browser_open.lock().expect("browser open hook").as_ref() {
                    hook(cx, session.to_owned());
                }
                pend(reply, BrowserWait::Open { url, navigated: false, before: None });
            }
            "browser_read" | "browser_links" | "browser_click" | "browser_type" => {
                let Some((js, render)) = self.browser_script(tool, params) else {
                    let want = match tool {
                        "browser_click" => "browser_click needs selector",
                        "browser_type" => "browser_type needs selector and text",
                        _ => "unreachable",
                    };
                    send(&reply, job_id, false, json!({"error": want}));
                    return;
                };
                if self.shared.browsers.lock().expect("browser registry").get(session).is_none() {
                    send(
                        &reply,
                        job_id,
                        false,
                        json!({"error": "no browser for this session yet; browser_open opens it"}),
                    );
                    return;
                }
                let request_id = self.shared.next_request.fetch_add(1, Ordering::Relaxed);
                pend(reply, BrowserWait::Eval { request_id, sent: false, js, render });
            }
            "browser_screenshot" => {
                if self.shared.browsers.lock().expect("browser registry").get(session).is_none() {
                    send(
                        &reply,
                        job_id,
                        false,
                        json!({"error": "no browser for this session yet; browser_open opens it"}),
                    );
                    return;
                }
                pend(reply, BrowserWait::Shot { captured: false });
            }
            // `execute` only routes names in `BROWSER_TOOL_NAMES`.
            _ => send(&reply, job_id, false, json!({"error": format!("unknown tool: {tool}")})),
        }
    }

    /// The evaluation script and answer shape for a read/click/type tool,
    /// or `None` when a required argument is missing.
    fn browser_script(&self, tool: &str, params: &Value) -> Option<(String, BrowserRender)> {
        match tool {
            "browser_read" => {
                let max_chars = params
                    .get("max_chars")
                    .and_then(Value::as_u64)
                    .unwrap_or(BROWSER_READ_DEFAULT_MAX as u64)
                    // Capped like `terminal_read` (review): a caller cannot pull a
                    // whole huge page through the socket into the model.
                    .min(READ_MAX as u64) as usize;
                Some((agent_js::page_text(max_chars), BrowserRender::Read))
            }
            "browser_links" => {
                let max = params
                    .get("max")
                    .and_then(Value::as_u64)
                    .unwrap_or(BROWSER_LINKS_DEFAULT_MAX as u64)
                    as usize;
                Some((agent_js::list_links(max), BrowserRender::Links { max }))
            }
            "browser_click" => {
                let selector = params.get("selector").and_then(Value::as_str)?;
                if selector.is_empty() {
                    return None;
                }
                Some((agent_js::click(selector), BrowserRender::Text))
            }
            "browser_type" => {
                let selector = params.get("selector").and_then(Value::as_str)?;
                let text = params.get("text").and_then(Value::as_str)?;
                if selector.is_empty() {
                    return None;
                }
                Some((agent_js::type_text(selector, text), BrowserRender::Text))
            }
            _ => None,
        }
    }

    /// Poll every `browser_*` call still waiting on its page: navigate once
    /// the session's webview exists, send each script once, and answer when
    /// the matching answer lands or the deadline passes. Never blocks: one
    /// pass over the queue per frame, like [`poll_runs`][TerminalService::poll_runs].
    fn poll_browser(&self, cx: &mut App) {
        let mut pending = self.shared.pending_browser.lock().expect("pending browser");
        let mut done: Vec<usize> = Vec::new();
        for (index, item) in pending.iter_mut().enumerate() {
            match &mut item.wait {
                BrowserWait::Open { url, navigated, before } => {
                    // Re-resolve every pass: the lane's thread id may have
                    // landed after this call queued under its request id.
                    let key = self.resolve_session(&item.session);
                    let view =
                        self.shared.browsers.lock().expect("browser registry").get(&key).cloned();
                    let Some(view) = view else {
                        // No webview yet: the pane has not created one for
                        // this session. Wait for the registration (or the
                        // deadline) rather than failing the open.
                        if Instant::now() >= item.deadline {
                            send(&item.reply, &item.id, false, json!({"error": open_timeout(url)}));
                            done.push(index);
                        }
                        continue;
                    };
                    if !*navigated {
                        // What the pane showed before this call: the old
                        // page's title survives until the new one lands, so
                        // only a change from here counts as this load.
                        *before = Some((view.read(cx).url().to_string(), view.read(cx).title().to_string()));
                        let url = url.clone();
                        view.update(cx, |state, _| state.navigate(&url));
                        *navigated = true;
                        // The navigation only starts the load: judge it on a
                        // later pass, once the page reported back (normalised
                        // URL, redirect, title) — never on this pass's
                        // pre-navigation title.
                        continue;
                    }
                    let current = view.read(cx).url().to_string();
                    let title = view.read(cx).title().to_string();
                    // The URL flips the moment navigation starts; the title only
                    // once the page is in (seen live: `"title": ""` every time).
                    // Answer when the title arrives, or after a short grace for a
                    // page that has none — with the page's FINAL url, never the
                    // requested one: WebKit normalises (`https://example.com`
                    // → `https://example.com/`) and follows redirects
                    // (wikipedia.org → www.wikipedia.org), so requiring the
                    // requested URL exactly never completes.
                    let timeout = *self.shared.browser_timeout.lock().expect("browser timeout");
                    let waited = timeout.saturating_sub(item.deadline.saturating_duration_since(Instant::now()));
                    // A title counts only once it differs from the page this
                    // call navigated away from; a page whose title matches
                    // the old one (or has none) answers after the grace.
                    let fresh_title = !title.is_empty() && before.as_ref().is_none_or(|(_, old)| *old != title);
                    let settled = fresh_title || waited >= OPEN_TITLE_GRACE.min(timeout / 2);
                    if settled {
                        send(&item.reply, &item.id, true, json!({"url": current, "title": title}));
                        done.push(index);
                    } else if Instant::now() >= item.deadline {
                        send(&item.reply, &item.id, false, json!({"error": open_timeout(url)}));
                        done.push(index);
                    }
                }
                BrowserWait::Eval { request_id, sent, js, render } => {
                    let request_id = *request_id;
                    let render = *render;
                    // Re-resolved every pass, like the open above: the lane's
                    // thread id may have landed after this call queued.
                    let key = self.resolve_session(&item.session);
                    // An answer that arrived with another evaluation's batch.
                    if let Some(answer) =
                        self.shared.eval_stash.lock().expect("eval stash").remove(&(key.clone(), request_id))
                    {
                        answer_browser_eval(&item.reply, &item.id, render, answer);
                        done.push(index);
                        continue;
                    }
                    let view =
                        self.shared.browsers.lock().expect("browser registry").get(&key).cloned();
                    let Some(view) = view else {
                        send(&item.reply, &item.id, false, json!({"error": BROWSER_NO_ANSWER}));
                        done.push(index);
                        continue;
                    };
                    if !*sent {
                        let js = js.clone();
                        view.update(cx, |state, _| state.eval_with_result(request_id, &js));
                        *sent = true;
                    }
                    let answers = view.update(cx, |state, _| state.take_eval_results());
                    let mut ours = None;
                    for (id, answer) in answers {
                        if id == request_id && ours.is_none() {
                            ours = Some(answer);
                        } else {
                            let mut stash = self.shared.eval_stash.lock().expect("eval stash");
                            // Bounded (review): an answer whose request already
                            // timed out is never collected, so the stash must not
                            // grow for the life of the process.
                            if stash.len() >= EVAL_STASH_MAX {
                                stash.clear();
                            }
                            stash.insert((key.clone(), id), answer);
                        }
                    }
                    if let Some(answer) = ours {
                        answer_browser_eval(&item.reply, &item.id, render, answer);
                        done.push(index);
                    } else if Instant::now() >= item.deadline {
                        send(&item.reply, &item.id, false, json!({"error": BROWSER_NO_ANSWER}));
                        done.push(index);
                    }
                }
                BrowserWait::Shot { captured } => {
                    let key = self.resolve_session(&item.session);
                    let view =
                        self.shared.browsers.lock().expect("browser registry").get(&key).cloned();
                    let Some(view) = view else {
                        send(&item.reply, &item.id, false, json!({"error": BROWSER_NO_ANSWER}));
                        done.push(index);
                        continue;
                    };
                    if !*captured {
                        view.update(cx, |state, _| state.capture());
                        *captured = true;
                    }
                    if let Some(bytes) = view.read(cx).screenshot().map(<[u8]>::to_vec) {
                        let png = base64::engine::general_purpose::STANDARD.encode(&bytes);
                        send(&item.reply, &item.id, true, json!({"image_base64": png}));
                        done.push(index);
                    } else if Instant::now() >= item.deadline {
                        send(&item.reply, &item.id, false, json!({"error": BROWSER_NO_ANSWER}));
                        done.push(index);
                    }
                }
            }
        }
        for index in done.into_iter().rev() {
            pending.remove(index);
        }
    }
}

/// What a `browser_open` for `url` reports when the page never settled:
/// the shared [`BROWSER_NO_ANSWER`] plus the URL and the note that the page
/// may still be loading — the deadline only stops waiting, never the load.
fn open_timeout(url: &str) -> String {
    format!("{BROWSER_NO_ANSWER} for {url} (the page may still be loading)")
}

/// Whether `browser_open` takes `url`: `http`, `https`, `file`, or
/// `about:blank` — anything else (`javascript:`, `data:`, custom schemes)
/// is refused before any webview ever sees it.
fn browser_url_allowed(url: &str) -> bool {
    if url == "about:blank" {
        return true;
    }
    let lower = url.to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("file://")
}

/// Answer one evaluation from its page's response: a thrown page error
/// (an unknown selector, for instance) is a tool error, and each answer
/// is shaped by its tool.
fn answer_browser_eval(
    reply: &mpsc::Sender<String>,
    id: &Value,
    render: BrowserRender,
    answer: Result<String, String>,
) {
    let text = match answer {
        Ok(text) => text,
        Err(page) => {
            send(reply, id, false, json!({"error": page}));
            return;
        }
    };
    let outcome: Result<Value, String> = match render {
        BrowserRender::Read => {
            serde_json::from_str(&text).map_err(|_| "the page answered unreadably".to_owned())
        }
        BrowserRender::Links { max } => match serde_json::from_str::<Vec<Value>>(&text) {
            Ok(links) => Ok(json!({"links": links.into_iter().take(max).collect::<Vec<_>>() })),
            Err(_) => Err("the page answered unreadably".to_owned()),
        },
        BrowserRender::Text => {
            let value: Value = serde_json::from_str(&text).unwrap_or(Value::String(text));
            Ok(json!({"text": value}))
        }
    };
    match outcome {
        Ok(result) => send(reply, id, true, result),
        Err(error) => send(reply, id, false, json!({"error": error})),
    }
}

/// Mark blocks `before..len` agent-owned: the assembler leaves every new
/// block human, and only the agent's runs stamp what they started.
fn stamp_agent(entity: &Entity<aui_terminal::TerminalSession>, cx: &mut App, before: usize, len: usize) {
    if len > before {
        entity.update(cx, |session, _| {
            let fresh = session.blocks().len().min(len);
            for i in before..fresh {
                session.set_block_author(i, BlockAuthor::Agent);
            }
        });
    }
}

/// This tab's tail cursor line, or the start cursor when the tab is gone.
fn tail_of(cx: &App, host: &Entity<TerminalHost>, tab: &str) -> i32 {
    host.read(cx).get(tab).map(|tab| tab.session.read(cx).tail_cursor().line).unwrap_or(TextCursor::start().line)
}

/// This tab's block text, or empty when the block is out of range.
fn block_text_of(cx: &App, host: &Entity<TerminalHost>, tab: &str, index: usize) -> String {
    host.read(cx)
        .get(tab)
        .and_then(|tab| tab.session.read(cx).block_text(index))
        .unwrap_or_default()
}

/// Head+tail cap: outputs longer than `cap` bytes keep the first and last
/// halves whole (cut on char boundaries) and report what fell out.
fn cap_bytes(text: &str, cap: usize) -> (String, usize) {
    let bytes = text.as_bytes();
    if bytes.len() <= cap {
        return (text.to_owned(), 0);
    }
    let head_len = cap / 2;
    let tail_len = cap - head_len;
    let mut head = head_len;
    while head > 0 && !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail_start = bytes.len() - tail_len;
    while tail_start < bytes.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    let out = format!("{}{}", &text[..head], &text[tail_start..]);
    // What fell out is the middle minus the boundary retreat: the bytes
    // the output no longer carries, not the nominal over-cap count.
    let truncated = bytes.len() - out.len();
    (out, truncated)
}

/// Strip ANSI/VT100 escape sequences (CSI, OSC, stray ESC): grid text
/// arrives ANSI-free already, so this is the belt beside the suspenders —
/// a program that smuggles bytes past the emulator never reaches the agent
/// styled.
fn strip_ansi(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != 0x1b {
            let start = i;
            while i < bytes.len() && bytes[i] != 0x1b {
                i += 1;
            }
            out.push_str(&text[start..i]);
            continue;
        }
        // An escape: CSI `ESC [ … final`, OSC `ESC ] … BEL`, or a two-byte
        // sequence. Anything unrecognised is dropped, never passed through.
        i += 1;
        if i < bytes.len() && bytes[i] == b'[' {
            i += 1;
            while i < bytes.len() && !matches!(bytes[i], 0x40..=0x7e) {
                i += 1;
            }
            i += 1;
        } else if i < bytes.len() && bytes[i] == b']' {
            i += 1;
            while i < bytes.len() && bytes[i] != 0x07 {
                // An ESC-terminated string ends `ESC \`; skip both.
                if bytes[i] == 0x1b && bytes.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    break;
                }
                i += 1;
            }
            if bytes.get(i) == Some(&0x07) {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
    out
}

/// The named keys `terminal_send` accepts, and the bytes each one is.
fn key_bytes_for(key: &str) -> Result<Vec<u8>, String> {
    match key {
        "enter" => Ok(b"\r".to_vec()),
        "tab" => Ok(b"\t".to_vec()),
        "esc" => Ok(b"\x1b".to_vec()),
        "up" => Ok(b"\x1b[A".to_vec()),
        "down" => Ok(b"\x1b[B".to_vec()),
        "left" => Ok(b"\x1b[D".to_vec()),
        "right" => Ok(b"\x1b[C".to_vec()),
        "backspace" => Ok(b"\x7f".to_vec()),
        "ctrl-c" => Ok(b"\x03".to_vec()),
        "ctrl-d" => Ok(b"\x04".to_vec()),
        "ctrl-z" => Ok(b"\x1a".to_vec()),
        "ctrl-l" => Ok(b"\x0c".to_vec()),
        other => Err(format!(
            "unknown key: {other} (enter, tab, esc, up, down, left, right, backspace, ctrl-c, ctrl-d, ctrl-z, ctrl-l)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicU64};
    use std::time::Duration;

    use aui_terminal::{ScriptChunk, TermEvent, TerminalBackend, TerminalSession};
    use aui_webview::FakeWebBackend;
    use gpui::AppContext as _;

    static NEXT_PID: AtomicU32 = AtomicU32::new(1_000_000);
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);

    /// A fresh support-dir stand-in per test: parallel tests never share a
    /// socket path, and the tree is removed afterwards.
    struct TmpDir {
        path: PathBuf,
    }

    impl TmpDir {
        fn new() -> Self {
            // Short on purpose: unix socket paths die past ~104 bytes
            // (SUN_LEN), and the default temp dir on this machine is already
            // most of that.
            let n = NEXT_PID.fetch_add(1, Ordering::Relaxed);
            let path = PathBuf::from(format!("/tmp/bt-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("tmp dir");
            Self { path }
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    struct Fixture {
        service: TerminalService,
        host: Entity<TerminalHost>,
        tmp: TmpDir,
        root: PathBuf,
        session: String,
    }

    fn fixture(cx: &mut gpui::TestAppContext, session: &str) -> Fixture {
        let tmp = TmpDir::new();
        let root = tmp.path.clone();
        let host = cx.new(|_| TerminalHost::new());
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let service = TerminalService::start_at(host.clone(), &tmp.path, pid);
        service.register_session(session, root.clone());
        Fixture { service, host, tmp, root, session: session.into() }
    }

    /// One line-delimited request over a real socket, draining the service
    /// on the UI thread until the reply arrives. The socket thread waits;
    /// the test pumps — the same division the app's 15 ms task performs.
    fn call_as(
        cx: &mut gpui::TestAppContext,
        fx: &Fixture,
        session: &str,
        tool: &str,
        params: Value,
    ) -> Value {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::to_string(&json!({
            "id": id,
            "session": session,
            "tool": tool,
            "params": params,
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(300))).expect("read timeout");
        stream.write_all(request.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        let mut reader = BufReader::new(stream);
        for _ in 0..400 {
            cx.update(|cx| fx.service.drain(cx));
            if let Some(reply) = try_recv(&mut reader, id) {
                return reply;
            }
        }
        panic!("{tool}: no reply after draining");
    }

    fn call(cx: &mut gpui::TestAppContext, fx: &Fixture, tool: &str, params: Value) -> Value {
        call_as(cx, fx, &fx.session.clone(), tool, params)
    }

    fn result(reply: &Value) -> Value {
        assert_eq!(reply.get("ok"), Some(&Value::Bool(true)), "tool succeeded: {reply}");
        reply.get("result").cloned().expect("success carries a result")
    }

    fn err_text(reply: &Value) -> String {
        assert_eq!(reply.get("ok"), Some(&Value::Bool(false)), "tool refused: {reply}");
        reply
            .pointer("/error/error")
            .and_then(Value::as_str)
            .expect("refusal names itself")
            .to_owned()
    }

    /// A scripted pty that answers writes: once the session pastes a command
    /// and sends Enter, later polls replay a full OSC 133 block — a running
    /// one first, the `D` close after `finish_after` running polls (never,
    /// when that is `usize::MAX`). Each running poll also replays `trickle`.
    #[derive(Clone, Copy)]
    enum Stage {
        Idle,
        Running { polls: usize },
        Done,
    }

    struct RespondBackend {
        nonce: String,
        seen: Vec<u8>,
        used: usize,
        stage: Stage,
        exit: i32,
        first_output: Vec<u8>,
        trickle: Vec<u8>,
        finish_after: usize,
    }

    impl RespondBackend {
        fn new(nonce: &str, exit: i32, first_output: Vec<u8>) -> Self {
            Self {
                nonce: nonce.into(),
                seen: Vec::new(),
                used: 0,
                stage: Stage::Idle,
                exit,
                first_output,
                trickle: Vec::new(),
                finish_after: 1,
            }
        }

        fn trickling(nonce: &str, first_output: Vec<u8>, trickle: Vec<u8>) -> Self {
            Self {
                nonce: nonce.into(),
                seen: Vec::new(),
                used: 0,
                stage: Stage::Idle,
                exit: 0,
                first_output,
                trickle,
                finish_after: usize::MAX,
            }
        }

        /// The command line the session just submitted: bytes since the last
        /// Enter, without bracketed-paste wrapping.
        fn take_command(&mut self) -> String {
            let end = self.seen[self.used..]
                .iter()
                .position(|b| *b == b'\r')
                .map(|i| self.used + i)
                .unwrap_or(self.seen.len());
            let raw = String::from_utf8_lossy(&self.seen[self.used..end]).into_owned();
            self.used = if end < self.seen.len() { end + 1 } else { end };
            raw.replace("\u{1b}[200~", "").replace("\u{1b}[201~", "").trim().to_owned()
        }

        fn open_chunk(&self, command: &str) -> Vec<u8> {
            let cmd = base64::engine::general_purpose::STANDARD.encode(command);
            // No prompt text: the block's output range then holds exactly
            // the program's bytes, which is what the cap tests compare.
            format!(
                "\x1b]133;A;k={n}\x07\x1b]133;C;k={n};cmd={cmd};enc=b64\x07\r\n{out}",
                n = self.nonce,
                out = String::from_utf8_lossy(&self.first_output),
            )
            .into_bytes()
        }

        fn done_chunk(&self) -> Vec<u8> {
            format!("\x1b]133;D;{e};k={n}\x07", e = self.exit, n = self.nonce).into_bytes()
        }
    }

    impl TerminalBackend for RespondBackend {
        fn spawn(&mut self, _shell: &str, _cwd: &Path) -> std::io::Result<()> {
            Ok(())
        }

        fn write(&mut self, bytes: &[u8]) {
            self.seen.extend_from_slice(bytes);
        }

        fn resize(&mut self, _cols: u16, _rows: u16) {}

        fn poll(&mut self) -> Vec<TermEvent> {
            match self.stage {
                Stage::Idle => {
                    if self.seen[self.used..].contains(&b'\r') {
                        let command = self.take_command();
                        self.stage = Stage::Running { polls: 0 };
                        vec![TermEvent::Output(self.open_chunk(&command))]
                    } else {
                        vec![]
                    }
                }
                Stage::Running { polls } => {
                    if polls >= self.finish_after {
                        self.stage = Stage::Done;
                        vec![TermEvent::Output(self.done_chunk())]
                    } else {
                        self.stage = Stage::Running { polls: polls + 1 };
                        if self.trickle.is_empty() {
                            vec![]
                        } else {
                            vec![TermEvent::Output(self.trickle.clone())]
                        }
                    }
                }
                Stage::Done => vec![],
            }
        }
    }

    /// A tab over a responding backend. The owner is the caller's: most
    /// service tabs are agent-owned, and the user-tab sharing tests need a
    /// user-owned one running an agent-started block.
    fn open_responding(
        cx: &mut gpui::TestAppContext,
        fx: &Fixture,
        title: &str,
        owner: TabOwner,
        backend: RespondBackend,
    ) -> String {
        let nonce = backend.nonce.clone();
        let root = fx.root.clone();
        let origin = Some(fx.session.clone());
        cx.update(|cx| {
            fx.host.update(cx, |host, cx| {
                let session =
                    TerminalSession::new(Box::new(backend), 100, 32).with_nonce(&nonce);
                host.open_session(&root, title.into(), owner, origin, session, cx)
            })
        })
    }

    /// A tab over a recording: writes vanish, so runs never complete — the
    /// timeout and refusal paths' route.
    fn open_recording(
        cx: &mut gpui::TestAppContext,
        fx: &Fixture,
        title: &str,
        owner: TabOwner,
        bytes: Vec<u8>,
        nonce: &str,
    ) -> String {
        let root = fx.root.clone();
        let origin = Some(fx.session.clone());
        cx.update(|cx| {
            fx.host.update(cx, |host, cx| {
                let script = vec![ScriptChunk { at: Duration::from_millis(0), bytes }];
                let id = host.open_fake(&root, title.into(), owner, origin, script, nonce, cx);
                host.drain(&id, cx);
                id
            })
        })
    }

    /// One scripted chunk whose block is still running: `C` arrived, `D`
    /// never did, the command riding the `C` payload.
    fn running_script(nonce: &str, command: &str) -> Vec<u8> {
        let cmd = base64::engine::general_purpose::STANDARD.encode(command);
        format!(
            "\x1b]133;A;k={nonce}\x07test % \x1b]133;C;k={nonce};cmd={cmd};enc=b64\x07\r\npartial output"
        )
        .into_bytes()
    }

    /// One request written, reply not yet read: for tests that drain by
    /// hand between the two (a `wait: exit` observed mid-flight).
    fn send_json(fx: &Fixture, session: &str, id: u64, tool: &str, params: Value) -> BufReader<UnixStream> {
        let request = serde_json::to_string(&json!({
            "id": id, "session": session, "tool": tool, "params": params,
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(300))).expect("read timeout");
        stream.write_all(request.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        BufReader::new(stream)
    }

    /// One non-blocking reply attempt: `None` when the service has not
    /// answered yet (it answers from `drain`, on the UI thread).
    fn try_recv(reader: &mut BufReader<UnixStream>, id: u64) -> Option<Value> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => panic!("server closed the connection"),
            Ok(_) => {
                if line.trim().is_empty() {
                    return None;
                }
                let reply: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("reply parses ({} bytes): {e}", line.len()));
                assert_eq!(reply.get("id"), Some(&json!(id)), "ids echo");
                Some(reply)
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                None
            }
            Err(e) => panic!("socket read failed: {e}"),
        }
    }

    #[gpui::test]
    fn the_socket_is_owner_only_and_dies_with_the_service(cx: &mut gpui::TestAppContext) {
        let tmp = TmpDir::new();
        // A crash-leftover file binds fine once removed.
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let stale = socket_path_for(&tmp.path, pid);
        std::fs::create_dir_all(stale.parent().expect("run dir")).expect("run dir");
        // A pre-existing run dir with loose modes is tightened, not kept.
        std::fs::set_permissions(
            stale.parent().expect("run dir"),
            std::fs::Permissions::from_mode(0o777),
        )
        .expect("loose run dir");
        std::fs::write(&stale, b"stale").expect("stale file");
        let host = cx.new(|_| TerminalHost::new());
        let service = TerminalService::start_at(host, &tmp.path, pid);
        assert_eq!(service.socket_path(), stale);
        let dir_mode = std::fs::metadata(stale.parent().expect("run dir"))
            .expect("run dir reads")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the run dir is owner-only");
        let sock_mode =
            std::fs::metadata(&stale).expect("socket reads").permissions().mode() & 0o777;
        assert_eq!(sock_mode, 0o600, "the socket is owner-only");
        assert!(UnixStream::connect(&stale).is_ok(), "the service answers");
        // A second window keeps its hands off the first window's name.
        let host2 = cx.new(|_| TerminalHost::new());
        let second = TerminalService::start_at(host2, &tmp.path, pid);
        drop(second);
        assert!(stale.exists(), "a guest drop removes nothing");
        drop(service);
        assert!(!stale.exists(), "quit removes the socket");
    }

    #[gpui::test]
    fn unknown_sessions_are_refused(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        let reply = call_as(cx, &fx, "ghost", "terminal_list", json!({}));
        assert!(err_text(&reply).contains("unknown session"), "refusal names it: {reply}");
    }

    #[gpui::test]
    fn open_and_list_report_the_contract_shape(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        assert_eq!(result(&call(cx, &fx, "terminal_list", json!({}))), json!({"tabs": []}));
        let subdir = fx.tmp.path.join("sub");
        std::fs::create_dir_all(&subdir).expect("subdir");
        let t1 = result(&call(cx, &fx, "terminal_open", json!({"title": "agent shell"})));
        assert_eq!(t1.get("tab"), Some(&json!("t1")));
        let t2 = result(
            &call(cx, &fx, "terminal_open", json!({"cwd": subdir.to_string_lossy(), "title": "down"})),
        );
        assert_eq!(t2.get("tab"), Some(&json!("t2")));
        let bad = call(cx, &fx, "terminal_open", json!({"cwd": "/no/such/dir-anywhere"}));
        assert!(err_text(&bad).contains("cwd does not exist"), "refusal: {bad}");
        let tabs = result(&call(cx, &fx, "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].get("owner"), Some(&json!("muse")));
        assert_eq!(tabs[0].get("cwd"), Some(&json!(fx.root.to_string_lossy())));
        assert_eq!(tabs[0].get("last_block"), Some(&Value::Null));
        assert_eq!(tabs[0].get("busy"), Some(&Value::Bool(false)));
        assert_eq!(tabs[1].get("cwd"), Some(&json!(subdir.to_string_lossy())));
        assert_eq!(tabs[1].get("owner"), Some(&json!("muse")));
    }

    #[gpui::test]
    fn run_wait_exit_returns_code_and_output(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        let hooks = Arc::new(AtomicU64::new(0));
        let seen = hooks.clone();
        fx.service.set_activity_hook(move |_| {
            seen.fetch_add(1, Ordering::Relaxed);
        });
        open_responding(
            cx,
            &fx,
            "agent",
            TabOwner::Agent,
            RespondBackend::new("exit-nonce", 3, b"hello from agent\r\nsecond line\r\n".to_vec()),
        );
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "echo hi", "tab": "t1", "wait": "exit"})),
        );
        assert_eq!(out.get("tab"), Some(&json!("t1")));
        assert_eq!(out.get("block"), Some(&json!("t1:0")));
        assert_eq!(out.get("status"), Some(&json!("exited")));
        assert_eq!(out.get("exit_code"), Some(&json!(3)));
        assert!(out.get("duration_ms").and_then(Value::as_u64).is_some(), "duration: {out}");
        let output = out.get("output").and_then(Value::as_str).expect("output text");
        assert!(output.contains("hello from agent") && output.contains("second line"), "output: {output}");
        assert_eq!(out.get("truncated_bytes"), Some(&json!(0)));
        assert!(out.get("cursor").and_then(Value::as_i64).is_some(), "cursor: {out}");
        assert_eq!(fx.service.pending_runs(), 0, "the run answered and retired");
        assert_eq!(hooks.load(Ordering::Relaxed), 1, "the dock opens for a run");
        cx.read(|cx| {
            let blocks = fx.host.read(cx).get("t1").expect("tab").session.read(cx).blocks();
            assert_eq!(blocks.len(), 1);
            assert!(!blocks[0].running());
            assert_eq!(blocks[0].author, BlockAuthor::Agent, "the run's block carries the agent mark");
            assert_eq!(blocks[0].command, "echo hi", "the command rides the C payload");
        });
        let tabs = result(&call(cx, &fx, "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs[0].get("last_block"), Some(&json!("t1:0")));
    }

    #[gpui::test]
    fn exit_waits_across_drains(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_responding(cx, &fx, "agent", TabOwner::Agent, RespondBackend::new("mid-nonce", 0, b"hi\r\n".to_vec()));
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut reader =
            send_json(&fx, &fx.session.clone(), id, "terminal_run", json!({"command": "echo hi", "tab": "t1"}));
        // The accept thread queues asynchronously (it polls every 50 ms):
        // drain until the run is in flight, then observe it mid-flight.
        for _ in 0..400 {
            cx.update(|cx| fx.service.drain(cx));
            if fx.service.pending_runs() == 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(fx.service.pending_runs(), 1, "the C arrived, the D has not");
        let mut reply = try_recv(&mut reader, id);
        for _ in 0..400 {
            if reply.is_some() {
                break;
            }
            cx.update(|cx| fx.service.drain(cx));
            reply = try_recv(&mut reader, id);
        }
        let reply = reply.expect("the D closes the run");
        assert_eq!(result(&reply).get("status"), Some(&json!("exited")));
        assert_eq!(fx.service.pending_runs(), 0);
    }

    #[gpui::test]
    fn wait_none_answers_running_and_marks_late(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_responding(cx, &fx, "agent", TabOwner::Agent, RespondBackend::new("late-nonce", 0, b"late output\r\n".to_vec()));
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "echo late", "tab": "t1", "wait": "none"})),
        );
        assert_eq!(out.get("status"), Some(&json!("running")));
        assert_eq!(out.get("block"), Some(&json!("t1:0")));
        assert_eq!(out.get("duration_ms"), Some(&json!(0)));
        assert_eq!(fx.service.pending_runs(), 0, "wait:none never pends");
        for _ in 0..6 {
            cx.update(|cx| fx.service.drain(cx));
        }
        cx.read(|cx| {
            let blocks = fx.host.read(cx).get("t1").expect("tab").session.read(cx).blocks();
            assert_eq!(blocks.len(), 1);
            assert!(!blocks[0].running(), "drains pumped the close");
            assert_eq!(blocks[0].author, BlockAuthor::Agent, "the late block is still marked");
        });
    }

    #[gpui::test]
    fn timeout_leaves_it_running_and_read_continues(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_responding(
            cx,
            &fx,
            "agent",
            TabOwner::Agent,
            RespondBackend::trickling("tick-nonce", b"partial\r\n".to_vec(), b"tick\r\n".to_vec()),
        );
        let out = result(
            &call(
                cx,
                &fx,
                "terminal_run",
                json!({"command": "sleep 300", "tab": "t1", "timeout_ms": 50}),
            ),
        );
        assert_eq!(out.get("status"), Some(&json!("timeout")));
        assert!(out.get("duration_ms").and_then(Value::as_u64).is_some(), "duration: {out}");
        let output = out.get("output").and_then(Value::as_str).expect("output text");
        assert!(output.contains("partial"), "what arrived so far: {output}");
        let cursor = out.get("cursor").and_then(Value::as_i64).expect("cursor");
        assert_eq!(fx.service.pending_runs(), 0, "timeout stops waiting");
        for _ in 0..5 {
            cx.update(|cx| fx.service.drain(cx));
        }
        let read = result(&call(cx, &fx, "terminal_read", json!({"tab": "t1", "since": cursor})));
        assert_eq!(read.get("running"), Some(&Value::Bool(true)), "the command runs on");
        let output = read.get("output").and_then(Value::as_str).expect("delta text");
        assert!(output.contains("tick"), "what arrived since: {output}");
        assert!(
            read.get("cursor").and_then(Value::as_i64).expect("cursor") > cursor,
            "the cursor advanced: {read}"
        );
        let block = result(&call(cx, &fx, "terminal_read", json!({"tab": "t1", "block": "t1:0"})));
        assert_eq!(block.get("running"), Some(&Value::Bool(true)));
        assert!(
            block.get("output").and_then(Value::as_str).expect("block text").contains("partial"),
            "block reads see the running block: {block}"
        );
    }

    #[gpui::test]
    fn auto_reuses_idle_and_opens_new_when_busy(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(cx, &fx, "user shell", TabOwner::User, b"test % ".to_vec(), "idle-nonce");
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "true", "tab": "auto", "wait": "none"})),
        );
        assert_eq!(out.get("tab"), Some(&json!("t1")), "D43: the idle active tab takes it");
        let busy =
            open_recording(cx, &fx, "busy shell", TabOwner::User, running_script("busy-nonce", "sleep 300"), "busy-nonce");
        assert_eq!(busy, "t2");
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "true", "tab": "auto", "wait": "none"})),
        );
        assert_eq!(out.get("tab"), Some(&json!("t3")), "D43: a busy tab never takes a command");
        let tabs = result(&call(cx, &fx, "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs[2].get("owner"), Some(&json!("muse")), "the fallback tab is the agent's");
        let refused = call(cx, &fx, "terminal_run", json!({"command": "echo hi", "tab": "t2"}));
        let message = err_text(&refused);
        assert!(
            message.contains("owned by the user") && message.contains("sleep 300"),
            "names it: {message}"
        );
    }

    #[gpui::test]
    fn run_into_an_idle_user_tab_succeeds(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(cx, &fx, "user shell", TabOwner::User, b"test % ".to_vec(), "idle-run-nonce");
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "true", "tab": "t1", "wait": "none"})),
        );
        assert_eq!(out.get("tab"), Some(&json!("t1")), "D43: an idle user tab takes the run");
    }

    #[gpui::test]
    fn run_refuses_an_altscreen_user_tab_and_auto_opens_an_agent_one(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(
            cx,
            &fx,
            "person pager",
            TabOwner::User,
            b"\x1b[?1049hless\rlines\r\n:".to_vec(),
            "less-nonce",
        );
        cx.read(|cx| {
            assert!(
                fx.host.read(cx).get("t1").expect("tab").session.read(cx).alt_screen(),
                "the script entered the alternate screen"
            );
        });
        let refused = call(cx, &fx, "terminal_run", json!({"command": "echo hi", "tab": "t1"}));
        let message = err_text(&refused);
        assert!(
            message.contains("owned by the user") && message.contains("busy"),
            "names it: {message}"
        );
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "true", "tab": "auto", "wait": "none"})),
        );
        assert_eq!(out.get("tab"), Some(&json!("t2")), "auto never takes an alt-screen tab");
        let tabs = result(&call(cx, &fx, "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs[1].get("owner"), Some(&json!("muse")), "the fallback tab is the agent's");
    }

    #[gpui::test]
    fn send_into_user_tabs_reaches_only_agent_started_prompts(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        // Idle: the person's own prompt — hands off.
        open_recording(cx, &fx, "idle shell", TabOwner::User, b"test % ".to_vec(), "idle-nonce");
        let refused = call(cx, &fx, "terminal_send", json!({"tab": "t1", "text": "x"}));
        assert!(err_text(&refused).contains("owned by the user"), "refusal: {refused}");
        // Busy with the person's own command: a human-authored running
        // block the agent must never type into.
        open_recording(
            cx,
            &fx,
            "person shell",
            TabOwner::User,
            running_script("person-nonce", "ssh prod"),
            "person-nonce",
        );
        let refused = call(cx, &fx, "terminal_send", json!({"tab": "t2", "text": "x"}));
        assert!(err_text(&refused).contains("owned by the user"), "refusal: {refused}");
        // Busy with a command the agent ran there: answering its prompt is
        // the one send a user tab takes.
        open_responding(
            cx,
            &fx,
            "shared shell",
            TabOwner::User,
            RespondBackend::trickling(
                "shared-nonce",
                b"partial\r\n".to_vec(),
                b"tick\r\n".to_vec(),
            ),
        );
        let started = result(
            &call(cx, &fx, "terminal_run", json!({"command": "sleep 300", "tab": "t3", "wait": "none"})),
        );
        assert_eq!(started.get("tab"), Some(&json!("t3")));
        assert_eq!(
            result(&call(cx, &fx, "terminal_send", json!({"tab": "t3", "text": "y", "keys": "enter"}))),
            json!({"ok": true})
        );
        // Agent-owned tabs stay unrestricted, even mid-command.
        open_responding(
            cx,
            &fx,
            "agent shell",
            TabOwner::Agent,
            RespondBackend::trickling("own-nonce", b"partial\r\n".to_vec(), Vec::new()),
        );
        let started = result(
            &call(cx, &fx, "terminal_run", json!({"command": "sleep 300", "tab": "t4", "wait": "none"})),
        );
        assert_eq!(started.get("tab"), Some(&json!("t4")));
        assert_eq!(
            result(&call(cx, &fx, "terminal_send", json!({"tab": "t4", "text": "z"}))),
            json!({"ok": true})
        );
    }

    #[gpui::test]
    fn close_is_refused_for_user_tabs(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(cx, &fx, "user shell", TabOwner::User, b"test % ".to_vec(), "close-nonce");
        let open = result(&call(cx, &fx, "terminal_open", json!({})));
        assert_eq!(open.get("tab"), Some(&json!("t2")));
        let refused = call(cx, &fx, "terminal_close", json!({"tab": "t1"}));
        assert!(err_text(&refused).contains("owned by the user"), "refusal: {refused}");
        let missing = call(cx, &fx, "terminal_close", json!({"tab": "t404"}));
        assert!(err_text(&missing).contains("unknown tab"), "refusal: {missing}");
        assert_eq!(result(&call(cx, &fx, "terminal_close", json!({"tab": "t2"}))), json!({"ok": true}));
        let tabs = result(&call(cx, &fx, "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs.len(), 1, "only the agent tab closed: {tabs:?}");
        assert_eq!(tabs[0].get("tab"), Some(&json!("t1")));
    }

    #[gpui::test]
    fn tabs_belong_to_the_registered_project(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        let other = fx.tmp.path.join("other");
        std::fs::create_dir_all(&other).expect("other root");
        fx.service.register_session("s2", other.clone());
        assert_eq!(
            result(&call_as(cx, &fx, "s2", "terminal_list", json!({}))),
            json!({"tabs": []}),
            "a session sees only its own project's tabs"
        );
        let open = result(&call_as(cx, &fx, "s2", "terminal_open", json!({})));
        assert_eq!(open.get("tab"), Some(&json!("t1")));
        let tabs = result(&call_as(cx, &fx, "s2", "terminal_list", json!({})));
        let tabs = tabs.get("tabs").and_then(Value::as_array).expect("tabs array");
        assert_eq!(tabs[0].get("cwd"), Some(&json!(other.to_string_lossy())));
        let refused = call_as(cx, &fx, "s2", "terminal_read", json!({"tab": "t9"}));
        assert!(err_text(&refused).contains("unknown tab"), "refusal: {refused}");
        let mine = result(&call(cx, &fx, "terminal_list", json!({})));
        assert_eq!(mine.get("tabs").and_then(Value::as_array).expect("tabs").len(), 0);
        let others = call(cx, &fx, "terminal_read", json!({"tab": "t1"}));
        assert!(
            err_text(&others).contains("belongs to another project"),
            "s1 cannot touch s2's tab: {others}"
        );
    }

    #[gpui::test]
    fn run_validates_its_inputs(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        let empty = call(cx, &fx, "terminal_run", json!({"command": "   "}));
        assert!(err_text(&empty).contains("command is empty"), "refusal: {empty}");
        let wait = call(cx, &fx, "terminal_run", json!({"command": "echo hi", "wait": "whenever"}));
        assert!(err_text(&wait).contains("wait must be"), "refusal: {wait}");
        let tab = call(cx, &fx, "terminal_run", json!({"command": "echo hi", "tab": "t404"}));
        assert!(err_text(&tab).contains("unknown tab"), "refusal: {tab}");
    }

    #[gpui::test]
    fn send_types_text_and_named_keys(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(cx, &fx, "agent shell", TabOwner::Agent, b"test % ".to_vec(), "send-nonce");
        assert_eq!(
            result(&call(cx, &fx, "terminal_send", json!({"tab": "t1", "text": "hello"}))),
            json!({"ok": true})
        );
        assert_eq!(
            result(&call(cx, &fx, "terminal_send", json!({"tab": "t1", "keys": "enter"}))),
            json!({"ok": true})
        );
        assert_eq!(
            result(&call(cx, &fx, "terminal_send", json!({"tab": "t1", "keys": ["ctrl-c", "ctrl-d"]}))),
            json!({"ok": true})
        );
        let neither = call(cx, &fx, "terminal_send", json!({"tab": "t1"}));
        assert!(err_text(&neither).contains("needs text or keys"), "refusal: {neither}");
        let bogus = call(cx, &fx, "terminal_send", json!({"tab": "t1", "keys": "bogus"}));
        assert!(err_text(&bogus).contains("unknown key"), "refusal: {bogus}");
        let missing = call(cx, &fx, "terminal_send", json!({"tab": "t404", "text": "hi"}));
        assert!(err_text(&missing).contains("unknown tab"), "refusal: {missing}");
    }

    #[gpui::test]
    fn screen_and_read_report_the_grid(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        open_recording(
            cx,
            &fx,
            "user shell",
            TabOwner::User,
            b"visible line\r\nsecond\r\ntest % ".to_vec(),
            "screen-nonce",
        );
        let screen = result(&call(cx, &fx, "terminal_screen", json!({"tab": "t1"})));
        assert!(
            screen.get("lines").and_then(Value::as_str).expect("lines").contains("visible line"),
            "screen: {screen}"
        );
        assert_eq!(screen.get("rows"), Some(&json!(32)));
        assert_eq!(screen.get("cols"), Some(&json!(100)));
        assert_eq!(screen.get("alt_screen"), Some(&Value::Bool(false)));
        assert!(screen.get("cursor").and_then(Value::as_object).is_some(), "cursor: {screen}");
        let missing = call(cx, &fx, "terminal_screen", json!({"tab": "t404"}));
        assert!(err_text(&missing).contains("unknown tab"), "refusal: {missing}");
        let read = result(&call(cx, &fx, "terminal_read", json!({"tab": "t1"})));
        assert!(
            read.get("output").and_then(Value::as_str).expect("output").contains("visible line"),
            "read: {read}"
        );
        assert_eq!(read.get("running"), Some(&Value::Bool(false)));
        assert!(read.get("cursor").and_then(Value::as_i64).is_some(), "cursor: {read}");
        let bad_block = call(cx, &fx, "terminal_read", json!({"tab": "t1", "block": "zzz"}));
        assert!(err_text(&bad_block).contains("bad block id"), "refusal: {bad_block}");
        let foreign = call(cx, &fx, "terminal_read", json!({"tab": "t1", "block": "t2:0"}));
        assert!(err_text(&foreign).contains("not in tab"), "refusal: {foreign}");
    }

    #[gpui::test]
    fn output_is_head_and_tail_capped(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        // Trailing newline so the `D` close lands on a fresh line, outside
        // the output range.
        let big = format!("{}{}\r\n", "Q".repeat(20_000), "Z".repeat(20_000));
        open_responding(cx, &fx, "agent", TabOwner::Agent, RespondBackend::new("cap-nonce", 0, big.into_bytes()));
        let out = result(
            &call(cx, &fx, "terminal_run", json!({"command": "firehose", "tab": "t1", "wait": "exit"})),
        );
        let output = out.get("output").and_then(Value::as_str).expect("output text");
        assert_eq!(output.len(), RUN_OUTPUT_CAP, "runs cap at 4 KB");
        // The grid wraps long lines, so wrap newlines fall out of the
        // comparison; what remains must be one Q run then one Z run, both
        // substantial — head+tail, not head-only.
        assert_head_tail(output, 1500, "runs cap head+tail");
        assert!(out.get("truncated_bytes").and_then(Value::as_u64).expect("count") > 0, "count: {out}");
        let read = result(
            &call(cx, &fx, "terminal_read", json!({"tab": "t1", "block": "t1:0", "max_bytes": 1_000_000})),
        );
        let output = read.get("output").and_then(Value::as_str).expect("output text");
        assert_eq!(output.len(), READ_MAX, "reads clamp at 32 KB");
        assert_head_tail(output, 12_000, "reads clamp head+tail");
        let small = result(
            &call(cx, &fx, "terminal_read", json!({"tab": "t1", "block": "t1:0", "max_bytes": 8})),
        );
        let output = small.get("output").and_then(Value::as_str).expect("output text");
        assert!(output.len() <= 8, "small caps hold: {small}");
        assert!(small.get("truncated_bytes").and_then(Value::as_u64).expect("count") > 0, "count: {small}");
    }

    #[gpui::test]
    fn malformed_requests_error_and_unknown_tools_name_themselves(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "s1");
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_secs(5))).expect("read timeout");
        stream.write_all(br#"{"id": 501}"#).expect("malformed writes");
        stream.write_all(b"\n").expect("malformed ends");
        stream.flush().expect("malformed flushes");
        let mut reader = BufReader::new(stream.try_clone().expect("socket clones"));
        let mut line = String::new();
        reader.read_line(&mut line).expect("malformed answered");
        let reply: Value = serde_json::from_str(&line).expect("reply parses");
        assert_eq!(reply.get("id"), Some(&json!(501)));
        assert_eq!(reply.get("ok"), Some(&Value::Bool(false)));
        drop(reader);
        drop(stream);
        // The server outlives a bad line: the next connection works.
        assert_eq!(result(&call(cx, &fx, "terminal_list", json!({}))), json!({"tabs": []}));
        let _ = id;
        let unknown = call(cx, &fx, "frobnicate", json!({}));
        let message = err_text(&unknown);
        assert!(message.contains("unknown tool") && message.contains("terminal_run"), "names: {message}");
    }

    /// Wrap newlines out, then one Q run into one Z run, both at least
    /// `each` long: the shape a head+tail cap leaves behind.
    fn assert_head_tail(output: &str, each: usize, what: &str) {
        let flat: String = output.chars().filter(|c| *c != '\n' && *c != '\r').collect();
        let tail = flat.trim_start_matches('Q');
        let head_len = flat.len() - tail.len();
        assert!(head_len >= each && tail.len() >= each, "{what}: {head_len} Q then {} Z", tail.len());
        assert!(tail.chars().all(|c| c == 'Z'), "{what}: the tail is all Z");
    }

    #[test]
    fn cap_bytes_keeps_head_and_tail() {
        let (out, trunc) = cap_bytes("abc", 4096);
        assert_eq!((out.as_str(), trunc), ("abc", 0));
        let text = format!("{}{}", "H".repeat(5000), "T".repeat(5000));
        let (out, trunc) = cap_bytes(&text, 4096);
        assert_eq!(out.len(), 4096);
        assert_eq!(trunc, 10_000 - 4096);
        assert!(out.starts_with("HHH") && out.ends_with("TTT"));
        // A cut landing mid-emoji retreats to the char boundary, and the
        // retreated byte counts as truncated too.
        let text = format!("{}◉{}", "a".repeat(2047), "b".repeat(3000));
        let (out, trunc) = cap_bytes(&text, 4096);
        assert_eq!(out.len(), 4095, "the split head byte falls out");
        assert_eq!(trunc, text.len() - out.len());
        assert!(out.starts_with("aaa") && out.ends_with("bbb"));
    }

    #[test]
    fn strip_ansi_drops_escapes_and_keeps_text() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m plain"), "red plain");
        assert_eq!(strip_ansi("\x1b]8;;https://example.com\x07link"), "link");
        assert_eq!(strip_ansi("a\x1bbc"), "ac");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn socket_path_for_names_the_run_socket() {
        assert_eq!(
            socket_path_for(Path::new("/sup"), 42),
            PathBuf::from("/sup/run/terminal-42.sock")
        );
    }

    #[test]
    fn the_contract_lists_seven_tools() {
        assert_eq!(
            TOOL_NAMES,
            [
                "terminal_list",
                "terminal_open",
                "terminal_run",
                "terminal_read",
                "terminal_screen",
                "terminal_send",
                "terminal_close"
            ]
        );
        assert_eq!((RUN_DEFAULT_TIMEOUT_MS, RUN_MAX_TIMEOUT_MS), (30_000, 600_000));
        assert_eq!((READ_DEFAULT_MAX, READ_MAX), (4_096, 32_768));
        assert_eq!(RUN_OUTPUT_CAP, 4_096);
    }

    #[test]
    fn send_keys_cover_the_contract_set() {
        for key in [
            "enter", "tab", "esc", "up", "down", "left", "right", "backspace", "ctrl-c", "ctrl-d",
            "ctrl-z", "ctrl-l",
        ] {
            assert!(key_bytes_for(key).is_ok(), "{key} has bytes");
        }
        assert!(key_bytes_for("bogus").is_err());
    }

    /// Offline `--no-connect` args for a Harness that never spawns a child:
    /// the same shape the session-lifecycle tests boot.
    fn harness_args(dir: &Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            bench_out: None,
            sidebar_fixture: None,
            no_project: false,
        }
    }

    /// A real Harness whose service owns the pid socket name. The name is
    /// process-wide, so when a parallel Harness test holds it ours is a
    /// guest and its requests would land on the other service: build until
    /// ours binds, waiting the other out. A guest drops without removing
    /// anything, so retries are cheap.
    fn harness_with_socket(
        vc: &mut gpui::VisualTestContext,
        dir: &Path,
    ) -> gpui::Entity<crate::app::Harness> {
        for _ in 0..600 {
            let harness = vc.update(|window, cx| {
                cx.new(|cx| {
                    crate::app::Harness::new(
                        harness_args(dir),
                        crate::shot::CaptureToken::default(),
                        window,
                        cx,
                    )
                })
            });
            let owned = vc.update(|_, cx| harness.read(cx).terminal_service.owned);
            if owned {
                return harness;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("no Harness owned the terminal socket after 60s");
    }

    /// `terminal_run` through the real Harness wiring: the Harness `new`
    /// builds, its own activity hook installed, the request over its real
    /// socket, and the drain inside a Harness update — exactly the pump
    /// task's shape. Before the hook deferred, this died re-entering the
    /// Harness (`cannot update baaz::app::Harness while it is already being
    /// updated`); the service tests never saw it because their hosts were
    /// standalone and their hooks never touched an entity.
    #[gpui::test]
    fn run_through_the_harness_pump_never_reenters(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let tmp = TmpDir::new();
        let root = tmp.path.clone();
        let vc = cx.add_empty_window();
        let harness = harness_with_socket(vc, &root);
        vc.update(|_, cx| {
            harness.update(cx, |harness, _| {
                harness.register_terminal_session("t1c", root.clone(), "muse");
            });
        });
        let host = vc.update(|_, cx| harness.read(cx).terminal_host.clone());
        let tab = vc.update(|_, cx| {
            host.update(cx, |host, cx| {
                host.open_fake(&root, "agent".into(), TabOwner::Agent, Some("t1c".into()), Vec::new(), "t1c-nonce", cx)
            })
        });
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::to_string(&json!({
            "id": id, "session": "t1c", "tool": "terminal_run",
            "params": {"command": "echo hi-from-harness", "tab": tab, "wait": "none"},
        }))
        .expect("request serializes");
        let socket = vc.update(|_, cx| harness.read(cx).terminal_service.socket_path().to_owned());
        let mut stream = UnixStream::connect(&socket).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(300))).expect("read timeout");
        stream.write_all(request.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        let mut reader = BufReader::new(stream);
        let mut reply = None;
        for _ in 0..400 {
            // The pump task's exact shape: the service drains inside a
            // Harness update, so a hook that touches the Harness re-enters.
            vc.update(|_, cx| {
                harness.update(cx, |harness, cx| harness.terminal_service.drain(cx));
            });
            reply = try_recv(&mut reader, id);
            if reply.is_some() {
                break;
            }
        }
        let out = result(&reply.expect("terminal_run answered through the Harness pump"));
        assert_eq!(out.get("tab"), Some(&json!(tab)), "the run landed: {out}");
        assert_eq!(out.get("status"), Some(&json!("running")));
        vc.update(|_, cx| {
            assert!(
                harness.read(cx).layout.terminal_open,
                "the run opens the dock for the person to watch"
            );
        });
    }

    /// One browser request over a real socket, draining until the reply
    /// arrives. Evaluations ride the webview's own poll timer (50 ms)
    /// before the service can collect them, and the test executor is
    /// deterministic — wall-clock never fires its timers — so each pass
    /// advances the virtual clock past one poll, then parks: the app's
    /// 15 ms pump, at test speed. A breath of real time per pass lets
    /// `Instant` deadlines expire for the timeout path.
    fn bcall(
        cx: &mut gpui::TestAppContext,
        fx: &Fixture,
        session: &str,
        tool: &str,
        params: Value,
    ) -> Value {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::to_string(&json!({
            "id": id,
            "session": session,
            "tool": tool,
            "params": params,
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(10))).expect("read timeout");
        stream.write_all(request.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        let mut reader = BufReader::new(stream);
        for _ in 0..300 {
            cx.update(|cx| fx.service.drain(cx));
            cx.dispatcher.advance_clock(Duration::from_millis(60));
            cx.run_until_parked();
            if let Some(reply) = try_recv(&mut reader, id) {
                return reply;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("{tool}: no reply after draining");
    }

    /// A fixture with the scripted page registered as `session`'s webview.
    fn browser_fixture(
        cx: &mut gpui::TestAppContext,
        session: &str,
    ) -> (Fixture, Entity<WebviewState>) {
        let fx = fixture(cx, session);
        let state = cx.new(|cx| WebviewState::new(Box::new(FakeWebBackend::new()), cx));
        fx.service.register_browser(session, state.clone());
        (fx, state)
    }

    /// Only page URLs reach a webview: `javascript:`, `data:` and unknown
    /// schemes are refused before any navigation.
    #[test]
    fn only_page_urls_reach_the_webview() {
        assert!(browser_url_allowed("https://example.com/x?q=1"));
        assert!(browser_url_allowed("http://localhost:3000/pricing"));
        assert!(browser_url_allowed("HTTP://UPPERCASE-SCHEME.EXAMPLE/"));
        assert!(browser_url_allowed("file:///tmp/page.html"));
        assert!(browser_url_allowed("about:blank"));
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "data:text/html,<h1>hi</h1>",
            "about:config",
            "chrome://settings",
            "ftp://files.example/x",
            "https",
            "",
        ] {
            assert!(!browser_url_allowed(bad), "refused: {bad}");
        }
    }

    /// `browser_open` navigates the calling session's page and answers its
    /// URL and title, and fires the pane hook for that session.
    #[gpui::test]
    fn browser_open_navigates_and_answers_url_and_title(cx: &mut gpui::TestAppContext) {
        let (fx, state) = browser_fixture(cx, "sb-open");
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = seen.clone();
            fx.service.set_browser_open_hook(move |_, session: String| {
                seen.lock().expect("hook log").push(session);
            });
        }
        let out = result(&bcall(cx, &fx, "sb-open", "browser_open", json!({"url": "https://example.com"})));
        assert_eq!(out.get("url"), Some(&json!("https://example.com")), "the open answers: {out}");
        assert!(
            out.get("title").and_then(Value::as_str).is_some_and(|title| !title.is_empty()),
            "the open names the title: {out}"
        );
        assert_eq!(
            cx.update(|cx| state.read(cx).url().to_string()),
            "https://example.com",
            "the session's page really moved"
        );
        assert_eq!(&*seen.lock().expect("hook log"), &["sb-open".to_owned()], "the pane opens");
    }

    /// `browser_open` refuses `javascript:` and `data:` URLs without
    /// touching the page; `about:blank` opens fine.
    #[gpui::test]
    fn browser_open_refuses_non_page_schemes(cx: &mut gpui::TestAppContext) {
        let (fx, state) = browser_fixture(cx, "sb-scheme");
        let before = cx.update(|cx| state.read(cx).url().to_string());
        for url in ["javascript:alert(1)", "data:text/html,<h1>hi</h1>"] {
            let message = err_text(&bcall(cx, &fx, "sb-scheme", "browser_open", json!({"url": url})));
            assert!(message.contains("refused URL"), "refused: {message}");
        }
        assert_eq!(
            cx.update(|cx| state.read(cx).url().to_string()),
            before,
            "a refused open never navigates"
        );
        let out = result(&bcall(cx, &fx, "sb-scheme", "browser_open", json!({"url": "about:blank"})));
        assert_eq!(out.get("url"), Some(&json!("about:blank")));
    }

    /// `browser_read`, `browser_links`, `browser_click` and `browser_type`
    /// round-trip through the service against the scripted page, and an
    /// unknown selector is a tool error.
    #[gpui::test]
    fn browser_read_links_click_type_round_trip(cx: &mut gpui::TestAppContext) {
        let (fx, _) = browser_fixture(cx, "sb-round");
        let opened = result(&bcall(cx, &fx, "sb-round", "browser_open", json!({"url": "https://example.com"})));
        assert_eq!(opened.get("url"), Some(&json!("https://example.com")));
        let read = result(&bcall(cx, &fx, "sb-round", "browser_read", json!({})));
        assert_eq!(read.get("url"), Some(&json!("https://example.com")), "the read names the page: {read}");
        assert!(
            read.get("text").and_then(Value::as_str).is_some_and(|text| text.contains("Simple pricing")),
            "the read carries the body: {read}"
        );
        let links = result(&bcall(cx, &fx, "sb-round", "browser_links", json!({})));
        assert_eq!(links.get("links"), Some(&json!([])), "the scripted page has no links: {links}");
        let clicked = result(&bcall(cx, &fx, "sb-round", "browser_click", json!({"selector": "h1"})));
        assert_eq!(clicked.get("text"), Some(&json!("Simple pricing")), "the click answers: {clicked}");
        let typed = result(
            &bcall(cx, &fx, "sb-round", "browser_type", json!({"selector": "p.lead", "text": "hello"})),
        );
        assert_eq!(typed.get("text"), Some(&json!("hello")), "the type answers: {typed}");
        let missing =
            err_text(&bcall(cx, &fx, "sb-round", "browser_click", json!({"selector": "main.missing"})));
        assert!(missing.contains("element not found"), "the page's refusal surfaces: {missing}");
    }

    /// A call for session B acts on B's page: it cannot read, move, or
    /// click A's.
    #[gpui::test]
    fn browser_sessions_cannot_touch_each_others_pages(cx: &mut gpui::TestAppContext) {
        let (fx, state_a) = browser_fixture(cx, "sb-a");
        fx.service.register_session("sb-b", fx.root.clone());
        let state_b = cx.new(|cx| WebviewState::new(Box::new(FakeWebBackend::new()), cx));
        fx.service.register_browser("sb-b", state_b.clone());
        let open_a =
            result(&bcall(cx, &fx, "sb-a", "browser_open", json!({"url": "https://a.example/"})));
        assert_eq!(open_a.get("url"), Some(&json!("https://a.example/")));
        let open_b =
            result(&bcall(cx, &fx, "sb-b", "browser_open", json!({"url": "https://b.example/"})));
        assert_eq!(open_b.get("url"), Some(&json!("https://b.example/")));
        let read_a = result(&bcall(cx, &fx, "sb-a", "browser_read", json!({})));
        assert_eq!(read_a.get("url"), Some(&json!("https://a.example/")), "A reads A's page: {read_a}");
        let read_b = result(&bcall(cx, &fx, "sb-b", "browser_read", json!({})));
        assert_eq!(read_b.get("url"), Some(&json!("https://b.example/")), "B reads B's page: {read_b}");
        assert_eq!(cx.update(|cx| state_a.read(cx).url().to_string()), "https://a.example/");
        assert_eq!(cx.update(|cx| state_b.read(cx).url().to_string()), "https://b.example/");
    }

    /// A page that never answers is an error, not a hung tool: the scripted
    /// page never produces a screenshot, so the capture waits out the
    /// (test-shortened) deadline and reports it.
    #[gpui::test]
    fn browser_without_an_answer_times_out(cx: &mut gpui::TestAppContext) {
        let (fx, _) = browser_fixture(cx, "sb-shot");
        fx.service.set_browser_timeout(Duration::from_millis(150));
        let message = err_text(&bcall(cx, &fx, "sb-shot", "browser_screenshot", json!({})));
        assert_eq!(message, BROWSER_NO_ANSWER, "the timeout names itself");
    }

    /// One `browser_open` where the test plays WebKit: after the service
    /// navigates to `request`, the page reports `reported` (the normalised /
    /// redirected URL) before the reply arrives. Drains like [`bcall`].
    fn bopen_with_report(
        cx: &mut gpui::TestAppContext,
        fx: &Fixture,
        state: &Entity<WebviewState>,
        session: &str,
        request: &str,
        reported: &str,
    ) -> Value {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let line = serde_json::to_string(&json!({
            "id": id, "session": session, "tool": "browser_open",
            "params": {"url": request},
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(10))).expect("read timeout");
        stream.write_all(line.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        let mut reader = BufReader::new(stream);
        let mut reported_yet = false;
        for _ in 0..300 {
            cx.update(|cx| fx.service.drain(cx));
            // The service navigates on its first drain; WebKit then reports
            // the normalised URL, which is what the state carries after.
            if !reported_yet && cx.update(|cx| state.read(cx).url().to_string()) == request {
                cx.update(|cx| state.update(cx, |state, _| state.navigate(reported)));
                reported_yet = true;
            }
            cx.dispatcher.advance_clock(Duration::from_millis(60));
            cx.run_until_parked();
            if let Some(reply) = try_recv(&mut reader, id) {
                return reply;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("browser_open: no reply after draining");
    }

    /// WebKit normalises `https://example.com` to `https://example.com/`:
    /// the open answers the FINAL url, not the requested one.
    #[gpui::test]
    fn browser_open_answers_the_normalised_url(cx: &mut gpui::TestAppContext) {
        let (fx, state) = browser_fixture(cx, "sb-norm");
        let out = result(&bopen_with_report(
            cx,
            &fx,
            &state,
            "sb-norm",
            "https://example.com",
            "https://example.com/",
        ));
        assert_eq!(out.get("url"), Some(&json!("https://example.com/")), "the final URL answers: {out}");
        assert!(
            out.get("title").and_then(Value::as_str).is_some_and(|title| !title.is_empty()),
            "the open still names the title: {out}"
        );
    }

    /// The old page's title survives the navigation until the new one lands
    /// (the fake keeps one title across pages, the worst case): the open must
    /// not answer on that stale title a pass after navigating, only once the
    /// title changes or the grace runs out.
    #[gpui::test]
    fn browser_open_never_settles_on_the_previous_pages_title(cx: &mut gpui::TestAppContext) {
        let (fx, state) = browser_fixture(cx, "sb-stale");
        fx.service.set_browser_timeout(Duration::from_secs(4));
        assert!(!cx.update(|cx| state.read(cx).title().is_empty()), "the pane starts on a titled page");
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let line = serde_json::to_string(&json!({
            "id": id, "session": "sb-stale", "tool": "browser_open",
            "params": {"url": "https://example.org/next"},
        }))
        .expect("request serializes");
        let mut stream = UnixStream::connect(fx.service.socket_path()).expect("socket answers");
        stream.set_read_timeout(Some(Duration::from_millis(10))).expect("read timeout");
        stream.write_all(line.as_bytes()).expect("request writes");
        stream.write_all(b"\n").expect("request ends");
        stream.flush().expect("request flushes");
        let mut reader = BufReader::new(stream);
        let started = Instant::now();
        let mut reply = None;
        while reply.is_none() && started.elapsed() < Duration::from_secs(8) {
            cx.update(|cx| fx.service.drain(cx));
            cx.run_until_parked();
            reply = try_recv(&mut reader, id);
            std::thread::sleep(Duration::from_millis(15));
        }
        let reply = reply.expect("the open answers once the grace runs out");
        assert!(
            started.elapsed() >= Duration::from_millis(1500),
            "answered after {:?} on the old page's unchanged title",
            started.elapsed()
        );
        assert_eq!(result(&reply).get("url"), Some(&json!("https://example.org/next")));
    }

    /// A redirect to another host answers with the final URL: opening
    /// `https://wikipedia.org` lands on `https://www.wikipedia.org/`.
    #[gpui::test]
    fn browser_open_answers_the_redirected_url(cx: &mut gpui::TestAppContext) {
        let (fx, state) = browser_fixture(cx, "sb-redirect");
        let out = result(&bopen_with_report(
            cx,
            &fx,
            &state,
            "sb-redirect",
            "https://wikipedia.org",
            "https://www.wikipedia.org/",
        ));
        assert_eq!(
            out.get("url"),
            Some(&json!("https://www.wikipedia.org/")),
            "the redirect target answers: {out}"
        );
    }

    /// A Codex bridge calls with `--session <request_id>` after its lane
    /// moved to the thread id: the aliased call reaches the browser
    /// registered under the thread id, the pane hook sees the resolved id
    /// (which is what flips the visible pane for the active lane), and a
    /// follow-up `browser_read` under the request id reads that page.
    #[gpui::test]
    fn browser_calls_through_a_codex_request_id_alias(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "cmd-9");
        fx.service.register_session("thread-9", fx.root.clone());
        let state = cx.new(|cx| WebviewState::new(Box::new(FakeWebBackend::new()), cx));
        fx.service.register_browser("thread-9", state.clone());
        fx.service.alias_session("cmd-9", "thread-9");
        let seen = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = seen.clone();
            fx.service.set_browser_open_hook(move |_, session: String| {
                seen.lock().expect("hook log").push(session);
            });
        }
        let out = result(&bcall(cx, &fx, "cmd-9", "browser_open", json!({"url": "https://example.com"})));
        assert_eq!(out.get("url"), Some(&json!("https://example.com")), "the open answers: {out}");
        assert_eq!(
            &*seen.lock().expect("hook log"),
            &["thread-9".to_owned()],
            "the pane hook sees the lane's current id"
        );
        let resolved = fx.service.resolve_session("cmd-9");
        assert_eq!(resolved, "thread-9");
        assert!(
            crate::right::agent_open_flips_visible_pane(Some("thread-9"), &resolved),
            "the resolved id flips the visible pane for the active lane"
        );
        let read = result(&bcall(cx, &fx, "cmd-9", "browser_read", json!({})));
        assert_eq!(read.get("url"), Some(&json!("https://example.com")), "the read reaches the page: {read}");
    }

    /// A `browser_open` with no page behind the session still times out —
    /// and the error names the URL and says the page may still be loading.
    #[gpui::test]
    fn browser_open_without_a_page_times_out_naming_the_url(cx: &mut gpui::TestAppContext) {
        let fx = fixture(cx, "sb-stuck");
        fx.service.set_browser_timeout(Duration::from_millis(150));
        let message =
            err_text(&bcall(cx, &fx, "sb-stuck", "browser_open", json!({"url": "https://example.com/page"})));
        assert!(message.contains("https://example.com/page"), "the timeout names the URL: {message}");
        assert!(message.contains("may still be loading"), "the load may yet land: {message}");
    }
}
