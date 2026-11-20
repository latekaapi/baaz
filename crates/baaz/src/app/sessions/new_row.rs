//! The new-session row (docs/82 6.6, docs/83 §5): on an unsent draft the
//! composer's quiet row starts with the folder control (`project ▾`, the
//! folder picker) and the "Own worktree" toggle. A session with turns
//! shows neither: its folder can no longer change.
//!
//! Both move the draft through the replace a provider switch uses (same
//! provider): the typed text carries over to the replacement and the old
//! draft's lane is hung up ([`Harness::close_replaced`]). The toggle is
//! [`Harness::move_draft_worktree`]; a folder pick is
//! [`Harness::move_draft_to_project`], with the picked project in
//! [`OpenState::folder_pick`] for the replacement's start.
//!
//! What the row shows is pushed onto the session's runtime on every
//! activation ([`Harness::sync_new_row`]); the view draws it and hides it
//! itself once a turn starts.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use super::open::is_local_draft;
use crate::session::pickers::{FolderPick, NewRow, CHOOSE_FOLDER_ROW, PROJECT_ROW_PREFIX};

impl Harness {
    /// The project an unsent draft `session_id` belongs to: the drafts map
    /// names it, else its stored override (a worktree draft's project is the
    /// original one), else — for a local draft, which is always the current
    /// project's — the current project.
    fn draft_project_id(&self, session_id: &str, cx: &gpui::App) -> Option<String> {
        self.open_state
            .draft_project(session_id)
            .map(str::to_owned)
            .or_else(|| self.session_list.read(cx).overrides.get(session_id).and_then(|meta| meta.project.clone()))
            .or_else(|| is_local_draft(session_id).then(|| self.current_project_id()).flatten())
    }

    /// What the new-session row shows for `runtime`'s session, or `None`
    /// when it is not an unsent draft. The worktree toggle is offered where
    /// it works (rule 13): a draft with a lane (every provider opens its
    /// session in the folder it is given; a local draft has no lane to
    /// move), in a project whose root is in a git repository.
    pub(super) fn new_row_for(&self, runtime: &Entity<SessionRuntime>, cx: &gpui::App) -> Option<NewRow> {
        let rt = runtime.read(cx);
        let id = rt.session_id.as_str();
        if rt.has_turns() || !(is_local_draft(id) || self.open_state.is_unsent(id)) {
            return None;
        }
        let folder = rt.workspace_path();
        let project = self.draft_project_id(id, cx).and_then(|project| self.session_list.read(cx).projects.find(&project).cloned());
        let label = match &project {
            Some(project) => project.name.clone(),
            None => std::path::Path::new(folder).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| folder.to_owned()),
        };
        let worktree = project
            .filter(|project| !is_local_draft(id) && baaz_core::worktrees::in_git_repository(&project.root))
            .map(|_| Self::in_worktree(folder));
        Some(NewRow { folder: label, worktree })
    }

    /// Push the new-session row's facts onto `view`'s runtime: on every
    /// activation, so a draft reopened, moved or replaced reads its own.
    pub(super) fn sync_new_row(&self, view: &Entity<SessionView>, cx: &mut Context<Self>) {
        let runtime = view.read(cx).runtime().clone();
        let row = self.new_row_for(&runtime, cx);
        runtime.update(cx, |rt, cx| rt.set_new_row(row, cx));
    }

    /// The folder picker's rows: the adopted projects in sidebar order with
    /// the draft's own first (the picker checks the first row), then
    /// "Choose a folder…".
    pub(crate) fn folder_menu_rows(&self, runtime: &Entity<SessionRuntime>, cx: &gpui::App) -> Vec<aui::composer::PickerRow> {
        let own = self.draft_project_id(&runtime.read(cx).session_id, cx);
        let mut projects = self.ordered_projects(cx);
        if let Some(ix) = own.as_deref().and_then(|own| projects.iter().position(|project| project.id == own)) {
            let first = projects.remove(ix);
            projects.insert(0, first);
        }
        projects
            .into_iter()
            .map(|project| {
                let root = crate::palette::tilde_root(&project.root.to_string_lossy());
                aui::composer::PickerRow::new(format!("{PROJECT_ROW_PREFIX}{}", project.id), project.name, root)
            })
            .chain(std::iter::once(aui::composer::PickerRow::new(CHOOSE_FOLDER_ROW, "Choose a folder…", "Add a folder as a project")))
            .collect()
    }

    /// The folder control asked for its picker: list the projects onto the
    /// view's. `Err` when the open session shows no new-session row.
    pub(crate) fn open_folder_menu(&mut self, view: &Entity<SessionView>, cx: &mut Context<Self>) -> Result<(), String> {
        let runtime = view.read(cx).runtime().clone();
        if runtime.read(cx).new_row().is_none() {
            return Err("the open session is not an unsent draft".into());
        }
        if runtime.read(cx).is_moving() {
            return Err("the draft is already moving".into());
        }
        let rows = self.folder_menu_rows(&runtime, cx);
        view.update(cx, |view, cx| view.open_folder_picker(rows, cx));
        Ok(())
    }

    /// A folder-picker pick on `runtime`'s draft: the draft's own project
    /// does nothing; another moves the draft there; "Choose a folder…"
    /// opens the folder panel first (as Add project does: the folder
    /// becomes a project). A refused move says why.
    pub(super) fn on_folder_pick(&mut self, runtime: Entity<SessionRuntime>, pick: &FolderPick, cx: &mut Context<Self>) {
        match pick {
            FolderPick::Project(project) => {
                if self.draft_project_id(&runtime.read(cx).session_id, cx).as_deref() == Some(project.as_str()) {
                    return;
                }
                if let Err(why) = self.move_draft_to_project(runtime, project, cx) {
                    crate::baaz_log!("new-session row: move to project {project} refused ({why})");
                    self.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't move the draft", why, cx));
                }
            }
            FolderPick::Choose => self.choose_folder_for_draft(runtime, cx),
        }
    }

    /// "Choose a folder…": the native folder panel, directories only; the
    /// chosen folder moves the draft ([`Self::move_draft_to_folder`]).
    /// Cancel does nothing.
    fn choose_folder_for_draft(&mut self, runtime: Entity<SessionRuntime>, cx: &mut Context<Self>) {
        // The panel opens behind everything when this app is not active.
        cx.activate(true);
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = paths.await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(root) = paths.first() {
                        if let Err(why) = this.move_draft_to_folder(runtime, root, cx) {
                            crate::baaz_log!("new-session row: move to {} refused ({why})", root.display());
                            this.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't move the draft", why, cx));
                        }
                    }
                });
            }
        }));
    }

    /// Move the open unsent draft `runtime` into `root`: the folder is
    /// adopted as a project (or found, when it already is one), as Add
    /// project does, and the draft moves there. What the folder panel's
    /// answer runs, and the tests' seam for it.
    pub(crate) fn move_draft_to_folder(&mut self, runtime: Entity<SessionRuntime>, root: &std::path::Path, cx: &mut Context<Self>) -> Result<(), String> {
        if !root.is_dir() {
            return Err(format!("{} is not a folder", root.display()));
        }
        self.draft_move_refusal(&runtime, cx)?;
        let project = self.adopt_root(root, cx);
        self.move_draft_to_project(runtime, &project, cx)
    }

    /// Move the open unsent draft `runtime` into `project` through the
    /// replace a provider switch uses (same provider): the replacement
    /// starts there — in its own worktree when that project's default says
    /// so — with the typed text carried over, and the old draft's lane is
    /// hung up (its unused worktree goes). `Err`, with nothing changed, when
    /// [`Self::draft_move_refusal`] says it cannot, the lane is still
    /// opening, or the project is unknown or already the draft's.
    pub(crate) fn move_draft_to_project(&mut self, runtime: Entity<SessionRuntime>, project: &str, cx: &mut Context<Self>) -> Result<(), String> {
        if self.draft_move_refusal(&runtime, cx)? {
            return Err("the draft is still opening".into());
        }
        let default = self
            .session_list
            .read(cx)
            .projects
            .find_available(project)
            .map(|found| found.defaults.worktree)
            .ok_or_else(|| format!("no project `{project}`"))?;
        let old = runtime.read(cx).session_id.clone();
        if self.draft_project_id(&old, cx).as_deref() == Some(project) {
            return Err("the draft is already in that project".into());
        }
        let provider = runtime.read(cx).provider_kind();
        self.open_state.folder_pick = Some(project.to_owned());
        // The new project's own default, never the old draft's worktree
        // (a plain switch keeps that; a move leaves it).
        self.open_state.worktree_pick = Some(default);
        self.on_switch_provider(runtime, &provider, cx);
        // The old project no longer names this draft; an aborted move gives
        // the name back ([`OpenState::abort_replace`]).
        self.open_state.forget_draft(&old);
        Ok(())
    }

    /// The "Own worktree" toggle on `runtime`'s draft: into a worktree of
    /// its project, or back to the project root. A refusal says why.
    pub(super) fn on_toggle_worktree(&mut self, runtime: Entity<SessionRuntime>, cx: &mut Context<Self>) {
        let on = !Self::in_worktree(runtime.read(cx).workspace_path());
        if let Err(why) = self.move_draft_worktree(runtime, on, cx) {
            crate::baaz_log!("new-session row: worktree {} refused ({why})", if on { "on" } else { "off" });
            let title = if on { "Couldn't move to a worktree" } else { "Couldn't move to the project folder" };
            self.overlays.update(cx, |overlays, cx| overlays.toast(title, why, cx));
        }
    }
}
