//! What the composer's verbs do: send, steer, interrupt, the slash
//! commands, and plan mode.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // -------------------------------------------------------------- commands

    /// Send the draft as a turn (`turn/start`, provider `meta`).
    ///
    /// `displayText` carries what the person typed, verbatim: it is what the
    /// transcript shows, and it stays the person's words even when plan mode
    /// prefixes the model-visible input.
    pub fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() && self.images.is_empty() && self.files.is_empty() {
            return;
        }
        // An image still being read has no bytes to send (finding
        // `performance-14`). The send button is already down; this is the
        // keyboard's path to the same refusal, and the draft stays put.
        if self.attachments_pending() {
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.note_draft(cx);
        // `!` is the shell escape hatch (research §1.12): a command, not a turn,
        // outside any turn, and still subject to the approval policy.
        if let Some(command) = text.strip_prefix('!') {
            if !command.trim().is_empty() {
                self.run_user_shell(command.trim().to_owned(), cx);
                return;
            }
        }
        // A `/` command typed in full and sent is the command, not a prompt.
        // The menu is one way to reach these; typing is the other, and it is
        // the only way to reach the one that takes an argument (`/name`).
        if let Some((command, argument)) = Command::parse_line(&text) {
            self.run_command_with(command, argument.to_owned(), window, cx);
            return;
        }
        self.submit(text, cx);
    }

    /// Send `text` without touching the composer — the scripting hook.
    pub fn send_text(&mut self, text: String, cx: &mut Context<Self>) {
        self.submit(text, cx);
    }

    pub(super) fn submit(&mut self, text: String, cx: &mut Context<Self>) {
        // Nothing leaves a replayed capture, and nothing about the person's
        // draft or their history is touched on the way to finding that out.
        if self.wire_client(cx).is_none() {
            self.restore_prompt(text, cx);
            return;
        }
        // The lease is gone: another window holds this session, so the turn
        // would never stream back. The draft goes back in the composer and
        // the notice is re-shown — it is already above the composer unless
        // it was dismissed.
        if let Some(notice) = self.lease_notice.clone() {
            self.banner = Some(notice);
            self.banner_action = None;
            self.restore_prompt(text, cx);
            return;
        }
        // The billing guard. A pay-as-you-go login bills every turn as API
        // usage, so the turn does not leave until the person has said once,
        // out loud, that they meant it. The draft goes back in the composer:
        // the banner explaining why is already above it.
        if self.tier_banner.as_ref().is_some_and(|b| b.blocking) {
            self.restore_prompt(text, cx);
            return;
        }
        self.banner = None;
        self.submitting = true;
        self.append_history(text.clone(), cx);
        let command_id = new_command_id();
        // The wire never gives the prompt back, so the fold has to remember it
        // before the command leaves: a retraction identifies the submission by
        // `commandId` and by nothing else.
        self.fold.record_command(&self.session_id, &command_id, &text);
        // Plan mode is the one thing that makes the model-visible text differ
        // from the person's: `/plan ` fires the bundled skill (see plan.rs).
        let model_text = if self.plan { plan::prefix(&text) } else { text.clone() };
        let params = TurnStartParams {
            command_id,
            session_id: self.session_id.clone(),
            input: self.parts(model_text),
            display_text: Some(text.clone()),
            reasoning_effort: self.effort.map(effort_wire),
            ..Default::default()
        };
        self.images.clear();
        self.files.clear();
        let Some(client) = self.wire_client(cx) else { return };
        let planning = self.plan;
        self.wire_call(cx, move || client.turn_start(&params), move |this, result, cx| {
            this.sent(result, text, planning, cx);
        });
        cx.notify();
    }

    /// The turn's content parts: one text part per attached file, then the
    /// prompt text, then every attached image.
    pub(super) fn parts(&self, text: String) -> Vec<TurnInputPart> {
        let mut parts = Vec::new();
        parts.extend(self.files.iter().map(attachments::AttachedFile::part));
        if !text.trim().is_empty() {
            parts.push(TurnInputPart::text(text));
        }
        parts.extend(self.images.iter().map(images::Image::part));
        if parts.is_empty() {
            parts.push(TurnInputPart::text(String::new()));
        }
        parts
    }

    /// The `turn/start` ack. Admission only — the authority for what the turn
    /// is doing is always the view event.
    pub(super) fn sent(
        &mut self,
        result: Result<TurnStartResult, MuseError>,
        text: String,
        planning: bool,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok(ack) => {
                if ack.disposition == TurnStartDisposition::Queued {
                    self.fold.record_queued(&self.session_id, &ack.turn_id, &ack.command_id, &text);
                    self.submitting = false;
                } else if planning {
                    // The plan card is appended when this turn's reply lands.
                    self.plan_turn = Some(ack.turn_id.clone());
                }
            }
            Err(error) => {
                self.submitting = false;
                // The turn never left, so the person keeps their words — and,
                // when the wire only said "not now", the banner offers to send
                // them again rather than making the person press Enter twice.
                self.report_retryable(&error, BannerAction::RetryTurn(text.clone()), cx);
                self.restore_prompt(text, cx);
            }
        }
        cx.notify();
    }

    /// ⌘↩: interject into the running turn instead of queueing behind it.
    pub fn steer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        if self.running.is_none() {
            // Nothing to steer into; the honest thing is an ordinary send.
            self.send(window, cx);
            return;
        }
        self.composer.update(cx, |state, cx| state.set_value("", window, cx));
        self.note_draft(cx);
        self.steer_text(text, cx);
    }

    pub(crate) fn steer_text(&mut self, text: String, cx: &mut Context<Self>) {
        let Some(turn_id) = self.running.as_ref().map(|r| r.turn_id.clone()) else {
            self.restore_prompt(text, cx);
            return;
        };
        let command_id = new_command_id();
        self.fold.record_command(&self.session_id, &command_id, &text);
        self.append_history(text.clone(), cx);
        let params = TurnSteerParams {
            command_id,
            session_id: self.session_id.clone(),
            expected_turn_id: turn_id,
            input: self.parts(text),
            reasoning_effort: self.effort.map(effort_wire),
        };
        self.images.clear();
        self.files.clear();
        let Some(client) = self.wire_client(cx) else { return };
        self.wire_call(cx, move || client.turn_steer(&params), |this, result, cx| {
            if let Err(error) = result {
                this.report(&error, cx);
            }
        });
        cx.notify();
    }

    /// Stop, with the retract intent paired: a turn interrupted before any
    /// output committed is durably retracted and its prompt comes back.
    pub fn interrupt(&mut self, cx: &mut Context<Self>) {
        if !self.busy() {
            return;
        }
        let params = TurnInterruptParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            retract: Some(true),
            turn_id: self.running.as_ref().map(|r| r.turn_id.clone()),
        };
        let Some(client) = self.wire_client(cx) else { return };
        self.wire_call(cx, move || client.turn_interrupt(&params), |this, result, cx| {
            if let Err(error) = result {
                this.report(&error, cx);
            }
        });
    }

    /// `turn/unqueue`, remembering why so `turn/unqueued` knows what to do with
    /// the text it hands back.
    pub(super) fn unqueue(&mut self, turn_id: &str, why: Unqueue, cx: &mut Context<Self>) {
        if self.wire_client(cx).is_none() {
            return;
        }
        self.unqueueing.insert(turn_id.to_owned(), why);
        let params = TurnUnqueueParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            turn_id: turn_id.to_owned(),
        };
        let Some(client) = self.wire_client(cx) else { return };
        let turn_id = turn_id.to_owned();
        self.wire_call(cx, move || client.turn_unqueue(&params), move |this, result, cx| {
            if let Err(error) = result {
                // The reclaim lost the race; the row stays, because only
                // the wire removes it.
                this.unqueueing.remove(&turn_id);
                this.report(&error, cx);
            }
        });
        cx.notify();
    }

    /// `session/setModel`. The chip changes on `session/modelChanged`, never
    /// here.
    pub(crate) fn set_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        let row = self.models.iter().find(|m| m.model_id == model_id);
        let params = SessionSetModelParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            model: ModelSelection {
                display_label: row.map(|r| r.display_label.clone()),
                model_id: model_id.to_owned(),
                profile_id: row.and_then(|r| r.profile_id.clone()),
                provider_id: row.map(|r| r.provider_id.clone()),
            },
        };
        let Some(client) = self.wire_client(cx) else { return };
        self.wire_call(cx, move || client.session_set_model(&params), |this, result, cx| {
            if let Err(error) = result {
                // On the echo provider this is `commandRejected:
                // unsupported_route`, and the banner saying so is correct.
                this.report(&error, cx);
            }
        });
    }

    /// `session/setApprovalMode`. The chip and the marker both come from
    /// `session/approvalModeChanged`.
    pub(super) fn set_mode(&mut self, mode: PermissionMode, cx: &mut Context<Self>) {
        let params = SessionSetApprovalModeParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            mode: wire_mode(mode),
        };
        let Some(client) = self.wire_client(cx) else { return };
        self.wire_call(cx, move || client.session_set_approval_mode(&params), |this, result, cx| {
            if let Err(error) = result {
                this.report(&error, cx);
            }
        });
    }

    /// `session/compact`. An ack of `noop` is a success, and says why.
    pub(crate) fn compact(&mut self, cx: &mut Context<Self>) {
        let params = SessionCompactParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            turn_id: None,
        };
        let Some(client) = self.wire_client(cx) else { return };
        self.wire_call(cx, move || client.session_compact(&params), |this, result, cx| match result {
            Ok(ack) if ack.status == muse_client::schema::CompactStatus::Noop => {
                let reason = ack.reason.unwrap_or_else(|| "nothing to summarize".to_owned());
                this.toast("Nothing to compact", reason, cx);
            }
            Ok(_) => {}
            Err(error) => this.report(&error, cx),
        });
    }

    /// Fetch the catalog for this session. A snapshot, on every open: MSP has
    /// no catalog subscription, so a stale list would be worse than a wait.
    pub(super) fn load_models(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        let params = ModelListParams { session_id: Some(self.session_id.clone()) };
        self.wire_call(cx, move || client.model_list(&params), |this, result, cx| {
            if let Ok(list) = result {
                this.models = list.models;
            }
            cx.notify();
        });
    }

    /// Route a failed command to its banner or its dialog (spec §3.8).
    pub(super) fn report(&mut self, error: &MuseError, cx: &mut Context<Self>) {
        let title = conn::title(error);
        match conn::severity(error) {
            Severity::Banner => self.banner = Some(format!("{title}. {error}")),
            Severity::Dialog => cx.emit(SessionEvent::Dialog { title, detail: error.to_string() }),
        }
        cx.notify();
    }

    /// The lease is gone: banner the view and refuse sends
    /// until a later resume succeeds. The banner dismisses like any other;
    /// the refusal re-shows it, so the notice outlives a dismissal everywhere
    /// except a successful resume.
    pub(crate) fn set_lease_lost(&mut self, message: &str, cx: &mut Context<Self>) {
        self.lease_notice = Some(message.to_owned());
        self.banner = Some(message.to_owned());
        self.banner_action = None;
        cx.notify();
    }

    /// A later resume succeeded: the lease is back, sends flow again, and the
    /// notice stands down. A banner some other error left since is kept —
    /// only the lease notice itself is cleared.
    pub(crate) fn note_resumed(&mut self, cx: &mut Context<Self>) {
        let Some(notice) = self.lease_notice.take() else { return };
        if self.banner.as_deref() == Some(notice.as_str()) {
            self.banner = None;
            self.banner_action = None;
        }
        cx.notify();
    }

    pub(super) fn toast(&mut self, title: impl Into<String>, body: impl Into<String>, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.toast(title, body));
        cx.notify();
    }

    /// Keep the elapsed time honest while a turn runs: 1 Hz, notifying only
    /// when the displayed second changes, so the clock costs one frame per
    /// second instead of four whole-transcript rebuilds (P2).
    pub(super) fn start_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(TICK).await;
            let alive = this.update(cx, |this, cx| {
                let secs = this.running.as_ref().map(|r| crate::clock::elapsed_since(r.started).as_secs());
                if secs != this.last_tick_secs {
                    this.last_tick_secs = secs;
                    cx.notify();
                }
                this.running.is_some()
            });
            if !matches!(alive, Ok(true)) {
                return;
            }
        }));
    }

    // -------------------------------------------------------------- plan mode

    /// Turn plan mode on or off (Shift+Tab, `/plan`, the pill's `x`).
    ///
    /// Turning it on asks the server for `denyUnmatched` and remembers what the
    /// session was in; the chip only moves when `session/approvalModeChanged`
    /// says it did.
    pub fn set_plan(&mut self, on: bool, cx: &mut Context<Self>) {
        if self.plan == on {
            return;
        }
        self.plan = on;
        if on {
            self.plan_previous_mode = Some(self.mode());
            self.set_mode(PermissionMode::DenyUnmatched, cx);
        } else if let Some(previous) = self.plan_previous_mode.take() {
            self.set_mode(previous, cx);
        }
        cx.notify();
    }

    /// The plan turn finished: append the plan card built from its reply.
    pub(super) fn plan_completed(&mut self, turn_id: &str, cx: &mut Context<Self>) {
        if self.plan_turn.as_deref() != Some(turn_id) {
            return;
        }
        self.plan_turn = None;
        let Some(reply) = self.last_assistant_text() else { return };
        let (items, sections) = plan::steps(&reply);
        if items.is_empty() {
            return;
        }
        self.plan_seq += 1;
        let id = format!("plan-{}", self.plan_seq);
        self.fold.append_client_block(
            &self.session_id,
            &id,
            Block::Plan { id: id.clone(), items, sections, state: PlanState::Proposed },
        );
        self.follow = true;
        cx.notify();
    }

    /// The sidebar's description line for this session: the first meaningful
    /// line of the newest assistant text block — blanks, code fences and
    /// markdown markers skipped — cut to the row's own width. Free — the
    /// fold is already in memory — so a completed turn writes it with no
    /// model call. The byline's result half.
    pub fn last_summary_text(&self) -> Option<String> {
        let text = self.last_assistant_text()?;
        crate::byline::excerpt_line(&text)
    }

    /// The byline's ask half: the user's newest request in this session,
    /// excerpted the free way. The newest folded user turn covers replays
    /// and restarts; the newest recorded submission covers a turn whose
    /// `userMessage` has not echoed yet.
    pub fn last_user_text(&self) -> Option<String> {
        let sent = self.fold.side(&self.session_id)?.command_text.values().next_back().cloned();
        let said = self.session()?.turns.iter().rev().find_map(|turn| match turn {
            Turn::User { text, .. } => Some(text.clone()),
            _ => None,
        });
        let text = sent.or(said)?;
        crate::byline::excerpt_line(&text)
    }

    /// The prompt behind the newest submission, first line only: the
    /// sidebar's local row is titled from this on `turn/started`, before the
    /// wire lists the session.
    ///
    /// The newest recorded submission first — at `turn/started` the server
    /// has not echoed the `userMessage` yet, but `submit` already recorded
    /// its text under a time-ordered (UUIDv7) command id — then the newest
    /// folded user turn, which covers replays and restarts that recorded
    /// nothing.
    pub fn first_prompt_text(&self) -> Option<String> {
        let sent = self.fold.side(&self.session_id)?.command_text.values().next_back().cloned();
        let said = self.session()?.turns.iter().rev().find_map(|turn| match turn {
            Turn::User { text, .. } => Some(text.clone()),
            _ => None,
        });
        let text = sent.or(said)?;
        let first = text.lines().next()?.trim();
        if first.is_empty() {
            return None;
        }
        Some(first.split_whitespace().collect::<Vec<_>>().join(" "))
    }

    /// The text of the newest assistant text block, which is the plan reply.
    pub(super) fn last_assistant_text(&self) -> Option<String> {
        let session = self.session()?;
        for turn in session.turns.iter().rev() {
            if let aui_protocol::Turn::Assistant { blocks, .. } = turn {
                for block in blocks.iter().rev() {
                    if let Block::Text { text, .. } = block {
                        if !text.trim().is_empty() {
                            return Some(text.clone());
                        }
                    }
                }
            }
        }
        None
    }

    /// Accept / Refine / Reject on a plan card (spec §3.1).
    pub(super) fn plan_action(&mut self, id: &str, action: PlanAction, window: &mut Window, cx: &mut Context<Self>) {
        let state = match action {
            PlanAction::Accept => PlanState::Accepted,
            PlanAction::Reject => PlanState::Rejected,
            PlanAction::Refine => PlanState::Proposed,
        };
        if action != PlanAction::Refine {
            if let Some(block) = self.plan_block(id) {
                let Block::Plan { items, sections, .. } = &block else { return };
                let replaced =
                    Block::Plan { id: id.to_owned(), items: items.clone(), sections: sections.clone(), state };
                self.fold.replace_client_block(&self.session_id, id, replaced);
                self.follow = true;
            }
        }
        match action {
            PlanAction::Accept => {
                self.set_plan(false, cx);
                self.submit(plan::ACCEPT_PROMPT.to_owned(), cx);
            }
            PlanAction::Reject => self.set_plan(false, cx),
            PlanAction::Refine => self.focus_composer(window, cx),
        }
        cx.notify();
    }

    pub(super) fn plan_block(&self, id: &str) -> Option<Block> {
        let session = self.session()?;
        session.turns.iter().find_map(|turn| match turn {
            aui_protocol::Turn::Assistant { id: turn_id, blocks, .. } if turn_id == id => blocks.first().cloned(),
            _ => None,
        })
    }
}
