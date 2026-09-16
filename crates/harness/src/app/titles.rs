//! Generated session titles and byline rewrites: the side-session drivers
//! (parts 2 and 3).
//!
//! [`crate::titles`] owns the title policy and [`crate::byline`] the byline
//! policy; this module owns the wire. On the first `turn/started` of an
//! unnamed session, [`Harness::maybe_start_title`] marks the attempt
//! (persisted, once-ever), shows the pending placeholder, and runs one
//! background chain — `model/list`, `session/start` with `modelId` pinned
//! and a namespaced client id, one `turn/start` — then waits for that side
//! session's `turn/completed`, harvested with a free `session/read`. The
//! result lands as `generated_title`; the side session is hidden the moment
//! it starts. On a completed turn whose free byline excerpt is poor,
//! [`Harness::maybe_rewrite_byline`] spends one debounced call the same way
//! to rewrite the two lines.
//!
//! Nothing here ever touches the UI thread except through completions: the
//! real turn is never blocked or delayed by a title. Failure (timeout, wire
//! error, missing model, switch off) falls back to today's labels with one
//! log line, never a dialog, and at most one retry per session — and a
//! timeout stands the row down without giving the job up, so a reply that
//! lands late still harvests read-only instead of being thrown away.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;
use crate::{byline, titles};

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
    /// Whether `session_id` names one of this app's throwaway title/summary
    /// side sessions. The explicit record — the in-memory set this run
    /// minted, or the persisted `side_session` flag a restart reloaded —
    /// never the id shape: a side id is a bare uuid, exactly like a real
    /// session's, because muse 1.3.0 rejects any `session/start` id that is
    /// not its own shape.
    pub(crate) fn is_side_session(&self, session_id: &str) -> bool {
        self.side_sessions.contains(session_id)
            || self.overrides.get(session_id).is_some_and(|meta| meta.side_session)
    }

    /// Remember a freshly minted side id before its `session/start` runs:
    /// the in-memory set, and the persisted `side_session` plus `hidden`
    /// override. Landing first means a crash between the start and the hide
    /// still hides by record after a restart — the old prefix rule's one
    /// job, without an id shape the server rejects.
    fn remember_side_session(&mut self, side_id: &str, cx: &mut Context<Self>) {
        self.side_sessions.insert(side_id.to_owned());
        self.set_override(
            side_id,
            |meta| {
                meta.side_session = true;
                meta.hidden = true;
            },
            cx,
        );
    }

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
            self.is_side_session(session_id),
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
        // Recorded before the start runs, so a crash between the start and
        // the hide still hides by record after a restart.
        self.remember_side_session(&side_id, cx);
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
                // Already hidden by the pre-start record, before any list
                // refresh could show it.
                // A timeout that fired while the chain ran stood the row
                // down, but the turn this started is paid for: record the
                // job anyway, so its `turn/completed` still harvests
                // read-only below rather than dropping unheard. Recording
                // starts nothing — no second generation, never a retry.
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
                if titles::should_retry_title(tries, false) {
                    crate::harness_log!("auto-title for {real_id} failed ({reason}); retrying once");
                    this.run_title_attempt(real_id, retry_message, tries + 1, cx);
                } else {
                    this.fail_title(&real_id, &reason, cx);
                }
            }
        });
    }

    /// A `turn/completed` on a side session: harvest its answer with a free
    /// `session/read`, then land the title or retry / give up per budget. A
    /// reply that lands after the watchdog stood the job down harvests the
    /// same way — the turn is already paid for — except it never retries
    /// into a second generation and lands only while the session still
    /// wants a name (gone, or named since, and it is dropped).
    pub(super) fn harvest_title(&mut self, side_id: &str, cx: &mut Context<Self>) {
        // Stood down already (timeout): the row fell back to the first
        // prompt, but the job stays so this late `turn/completed` still
        // harvests below instead of being dropped unheard. (The closure
        // re-reads the pending set: a stand-down can land between here and
        // the read coming back.)
        if !self.title_jobs.contains_key(side_id) {
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
            let late = !this.titles_pending.contains(&job.real_id);
            match result {
                Ok(read) => match titles::harvest_title_text(&read) {
                    Some(title) => {
                        // A late answer lands exactly like an in-time one —
                        // unless the session went away or someone named it
                        // while the turn ran, in which case it is dropped.
                        let known = this.sessions.iter().any(|entry| entry.id == job.real_id);
                        if late
                            && (!known
                                || !titles::should_land_late(this.overrides.get(&job.real_id)))
                        {
                            crate::harness_log!(
                                "late auto-title for {} dropped (renamed or gone); keeping the current label",
                                job.real_id
                            );
                            this.title_jobs.remove(&side_id);
                            return;
                        }
                        this.land_title(&job.real_id, &side_id, &title, cx);
                    }
                    None if titles::should_retry_title(job.tries, late) => {
                        crate::harness_log!("auto-title for {} came back empty; retrying once", job.real_id);
                        this.title_jobs.remove(&side_id);
                        this.run_title_attempt(job.real_id, job.first_message, job.tries + 1, cx);
                    }
                    None => this.fail_title(&job.real_id, "empty reply", cx),
                },
                Err(error) => {
                    if titles::should_retry_title(job.tries, late) {
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

    /// Give up waiting: the row falls back to the first prompt and the
    /// placeholder clears, but the job stays — the side session (already
    /// hidden) is left to its turn, and a late answer still harvests
    /// read-only rather than being thrown away. Stood down is not given up:
    /// never a retry, never a second generation, so a timeout can never
    /// double-bill.
    fn title_timeout(&mut self, real_id: &str, cx: &mut Context<Self>) {
        if !self.titles_pending.contains(real_id) {
            return;
        }
        self.stand_down_title(real_id, &format!("no answer in {} s", titles::TITLE_TIMEOUT_SECS), cx);
    }

    /// Stand down, not give up: the pending placeholder falls back to the
    /// first prompt — however the job ends, it never lingers — while the
    /// job stays for a late harvest. The once-ever marker is untouched, so
    /// no resume, reconnect, replay or restart can start another generation
    /// off the back of this one.
    fn stand_down_title(&mut self, real_id: &str, reason: &str, cx: &mut Context<Self>) {
        crate::harness_log!("auto-title for {real_id} failed ({reason}); keeping the first prompt");
        self.titles_pending.remove(real_id);
        if let Some(entry) = self.sessions.iter_mut().find(|e| e.id == real_id) {
            entry.title_pending = false;
        }
        self.invalidate_list();
        cx.notify();
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
    /// override writes land. A free function of the two sets (rather than
    /// `&self`) so [`Self::rejoin`] can call it mid-iteration over its own
    /// rows; the persisted `hidden` override is what carries a restart.
    pub(super) fn apply_title_flags(
        pending: &std::collections::HashSet<String>,
        sides: &std::collections::HashSet<String>,
        entry: &mut crate::sidebar::SessionEntry,
    ) {
        if pending.contains(&entry.id) {
            entry.title_pending = true;
        }
        if sides.contains(&entry.id) {
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

    /// `title-timeout`: capture aid — stand the active session's generation
    /// down through the watchdog's own path, so a `--replay` run screenshots
    /// the `Naming this session…` fallback to the first prompt without
    /// spending a turn. A following `title-land:<text>` then lands like a
    /// late answer would.
    pub(crate) fn step_title_timeout(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active.as_ref().map(|view| view.read(cx).session_id.clone()) else { return };
        self.title_timeout(&id, cx);
    }

    /// `title-land:<text>`: capture aid — land `<text>` as the active
    /// session's generated title, so a `--replay` run screenshots the
    /// placeholder-to-title update in the row and the crumb, free. After a
    /// `title-timeout` the same step lands like a late answer: the row fell
    /// back to the first prompt meanwhile, and the title updates it in
    /// place — exactly what a stood-down job's harvest does on the wire.
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

/// One byline rewrite in flight: which real session it summarises, which
/// attempt of the budget, and the poor excerpt it re-asks about (retries
/// reuse it rather than re-reading a transcript that may have moved on).
#[derive(Clone, Debug)]
pub(crate) struct BylineJob {
    /// The real session this summarises.
    pub real_id: String,
    /// 1 for the first try, 2 for the one retry.
    pub tries: u8,
    /// The poor ask half, as excerpted when the rewrite started.
    pub ask: String,
    /// The poor result half, as excerpted when the rewrite started.
    pub result: String,
}

impl Harness {
    /// A turn completed: the free excerpt already landed via
    /// [`Self::record_last_summary`], and now the rewrite question — only
    /// when the switch is on AND the excerpt is poor, the session is idle,
    /// the pair changed, and the last start cooled down. One debounced side
    /// session, same mechanism as a title; never on a running turn.
    pub(super) fn maybe_rewrite_byline(&mut self, cx: &mut Context<Self>) {
        if !self.layout.auto_summary || self.client.is_none() {
            return;
        }
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        if self.is_side_session(&session_id) {
            return;
        }
        let running = view.read(cx).is_sending();
        let ask = view.read(cx).last_user_text();
        let result = view.read(cx).last_summary_text();
        if ask.is_none() && result.is_none() {
            return;
        }
        let poor = byline::is_poor_excerpt(ask.as_deref(), result.as_deref());
        let stored = self.overrides.get(&session_id);
        let stored_pair =
            (stored.and_then(|m| m.last_ask.as_deref()), stored.and_then(|m| m.last_summary.as_deref()));
        let now = std::time::Instant::now();
        if !byline::should_rewrite(
            true,
            running,
            poor,
            stored_pair,
            (ask.as_deref(), result.as_deref()),
            self.byline_last_start.get(&session_id).copied(),
            now,
        ) {
            return;
        }
        self.byline_last_start.insert(session_id.clone(), now);
        self.byline_live.insert(session_id.clone());
        // One watchdog for the whole rewrite, armed with the start rather
        // than with an admission: a hanging chain still stands down on time,
        // and a retry never extends the deadline.
        self.arm_byline_timeout(&session_id, cx);
        self.run_byline_attempt(
            session_id,
            ask.unwrap_or_default(),
            result.unwrap_or_default(),
            1,
            cx,
        );
        cx.notify();
    }

    /// One background rewrite attempt: the same side-session shape as a
    /// title — model list, start, one prompt — with the rewrite prompt.
    fn run_byline_attempt(
        &mut self,
        real_id: String,
        ask: String,
        result: String,
        tries: u8,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else { return };
        if self.byline_jobs.values().any(|job| job.real_id == real_id) {
            return;
        }
        let workspace = self.session_workspace(&real_id);
        let side_id = titles::side_session_id();
        // Recorded before the start runs, like a title side session.
        self.remember_side_session(&side_id, cx);
        let prompt = byline::rewrite_prompt(&ask, &result);
        let retry = (ask.clone(), result.clone());
        let work = move || -> Result<String, String> {
            let models = client
                .model_list(&muse_client::schema::ModelListParams { session_id: None })
                .map(|r| r.models)
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
                    display_text: Some("harness auto-summary".to_owned()),
                    ..Default::default()
                })
                .map_err(|error| format!("turn/start: {error}"))?;
            Ok(started_id)
        };
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok(side_id) => {
                // Already hidden by the pre-start record, whatever follows.
                // Stood down while the chain ran: hide and ignore, never
                // land a late answer over the free excerpt.
                if !this.byline_live.contains(&real_id) {
                    return;
                }
                this.byline_jobs.insert(
                    side_id,
                    BylineJob { real_id, tries, ask: retry.0, result: retry.1 },
                );
                cx.notify();
            }
            Err(reason) => {
                if !this.byline_live.contains(&real_id) {
                    return;
                }
                if tries < titles::TITLE_MAX_ATTEMPTS {
                    crate::harness_log!("auto-summary failed ({reason}); retrying once");
                    let (ask, result) = retry;
                    this.run_byline_attempt(real_id, ask, result, tries + 1, cx);
                } else {
                    this.fail_byline(&real_id, &reason, cx);
                }
            }
        });
    }

    /// A rewrite side session's turn completed: read its two lines free and
    /// land them, or retry / give up per budget.
    pub(super) fn harvest_byline(&mut self, side_id: &str, cx: &mut Context<Self>) {
        if !self.byline_jobs.contains_key(side_id) {
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
            let Some(job) = this.byline_jobs.get(&side_id).cloned() else { return };
            match result {
                Ok(read) => {
                    let texts: Vec<String> = {
                        let inline = read.history.items.iter().flatten();
                        let snapshot = read.history.snapshot.iter().flat_map(|s| s.state.items.iter());
                        inline
                            .chain(snapshot)
                            .filter(|item| item.kind == muse_client::schema::ItemKind::AgentMessage)
                            .filter_map(|item| item.text.clone())
                            .collect()
                    };
                    match byline::parse_rewrite(&texts.join("\n")) {
                        Some((ask, result)) => this.land_byline(&job.real_id, &side_id, &ask, &result, cx),
                        None if job.tries < titles::TITLE_MAX_ATTEMPTS => {
                            crate::harness_log!("auto-summary came back unusable; retrying once");
                            this.byline_jobs.remove(&side_id);
                            this.run_byline_attempt(job.real_id, job.ask, job.result, job.tries + 1, cx);
                        }
                        None => this.fail_byline(&job.real_id, "unusable reply", cx),
                    }
                }
                Err(error) => {
                    if job.tries < titles::TITLE_MAX_ATTEMPTS {
                        crate::harness_log!("auto-summary read failed ({error}); retrying once");
                        this.byline_jobs.remove(&side_id);
                        this.run_byline_attempt(job.real_id, job.ask, job.result, job.tries + 1, cx);
                    } else {
                        this.fail_byline(&job.real_id, &error.to_string(), cx);
                    }
                }
            }
        });
    }

    /// The rewrite landed: both halves overwrite the free excerpt, and the
    /// row updates in place through the one-row rejoin.
    fn land_byline(&mut self, real_id: &str, side_id: &str, ask: &str, result: &str, cx: &mut Context<Self>) {
        let (ask, result) = (ask.to_owned(), result.to_owned());
        self.set_override(real_id, |meta| {
            meta.last_ask = Some(ask);
            meta.last_summary = Some(result);
        }, cx);
        self.byline_jobs.remove(side_id);
        self.byline_live.remove(real_id);
        self.invalidate_list();
        cx.notify();
    }

    /// Silent and cheap: one log line, the free excerpt stands, never a
    /// dialog, never another attempt past budget.
    fn fail_byline(&mut self, real_id: &str, reason: &str, cx: &mut Context<Self>) {
        crate::harness_log!("auto-summary for {real_id} failed ({reason}); keeping the free excerpt");
        self.byline_jobs.retain(|_, job| job.real_id != real_id);
        self.byline_live.remove(real_id);
        cx.notify();
    }

    /// Give up waiting on a rewrite: same shape as a title timeout — the
    /// free excerpt stands, and a late answer is ignored, never retried.
    fn byline_timeout(&mut self, real_id: &str, cx: &mut Context<Self>) {
        if !self.byline_live.contains(real_id) {
            return;
        }
        self.fail_byline(real_id, &format!("no answer in {} s", titles::TITLE_TIMEOUT_SECS), cx);
    }

    /// One watchdog per rewrite start, parked with the wire tasks.
    fn arm_byline_timeout(&mut self, real_id: &str, cx: &mut Context<Self>) {
        let real_id = real_id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(titles::TITLE_TIMEOUT_SECS)).await;
            let _ = this.update(cx, |this, cx| this.byline_timeout(&real_id, cx));
        });
        self.wire_tasks().push(task);
    }
}
