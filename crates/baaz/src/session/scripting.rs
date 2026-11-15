//! `--steps` for a session view: the verbs that drive one session from
//! the command line, for screenshots and smoke runs.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;
use crate::steps::StepResult;

impl SessionView {
    // -------------------------------------------------------------- scripting

    // One handler per `--steps` verb that needs more than a single existing
    // call. The verb table that reaches them — and the parser, the runner and
    // the cost notes — is [`crate::steps`]; these are only the bodies that
    // touch this view's own state, which is why they live here.
    //
    // Every handler returns a [`StepResult`]: `Err(reason)` whenever the
    // step could not do what it says (a payload naming nothing, no card to
    // answer, a send the view held back), so the runner stops the script
    // there instead of capturing a window where nothing happened.

    /// Why a provider command sent now would go nowhere: the lane is still
    /// opening (or never opened), and [`Self::provider_send`] would only
    /// banner. `None` when a provider is attached.
    fn step_lane_refusal(&self) -> Option<String> {
        if self.lane_provider().is_some() {
            return None;
        }
        Some(if self.is_starting() {
            "the session is still starting: no provider lane to send on".to_owned()
        } else {
            "the session did not start: no provider lane to send on".to_owned()
        })
    }

    /// `send:<text>`: send `text` as a turn, as the composer's Enter does
    /// without touching the composer. Fails on empty text, and when the
    /// view holds the send back instead of handing it to the provider — a
    /// failed start, a retired or quiescing handoff source, another
    /// window's lease, the billing guard, images the provider cannot take
    /// — or there is no provider lane to carry it. The reason is the
    /// banner the view raised. **Costs a turn** when it succeeds.
    pub(crate) fn step_send(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        if rest.trim().is_empty() {
            return Err("no text to send".into());
        }
        let lane = self.step_lane_refusal();
        let before = self.pending_optimistic.len();
        self.send_text(rest.to_owned(), cx);
        // Only a send handed to the provider folds its optimistic bubble;
        // every hold in `submit` returns before that, restoring the words.
        if self.pending_optimistic.len() <= before {
            return Err(self.banner.clone().unwrap_or_else(|| "the send was held back".into()));
        }
        match lane {
            Some(reason) => Err(reason),
            None => Ok(()),
        }
    }

    /// `send-anyway`: the billing banner's "Send anyway" — sends what the
    /// guard held back (the restored prompt, or the composer's draft and
    /// attachments). Fails, changing nothing, when nothing is held to send.
    pub(crate) fn step_send_anyway(&mut self, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let held = self.pending_prompt.is_some()
            || !self.draft_is_empty(cx)
            || !self.images.is_empty()
            || !self.files.is_empty();
        if !held {
            return Err("nothing held back to send".into());
        }
        self.send_anyway(window, cx);
        Ok(())
    }

    /// `steer:<text>`: interject `text` into the running turn. Fails on
    /// empty text, when the provider does not offer steering, when no turn
    /// is running, or with no provider lane — each of which banners and
    /// keeps the words rather than steering.
    pub(crate) fn step_steer(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        if rest.trim().is_empty() {
            return Err("no text to steer with".into());
        }
        let refusal = if !crate::providers::capability_state(self.provider_kind(), provider::Capability::SteerTurn)
            .allows_attempt()
        {
            Some(self.steer_gate().unwrap_or_else(|| "steering is not available on this provider".into()))
        } else if self.running.is_none() {
            Some("no running turn to steer".to_owned())
        } else {
            self.step_lane_refusal()
        };
        self.steer_text(rest.to_owned(), cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `confirm`: Enter on the open composer menu. Fails when no menu is
    /// open (Enter on nothing).
    pub(crate) fn step_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        if self.overlays.read(cx).menu.is_none() {
            return Err("no menu is open".into());
        }
        self.confirm_menu(window, cx);
        Ok(())
    }

    /// `setmodel:<id>`: pick a model without opening the picker. Fails,
    /// changing nothing, on an empty id, when the provider does not offer
    /// model switching, or when the session's catalog is known and does not
    /// list the id. With no catalog yet (offline, still listing) the pick
    /// goes through as the picker's would, recorded on the chip.
    pub(crate) fn step_setmodel(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let model = rest.trim();
        if model.is_empty() {
            return Err("no model id".into());
        }
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::SessionConfig)
            .allows_attempt()
        {
            return Err(crate::providers::gate(self.provider_kind(), provider::Capability::SessionConfig)
                .unwrap_or_else(|| "model switching is not available on this provider".into()));
        }
        if !self.models.is_empty() && !self.models.iter().any(|row| row.model_id == model) {
            let known: Vec<&str> = self.models.iter().map(|row| row.model_id.as_str()).collect();
            return Err(format!("the session does not offer model `{model}`; it offers {}", known.join(", ")));
        }
        self.set_model(model, cx);
        Ok(())
    }

    /// `compact`: compact the session's context now. Fails when the
    /// provider does not offer compaction or there is no provider lane —
    /// both banner the reason. A provider's own `noop` answer arrives later
    /// as a toast and is not this step's failure.
    pub(crate) fn step_compact(&mut self, cx: &mut Context<Self>) -> StepResult {
        let refusal = if !crate::providers::capability_state(self.provider_kind(), provider::Capability::CompactSession)
            .allows_attempt()
        {
            Some(
                crate::providers::gate(self.provider_kind(), provider::Capability::CompactSession)
                    .unwrap_or_else(|| "compacting is not available on this provider".into()),
            )
        } else {
            self.step_lane_refusal()
        };
        self.compact(cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `image:<path>` / `file:<path>`: attach the file at `path` as the `+`
    /// menu does. Fails, attaching nothing, on an empty or missing path;
    /// fails after the attempt when nothing was attached — an image on a
    /// provider without image input, the per-turn file cap, or a file that
    /// could not be read (left as a failed chip).
    pub(crate) fn step_attach(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let path = rest.trim();
        if path.is_empty() {
            return Err("no path".into());
        }
        if !std::path::Path::new(path).exists() {
            return Err(format!("no file at `{path}`"));
        }
        let (images, files, failed) = (self.images.len(), self.files.len(), self.failed_chips.len());
        self.attach_paths(vec![path.into()], cx);
        if self.images.len() > images || self.files.len() > files {
            return Ok(());
        }
        if self.failed_chips.len() > failed {
            let reason = self.failed_chips.last().map(|(_, _, reason)| reason.to_string()).unwrap_or_default();
            return Err(format!("`{path}` could not be attached: {reason}"));
        }
        Err(self.banner.clone().unwrap_or_else(|| format!("`{path}` was not attached")))
    }

    /// `shell:<command>`: run `command` out of turn (`session/userShell`).
    /// Fails on an empty command, and when the run is refused — the
    /// provider does not offer it, another window holds the session, or
    /// there is no provider lane — each of which banners.
    pub(crate) fn step_shell(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let command = rest.trim();
        if command.is_empty() {
            return Err("no command".into());
        }
        let refusal = if !crate::providers::capability_state(self.provider_kind(), provider::Capability::SessionShell)
            .allows_attempt()
        {
            Some(
                crate::providers::gate(self.provider_kind(), provider::Capability::SessionShell)
                    .unwrap_or_else(|| "shell commands are not available on this provider".into()),
            )
        } else if let Some(notice) = self.lease_notice.clone() {
            Some(notice)
        } else {
            self.step_lane_refusal()
        };
        self.run_user_shell(command.to_owned(), cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `fork`: fork at the newest completed turn. Fails, sending nothing,
    /// when the provider does not offer forking (after bannering why, as
    /// the command does), when there is no completed turn to fork at, or
    /// with no provider lane. A refusal the provider answers later banners
    /// and is not seen here.
    pub(crate) fn step_fork(&mut self, cx: &mut Context<Self>) -> StepResult {
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::ForkSession)
            .allows_attempt()
        {
            self.fork(None, cx);
            return Err(crate::providers::gate(self.provider_kind(), provider::Capability::ForkSession)
                .unwrap_or_else(|| "forking is not available on this provider".into()));
        }
        if self.newest_completed_turn().is_none() {
            return Err("no completed turn to fork from".into());
        }
        let lane = self.step_lane_refusal();
        self.fork(None, cx);
        lane.map_or(Ok(()), Err)
    }

    /// `name:<title>`: `/name <title>`, renaming the session. Fails on an
    /// empty name — which the command would read as "open the field" or
    /// "clear the name", neither of which is naming.
    pub(crate) fn step_name(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        if rest.trim().is_empty() {
            return Err("no name".into());
        }
        self.run_command_with(crate::overlays::Command::Name, rest.to_owned(), window, cx);
        Ok(())
    }

    /// `fixture-models`: a fixed three-row catalog shaped like Muse's own
    /// (owner screenshot 2026-10-30), each row listing reasoning levels so
    /// it opens a submenu — offline, no catalog request, nothing persisted.
    pub(crate) fn step_fixture_models(&mut self, cx: &mut Context<Self>) -> StepResult {
        let levels: Vec<String> = ["minimal", "low", "medium", "high", "xhigh"].map(str::to_owned).to_vec();
        let row = |id: &str, label: &str, default: bool, active: bool| ModelCatalogEntry {
            model_id: id.to_owned(),
            display_label: label.to_owned(),
            description: Some("A sample catalog row for captures.".to_owned()),
            is_active: active,
            is_default: default,
            provider_id: "meta".to_owned(),
            context_limit: Some(1_000_000),
            efforts: levels.clone(),
            default_reasoning_effort: Some("medium".to_owned()),
            effort_notes: Vec::new(),
        };
        self.models = vec![
            row("muse-wonder-1.0", "Muse Wonder 1.0", true, false),
            row("muse-spark-1.3", "Muse Spark 1.3", false, false),
            row("muse-spark-1.2", "Muse Spark 1.2", false, true),
        ];
        self.models_error = None;
        self.models_fixture = true;
        cx.notify();
        Ok(())
    }

    /// `submenu`: open the model menu's highlighted row's submenu, as the
    /// → key does. Fails when the model menu is not open, or when the
    /// highlighted row lists no reasoning levels (it has no submenu to
    /// open).
    pub(crate) fn step_submenu(&mut self, cx: &mut Context<Self>) -> StepResult {
        let Some(selected) =
            self.overlays.read(cx).menu.as_ref().filter(|menu| menu.kind == MenuKind::Model).map(|menu| menu.selected)
        else {
            return Err("the model menu is not open".into());
        };
        if self.models.get(selected).is_none_or(|row| row.efforts.is_empty()) {
            return Err("the highlighted model row has no submenu".into());
        }
        self.overlays.update(cx, |overlays, _| {
            if let Some(menu) = overlays.menu.as_mut() {
                menu.sub_open = true;
            }
        });
        cx.notify();
        Ok(())
    }

    /// `meter`: pin the context meter's breakdown open.
    pub(crate) fn step_meter(&mut self, cx: &mut Context<Self>) -> StepResult {
        self.meter_open = true;
        cx.notify();
        Ok(())
    }

    /// `context:<used>/<window>/<level>`: a pressure state the echo provider
    /// cannot be pushed into — a synthetic `session/contextUsage`, only ever
    /// reachable from this flag. An empty payload clears it; one that does
    /// not parse fails and changes nothing.
    pub(crate) fn step_context(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        if rest.trim().is_empty() {
            self.fake_context = None;
        } else {
            let reading = parse_context(rest.trim()).ok_or_else(|| format!("`{rest}` is not <used>/<window>/<level>"))?;
            self.fake_context = Some(reading);
        }
        cx.notify();
        Ok(())
    }

    /// `plus`: toggle the composer's `+` menu.
    pub(crate) fn step_plus(&mut self, cx: &mut Context<Self>) -> StepResult {
        // The click's own path, so a scripted open closes any open menu too.
        self.toggle_plus_menu(cx);
        Ok(())
    }

    /// `drop`: raise the drop overlay.
    pub(crate) fn step_drop(&mut self, cx: &mut Context<Self>) -> StepResult {
        self.dragging = true;
        cx.notify();
        Ok(())
    }

    /// `command:<filter>` / `mention:<filter>`: type the sigil and the filter
    /// into the draft, which is what opens the caret popover.
    pub(crate) fn step_caret_menu(&mut self, sigil: &str, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let text = format!("{sigil}{rest}");
        self.set_draft(text, window, cx);
        self.on_draft_changed(cx);
        Ok(())
    }

    /// `setmode:<mode>`: `session/setApprovalMode`, without opening the
    /// picker. An unknown mode fails and changes nothing.
    pub(crate) fn step_setmode(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let mode = MODES
            .iter()
            .copied()
            .find(|m| format!("{m:?}").eq_ignore_ascii_case(rest) || m.label().eq_ignore_ascii_case(rest))
            .ok_or_else(|| format!("unknown approval mode `{rest}`"))?;
        self.set_mode(mode, cx);
        Ok(())
    }

    /// `setprovider:<id>`: the provider picker's menu row as a verb —
    /// the same [`SessionView::pick_provider`] the click and Enter
    /// paths call, so the no-live-switch invariant holds here too: a
    /// fresh session swaps lanes through the application, and a
    /// session with turns starts a new session on the pick instead.
    /// That second case still starts the new session but fails the step,
    /// saying so, rather than reading as an in-place swap. A provider
    /// disabled in Settings is refused (with the menu's toast) and fails.
    /// An unknown id fails and changes nothing — never the registry
    /// fallback (which would open Muse for a typo).
    /// A setter, not a toggle: repeating the pick is a no-op success.
    pub(crate) fn step_setprovider(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let id = rest.trim();
        let picked = ProviderId::all()
            .iter()
            .find(|p| p.as_str() == id)
            .copied()
            .ok_or_else(|| format!("unknown provider `{rest}`; known providers are muse, claude-code, codex"))?;
        if picked == self.provider_kind() {
            self.pick_provider(picked.as_str(), cx);
            return Ok(());
        }
        if !crate::settings_providers::live_visible_provider_ids().contains(&picked) {
            self.pick_provider(picked.as_str(), cx);
            return Err(format!("{} is disabled in Settings → Providers", picked.label()));
        }
        if self.has_turns() {
            self.pick_provider(picked.as_str(), cx);
            return Err(format!(
                "`setprovider:{id}` on a session with turns starts a new session on {} instead of swapping this one",
                picked.label()
            ));
        }
        self.pick_provider(picked.as_str(), cx);
        Ok(())
    }

    /// `seteffort:<level>`: the effort picker's menu row as a verb —
    /// the same [`SessionView::pick_effort`] Enter calls, so the chip
    /// reads the pick at once. `default` (or empty) clears to the
    /// Default row. An unknown spelling fails and changes nothing.
    pub(crate) fn step_seteffort(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let level = rest.trim();
        if level.is_empty() || level.eq_ignore_ascii_case("default") {
            self.pick_effort(None, cx);
            return Ok(());
        }
        let effort = crate::projects::parse_effort(level)
            .or_else(|| crate::projects::parse_effort(&level.to_lowercase()))
            .ok_or_else(|| format!("unknown reasoning effort `{rest}`"))?;
        self.pick_effort(Some(effort), cx);
        Ok(())
    }

    /// `choose:<n>`: the n-th choice of the newest pending approval, 1-based,
    /// exactly as the digits on the card are. With no muse card pending, a
    /// parked provider tap answers to the same digits (V1) — the newest
    /// undecided one, in the card's own choice order. Taps take no
    /// feedback, so the press decides at once. A choice that takes feedback
    /// opens its field on the first press (the second sends). Fails on a
    /// payload that is not a number, with no approval pending, or a number
    /// past the card's choices.
    pub(crate) fn step_choose(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let n = rest.trim().parse::<usize>().map_err(|_| format!("`{rest}` is not a choice number"))?;
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
            self.decide_approval(approval_id, choice.id.clone(), feedback, cx);
            return Ok(());
        }
        let tap = self.external_approvals.pending().last().cloned().cloned().ok_or("no approval is pending")?;
        let choices = crate::providers::ApprovalChoice::all();
        let choice = choices
            .get(n - 1)
            .ok_or_else(|| format!("the pending approval has {} choices, not {n}", choices.len()))?;
        self.decide_external_approval(tap.id.clone(), *choice, None, cx);
        Ok(())
    }

    /// `feedback:<text>`: type into whichever field is open — an approval's
    /// feedback or a question's clarification — without sending it. Fails
    /// when neither field is open: the text would land in a field nobody
    /// sees.
    pub(crate) fn step_feedback(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let field = if self.feedback_open.is_some() {
            self.feedback.clone()
        } else if self.clarify_open.is_some() {
            self.clarify.clone()
        } else {
            return Err("no feedback or clarification field is open".into());
        };
        field.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        cx.notify();
        Ok(())
    }

    /// Why a question command would be refused here: the provider does not
    /// offer questions, or there is no provider lane to carry the answer.
    fn step_questions_refusal(&self) -> Option<String> {
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::Questions).allows_attempt()
        {
            return Some(self.questions_gate().unwrap_or_else(|| "questions are not available on this provider".into()));
        }
        self.step_lane_refusal()
    }

    /// `answer:<label>` / `answers:<a|b>`: pick options on the newest question
    /// and send them. Fails, picking nothing, with no question pending, no
    /// label given, or a label the question does not offer; fails after the
    /// attempt when the answer is refused (no questions capability, no
    /// lane).
    pub(crate) fn step_answer(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let (block_id, labels) = self.newest_pending_question().ok_or("no question is pending")?;
        let wanted: Vec<&str> = rest.split('|').map(str::trim).filter(|s| !s.is_empty()).collect();
        if wanted.is_empty() {
            return Err("no option label given".into());
        }
        let mut indices = Vec::new();
        for label in &wanted {
            let index = labels
                .iter()
                .position(|l| l == label)
                .ok_or_else(|| format!("no option labelled `{label}`; the options are {}", labels.join(", ")))?;
            indices.push(index);
        }
        let refusal = self.step_questions_refusal();
        for index in indices {
            self.select_option(block_id.clone(), index, cx);
        }
        self.answer_question(block_id, cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `clarify:<text>`: with no text this only opens the field, which is what
    /// a screenshot of the open field wants; with text it opens, fills and
    /// sends, which is what the round-trip wants. Fails with no question
    /// pending, and when a send is refused (no questions capability, no
    /// lane).
    pub(crate) fn step_clarify(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        let (block_id, _) = self.newest_pending_question().ok_or("no question is pending")?;
        self.clarify_open = Some(block_id.clone());
        self.clarify.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        if rest.trim().is_empty() {
            cx.notify();
            return Ok(());
        }
        let refusal = self.step_questions_refusal();
        self.send_clarification(&block_id, window, cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `preview:<n>`: toggle the n-th option's preview on the newest
    /// question, 0-based. Fails on a payload that is not a number, with no
    /// question pending, or an index past its options.
    pub(crate) fn step_preview(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let n = rest.trim().parse::<usize>().map_err(|_| format!("`{rest}` is not an option index"))?;
        let (block_id, labels) = self.newest_pending_question().ok_or("no question is pending")?;
        if n >= labels.len() {
            return Err(format!("the question has {} options; there is no option {n}", labels.len()));
        }
        self.toggle_preview(block_id, n, cx);
        Ok(())
    }

    /// `select:<label>`: pick without sending, for a capture of a half-answered
    /// card. Fails with no question pending or a label it does not offer.
    pub(crate) fn step_select(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let (block_id, labels) = self.newest_pending_question().ok_or("no question is pending")?;
        let index = labels
            .iter()
            .position(|l| l == rest)
            .ok_or_else(|| format!("no option labelled `{rest}`; the options are {}", labels.join(", ")))?;
        self.select_option(block_id, index, cx);
        Ok(())
    }

    /// `skip`: decline the newest question. Fails with no question pending,
    /// and when the decline is refused (no questions capability, no lane).
    pub(crate) fn step_skip(&mut self, cx: &mut Context<Self>) -> StepResult {
        let (block_id, _) = self.newest_pending_question().ok_or("no question is pending")?;
        let refusal = self.step_questions_refusal();
        self.skip_question(block_id, cx);
        refusal.map_or(Ok(()), Err)
    }

    /// `top`: jump to the head of the transcript without touching the pointer.
    pub(crate) fn step_top(&mut self, cx: &mut Context<Self>) -> StepResult {
        self.follow = false;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        cx.notify();
        Ok(())
    }

    /// `end`: jump to the tail of the transcript.
    pub(crate) fn step_end(&mut self, cx: &mut Context<Self>) -> StepResult {
        self.list_state.scroll_to_end();
        cx.notify();
        Ok(())
    }

    /// `mid`: jump to the middle of the transcript.
    pub(crate) fn step_mid(&mut self, cx: &mut Context<Self>) -> StepResult {
        self.follow = false;
        let mid = self.list_len / 2;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: mid, offset_in_item: px(0.0) });
        cx.notify();
        Ok(())
    }

    /// `bench:<n>`: the frame-stats driver — N back-to-back frames so
    /// `BAAZ_FRAME_STATS` percentiles have samples on a static replay,
    /// which would otherwise idle after a few frames. An empty payload is
    /// 240 frames; one that is not a number fails.
    pub(crate) fn step_bench(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let n: usize = match rest.trim() {
            "" => 240,
            n => n.parse().map_err(|_| format!("`{rest}` is not a frame count"))?,
        };
        self.tasks.push(cx.spawn(async move |this, cx| {
            for _ in 0..n {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
        Ok(())
    }

    /// `retry`: retry the newest failed turn. Fails when no turn failed, or
    /// when this app does not remember what the failed turn sent (a turn
    /// that arrived through a backfill), so there is nothing to resend.
    pub(crate) fn step_retry(&mut self, cx: &mut Context<Self>) -> StepResult {
        let turn_id = self.newest_failed_turn().ok_or("no failed turn to retry")?;
        if self.remembered_text(&turn_id).is_none() {
            return Err(format!("the failed turn `{turn_id}` was not sent from here; nothing to resend"));
        }
        self.retry_turn(turn_id, cx);
        Ok(())
    }

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

    /// The newest turn that ended in an error card, for `--steps retry`.
    pub(super) fn newest_failed_turn(&self) -> Option<String> {
        let session = self.session()?;
        session.turns.iter().rev().find_map(|turn| {
            turn.blocks()
                .iter()
                .any(|block| matches!(block, Block::Error { .. }))
                .then(|| turn.id().to_owned())
        })
    }
}
