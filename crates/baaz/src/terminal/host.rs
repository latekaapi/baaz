//! [`TerminalHost`]: the tabs behind the dock, keyed by project root.
//!
//! One [`TerminalHost`] lives on [`Harness`][crate::app::Harness] as a gpui
//! entity. Each tab holds an [`aui_terminal::TerminalSession`]
//! over the library's pty backend (`$SHELL -l -i`, cwd the project root,
//! shell integration carrying a per-tab nonce), plus a title, an owner,
//! the session the tab was opened from, if any, and the shell's pid. The
//! host also keeps the task book: what each session's agent started in
//! these tabs ([`super::tasks`], docs/82 6.5).
//!
//! Tabs belong to the project and outlive session switches (D43). Nothing
//! persists across app restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use std::time::Instant;

use aui_terminal::{ScriptChunk, TermEvent, TerminalBackend, TerminalSession};
use gpui::{App, AppContext as _, Context, Entity};

use super::tasks::{listening_ports, Seen, TaskBook, TaskRow, POLL_EVERY, PORTS_EVERY, QUIT_GRACE, STOP_GRACE};

/// A session id, as [`SessionView`][crate::session::SessionView] spells it.
pub type SessionId = String;

/// Who owns a tab: the person, or Muse.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabOwner {
    /// The person opened the tab.
    User,
    /// Muse opened the tab.
    Agent,
}

/// One terminal tab: a session over a pty, and who it belongs to.
pub struct TerminalTab {
    /// Short stable id (`t1`, `t2`, …), minted in open order.
    pub id: String,
    /// The project this tab belongs to: [`Project::root`][crate::projects::Project].
    pub project_root: PathBuf,
    /// The shell's starting directory: the project root, unless the agent
    /// named another `cwd` at open. What `terminal_list` reports.
    pub cwd: PathBuf,
    /// The tab title, from the command that opened it.
    pub title: String,
    /// Who opened the tab.
    pub owner: TabOwner,
    /// The session the tab was opened from, if any. Written here, read by
    /// the agent route (H1b), which is the only consumer of tab provenance.
    pub origin_session: Option<SessionId>,
    /// The emulator behind the tab.
    pub session: Entity<TerminalSession>,
    /// The tab's shell, when it is a real pty: the session leader whose
    /// foreground group a task's Stop ends (docs/82 6.5).
    pub shell_pid: Option<u32>,
    /// The port block a plain session's Run holds while this tab lives
    /// (docs/82 7.4): its first port, in the shell's `BAAZ_PORT`.
    pub port_block: Option<u16>,
}

/// The pure facts [`pick_tab`] decides on: one row per tab the host holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TabState {
    /// The tab's short id.
    pub id: String,
    /// The project the tab belongs to.
    pub root: PathBuf,
    /// A block is running, or the alternate screen is active.
    pub busy: bool,
}

/// Where a command should run: an existing tab, or a fresh one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Pick {
    /// Run in this tab.
    Existing(String),
    /// Open a new tab.
    New,
}

/// Which tab a command should run in (D43): the project's active tab when it
/// is idle, otherwise a new tab titled from the command.
///
/// `active_id` is the project's active tab, if it names a tab of this
/// project. `prefer_active` is the caller's route choice: `"auto"` honours
/// the idle active tab, `"new"` always opens one. A busy tab (a running
/// block, or the alternate screen) never takes a command.
pub fn pick_tab(
    tabs: &[TabState],
    project_root: &Path,
    active_id: Option<&str>,
    prefer_active: bool,
) -> Pick {
    if !prefer_active {
        return Pick::New;
    }
    let Some(active) = active_id else {
        return Pick::New;
    };
    let idle = tabs.iter().find(|tab| tab.id == active && tab.root == project_root && !tab.busy);
    match idle {
        Some(tab) => Pick::Existing(tab.id.clone()),
        None => Pick::New,
    }
}

#[cfg(feature = "probe")]
/// One deterministic chunk: bytes handed to the grid on the first poll.
fn chunk(bytes: Vec<u8>) -> ScriptChunk {
    ScriptChunk { at: std::time::Duration::from_millis(0), bytes }
}

#[cfg(feature = "probe")]
/// The `--steps` verb's script: two finished blocks and a fresh prompt,
/// every marker carrying `k=<nonce>`.
///
/// One chunk, so a single synchronous drain replays the whole tab and every
/// duration reads `0.0 s` on every run.
pub fn deterministic_script(nonce: &str) -> Vec<ScriptChunk> {
    use base64::Engine;
    // A LONG prompt on purpose. The cursor is positioned by a measured cell
    // advance and the rows are painted by the text system; when those two
    // disagree the error is proportional to the column, so a short prompt
    // hides it. A 13-character prompt showed a ~1-cell drift that read as
    // correct in a capture while the owner's 43-character prompt put the
    // cursor four cells into the text. Keep this at a realistic width so a
    // capture can fail.
    let prompt = "adalovelace@adas-MacBook-Pro.lan acme % ";
    let block = |cmd: &str, output: &str, exit: i32| {
        let cmd = base64::engine::general_purpose::STANDARD.encode(cmd);
        // The newline after `C` is the shell's own: preexec hands the
        // command over, and output starts on the next row — without it the
        // prompt and the output share one grid row and the overlay collides
        // with both. It is `\r\n`, as a pty speaks it: a bare line feed
        // moves down but keeps the column, staggering every later row.
        format!(
            "\x1b]133;A;k={nonce}\x07{prompt}\x1b]133;C;k={nonce};cmd={cmd};enc=b64\x07\r\n{output}\x1b]133;D;{exit};k={nonce}\x07"
        )
    };
    // Coloured exactly as the real tools colour it: `git status -sb` paints
    // its branch line, a test runner paints its ticks, a linter paints the
    // error red. A capture in plain white proves nothing about whether SGR
    // renders, and this capture is the picture the terminal is judged by.
    let bytes = format!(
        "{}{}{}\x1b]133;A;k={nonce}\x07{prompt}",
        block(
            "git status -sb",
            "\x1b[32m## \x1b[1;32mmain\x1b[0m\x1b[32m...origin/main\x1b[0m\r\n\x1b[31m M\x1b[0m src/checkout/validators.ts\r\n\x1b[32mA \x1b[0m src/checkout/validators.test.ts\r\n",
            0,
        ),
        block(
            "pnpm vitest run",
            "\x1b[32m ✓\x1b[0m validators.test.ts \x1b[2m(18)\x1b[0m \x1b[32m412ms\x1b[0m\r\n\x1b[32m ✓\x1b[0m AddressForm.test.tsx \x1b[2m(9)\x1b[0m \x1b[32m1.1s\x1b[0m\r\n\x1b[1;32m Test Files  2 passed\x1b[0m \x1b[2m(2)\x1b[0m\r\n",
            0,
        ),
        block(
            "pnpm lint",
            "\x1b[1msrc/checkout/validators.ts\x1b[0m\r\n\x1b[2m  46:5\x1b[0m  \x1b[31merror\x1b[0m  'validateCanadianPostal' is not defined  \x1b[2mno-undef\x1b[0m\r\n\x1b[1;31m 1 problem\x1b[0m \x1b[2m(1 error, 0 warnings)\x1b[0m\r\n",
            1,
        ),
    );
    vec![chunk(bytes.into_bytes())]
}


/// Extra env for every terminal PTY shell: the identity markers plus the
/// shared env scrub ([`provider::child_env`]) as blanked values. The PTY
/// backend (`aui-terminal`'s `PtyConfig`) only adds vars — it has
/// `with_env` and no removal — so each scrubbed var the parent carries is
/// set to `""` instead of removed: empty is falsy for the node CLIs that
/// read these, which behave as if unset. Deliberately no Baaz homes: a
/// shell the person drives keeps the owner's own `~/.claude` / `~/.codex`,
/// only the inherited desktop-agent vars go.
pub fn pty_extra_env() -> Vec<(String, String)> {
    pty_extra_env_for(provider::child_env::startup_had_entrypoint())
}

/// [`pty_extra_env`] for an explicit entrypoint flag. The PTY tests drive
/// this directly — the flag is process-cached, so tests never depend on
/// when the capture landed.
fn pty_extra_env_for(parent_had_entrypoint: bool) -> Vec<(String, String)> {
    let mut env = vec![
        ("BAAZ_TERMINAL".to_owned(), "1".to_owned()),
        ("CLICOLOR".to_owned(), "1".to_owned()),
    ];
    env.extend(provider::child_env::scrub_overrides_for(parent_had_entrypoint));
    env
}

/// A tab title from a command line: the first line, without a leading
/// `$ ` prompt, capped at 32 characters.
pub fn title_from_command(command: &str) -> String {
    let line = command.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("shell");
    let line = line.strip_prefix("$ ").unwrap_or(line);
    let mut title: String = line.chars().take(32).collect();
    if line.chars().count() > 32 {
        title.push('…');
    }
    title
}

/// The tabs behind the dock. Owned by [`Harness`][crate::app::Harness].
pub struct TerminalHost {
    tabs: Vec<TerminalTab>,
    next_id: u64,
    active: HashMap<PathBuf, String>,
    /// What each session's agent started in these tabs (docs/82 6.5).
    pub(crate) tasks: TaskBook,
    /// The task poll is running ([`Self::ensure_task_poll`]).
    task_polling: bool,
}

impl TerminalHost {
    /// An empty host: no tabs anywhere.
    pub fn new() -> Self {
        Self { tabs: Vec::new(), next_id: 1, active: HashMap::new(), tasks: TaskBook::default(), task_polling: false }
    }

    /// Every tab of one project, in open order.
    pub fn tabs_for(&self, project_root: &Path) -> Vec<&TerminalTab> {
        self.tabs.iter().filter(|tab| tab.project_root == project_root).collect()
    }

    /// The project's active tab, when it still names one of its tabs.
    pub fn active_for(&self, project_root: &Path) -> Option<&TerminalTab> {
        let id = self.active.get(project_root)?;
        self.tabs.iter().find(|tab| tab.project_root == project_root && &tab.id == id)
    }

    /// One tab by id.
    pub fn get(&self, id: &str) -> Option<&TerminalTab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    /// D43's tab choice over the live tabs: [`pick_tab`] with liveness
    /// read now.
    pub fn pick(&self, cx: &App, project_root: &Path, prefer_active: bool) -> Pick {
        let active = self.active.get(project_root).map(String::as_str);
        pick_tab(&self.states(cx), project_root, active, prefer_active)
    }

    /// D43 for a block rerun: the originating tab when it is idle, else a
    /// new tab — [`pick_tab`] with the origin as the candidate.
    pub fn pick_rerun(&self, cx: &App, project_root: &Path, tab_id: &str) -> Pick {
        pick_tab(&self.states(cx), project_root, Some(tab_id), true)
    }

    /// The pure rows [`pick_tab`] decides on, with liveness read now.
    fn states(&self, cx: &App) -> Vec<TabState> {
        self.tabs
            .iter()
            .map(|tab| {
                let session = tab.session.read(cx);
                let busy = session.alt_screen() || session.blocks().iter().any(|block| block.running());
                TabState { id: tab.id.clone(), root: tab.project_root.clone(), busy }
            })
            .collect()
    }

    /// Whether the tab has a running block or owns the alternate screen.
    pub fn busy(&self, cx: &App, id: &str) -> bool {
        self.get(id).is_some_and(|tab| {
            let session = tab.session.read(cx);
            session.alt_screen() || session.blocks().iter().any(|block| block.running())
        })
    }

    /// The tab's running command, for confirmations that name it (D52): the
    /// first still-running block's command, or the tab title when the block
    /// carries none (alt-screen, or an unscraped echo). `None` when the tab
    /// is missing or idle.
    pub fn running_command(&self, cx: &App, id: &str) -> Option<String> {
        let tab = self.get(id)?;
        let session = tab.session.read(cx);
        let command = session.blocks().iter().find(|block| block.running()).map(|block| block.command.clone());
        match command {
            Some(command) if !command.trim().is_empty() => Some(command),
            _ if self.busy(cx, id) => Some(tab.title.clone()),
            _ => None,
        }
    }

    /// Every busy tab's `(tab id, running command)`, in open order: what a
    /// quit confirmation names (D52).
    pub fn running_terminals(&self, cx: &App) -> Vec<(String, String)> {
        self.tabs
            .iter()
            .filter(|tab| {
                let session = tab.session.read(cx);
                session.alt_screen() || session.blocks().iter().any(|block| block.running())
            })
            .filter_map(|tab| self.running_command(cx, &tab.id).map(|command| (tab.id.clone(), command)))
            .collect()
    }

    /// Open a tab on the project's root: `$SHELL -l -i` with shell
    /// integration carrying a per-tab nonce (D45). A shell that fails to
    /// spawn still opens its tab; its grid reports the exit.
    pub fn open(
        &mut self,
        project_root: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> String {
        self.open_with_cwd(project_root, project_root, title, owner, origin_session, cx)
    }

    /// Open a tab of `project_root` with the shell starting in `cwd`: the
    /// agent's `terminal_open` route, whose `cwd` defaults to the project
    /// root. The tab still belongs to the project (D43); only the shell's
    /// starting directory differs.
    pub fn open_with_cwd(
        &mut self,
        project_root: &Path,
        cwd: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> String {
        // `CLICOLOR` is why a macOS terminal looks alive rather than white.
        // The library already advertises `TERM=xterm-256color` and
        // `COLORTERM=truecolor`, but BSD `ls` gates its colour on `CLICOLOR`
        // specifically — measured on this machine, `COLORTERM` alone leaves
        // `ls` monochrome while `CLICOLOR=1` paints it. Advertising a
        // capability is the library's job; deciding that this terminal's
        // tools should use it is the host's, so it is set here. A user who
        // wants it off can unset it in their rc, which runs after this.
        // The scrub rides along ([`pty_extra_env`]): the inherited
        // desktop-agent vars blank, the owner's homes untouched.
        self.open_pty(project_root, cwd, title, owner, origin_session, Vec::new(), cx)
    }

    /// Open the tab a session's Run starts in (docs/82 7.4): a person's
    /// tab of `project_root`, its shell in `cwd`, started with `env` on top
    /// of what every tab gets — a plain session's port block
    /// (`port_block`, held while the tab lives), or nothing extra for a
    /// worktree session, whose tab gets the worktree's variables anyway.
    #[allow(clippy::too_many_arguments)]
    pub fn open_run_tab(
        &mut self,
        project_root: &Path,
        cwd: &Path,
        title: String,
        origin_session: Option<SessionId>,
        env: Vec<(String, String)>,
        port_block: Option<u16>,
        cx: &mut Context<Self>,
    ) -> String {
        let id = self.open_pty(project_root, cwd, title, TabOwner::User, origin_session, env, cx);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) {
            tab.port_block = port_block;
        }
        id
    }

    /// The port blocks plain sessions' Run tabs hold now: what the next
    /// plain Run's block must not be ([`baaz_core::worktrees::run_port_block`]).
    pub fn run_port_blocks(&self) -> Vec<u16> {
        self.tabs.iter().filter_map(|tab| tab.port_block).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn open_pty(
        &mut self,
        project_root: &Path,
        cwd: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        extra: Vec<(String, String)>,
        cx: &mut Context<Self>,
    ) -> String {
        let mut config = aui_terminal::PtyConfig::login(cwd);
        // A session in one of Baaz's worktrees (its root is the worktree):
        // the shell gets the worktree's variables too (docs/82 6.1).
        for (key, value) in pty_extra_env().into_iter().chain(baaz_core::worktrees::cached_env_for_folder(project_root)).chain(extra) {
            config = config.with_env(key, value);
        }
        let nonce = config.nonce().to_owned();
        let mut pty = aui_terminal::Pty::new();
        let _ = pty.spawn_config(&config);
        let shell_pid = pty.child_pid();
        let session = TerminalSession::new(Box::new(pty), 100, 32).with_nonce(&nonce);
        self.push(project_root, cwd, title, owner, origin_session, session, shell_pid, cx)
    }

    #[cfg(feature = "probe")]
    /// Open a tab over a scripted backend: the `--steps` verb's route, so
    /// captures replay the same bytes every run. Markers in `script` must
    /// carry `k=<nonce>`; anything else never becomes a block.
    ///
    /// The bytes are [`aui_terminal::FakePty`]'s script shape,
    /// replayed through a small `Send` backend rather than the fake itself:
    /// the fake's clock is `Rc`, so it cannot cross into
    /// [`TerminalSession`], which drives its backend on a reader thread and
    /// requires `Send`.
    #[allow(clippy::too_many_arguments)]
    pub fn open_fake(
        &mut self,
        project_root: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        script: Vec<ScriptChunk>,
        nonce: &str,
        cx: &mut Context<Self>,
    ) -> String {
        let session = TerminalSession::new(Box::new(ScriptBackend::new(script)), 100, 32).with_nonce(nonce);
        self.push(project_root, project_root, title, owner, origin_session, session, None, cx)
    }

    /// Open a tab over an already-built session: the service tests' route
    /// for scripted backends that answer writes (a recording cannot). The
    /// tab is otherwise ordinary — titled, owned, and made active like
    /// every other open. Test-only: production opens go through
    /// [`open`](Self::open), [`open_with_cwd`](Self::open_with_cwd) or
    /// [`open_fake`](Self::open_fake).
    #[cfg(all(test, feature = "probe"))]
    pub fn open_session(
        &mut self,
        project_root: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        session: TerminalSession,
        cx: &mut Context<Self>,
    ) -> String {
        self.push(project_root, project_root, title, owner, origin_session, session, None, cx)
    }

    /// Open a tab over a real pty spawned from `config`: the task tests'
    /// route to a shell with no rc files of the owner's, whose foreground
    /// group a Stop can end. Test-only, like [`open_session`](Self::open_session).
    #[cfg(all(test, feature = "probe"))]
    pub fn open_config(
        &mut self,
        project_root: &Path,
        config: &aui_terminal::PtyConfig,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        cx: &mut Context<Self>,
    ) -> String {
        let mut pty = aui_terminal::Pty::new();
        let _ = pty.spawn_config(config);
        let shell_pid = pty.child_pid();
        let session = TerminalSession::new(Box::new(pty), 100, 32).with_nonce(config.nonce());
        self.push(project_root, project_root, "shell".to_owned(), owner, origin_session, session, shell_pid, cx)
    }

    #[cfg(feature = "probe")]
    /// Drive one tab's backend until its script is spent, synchronously.
    ///
    /// The `--steps` verb drains a deterministic tab the moment it opens,
    /// so every mark is scanned microseconds apart and finished blocks all
    /// read `0.0 s` — the same pixels on every run. Must run before the
    /// tab's first render: afterwards the reader thread owns the backend.
    pub fn drain(&self, id: &str, cx: &App) {
        if let Some(tab) = self.get(id) {
            for _ in 0..512 {
                if !tab.session.read(cx).pump() {
                    break;
                }
            }
        }
    }

    /// Close a tab. Dropping the session ends its reader thread and kills
    /// its shell; a task still running there ends with it. Notifies (the
    /// dock and the Tasks view draw the tab list).
    pub fn close(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(at) = self.tabs.iter().position(|tab| tab.id == id) {
            let tab = self.tabs.remove(at);
            if self.active.get(&tab.project_root).is_some_and(|active| active == id) {
                let next = self.tabs_for(&tab.project_root).last().map(|tab| tab.id.clone());
                match next {
                    Some(next) => {
                        self.active.insert(tab.project_root, next);
                    }
                    None => {
                        self.active.remove(&tab.project_root);
                    }
                }
            }
            self.publish(true, cx);
        }
    }

    /// Point the project's active tab at `id`, and notify the dock.
    pub fn activate(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(tab) = self.get(id) {
            self.active.insert(tab.project_root.clone(), id.to_owned());
            self.publish(true, cx);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        project_root: &Path,
        cwd: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        session: TerminalSession,
        shell_pid: Option<u32>,
        cx: &mut Context<Self>,
    ) -> String {
        let id = format!("t{}", self.next_id);
        self.next_id += 1;
        let entity = cx.new(|_| session);
        self.tabs.push(TerminalTab {
            id: id.clone(),
            project_root: project_root.to_owned(),
            cwd: cwd.to_owned(),
            title,
            owner,
            origin_session,
            session: entity,
            shell_pid,
            port_block: None,
        });
        self.active.insert(project_root.to_owned(), id.clone());
        id
    }
}

// ── tasks (docs/82 6.5) ─────────────────────────────────────────────────

impl TerminalHost {
    /// Record a command `session`'s agent started in `tab` (its block will
    /// be `block`) and make sure the poll follows it.
    pub fn record_task(&mut self, session: &str, command: &str, tab: &str, block: usize, cx: &mut Context<Self>) {
        self.tasks.record(session, command, tab, block);
        self.ensure_task_poll(cx);
    }

    /// Record the session's Run (docs/82 7.4) started in `tab` (its block
    /// will be `block`) and make sure the poll follows it. Returns its id.
    pub fn record_run(&mut self, session: &str, command: &str, tab: &str, block: usize, preview: bool, cx: &mut Context<Self>) -> u64 {
        let id = self.tasks.record_run(session, command, tab, block, preview);
        self.ensure_task_poll(cx);
        id
    }

    /// The foreground process group of `tab`'s shell — the command running
    /// at its prompt — when the tab is a real pty and something runs there.
    fn foreground_group(&self, tab: &str) -> Option<i32> {
        let pid = i32::try_from(self.get(tab)?.shell_pid?).ok()?;
        provider::runtime::foreground_group(pid)
    }

    /// Stop task `id`: end its process group (SIGTERM, SIGKILL after
    /// [`STOP_GRACE`]) off the UI thread; the shell stays. The task reads
    /// `stopped` once its block closes. `Err` when it is not running, its
    /// tab is gone, or no group of its own runs in the tab's foreground —
    /// then nothing was signalled.
    pub fn stop_task(&mut self, id: u64, cx: &mut Context<Self>) -> Result<i32, String> {
        let (tab, block) = self.tasks.stop_target(id)?;
        let tab_state = self.get(&tab).ok_or_else(|| format!("tab {tab} is gone"))?;
        // The task's own block must be what runs (or be about to form): a
        // later command in the same tab is someone else's.
        let blocks = tab_state.session.read(cx).blocks();
        if blocks.get(block).is_some_and(|block| !block.running()) {
            return Err(format!("task {id} has finished"));
        }
        let group = self.foreground_group(&tab).ok_or_else(|| format!("no process group of its own runs in tab {tab}"))?;
        self.tasks.mark_stopping(id);
        cx.background_spawn(async move {
            provider::runtime::end_groups(&[group], STOP_GRACE);
        })
        .detach();
        self.ensure_task_poll(cx);
        Ok(group)
    }

    /// End every running task's group at once, waiting at most
    /// [`QUIT_GRACE`] before SIGKILL: the quit path, so nothing the agent
    /// started in a tab outlives Baaz (a server that ignores the hang-up
    /// the closing pty sends). Blocking, briefly; only quit calls it.
    pub fn end_all_tasks(&self, cx: &App) {
        let groups: Vec<i32> = self
            .tasks
            .running()
            .into_iter()
            .filter(|(_, tab, block)| {
                self.get(tab).is_some_and(|state| state.session.read(cx).blocks().get(*block).is_none_or(|block| block.running()))
            })
            .filter_map(|(_, tab, _)| self.foreground_group(&tab))
            .collect();
        if !groups.is_empty() {
            provider::runtime::end_groups(&groups, QUIT_GRACE);
        }
    }

    /// These sessions are gone: their finished tasks go (running ones stay
    /// until they end).
    pub fn forget_tasks(&mut self, sessions: &[String], cx: &mut Context<Self>) {
        self.tasks.forget_finished(sessions);
        self.ensure_task_poll(cx);
    }

    /// A session answers as `to` now: its tasks follow.
    pub fn rekey_tasks(&mut self, from: &str, to: &str, cx: &mut Context<Self>) {
        self.tasks.rekey(from, to);
        self.ensure_task_poll(cx);
    }

    /// Run [`Self::poll_tasks`] now and every [`POLL_EVERY`] while it says
    /// there is more to follow. Idempotent.
    pub fn ensure_task_poll(&mut self, cx: &mut Context<Self>) {
        if self.task_polling {
            return;
        }
        self.task_polling = true;
        cx.spawn(async move |this, cx| loop {
            let more = this.update(cx, |host, cx| host.poll_tasks(cx)).unwrap_or(false);
            if !more {
                break;
            }
            cx.background_executor().timer(POLL_EVERY).await;
        })
        .detach();
    }

    /// One poll: read each running task's block (and its foreground group),
    /// start a port lookup when the Tasks view looks and one is due, and
    /// notify once when anything drawn changed — or every poll while the
    /// view watches a running task, so its duration ticks. False when there
    /// is nothing left to follow (the poll stops; it clears its own flag in
    /// the same update, so a record that lands next restarts it).
    pub fn poll_tasks(&mut self, cx: &mut Context<Self>) -> bool {
        let more = self.publish(false, cx);
        if !more {
            self.task_polling = false;
        }
        more
    }

    /// The host's one notify: settle the task book against the tabs, start
    /// a port lookup when due, and notify when `tabs_changed` (a tab closed
    /// or the active one moved), when a task changed, or while the Tasks view
    /// watches a running task (its duration ticks). True while something
    /// is left to follow.
    fn publish(&mut self, tabs_changed: bool, cx: &mut Context<Self>) -> bool {
        let tabs = &self.tabs;
        self.tasks.settle(|tab, block| {
            let Some(state) = tabs.iter().find(|candidate| candidate.id == tab) else { return Seen::Gone };
            // A tab the dock never drew has no reader thread: drain it here,
            // as the service does for the runs it waits on.
            let session = state.session.read(cx);
            session.pump();
            match session.blocks().get(block) {
                None => Seen::NotYet,
                Some(block) if block.running() => Seen::Running,
                Some(block) => Seen::Finished { exit: block.exit, ended: block.ended },
            }
        });
        for (id, tab, _) in self.tasks.running() {
            let group = self.foreground_group(&tab);
            self.tasks.set_group(id, group);
        }
        let watched = self.tasks.watched();
        let running = self.tasks.any_running();
        let ports_due = self.tasks.ports_at.is_none_or(|at| at.elapsed() >= PORTS_EVERY);
        let groups = self.tasks.running_groups();
        if watched && running && ports_due && !self.tasks.ports_in_flight && !groups.is_empty() {
            self.tasks.ports_in_flight = true;
            self.tasks.ports_at = Some(Instant::now());
            cx.spawn(async move |this, cx| {
                let ports = cx.background_spawn(async move { listening_ports(&groups) }).await;
                let _ = this.update(cx, |host, _| {
                    host.tasks.set_ports(&ports);
                    host.tasks.ports_in_flight = false;
                });
            })
            .detach();
        }
        if self.tasks.take_dirty() || tabs_changed || (watched && running) {
            cx.notify();
        }
        running || self.tasks.ports_in_flight
    }

    /// `session`'s tasks now, newest first, for the Tasks view; marks the
    /// view as watching, so running durations tick.
    pub fn task_rows(&self, session: &str) -> Vec<TaskRow> {
        self.tasks.watch();
        self.tasks.rows(session, Instant::now())
    }
}

impl Default for TerminalHost {
    fn default() -> Self {
        Self::new()
    }
}

/// A scripted [`TerminalBackend`]: one [`ScriptChunk`] per poll, then
/// silence. Writes and resizes are accepted and ignored — a recording
/// cannot answer.
///
/// [`aui_terminal::FakePty`]'s shape without its clock, so the
/// backend is `Send` and can ride [`TerminalSession`]'s reader thread.
pub(crate) struct ScriptBackend {
    script: Vec<ScriptChunk>,
    next: usize,
}

impl ScriptBackend {
    pub(crate) fn new(script: Vec<ScriptChunk>) -> Self {
        Self { script, next: 0 }
    }
}

impl TerminalBackend for ScriptBackend {
    fn spawn(&mut self, _shell: &str, _cwd: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn write(&mut self, _bytes: &[u8]) {}

    fn resize(&mut self, _cols: u16, _rows: u16) {}

    fn poll(&mut self) -> Vec<TermEvent> {
        match self.script.get(self.next) {
            Some(chunk) => {
                self.next += 1;
                vec![TermEvent::Output(chunk.bytes.clone())]
            }
            None => Vec::new(),
        }
    }
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;

    fn tab(id: &str, root: &str, busy: bool) -> TabState {
        TabState { id: id.into(), root: PathBuf::from(root), busy }
    }

    #[test]
    fn an_idle_active_tab_takes_the_command() {
        let tabs = vec![tab("t1", "/acme", false)];
        assert_eq!(pick_tab(&tabs, Path::new("/acme"), Some("t1"), true), Pick::Existing("t1".into()));
    }

    #[test]
    fn a_busy_active_tab_opens_a_new_one() {
        let tabs = vec![tab("t1", "/acme", true)];
        assert_eq!(pick_tab(&tabs, Path::new("/acme"), Some("t1"), true), Pick::New);
    }

    #[test]
    fn no_active_tab_opens_a_new_one() {
        let tabs = vec![tab("t1", "/acme", false)];
        assert_eq!(pick_tab(&tabs, Path::new("/acme"), None, true), Pick::New);
    }

    #[test]
    fn an_active_tab_of_another_project_is_not_picked() {
        let tabs = vec![tab("t1", "/other", false)];
        assert_eq!(pick_tab(&tabs, Path::new("/acme"), Some("t1"), true), Pick::New);
    }

    #[test]
    fn asking_for_new_never_picks_the_idle_tab() {
        let tabs = vec![tab("t1", "/acme", false)];
        assert_eq!(pick_tab(&tabs, Path::new("/acme"), Some("t1"), false), Pick::New);
    }

    #[test]
    fn titles_come_from_the_command() {
        assert_eq!(title_from_command("pnpm vitest"), "pnpm vitest");
        assert_eq!(title_from_command("$ git status -sb"), "git status -sb");
        assert_eq!(title_from_command(""), "shell");
    }

    struct SavedScrubEnv {
        vars: Vec<(String, Option<std::ffi::OsString>)>,
    }

    impl SavedScrubEnv {
        fn save(names: &[&str]) -> Self {
            Self { vars: names.iter().map(|name| ((*name).to_owned(), std::env::var_os(name))).collect() }
        }
    }

    impl Drop for SavedScrubEnv {
        fn drop(&mut self) {
            for (name, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn pty_shells_keep_markers_and_blank_the_desktop_inheritance() {
        // The entrypoint flag is process-cached, so the explicit-flag
        // builder carries the flag while the parent env carries the
        // names. The shared lock serializes against the other tests in
        // this binary that touch these names.
        let _guard =
            crate::connect::SCRUB_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedScrubEnv::save(&[
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
        ]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "s-desktop");
        std::env::set_var("ANTHROPIC_BASE_URL", "http://desktop:2000");
        std::env::set_var("ANTHROPIC_API_KEY", "owner-key");

        let env = pty_extra_env_for(true);
        // The markers that make the terminal Baaz's and colourful.
        assert!(env.contains(&("BAAZ_TERMINAL".to_owned(), "1".to_owned())));
        assert!(env.contains(&("CLICOLOR".to_owned(), "1".to_owned())));
        // The inherited desktop vars blank — never the homes.
        for name in ["CLAUDECODE", "CLAUDE_CODE_SESSION_ID", "ANTHROPIC_BASE_URL"] {
            assert!(
                env.contains(&(name.to_owned(), String::new())),
                "scrubbed vars blank: {name}"
            );
        }
        assert!(
            !env.iter().any(|(name, _)| name == "CLAUDE_CONFIG_DIR" || name == "CODEX_HOME"),
            "PTY shells keep the owner's homes: {env:?}"
        );
        assert!(
            !env.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY"),
            "owner auth is untouched: {env:?}"
        );
    }

    #[test]
    fn pty_shells_keep_an_owner_base_url_off_a_plain_launch() {
        let _guard =
            crate::connect::SCRUB_ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedScrubEnv::save(&["ANTHROPIC_BASE_URL"]);
        std::env::set_var("ANTHROPIC_BASE_URL", "https://owner-proxy:8443");
        let env = pty_extra_env_for(false);
        assert!(
            !env.iter().any(|(name, _)| name == "ANTHROPIC_BASE_URL"),
            "an owner-set value on a plain launch survives: {env:?}"
        );
    }

    /// A rerun goes through [`pick_tab`] with the originating tab as the
    /// active one: an idle tab takes the command, a busy one opens a new
    /// tab (D43). The rule the unit tests pin above is the rule a rerun
    /// runs — this names it once, so the wiring has a test to answer to.
    #[test]
    fn rerun_reuses_an_idle_tab_and_opens_a_new_one_when_busy() {
        let root = Path::new("/acme");
        let idle = vec![tab("t1", "/acme", false)];
        assert_eq!(pick_tab(&idle, root, Some("t1"), true), Pick::Existing("t1".into()));
        let busy = vec![tab("t1", "/acme", true)];
        assert_eq!(pick_tab(&busy, root, Some("t1"), true), Pick::New);
    }

    /// One scripted chunk of raw grid bytes, like [`deterministic_script`].
    fn raw(bytes: String) -> aui_terminal::ScriptChunk {
        aui_terminal::ScriptChunk { at: std::time::Duration::from_millis(0), bytes: bytes.into_bytes() }
    }

    /// A tab whose one block is still running: `C` arrived, `D` never did.
    /// The command rides the `C` marker's payload, as our shell integration
    /// sends it.
    fn running_script(nonce: &str, command: &str) -> Vec<aui_terminal::ScriptChunk> {
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(command);
        vec![raw(format!(
            "\x1b]133;A;k={nonce}\x07~/work ❯ \x1b]133;C;k={nonce};cmd={encoded};enc=b64\x07\r\npartial output"
        ))]
    }

    fn open_scripted(
        host: &gpui::Entity<TerminalHost>,
        cx: &mut gpui::App,
        title: &str,
        origin: Option<SessionId>,
        script: Vec<aui_terminal::ScriptChunk>,
        nonce: &str,
    ) -> String {
        let root = PathBuf::from("/acme");
        host.update(cx, |host, cx| {
            let id = host.open_fake(&root, title.to_owned(), TabOwner::User, origin, script, nonce, cx);
            host.drain(&id, cx);
            id
        })
    }

    /// The confirmation names the running block's command, and stays quiet
    /// for a finished block.
    #[gpui::test]
    fn the_running_command_is_the_block_still_open(cx: &mut gpui::TestAppContext) {
        let host = cx.new(|_| TerminalHost::new());
        let nonce = "confirm-nonce";
        let running =
            cx.update(|cx| open_scripted(&host, cx, "sleep 300", Some("s1".into()), running_script(nonce, "sleep 300"), nonce));
        let done = cx.update(|cx| {
            open_scripted(&host, cx, "shell", Some("s1".into()), deterministic_script("done-nonce"), "done-nonce")
        });
        cx.read(|cx| {
            let host = host.read(cx);
            assert!(host.busy(cx, &running));
            assert_eq!(host.running_command(cx, &running).as_deref(), Some("sleep 300"));
            assert!(!host.busy(cx, &done));
            assert_eq!(host.running_command(cx, &done), None);
            assert_eq!(host.running_command(cx, "t404"), None);
        });
    }

    /// The dock's render path only reads (`tabs_for`, `active_for`, `get`,
    /// `pick`, `busy`): querying a fresh host must never create a tab, so a
    /// render alone can never spawn a shell — only `open`/`open_fake`, the
    /// explicit-action routes, create sessions.
    #[test]
    fn read_only_queries_never_open_tabs() {
        let host = TerminalHost::new();
        let root = Path::new("/acme");
        assert!(host.tabs_for(root).is_empty());
        assert!(host.active_for(root).is_none());
        assert!(host.get("t1").is_none());
        assert!(
            host.tabs_for(root).is_empty(),
            "queries must not create tabs: a render alone spawns nothing"
        );
    }
}
