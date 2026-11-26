//! Branch from here, the app's half (docs/85 16.3): a branch is recorded in
//! baaz.db's `branch` table, nests under its source in the sidebar (the
//! same parent map orchestration's workers use), and counts under the
//! message it started at.
//!
//! Keyed by session id — holders: the `branch` table, `SessionList`'s
//! `worker_leads` (branch → source) and `branch_counts` (source → message →
//! count), each runtime's `branch_counts`.
//!
//! docs/87 18.9: in a git folder, a branch on a provider whose fork can move
//! runs in its own worktree, cut at the turn's checkpoint
//! ([`Harness::branch_into_worktree`]); its session row is tied to the
//! worktree's when it opens there, as any session in a worktree is.

use gpui::{AppContext as _, Context, Entity};

use crate::wire::WireCall as _;

use crate::app::Harness;
use crate::session::SessionRuntime;

impl Harness {
    /// A branch landed (`SessionEvent::Branched`): recorded, nested, counted.
    pub(crate) fn branched(&mut self, branch: &str, source: &str, message: &str, cx: &mut Context<Self>) {
        let (b, s, m) = (branch.to_owned(), source.to_owned(), message.to_owned());
        let at = baaz_core::clock::now_ms() as i64;
        cx.background_spawn(async move {
            crate::db::call(crate::db::path(), move |opened| {
                if let Err(why) = baaz_core::db::branches::record(&opened.conn, &b, &s, &m, at) {
                    crate::baaz_log!("branch: not recorded ({why})");
                }
            });
        })
        .detach();
        self.session_list.update(cx, |list, cx| {
            *list.branch_counts.entry(source.to_owned()).or_default().entry(message.to_owned()).or_insert(0) += 1;
            list.set_worker_lead(branch, source, cx);
        });
    }

    /// docs/87 18.9: cut a worktree at the checkpoint after `through` (the
    /// branch's answer turn) off the UI thread, then fork into it. No
    /// checkpoint for that turn, a folder that is not a git repository, or
    /// git refusing: the branch shares the session's folder instead (git's
    /// refusal is a toast).
    pub(crate) fn branch_into_worktree(
        &mut self,
        runtime: Entity<SessionRuntime>,
        message: String,
        through: String,
        folder: std::path::PathBuf,
        key: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let (provider, session) = {
            let rt = runtime.read(cx);
            (rt.provider_kind().as_str().to_owned(), rt.session_id.clone())
        };
        let root = baaz_core::providers::main_repo_root(&folder);
        let project = self.session_list.read(cx).projects.find_by_root(&root).map(|project| project.id.clone());
        let prefix = <crate::layout::Layout as crate::layout::LayoutGlobal>::get(cx).worktree_branch_prefix();
        let state_dir = crate::store::support_dir();
        let through_turn = through.clone();
        let work = move || -> Result<Option<baaz_core::worktrees::Worktree>, String> {
            if !baaz_core::worktrees::in_git_repository(&folder) {
                return Ok(None);
            }
            let key = key.or_else(|| {
                crate::db::read(crate::db::path_in(&state_dir), move |db| crate::db::sessions::read_row(&db.conn, &provider, &session).ok().flatten()).flatten()
            });
            let commit = key
                .and_then(|key| crate::checkpoints::session_refs(&folder, &key))
                .and_then(|refs| refs.get(&crate::checkpoints::component(&through)).and_then(|ends| ends.post.clone()));
            let Some(commit) = commit else {
                crate::baaz_log!("branch: no checkpoint after turn {through}; the branch shares the folder");
                return Ok(None);
            };
            baaz_core::worktrees::create(&baaz_core::worktrees::CreateSpec {
                state_dir: &state_dir,
                project_root: &root,
                project_id: project.as_deref(),
                branch_prefix: &prefix,
                name: None,
                base: Some(&commit),
            })
            .map(Some)
        };
        self.wire_call(cx, work, move |this, made, cx| {
            let workspace = match made {
                Ok(Some(worktree)) => {
                    let folder = worktree.path.to_string_lossy().into_owned();
                    this.run_worktree_setup_for(worktree, cx);
                    Some(folder)
                }
                Ok(None) => None,
                Err(why) => {
                    crate::baaz_log!("branch: no worktree ({why}); the branch shares the folder");
                    this.overlays.update(cx, |overlays, cx| overlays.toast("Couldn't make a worktree", why, cx));
                    None
                }
            };
            runtime.update(cx, |rt, cx| rt.send_branch(message, through_turn, workspace, cx));
        });
    }

    /// At boot: every recorded branch nests under its source again, and
    /// each source's counts are kept for its view.
    pub(crate) fn load_branches(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let rows = cx
                .background_spawn(async move {
                    crate::db::call(crate::db::path(), |opened| {
                        let all = baaz_core::db::branches::all(&opened.conn).unwrap_or_default();
                        let counts: Vec<(String, Vec<(String, u32)>)> = {
                            let mut sources: Vec<String> = all.iter().map(|(_, s)| s.clone()).collect();
                            sources.sort();
                            sources.dedup();
                            sources.into_iter().map(|s| { let c = baaz_core::db::branches::counts(&opened.conn, &s).unwrap_or_default(); (s, c) }).collect()
                        };
                        (all, counts)
                    })
                    .unwrap_or_default()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let (all, counts) = rows;
                this.session_list.update(cx, |list, cx| {
                    for (source, per) in counts {
                        list.branch_counts.insert(source, per.into_iter().collect());
                    }
                    for (branch, source) in all {
                        list.set_worker_lead(&branch, &source, cx);
                    }
                });
                this.push_branch_counts(cx);
            });
        })
        .detach();
    }

    /// Each live session's counts, from the list (a view opened after the
    /// boot's read gets them here, on its resubscribe).
    pub(crate) fn push_branch_counts(&mut self, cx: &mut Context<Self>) {
        let runtimes: Vec<Entity<SessionRuntime>> = self.registry.runtimes(self.active.as_ref(), cx);
        for runtime in runtimes {
            let id = runtime.read(cx).session_id.clone();
            let Some(counts) = self.session_list.read(cx).branch_counts.get(&id).cloned() else { continue };
            if runtime.read(cx).branch_counts != counts {
                runtime.update(cx, |rt, _| rt.branch_counts = counts);
            }
        }
    }
}

#[cfg(feature = "probe")]
impl Harness {
    /// `branches-fixture[:nongit]` (docs/85 16.3 captures): the open
    /// session's last user message reads "2 branches", with no fork made and
    /// no row written; `:nongit` (docs/87 18.9) also takes the session's
    /// folder as outside git, so the count says the branches share it.
    #[cfg(feature = "probe")]
    pub(crate) fn step_branches_fixture(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let nongit = match rest.trim() {
            "" => false,
            "nongit" => true,
            other => return Err(format!("unknown branches-fixture variant `{other}` (try `nongit`)")),
        };
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        let last_user = runtime.read(cx).last_user_turn_id().ok_or("the session has no user message")?;
        runtime.update(cx, |rt, cx| {
            rt.branch_counts.insert(last_user, 2);
            if nongit {
                rt.set_branches_share_folder_for_capture();
            }
            cx.emit(crate::session::RuntimeEvent::BannerChanged);
        });
        Ok(())
    }

    /// `branch-from:<n>` (docs/85 16.3 live checks): Branch from here on
    /// the open session's nth user message (from 1), through the
    /// message's own action; the branch opens as a Branch does. No session,
    /// no such message, or a refusal (the provider cannot cut at a turn,
    /// no finished answer) fails the step.
    #[cfg(feature = "probe")]
    pub(crate) fn step_branch_from(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let n: usize = rest.trim().parse().map_err(|_| format!("`{rest}` is not a message number from 1"))?;
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        let message = runtime.read(cx).nth_user_turn_id(n).ok_or_else(|| format!("the session has no user message {n}"))?;
        if runtime.update(cx, |rt, cx| rt.branch_from(&message, cx)) {
            Ok(())
        } else {
            Err("the branch was refused (see the banner)".into())
        }
    }
}
