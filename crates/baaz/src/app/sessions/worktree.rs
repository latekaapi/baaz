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
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

use baaz_core::worktrees;

#[cfg(feature = "probe")]
use super::open::is_local_draft;

/// The environment a worktree's setup script inherits before its own
/// variables: Baaz's, with the login shell's `PATH` (a Dock launch's own
/// `PATH` finds no project tools).
fn setup_base_env() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    let mut env: Vec<_> = std::env::vars_os().filter(|(key, _)| key != "PATH").collect();
    env.push(("PATH".into(), provider::env_path::login_path().to_owned()));
    env
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
    /// Baaz's worktrees, remove it — unless it has changes (6.2 owns the
    /// rest of the lifecycle).
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

    #[cfg(feature = "probe")]
    /// `worktree:on` / `worktree:off`: move the open unsent draft into a
    /// new worktree of its project, or back to the project root, through
    /// the replace a provider switch uses (same provider). Fails when there
    /// is no such draft or it already runs there.
    pub(crate) fn step_worktree(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = match rest.trim() {
            "on" => true,
            "off" => false,
            other => return Err(format!("worktree wants on or off, not {other:?}")),
        };
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        let (id, provider, folder, opening) = {
            let rt = runtime.read(cx);
            (rt.session_id.clone(), rt.provider_kind(), rt.workspace_path().to_owned(), rt.is_opening_lane())
        };
        if is_local_draft(&id) {
            return Err("a local draft has no lane to move".into());
        }
        if opening {
            return Err("the draft is still opening".into());
        }
        if !self.open_state.is_unsent(&id) || !self.is_live_draft(&id, cx) {
            return Err("only an unsent draft moves into or out of a worktree".into());
        }
        if Self::in_worktree(&folder) == on {
            return Err(format!("the draft already runs {}", if on { "in a worktree" } else { "in the project root" }));
        }
        if on && crate::runtime::state_isolated() {
            return Err("a capture never makes a worktree in the person's repository".into());
        }
        if on && self.current_project(cx).is_none() {
            return Err("no project to make a worktree of".into());
        }
        self.open_state.worktree_pick = Some(on);
        self.on_switch_provider(runtime, &provider, cx);
        Ok(())
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
