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
///
/// The same map also carries handoff-summary jobs (Z8): when
/// `handoff_epoch` is `Some`, the side session is a checkpoint's summary
/// turn for `real_id` (the handoff source) under that owner-epoch — never
/// a title. [`Harness::harvest_title`] diverts those to the summary
/// harvest, and the title settle paths leave them alone.
#[derive(Clone, Debug)]
pub(crate) struct TitleJob {
    /// The real session this names.
    pub real_id: String,
    /// 1 for the first try, 2 for the one retry.
    pub tries: u8,
    /// The first message the prompt was built from, so a harvest retry
    /// re-asks about the same text rather than about nothing.
    pub first_message: String,
    /// `Some(epoch)` when this job is a handoff summary, `None` for a title.
    pub handoff_epoch: Option<u64>,
}

impl Harness {
    /// Whether `session_id` names one of this app's throwaway title/summary
    /// side sessions. The explicit record — the in-memory set this run
    /// minted, or the persisted `side_session` flag a restart reloaded —
    /// never the id shape: a side id is a bare uuid, exactly like a real
    /// session's, because muse 1.3.0 rejects any `session/start` id that is
    /// not its own shape. Beside it, the wire-alone mark the row joined
    /// with: a side started by any other state dir — a scratch run, a relay
    /// lane, a second install, a restored backup — carries no local record
    /// here, so its prompt prefix or side workspace speaks instead. The
    /// local override path keeps working exactly as before.
    pub(crate) fn is_side_session(&self, session_id: &str) -> bool {
        self.side_sessions.contains(session_id)
            || self.overrides.get(session_id).is_some_and(|meta| meta.side_session)
            || self.sessions.iter().any(|entry| entry.id == session_id && entry.side_marker)
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
        // A handoff destination keeps the chain title: never start the
        // paid titler there, whatever the turn count reads (Y2a). Any
        // other session has no pack turn, so its first message always
        // qualifies, pack-shaped or not (Y2a3).
        if crate::sidebar::is_handoff_dest(session_id, &self.provider_sessions, &self.overrides) {
            return;
        }
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
        // The side workspace, never the real session's: the prompt already
        // carries the user's message, so the side session needs no repo —
        // and outside every adoption it can never match a project.
        let workspace = titles::side_workspace_dir().to_string_lossy().into_owned();
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
                    display_text: Some("baaz auto-title".to_owned()),
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
                    TitleJob { real_id: real_id.clone(), tries, first_message: retry_message, handoff_epoch: None },
                );
                cx.notify();
            }
            Err(reason) => {
                if !this.titles_pending.contains(&real_id) {
                    return;
                }
                if titles::should_retry_title(tries, false) {
                    crate::baaz_log!("auto-title for {real_id} failed ({reason}); retrying once");
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
        // A handoff-summary side session rides the same map under its
        // `handoff_epoch` marker: harvest the summary, never a title.
        if self.title_jobs.get(side_id).is_some_and(|job| job.handoff_epoch.is_some()) {
            self.harvest_handoff_summary(side_id, cx);
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
                            crate::baaz_log!(
                                "late auto-title for {} dropped (renamed or gone); keeping the current label",
                                job.real_id
                            );
                            this.title_jobs.remove(&side_id);
                            return;
                        }
                        this.land_title(&job.real_id, &side_id, &title, cx);
                    }
                    None if titles::should_retry_title(job.tries, late) => {
                        crate::baaz_log!("auto-title for {} came back empty; retrying once", job.real_id);
                        this.title_jobs.remove(&side_id);
                        this.run_title_attempt(job.real_id, job.first_message, job.tries + 1, cx);
                    }
                    None => this.fail_title(&job.real_id, "empty reply", cx),
                },
                Err(error) => {
                    if titles::should_retry_title(job.tries, late) {
                        crate::baaz_log!("auto-title read for {} failed ({error}); retrying once", job.real_id);
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
    /// a dialog, never another attempt. A handoff-summary job for the same
    /// session is not a title and survives this — it settles on its own
    /// harvest or watchdog.
    fn fail_title(&mut self, real_id: &str, reason: &str, cx: &mut Context<Self>) {
        crate::baaz_log!("auto-title for {real_id} failed ({reason}); keeping the first prompt");
        self.titles_pending.remove(real_id);
        self.title_jobs.retain(|_, job| job.real_id != real_id || job.handoff_epoch.is_some());
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
        crate::baaz_log!("auto-title for {real_id} failed ({reason}); keeping the first prompt");
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

/// A handoff checkpoint's model-written summary (Z8): the side-session
/// driver, same shape as a title.
///
/// At checkpoint, with the switch on and a Muse sign-in, the app starts
/// one hidden side session on the cheapest model and sends the pack's
/// goal + transcript excerpt ([`crate::handoff::summary_prompt`]). The
/// run stays Checkpointed — the card reads "Summarising…" — until the
/// side session's `turn/completed` harvests into the pack's summary field
/// or the 20 s watchdog keeps the extractive text; only then does the
/// destination open. Timeout, wire error, empty reply and signed-out all
/// keep the extractive summary with one log line, never a dialog, never
/// a retry. Cancel during the wait abandons the hidden side session: the
/// run is already Cancelled, so the harvest and the watchdog drop their
/// answers and no destination ever opens.
impl Harness {
    /// Start the checkpoint's summary side session. Called once from the
    /// handoff checkpoint path, after the run went Checkpointed with its
    /// extractive pack and the card went "Summarising…".
    pub(crate) fn start_handoff_summary(
        &mut self,
        source: String,
        epoch: u64,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        let Some(client) = self.client.clone() else {
            // Signed out between the checkpoint's check and now: keep the
            // extractive text and open the destination.
            self.resolve_handoff_summary(source, epoch, None, cx);
            return;
        };
        // The side workspace, never the real session's: the prompt already
        // carries the goal and the excerpt, so the side session needs no
        // repo — and outside every adoption it can never match a project.
        let workspace = titles::side_workspace_dir().to_string_lossy().into_owned();
        let side_id = titles::side_session_id();
        // Recorded before the start runs, so a crash between the start and
        // the hide still hides by record after a restart.
        self.remember_side_session(&side_id, cx);
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
                    display_text: Some("baaz handoff summary".to_owned()),
                    ..Default::default()
                })
                .map_err(|error| format!("turn/start: {error}"))?;
            Ok(started_id)
        };
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok(side_id) => {
                // Already hidden by the pre-start record, before any list
                // refresh could show it. Marked as a summary job, so the
                // shared `turn/completed` route harvests a summary, and so
                // a title settle for the same session never touches it.
                this.title_jobs.insert(
                    side_id,
                    TitleJob {
                        real_id: source.clone(),
                        tries: 1,
                        first_message: String::new(),
                        handoff_epoch: Some(epoch),
                    },
                );
                this.arm_handoff_summary_timeout(source, epoch, cx);
                cx.notify();
            }
            Err(reason) => {
                crate::baaz_log!(
                    "handoff summary for {source} failed ({reason}); keeping the extractive summary"
                );
                this.resolve_handoff_summary(source, epoch, None, cx);
            }
        });
    }

    /// A `turn/completed` on a summary side session: harvest its answer
    /// with a free `session/read`, then resolve the wait — model text in,
    /// or the extractive text on an empty reply or a wire error — and open
    /// the destination. A reply that lands after Cancel (or a supersede)
    /// finds no waiting run and is dropped: the side session stays hidden,
    /// nothing opens.
    fn harvest_handoff_summary(&mut self, side_id: &str, cx: &mut Context<Self>) {
        let Some(job) = self.title_jobs.get(side_id).cloned() else { return };
        let Some(epoch) = job.handoff_epoch else { return };
        if !self.handoff_summary_waiting(&job.real_id, epoch) {
            self.title_jobs.remove(side_id);
            return;
        }
        let Some(client) = self.client.clone() else {
            self.resolve_handoff_summary(job.real_id, epoch, None, cx);
            return;
        };
        let side_id = side_id.to_owned();
        let read_id = side_id.clone();
        let work = move || {
            client.session_read(&muse_client::schema::SessionReadParams {
                session_id: read_id.clone(),
                exclude_items: Some(false),
            })
        };
        self.wire_call(cx, work, move |this, result, cx| {
            // Abandoned while the read ran (cancelled, superseded): drop.
            if !this.handoff_summary_waiting(&job.real_id, epoch) {
                this.title_jobs.remove(&side_id);
                return;
            }
            match result {
                Ok(read) => {
                    let harvested = titles::harvest_summary_text(&read);
                    if harvested.is_none() {
                        crate::baaz_log!(
                            "handoff summary for {} came back empty; keeping the extractive summary",
                            job.real_id
                        );
                    }
                    this.resolve_handoff_summary(job.real_id, epoch, harvested, cx);
                }
                Err(error) => {
                    crate::baaz_log!(
                        "handoff summary read for {} failed ({error}); keeping the extractive summary",
                        job.real_id
                    );
                    this.resolve_handoff_summary(job.real_id, epoch, None, cx);
                }
            }
        });
    }

    /// Whether the summary side session may still land: the run exists
    /// under this epoch, is still Checkpointed, and is still waiting.
    fn handoff_summary_waiting(&self, source: &str, epoch: u64) -> bool {
        self.handoffs.get(source).is_some_and(|run| {
            run.epoch == epoch && run.summarising && matches!(&run.state, aui_protocol::HandoffState::Checkpointed)
        })
    }

    /// Settle the summary wait and open the destination. `Some` harvested
    /// text upgrades the pack to a model summary; `None` keeps the
    /// extractive one. A run that stopped waiting meanwhile (cancelled,
    /// superseded, failed) is left alone and nothing opens.
    fn resolve_handoff_summary(
        &mut self,
        source: String,
        epoch: u64,
        harvested: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut run) = self.handoffs.get(&source).cloned() else { return };
        if run.epoch != epoch
            || !run.summarising
            || !matches!(&run.state, aui_protocol::HandoffState::Checkpointed)
        {
            self.title_jobs.retain(|_, job| !(job.real_id == source && job.handoff_epoch == Some(epoch)));
            return;
        }
        match harvested.filter(|text| !text.trim().is_empty()) {
            Some(text) => run.apply_model_summary(text),
            None => run.note_summary_fallback(),
        }
        self.handoffs.insert(source.clone(), run);
        self.title_jobs.retain(|_, job| !(job.real_id == source && job.handoff_epoch == Some(epoch)));
        self.refresh_handoff_card(&source, cx);
        // The destination opens on a window, like every other open: back
        // through `update_in`, the way a window-less event reopens.
        self.tasks.push(cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| this.open_handoff_destination(source, epoch, window, cx));
        }));
        cx.notify();
    }

    /// One watchdog per summary: [`crate::handoff::SUMMARY_TIMEOUT_SECS`]
    /// on the background executor, then back on the UI thread. Single
    /// attempt, never a retry — the turn may still be running server-side,
    /// so another `turn/start` would double-bill a turn this checkpoint
    /// already owns.
    fn arm_handoff_summary_timeout(&mut self, source: String, epoch: u64, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_secs(crate::handoff::SUMMARY_TIMEOUT_SECS))
                .await;
            let _ = this.update(cx, |this, cx| this.handoff_summary_timeout(&source, epoch, cx));
        });
        self.wire_tasks().push(task);
    }

    /// The watchdog fired: still waiting means the summary did not arrive
    /// in time — keep the extractive text and open the destination. A late
    /// answer afterwards finds no waiting run and is dropped.
    fn handoff_summary_timeout(&mut self, source: &str, epoch: u64, cx: &mut Context<Self>) {
        if !self.handoff_summary_waiting(source, epoch) {
            return;
        }
        crate::baaz_log!("handoff summary for {source} timed out; keeping the extractive summary");
        self.resolve_handoff_summary(source.to_owned(), epoch, None, cx);
    }

    /// Refresh the source's card from the run, when the source view is the
    /// active one (it is: the destination has not opened yet). A parked
    /// source heals at landing, which rebuilds its card from the run.
    fn refresh_handoff_card(&mut self, source: &str, cx: &mut Context<Self>) {
        let Some(run) = self.handoffs.get(source) else { return };
        let card = run.card();
        let card_id = run.card_id.clone();
        if let Some(view) = self.active.clone() {
            if view.read(cx).session_id == source {
                view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
            }
        }
    }

    /// Open the handoff destination after the summary settled: the
    /// checkpoint path's own tail (`pending_handoff`, then the lane open),
    /// rerun with the resolved pack. Guarded like the settle — a run that
    /// stopped waiting meanwhile opens nothing.
    fn open_handoff_destination(
        &mut self,
        source: String,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(run) = self.handoffs.get(&source).cloned() else { return };
        if run.epoch != epoch
            || run.summarising
            || !matches!(&run.state, aui_protocol::HandoffState::Checkpointed)
        {
            return;
        }
        let workspace = run.pack.map(|pack| pack.workspace).unwrap_or_default();
        let to = run.to;
        self.pending_handoff = Some(crate::handoff::PendingHandoff { source_session: source.clone(), epoch });
        if to == crate::providers::ProviderId::Muse {
            self.select_new_provider(crate::providers::ProviderId::Muse, cx);
            self.session_switch_pending = true;
            self.switch_claim = Some((source, self.switch_epoch));
            self.new_session(window, cx);
        } else {
            let project =
                self.projects.resolve_available(Some(&workspace), None).map(|p| p.id.clone());
            self.open_on_provider(to, project, workspace, window, cx);
        }
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
        // The pack turn and its acknowledgement are the handoff speaking,
        // never words to rewrite a byline from — but only a handoff
        // destination ever has a pack turn (Y2a, Y2a3).
        if crate::sidebar::is_handoff_dest(&session_id, &self.provider_sessions, &self.overrides)
            && ask.as_deref().is_some_and(crate::sidebar::is_pack_text)
        {
            return;
        }
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
        // Same side workspace as a title side session: the rewrite prompt
        // already quotes both lines, so no repo is needed either.
        let workspace = titles::side_workspace_dir().to_string_lossy().into_owned();
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
                    display_text: Some("baaz auto-summary".to_owned()),
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
                    crate::baaz_log!("auto-summary failed ({reason}); retrying once");
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
                            crate::baaz_log!("auto-summary came back unusable; retrying once");
                            this.byline_jobs.remove(&side_id);
                            this.run_byline_attempt(job.real_id, job.ask, job.result, job.tries + 1, cx);
                        }
                        None => this.fail_byline(&job.real_id, "unusable reply", cx),
                    }
                }
                Err(error) => {
                    if job.tries < titles::TITLE_MAX_ATTEMPTS {
                        crate::baaz_log!("auto-summary read failed ({error}); retrying once");
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
        crate::baaz_log!("auto-summary for {real_id} failed ({reason}); keeping the free excerpt");
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
