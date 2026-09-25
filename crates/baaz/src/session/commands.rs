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
        // The capability gate, before any wire: on a provider where
        // steering is `Unavailable` the seam would refuse the command, so
        // the control must not invite the press — the words stay and the
        // banner carries the provider's own reason. `Unverified` stays
        // attemptable (the seam never refuses it); the strip above the
        // composer says it is unverified.
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::SteerTurn)
            .allows_attempt()
        {
            let reason = self.steer_gate().unwrap_or_else(|| "Steering is not available on this provider.".into());
            self.banner = Some(reason);
            self.banner_action = None;
            self.restore_prompt(text, cx);
            return;
        }
        let Some(turn_id) = self.running.as_ref().map(|r| r.turn_id.clone()) else {
            // The turn ended while the reclaim was in flight: the steer has
            // nowhere to go, so the words go back where they came from — with
            // the reason, so a message that reappears does not read as sent.
            self.banner = Some(
                "The turn ended before the message could be steered — kept it in the composer."
                    .to_owned(),
            );
            self.banner_action = None;
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
            input: self.parts(text.clone()),
            reasoning_effort: self.effort.map(effort_wire),
        };
        self.images.clear();
        self.files.clear();
        let Some(client) = self.wire_client(cx) else {
            // No wire to steer on (a replayed capture — and `wire_client`
            // already said so above the composer): keep the words, not just
            // the notice.
            self.restore_prompt(text, cx);
            return;
        };
        self.wire_call(cx, move || client.turn_steer(&params), move |this, result, cx| {
            if let Err(error) = result {
                // The steer never landed (the turn ended mid-flight, or the
                // server refused it): the banner says why, and the words go
                // back in the composer instead of evaporating.
                this.report(&error, cx);
                this.restore_prompt(text, cx);
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
        // Same gate as steering: an `Unavailable` stop button must say why
        // instead of sending a command the seam refuses.
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::TurnControl)
            .allows_attempt()
        {
            let reason = self.turn_gate().unwrap_or_else(|| "Stopping is not available on this provider.".into());
            self.banner = Some(reason);
            self.banner_action = None;
            cx.notify();
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
                // `commandRejected` / `already_terminal` is not a failure to
                // report: the server is saying the turn this asked to stop
                // had already finished. That is a better answer about the
                // turn than the local state, which still believes it is
                // running — so take it, and let the window settle.
                //
                // Without this the only thing a person can do about a turn
                // the server has forgotten is press Stop, and Stop answers
                // with a red banner and leaves the composer counting. The
                // turn seen behind this was "running" for twelve hours.
                if interrupt_found_turn_over(&error) {
                    crate::baaz_log!("interrupt: the turn was already over; settling the view");
                    this.clear_running();
                    this.submitting = false;
                    cx.notify();
                    return;
                }
                this.report(&error, cx);
            }
        });
    }

    /// `turn/unqueue`, remembering why — and the row's text, captured now —
    /// so `turn/unqueued` needs no fold echo to know what was reclaimed.
    pub(super) fn unqueue(&mut self, turn_id: &str, why: Unqueue, text: String, cx: &mut Context<Self>) {
        if self.wire_client(cx).is_none() {
            return;
        }
        self.unqueueing.insert(turn_id.to_owned(), PendingUnqueue { kind: why, text });
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

    /// Switch the session's model. On the muse lane that is the existing
    /// `session/setModel` path, and the chip moves only on
    /// `session/modelChanged`, never here. On the provider lane (Claude
    /// Code / Codex) the pick travels as a neutral `SelectModel` on the
    /// outbox the lane drains — never past the seam to the muse wire —
    /// and the chip reads the recorded pick at once, since no echo flows
    /// back into this fold for those sessions. Both lanes apply to a
    /// session with turns in it exactly as to a fresh one: model is
    /// switchable, provider is not, and nothing here counts turns.
    pub(crate) fn set_model(&mut self, model_id: &str, cx: &mut Context<Self>) {
        if !crate::providers::uses_legacy_pump(self.provider_kind()) {
            self.external_outbox.push(ProviderCommand::SelectModel {
                request_id: new_command_id(),
                session_id: self.session_id.clone(),
                model: model_id.to_owned(),
                model_provider: None,
            });
            self.apply_model_selected(model_id, cx);
            return;
        }
        let model = self.model_selection_for(model_id);
        let params = SessionSetModelParams {
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            model,
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

    /// The `session/setModel` selection for `model_id`, from the catalog
    /// row when one names it. A bare id still selects (aliases travel);
    /// only the presentation comes from the row.
    fn model_selection_for(&self, model_id: &str) -> ModelSelection {
        let row = self.models.iter().find(|m| m.model_id == model_id);
        ModelSelection {
            display_label: row.map(|r| r.display_label.clone()),
            model_id: model_id.to_owned(),
            profile_id: row.and_then(|r| r.profile_id.clone()),
            provider_id: row.map(|r| r.provider_id.clone()),
        }
    }

    /// Fold a neutral model catalog into the picker: human labels shown,
    /// wire ids kept, the current model marked. An empty answer is a
    /// failure with its reason, never an empty menu: the picker renders
    /// the reason row instead of nothing.
    pub(super) fn apply_model_catalog(
        &mut self,
        rows: Vec<provider::ModelSummary>,
        provider_id: &str,
        cx: &mut Context<Self>,
    ) {
        if rows.is_empty() {
            self.models = Vec::new();
            self.models_error = Some("the model catalog answered with no usable model".to_owned());
            cx.notify();
            return;
        }
        self.models = rows
            .into_iter()
            .map(|row| ModelCatalogEntry {
                context_limit: None,
                cost: None,
                description: None,
                display_label: row.label,
                is_active: row.active,
                is_default: false,
                model_id: row.id,
                output_limit: None,
                profile_id: None,
                provider_id: provider_id.to_owned(),
                release_date: None,
            })
            .collect();
        self.models_error = None;
        cx.notify();
    }

    /// Record a model pick on the provider lane: the matching row marks
    /// active and the chip reads the pick at once. A bare id still
    /// applies (aliases travel) without marking a row it does not name.
    pub(super) fn apply_model_selected(&mut self, model_id: &str, cx: &mut Context<Self>) {
        for row in &mut self.models {
            row.is_active = row.model_id == model_id;
        }
        self.pending_model = Some(model_id.to_owned());
        cx.notify();
    }

    /// Fold a Codex `model/list` answer into the per-model effort map: each
    /// catalog row's `supportedReasoningEfforts`, parsed by the adapter
    /// that owns the shape. The effort menu reads the selected model's
    /// entry, so changing the model changes the levels with no second
    /// fetch — a snapshot, like [`SessionView::models`].
    ///
    /// No production caller yet: the lane feeds this the moment it lands,
    /// the way the model lane drains its outbox today.
    #[allow(dead_code)]
    pub(super) fn apply_codex_catalog(&mut self, result: &serde_json::Value, cx: &mut Context<Self>) {
        let mut efforts = std::collections::HashMap::new();
        for row in provider_codex::child::model_catalog(result) {
            efforts.insert(
                row.id.clone(),
                provider_codex::child::supported_efforts(result, &row.id),
            );
        }
        self.codex_efforts = efforts;
        cx.notify();
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
    ///
    /// Per lane: muse lists over its own wire; Claude Code folds Baaz's
    /// supplied alias list (the `Emulated` cell — no fixture enumerates
    /// them, so Baaz owns them); Codex answers `model/list` from its
    /// session child, which this view does not hold, so the picker says
    /// why instead of opening empty. Whatever cannot answer explains
    /// itself in the picker's typed-reason row — never a dead click and
    /// never a silently empty menu.
    pub(super) fn load_models(&mut self, cx: &mut Context<Self>) {
        match self.provider_kind() {
            ProviderId::ClaudeCode => {
                let current = self.model().to_string();
                let rows = crate::providers::claude_code_catalog(Some(current.as_str()));
                let provider = self.provider_id.clone();
                self.apply_model_catalog(rows, &provider, cx);
            }
            ProviderId::Codex => {
                self.models_error = Some(
                    "Codex lists models from its own session child, and this view holds none: \
                     reopen the session on its lane to list them"
                        .to_owned(),
                );
                cx.notify();
            }
            ProviderId::Muse => {
                let Some(client) = self.wire_client(cx) else { return };
                let params = ModelListParams { session_id: Some(self.session_id.clone()) };
                self.wire_call(
                    cx,
                    move || client.model_list(&params),
                    |this, result, cx| match result {
                        Ok(list) => {
                            this.models = list.models;
                            this.models_error = None;
                            cx.notify();
                        }
                        Err(error) => {
                            this.models_error = Some(error.to_string());
                            this.report(&error, cx);
                        }
                    },
                );
            }
        }
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

/// Whether a failed `turn/interrupt` means the turn had already finished.
///
/// `commandRejected` with reason `already_terminal` is the server declining
/// to stop something that is already stopped — an answer about the turn, not
/// an error about the request. Branching on the kind and the reason rather
/// than the message is the wire contract (research §1.14); the message for
/// this case reads `turn/interrupt command <id> rejected: already_terminal`
/// and is explicitly not a branch point.
pub(crate) fn interrupt_found_turn_over(error: &MuseError) -> bool {
    matches!(error.kind(), Some(ErrorKind::CommandRejected)) && error.reason() == Some("already_terminal")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Open a view on one provider lane: no child, no wire — the catalog
    /// and the pick are what these tests drive, offline throughout.
    fn lane_view(
        vc: &mut gpui::VisualTestContext,
        provider_id: &str,
        session_id: &str,
    ) -> Entity<SessionView> {
        let provider_id = provider_id.to_owned();
        let session_id = session_id.to_owned();
        vc.update(|window, cx| {
            let host = SessionHost {
                provider_id,
                workspace: "/tmp/p3-model-picker".to_owned(),
                overlays: cx.new(|_| Overlays::default()),
                capture: CaptureToken::default(),
            };
            cx.new(|cx| SessionView::new(session_id, None, host, window, cx))
        })
    }

    /// The real `model/list` response out of `fixtures/codex/basic.jsonl`,
    /// mapped the way provider-codex's `ListModels` maps it: `displayName`
    /// into the seam's `label`, the id kept for the wire.
    fn codex_fixture_catalog() -> Vec<provider::ModelSummary> {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/codex/basic.jsonl"
        ))
        .expect("codex fixture reads");
        let mut result = None;
        for line in text.lines() {
            let frame: serde_json::Value = serde_json::from_str(line).expect("fixture decodes");
            let is_answer = frame.get("_dir").and_then(serde_json::Value::as_str)
                == Some("server->client")
                && frame.get("frame").and_then(|frame| frame.get("id"))
                    == Some(&serde_json::json!(10));
            if is_answer {
                result = frame.get("frame").and_then(|frame| frame.get("result")).cloned();
            }
        }
        let result = result.expect("model/list response id 10");
        result
            .get("data")
            .and_then(serde_json::Value::as_array)
            .expect("the key is data, not models")
            .iter()
            .map(|row| {
                let id = row.get("id").and_then(serde_json::Value::as_str).expect("row id");
                provider::ModelSummary {
                    id: id.to_owned(),
                    label: row
                        .get("displayName")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or(id)
                        .to_owned(),
                    active: id == "gpt-5.6-sol",
                }
            })
            .collect()
    }

    /// P3, codex: the catalog folds into `self.models` from the real
    /// fixture, human labels shown, wire ids kept, the current one marked.
    #[gpui::test]
    fn codex_catalog_from_the_fixture_folds_with_display_names(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let rows = codex_fixture_catalog();
        assert_eq!(rows.len(), 4, "the fixture lists four models");
        let view = lane_view(vc, "codex", "s-codex");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.apply_model_catalog(rows, "openai", cx));
            view.update(cx, |view, _| {
                assert!(view.models_error.is_none());
                let ids: Vec<&str> =
                    view.models.iter().map(|m| m.model_id.as_str()).collect();
                assert_eq!(ids, ["gpt-5.6-sol", "gpt-5.6-terra", "gpt-5.6-luna", "gpt-5.5"]);
                let labels: Vec<&str> =
                    view.models.iter().map(|m| m.display_label.as_str()).collect();
                assert_eq!(labels, ["GPT-5.6-Sol", "GPT-5.6-Terra", "GPT-5.6-Luna", "GPT-5.5"]);
                assert!(
                    view.models.iter().all(|m| m.display_label != m.model_id),
                    "no raw slug where a display name exists"
                );
                let active: Vec<&str> = view
                    .models
                    .iter()
                    .filter(|m| m.is_active)
                    .map(|m| m.model_id.as_str())
                    .collect();
                assert_eq!(active, ["gpt-5.6-sol"], "the current one is marked");
            });
        });
    }

    /// P3, claude-code: the picker lists Baaz's supplied aliases with
    /// human labels, and the chip follows the pick on the lane.
    #[gpui::test]
    fn claude_code_supplied_list_folds_with_current_marked(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let supplied = crate::providers::claude_code_models();
        assert_eq!(
            supplied.map(|row| row.id),
            ["sonnet", "opus", "haiku"],
            "Baaz owns this list: aliases --model accepts"
        );
        assert!(
            supplied.iter().all(|row| row.label != row.id),
            "the raw alias never renders"
        );
        let view = lane_view(vc, "claude-code", "s-claude");
        vc.update(|_, cx| {
            // Opening the chip folds the supplied list; nothing marks yet
            // because the session names no supplied alias.
            view.update(cx, |view, cx| view.load_models(cx));
            view.update(cx, |view, _| {
                assert_eq!(view.models.len(), 3);
                assert!(view.models_error.is_none());
                assert!(view.models.iter().all(|m| !m.is_active));
            });
            // The pick routes through the seam (outbox `SelectModel`) and
            // the chip updates at once.
            view.update(cx, |view, cx| view.set_model("opus", cx));
            view.update(cx, |view, cx| {
                assert_eq!(view.model().as_ref(), "opus");
                assert!(view.take_external_outbox().iter().any(|command| matches!(
                    command,
                    ProviderCommand::SelectModel { model, .. } if model == "opus"
                )));
                // Reopening the chip marks the pick.
                view.load_models(cx);
            });
            view.update(cx, |view, _| {
                let active: Vec<&str> = view
                    .models
                    .iter()
                    .filter(|m| m.is_active)
                    .map(|m| m.model_id.as_str())
                    .collect();
                assert_eq!(active, ["opus"]);
            });
        });
    }

    /// P3, muse: the `session/setModel` selection still carries the
    /// catalog row's presentation, and a bare id still selects.
    #[gpui::test]
    fn muse_selection_carries_the_catalog_row(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "muse", "s-muse");
        vc.update(|_, cx| {
            view.update(cx, |view, _| {
                view.models = vec![ModelCatalogEntry {
                    context_limit: None,
                    cost: None,
                    description: Some("The everyday model".into()),
                    display_label: "Muse Everyday".into(),
                    is_active: true,
                    is_default: true,
                    model_id: "muse-everyday".into(),
                    output_limit: None,
                    profile_id: Some("p-1".into()),
                    provider_id: "meta".into(),
                    release_date: None,
                }];
                let selection = view.model_selection_for("muse-everyday");
                assert_eq!(selection.model_id, "muse-everyday");
                assert_eq!(selection.display_label.as_deref(), Some("Muse Everyday"));
                assert_eq!(selection.profile_id.as_deref(), Some("p-1"));
                assert_eq!(selection.provider_id.as_deref(), Some("meta"));
                // A bare id still selects; only the presentation is absent.
                let bare = view.model_selection_for("unlisted-alias");
                assert_eq!(bare.model_id, "unlisted-alias");
                assert_eq!(bare.display_label, None);
            });
        });
    }

    /// P3, existing session: a session with turns in it changes model —
    /// the pick applies, the chip updates, and the lane never moves.
    #[gpui::test]
    fn a_session_with_turns_changes_model_and_the_chip_updates(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "codex", "s-turns");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                // A transcript already in the session, not a fresh one.
                view.fold.append_client_block(
                    "s-turns",
                    "t-seed",
                    Block::Text { text: "earlier work".into(), streaming: false },
                );
                assert!(view.has_turns(), "the session has turns in it");
                view.apply_model_catalog(codex_fixture_catalog(), "openai", cx);
            });
            // The pick routes through the seam and the chip updates, with
            // turns in the session exactly as without.
            view.update(cx, |view, cx| view.set_model("gpt-5.6-terra", cx));
            view.update(cx, |view, _| {
                assert_eq!(view.model().as_ref(), "gpt-5.6-terra");
                let active: Vec<&str> = view
                    .models
                    .iter()
                    .filter(|m| m.is_active)
                    .map(|m| m.model_id.as_str())
                    .collect();
                assert_eq!(active, ["gpt-5.6-terra"]);
                assert_eq!(view.provider_kind(), ProviderId::Codex, "the lane never moves");
                let outbox = view.take_external_outbox();
                assert_eq!(outbox.len(), 1, "one seam command, never past it");
                assert!(
                    matches!(
                        &outbox[0],
                        ProviderCommand::SelectModel { session_id, model, .. }
                        if session_id == "s-turns" && model == "gpt-5.6-terra"
                    ),
                    "the pick travels as SelectModel: {outbox:?}"
                );
            });
        });
    }

    /// The raw `model/list` answer behind [`codex_fixture_catalog`]: the
    /// `result` of the id-10 frame in `fixtures/codex/basic.jsonl`, so the
    /// effort fold below reads the same bytes the adapter parses.
    fn codex_fixture_result() -> serde_json::Value {
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/codex/basic.jsonl"
        ))
        .expect("codex fixture reads");
        let mut result = None;
        for line in text.lines() {
            let frame: serde_json::Value = serde_json::from_str(line).expect("fixture decodes");
            let is_answer = frame.get("_dir").and_then(serde_json::Value::as_str)
                == Some("server->client")
                && frame.get("frame").and_then(|frame| frame.get("id"))
                    == Some(&serde_json::json!(10));
            if is_answer {
                result = frame.get("frame").and_then(|frame| frame.get("result")).cloned();
            }
        }
        result.expect("model/list response id 10")
    }

    /// P4, codex: the effort list for `gpt-5.6-sol` comes from the fixture's
    /// `supportedReasoningEfforts` — with the provider's own descriptions —
    /// and selecting a different model changes the list. That second half
    /// is the per-model proof: a hardcoded menu cannot pass it.
    #[gpui::test]
    fn codex_effort_list_comes_from_the_fixture_and_follows_the_model(cx: &mut gpui::TestAppContext) {
        use crate::overlays::{EffortOptions, effort_label, effort_row_id};
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "codex", "s-effort");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply_model_catalog(codex_fixture_catalog(), "openai", cx);
                view.apply_codex_catalog(&codex_fixture_result(), cx);
            });
            view.update(cx, |view, _| {
                let EffortOptions::Available(options) = view.effort_options() else {
                    panic!("sol names a catalog row with levels");
                };
                let ids: Vec<String> = options.iter().map(|o| effort_row_id(o.effort)).collect();
                assert_eq!(ids, ["default", "Low", "Medium", "High", "Xhigh", "Max", "Ultra"]);
                let low = options.iter().find(|o| effort_row_id(o.effort) == "Low").expect("low");
                assert_eq!(
                    low.detail, "Fast responses with lighter reasoning",
                    "the detail line is the provider's text, not ours"
                );
                assert_eq!(effort_label(low.effort), "Low");
            });
            // Selecting a different model changes the list: gpt-5.5 stops
            // at xhigh, where sol reaches ultra.
            view.update(cx, |view, cx| view.set_model("gpt-5.5", cx));
            view.update(cx, |view, _| {
                let EffortOptions::Available(options) = view.effort_options() else {
                    panic!("gpt-5.5 names a catalog row with levels");
                };
                let ids: Vec<String> = options.iter().map(|o| effort_row_id(o.effort)).collect();
                assert_eq!(ids, ["default", "Low", "Medium", "High", "Xhigh"]);
                assert!(
                    !ids.iter().any(|id| id == "Ultra" || id == "Max"),
                    "the old model's levels are gone: {ids:?}"
                );
            });
            // The open menu counts the new list, not the old constant.
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Effort, cx));
            let rows = view.read(cx).menu_rows(cx);
            assert_eq!(rows, 5, "the menu follows the new model, not the old constant");
        });
    }

    /// P4, claude-code: no reasoning control is evidenced anywhere on the
    /// lane, so the picker renders the adapter's reason — never an empty
    /// menu, never a dead click.
    #[gpui::test]
    fn a_provider_without_reasoning_support_renders_a_reason(cx: &mut gpui::TestAppContext) {
        use crate::overlays::EffortOptions;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "claude-code", "s-noeffort");
        vc.update(|window, cx| {
            view.update(cx, |view, _| {
                let EffortOptions::Unavailable(reason) = view.effort_options() else {
                    panic!("claude-code evidences no effort control");
                };
                assert!(!reason.is_empty(), "a stated reason, not silence");
            });
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Effort, cx));
            let rows = view.read(cx).menu_rows(cx);
            assert_eq!(rows, 1, "the reason row, never zero rows");
            view.update(cx, |view, cx| view.confirm_menu(window, cx));
            view.update(cx, |view, cx| {
                assert!(view.overlays.read(cx).menu.is_none(), "the row acted and closed");
            });
        });
    }

    /// P4, existing session: a session with turns in it changes effort —
    /// the pick applies and the chip reads it at once, exactly as on a
    /// fresh session.
    #[gpui::test]
    fn an_existing_session_changes_effort_and_the_chip_updates(cx: &mut gpui::TestAppContext) {
        use crate::overlays::effort_label;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "codex", "s-effort-turns");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                // A transcript already in the session, not a fresh one.
                view.fold.append_client_block(
                    "s-effort-turns",
                    "t-seed",
                    Block::Text { text: "earlier work".into(), streaming: false },
                );
                assert!(view.has_turns(), "the session has turns in it");
                view.apply_model_catalog(codex_fixture_catalog(), "openai", cx);
                view.apply_codex_catalog(&codex_fixture_result(), cx);
            });
            view.update(cx, |view, cx| {
                view.pick_effort(Some(aui_protocol::ReasoningEffort::High), cx)
            });
            view.update(cx, |view, _| {
                assert_eq!(view.effort, Some(aui_protocol::ReasoningEffort::High));
                assert_eq!(effort_label(view.effort), "High", "the chip follows the pick");
            });
            // The open menu highlights the picked row.
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Effort, cx));
            view.update(cx, |view, cx| {
                use crate::overlays::EffortOptions;
                let selected = view.overlays.read(cx).menu.as_ref().map(|m| m.selected);
                let EffortOptions::Available(options) = view.effort_options() else {
                    panic!("the picked row lists while turns are in");
                };
                let position = options.iter().position(|o| o.effort == view.effort);
                assert_eq!(selected, position, "the highlight sits on the pick");
            });
        });
    }

    /// P4, muse: the lane that always worked keeps its full menu —
    /// `Default` plus the whole closed enum, with today's detail lines.
    #[gpui::test]
    fn muse_keeps_the_full_menu(cx: &mut gpui::TestAppContext) {
        use crate::overlays::{EffortOptions, effort_row_id};
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "muse", "s-muse-effort");
        vc.update(|_, cx| {
            view.update(cx, |view, _| {
                let EffortOptions::Available(options) = view.effort_options() else {
                    panic!("the muse lane always offers effort");
                };
                let ids: Vec<String> = options.iter().map(|o| effort_row_id(o.effort)).collect();
                assert_eq!(
                    ids,
                    [
                        "default", "None", "Minimal", "Low", "Medium", "High", "Xhigh", "Max",
                        "Ultra"
                    ]
                );
                let ultra = options.iter().find(|o| effort_row_id(o.effort) == "Ultra").expect("ultra");
                assert_eq!(ultra.detail, "The largest budget MSP accepts.");
            });
        });
    }

    /// P3, unavailable catalog: the chip's menu explains itself in the
    /// typed-reason row — never a dead click, never a silent empty menu.
    #[gpui::test]
    fn an_unanswered_catalog_explains_itself(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "codex", "s-nolane");
        vc.update(|window, cx| {
            // Opening the chip cannot list: no live lane, so a reason.
            view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx));
            view.update(cx, |view, _| {
                assert!(view.models.is_empty());
                assert!(
                    view.models_error.as_ref().is_some_and(|reason| !reason.is_empty()),
                    "the chip says why"
                );
            });
            // The menu still counts one row — the reason — and Enter on it
            // restates the reason and closes, rather than going dead.
            let rows = view.read(cx).menu_rows(cx);
            assert_eq!(rows, 1, "the reason row, never zero rows");
            view.update(cx, |view, cx| view.confirm_menu(window, cx));
            view.update(cx, |view, cx| {
                assert!(view.overlays.read(cx).menu.is_none(), "the row acted and closed");
            });
        });
    }

    fn rpc(code: i64, message: &str, data: serde_json::Value) -> MuseError {
        let object: muse_client::schema::ErrorObject =
            serde_json::from_value(serde_json::json!({"code": code, "message": message, "data": data}))
                .expect("error object decodes");
        MuseError::Rpc(Box::new(object))
    }

    /// P6: `setprovider:<id>` is the provider picker's menu row as a
    /// verb — the same `pick_provider` the click and Enter paths call.
    /// A fresh session emits the swap; a session with turns keeps its
    /// lane (the verb still routes through the menu, so a new session
    /// starts on the pick) and fails loudly saying so; an unknown id
    /// fails loudly and changes nothing. No child spawns anywhere:
    /// the pick and the failure accounting are what this drives,
    /// offline throughout.
    #[gpui::test]
    fn setprovider_verbs_route_through_the_menu_path(cx: &mut gpui::TestAppContext) {
        use std::cell::RefCell;
        use std::rc::Rc;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = lane_view(vc, "muse", "s-setprovider");
        // The picks the verb asks for, as the application would see
        // them. gpui activates the listener on defer, so subscribe in
        // one update and emit in the next.
        let swaps: Rc<RefCell<Vec<ProviderId>>> = Rc::default();
        let new_on: Rc<RefCell<Vec<ProviderId>>> = Rc::default();
        let sub = vc.update(|_, cx| {
            let (swaps, new_on) = (swaps.clone(), new_on.clone());
            cx.subscribe(&view, move |_, event: &SessionEvent, _| match event {
                SessionEvent::SwitchProvider { provider } => swaps.borrow_mut().push(*provider),
                SessionEvent::NewSessionOnProvider { provider } => new_on.borrow_mut().push(*provider),
                _ => {}
            })
        });
        // A fresh session on Muse moves to Claude Code.
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                crate::steps::session_step(view, "setprovider:claude-code", window, cx)
            });
        });
        vc.update(|_, cx| {
            assert_eq!(*swaps.borrow(), [ProviderId::ClaudeCode]);
            assert!(new_on.borrow().is_empty(), "no new-session detour on a fresh session");
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Muse, "the view never swaps lanes live");
        });
        // Picking the session's own lane is a no-op success, like its
        // menu row — a setter, not a toggle — and an unknown id fails
        // loudly instead of falling back to Muse for a typo.
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                crate::steps::session_step(view, "setprovider:muse", window, cx);
                crate::steps::session_step(view, "setprovider:not-a-lane", window, cx)
            });
        });
        assert!(
            crate::steps::step_failure_names().iter().any(|name| name == "setprovider:not-a-lane"),
            "an unknown provider id is a named failure, not a silent no-op"
        );
        vc.update(|_, cx| {
            assert_eq!(swaps.borrow().len(), 1, "the unknown id emitted nothing");
            assert!(new_on.borrow().is_empty());
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Muse);
        });
        // A session with turns keeps its lane: the verb behaves exactly
        // as the menu does (a new session starts on the pick) and says
        // so as a failure rather than reading as an in-place swap.
        vc.update(|_, cx| {
            view.update(cx, |view, _cx| {
                view.fold.append_client_block(
                    "s-setprovider",
                    "t-seed",
                    Block::Text { text: "earlier work".into(), streaming: false },
                );
            });
            assert!(view.read(cx).has_turns(), "the session has turns in it");
        });
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                crate::steps::session_step(view, "setprovider:codex", window, cx)
            });
        });
        assert_eq!(*new_on.borrow(), [ProviderId::Codex], "the menu's new-session path, verbatim");
        assert!(
            crate::steps::step_failure_names().iter().any(|name| name == "setprovider:codex"),
            "the detour is a named failure, never a silent non-swap"
        );
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Muse, "the live session never changes lanes");
        });
        // `seteffort:` rides the same shape: a known level applies
        // through the menu's own pick, an unknown spelling fails loudly.
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                crate::steps::session_step(view, "seteffort:high", window, cx);
                crate::steps::session_step(view, "seteffort:sideways", window, cx);
            });
        });
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).effort, Some(aui_protocol::ReasoningEffort::High));
            assert!(
                crate::steps::step_failure_names().iter().any(|name| name == "seteffort:sideways"),
                "an unknown effort level is a named failure"
            );
        });
        drop(sub);
    }

    /// The case that stranded a session for twelve hours: Stop is the only
    /// way out of a turn the window thinks is running, and the server
    /// answers that there is nothing to stop.
    #[test]
    fn an_already_terminal_rejection_means_the_turn_is_over() {
        let error = rpc(
            -32030,
            "turn/interrupt command 01a0be2a-99f3-7640-a50c-6f5e8b0fa152 rejected: already_terminal",
            serde_json::json!({"kind": "commandRejected", "reason": "already_terminal"}),
        );
        assert!(interrupt_found_turn_over(&error));
    }

    /// Every other rejection still reports: `not_paired_interrupt` means the
    /// request was malformed, which is worth a banner.
    #[test]
    fn other_rejections_are_still_errors() {
        let other = rpc(
            -32030,
            "rejected: not_paired_interrupt",
            serde_json::json!({"kind": "commandRejected", "reason": "not_paired_interrupt"}),
        );
        assert!(!interrupt_found_turn_over(&other));

        let no_reason = rpc(-32030, "rejected", serde_json::json!({"kind": "commandRejected"}));
        assert!(!interrupt_found_turn_over(&no_reason));

        // The reason alone is not enough: it has to be a rejection.
        let wrong_kind = rpc(
            -32603,
            "internal",
            serde_json::json!({"kind": "internal", "reason": "already_terminal"}),
        );
        assert!(!interrupt_found_turn_over(&wrong_kind));

        // And a transport failure carries no kind at all.
        assert!(!interrupt_found_turn_over(&MuseError::Closed));
    }
}
