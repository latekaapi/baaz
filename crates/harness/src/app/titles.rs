//! Generated session titles: the side-session driver (part 2).
//!
//! [`crate::titles`] owns the pure policy; this module owns the wire. On the
//! first `turn/started` of an unnamed session,
//! [`Harness::maybe_start_title`] marks the attempt (persisted, once-ever),
//! shows the pending placeholder, and runs one background chain —
//! `model/list`, `session/start` with `modelId` pinned and a namespaced
//! client id, one `turn/start` — then waits for that side session's
//! `turn/completed`, harvested with a free `session/read`. The result lands
//! as `generated_title`; the side session is hidden the moment it starts.
//!
//! Nothing here ever touches the UI thread except through completions: the
//! real turn is never blocked or delayed by a title. Failure (timeout, wire
//! error, missing model, switch off) falls back to the first-prompt label
//! with one log line, never a dialog, and at most one retry per session.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;
use crate::titles;

/// One title generation in flight: which real session it names, and which
/// attempt of this run's budget it is on.
#[derive(Clone, Debug)]
pub(crate) struct TitleJob {
    /// The real session this names.
    pub real_id: String,
    /// 1 for the first try, 2 for the one retry.
    pub tries: u8,
    /// The first message the prompt was built from, so a harvest retry
    /// re-asks about the same text rather than about nothing.
    pub first_message: String,
}

impl Harness {
    /// The first `turn/started` may earn this session a generated title.
    /// Pure decision first ([`titles::should_title`]), then — and only then —
    /// the persisted attempt marker, the pending placeholder, and the
    /// background chain. Called from the event route, after the local row
    /// insert, so it never delays the real turn's own handling.
    pub(super) fn maybe_start_title(
        &mut self,
        session_id: &str,
        first_message: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let entry = self.sessions.iter().find(|e| e.id == session_id);
        let turns = entry.map(|e| e.turns).unwrap_or(0);
        let eligible = titles::should_title(
            self.layout.auto_title,
            self.client.is_some(),
            self.overrides.get(session_id),
            turns,
            session_id,
        );
        if !eligible {
            return;
        }
        let Some(message) = first_message.filter(|m| !m.trim().is_empty()) else {
            // No prompt text to title from: leave the row on its derived
            // label rather than spending a turn on nothing.
            return;
        };
        // The once-ever marker, first and synchronously: a reconnect, a
        // restart, or a second `turn/started` before the chain lands all
        // see `title_attempted` and stand down.
        self.set_override(session_id, |meta| meta.title_attempted = true, cx);
        self.titles_pending.insert(session_id.to_owned());
        // One watchdog for the whole generation, armed with the trigger
        // rather than with an admission: a chain that hangs still stands
        // down on time, and a retry never extends the deadline.
        self.arm_title_timeout(session_id, cx);
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == session_id) {
            entry.title_pending = true;
        }
        self.invalidate_list();
        self.run_title_attempt(session_id.to_owned(), message, 1, cx);
        cx.notify();
    }

    /// One background attempt: list models, start the side session, send the
    /// one title prompt. The completion either records the harvest job (and
    /// arms its timeout) or retries once / gives up.
    fn run_title_attempt(
        &mut self,
        real_id: String,
        first_message: String,
        tries: u8,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else { return };
        if !self.titles_pending.contains(&real_id) {
            return;
        }
        let workspace = self.session_workspace(&real_id);
        let side_id = titles::side_session_id();
        let prompt = titles::title_prompt(&first_message);
        let retry_message = first_message.clone();
        let work = move || -> Result<String, String> {
            // Free pre-flight: the pinned id when listed, else `None` — omit
            // `modelId` and take the server default rather than failing.
            let models = client
                .model_list(&muse_client::schema::ModelListParams { session_id: None })
                .map(|result| result.models)
                .unwrap_or_default();
            let model_id = titles::pick_title_model(&models);
            let started = client
                .session_start(&muse_client::schema::SessionStartParams {
                    command_id: muse_client::new_command_id(),
                    model_id,
                    session_id: Some(side_id.clone()),
                    workspace_root: Some(workspace),
                    ..Default::default()
                })
                .map_err(|error| format!("session/start: {error}"))?;
            let started_id = started.session.session_id.clone();
            client
                .turn_start(&muse_client::schema::TurnStartParams {
                    command_id: muse_client::new_command_id(),
                    session_id: started_id.clone(),
                    input: vec![muse_client::schema::TurnInputPart::text(prompt)],
                    display_text: Some("harness auto-title".to_owned()),
                    ..Default::default()
                })
                .map_err(|error| format!("turn/start: {error}"))?;
            Ok(started_id)
        };
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok(side_id) => {
                // Hidden at once, before any list refresh can show it: the
                // override persists, and the prefix rule covers a lost write.
                this.set_override(&side_id, |meta| meta.hidden = true, cx);
                // A timeout that fired while the chain ran already stood
                // this generation down: hide and ignore, never double-bill.
                if !this.titles_pending.contains(&real_id) {
                    return;
                }
                this.title_jobs.insert(
                    side_id.clone(),
                    TitleJob { real_id: real_id.clone(), tries, first_message: retry_message },
                );
                cx.notify();
            }
            Err(reason) => {
                if !this.titles_pending.contains(&real_id) {
                    return;
                }
                if tries < titles::TITLE_MAX_ATTEMPTS {
                    crate::harness_log!("auto-title for {real_id} failed ({reason}); retrying once");
                    this.run_title_attempt(real_id, retry_message, tries + 1, cx);
                } else {
                    this.fail_title(&real_id, &reason, cx);
                }
            }
        });
    }

    /// A `turn/completed` on a side session: harvest its answer with a free
    /// `session/read`, then land the title or retry / give up per budget.
    pub(super) fn harvest_title(&mut self, side_id: &str, cx: &mut Context<Self>) {
        let Some(job) = self.title_jobs.get(side_id) else { return };
        // Stood down already (timeout): the side row stays hidden, the late
        // answer is ignored rather than landed over the fallback.
        if !self.titles_pending.contains(&job.real_id) {
            self.title_jobs.remove(side_id);
            return;
        }
        let Some(client) = self.client.clone() else { return };
        let side_id = side_id.to_owned();
        let read_id = side_id.clone();
        let work = move || {
            client.session_read(&muse_client::schema::SessionReadParams {
                session_id: read_id.clone(),
                exclude_items: Some(false),
            })
        };
        self.wire_call(cx, work, move |this, result, cx| {
            let Some(job) = this.title_jobs.get(&side_id).cloned() else { return };
            if !this.titles_pending.contains(&job.real_id) {
                this.title_jobs.remove(&side_id);
                return;
            }
            match result {
                Ok(read) => match titles::harvest_title_text(&read) {
                    Some(title) => this.land_title(&job.real_id, &side_id, &title, cx),
                    None if job.tries < titles::TITLE_MAX_ATTEMPTS => {
                        crate::harness_log!("auto-title for {} came back empty; retrying once", job.real_id);
                        this.title_jobs.remove(&side_id);
                        this.run_title_attempt(job.real_id, job.first_message, job.tries + 1, cx);
                    }
                    None => this.fail_title(&job.real_id, "empty reply", cx),
                },
                Err(error) => {
                    if job.tries < titles::TITLE_MAX_ATTEMPTS {
                        crate::harness_log!("auto-title read for {} failed ({error}); retrying once", job.real_id);
                        this.title_jobs.remove(&side_id);
                        this.run_title_attempt(job.real_id, job.first_message, job.tries + 1, cx);
                    } else {
                        this.fail_title(&job.real_id, &error.to_string(), cx);
                    }
                }
            }
        });
    }

    /// The title landed: store it under the user-given name in the label
    /// order, clear the pending state, and let the row and the crumb update
    /// in place — `set_override` rejoins the one row, so no flicker and no
    /// scroll jump.
    fn land_title(&mut self, real_id: &str, side_id: &str, title: &str, cx: &mut Context<Self>) {
        let title = title.to_owned();
        self.set_override(real_id, |meta| meta.generated_title = Some(title), cx);
        self.title_jobs.remove(side_id);
        self.titles_pending.remove(real_id);
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == real_id) {
            entry.title_pending = false;
        }
        self.invalidate_list();
        cx.notify();
    }

    /// Silent and cheap: one log line, the first-prompt label stands, never
    /// a dialog, never another attempt.
    fn fail_title(&mut self, real_id: &str, reason: &str, cx: &mut Context<Self>) {
        crate::harness_log!("auto-title for {real_id} failed ({reason}); keeping the first prompt");
        self.titles_pending.remove(real_id);
        self.title_jobs.retain(|_, job| job.real_id != real_id);
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == real_id) {
            entry.title_pending = false;
        }
        self.invalidate_list();
        cx.notify();
    }

    /// Give up waiting: the row falls back to the first prompt, the side
    /// session (already hidden) is left to its turn, and a late answer is
    /// ignored rather than retried — a timeout can never double-bill.
    fn title_timeout(&mut self, real_id: &str, cx: &mut Context<Self>) {
        if !self.titles_pending.contains(real_id) {
            return;
        }
        crate::harness_log!(
            "auto-title for {real_id} timed out after {} s; keeping the first prompt",
            titles::TITLE_TIMEOUT_SECS
        );
        self.fail_title(real_id, "timeout", cx);
    }

    /// One watchdog per attempt: [`titles::TITLE_TIMEOUT_SECS`] on the
    /// background executor, then back on the UI thread. Parked with the
    /// wire tasks, so it dies with the window.
    fn arm_title_timeout(&mut self, real_id: &str, cx: &mut Context<Self>) {
        let real_id = real_id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(titles::TITLE_TIMEOUT_SECS)).await;
            let _ = this.update(cx, |this, cx| this.title_timeout(&real_id, cx));
        });
        self.wire_tasks().push(task);
    }

    /// Re-apply the titler's ephemeral flags after a list rebuild: pending
    /// rows read pending, side sessions read hidden — even before their
    /// override writes land, and even when a restart lost one. A free
    /// function of the pending set (rather than `&self`) so [`Self::rejoin`]
    /// can call it mid-iteration over its own rows.
    pub(super) fn apply_title_flags(pending: &std::collections::HashSet<String>, entry: &mut crate::sidebar::SessionEntry) {
        if pending.contains(&entry.id) {
            entry.title_pending = true;
        }
        if titles::is_side_session(&entry.id) {
            entry.hidden = true;
        }
    }

    /// `title-pending`: capture aid — the active session reads as if its
    /// generation were in flight, so a `--replay` run screenshots the
    /// `Naming this session…` row without spending a turn.
    pub(crate) fn step_title_pending(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active.as_ref().map(|view| view.read(cx).session_id.clone()) else { return };
        self.titles_pending.insert(id.clone());
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == id) {
            entry.title_pending = true;
        }
        self.invalidate_list();
        cx.notify();
    }

    /// `title-land:<text>`: capture aid — land `<text>` as the active
    /// session's generated title, so a `--replay` run screenshots the
    /// placeholder-to-title update in the row and the crumb, free.
    pub(crate) fn step_title_land(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Some(id) = self.active.as_ref().map(|view| view.read(cx).session_id.clone()) else { return };
        let Some(title) = titles::clean_title(rest) else { return };
        self.set_override(&id, |meta| meta.generated_title = Some(title), cx);
        self.titles_pending.remove(&id);
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == id) {
            entry.title_pending = false;
        }
        self.invalidate_list();
        cx.notify();
    }
}
