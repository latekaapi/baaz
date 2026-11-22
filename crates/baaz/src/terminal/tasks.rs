//! The task book (docs/82 6.5): what each session's agent started through
//! Baaz's terminal tools, for the right pane's Tasks view and the sidebar
//! row's "N running".
//!
//! A task is one command the agent ran: every `terminal_run`, and every
//! command line it typed with `terminal_send` into an idle tab it opened
//! (`terminal_open` then `send "npm run dev" + enter`). An idle shell the
//! agent opened is not a task until something runs in it; the person's own
//! commands, in their tabs or in the agent's, are never recorded — except
//! the session's Run (docs/82 7.4, [`crate::project_run`]): the project's
//! declared `run`, started from the header menu, is a task too
//! ([`TaskBook::record_run`]), carrying its preview's progress.
//!
//! The book lives on [`TerminalHost`](super::TerminalHost), which owns the
//! tabs the tasks run in: the host reads each running task's block on a
//! one-second poll ([`TaskBook::settle`]) and notifies once when something
//! changed. Finished tasks stay listed until their session goes (hidden,
//! archived or removed), at most [`TASKS_PER_SESSION`] per session, oldest
//! finished first out. Nothing persists across launches.
//!
//! Stop ends the task's process group: the command running in the
//! foreground of its tab's shell, which an interactive shell runs in a
//! group of its own ([`provider::runtime::foreground_group`]), SIGTERM then
//! SIGKILL after [`STOP_GRACE`] ([`provider::runtime::end_groups`]). The
//! shell itself stays, so the tab is still usable.

use std::cell::Cell;
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How many tasks a session keeps listed; past it the oldest finished one
/// goes (running ones are never dropped).
pub const TASKS_PER_SESSION: usize = 50;

/// How long a stopped group gets to leave on SIGTERM before SIGKILL: long
/// enough for a dev server to close its sockets, short enough that Stop
/// reads as Stop.
pub const STOP_GRACE: Duration = Duration::from_millis(1500);

/// How long quitting gives every running task's group on SIGTERM, all at
/// once, before SIGKILL.
pub const QUIT_GRACE: Duration = Duration::from_millis(300);

/// How often the host reads its running tasks' blocks.
pub const POLL_EVERY: Duration = Duration::from_secs(1);

/// How often listening ports are looked up while the Tasks view shows.
pub const PORTS_EVERY: Duration = Duration::from_secs(3);

/// How long the Tasks view counts as watching after it last drew: the poll
/// keeps the running durations ticking only while someone looks.
const WATCH_FOR: Duration = Duration::from_secs(2);

/// Where a task stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    /// Still running (or about to: its block has not formed yet).
    Running,
    /// It exited with this code.
    Exited(i32),
    /// The person pressed Stop and it is gone.
    Stopped,
    /// It is gone and its exit is unknown (its tab closed).
    Ended,
}

impl TaskState {
    /// The words the Tasks view shows: `running`, `exited 0`, `exited 1`,
    /// `stopped`, `ended`.
    pub fn label(self) -> String {
        match self {
            TaskState::Running => "running".to_owned(),
            TaskState::Exited(code) => format!("exited {code}"),
            TaskState::Stopped => "stopped".to_owned(),
            TaskState::Ended => "ended".to_owned(),
        }
    }

    /// Still running.
    pub fn running(self) -> bool {
        self == TaskState::Running
    }
}

/// How long a task has run: live (start to now, or to its end), or — in a
/// probe build — a fixed span (a capture's fixture, so two shots agree).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Span {
    Live { started: Instant, ended: Option<Instant> },
    #[cfg(feature = "probe")]
    Fixed(Duration),
}

/// Where a Run's preview stands (docs/82 7.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preview {
    /// The project declares no `preview_url`.
    None,
    /// Waiting for its port to answer.
    Waiting,
    /// It answered and the Page opened on it.
    Opened,
    /// It answered, but the person had moved on: a toast said so instead.
    Ready,
    /// Nothing answered in time.
    GaveUp,
    /// The run ended before its port answered.
    RunEnded,
}

/// One recorded task.
#[derive(Clone, Debug)]
pub struct Task {
    /// Unique in this window, in record order.
    pub id: u64,
    /// The session whose agent started it (the id the tools answered as).
    pub session: String,
    /// The command line as the agent sent it.
    pub command: String,
    /// The tab it runs in, while it has one.
    pub tab: Option<String>,
    /// Its block's index in that tab.
    pub block: usize,
    pub state: TaskState,
    span: Span,
    /// The TCP ports its process group listens on, when looked up.
    pub port: Option<u16>,
    /// The process group in its tab's foreground while it runs, when read.
    pub group: Option<i32>,
    /// The person pressed Stop.
    pub stop_asked: bool,
    /// The session's Run (docs/82 7.4), with its preview's progress; `None`
    /// for what the agent started.
    pub run: Option<Preview>,
}

impl Task {
    /// How long it has run, as of `now`.
    pub fn duration(&self, now: Instant) -> Duration {
        match self.span {
            Span::Live { started, ended } => ended.unwrap_or(now).saturating_duration_since(started),
            #[cfg(feature = "probe")]
            Span::Fixed(span) => span,
        }
    }
}

/// What the host saw of a task's block on one poll.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    /// The tab is gone.
    Gone,
    /// The block has not formed yet (the shell has not echoed the command).
    NotYet,
    /// The block runs.
    Running,
    /// The block closed, with this exit when the shell reported one.
    Finished { exit: Option<i32>, ended: Option<Instant> },
}

/// One row of the Tasks view, read at one instant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskRow {
    pub id: u64,
    pub command: String,
    pub state: TaskState,
    pub port: Option<u16>,
    pub duration: Duration,
    /// The tab a click opens, while the task has one.
    pub tab: Option<String>,
}

/// Every session's tasks (see the module doc).
#[derive(Default)]
pub struct TaskBook {
    tasks: Vec<Task>,
    next_id: u64,
    /// Something the views draw changed since the last notify.
    dirty: bool,
    /// When the Tasks view last drew: the poll ticks durations only while
    /// it is recent. Stamped from a render, which only reads.
    watched: Cell<Option<Instant>>,
    /// When ports were last looked up, and whether a lookup is out.
    pub(crate) ports_at: Option<Instant>,
    pub(crate) ports_in_flight: bool,
}

impl TaskBook {
    /// Record a command `session`'s agent started in `tab`, whose block
    /// will be `block`. Returns the task's id.
    pub fn record(&mut self, session: &str, command: &str, tab: &str, block: usize) -> u64 {
        let id = self.push(Task {
            id: 0,
            session: session.to_owned(),
            command: command.trim().to_owned(),
            tab: Some(tab.to_owned()),
            block,
            state: TaskState::Running,
            span: Span::Live { started: Instant::now(), ended: None },
            port: None,
            group: None,
            stop_asked: false,
            run: None,
        });
        self.trim(session);
        id
    }

    /// Record the session's Run (docs/82 7.4): the project's `run` started
    /// in `tab`, whose block will be `block`; `preview` says whether a
    /// preview waits on it. Returns the task's id.
    pub fn record_run(&mut self, session: &str, command: &str, tab: &str, block: usize, preview: bool) -> u64 {
        let id = self.record(session, command, tab, block);
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.run = Some(if preview { Preview::Waiting } else { Preview::None });
        }
        id
    }

    /// `session`'s Run that still runs, if any: what keeps a second Run
    /// from starting a second copy.
    pub fn running_run(&self, session: &str) -> Option<u64> {
        self.tasks.iter().rev().find(|task| task.session == session && task.run.is_some() && task.state.running()).map(|task| task.id)
    }

    /// The tab `session`'s newest Run ran in, while it has one: the tab a
    /// next Run reuses when it is idle.
    pub fn last_run_tab(&self, session: &str) -> Option<String> {
        self.tasks.iter().rev().find(|task| task.session == session && task.run.is_some()).and_then(|task| task.tab.clone())
    }

    /// `session`'s newest Run: `(id, state, preview)` — what the probe's
    /// `run-wait-preview` reads.
    #[cfg(feature = "probe")]
    pub fn latest_run(&self, session: &str) -> Option<(u64, TaskState, Preview)> {
        self.tasks.iter().rev().find(|task| task.session == session && task.run.is_some()).map(|task| (task.id, task.state, task.run.unwrap_or(Preview::None)))
    }

    /// Task `id`'s state and, for a Run, its preview's progress.
    pub fn run_state(&self, id: u64) -> Option<(TaskState, Preview)> {
        self.tasks.iter().find(|task| task.id == id).and_then(|task| Some((task.state, task.run?)))
    }

    /// Note where Run `id`'s preview stands.
    pub fn set_preview(&mut self, id: u64, preview: Preview) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id && task.run.is_some()) {
            task.run = Some(preview);
        }
    }

    fn push(&mut self, mut task: Task) -> u64 {
        self.next_id += 1;
        task.id = self.next_id;
        self.tasks.push(task);
        self.dirty = true;
        self.next_id
    }

    /// Keep at most [`TASKS_PER_SESSION`] of `session`'s tasks: the oldest
    /// finished ones go first; running ones always stay.
    fn trim(&mut self, session: &str) {
        let mut over = self.tasks.iter().filter(|task| task.session == session).count().saturating_sub(TASKS_PER_SESSION);
        self.tasks.retain(|task| {
            if over > 0 && task.session == session && !task.state.running() {
                over -= 1;
                return false;
            }
            true
        });
    }

    /// Fold one poll's observations in: `seen(tab, block)` for every running
    /// task with a tab. True when a state changed.
    pub fn settle(&mut self, mut seen: impl FnMut(&str, usize) -> Seen) -> bool {
        let mut changed = false;
        let now = Instant::now();
        for task in self.tasks.iter_mut().filter(|task| task.state.running()) {
            let Some(tab) = task.tab.clone() else { continue };
            let observed = seen(&tab, task.block);
            let (state, ended) = match observed {
                Seen::NotYet | Seen::Running => continue,
                Seen::Gone => (if task.stop_asked { TaskState::Stopped } else { TaskState::Ended }, now),
                Seen::Finished { exit, ended } => {
                    let state = match (task.stop_asked, exit) {
                        (true, _) => TaskState::Stopped,
                        (false, Some(code)) => TaskState::Exited(code),
                        (false, None) => TaskState::Ended,
                    };
                    (state, ended.unwrap_or(now))
                }
            };
            task.state = state;
            task.group = None;
            task.port = None;
            if let Span::Live { ended: end @ None, .. } = &mut task.span {
                *end = Some(ended);
            }
            if observed == Seen::Gone {
                task.tab = None;
            }
            changed = true;
        }
        self.dirty |= changed;
        changed
    }

    /// Every running task with a tab, as `(id, tab, block)`: what a poll reads.
    pub fn running(&self) -> Vec<(u64, String, usize)> {
        self.tasks
            .iter()
            .filter(|task| task.state.running())
            .filter_map(|task| Some((task.id, task.tab.clone()?, task.block)))
            .collect()
    }

    /// Note task `id`'s foreground group (read this poll), or none.
    pub fn set_group(&mut self, id: u64, group: Option<i32>) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.group = group;
        }
    }

    /// The process groups of every running task, as last read.
    pub fn running_groups(&self) -> Vec<i32> {
        let mut groups: Vec<i32> = self.tasks.iter().filter(|task| task.state.running()).filter_map(|task| task.group).collect();
        groups.sort_unstable();
        groups.dedup();
        groups
    }

    /// Land a port lookup: `ports` by process group. Running tasks whose
    /// group is not in it lose their port.
    pub fn set_ports(&mut self, ports: &HashMap<i32, u16>) {
        for task in self.tasks.iter_mut().filter(|task| task.state.running()) {
            let port = task.group.and_then(|group| ports.get(&group).copied());
            if task.port != port {
                task.port = port;
                self.dirty = true;
            }
        }
    }

    /// Where task `id` runs, `(tab, block)`, when it runs; else why there is
    /// nothing to stop.
    pub fn stop_target(&self, id: u64) -> Result<(String, usize), String> {
        let task = self.tasks.iter().find(|task| task.id == id).ok_or_else(|| format!("no task {id}"))?;
        if !task.state.running() {
            return Err(format!("task {id} is not running ({})", task.state.label()));
        }
        let tab = task.tab.clone().ok_or_else(|| format!("task {id} has no tab"))?;
        Ok((tab, task.block))
    }

    /// The person pressed Stop on task `id`: once its block closes it reads
    /// `stopped`, whatever code the signal left.
    pub fn mark_stopping(&mut self, id: u64) {
        if let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) {
            task.stop_asked = true;
            self.dirty = true;
        }
    }

    /// How many tasks run, per session, sorted by session id: what the
    /// sidebar's "N running" reads. Sessions with none are absent.
    pub fn running_counts(&self) -> Vec<(String, u32)> {
        let mut counts: HashMap<&str, u32> = HashMap::new();
        for task in self.tasks.iter().filter(|task| task.state.running()) {
            *counts.entry(task.session.as_str()).or_default() += 1;
        }
        let mut counts: Vec<(String, u32)> = counts.into_iter().map(|(session, n)| (session.to_owned(), n)).collect();
        counts.sort();
        counts
    }

    /// Whether anything with a tab runs: what keeps the poll going. (A
    /// capture fixture's running row has no tab and nothing to poll.)
    pub fn any_running(&self) -> bool {
        self.tasks.iter().any(|task| task.state.running() && task.tab.is_some())
    }

    /// Whether `session` has any task listed.
    pub fn has_tasks(&self, session: &str) -> bool {
        self.tasks.iter().any(|task| task.session == session)
    }

    /// `session`'s tasks as of `now`, newest first.
    pub fn rows(&self, session: &str, now: Instant) -> Vec<TaskRow> {
        self.tasks
            .iter()
            .rev()
            .filter(|task| task.session == session)
            .map(|task| TaskRow {
                id: task.id,
                command: task.command.clone(),
                state: task.state,
                port: task.port,
                duration: task.duration(now),
                tab: task.tab.clone(),
            })
            .collect()
    }

    /// The sessions are gone (hidden, archived, removed): their finished
    /// tasks go. Running ones stay, so a process still alive is never
    /// forgotten; they leave once they finish and the session goes again.
    pub fn forget_finished(&mut self, sessions: &[String]) {
        let before = self.tasks.len();
        self.tasks.retain(|task| task.state.running() || !sessions.contains(&task.session));
        self.dirty |= self.tasks.len() != before;
    }

    /// A session now answers as `to` (a lane's spawn-time id outgrown, as
    /// [`TerminalService::alias_session`](super::TerminalService::alias_session)
    /// records): its tasks follow.
    pub fn rekey(&mut self, from: &str, to: &str) {
        for task in self.tasks.iter_mut().filter(|task| task.session == from) {
            task.session = to.to_owned();
            self.dirty = true;
        }
    }

    /// The Tasks view drew just now: keep the durations ticking.
    pub fn watch(&self) {
        self.watched.set(Some(Instant::now()));
    }

    /// Whether the Tasks view drew recently.
    pub fn watched(&self) -> bool {
        self.watched.get().is_some_and(|at| at.elapsed() < WATCH_FOR)
    }

    /// Whether something changed since the last call (and clear it).
    pub fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    #[cfg(feature = "probe")]
    /// The capture fixture: four tasks of `session`, one in each state the
    /// view draws — a dev server running on a port, a test run that passed,
    /// a lint that failed, a watcher the person stopped — with fixed spans
    /// and no process behind any of them.
    pub fn fixture(&mut self, session: &str) {
        let rows: [(&str, TaskState, Option<u16>, u64); 4] = [
            ("pnpm vitest run --reporter=dot", TaskState::Exited(0), None, 41),
            ("pnpm lint", TaskState::Exited(1), None, 7),
            ("cargo watch -x 'test -p checkout'", TaskState::Stopped, None, 312),
            ("pnpm dev --port 5173", TaskState::Running, Some(5173), 754),
        ];
        for (command, state, port, secs) in rows {
            self.push(Task {
                id: 0,
                session: session.to_owned(),
                command: command.to_owned(),
                tab: None,
                block: 0,
                state,
                span: Span::Fixed(Duration::from_secs(secs)),
                port,
                group: None,
                stop_asked: state == TaskState::Stopped,
                run: None,
            });
        }
    }
}

/// A duration as the Tasks view shows it: `0s`, `42s`, `3m 07s`, `1h 04m`.
pub fn duration_label(span: Duration) -> String {
    let secs = span.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {:02}s", secs / 60, secs % 60),
        _ => format!("{}h {:02}m", secs / 3600, (secs % 3600) / 60),
    }
}

/// The TCP ports each of `groups` listens on (the lowest per group), from
/// one bounded `lsof` run (`-a -g <groups> -iTCP -sTCP:LISTEN`). Empty
/// when lsof is missing, slow (past 2 s it is killed) or finds nothing.
/// Blocking: run it off the UI thread.
pub fn listening_ports(groups: &[i32]) -> HashMap<i32, u16> {
    if groups.is_empty() {
        return HashMap::new();
    }
    let list = groups.iter().map(i32::to_string).collect::<Vec<_>>().join(",");
    let child = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-g", &list, "-iTCP", "-sTCP:LISTEN", "-Fgn"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return HashMap::new() };
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return HashMap::new();
            }
        }
    }
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read as _;
        let _ = stdout.read_to_string(&mut out);
    }
    parse_lsof(&out)
}

/// `lsof -Fgn` output → the lowest listening port per process group.
fn parse_lsof(out: &str) -> HashMap<i32, u16> {
    let mut ports: HashMap<i32, u16> = HashMap::new();
    let mut group: Option<i32> = None;
    for line in out.lines() {
        if let Some(rest) = line.strip_prefix('g') {
            group = rest.trim().parse().ok();
        } else if let (Some(rest), Some(group)) = (line.strip_prefix('n'), group) {
            let Some(port) = rest.rsplit(':').next().and_then(|port| port.parse::<u16>().ok()) else { continue };
            let slot = ports.entry(group).or_insert(port);
            *slot = (*slot).min(port);
        }
    }
    ports
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    #[test]
    fn a_recorded_task_runs_until_its_block_closes_with_its_code() {
        let mut book = TaskBook::default();
        let id = book.record("s1", "  pnpm test ", "t1", 3);
        assert_eq!(book.running_counts(), vec![("s1".to_owned(), 1)]);
        assert!(!book.settle(|_, _| Seen::NotYet), "no block yet: still running");
        assert!(!book.settle(|_, _| Seen::Running));
        assert!(book.settle(|tab, block| {
            assert_eq!((tab, block), ("t1", 3));
            Seen::Finished { exit: Some(1), ended: None }
        }));
        let rows = book.rows("s1", Instant::now());
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].id, rows[0].command.as_str(), rows[0].state), (id, "pnpm test", TaskState::Exited(1)));
        assert_eq!(rows[0].state.label(), "exited 1");
        assert!(book.running_counts().is_empty());
        // A finished task's span stands still.
        let first = rows[0].duration;
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(book.rows("s1", Instant::now())[0].duration, first);
    }

    #[test]
    fn stop_reads_stopped_and_a_closed_tab_ends_what_ran_in_it() {
        let mut book = TaskBook::default();
        let stopped = book.record("s1", "sleep 60", "t1", 0);
        let closed = book.record("s1", "npm run dev", "t2", 0);
        assert_eq!(book.stop_target(stopped), Ok(("t1".to_owned(), 0)));
        book.mark_stopping(stopped);
        book.settle(|tab, _| if tab == "t1" { Seen::Finished { exit: Some(143), ended: None } } else { Seen::Gone });
        let rows = book.rows("s1", Instant::now());
        let state = |id| rows.iter().find(|row| row.id == id).map(|row| (row.state, row.tab.clone()));
        assert_eq!(state(stopped), Some((TaskState::Stopped, Some("t1".to_owned()))));
        assert_eq!(state(closed), Some((TaskState::Ended, None)), "the tab is gone with the task");
        assert!(book.stop_target(stopped).is_err(), "nothing to stop twice");
    }

    #[test]
    fn counts_go_per_session_and_follow_a_rekey() {
        let mut book = TaskBook::default();
        book.record("req-1", "a", "t1", 0);
        book.record("req-1", "b", "t2", 0);
        book.record("s2", "c", "t3", 0);
        assert_eq!(book.running_counts(), vec![("req-1".to_owned(), 2), ("s2".to_owned(), 1)]);
        book.rekey("req-1", "thread-9");
        assert_eq!(book.running_counts(), vec![("s2".to_owned(), 1), ("thread-9".to_owned(), 2)]);
    }

    #[test]
    fn a_gone_session_keeps_only_what_still_runs() {
        let mut book = TaskBook::default();
        book.record("s1", "done", "t1", 0);
        book.record("s1", "live", "t2", 0);
        book.settle(|tab, _| if tab == "t1" { Seen::Finished { exit: Some(0), ended: None } } else { Seen::Running });
        book.forget_finished(&["s1".to_owned()]);
        let rows = book.rows("s1", Instant::now());
        assert_eq!(rows.iter().map(|row| row.command.as_str()).collect::<Vec<_>>(), vec!["live"]);
    }

    #[test]
    fn a_session_keeps_fifty_and_drops_its_oldest_finished() {
        let mut book = TaskBook::default();
        let first_running = book.record("s1", "server", "t0", 0);
        for i in 1..=TASKS_PER_SESSION {
            book.record("s1", &format!("job {i}"), &format!("t{i}"), 0);
            book.settle(|tab, _| if tab == "t0" { Seen::Running } else { Seen::Finished { exit: Some(0), ended: None } });
        }
        let rows = book.rows("s1", Instant::now());
        assert_eq!(rows.len(), TASKS_PER_SESSION);
        assert!(rows.iter().any(|row| row.id == first_running), "the running one stays");
        assert!(!rows.iter().any(|row| row.command == "job 1"), "the oldest finished went");
        assert_eq!(rows[0].command, format!("job {TASKS_PER_SESSION}"), "newest first");
    }

    #[test]
    fn ports_land_on_the_running_task_of_their_group() {
        let mut book = TaskBook::default();
        let id = book.record("s1", "serve", "t1", 0);
        book.set_group(id, Some(4242));
        assert_eq!(book.running_groups(), vec![4242]);
        book.take_dirty();
        book.set_ports(&HashMap::from([(4242, 5173)]));
        assert!(book.take_dirty());
        assert_eq!(book.rows("s1", Instant::now())[0].port, Some(5173));
        book.set_ports(&HashMap::new());
        assert_eq!(book.rows("s1", Instant::now())[0].port, None);
    }

    #[test]
    fn lsof_output_parses_to_the_lowest_port_per_group() {
        let out = "p100\ng100\nf5\nn*:5173\nf6\nn127.0.0.1:24678\np200\ng200\nf3\nn[::1]:8080\n";
        assert_eq!(parse_lsof(out), HashMap::from([(100, 5173), (200, 8080)]));
        assert!(parse_lsof("").is_empty());
    }

    /// A stand-in listener in this test process: its group's port is found.
    #[test]
    fn a_listener_in_a_group_is_found_by_lsof() {
        if !std::path::Path::new("/usr/sbin/lsof").exists() {
            eprintln!("skipped: no /usr/sbin/lsof");
            return;
        }
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("skipped: this sandbox forbids listening ({error})");
                return;
            }
        };
        let port = listener.local_addr().expect("bound").port();
        // SAFETY: a plain getter.
        let group = unsafe { libc::getpgrp() };
        let found = listening_ports(&[group]);
        let got = found.get(&group).copied();
        assert!(got.is_some_and(|lowest| lowest <= port), "group {group} listens on {port}: {found:?}");
        drop(listener);
    }

    /// docs/82 7.4: a session has one Run at a time — the running one is
    /// found, not doubled — and the next Run reuses the last one's tab.
    #[test]
    fn a_run_is_found_while_it_runs_and_its_tab_is_remembered() {
        let mut book = TaskBook::default();
        assert_eq!(book.running_run("s1"), None);
        book.record("s1", "pnpm test", "t1", 0);
        assert_eq!(book.running_run("s1"), None, "the agent's task is no Run");
        let run = book.record_run("s1", "python3 -m http.server $BAAZ_PORT", "t2", 0, true);
        assert_eq!(book.running_run("s1"), Some(run));
        assert_eq!(book.running_run("s2"), None, "per session");
        assert_eq!(book.latest_run("s1"), Some((run, TaskState::Running, Preview::Waiting)));
        book.set_preview(run, Preview::Opened);
        assert_eq!(book.run_state(run), Some((TaskState::Running, Preview::Opened)));
        book.settle(|tab, _| if tab == "t2" { Seen::Finished { exit: Some(0), ended: None } } else { Seen::Running });
        assert_eq!(book.running_run("s1"), None, "it ended");
        assert_eq!(book.last_run_tab("s1"), Some("t2".to_owned()), "the next Run reuses its tab");
        book.rekey("s1", "thread-1");
        assert_eq!(book.last_run_tab("thread-1"), Some("t2".to_owned()), "a Run follows its session's new id");
    }

    #[test]
    fn durations_read_short() {
        assert_eq!(duration_label(Duration::from_millis(400)), "0s");
        assert_eq!(duration_label(Duration::from_secs(42)), "42s");
        assert_eq!(duration_label(Duration::from_secs(187)), "3m 07s");
        assert_eq!(duration_label(Duration::from_secs(3840)), "1h 04m");
    }
}
