//! Run and preview (docs/82 7.4): the project's declared `run` started in a
//! terminal tab of the session's own, on the session's ports, and its
//! `preview_url` opened in the session's Page once the port answers.
//!
//! * **Where Run lives.** The centre header's ⋯ menu, beside the session's
//!   other one-off acts (Fork, Undo last turn, Move back): `Run` while
//!   nothing runs; `Show run` and `Stop run` while it does. Only when the
//!   session's folder declares `run` in `.baaz/project.toml` (rule 13: a
//!   control is shown only if it works). Its running copy is a task in the
//!   right pane's Tasks (6.5), which already draws state, port, duration
//!   and Stop — so Run adds one menu row, not a toolbar.
//! * **One copy.** Run while the session's Run still runs starts nothing:
//!   it shows the running one (the pane on Tasks). A finished Run's idle
//!   tab is reused; a busy or closed one gets a new tab.
//! * **Ports.** A worktree session's tab already carries its block
//!   (`BAAZ_PORT…`, 6.1). A session in no worktree gets a block for the
//!   life of its Run tab ([`baaz_core::worktrees::run_port_block`], from
//!   the top of the range down; nothing written to the database), with
//!   `BAAZ_PORT`, `BAAZ_PORT_COUNT` and `BAAZ_PROJECT_ROOT`.
//! * **Preview.** `${BAAZ_PORT}` and `$BAAZ_PORT` (not followed by a letter,
//!   digit or `_`) in `preview_url` become the first port ([`substitute`]).
//!   After Run, the URL's host and port are tried with a plain TCP connect
//!   off the UI thread every [`PREVIEW_EVERY`] ([`answers`]); when one
//!   answers, the session's Page opens on the URL — if the person is still
//!   on that session and the pane still shows Tasks (where Run put it) or
//!   the Page. Moved on to another session or another view, nothing is
//!   taken from them: a toast says the preview is ready. Nothing answering
//!   in [`PREVIEW_GIVE_UP`], or the run ending first, is a quiet toast too.
//! * **Stop** ends the run's process group, as a task's Stop does; quit
//!   ends it with every other running task (6.5).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use baaz_core::worktrees::{self, ProjectConfig};
use gpui::{AppContext as _, Context};

use crate::app::Harness;
use crate::layout::{LayoutGlobal as _, RightKind};
use crate::terminal::tasks::{Preview, TaskState};

/// How often a Run's preview address is tried.
pub(crate) const PREVIEW_EVERY: Duration = Duration::from_millis(300);
/// How long a Run's preview is waited for before a quiet note.
pub(crate) const PREVIEW_GIVE_UP: Duration = Duration::from_secs(60);
/// How long one connect may take.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
/// How long a read of `.baaz/project.toml` stands for the menu.
const CONFIG_FRESH: Duration = Duration::from_secs(2);

/// What the header menu offers for the open session's Run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunOffer {
    /// Its folder declares `run` and nothing of it runs: Run.
    Idle,
    /// Its Run runs: Show run, Stop run.
    Running,
}

/// What a press of Run did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RunPressed {
    /// It started a new Run, task `id`.
    Started(u64),
    /// The session's Run already runs (task `id`): it was shown, not doubled.
    AlreadyRunning(u64),
}

/// `url` with `${BAAZ_PORT}` and `$BAAZ_PORT` replaced by `port`. The bare
/// form only where the name ends (`$BAAZ_PORT_COUNT` is left alone).
pub(crate) fn substitute(url: &str, port: u16) -> String {
    let port = port.to_string();
    let url = url.replace("${BAAZ_PORT}", &port);
    let mut out = String::with_capacity(url.len());
    let mut rest = url.as_str();
    while let Some(at) = rest.find("$BAAZ_PORT") {
        let after = &rest[at + "$BAAZ_PORT".len()..];
        let ends = after.chars().next().is_none_or(|c| !(c.is_ascii_alphanumeric() || c == '_'));
        out.push_str(&rest[..at]);
        if ends {
            out.push_str(&port);
        } else {
            out.push_str("$BAAZ_PORT");
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The host and port a preview URL answers on: `http` (port 80 unless
/// named) or `https` (443); `None` for anything else, or no host.
pub(crate) fn preview_address(url: &str) -> Option<(String, u16)> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let default = match scheme.to_ascii_lowercase().as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let authority = rest.split(['/', '?', '#']).next()?;
    let authority = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let (host, port) = if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        (host, after.strip_prefix(':'))
    } else {
        match authority.rsplit_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (authority, None),
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        Some(port) => port.parse().ok()?,
        None => default,
    };
    Some((host.to_owned(), port))
}

/// Whether something accepts a TCP connection at `host:port` (any address
/// the name resolves to) within `timeout` each. Blocking: off the UI thread.
pub(crate) fn answers(host: &str, port: u16, timeout: Duration) -> bool {
    use std::net::ToSocketAddrs as _;
    let Ok(addrs) = (host, port).to_socket_addrs() else { return false };
    addrs.into_iter().any(|addr| std::net::TcpStream::connect_timeout(&addr, timeout).is_ok())
}

/// What a preview poll does next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    /// The port answers: open the preview.
    Open,
    /// Try again in [`PREVIEW_EVERY`].
    Wait,
    /// Nothing answered in [`PREVIEW_GIVE_UP`].
    GiveUp,
    /// The run ended before its port answered.
    RunEnded,
    /// The Run is forgotten, or its preview was decided already: stop.
    Gone,
}

/// The poll's decision, from this try (`answered`), the Run's task as it
/// stands, and how long it has waited.
pub(crate) fn next(answered: bool, run: Option<(TaskState, Preview)>, waited: Duration) -> Next {
    let Some((state, preview)) = run else { return Next::Gone };
    if preview != Preview::Waiting {
        return Next::Gone;
    }
    if answered {
        return Next::Open;
    }
    if !state.running() {
        return Next::RunEnded;
    }
    if waited >= PREVIEW_GIVE_UP {
        return Next::GiveUp;
    }
    Next::Wait
}

/// `.baaz/project.toml` as the folder declares it, read at most once per
/// [`CONFIG_FRESH`] per folder: the header menu asks every frame it is open.
fn project_config(folder: &Path, project_root: &Path) -> ProjectConfig {
    type Cache = std::sync::Mutex<std::collections::HashMap<PathBuf, (Instant, ProjectConfig)>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((at, config)) = cache.get(folder) {
        if at.elapsed() < CONFIG_FRESH {
            return config.clone();
        }
    }
    let config = worktrees::read_project_config(folder, project_root);
    cache.insert(folder.to_path_buf(), (Instant::now(), config.clone()));
    config
}

/// The open session's Run facts: its id, its folder, the root its
/// terminal tabs belong to, and what the folder declares.
struct RunTarget {
    session: String,
    folder: PathBuf,
    tab_root: PathBuf,
    config: ProjectConfig,
}

impl Harness {
    fn run_target(&self, cx: &gpui::App) -> Option<RunTarget> {
        let view = self.active.as_ref()?;
        let rt = view.read(cx).rt(cx);
        let session = rt.session_id.clone();
        let folder = PathBuf::from(rt.workspace_path());
        if rt.workspace_path().trim().is_empty() {
            return None;
        }
        let project_root = worktrees::cached_worktree_for_folder(&folder).map_or_else(|| folder.clone(), |worktree| worktree.project_root);
        let config = project_config(&folder, &project_root);
        let tab_root = self.terminal_service.session_root(&session).unwrap_or_else(|| folder.clone());
        Some(RunTarget { session, folder, tab_root, config })
    }

    /// What the header menu offers for the open session's Run: `None` when
    /// no session is open or its folder declares no `run`.
    pub(crate) fn run_offer(&self, cx: &gpui::App) -> Option<RunOffer> {
        let target = self.run_target(cx)?;
        target.config.run.as_ref()?;
        let running = self.terminal_host(cx).read(cx).tasks.running_run(&target.session).is_some();
        Some(if running { RunOffer::Running } else { RunOffer::Idle })
    }

    /// Run: start the open session's declared `run` in a terminal tab of its
    /// own with its ports, list it in Tasks (the pane opens there), and wait
    /// for its preview. A Run that still runs is shown, not started again.
    pub(crate) fn run_project(&mut self, cx: &mut Context<Self>) -> Result<RunPressed, String> {
        let target = self.run_target(cx).ok_or("no session is open")?;
        let command = target.config.run.clone().ok_or_else(|| format!("{} declares no run", worktrees::PROJECT_FILE))?;
        let host = self.terminal_host(cx);
        if let Some(id) = host.read(cx).tasks.running_run(&target.session) {
            self.select_right(RightKind::Tasks, cx);
            return Ok(RunPressed::AlreadyRunning(id));
        }
        // The tab: the last Run's, while it is still open and idle (its env
        // already holds the ports); else a new one.
        let reuse = host.read(cx).tasks.last_run_tab(&target.session).filter(|tab| {
            let host = host.read(cx);
            host.get(tab).is_some() && !host.busy(cx, tab)
        });
        let worktree = worktrees::cached_worktree_for_folder(&target.folder);
        let (tab, port) = match reuse {
            Some(tab) => {
                let held = host.read(cx).get(&tab).and_then(|state| state.port_block);
                let port = held.or(worktree.as_ref().map(|worktree| worktree.port)).ok_or("the Run's tab holds no port")?;
                (tab, port)
            }
            None => {
                let (env, block, port) = match &worktree {
                    Some(worktree) => (Vec::new(), None, worktree.port),
                    None if Self::in_worktree(&target.folder.to_string_lossy()) => {
                        return Err("Baaz is still reading this worktree's ports; try again in a moment.".into());
                    }
                    None => {
                        let taken = host.read(cx).run_port_blocks();
                        let block = worktrees::run_port_block(&taken, worktrees::port_free).ok_or("Every port block is taken.")?;
                        (worktrees::plain_env(&target.folder, block), Some(block), block)
                    }
                };
                let title = crate::terminal::title_from_command(&command);
                let origin = Some(target.session.clone());
                let tab = host.update(cx, |host, cx| host.open_run_tab(&target.tab_root, &target.folder, title, origin, env, block, cx));
                (tab, port)
            }
        };
        let before = host.read(cx).get(&tab).map(|state| state.session.read(cx).blocks().len()).unwrap_or(0);
        if let Some(entity) = host.read(cx).get(&tab).map(|state| state.session.clone()) {
            entity.update(cx, |session, _| {
                session.paste(&command);
                session.write(b"\r");
            });
        }
        let preview = target.config.preview_url.as_deref().map(|url| substitute(url, port));
        let id = host.update(cx, |host, cx| host.record_run(&target.session, &command, &tab, before, preview.is_some(), cx));
        crate::baaz_log!("run: `{command}` in {tab} for {} on port {port}", target.session);
        self.select_right(RightKind::Tasks, cx);
        if let Some(url) = preview {
            match preview_address(&url) {
                Some(address) => self.watch_preview(id, target.session, url, address, cx),
                None => {
                    host.update(cx, |host, _| host.tasks.set_preview(id, Preview::GaveUp));
                    self.overlays.update(cx, |overlays, cx| {
                        overlays.toast("No preview", format!("{url} is not an http or https address."), cx);
                    });
                }
            }
        }
        Ok(RunPressed::Started(id))
    }

    /// Stop run: end the open session's running Run's process group.
    pub(crate) fn stop_project_run(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let session = self.active_id(cx).ok_or("no session is open")?;
        let host = self.terminal_host(cx);
        let id = host.read(cx).tasks.running_run(&session).ok_or("nothing of this session's Run runs")?;
        host.update(cx, |host, cx| host.stop_task(id, cx)).map(|_| ())
    }

    /// Try the preview's address every [`PREVIEW_EVERY`] off the UI thread
    /// until [`next`] says otherwise.
    fn watch_preview(&mut self, task: u64, session: String, url: String, address: (String, u16), cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let started = Instant::now();
            loop {
                let (host, port) = address.clone();
                let answered = cx.background_spawn(async move { answers(&host, port, CONNECT_TIMEOUT) }).await;
                let more = this
                    .update(cx, |this, cx| this.preview_tick(task, &session, &url, &address, answered, started.elapsed(), cx))
                    .unwrap_or(false);
                if !more {
                    break;
                }
                cx.background_executor().timer(PREVIEW_EVERY).await;
            }
        })
        .detach();
    }

    /// One preview try landed. True while the poll should go on.
    #[allow(clippy::too_many_arguments)]
    fn preview_tick(
        &mut self,
        task: u64,
        session: &str,
        url: &str,
        address: &(String, u16),
        answered: bool,
        waited: Duration,
        cx: &mut Context<Self>,
    ) -> bool {
        let host = self.terminal_host(cx);
        let decision = next(answered, host.read(cx).tasks.run_state(task), waited);
        let (verdict, note) = match decision {
            Next::Wait => return true,
            Next::Gone => return false,
            Next::Open => {
                let layout = crate::layout::Layout::get(cx);
                let still_here = self.active_id(cx).as_deref() == Some(session)
                    && layout.right_open
                    && matches!(crate::layout::right_kind(layout), RightKind::Tasks | RightKind::Browser);
                if still_here {
                    self.open_url_in_browser(url.to_owned(), cx);
                    (Preview::Opened, None)
                } else {
                    (Preview::Ready, Some(("Preview is ready", format!("{url} answers. Open it in the session's Page."))))
                }
            }
            // Stopped by the person: they know; no note.
            Next::RunEnded if host.read(cx).tasks.run_state(task).is_some_and(|(state, _)| state == TaskState::Stopped) => {
                (Preview::RunEnded, None)
            }
            Next::RunEnded => {
                (Preview::RunEnded, Some(("The run ended", format!("It stopped before {}:{} answered. Its output is in its terminal.", address.0, address.1))))
            }
            Next::GiveUp => (
                Preview::GaveUp,
                Some(("No preview yet", format!("Nothing answered at {}:{} in {} s. The run goes on.", address.0, address.1, PREVIEW_GIVE_UP.as_secs()))),
            ),
        };
        crate::baaz_log!("run: preview {url} → {verdict:?}");
        // Nothing drawn reads the preview's progress: no notify.
        host.update(cx, |host, _| host.tasks.set_preview(task, verdict));
        if let Some((title, body)) = note {
            self.overlays.update(cx, |overlays, cx| {
                overlays.toast(title, body, cx);
            });
        }
        false
    }

    /// Whether the open session's newest Run has a preview still waiting:
    /// what the `run-wait-preview` runner polls on.
    #[cfg(feature = "probe")]
    pub(crate) fn preview_waiting(&self, cx: &gpui::App) -> bool {
        self.active_id(cx)
            .and_then(|session| self.terminal_host(cx).read(cx).tasks.latest_run(&session))
            .is_some_and(|(_, _, preview)| preview == Preview::Waiting)
    }

    #[cfg(feature = "probe")]
    /// `run`: press Run on the open session. No session, or a folder that
    /// declares no `run`, fails the step; a Run that already runs is shown
    /// (the step's point: no second copy).
    pub(crate) fn step_run(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.run_project(cx).map(|pressed| crate::baaz_log!("steps: run → {pressed:?}"))
    }

    #[cfg(feature = "probe")]
    /// `run-stop`: Stop run on the open session; nothing running fails.
    pub(crate) fn step_run_stop(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.stop_project_run(cx)
    }

    #[cfg(feature = "probe")]
    /// `run-wait-preview[:<s>]`: the runner waits up to that many seconds
    /// (30 by default) while the preview waits; then this checks the Page
    /// opened on it. Anything else — no Run, gave up, the run ended, the
    /// person had moved on — fails the step.
    pub(crate) fn step_run_wait_preview(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let session = self.active_id(cx).ok_or("no session is open")?;
        match self.terminal_host(cx).read(cx).tasks.latest_run(&session) {
            Some((_, _, Preview::Opened)) => Ok(()),
            Some((_, state, preview)) => Err(format!("the preview is {preview:?} (the run is {})", state.label())),
            None => Err("the session has no Run".into()),
        }
    }
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_spellings_of_the_port_are_substituted_and_nothing_else() {
        assert_eq!(substitute("http://127.0.0.1:${BAAZ_PORT}/", 41000), "http://127.0.0.1:41000/");
        assert_eq!(substitute("http://localhost:$BAAZ_PORT", 41000), "http://localhost:41000");
        assert_eq!(substitute("http://h:$BAAZ_PORT/x?p=$BAAZ_PORT", 7), "http://h:7/x?p=7");
        assert_eq!(substitute("http://h:$BAAZ_PORT_COUNT/", 7), "http://h:$BAAZ_PORT_COUNT/", "another name is left alone");
        assert_eq!(substitute("http://h:8080/", 7), "http://h:8080/");
    }

    #[test]
    fn a_preview_address_is_its_host_and_port() {
        assert_eq!(preview_address("http://127.0.0.1:41000/"), Some(("127.0.0.1".into(), 41000)));
        assert_eq!(preview_address("http://localhost"), Some(("localhost".into(), 80)));
        assert_eq!(preview_address("https://app.test/path?q=1"), Some(("app.test".into(), 443)));
        assert_eq!(preview_address("http://[::1]:5173/"), Some(("::1".into(), 5173)));
        assert_eq!(preview_address("http://user@h:9/"), Some(("h".into(), 9)));
        assert_eq!(preview_address("file:///tmp/index.html"), None);
        assert_eq!(preview_address("http://h:notaport/"), None);
        assert_eq!(preview_address("127.0.0.1:41000"), None);
    }

    /// The poller's one try against a real listener: it answers while bound
    /// and not once let go.
    #[test]
    fn a_listener_answers_and_a_closed_port_does_not() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("skipped: this sandbox forbids listening ({error})");
                return;
            }
        };
        let port = listener.local_addr().expect("bound").port();
        assert!(answers("127.0.0.1", port, Duration::from_millis(500)));
        drop(listener);
        assert!(!answers("127.0.0.1", port, Duration::from_millis(500)));
    }

    #[test]
    fn the_poll_opens_on_an_answer_and_says_why_it_stops_otherwise() {
        let running = Some((TaskState::Running, Preview::Waiting));
        assert_eq!(next(false, running, Duration::ZERO), Next::Wait);
        assert_eq!(next(true, running, Duration::ZERO), Next::Open);
        assert_eq!(next(false, running, PREVIEW_GIVE_UP), Next::GiveUp);
        assert_eq!(next(false, Some((TaskState::Exited(1), Preview::Waiting)), Duration::ZERO), Next::RunEnded);
        assert_eq!(next(false, Some((TaskState::Stopped, Preview::Waiting)), Duration::ZERO), Next::RunEnded);
        assert_eq!(next(true, Some((TaskState::Running, Preview::Opened)), Duration::ZERO), Next::Gone, "decided already");
        assert_eq!(next(true, None, Duration::ZERO), Next::Gone, "forgotten");
    }
}
