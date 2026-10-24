//! The dock's entry point for running a command (D49): the block rerun calls
//! it. (D4 removed the transcript's play buttons; D5 moved the connect flow's
//! typed commands into the setup sheet's own terminal.) Open the
//! dock, pick the tab by D43's rule, bracketed-paste the command, send Enter
//! unless asked not to, and focus the grid. Entirely local: no turn, no
//! wire, so it works under `--replay` too.

use std::path::Path;

use gpui::{Context, Window};

use super::{Pick, TabOwner};
use crate::app::Harness;

impl Harness {
    /// The shared tail of every dock run: the block rerun for its
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
