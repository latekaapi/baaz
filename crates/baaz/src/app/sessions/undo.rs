//! Undo through the window (docs/82 6.4): the confirm dialog, the toast
//! and its Undo, the session menu's "Undo last turn", and Changes'
//! "Revert file". What can be undone and the git work are the session's
//! ([`crate::session::undo`], [`crate::undo`]); here the window asks,
//! confirms and reports.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// The confirm dialog's target: the session and the ask's number, so only
/// that confirm's own button runs it.
fn undo_target(session_id: &str, seq: u64) -> String {
    format!("{session_id}\n{seq}")
}

fn parse_undo_target(target: &str) -> Option<(&str, u64)> {
    let (session_id, seq) = target.split_once('\n')?;
    Some((session_id, seq.parse().ok()?))
}

impl Harness {
    /// The runtime of `session_id`, wherever it is held.
    fn undo_runtime(&self, session_id: &str, cx: &gpui::App) -> Option<Entity<SessionRuntime>> {
        self.registry.find_runtime(self.active.as_ref(), session_id, cx)
    }

    /// Whether the session menu offers "Undo last turn": the open
    /// session has a finished turn Undo can act on now (rule 13).
    pub(crate) fn undo_last_offered(&self, cx: &gpui::App) -> bool {
        self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).last_undoable_turn().is_some())
    }

    /// The session menu's "Undo last turn": the confirm for the open
    /// session's newest undoable turn.
    pub(crate) fn undo_last_turn(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        let runtime = self.active.as_ref().map(|view| view.read(cx).runtime().clone()).ok_or("no session is open")?;
        let turn = runtime.read(cx).last_undoable_turn().ok_or("no finished turn can be undone now")?;
        runtime.update(cx, |rt, cx| rt.ask_undo_turn(&turn, cx))
    }

    /// Put the confirm up (a [`SessionEvent::UndoConfirm`]).
    pub(super) fn open_undo_confirm(&mut self, session_id: &str, seq: u64, title: &str, detail: &str, primary: &'static str, cx: &mut Context<Self>) {
        self.set_dialog(cx, Dialog {
            title: title.to_owned(),
            detail: detail.to_owned(),
            kind: DialogKind::Warning,
            primary,
            action: DialogAction::UndoChanges,
            archive_target: Some(undo_target(session_id, seq)),
        });
    }

    #[cfg(feature = "probe")]
    /// `undo-confirm`: press the open Undo / Revert confirm's primary, the
    /// button's own path ([`Self::confirm_undo_dialog`]). Danger dialogs
    /// take no Enter, so a scripted run needs this. No such dialog open
    /// fails the step.
    pub(crate) fn step_undo_confirm(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let target = self
            .overlays
            .read(cx)
            .dialog()
            .filter(|d| d.action == DialogAction::UndoChanges)
            .map(|d| d.archive_target.clone())
            .ok_or("no undo or revert confirm is open")?;
        self.confirm_undo_dialog(target, cx);
        Ok(())
    }

    /// The confirm's Undo / Revert: the action its target names.
    pub(crate) fn confirm_undo_dialog(&mut self, target: Option<String>, cx: &mut Context<Self>) {
        self.close_dialog(cx);
        let Some((session_id, seq)) = target.as_deref().and_then(parse_undo_target) else { return };
        let Some(runtime) = self.undo_runtime(session_id, cx) else { return };
        if let Err(why) = runtime.update(cx, |rt, cx| rt.confirm_undo(seq, cx)) {
            crate::baaz_log!("undo: not confirmed ({why})");
            self.overlays.update(cx, |overlays, cx| {
                overlays.toast("Nothing was changed", "Undo can't act on this session right now.", cx);
            });
        }
    }

    /// An action landed or was refused (a [`SessionEvent::UndoLanded`]):
    /// the toast — with Undo when it can be taken back — and Changes reads
    /// the folder again.
    pub(super) fn undo_landed(&mut self, session_id: &str, title: &str, body: &str, undo: Option<u64>, cx: &mut Context<Self>) {
        match undo {
            Some(n) => self.push_undo(UndoBatch::Files { session_id: session_id.to_owned(), n }, title.to_owned(), body.to_owned(), cx),
            None => {
                self.overlays.update(cx, |overlays, cx| {
                    overlays.toast(title, body, cx);
                });
            }
        }
        self.refresh_right_now(cx);
    }

    /// The toast's Undo on an undo or a revert: take it back.
    pub(crate) fn take_back_undo(&mut self, session_id: &str, n: u64, cx: &mut Context<Self>) {
        let Some(runtime) = self.undo_runtime(session_id, cx) else { return };
        if let Err(why) = runtime.update(cx, |rt, cx| rt.take_back(n, cx)) {
            crate::baaz_log!("undo: not taken back ({why})");
        }
    }

    /// Changes' "Revert" on a row: the confirm for `path` in `root`, back
    /// to `HEAD` or out of the turn `turn` (its id, number and ends).
    pub(crate) fn revert_file_from_changes(
        &mut self,
        root: &std::path::Path,
        path: &str,
        turn: Option<(String, usize, String, String)>,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let runtime = self.active.as_ref().map(|view| view.read(cx).runtime().clone()).ok_or("no session is open")?;
        runtime.update(cx, |rt, cx| rt.ask_revert_file(root, path, turn, cx))
    }
}

impl Harness {
    #[cfg(feature = "probe")]
    /// `undo-turn:last` (docs/82 6.4): the confirm for the open session's
    /// newest undoable turn, through the menu's own path. A capture takes
    /// no checkpoints and runs in the owner's repository, so there the
    /// confirm comes from fixture data (the newest agent turn that edited
    /// files) and cannot apply. `Err` with no session, an unknown payload,
    /// or nothing to undo.
    pub(crate) fn step_undo_turn(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if rest.trim() != "last" {
            return Err(format!("unknown undo-turn payload {rest:?} (expected `last`)"));
        }
        if crate::runtime::captures() {
            let runtime = self.active.as_ref().map(|view| view.read(cx).runtime().clone()).ok_or("no session is open")?;
            return runtime.update(cx, |rt, cx| rt.fixture_undo_confirm(cx));
        }
        self.undo_last_turn(cx)
    }

    #[cfg(feature = "probe")]
    /// `revert-file:<path>` (docs/82 6.4): the confirm for that changed
    /// file in All changes (back to `HEAD`), through the row's own path.
    /// In a capture the confirm comes from fixture data and cannot apply.
    /// `Err` with no session or project, or no such changed row.
    pub(crate) fn step_revert_file(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let path = rest.trim();
        if path.is_empty() {
            return Err("revert-file needs a project-relative path".into());
        }
        let (root, _) = self.right_project(cx).ok_or("no project is open")?;
        let listed = self
            .with_right_pane(cx, |pane, cx| {
                if pane.cache.git_for(&root).is_none() {
                    pane.refresh_now(cx);
                }
                pane.shown_change(&root, path)
            })
            .is_some();
        if !listed {
            return Err(format!("{path} is not a changed row in Changes"));
        }
        if crate::runtime::captures() {
            let runtime = self.active.as_ref().map(|view| view.read(cx).runtime().clone()).ok_or("no session is open")?;
            runtime.update(cx, |rt, cx| rt.fixture_revert_confirm(&root, path, cx));
            return Ok(());
        }
        self.revert_file_from_changes(&root, path, None, cx)
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod tests {
    #[test]
    fn the_confirm_target_round_trips() {
        assert_eq!(super::parse_undo_target(&super::undo_target("s-1", 7)), Some(("s-1", 7)));
        assert_eq!(super::parse_undo_target("s-1"), None);
    }
}
