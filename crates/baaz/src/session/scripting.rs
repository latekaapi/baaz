//! `--steps` for a session view: the verbs that drive one session from
//! the command line, for screenshots and smoke runs.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // -------------------------------------------------------------- scripting

    // One handler per `--steps` verb that needs more than a single existing
    // call. The verb table that reaches them — and the parser, the runner and
    // the cost notes — is [`crate::steps`]; these are only the bodies that
    // touch this view's own state, which is why they live here.

    /// `meter`: pin the context meter's breakdown open.
    pub(crate) fn step_meter(&mut self, cx: &mut Context<Self>) {
        self.meter_open = true;
        cx.notify();
    }

    /// `context:<used>/<window>/<level>`: a pressure state the echo provider
    /// cannot be pushed into — a synthetic `session/contextUsage`, only ever
    /// reachable from this flag.
    pub(crate) fn step_context(&mut self, rest: &str, cx: &mut Context<Self>) {
        self.fake_context = parse_context(rest);
        cx.notify();
    }

    /// `plus`: toggle the composer's `+` menu.
    pub(crate) fn step_plus(&mut self, cx: &mut Context<Self>) {
        self.plus_open = !self.plus_open;
        cx.notify();
    }

    /// `drop`: raise the drop overlay.
    pub(crate) fn step_drop(&mut self, cx: &mut Context<Self>) {
        self.dragging = true;
        cx.notify();
    }

    /// `command:<filter>` / `mention:<filter>`: type the sigil and the filter
    /// into the draft, which is what opens the caret popover.
    pub(crate) fn step_caret_menu(&mut self, sigil: &str, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = format!("{sigil}{rest}");
        self.set_draft(text, window, cx);
        self.on_draft_changed(cx);
    }

    /// `setmode:<mode>`: `session/setApprovalMode`, without opening the picker.
    pub(crate) fn step_setmode(&mut self, rest: &str, cx: &mut Context<Self>) {
        match MODES
            .iter()
            .copied()
            .find(|m| format!("{m:?}").eq_ignore_ascii_case(rest) || m.label().eq_ignore_ascii_case(rest))
        {
            Some(mode) => self.set_mode(mode, cx),
            None => crate::baaz_log!("unknown approval mode `{rest}`"),
        }
    }

    /// `choose:<n>`: the n-th choice of the newest pending approval, 1-based,
    /// exactly as the digits on the card are.
    pub(crate) fn step_choose(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(n) = rest.parse::<usize>() else { return };
        let Some((approval_id, choices)) = self.newest_pending_approval() else { return };
        let Some(choice) = choices.get(n.saturating_sub(1)) else { return };
        let (approval_id, choice) = (approval_id.clone(), choice.clone());
        if choice.accepts_feedback && self.feedback_open.is_none() {
            // The same two-press dance a person does: the first press
            // opens the field, `feedback:` fills it, the second sends.
            self.toggle_feedback(approval_id, Some(choice.id.clone()), window, cx);
            return;
        }
        let feedback = self.feedback_open.is_some().then(|| self.feedback.read(cx).value().to_string());
        self.decide_approval(approval_id, choice.id.clone(), feedback, cx);
    }

    /// `feedback:<text>`: type into whichever field is open — an approval's
    /// feedback or a question's clarification — without sending it.
    pub(crate) fn step_feedback(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let field = if self.feedback_open.is_some() { self.feedback.clone() } else { self.clarify.clone() };
        field.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        cx.notify();
    }

    /// `answer:<label>` / `answers:<a|b>`: pick options on the newest question
    /// and send them.
    pub(crate) fn step_answer(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Some((block_id, labels)) = self.newest_pending_question() else { return };
        for wanted in rest.split('|').map(str::trim).filter(|s| !s.is_empty()) {
            if let Some(index) = labels.iter().position(|l| l == wanted) {
                self.select_option(block_id.clone(), index, cx);
            } else {
                crate::baaz_log!("no option labelled `{wanted}`");
            }
        }
        self.answer_question(block_id, cx);
    }

    /// `clarify:<text>`: with no text this only opens the field, which is what
    /// a screenshot of the open field wants; with text it opens, fills and
    /// sends, which is what the round-trip wants.
    pub(crate) fn step_clarify(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((block_id, _)) = self.newest_pending_question() else { return };
        self.clarify_open = Some(block_id.clone());
        self.clarify.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        if rest.trim().is_empty() {
            cx.notify();
            return;
        }
        self.send_clarification(&block_id, window, cx);
    }

    /// `preview:<n>`: the n-th option's preview on the newest question, 0-based.
    pub(crate) fn step_preview(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Ok(n) = rest.parse::<usize>() else { return };
        if let Some((block_id, _)) = self.newest_pending_question() {
            self.toggle_preview(block_id, n, cx);
        }
    }

    /// `select:<label>`: pick without sending, for a capture of a half-answered
    /// card.
    pub(crate) fn step_select(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Some((block_id, labels)) = self.newest_pending_question() else { return };
        if let Some(index) = labels.iter().position(|l| l == rest) {
            self.select_option(block_id, index, cx);
        }
    }

    /// `skip`: decline the newest question.
    pub(crate) fn step_skip(&mut self, cx: &mut Context<Self>) {
        if let Some((block_id, _)) = self.newest_pending_question() {
            self.skip_question(block_id, cx);
        }
    }

    /// `top`: jump to the head of the transcript without touching the pointer.
    pub(crate) fn step_top(&mut self, cx: &mut Context<Self>) {
        self.follow = false;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        cx.notify();
    }

    /// `end`: jump to the tail of the transcript.
    pub(crate) fn step_end(&mut self, cx: &mut Context<Self>) {
        self.list_state.scroll_to_end();
        cx.notify();
    }

    /// `mid`: jump to the middle of the transcript.
    pub(crate) fn step_mid(&mut self, cx: &mut Context<Self>) {
        self.follow = false;
        let mid = self.list_len / 2;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: mid, offset_in_item: px(0.0) });
        cx.notify();
    }

    /// `bench:<n>`: the frame-stats driver — N back-to-back frames so
    /// `BAAZ_FRAME_STATS` percentiles have samples on a static replay,
    /// which would otherwise idle after a few frames.
    pub(crate) fn step_bench(&mut self, rest: &str, cx: &mut Context<Self>) {
        let n: usize = rest.parse().unwrap_or(240);
        self.tasks.push(cx.spawn(async move |this, cx| {
            for _ in 0..n {
                cx.background_executor().timer(std::time::Duration::from_millis(16)).await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
        }));
    }

    /// `retry`: retry the newest failed turn.
    pub(crate) fn step_retry(&mut self, cx: &mut Context<Self>) {
        if let Some(turn_id) = self.newest_failed_turn() {
            self.retry_turn(turn_id, cx);
        }
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
        if let Some(side) = self.fold.side(&self.session_id) {
            submissions.extend(side.command_text.values().map(String::as_str));
        }
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
