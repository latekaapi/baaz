//! Approvals: the decision a server-minted choice makes, the full
//! output a settled shell card can then fetch, and the retry a failed turn
//! offers.
//!
//! Part of [`SessionRuntime`] (the model half: what it does to the
//! session) and [`SessionView`] (the drawing half: fields, menus, focus);
//! see [`crate::session`] for what each entity owns and why these are
//! their own files.

use super::*;
use crate::providers::ExternalApproval;

impl SessionRuntime {
    /// A server-minted choice was pressed: `approval/decide`.
    ///
    /// The `requirementId` is the guard MSP requires: it must equal the
    /// approval's **current** stage token, or the wire answers
    /// `approvalRequirementStale`. It is read from the pending request the fold
    /// keeps rather than from the card, because the block has no room for it and
    /// the choices change between stages.
    pub fn decide_approval(&mut self, approval_id: String, choice_id: String, feedback: Option<String>, cx: &mut Context<Self>) {
        // A provider lane has no muse wire: the inline card's choice
        // travels as the adapter's own `DecideApproval`, carrying the
        // card's choice id verbatim (each adapter answers its own
        // choices). The card stays pending — only the server's
        // resolution moves it, never this send.
        self.decide_provider_approval(approval_id, choice_id, feedback, cx);
    }
}

impl SessionView {
    /// Open (or close) the feedback field a choice asks for.
    pub fn toggle_feedback(&mut self, approval_id: String, choice_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback_open = choice_id.map(|choice| (approval_id, choice));
        self.feedback.update(cx, |state, cx| state.set_value("", window, cx));
        if let Some((open, _)) = &self.feedback_open {
            window.focus(&self.feedback.focus_handle(cx), cx);
            // The field grows the card by its height and its send row. The
            // newest card sits at the transcript's end, under the floating
            // band: anchor the list at its end, whose tail clears the band
            // and the stack, so the grown card shows whole above them
            // rather than its buttons sliding under the composer (live
            // 2026-10-17).
            if self.newest_pending_approval().is_some_and(|(newest, _)| newest == open) {
                self.follow = true;
                self.list_state.scroll_to_end();
            }
        }
        cx.notify();
    }

    /// Enter in an open feedback or clarification field; `false` when neither is
    /// open, so the caller can let the key mean what it usually means.
    pub fn confirm_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some((approval_id, choice_id)) = self.feedback_open.clone() {
            let text = self.feedback.read(cx).value().to_string();
            let feedback = (!text.trim().is_empty()).then_some(text);
            self.runtime.update(cx, |rt, cx| rt.decide_approval(approval_id, choice_id, feedback, cx));
            // The field is gone with the decision; the keyboard goes back
            // to the composer rather than to a field no longer drawn.
            self.focus_composer(window, cx);
            return true;
        }
        if let Some(block_id) = self.clarify_open.clone() {
            self.send_clarification(&block_id, window, cx);
            return true;
        }
        false
    }

    /// Escape: close an open card field, and say whether it closed one.
    pub fn close_card_field(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.feedback_open.take().is_some() || self.clarify_open.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }
}

impl SessionRuntime {
    // ---------------------------------------------------------- full output

    /// "Show full output" on a truncated tool card: page `item/readOutput` on
    /// a background task (02-app §3 — a page can block) and replace the card's
    /// body on the server's result (D4). A second press while pages are still
    /// arriving does nothing; a failed fetch reports its banner and leaves the
    /// truncated body alone.
    pub fn show_full_output(&mut self, block_id: String, cx: &mut Context<Self>) {
        // A lane reads the whole stored output with one command: the
        // provider pages, decodes and caps it.
        let Some(output) = self.store.side().stored_outputs.get(&block_id).cloned() else { return };
        if matches!(self.full_outputs.get(&block_id), Some(full_output::Fetch::Fetching)) {
            return;
        }
        self.full_outputs.insert(block_id.clone(), full_output::Fetch::Fetching);
        cx.emit(RuntimeEvent::CardsChanged);
        let command = ProviderCommand::ReadStoredOutput {
            session_id: self.session_id.clone(),
            item: block_id.clone(),
            output,
            offset: 0,
            length: None,
        };
        self.provider_send(command, cx, move |this, result, cx| {
            match result {
                Ok(provider::Ack::StoredOutput { content, complete }) => {
                    let lines = full_output::split_output_lines(&content);
                    this.full_outputs.insert(block_id, full_output::Fetch::Ready { lines, capped: !complete });
                }
                Ok(_) => {
                    this.full_outputs.remove(&block_id);
                }
                Err(error) => {
                    this.full_outputs.remove(&block_id);
                    this.report_provider_error(&error, cx);
                }
            }
            cx.emit(RuntimeEvent::CardsChanged);
        });
    }

    // ----------------------------------------------------------------- retry

    /// "Retry" on an error card: resend the failed turn's own input.
    ///
    /// The wire never gives a prompt back, so this only works where the app
    /// remembered it — which is every turn it sent itself. A turn that arrived
    /// through a backfill has no text here, and the card hides the button.
    pub fn retry_turn(&mut self, turn_id: String, cx: &mut Context<Self>) {
        let Some(text) = self.remembered_text(&turn_id) else { return };
        match text.strip_prefix('!') {
            Some(command) => self.run_user_shell(command.to_owned(), cx),
            None => self.submit(text, cx),
        }
    }

    /// What a turn was sent with, if this app sent it.
    ///
    /// A fresh `turn/start`'s `commandId` **equals** its `turnId`, which is what
    /// makes the command-text map a turn-text map for free.
    pub(super) fn remembered_text(&self, turn_id: &str) -> Option<String> {
        self.store.command_text(turn_id).map(str::to_owned)
    }

    /// Which failed turns the retry button may be offered on.
    pub(super) fn retryable_turns(&self) -> HashSet<String> {
        self.store.command_ids().map(str::to_owned).collect()
    }

    /// Banner `message` with the retry `action`; with `auto`, press it once
    /// on its own after the backoff unless the banner moved meanwhile — "not
    /// now" deserves one unattended attempt before a person's attention.
    pub(super) fn offer_retry(&mut self, message: &str, action: BannerAction, auto: bool, cx: &mut Context<Self>) {
        self.set_banner(message, Some(action.clone()), cx);
        if !auto {
            return;
        }
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(RETRY_BACKOFF).await;
            let _ = this.update(cx, |this, cx| {
                // Only if nothing else has happened to the banner since: a
                // person who dismissed it, or a newer error, wins.
                if this.banner_action.as_ref() == Some(&action) {
                    this.run_banner_action(cx);
                }
            });
        }));
    }

    /// Press the banner's action.
    pub fn run_banner_action(&mut self, cx: &mut Context<Self>) {
        let action = self.banner_action.take();
        self.banner = None;
        match action {
            Some(BannerAction::RetryTurn(text)) => self.submit(text, cx),
            Some(BannerAction::RetryShell(command)) => self.run_user_shell(command, cx),
            Some(BannerAction::OpenProviders) => cx.emit(SessionEvent::OpenProviders),
            Some(BannerAction::RetryNow) => self.retry_lost_child(cx),
            Some(BannerAction::SharedFolder { worktree: true }) => {
                cx.emit(RuntimeEvent::BannerChanged);
                cx.emit(SessionEvent::RunInWorktree);
            }
            Some(BannerAction::SharedFolder { worktree: false }) | None => cx.emit(RuntimeEvent::BannerChanged),
        }
    }

    #[cfg(all(test, feature = "probe"))]
    /// The banner's action, for the window's tests.
    pub(crate) fn banner_action_for_test(&self) -> Option<BannerAction> {
        self.banner_action.clone()
    }

    #[cfg(all(test, feature = "probe"))]
    /// Let the shared-folder warning show again (a test re-raises it).
    pub(crate) fn forget_shared_folder_warning(&mut self) {
        self.shared_folder_warned = false;
    }

    /// Take the banner down without running its action (a banner's
    /// secondary Dismiss).
    pub fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner_action = None;
        self.banner = None;
        cx.emit(RuntimeEvent::BannerChanged);
    }

    /// The shared-folder warning (docs/82 6.2), once per session: `false`
    /// when this session already showed it. `worktree` offers "Run in a
    /// worktree". It is a fact, not an error: a banner already up (an
    /// error the person has not read) is left alone.
    pub(crate) fn warn_shared_folder(&mut self, worktree: bool, cx: &mut Context<Self>) -> bool {
        if self.shared_folder_warned || self.banner.is_some() {
            return false;
        }
        self.shared_folder_warned = true;
        self.set_banner(crate::session::SHARED_FOLDER_TEXT, Some(BannerAction::SharedFolder { worktree }), cx);
        true
    }

    /// Put `message` on the session's banner, with no action.
    pub(crate) fn show_banner(&mut self, message: &str, cx: &mut Context<Self>) {
        self.set_banner(message, None, cx);
    }

    /// One place that writes the banner, so its message and its action can never
    /// disagree.
    pub(super) fn set_banner(&mut self, message: &str, action: Option<BannerAction>, cx: &mut Context<Self>) {
        // Another banner replaces a grown lost one: its failure goes too.
        if action != Some(BannerAction::RetryNow) {
            self.child_failure = None;
        }
        self.banner = Some(message.to_owned());
        self.banner_action = action;
        self.banner_is_mode_fallback = false;
        cx.emit(RuntimeEvent::BannerChanged);
    }

    /// The banner saying a model pick left Auto for Ask: the one banner a
    /// later mode pick takes down, since the person answered it.
    pub(super) fn show_mode_fallback(&mut self, message: &str, cx: &mut Context<Self>) {
        self.set_banner(message, None, cx);
        self.banner_is_mode_fallback = true;
    }

    /// Take down the Auto-fallback banner, if that is what shows: a mode
    /// pick supersedes it.
    pub(super) fn dismiss_mode_fallback(&mut self, cx: &mut Context<Self>) {
        if self.banner_is_mode_fallback {
            self.banner_is_mode_fallback = false;
            self.banner = None;
            self.banner_action = None;
            cx.emit(RuntimeEvent::BannerChanged);
        }
    }

    /// A session opened on a disabled provider: readable, but no child
    /// starts — the quiet banner with the way back instead.
    pub fn show_disabled_notice(&mut self, provider: ProviderId, cx: &mut Context<Self>) {
        self.set_banner(
            &crate::settings_providers::disabled_banner(provider),
            Some(BannerAction::OpenProviders),
            cx,
        );
    }
}

impl SessionRuntime {
    // --------------------------------------- external approvals (S46)

    /// Park a new provider's approval request on this surface: Claude
    /// Code's `can_use_tool` control request, or one of Codex's five
    /// server->client approval requests.
    ///
    /// The card this renders is never ahead of the server: it shows
    /// pending until [`Self::resolve_external_approval`] runs on the
    /// server's notification.
    /// Park a new provider's approval request on this surface.
    ///
    /// The provider lane task calls this the moment an `ApprovalRequested`
    /// lands.
    pub fn inject_external_approval(&mut self, approval: ExternalApproval, cx: &mut Context<Self>) {
        self.external_approvals.inject(approval);
        cx.emit(RuntimeEvent::ApprovalRequested);
    }

    /// Decide an external approval: `decline` ("no, do something else",
    /// the turn continues) and `cancel` ("no, stop", the turn is
    /// interrupted) ride as the different answers they are.
    ///
    /// The press sends exactly one `DecideApproval` onto the outbox the
    /// provider lane drains, then waits: the card shows "sent, waiting"
    /// and changes only when the server's notification resolves it. A
    /// repeat press while one is in flight sends nothing twice.
    pub fn decide_external_approval(
        &mut self,
        approval_id: String,
        choice: &str,
        feedback: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let headline =
            self.external_approvals.get(&approval_id).map(|a| a.headline.clone()).unwrap_or_default();
        let Some((choice_id, stage_token)) = self.external_approvals.decide(&approval_id, choice) else {
            crate::baaz_log!("external approval {approval_id}: nothing pending under that id, or a decision is already in flight; sending nothing twice");
            return;
        };
        crate::baaz_log!("external approval {approval_id} ({headline}): deciding {choice_id}");
        cx.emit(RuntimeEvent::ApprovalDecided(approval_id.clone()));
        self.external_outbox.push(ProviderCommand::DecideApproval {
            request_id: uuid::Uuid::new_v4().to_string(),
            session_id: self.session_id.clone(),
            approval: approval_id,
            choice: choice_id,
            stage_token,
            feedback,
        });
        // Drain the outbox oldest-first through the lane, now — this is the
        // production caller `take_external_outbox` was waiting for. The
        // card stays as the press left it ("sent, waiting"): only the
        // server's resolution moves it, never the ack below.
        for command in self.take_external_outbox() {
            let approval = match &command {
                ProviderCommand::DecideApproval { approval, .. } => approval.clone(),
                _ => String::new(),
            };
            self.provider_send(command, cx, move |this, result, cx| {
                if let Err(error) = result {
                    // The provider holds nothing under that id: nothing
                    // will ever settle it, so it leaves here too rather
                    // than staying armed for the next key.
                    if error.is_unknown_approval() {
                        crate::baaz_log!("external approval {approval}: the provider holds no such approval: {error}");
                        this.drop_gone_approval(&approval, cx);
                        return;
                    }
                    // The decision never landed: re-park the card so the
                    // press can be tried again, and banner the reason —
                    // an `Unsupported` refusal shows it verbatim.
                    crate::baaz_log!("external approval {approval}: the decision did not land: {error}");
                    this.external_approvals.repark(&approval);
                    this.report_provider_error(&error, cx);
                }
            });
        }
    }

    /// Decide the inline approval card on a provider lane: the card's own
    /// choice id travels as `DecideApproval`, with the parked tap's stage
    /// token when one is parked. The card stays pending — only the
    /// server's resolution moves it, never this send; a refused send
    /// banners and leaves the press retryable.
    pub fn decide_provider_approval(
        &mut self,
        approval_id: String,
        choice_id: String,
        feedback: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let stage_token =
            self.external_approvals.get(&approval_id).and_then(|tap| tap.stage_token.clone());
        crate::baaz_log!("provider approval {approval_id}: deciding {choice_id} on the inline card");
        // The press is the field's send: the field closes with it, as the
        // muse lane's does. Left open, it kept the composer's ⏎ bound to
        // `ConfirmField`, which re-sent this settled decision on every
        // later Enter ("unknown approval", live 2026-10-17).
        cx.emit(RuntimeEvent::ApprovalDecided(approval_id.clone()));
        self.external_outbox.push(ProviderCommand::DecideApproval {
            request_id: uuid::Uuid::new_v4().to_string(),
            session_id: self.session_id.clone(),
            approval: approval_id.clone(),
            choice: choice_id,
            stage_token,
            feedback,
        });
        for command in self.take_external_outbox() {
            let approval = match &command {
                ProviderCommand::DecideApproval { approval, .. } => approval.clone(),
                _ => String::new(),
            };
            self.provider_send(command, cx, move |this, result, cx| {
                if let Err(error) = result {
                    if error.is_unknown_approval() {
                        this.drop_gone_approval(&approval, cx);
                        return;
                    }
                    // A lost race the provider already settled in the
                    // transcript: someone else decided it, or the choices
                    // moved to the next stage. Logged, never bannered.
                    match error.hint() {
                        provider::RefusalHint::AlreadySettled | provider::RefusalHint::StaleChoice => {
                            crate::baaz_log!("provider approval {approval}: lost the race ({error}); the card follows the provider");
                            return;
                        }
                        provider::RefusalHint::InvalidChoice => {
                            this.set_banner("That choice is no longer offered for this command.", None, cx);
                            return;
                        }
                        provider::RefusalHint::ItemGone => {
                            let agent = this.provider_kind().label();
                            this.set_banner(&format!("{agent} no longer knows about that approval."), None, cx);
                            return;
                        }
                        _ => {}
                    }
                    this.report_provider_error(&error, cx);
                    crate::baaz_log!("provider approval {approval}: the decision never landed: {error}");
                }
            });
        }
    }
}

impl SessionView {
    /// Close the feedback field when it belongs to `approval_id`: the
    /// decision it was opened for is on its way.
    pub(super) fn close_feedback_for(&mut self, approval_id: &str) {
        if self.feedback_open.as_ref().is_some_and(|(open, _)| open == approval_id) {
            self.feedback_open = None;
        }
    }
}

impl SessionRuntime {
    /// The provider answered a decision with "unknown approval": no pending
    /// request carries that id there any more (answered already, expired,
    /// or its child is gone), so no notification will ever settle it. It
    /// leaves here too — the parked tap, a field open on it, and the
    /// inline card, which settles the way muse's `approvalNotFound` does:
    /// `Cancelled` ("Not run — <Provider> withdrew it"), never `Denied`,
    /// which would tell the person they refused it — so it can never hold
    /// a key again.
    pub(super) fn drop_gone_approval(&mut self, approval_id: &str, cx: &mut Context<Self>) {
        use aui_protocol::{ApprovalState, Block, Delta};
        crate::baaz_log!("provider approval {approval_id}: the provider holds nothing under that id; dropping the card");
        self.external_approvals.resolve(approval_id);
        cx.emit(RuntimeEvent::ApprovalDecided(approval_id.to_owned()));
        let pending = self.store.session().and_then(|session| {
            session.turns.iter().find_map(|turn| {
                let index = turn.blocks().iter().position(|block| {
                    matches!(block, Block::Approval { id, state: ApprovalState::Pending, .. } if id == approval_id)
                })?;
                Some((turn.id().to_owned(), index, turn.blocks()[index].clone()))
            })
        });
        if let Some((turn_id, block_index, mut block)) = pending {
            if let Block::Approval { state, .. } = &mut block {
                *state = ApprovalState::Cancelled { reason: Some(format!("{} withdrew it", self.provider_kind().label())) };
            }
            let landed = self.store.apply_deltas(vec![Delta::BlockUpdated { turn_id, block_index, block }]);
            self.announce_landed(&landed, cx);
        }
        cx.emit(super::SessionEvent::ProviderApprovalsChanged { session_id: self.session_id.clone() });
    }

    /// Settle an external approval from the server's notification: the
    /// only thing that ever moves the card after the press. Returns
    /// whether anything was waiting under that id.
    ///
    /// The provider lane calls this when a landed delta settles the
    /// approval block, and tests call it to stand in for that delta.
    pub fn resolve_external_approval(&mut self, approval_id: &str, cx: &mut Context<Self>) -> bool {
        let settled = self.external_approvals.resolve(approval_id);
        if settled {
            cx.emit(RuntimeEvent::EntriesAppended);
        }
        settled
    }
}

impl SessionView {
    /// The newest approval still awaiting a decision, and its current choices.
    ///
    /// Read from the transcript rather than from the pending map, because the
    /// transcript is in wire order and "newest" is a question about order —
    /// but read from the render cache rather than re-scanned, because the
    /// answer only changes when the fold does (finding `performance-2`).
    /// [`Self::refresh_render_cache`] fills it, and every fold change sets
    /// `follow`, so the cache is refreshed before the frame that could show a
    /// new card and before any keystroke that frame's reader could send.
    pub(super) fn newest_pending_approval(
        &self,
    ) -> Option<&(String, Vec<aui_protocol::ApprovalChoice>)> {
        self.cached_pending_approval.as_ref()
    }
}

impl SessionRuntime {
    /// The newest question still awaiting an answer, and its option labels.
    pub(super) fn newest_pending_question(&self) -> Option<(String, Vec<String>)> {
        let session = self.session()?;
        session.turns.iter().rev().flat_map(|turn| turn.blocks().iter().rev()).find_map(|block| match block {
            Block::Question { id, options, answer: None, .. } => {
                Some((id.clone(), options.iter().map(|o| o.label.clone()).collect()))
            }
            _ => None,
        })
    }

    /// F10. The transcript's own first user prompt, as a title: a session is
    /// named after what the person said, never after a command the agent
    /// ran. The newest submission is not the question — the earliest is —
    /// so this reads the fold's map oldest-first where `first_prompt_text`
    /// reads it newest-first for the local row.
    ///
    /// The fold is already in memory, so this costs nothing at all — and it
    /// reaches the case `session/read` cannot, because the history of a session
    /// nobody has loaded is not served.
    pub fn first_user_title(&self) -> Option<String> {
        let session = self.session()?;
        let mut submissions: Vec<&str> = Vec::new();
        submissions.extend(self.store.command_ids().filter_map(|id| self.store.command_text(id)));
        crate::sessions::first_user_title(&submissions, &session.turns)
    }

    /// F10. The first shell command in this session's transcript, as a title.
    ///
    /// Only the fallback for a session with no user text at all (a
    /// shell-only session): [`Self::first_user_title`] answers first.
    ///
    /// The fold is already in memory, so this costs nothing at all — and it
    /// reaches the case `session/read` cannot, because the history of a session
    /// nobody has loaded is not served.
    pub fn first_shell_title(&self) -> Option<String> {
        let session = self.session()?;
        session
            .turns
            .iter()
            .flat_map(|turn| turn.blocks().iter())
            .find_map(|block| match block {
                Block::ToolCall { kind: aui_protocol::ToolKind::Shell, target, .. } => {
                    crate::sessions::shell_title(target)
                }
                _ => None,
            })
    }

}

impl SessionView {
    /// Press the `n`-th choice (from 1) of the newest pending approval —
    /// the digits on the card, and the `choose:<n>` step. With no muse card
    /// pending, a parked provider tap answers to the same digits (V1) —
    /// the newest undecided one, in the card's own choice order. A choice
    /// that takes feedback opens its field on the first press (the second
    /// sends). Fails with nothing pending or a number past the card's
    /// choices.
    pub(crate) fn choose(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if n == 0 {
            return Err("choices are numbered from 1".into());
        }
        if let Some((approval_id, choices)) = self.newest_pending_approval() {
            let choice = choices
                .get(n - 1)
                .ok_or_else(|| format!("the pending approval has {} choices, not {n}", choices.len()))?;
            let (approval_id, choice) = (approval_id.clone(), choice.clone());
            if choice.accepts_feedback && self.feedback_open.is_none() {
                // The same two-press dance a person does: the first press
                // opens the field, `feedback:` fills it, the second sends.
                self.toggle_feedback(approval_id, Some(choice.id.clone()), window, cx);
                return Ok(());
            }
            let feedback = self.feedback_open.is_some().then(|| self.feedback.read(cx).value().to_string());
            self.runtime.update(cx, |rt, cx| rt.decide_approval(approval_id, choice.id.clone(), feedback, cx));
            return Ok(());
        }
        let tap = self.runtime.read(cx).external_approvals.pending().last().cloned().cloned().ok_or("no approval is pending")?;
        // The tap's own options, in the vocabulary's card order (docs/82
        // 5.5): what the provider said it takes, never a fixed list.
        let option = tap
            .options
            .get(n - 1)
            .ok_or_else(|| format!("the pending approval has {} choices, not {n}", tap.options.len()))?;
        let choice = option.choice.clone();
        self.runtime.update(cx, |rt, cx| rt.decide_external_approval(tap.id.clone(), &choice, None, cx));
        Ok(())
    }
}
