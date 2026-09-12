//! The clocks a session keeps: the question countdown and the retry
//! backoff, both ticked here because MSP sends durations and never deadlines.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ---------------------------------------------------------------- clocks

    /// Notice a countdown that has started, and start (or stop) the 1 s clock.
    ///
    /// MSP sends durations, never deadlines — an `autoResolutionMs` and a
    /// `retryDelayMs` — so the moment each one was observed is the app's to
    /// remember and the countdown is the app's to derive.
    pub(super) fn observe_clocks(&mut self, cx: &mut Context<Self>) {
        let Some(side) = self.fold.side(&self.session_id) else { return };
        let pending: Vec<String> = side.pending_inputs.keys().cloned().collect();
        let retry = side.retry.as_ref().map(|r| format!("{}:{}", r.turn_id, r.attempt));
        for id in &pending {
            self.question_started.entry(id.clone()).or_insert_with(crate::clock::now_instant);
        }
        self.question_started.retain(|id, _| pending.contains(id));
        match retry {
            Some(key) => {
                if self.retry_started.as_ref().map(|(k, _)| k.as_str()) != Some(key.as_str()) {
                    self.retry_started = Some((key, crate::clock::now_instant()));
                }
            }
            None => self.retry_started = None,
        }
        let wanted = !self.question_started.is_empty() || self.retry_started.is_some();
        match (wanted, self.countdown.is_some()) {
            (true, false) => self.start_countdown(cx),
            (false, true) => self.countdown = None,
            _ => {}
        }
    }

    /// One frame a second while anything is counting down. Deliberately not the
    /// 250 ms turn ticker: a countdown that changes once a second has no
    /// business waking the window four times as often.
    pub(super) fn start_countdown(&mut self, cx: &mut Context<Self>) {
        self.countdown = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(COUNTDOWN_TICK).await;
            let alive = this.update(cx, |this, cx| {
                cx.notify();
                !this.question_started.is_empty() || this.retry_started.is_some()
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
    }

    /// How long each pending question has left, keyed by its block id.
    pub(super) fn countdowns(&self) -> HashMap<String, (u64, u64)> {
        let Some(side) = self.fold.side(&self.session_id) else { return HashMap::new() };
        let mut out = HashMap::new();
        for (input_id, request) in &side.pending_inputs {
            let Some(total) = request.auto_resolution_ms else { continue };
            let Some(started) = self.question_started.get(input_id) else { continue };
            let remaining = total.saturating_sub(crate::clock::elapsed_since(*started).as_millis() as u64);
            for question in &request.questions {
                out.insert(format!("{input_id}:{}", question.id), (remaining, total));
            }
        }
        out
    }

    /// Re-read what is pending, after a resume or a reconnect.
    ///
    /// The server does not re-issue an `approval/request` it already sent, so a
    /// client that was away has to pull. `approval/listPending` is that pull;
    /// the fold dedupes on ids it has already seen, so folding both lists is
    /// safe even when nothing was missed.
    pub(super) fn refresh_pending_now(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let params = ApprovalListPendingParams { session_id: self.session_id.clone() };
        let session_id = self.session_id.clone();
        self.wire_call(cx, move || client.approval_list_pending(&params), move |this, result, cx| {
            let Ok(pending) = result else { return };
            let fold = |this: &mut Self, method: &str, value: &serde_json::Value| {
                this.fold.apply(MuseEvent::Notification {
                    method: method.to_owned(),
                    params: value.clone(),
                    cursor: value.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned),
                    session_id: Some(session_id.clone()),
                });
            };
            for approval in &pending.approvals {
                if let Ok(value) = serde_json::to_value(approval) {
                    fold(this, "approval/requested", &value);
                }
            }
            for request in &pending.user_inputs {
                if let Ok(value) = serde_json::to_value(request) {
                    fold(this, "userInput/requested", &value);
                }
            }
            this.observe_clocks(cx);
            cx.notify();
        });
    }

    /// Whether anything is waiting on the person, for the needs-you banner.
    pub(super) fn waiting_on_you(&self) -> Option<(usize, usize)> {
        let side = self.fold.side(&self.session_id)?;
        let (approvals, questions) = (side.pending_approvals.len(), side.pending_inputs.len());
        (approvals + questions > 0).then_some((approvals, questions))
    }

    /// The live retry row's data: attempt, bound, what is left of the backoff,
    /// and the reason the provider gave.
    pub(super) fn retry_countdown(&self) -> Option<(u32, u32, u64, String)> {
        let retry = self.fold.side(&self.session_id)?.retry.clone()?;
        let started = self.retry_started.as_ref()?.1;
        let remaining = retry.retry_delay_ms.saturating_sub(crate::clock::elapsed_since(started).as_millis() as u64);
        Some((retry.attempt, retry.max_attempts, remaining, retry.reason))
    }
}
