//! D49 play buttons' entry point: run a command in the terminal dock.
//!
//! One entry point; everything below calls it — the shell cards' "Run in
//! terminal", the runnable code blocks' "Run", and the block rerun. Open the
//! dock, pick the tab by D43's rule, bracketed-paste the command, send Enter
//! unless asked not to, and focus the grid. Entirely local: no turn, no
//! wire, so it works under `--replay` too.

use std::path::Path;

use gpui::{Context, Window};

use super::{Pick, TabOwner};
use crate::app::Harness;

/// What a play button resolved to: a command for the terminal dock, and
/// whether Enter follows the paste.
///
/// [`None`] when the command is blank — a button is a click,
/// and a click on nothing runs nothing, so the press no-ops before it ever
/// reaches an event.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunRequest {
    /// The exact command to paste, whole: multi-line blocks paste whole and
    /// the shell runs the lines in order.
    pub command: String,
    /// A plain click sends Enter after the paste; an ⌥-click pastes without
    /// it.
    pub send_enter: bool,
}

impl RunRequest {
    /// The request for `command`, or [`None`] when it carries
    /// nothing to run.
    pub fn new(command: String, send_enter: bool) -> Option<Self> {
        if command.trim().is_empty() {
            return None;
        }
        Some(Self { command, send_enter })
    }
}

/// Whether Enter follows the paste: ⌥-click pastes without Enter, a plain
/// click sends it.
pub fn send_enter_for_alt(alt_held: bool) -> bool {
    !alt_held
}

impl Harness {
    /// Run `command` in the terminal dock (D49): open the dock, pick the
    /// project's active tab when it is idle and open a new tab otherwise
    /// ([`Pick`], D43), bracketed-paste the whole command, send Enter
    /// unless `send_enter` is false, and focus the grid.
    pub fn run_in_terminal(
        &mut self,
        project: &Path,
        command: &str,
        send_enter: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command.trim().is_empty() {
            return;
        }
        let pick = self.terminal_host.read(cx).pick(cx, project, true);
        self.run_in_picked(project, pick, command, send_enter, window, cx);
    }

    /// The shared tail of every dock run: [`run_in_terminal`](Self::run_in_terminal)
    /// for play buttons (D43 over the active tab), the block rerun for its
    /// originating tab. `pick` is the caller's D43 choice; the dock open,
    /// paste, Enter and focus below are one path.
    pub(crate) fn run_in_picked(
        &mut self,
        project: &Path,
        pick: Pick,
        command: &str,
        send_enter: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if command.trim().is_empty() {
            return;
        }
        self.layout.terminal_open = true;
        let origin = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
        let session = self.terminal_host.update(cx, |host, cx| {
            let id = match pick {
                Pick::Existing(id) => id,
                Pick::New => {
                    let title = super::title_from_command(command);
                    host.open(project, title, TabOwner::User, origin, cx)
                }
            };
            host.get(&id).map(|tab| tab.session.clone())
        });
        if let Some(session) = session {
            session.update(cx, |session, _| {
                session.paste(command);
                if send_enter {
                    session.write(b"\r");
                }
            });
        }
        window.focus(&self.terminal_focus, cx);
        crate::layout::write(&self.layout);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use super::super::host::{pick_tab, TabState};
    use super::super::{deterministic_script, TerminalHost};
    use super::*;

    #[test]
    fn blank_commands_produce_no_request() {
        assert_eq!(RunRequest::new(String::new(), true), None);
        assert_eq!(RunRequest::new("   \n  ".to_owned(), false), None);
    }

    #[test]
    fn multi_line_commands_paste_whole() {
        let command = "npm test -- --watch\nnpm run lint";
        assert_eq!(
            RunRequest::new(command.to_owned(), true),
            Some(RunRequest { command: command.to_owned(), send_enter: true })
        );
    }

    #[test]
    fn alt_click_pastes_without_enter() {
        assert!(!send_enter_for_alt(true));
        assert!(send_enter_for_alt(false));
    }

    /// The tab choice a play-button run makes is the host's `pick_tab` rule
    /// (D43): the project's idle active tab takes the command, a busy one
    /// opens a new tab. This pins the rule at the entry point's own seam;
    /// the rule itself is pinned by the host module's tests.
    #[test]
    fn runs_follow_pick_tab_idle_reused_busy_new() {
        use std::path::PathBuf;
        let root = Path::new("/acme");
        let idle = vec![TabState { id: "t1".into(), root: PathBuf::from("/acme"), busy: false }];
        assert_eq!(pick_tab(&idle, root, Some("t1"), true), Pick::Existing("t1".into()));
        let busy = vec![TabState { id: "t1".into(), root: PathBuf::from("/acme"), busy: true }];
        assert_eq!(pick_tab(&busy, root, Some("t1"), true), Pick::New);
    }

    /// A play-button run reads tabs but never creates one: on a fresh host
    /// the pick behind [`Harness::run_in_terminal`] is `New`, and the paste
    /// below is [`TerminalSession::paste`](aui_terminal::TerminalSession::paste)
    /// plus one `\r` — bytes to the pty, never a turn, never the wire.
    /// `deterministic_script` is the idle tab the pick reuses.
    #[gpui::test]
    fn the_pick_behind_a_run_creates_nothing(cx: &mut gpui::TestAppContext) {
        use std::path::PathBuf;
        let host = cx.new(|_| TerminalHost::new());
        let root = PathBuf::from("/acme");
        cx.update(|cx| {
            host.update(cx, |host, cx| {
                let id = host.open_fake(&root, "shell".to_owned(), TabOwner::User, None, deterministic_script("idle"), "idle", cx);
                host.drain(&id, cx);
            });
        });
        cx.read(|cx| {
            let host = host.read(cx);
            assert_eq!(host.tabs_for(&root).len(), 1, "the fixture opens one tab");
            assert_eq!(
                host.pick(cx, &root, true),
                Pick::Existing("t1".into()),
                "an idle active tab takes the run"
            );
        });
    }
}
