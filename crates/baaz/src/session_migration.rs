//! The session move's window side (B4M): the prompt dialog, the
//! Providers-row wiring and the background plan cache on the app's
//! [`Harness`](crate::app::Harness). The planner, executor, journal and
//! the cache itself are [`baaz_core::session_migration`], re-exported here
//! so `crate::session_migration::…` names both.

use aui::overlay::DialogKind;

pub use baaz_core::session_migration::*;

use crate::app::Harness as BaazHarness;
use crate::overlays::{Dialog, DialogAction};

impl BaazHarness {
    /// This window's cached migration plan for renders: the cache, or
    /// nothing while the background compute is still running. Never
    /// walks the owner homes — that happens only in
    /// [`Self::refresh_migration_cache`], off the UI thread.
    pub(crate) fn migration_cached_plan(&self) -> Vec<PlannedMove> {
        self.migration.cache.as_ref().map(|cache| cache.plan_for_render().to_vec()).unwrap_or_default()
    }

    /// Recompute the plan off the UI thread and cache it with a timestamp:
    /// the registry and both dirs travel into a background task, and the
    /// cache (plus a re-render) lands when it completes. Re-entrant-safe:
    /// a second call while one is in flight is a no-op. Renders keep
    /// reading the old cache meanwhile — never the walker.
    pub(crate) fn refresh_migration_cache(&mut self, cx: &mut gpui::Context<Self>) {
        if self.migration.refresh_in_flight {
            return;
        }
        self.migration.refresh_in_flight = true;
        let owner = owner_home();
        let state = crate::store::support_dir();
        let registry = self.session_list.read(cx).provider_sessions.clone();
        cx.spawn(async move |this, cx| {
            let plan = cx
                .background_executor()
                .spawn(async move { plan_migration(&owner, &state, &registry) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.migration.refresh_in_flight = false;
                this.migration.cache = Some(MigrationCache::fresh(plan));
                // A startup offer deferred on this compute re-checks now
                // that the plan is here; a no-op when nothing waits.
                this.maybe_offer_session_migration(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Drop the cached plan and recompute it off the UI thread: what a
    /// finished confirm or lazy move calls, so the Providers row follows
    /// the move without ever walking on the render path.
    pub(crate) fn invalidate_migration_cache(&mut self, cx: &mut gpui::Context<Self>) {
        self.migration.cache = None;
        self.refresh_migration_cache(cx);
    }

    /// The one-time prompt's dialog for `plan`: the "Move N older …"
    /// title, the dry-run body with the file count behind its collapsed
    /// list, Move primary and Not-now secondary. Nothing moves without
    /// the click — building the dialog moves nothing.
    pub(crate) fn migration_dialog(plan: &[PlannedMove]) -> Dialog {
        let (n_claude, n_codex) = session_counts(plan);
        Dialog {
            title: prompt_title(n_claude, n_codex),
            detail: prompt_detail(plan),
            kind: DialogKind::Info,
            primary: "Move",
            action: DialogAction::MigrateSessions,
            archive_target: None,
        }
    }

    /// Offer the one-time prompt when moves remain and the owner has not
    /// dismissed it: deferred from startup, never over another dialog, a
    /// connect screen, or a deterministic capture. Reads only the cache —
    /// a missing or stale cache kicks a background recompute and offers
    /// when it lands, so the walk never runs on the UI thread.
    pub(crate) fn maybe_offer_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        if self.connect_open(cx) || crate::runtime::captures() {
            return;
        }
        let state = crate::store::support_dir();
        if is_dismissed(&state) {
            return;
        }
        if self.overlays.read(cx).dialog().is_some() {
            return;
        }
        let fresh = self.migration.cache.as_ref().is_some_and(|cache| cache.age_secs() < CACHE_STALE_SECS);
        if !fresh {
            if !self.migration.refresh_in_flight {
                self.refresh_migration_cache(cx);
            }
            return;
        }
        let plan = self.migration_cached_plan();
        if plan.is_empty() {
            return;
        }
        self.set_dialog(cx, Self::migration_dialog(&plan));
    }

    /// Run the prompt's Move (and the Providers row's Move — same
    /// click): record the person's consent first, so the lazy
    /// move-on-resume fallback is admitted from here on, then the
    /// cached plan through the journaling executor on the background
    /// executor, then (back on the UI thread) a toast with the honest
    /// counts (moved, already there, failed) and a cache refresh so the
    /// Providers row follows. The prompt never shows again afterwards.
    /// The click returns at once — nothing walks or copies on the UI
    /// thread.
    pub(crate) fn confirm_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        let state = crate::store::support_dir();
        // Consent lands with the click, before any byte moves — and every
        // legacy pin drains, so no later resume keeps reading the
        // owner's home after the move was admitted.
        record_consent(&state);
        clear_all_legacy_resumes();
        let plan = self.migration_cached_plan();
        // A click that beats the background plan (cold start) plans now, in
        // the same background task, instead of moving nothing.
        let owner = owner_home();
        let registry = self.session_list.read(cx).provider_sessions.clone();
        self.close_dialog(cx);
        cx.spawn(async move |this, cx| {
            let report = cx
                .background_executor()
                .spawn(async move {
                    let plan = if plan.is_empty() { plan_migration(&owner, &state, &registry) } else { plan };
                    let pending = pending_moves(&state, plan);
                    execute_plan(&state, &pending)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                dismiss(&crate::store::support_dir());
                this.invalidate_migration_cache(cx);
                let done = report.moved + report.copied;
                let body = if report.errors.is_empty() {
                    format!("{done} moved into Baaz, {} already there.", report.skipped,)
                } else {
                    format!(
                        "{done} moved, {} failed — retry from Settings → Providers. First failure: {}",
                        report.errors.len(),
                        report.errors.first().cloned().unwrap_or_default(),
                    )
                };
                this.overlays.update(cx, |overlays, cx| {
                    overlays.toast("Sessions moved", body, cx);
                });
            });
        })
        .detach();
    }

    /// The prompt's Not now: record the dismissal and close. The Providers
    /// row stays while moves remain, so the migration is always one click
    /// away. Records no consent: resumes keep reading the owner's home
    /// and move nothing until the person clicks Move.
    pub(crate) fn dismiss_session_migration(&mut self, cx: &mut gpui::Context<Self>) {
        dismiss(&crate::store::support_dir());
        self.close_dialog(cx);
    }
}
