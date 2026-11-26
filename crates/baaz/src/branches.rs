//! Branch from here, the app's half (docs/85 16.3): a branch is recorded in
//! baaz.db's `branch` table, nests under its source in the sidebar (the
//! same parent map orchestration's workers use), and counts under the
//! message it started at.
//!
//! Keyed by session id — holders: the `branch` table, `SessionList`'s
//! `worker_leads` (branch → source) and `branch_counts` (source → message →
//! count), each runtime's `branch_counts`.

use gpui::{AppContext as _, Context, Entity};

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
    /// `branches-fixture` (docs/85 16.3 captures): the open session's last
    /// user message reads "2 branches", with no fork made and no row
    /// written.
    #[cfg(feature = "probe")]
    pub(crate) fn step_branches_fixture(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        let last_user = runtime.read(cx).last_user_turn_id().ok_or("the session has no user message")?;
        runtime.update(cx, |rt, cx| {
            rt.branch_counts.insert(last_user, 2);
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
