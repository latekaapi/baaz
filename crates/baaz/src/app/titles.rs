//! Generated session titles and byline rewrites: the side-session drivers
//! (parts 2 and 3).
//!
//! [`crate::titles`] owns the title policy and [`crate::byline`] the byline
//! policy; this module drives them. On the first `turn/started` of an
//! unnamed session, [`Harness::maybe_start_title`] marks the attempt
//! (persisted, once-ever), shows the pending placeholder, and runs one
//! background completion on the session's own provider
//! ([`crate::utility::complete`]; on Muse a hidden side session the hub
//! answers, [`provider_muse::MuseHub::complete`]). The result lands as
//! `generated_title`; a Muse side session is hidden the moment it is
//! minted. On a completed turn whose free byline excerpt is poor,
//! [`Harness::maybe_rewrite_byline`] spends one debounced call the same way
//! to rewrite the two lines.
//!
//! Nothing here ever touches the UI thread except through completions: the
//! real turn is never blocked or delayed by a title. Failure (timeout, wire
//! error, missing model, switch off) falls back to today's labels with one
//! log line, never a dialog, and at most one retry per session — and a
//! timeout stands the row down without giving the call up, so a reply that
//! lands late still lands instead of being thrown away.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;
use crate::{byline, titles};

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
    pub(crate) fn remember_side_session(&mut self, side_id: &str, cx: &mut Context<Self>) {
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
            self.utility_available(self.session_lane_provider(session_id)),
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

    /// One background attempt: one completion on the session's own
    /// provider (docs/50 P2) — a Claude Code or Codex session never sends
    /// its prompt to Muse — landed, retried once, or given up. A reply that
    /// lands after the watchdog stood the row down still lands (the turn is
    /// paid for), unless the session went away or was named meanwhile.
    fn run_title_attempt(
        &mut self,
        real_id: String,
        first_message: String,
        tries: u8,
        cx: &mut Context<Self>,
    ) {
        if !self.titles_pending.contains(&real_id) {
            return;
        }
        let provider = self.session_lane_provider(&real_id);
        let prompt = titles::title_prompt(&first_message);
        let model = self.utility_model(&real_id, cx);
        let muse = self.muse_side(provider, "baaz auto-title", cx);
        let work = move || crate::utility::complete(provider, model.as_deref(), &prompt, muse);
        self.wire_call(cx, work, move |this, result, cx| {
            let late = !this.titles_pending.contains(&real_id);
            let reason = match result.map(|text| titles::clean_title(&text)) {
                Ok(Some(title)) => {
                    let known = this.sessions.iter().any(|entry| entry.id == real_id);
                    if late && (!known || !titles::should_land_late(this.overrides.get(&real_id))) {
                        crate::baaz_log!(
                            "late auto-title for {real_id} dropped (renamed or gone); keeping the current label"
                        );
                        return;
                    }
                    this.land_title(&real_id, &title, cx);
                    return;
                }
                Ok(None) => "empty reply".to_owned(),
                Err(reason) => reason,
            };
            if late {
                return;
            }
            if titles::should_retry_title(tries, false) {
                crate::baaz_log!("auto-title for {real_id} on {} failed ({reason}); retrying once", provider.label());
                this.run_title_attempt(real_id, first_message, tries + 1, cx);
            } else {
                this.fail_title(&real_id, &reason, cx);
            }
        });
    }

    /// The Muse half of a completion on `provider`: a fresh side session,
    /// recorded (hidden) before it starts so a crash between the start and
    /// the hide still hides it after a restart. `None` for the CLI
    /// providers, and while Muse is not up (the completion then fails and
    /// the caller keeps its free fallback).
    pub(crate) fn muse_side(
        &mut self,
        provider: crate::providers::ProviderId,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> Option<crate::utility::MuseSide> {
        if provider != crate::providers::ProviderId::Muse {
            return None;
        }
        let hub = self.muse_slot.hub()?;
        let side_id = titles::side_session_id();
        self.remember_side_session(&side_id, cx);
        Some(crate::utility::MuseSide { hub, side_id, label })
    }

    /// The provider whose lane `session_id` rides: its provider record's,
    /// else Muse.
    pub(crate) fn session_lane_provider(&self, session_id: &str) -> crate::providers::ProviderId {
        self.provider_sessions
            .get(session_id)
            .map(|record| crate::providers::ProviderId::parse(&record.provider))
            .unwrap_or(crate::providers::ProviderId::Muse)
    }

    /// Whether `provider` can write a title, byline or summary now: Muse
    /// through its connection, the others through their CLIs while enabled.
    pub(crate) fn utility_available(&self, provider: crate::providers::ProviderId) -> bool {
        match provider {
            crate::providers::ProviderId::Muse => self.muse_slot.is_ready(),
            other => {
                !self.scripted_chrome()
                    && crate::utility::supports(other)
                    && crate::provider_status::provider_enabled(other)
            }
        }
    }

    /// The model a one-shot completion for `session_id` runs on: the live
    /// view's own model (what Codex is known to run for this account —
    /// see [`crate::utility`]), or none when no live view names one.
    pub(crate) fn utility_model(&self, session_id: &str, cx: &Context<Self>) -> Option<String> {
        Self::live_view_for(&self.active, &self.session_cache, session_id, cx)
            .map(|view| view.read(cx).model_id())
            .filter(|model| !model.is_empty())
    }

    /// The title landed: store it under the user-given name in the label
    /// order, clear the pending state, and let the row and the crumb update
    /// in place — `set_override` rejoins the one row, so no flicker and no
    /// scroll jump.
    fn land_title(&mut self, real_id: &str, title: &str, cx: &mut Context<Self>) {
        let title = title.to_owned();
        self.set_override(real_id, |meta| meta.generated_title = Some(title), cx);
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
/// At checkpoint, when the summary could add something over the
/// extractive pack ([`crate::handoff::should_model_summary`]), the app
/// starts one hidden side session on the cheapest model and sends the
/// pack's goal + transcript excerpt
/// ([`crate::handoff::summary_prompt`]). The card's summary step reads
/// Current (with its elapsed counter) until the side session's
/// `turn/completed` harvests into the pack's summary field, the 8 s
/// watchdog keeps the extractive text, or the destination lands —
/// landing stands the wait down, because the pack submits at land with
/// the extractive summary and a later answer would never reach the
/// destination model. Timeout, wire error, empty reply and signed-out
/// all keep the extractive summary with one log line, never a dialog,
/// never a retry. Cancel during the wait abandons the hidden side
/// session: the run is already Cancelled, so the harvest and the
/// watchdog drop their answers.
impl Harness {
    /// Start the checkpoint's summary side session. Called once from the
    /// handoff checkpoint path, after the run went Checkpointed with its
    /// extractive pack and the card's summary step went Current.
    pub(crate) fn start_handoff_summary(
        &mut self,
        source: String,
        epoch: u64,
        prompt: String,
        cx: &mut Context<Self>,
    ) {
        // The source session's own provider writes the summary (docs/50
        // P2): its transcript never leaves for another provider's model.
        let provider = self.session_lane_provider(&source);
        // The watchdog is armed now, not once the completion starts: a
        // provider that hangs would otherwise hold the handoff. An answer
        // that lands after the watchdog settled finds no waiting run.
        self.arm_handoff_summary_timeout(source.clone(), epoch, cx);
        let model = self.utility_model(&source, cx);
        let muse = self.muse_side(provider, "baaz handoff summary", cx);
        let work = move || crate::utility::complete(provider, model.as_deref(), &prompt, muse);
        self.wire_call(cx, work, move |this, reply, cx| {
            let summary = match reply {
                Ok(text) => titles::clean_summary(&text),
                Err(reason) => {
                    crate::baaz_log!("handoff summary for {source} failed ({reason}); keeping the extractive summary");
                    None
                }
            };
            if this.handoff_summary_waiting(&source, epoch) {
                this.resolve_handoff_summary(source, epoch, summary, cx);
            }
        });
    }

    /// Whether the summary side session may still land: the run exists
    /// under this epoch and is still waiting. Landing stands the wait
    /// down, so in practice this only ever holds while Checkpointed —
    /// but the flag, not the state, is the guard.
    pub(crate) fn handoff_summary_waiting(&self, source: &str, epoch: u64) -> bool {
        self.handoffs.get(source).is_some_and(|run| {
            run.epoch == epoch
                && run.summarising
                && matches!(
                    &run.state,
                    aui_protocol::HandoffState::Checkpointed | aui_protocol::HandoffState::Prepared
                )
        })
    }

    /// Settle the summary wait. `Some` harvested text upgrades the pack
    /// to a model summary while still Checkpointed, and is dropped
    /// otherwise; `None` keeps the extractive one. Either way the wait
    /// ends in every state — a late harvest after acknowledge,
    /// activation, failure, landing or cancel stands a stuck card down
    /// without touching the pack (see
    /// [`crate::handoff::HandoffRun::apply_model_summary`]). The
    /// destination already opened beside the wait, so this only refreshes
    /// the card — it never opens. A run that stopped waiting meanwhile
    /// (flag already down, or a superseding epoch) only drops its jobs.
    fn resolve_handoff_summary(
        &mut self,
        source: String,
        epoch: u64,
        harvested: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let Some(mut run) = self.handoffs.get(&source).cloned() else { return };
        if run.epoch != epoch || !run.summarising {
            return;
        }
        match harvested.filter(|text| !text.trim().is_empty()) {
            Some(text) => run.apply_model_summary(text),
            None => run.note_summary_fallback(),
        }
        self.handoffs.insert(source.clone(), run);
        self.refresh_handoff_card(&source, cx);
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
    /// in time — keep the extractive text. A late answer afterwards finds
    /// no waiting run and is dropped. `pub(crate)` so lifecycle tests can
    /// drive the real handler after acknowledge.
    pub(crate) fn handoff_summary_timeout(&mut self, source: &str, epoch: u64, cx: &mut Context<Self>) {
        if !self.handoff_summary_waiting(source, epoch) {
            return;
        }
        crate::baaz_log!("handoff summary for {source} timed out; keeping the extractive summary");
        self.resolve_handoff_summary(source.to_owned(), epoch, None, cx);
    }

    /// Refresh the source's card from the run, wherever the source view
    /// is: the destination opens beside the summary wait, so the source
    /// is usually parked (or retired) when the wait settles.
    fn refresh_handoff_card(&mut self, source: &str, cx: &mut Context<Self>) {
        let Some(run) = self.handoffs.get(source) else { return };
        let card = run.card();
        let card_id = run.card_id.clone();
        if let Some(view) = self.find_view(source, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
    }

    /// Arm the per-second card refresh behind the current step's elapsed
    /// counter. The transcript re-derives the steps and the `"n s"`
    /// detail from the block at render time, so the refresh itself is
    /// what ticks the counter: one replace plus a notify, once a second.
    /// The tick re-arms itself only while the run is still in flight
    /// under this epoch — settled, superseded and missing runs own no
    /// timer (see [`crate::handoff::handoff_tick_wanted`]).
    pub(crate) fn arm_handoff_tick(&mut self, source: String, epoch: u64, cx: &mut Context<Self>) {
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
            let _ = this.update(cx, |this, cx| this.handoff_tick(&source, epoch, cx));
        });
        self.wire_tasks().push(task);
    }

    /// One tick fired: still live means refresh and re-arm, anything else
    /// means the chain ends here with no timer left behind.
    fn handoff_tick(&mut self, source: &str, epoch: u64, cx: &mut Context<Self>) {
        let live =
            self.handoffs.get(source).is_some_and(|run| run.epoch == epoch && run.is_live());
        if !live {
            return;
        }
        self.refresh_handoff_card(source, cx);
        self.arm_handoff_tick(source.to_owned(), epoch, cx);
        cx.notify();
    }
}

impl Harness {
    /// A turn completed: the free excerpt already landed via
    /// [`Self::record_last_summary`], and now the rewrite question — only
    /// when the switch is on AND the excerpt is poor, the session is idle,
    /// the pair changed, and the last start cooled down. One debounced side
    /// session, same mechanism as a title; never on a running turn.
    pub(super) fn maybe_rewrite_byline(&mut self, cx: &mut Context<Self>) {
        // The muse lane's settled-turn hook doubles as the pack check: a
        // pack turn that ended failed fails its handoff here, before any
        // byline work and independent of the byline switch below. Runs
        // first so an early return never skips it.
        self.reconcile_pack_turns(cx);
        if !self.layout.auto_summary {
            return;
        }
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        // The session's own provider rewrites its byline (docs/50 P2).
        if !self.utility_available(self.session_lane_provider(&session_id)) {
            return;
        }
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

    /// One background rewrite attempt: one completion on the session's own
    /// provider (docs/50 P2) with the rewrite prompt, landed, retried once,
    /// or given up. A late answer after the watchdog is ignored.
    fn run_byline_attempt(
        &mut self,
        real_id: String,
        ask: String,
        result: String,
        tries: u8,
        cx: &mut Context<Self>,
    ) {
        let provider = self.session_lane_provider(&real_id);
        let prompt = byline::rewrite_prompt(&ask, &result);
        let model = self.utility_model(&real_id, cx);
        let muse = self.muse_side(provider, "baaz auto-summary", cx);
        let work = move || crate::utility::complete(provider, model.as_deref(), &prompt, muse);
        self.wire_call(cx, work, move |this, reply, cx| {
            if !this.byline_live.contains(&real_id) {
                return;
            }
            let reason = match reply.map(|text| byline::parse_rewrite(&text)) {
                Ok(Some((new_ask, new_result))) => {
                    this.land_byline(&real_id, &new_ask, &new_result, cx);
                    return;
                }
                Ok(None) => "unparseable reply".to_owned(),
                Err(reason) => reason,
            };
            if tries < titles::TITLE_MAX_ATTEMPTS {
                crate::baaz_log!("auto-summary on {} failed ({reason}); retrying once", provider.label());
                this.run_byline_attempt(real_id, ask, result, tries + 1, cx);
            } else {
                this.fail_byline(&real_id, &reason, cx);
            }
        });
    }

    fn land_byline(&mut self, real_id: &str, ask: &str, result: &str, cx: &mut Context<Self>) {
        let (ask, result) = (ask.to_owned(), result.to_owned());
        self.set_override(real_id, |meta| {
            meta.last_ask = Some(ask);
            meta.last_summary = Some(result);
        }, cx);
        self.byline_live.remove(real_id);
        self.invalidate_list();
        cx.notify();
    }

    /// Silent and cheap: one log line, the free excerpt stands, never a
    /// dialog, never another attempt past budget.
    fn fail_byline(&mut self, real_id: &str, reason: &str, cx: &mut Context<Self>) {
        crate::baaz_log!("auto-summary for {real_id} failed ({reason}); keeping the free excerpt");
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

/// D6: drafts on Muse through side sessions (the right pane's commit box
/// and PR form; docs/43 §2.6), here beside the titles that use the same
/// route, so the wire stays behind this seam.
impl Harness {
    /// Draft on Muse: one completion in a hidden side session; the answer
    /// lands where the person has not typed.
    pub(crate) fn draft_on_muse(&mut self, root: &std::path::Path, kind: crate::right_changes::DraftKind, prompt: String, cx: &mut Context<Self>) {
        let muse_id = crate::providers::ProviderId::Muse;
        let Some(muse) = self.muse_side(muse_id, "baaz draft", cx) else {
            self.draft_failed(root, kind, cx);
            return;
        };
        let root = root.to_path_buf();
        let work = move || crate::utility::complete(muse_id, None, &prompt, Some(muse));
        self.wire_call(cx, work, move |this, reply, cx| {
            this.draft_failed(&root, kind, cx);
            let text = match reply {
                Ok(text) => crate::right_changes::clean_draft(&text),
                Err(reason) => {
                    crate::baaz_log!("draft on Muse failed: {reason}");
                    return;
                }
            };
            if text.is_empty() {
                return;
            }
            match kind {
                crate::right_changes::DraftKind::Commit => {
                    let ui = this.changes_ui(&root);
                    if !ui.message_edited && ui.message.trim().is_empty() {
                        ui.message = text;
                    }
                }
                crate::right_changes::DraftKind::PrBody => {
                    if let Some(form) = this.changes_ui(&root).form.as_ref() {
                        let body = form.body.clone();
                        if body.read(cx).value().is_empty() {
                            this.pending_pr_body = Some((body, text));
                        }
                    }
                }
            }
            cx.notify();
        });
    }

}
