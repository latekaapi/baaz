//! [`TerminalHost`]: the tabs behind the dock, keyed by project root.
//!
//! One [`TerminalHost`] lives on [`Harness`][crate::app::Harness] as a gpui
//! entity. Each tab holds an [`aui_terminal::TerminalSession`]
//! over the library's pty backend (`$SHELL -l -i`, cwd the project root,
//! shell integration carrying a per-tab nonce), plus a title, an owner and
//! the session the tab was opened from, if any.
//!
//! Tabs belong to the project and outlive session switches (D43). Nothing
//! persists across app restarts.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use aui_terminal::{ScriptChunk, TermEvent, TerminalBackend, TerminalSession};
use gpui::{App, AppContext as _, Context, Entity};

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
    /// The tab title, from the command that opened it.
    pub title: String,
    /// Who opened the tab.
    pub owner: TabOwner,
    /// The session the tab was opened from, if any. Written here, read by
    /// the agent route (H1b), which is the only consumer of tab provenance.
    #[allow(dead_code)]
    pub origin_session: Option<SessionId>,
    /// The emulator behind the tab.
    pub session: Entity<TerminalSession>,
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

/// One deterministic chunk: bytes handed to the grid on the first poll.
fn chunk(bytes: Vec<u8>) -> ScriptChunk {
    ScriptChunk { at: std::time::Duration::from_millis(0), bytes }
}

/// The `--steps` verb's script: two finished blocks and a fresh prompt,
/// every marker carrying `k=<nonce>`.
///
/// One chunk, so a single synchronous drain replays the whole tab and every
/// duration reads `0.0 s` on every run.
pub fn deterministic_script(nonce: &str) -> Vec<ScriptChunk> {
    use base64::Engine;
    let prompt = "~/work/acme ❯ ";
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
    let bytes = format!(
        "{}{}\x1b]133;A;k={nonce}\x07{prompt}",
        block("git status -sb", "## main...origin/main\r\n", 0),
        block("pnpm vitest", " ✓ 12 passed\r\n", 0),
    );
    vec![chunk(bytes.into_bytes())]
}

/// Maps a gpui key name onto the encoder's input, mirroring the grid's own
/// routing: single characters pass through as text; the rest is the
/// terminal's special-key set. Anything unknown returns `None` and is
/// ignored.
pub(crate) fn key_input(key: &str) -> Option<aui_terminal::keys::KeyInput> {
    use aui_terminal::keys::{KeyInput, SpecialKey};
    if key.chars().count() == 1 {
        return key.chars().next().map(KeyInput::Text);
    }
    let special = match key {
        "enter" => SpecialKey::Enter,
        "tab" => SpecialKey::Tab,
        "backspace" => SpecialKey::Backspace,
        "escape" => SpecialKey::Escape,
        "left" => SpecialKey::Left,
        "up" => SpecialKey::Up,
        "right" => SpecialKey::Right,
        "down" => SpecialKey::Down,
        "home" => SpecialKey::Home,
        "end" => SpecialKey::End,
        "insert" => SpecialKey::Insert,
        "delete" => SpecialKey::Delete,
        "pageup" => SpecialKey::PageUp,
        "pagedown" => SpecialKey::PageDown,
        "space" => return Some(KeyInput::Text(' ')),
        _ => {
            if let Some(number) = key.strip_prefix('f') {
                if let Ok(n) = number.parse::<u8>() {
                    if (1..=20).contains(&n) {
                        return Some(KeyInput::Key(SpecialKey::F(n)));
                    }
                }
            }
            return None;
        }
    };
    Some(KeyInput::Key(special))
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
}

impl TerminalHost {
    /// An empty host: no tabs anywhere.
    pub fn new() -> Self {
        Self { tabs: Vec::new(), next_id: 1, active: HashMap::new() }
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
        let config =
            aui_terminal::PtyConfig::login(project_root).with_env("BAAZ_TERMINAL", "1");
        let nonce = config.nonce().to_owned();
        let mut pty = aui_terminal::Pty::new();
        let _ = pty.spawn_config(&config);
        let session = TerminalSession::new(Box::new(pty), 100, 32).with_nonce(&nonce);
        self.push(project_root, title, owner, origin_session, session, cx)
    }

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
        self.push(project_root, title, owner, origin_session, session, cx)
    }

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
    /// its shell.
    pub fn close(&mut self, id: &str) {
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
        }
    }

    /// Point the project's active tab at `id`.
    pub fn activate(&mut self, id: &str) {
        if let Some(tab) = self.get(id) {
            self.active.insert(tab.project_root.clone(), id.to_owned());
        }
    }

    fn push(
        &mut self,
        project_root: &Path,
        title: String,
        owner: TabOwner,
        origin_session: Option<SessionId>,
        session: TerminalSession,
        cx: &mut Context<Self>,
    ) -> String {
        let id = format!("t{}", self.next_id);
        self.next_id += 1;
        let entity = cx.new(|_| session);
        self.tabs.push(TerminalTab {
            id: id.clone(),
            project_root: project_root.to_owned(),
            title,
            owner,
            origin_session,
            session: entity,
        });
        self.active.insert(project_root.to_owned(), id.clone());
        id
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
struct ScriptBackend {
    script: Vec<ScriptChunk>,
    next: usize,
}

impl ScriptBackend {
    fn new(script: Vec<ScriptChunk>) -> Self {
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

#[cfg(test)]
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
}
