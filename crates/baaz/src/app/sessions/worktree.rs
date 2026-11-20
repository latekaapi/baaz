//! A worktree per session (docs/82 6.1): where a new session's worktree is
//! made, how its session is tied to the row, and how an unsent draft's
//! unused worktree goes. The git work is [`baaz_core::worktrees`], run off
//! the UI thread.
//!
//! Whether a new session gets one: [`OpenState::worktree_pick`] when
//! something asked (a draft moved into or out of a worktree), else the
//! project's own default ([`crate::projects::ProjectDefaults::worktree`]).
//! The session's folder (`workspace`) is the worktree; its project stays
//! the original one. The visible toggle and folder picker are task 6.6.
//!
//! The lifecycle (docs/82 6.2): archiving a worktree session preserves and
//! removes its worktree ([`Harness::archive_worktrees`]), unarchiving
//! brings it back ([`Harness::unarchive_with_worktrees`]), "Move back to the
//! project folder" hands its changes to the root ([`Harness::move_back_to_root`]),
//! and a session landing where another one is working is warned once
//! ([`Harness::warn_if_folder_busy`]).
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use baaz_core::worktrees;

use super::open::is_local_draft;

/// The environment a worktree's setup script inherits before its own
/// variables: Baaz's, with the login shell's `PATH` (a Dock launch's own
/// `PATH` finds no project tools).
fn setup_base_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = std::env::vars_os().filter(|(key, _)| key != "PATH").collect();
    env.push(("PATH".into(), provider::env_path::login_path().to_owned()));
    env
}

/// [`Harness::worktree_move_refusal`]'s word for a draft whose lane has
/// not finished opening: not yet, rather than never.
const STILL_OPENING: &str = "the draft is still opening";

/// [`Harness::worktree_move_refusal`]'s word inside a capture, which never
/// makes a worktree in the person's repository.
const CAPTURE_NEVER: &str = "a capture never makes a worktree in the person's repository";

/// A session that runs in one of Baaz's worktrees
/// ([`Harness::worktree_members`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorktreeMember {
    /// Its provider's key (`codex`).
    pub(crate) provider: String,
    /// Its id.
    pub(crate) id: String,
    /// Its folder: the worktree.
    pub(crate) folder: String,
}

/// Where a new session runs ([`Harness::take_worktree_plan`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum WorktreePlan {
    /// The project root.
    Root,
    /// A new worktree of the project.
    New,
    /// This worktree, already made (a provider switch keeps the draft's).
    Reuse(String),
}

impl WorktreePlan {
    /// Whether the session runs in a worktree.
    pub(super) fn in_worktree(&self) -> bool {
        !matches!(self, Self::Root)
    }
}

impl Harness {
    /// Whether `folder` is one of Baaz's worktrees.
    pub(crate) fn in_worktree(folder: &str) -> bool {
        worktrees::is_worktree_folder(&crate::store::support_dir(), std::path::Path::new(folder))
    }

    /// Where the next new session in `project` runs, worktree-wise: the
    /// pick something made for it (taken), else — for a provider switch of
    /// a draft already in a worktree — that same worktree (the folder stays
    /// with the draft), else the project's default. A capture never makes
    /// one: the person's repository is off limits to it
    /// ([`crate::runtime::state_isolated`]).
    pub(super) fn take_worktree_plan(&mut self, project: Option<&str>, switch: Option<u64>, cx: &gpui::App) -> WorktreePlan {
        let pick = self.open_state.worktree_pick.take();
        if pick.is_none() && switch.is_some() {
            let replaced = self.open_state.replacing_id().and_then(|old| self.find_view(old, cx));
            if let Some(folder) = replaced.map(|view| view.read(cx).rt(cx).workspace_path().to_owned()) {
                if Self::in_worktree(&folder) {
                    return WorktreePlan::Reuse(folder);
                }
            }
        }
        let default = project
            .and_then(|id| self.session_list.read(cx).projects.find(id))
            .is_some_and(|project| project.defaults.worktree);
        if !pick.unwrap_or(default) {
            return WorktreePlan::Root;
        }
        if crate::runtime::state_isolated() {
            crate::baaz_log!("worktree: a capture makes none; the session opens in the project root");
            return WorktreePlan::Root;
        }
        WorktreePlan::New
    }

    /// Make a worktree of `root` off the UI thread, then open the new
    /// session's lane in it (`open_new_on_provider`, as any new session).
    /// Verbs wait meanwhile and a second `new` is refused (a switch holds
    /// them already; a plain new marks itself with [`OpenState::begin_open`]).
    /// A worktree that lands after the person moved on is removed unused.
    /// When git refuses, a plain new session opens in the project root and
    /// a toast says why; a draft being moved stays where it was.
    pub(super) fn open_in_new_worktree(
        &mut self,
        wanted: ProviderId,
        project: Option<String>,
        root: String,
        switch: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let epoch = switch.is_none().then(|| self.open_state.begin_open());
        let state_dir = crate::store::support_dir();
        let prefix = crate::layout::Layout::get(cx).worktree_branch_prefix();
        let (work_root, work_project) = (root.clone(), project.clone());
        let work = move || {
            worktrees::create(&worktrees::CreateSpec {
                state_dir: &state_dir,
                project_root: std::path::Path::new(&work_root),
                project_id: work_project.as_deref(),
                branch_prefix: &prefix,
                name: None,
            })
        };
        self.wire_call_in(cx, work, move |this, made, window, cx| {
            let current = match (switch, epoch) {
                (Some(switch), _) => this.open_state.replaces_under(switch),
                (None, Some(epoch)) => this.open_state.is_current_open(epoch),
                (None, None) => false,
            };
            match made {
                Ok(worktree) if !current => {
                    crate::baaz_log!("worktree: the new session moved on before {} was made", worktree.path.display());
                    Self::discard_worktree_in_background(worktree, cx);
                }
                Ok(worktree) => {
                    let folder = worktree.path.to_string_lossy().into_owned();
                    this.run_worktree_setup(worktree, cx);
                    this.open_new_on_provider(wanted, project, folder, switch, window, cx);
                }
                Err(error) => {
                    crate::baaz_log!("worktree: not made ({error})");
                    this.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't make a worktree", error, cx));
                    if !current {
                        return;
                    }
                    if switch.is_some() {
                        this.abandon_switch(cx);
                    } else {
                        this.open_new_on_provider(wanted, project, root, None, window, cx);
                    }
                }
            }
        });
    }

    /// The project's `setup` script, once, in the background: the lane is
    /// not held for it. A failure keeps the worktree and says so.
    fn run_worktree_setup(&mut self, worktree: worktrees::Worktree, cx: &mut Context<Self>) {
        let state_dir = crate::store::support_dir();
        let work = move || {
            worktrees::run_setup_once(&state_dir, &worktree, &setup_base_env(), worktrees::SETUP_TIMEOUT)
        };
        self.wire_call_in(cx, work, |this, outcome, _, cx| {
            if let worktrees::SetupOutcome::Failed(why) = outcome {
                let body = format!("The project's setup script failed ({why}). The worktree is kept.");
                this.overlays.update(cx, |overlays, cx| overlays.toast("Setup failed", body, cx));
            }
        });
    }

    /// A session landed in `workspace`: when that is one of Baaz's
    /// worktrees, tie its row to the session (and relock it in the
    /// session's name). Queued behind the session's record.
    pub(super) fn link_worktree_session(&mut self, provider_id: ProviderId, session_id: &str, workspace: &str, cx: &mut Context<Self>) {
        if !Self::in_worktree(workspace) {
            return;
        }
        let state_dir = crate::store::support_dir();
        let (provider_key, id, folder) = (provider_id.as_str().to_owned(), session_id.to_owned(), std::path::PathBuf::from(workspace));
        cx.background_spawn(async move {
            if !worktrees::link_session(&state_dir, &provider_key, &id, &folder) {
                crate::baaz_log!("worktree: {} has no row to tie session {id} to", folder.display());
            }
        })
        .detach();
    }

    /// An unsent draft in `workspace` is going for good: when that is one of
    /// Baaz's worktrees, remove it — unless it has changes, when it stays
    /// (archiving its session is what preserves and removes such a one).
    pub(super) fn release_unused_worktree(&mut self, workspace: &str, cx: &mut Context<Self>) {
        if !Self::in_worktree(workspace) {
            return;
        }
        let state_dir = crate::store::support_dir();
        let folder = std::path::PathBuf::from(workspace);
        cx.background_spawn(async move {
            if let Some(worktree) = worktrees::by_path(&state_dir, &folder) {
                if let Err(error) = worktrees::discard_unused(&state_dir, &worktree) {
                    crate::baaz_log!("worktree: {} not removed ({error})", folder.display());
                }
            }
        })
        .detach();
    }

    fn discard_worktree_in_background(worktree: worktrees::Worktree, cx: &mut Context<Self>) {
        let state_dir = crate::store::support_dir();
        cx.background_spawn(async move {
            if let Err(error) = worktrees::discard_unused(&state_dir, &worktree) {
                crate::baaz_log!("worktree: {} not removed ({error})", worktree.path.display());
            }
        })
        .detach();
    }

    /// Each of `ids` that runs in one of Baaz's worktrees, with its
    /// provider's key and its folder: read from its provider record, else its
    /// open view, else its sidebar row.
    pub(crate) fn worktree_members(&self, ids: &[String], cx: &gpui::App) -> Vec<WorktreeMember> {
        let list = self.session_list.read(cx);
        ids.iter()
            .filter_map(|id| {
                let entry = list.sessions.iter().find(|entry| &entry.id == id);
                let record = list.provider_sessions.get(id);
                let view = self.find_view(id, cx);
                // The folder as the session opened it first: a row's is
                // canonical, which may spell the state dir differently.
                let folder = record
                    .and_then(|record| record.workspace.clone())
                    .or_else(|| view.as_ref().map(|view| view.read(cx).rt(cx).workspace_path().to_owned()))
                    .or_else(|| entry.and_then(|entry| entry.workspace.clone()))?;
                if !Self::in_worktree(&folder) {
                    return None;
                }
                let provider = record
                    .map(|record| record.provider.clone())
                    .or_else(|| entry.map(|entry| entry.provider.as_str().to_owned()))
                    .or_else(|| view.as_ref().map(|view| view.read(cx).rt(cx).provider_kind().as_str().to_owned()))?;
                Some(WorktreeMember { provider, id: id.clone(), folder })
            })
            .collect()
    }

    /// The first of `members` with a turn running: a worktree is never
    /// removed from under its agent.
    pub(crate) fn busy_worktree_member<'a>(&self, members: &'a [WorktreeMember], cx: &gpui::App) -> Option<&'a WorktreeMember> {
        members.iter().find(|member| {
            self.registry.find_runtime(self.active.as_ref(), &member.id, cx).is_some_and(|runtime| runtime.read(cx).is_sending())
        })
    }

    /// Archive each member's worktree off the UI thread (docs/82 6.2): its
    /// work preserved to a hidden ref named by the session's uuid, the
    /// snapshot on its row, the folder removed, the branch kept. A failure
    /// keeps the worktree and says why.
    pub(crate) fn archive_worktrees(&mut self, members: Vec<WorktreeMember>, cx: &mut Context<Self>) {
        if members.is_empty() {
            return;
        }
        let state_dir = crate::store::support_dir();
        let work = move || {
            worktrees::one_at_a_time(|| members
                .into_iter()
                .filter_map(|member| {
                    let folder = std::path::PathBuf::from(&member.folder);
                    let (worktree, key) = worktrees::for_session(&state_dir, &member.provider, &member.id, Some(&folder))?;
                    worktrees::archive(&state_dir, &worktree, &key).err()
                })
                .collect::<Vec<String>>())
        };
        self.wire_call_in(cx, work, |this, failures, _, cx| {
            if let Some(error) = failures.into_iter().next() {
                crate::baaz_log!("worktree: archive left it in place ({error})");
                let body = format!("Its worktree is kept as it was ({error}).");
                this.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't archive the worktree", body, cx));
            }
            this.invalidate_list(cx);
        });
    }

    /// Unarchive `ids` (a chain, as one): a member whose worktree was
    /// archived gets it back first, off the UI thread (folder added on its
    /// branch, its work put back unstaged, re-locked), and the rows return
    /// once that is done, so no session opens onto a missing folder. With
    /// no worktree among them the rows return at once.
    pub(crate) fn unarchive_with_worktrees(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let members = self.worktree_members(&ids, cx);
        if members.is_empty() {
            self.set_overrides(&ids, |meta| meta.archived = false, cx);
            return;
        }
        let state_dir = crate::store::support_dir();
        let work = move || worktrees::one_at_a_time(|| {
            let mut notes = Vec::new();
            let mut moved = Vec::new();
            for member in members {
                let folder = std::path::PathBuf::from(&member.folder);
                let Some((worktree, _)) = worktrees::for_session(&state_dir, &member.provider, &member.id, Some(&folder)) else {
                    continue;
                };
                if worktree.snapshot.is_none() {
                    continue;
                }
                match worktrees::restore(&state_dir, &worktree) {
                    Ok(restored) => {
                        if restored.worktree.path != folder {
                            notes.push(format!("Its old folder was taken; it is back at {}.", restored.worktree.path.display()));
                            moved.push((member.id.clone(), restored.worktree.path.to_string_lossy().into_owned()));
                        }
                        notes.extend(restored.note);
                    }
                    Err(error) => notes.push(format!("Its worktree did not come back ({error}).")),
                }
            }
            (notes, moved)
        });
        self.wire_call_in(cx, work, move |this, (notes, moved), _, cx| {
            // A worktree back at a fresh folder: the session's record names
            // it, so its next reopen (and its terminal) starts there.
            if !moved.is_empty() {
                this.session_list.update(cx, |list, _| {
                    for (id, folder) in &moved {
                        if let Some(record) = list.provider_sessions.get_mut(id) {
                            record.workspace = Some(folder.clone());
                        }
                    }
                });
                this.persist_provider_sessions(cx);
                this.merge_provider_rows(cx);
            }
            this.set_overrides(&ids, |meta| meta.archived = false, cx);
            if let Some(note) = notes.into_iter().next() {
                crate::baaz_log!("worktree: unarchive: {note}");
                this.overlays.update(cx, |overlays, cx| overlays.toast("Worktree restored with a note", note, cx));
            }
        });
    }

    /// Whether "Move back to the project folder" is offered on the open
    /// session (rule 13): it runs in one of Baaz's worktrees, has no turn
    /// running, and its provider resumes a session in a folder the app
    /// names ([`provider::ProviderTrait::ResumesInNamedFolder`]) — the
    /// others resume only where their own store recorded it.
    pub(crate) fn move_back_offered(&self, cx: &gpui::App) -> bool {
        self.active.as_ref().is_some_and(|view| {
            let rt = view.read(cx).rt(cx);
            Self::in_worktree(rt.workspace_path())
                && rt.provider_kind().has(provider::ProviderTrait::ResumesInNamedFolder)
                && !rt.is_sending()
                && !is_local_draft(&rt.session_id)
        })
    }

    /// "Move back to the project folder" (docs/82 6.2) on the open
    /// session: its worktree's changes (branch commits and uncommitted
    /// work) land in the project root as uncommitted changes, the worktree
    /// is preserved and removed (branch kept), and the session reopens in
    /// the root. Refused, with nothing changed, when the changes would not
    /// apply cleanly there, or when [`Self::move_back_offered`] says no.
    pub(crate) fn move_back_to_root(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.move_back_offered(cx) {
            return Err("the open session cannot move back to its project folder".into());
        }
        let session_id = self.active_id(cx).ok_or("no open session")?;
        let member = self.worktree_members(std::slice::from_ref(&session_id), cx).into_iter().next().ok_or("the session runs in no worktree")?;
        let state_dir = crate::store::support_dir();
        let work = move || {
            worktrees::one_at_a_time(|| {
                let folder = std::path::PathBuf::from(&member.folder);
                let (worktree, key) = worktrees::for_session(&state_dir, &member.provider, &member.id, Some(&folder))
                    .ok_or_else(|| "Baaz has no record of this worktree.".to_owned())?;
                worktrees::move_back(&state_dir, &worktree, &key)?;
                Ok::<String, String>(worktree.project_root.to_string_lossy().into_owned())
            })
        };
        self.wire_call_in(cx, work, move |this, moved, window, cx| match moved {
            Ok(root) => this.reopen_in_folder(&session_id, root, window, cx),
            Err(error) => {
                crate::baaz_log!("worktree: move back refused ({error})");
                this.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't move back", error, cx));
            }
        });
        Ok(())
    }

    /// The session now runs in `folder`: its record says so, its open
    /// lane closes, and it reopens there (the provider resumes it in the
    /// folder its `ResumeSession` names).
    fn reopen_in_folder(&mut self, session_id: &str, folder: String, window: &mut Window, cx: &mut Context<Self>) {
        self.session_list.update(cx, |list, _| {
            if let Some(record) = list.provider_sessions.get_mut(session_id) {
                record.workspace = Some(folder.clone());
            }
        });
        self.persist_provider_sessions(cx);
        self.merge_provider_rows(cx);
        self.registry.forget_where(|id| id == session_id);
        if self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == session_id) {
            self.active = None;
        }
        self.overlays.update(cx, |overlays, cx| {
            overlays.toast("Moved to the project folder", "The worktree's changes are in the project folder, uncommitted.", cx)
        });
        self.resume(session_id.to_owned(), window, cx);
    }

    /// Why the session `runtime` cannot move into (`on`) or out of a
    /// worktree now, or `Ok`: only the open unsent draft, with a lane that
    /// finished opening, not already there, and — into one — with a
    /// project, outside a capture.
    pub(crate) fn worktree_move_refusal(&self, runtime: &Entity<SessionRuntime>, on: bool, cx: &gpui::App) -> Result<(), String> {
        let (id, folder, opening) = {
            let rt = runtime.read(cx);
            (rt.session_id.clone(), rt.workspace_path().to_owned(), rt.is_opening_lane())
        };
        if self.active.as_ref().is_none_or(|view| view.read(cx).runtime() != runtime) {
            return Err("only the open session moves".into());
        }
        if is_local_draft(&id) {
            return Err("a local draft has no lane to move".into());
        }
        // A lane still opening is a new draft only when nothing stored
        // names it (a reopen of a session with turns opens too).
        let unsent = if opening {
            let list = self.session_list.read(cx);
            !list.provider_sessions.contains_key(&id) && !list.sessions.iter().any(|entry| entry.id == id)
        } else {
            self.open_state.is_unsent(&id) && self.is_live_draft(&id, cx)
        };
        if !unsent {
            return Err("only an unsent draft moves into or out of a worktree".into());
        }
        if Self::in_worktree(&folder) == on {
            return Err(format!("the draft already runs {}", if on { "in a worktree" } else { "in the project root" }));
        }
        if on && crate::runtime::state_isolated() {
            return Err(CAPTURE_NEVER.into());
        }
        if on && self.current_project(cx).is_none() {
            return Err("no project to make a worktree of".into());
        }
        // Last, so an offer can tell "not yet" from "never"
        // ([`Self::warn_if_folder_busy`]).
        if opening {
            return Err(STILL_OPENING.into());
        }
        Ok(())
    }

    /// Move the open unsent draft `runtime` into a new worktree of its
    /// project (`on`), or back to the project root, through the replace a
    /// provider switch uses (same provider). `Err` when
    /// [`Self::worktree_move_refusal`] says it cannot.
    pub(crate) fn move_draft_worktree(&mut self, runtime: Entity<SessionRuntime>, on: bool, cx: &mut Context<Self>) -> Result<(), String> {
        self.worktree_move_refusal(&runtime, on, cx)?;
        let provider = runtime.read(cx).provider_kind();
        self.open_state.worktree_pick = Some(on);
        self.on_switch_provider(runtime, &provider, cx);
        Ok(())
    }

    /// The shared-folder warning (docs/82 6.2): when `view`'s session
    /// lands in a folder where another session has a turn running, it
    /// shows one banner, once per session. "Run in a worktree" is offered
    /// only where it works: an unsent draft that can move (rule 13).
    pub(super) fn warn_if_folder_busy(&mut self, view: &Entity<SessionView>, cx: &mut Context<Self>) {
        let runtime = view.read(cx).runtime().clone();
        let (id, folder) = {
            let rt = runtime.read(cx);
            (rt.session_id.clone(), rt.workspace_path().to_owned())
        };
        if folder.is_empty() {
            return;
        }
        let busy = self.registry.iter_running(self.active.as_ref(), cx).into_iter().any(|other| {
            let other = other.read(cx);
            other.session_id != id && other.workspace_path() == folder
        });
        if !busy {
            return;
        }
        // A draft whose lane is still opening (a new session lands here
        // before its open finishes) is offered the move: it can make it the
        // moment the lane is up, and the click checks again. A capture shows
        // the offer as a person would see it; its click refuses.
        let refusal =
            self.worktree_move_refusal(&runtime, true, cx).err().filter(|why| why != STILL_OPENING && why != CAPTURE_NEVER);
        let worktree = refusal.is_none();
        if runtime.update(cx, |rt, cx| rt.warn_shared_folder(worktree, cx)) {
            let offer = refusal.map(|why| format!(" (no worktree offered: {why})")).unwrap_or_default();
            crate::baaz_log!("worktree: {id} opened in {folder}, where another session is working; warned{offer}");
        }
    }

    #[cfg(feature = "probe")]
    /// `worktree:on` / `worktree:off`: [`Self::move_draft_worktree`] on the
    /// open session. Fails when it would do nothing.
    pub(crate) fn step_worktree(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = match rest.trim() {
            "on" => true,
            "off" => false,
            other => return Err(format!("worktree wants on or off, not {other:?}")),
        };
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        self.move_draft_worktree(runtime, on, cx)
    }

    #[cfg(feature = "probe")]
    /// `unarchive:<session_id>`: [`Self::unarchive_session`] (a worktree
    /// member's worktree comes back first). Fails when the id names no
    /// archived row.
    pub(crate) fn step_unarchive(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let id = rest.trim();
        let archived = self.session_list.read(cx).sessions.iter().any(|entry| entry.id == id && entry.archived);
        if !archived {
            return Err(format!("no archived session `{id}`"));
        }
        self.unarchive_session(id.to_owned(), cx);
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `move-back`: [`Self::move_back_to_root`] on the open session. Fails
    /// when it is not offered there (no worktree, a running turn, or a
    /// provider that cannot resume in the project folder).
    pub(crate) fn step_move_back(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        self.move_back_to_root(cx)
    }

    #[cfg(feature = "probe")]
    /// `worktree-default:on|off`: the current project's default for new
    /// sessions, stored with its other defaults.
    pub(crate) fn step_worktree_default(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = match rest.trim() {
            "on" => true,
            "off" => false,
            other => return Err(format!("worktree-default wants on or off, not {other:?}")),
        };
        let id = self.current_project(cx).map(|project| project.id.clone()).ok_or("no current project")?;
        self.set_worktree_default(&id, on, cx)
    }

    /// Set `project`'s worktree default and write the projects file.
    /// `Err` when the project is unknown or already says so.
    pub(crate) fn set_worktree_default(&mut self, project: &str, on: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let changed = self.session_list.update(cx, |list, _| {
            let project = list.projects.projects.iter_mut().find(|candidate| candidate.id == project)?;
            let changed = project.defaults.worktree != on;
            project.defaults.worktree = on;
            Some(changed)
        });
        match changed {
            None => Err(format!("no project `{project}`")),
            Some(false) => Err(format!("the project's worktree default is already {}", if on { "on" } else { "off" })),
            Some(true) => {
                projects::write(&self.session_list.read(cx).projects);
                Ok(())
            }
        }
    }
}
