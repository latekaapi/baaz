//! The terminal service: the agent's seven tools over a unix socket (D46).
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
//! [`TerminalHost`] and answers. A `terminal_run` with `wait: exit` stays a
//! [`PendingRun`] across drains: each drain pumps the tab once and checks
//! the block, so a long run never stalls a frame and the UI never blocks.
//!
//! Only session ids the app registered ([`register_session`][TerminalService::register_session],
//! [`unregister`][TerminalService::unregister_session], D53) are served.
//! The socket file is removed when the service drops (quit).
//!
//! Terminal output never reaches a log line: this module has no logging at
//! all, by construction (pinned by `crates/baaz/tests/terminal_service.rs`).

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use aui_terminal::{BlockAuthor, TextCursor};
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

/// `terminal_run` output is head+tail capped at this many bytes.
pub const RUN_OUTPUT_CAP: usize = 4_096;
/// `terminal_read`'s default `max_bytes`.
pub const READ_DEFAULT_MAX: usize = 4_096;
/// `terminal_read`'s maximum `max_bytes`.
pub const READ_MAX: usize = 32_768;
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

/// The service: session registry, request queue, pending runs, and the
/// socket's lifecycle. Clone it freely; every clone serves the same socket.
pub struct TerminalService {
    shared: Arc<Shared>,
    socket_path: PathBuf,
    shutdown: Arc<AtomicBool>,
}

struct Shared {
    host: Entity<TerminalHost>,
    sessions: Mutex<HashMap<String, PathBuf>>,
    queue: Mutex<VecDeque<Job>>,
    pending: Mutex<Vec<PendingRun>>,
    activity: Mutex<Option<ActivityHook>>,
}

/// What the harness does when the agent runs something: open the dock (not
/// focused) so the person can watch. Runs on the UI thread, inside `drain`.
type ActivityHook = Box<dyn Fn(&mut App) + Send + Sync>;

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
    deadline: Instant,
    reply: mpsc::Sender<String>,
}

impl TerminalService {
    /// Serve on `<support_dir>/run/terminal-<pid>.sock` for this process.
    /// Best-effort: when the path is already live (a second window in this
    /// process) the service still registers sessions and drains, but owns
    /// no listener — the relay then reports the terminal unavailable
    /// rather than stealing the first window's socket.
    pub fn start(host: Entity<TerminalHost>, support_dir: &Path) -> Self {
        Self::start_at(host, &support_dir.join("run"), std::process::id())
    }

    /// Serve on `<run_dir>/terminal-<pid>.sock`: the seam the tests drive
    /// with a temp dir.
    pub fn start_at(host: Entity<TerminalHost>, run_dir: &Path, pid: u32) -> Self {
        let _ = std::fs::create_dir_all(run_dir);
        let _ = std::fs::set_permissions(run_dir, std::fs::Permissions::from_mode(0o700));
        let path = run_dir.join(format!("terminal-{pid}.sock"));
        let shared = Arc::new(Shared {
            host,
            sessions: Mutex::new(HashMap::new()),
            queue: Mutex::new(VecDeque::new()),
            pending: Mutex::new(Vec::new()),
            activity: Mutex::new(None),
        });
        let shutdown = Arc::new(AtomicBool::new(false));
        // A leftover file from a crashed run binds fine once removed; a
        // live socket means another window owns the name, so keep hands off.
        if path.exists() && UnixStream::connect(&path).is_ok() {
            return Self { shared, socket_path: path, shutdown };
        }
        let _ = std::fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => {
                let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
                let _ = listener.set_nonblocking(true);
                let worker = shared.clone();
                let done = shutdown.clone();
                std::thread::spawn(move || accept_loop(listener, worker, done));
            }
            Err(_) => {
                // No listener (permissions, a second window that raced us):
                // the object still works, the socket just is not ours.
            }
        }
        Self { shared, socket_path: path, shutdown }
    }

    /// Where this service listens (or would, when another window owns it).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Whether this service owns a live listener: false when another
    /// window kept the name.
    pub fn is_serving(&self) -> bool {
        UnixStream::connect(&self.socket_path).is_ok()
    }

    /// Trust `id` (a session the app opened) for `project_root` (D53).
    /// Re-registering moves the session to the new root.
    pub fn register_session(&self, id: &str, project_root: PathBuf) {
        self.shared.sessions.lock().expect("session registry").insert(id.to_owned(), project_root);
    }

    /// Forget `id`: its tools are refused from here on.
    pub fn unregister_session(&self, id: &str) {
        self.shared.sessions.lock().expect("session registry").remove(id);
    }

    /// What the harness runs on the UI thread when the agent starts
    /// something (open the dock, unfocused). Test hooks observe it instead.
    pub fn set_activity_hook(&self, hook: impl Fn(&mut App) + Send + Sync + 'static) {
        *self.shared.activity.lock().expect("activity hook") = Some(Box::new(hook));
    }

    /// Run every queued request and poll every pending run. Call on the UI
    /// thread only — everything here touches [`TerminalHost`].
    pub fn drain(&self, cx: &mut App) {
        let jobs: Vec<Job> = self.shared.queue.lock().expect("job queue").drain(..).collect();
        for job in jobs {
            self.execute(job, cx);
        }
        self.poll_runs(cx);
    }

    /// How many runs are still waiting on their blocks. Tests read this;
    /// the app never needs it.
    #[cfg(test)]
    pub(crate) fn pending_runs(&self) -> usize {
        self.shared.pending.lock().expect("pending runs").len()
    }

    fn execute(&self, job: Job, cx: &mut App) {
        let root = match self.shared.sessions.lock().expect("session registry").get(&job.session) {
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
            self.run(&root, Some(job.session.clone()), &job.params, &job.id, job.reply, cx);
            return;
        }
        let outcome: Result<Value, String> = match job.tool.as_str() {
            "terminal_list" => self.list(&root, cx),
            "terminal_open" => self.open(&root, Some(job.session.clone()), &job.params, cx),
            "terminal_read" => self.read(&root, &job.params, cx),
            "terminal_screen" => self.screen(&root, &job.params, cx),
            "terminal_send" => self.send(&root, &job.params, cx),
            "terminal_close" => self.close(&root, &job.params, cx),
            other => Err(format!("unknown tool: {other}")),
        };
        match outcome {
            Ok(result) => send(&job.reply, &job.id, true, result),
            Err(error) => send(&job.reply, &job.id, false, json!({"error": error})),
        }
    }
}

impl Drop for TerminalService {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Release);
        // Quit removes the socket; a second window's service never owned
        // the name, so only remove what answers to no one. A connect here
        // races a fresh bind after a crash — removing a live stranger's
        // socket is worse than leaving a stale file, so check first.
        if UnixStream::connect(&self.socket_path).is_err() {
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
                "cwd": tab.project_root.to_string_lossy(),
                "owner": match tab.owner {
                    TabOwner::User => "user",
                    TabOwner::Agent => "agent",
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
                if self.host().read(cx).busy(cx, named) {
                    let running = self
                        .host()
                        .read(cx)
                        .running_command(cx, named)
                        .unwrap_or_else(|| "an interactive program".to_owned());
                    fail(format!("tab {named} is busy (running: {running})"));
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
                    "output": "",
                    "truncated_bytes": 0,
                    "cursor": cursor,
                }),
            );
            return;
        }
        self.shared.pending.lock().expect("pending runs").push(PendingRun {
            id: job_id.clone(),
            tab: id,
            before,
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
            if blocks.len() > run.before {
                entity.update(cx, |session, _| {
                    let fresh = session.blocks().len().min(blocks.len());
                    for i in run.before..fresh {
                        session.set_block_author(i, BlockAuthor::Agent);
                    }
                });
            }
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
                        "output": output,
                        "truncated_bytes": truncated_bytes,
                        "cursor": tail_of(cx, self.host(), &run.tab),
                    }),
                );
                done.push(index);
            }
        }
        for index in done.into_iter().rev() {
            pending.remove(index);
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
    (format!("{}{}", &text[..head], &text[tail_start..]), bytes.len() - cap)
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
