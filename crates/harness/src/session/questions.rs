//! Structured questions: selection, previews, clarification and the
//! answer that settles one.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ------------------------------------------------------------- questions

    /// A radio or checkbox on a pending question.
    pub fn select_option(&mut self, block_id: String, index: usize, cx: &mut Context<Self>) {
        let multi = self.question(&block_id).is_some_and(|q| q.multi);
        let selected = self.selections.entry(block_id).or_default();
        match (multi, selected.iter().position(|i| *i == index)) {
            (true, Some(at)) => {
                selected.remove(at);
            }
            (true, None) => selected.push(index),
            (false, _) => *selected = vec![index],
        }
        cx.notify();
    }

    /// An option's "Preview" chevron.
    pub fn toggle_preview(&mut self, block_id: String, index: usize, cx: &mut Context<Self>) {
        let open = self.previews.entry(block_id).or_default();
        match open.iter().position(|i| *i == index) {
            Some(at) => {
                open.remove(at);
            }
            None => open.push(index),
        }
        cx.notify();
    }

    /// The pending question a block id names.
    ///
    /// The fold names a question block `"<userInputId>:<questionId>"`, because
    /// one MSP request may carry several questions and each is its own card.
    pub(super) fn question(&self, block_id: &str) -> Option<Pending> {
        let (input_id, question_id) = block_id.split_once(':')?;
        let side = self.fold.side(&self.session_id)?;
        let request = side.pending_inputs.get(input_id)?;
        let question = request.questions.iter().find(|q| q.id == question_id)?;
        Some(Pending {
            input_id: input_id.to_owned(),
            question_id: question_id.to_owned(),
            multi: matches!(question.selection.mode, UserInputSelectionMode::Multiple),
            labels: question.options.iter().map(|o| o.label.clone()).collect(),
            questions: request.questions.len(),
        })
    }

    /// "Continue" on a question.
    ///
    /// MSP settles the whole prompt at once and keys answers on option
    /// **labels**, so a request with several questions gathers its answers here
    /// and sends one `userInput/answer` when the last one is answered.
    pub fn answer_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(pending) = self.question(&block_id) else { return };
        let selected = self.selections.get(&block_id).cloned().unwrap_or_default();
        if selected.is_empty() {
            return;
        }
        let chosen: Vec<String> = selected.iter().filter_map(|i| pending.labels.get(*i).cloned()).collect();
        let answer = UserInputAnswer {
            free_text: None,
            note: None,
            question_id: pending.question_id.clone(),
            // Exactly one of `selectedLabel` and `selectedLabels`, chosen by the
            // question's own mode: sending both is `userInputAnswerInvalid`.
            selected_label: (!pending.multi).then(|| chosen.first().cloned()).flatten(),
            selected_labels: pending.multi.then(|| chosen.clone()),
        };
        let gathered = self.answers.entry(pending.input_id.clone()).or_default();
        gathered.insert(pending.question_id.clone(), answer);
        if gathered.len() < pending.questions {
            // Not the last question of the prompt: the answers wait here until
            // its siblings are answered, and the prompt settles once.
            cx.notify();
            return;
        }
        let answers: Vec<UserInputAnswer> =
            self.answers.remove(&pending.input_id).unwrap_or_default().into_values().collect();
        let Some(client) = self.wire_client(cx) else { return };
        let input_id = pending.input_id.clone();
        let params = UserInputAnswerParams {
            answers,
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        self.wire_call(cx, move || client.user_input_answer(&params), move |this, result, cx| {
            if let Err(error) = result {
                this.settle_failed(&input_id, &error, cx);
            }
        });
        cx.notify();
    }

    /// "Skip": `userInput/cancel`.
    pub fn skip_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(pending) = self.question(&block_id) else { return };
        let Some(client) = self.wire_client(cx) else { return };
        self.answers.remove(&pending.input_id);
        let input_id = pending.input_id.clone();
        let params = UserInputCancelParams {
            command_id: new_command_id(),
            reason: Some("The person declined to answer.".to_owned()),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        self.wire_call(cx, move || client.user_input_cancel(&params), move |this, result, cx| {
            if let Err(error) = result {
                this.settle_failed(&input_id, &error, cx);
            }
        });
        cx.notify();
    }

    /// "Explain instead": open the field, or send what is in it.
    pub fn clarify_question(&mut self, block_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.clarify_open.as_deref() == Some(block_id.as_str()) {
            self.send_clarification(&block_id, window, cx);
            return;
        }
        self.clarify_open = Some(block_id);
        self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
        window.focus(&self.clarify.focus_handle(cx), cx);
        cx.notify();
    }

    pub(super) fn send_clarification(&mut self, block_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.question(block_id) else { return };
        let content = self.clarify.read(cx).value().to_string();
        if content.trim().is_empty() {
            return;
        }
        let Some(client) = self.wire_client(cx) else { return };
        self.clarify_open = None;
        self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
        let input_id = pending.input_id.clone();
        let params = UserInputClarifyParams {
            // `format` is `"text"` in v1 and the field is open, so it is named
            // rather than left to a default that might change.
            clarification: UserInputClarification { content, format: "text".to_owned() },
            command_id: new_command_id(),
            session_id: self.session_id.clone(),
            user_input_id: input_id.clone(),
        };
        self.wire_call(cx, move || client.user_input_clarify(&params), move |this, result, cx| {
            if let Err(error) = result {
                this.settle_failed(&input_id, &error, cx);
            }
        });
        cx.notify();
    }

    /// The two ways a `userInput/*` settlement can lose.
    pub(super) fn settle_failed(&mut self, input_id: &str, error: &MuseError, cx: &mut Context<Self>) {
        match error.kind() {
            // Something already settled it — a timeout, most likely — and
            // `userInput/settled` is on its way with the real outcome.
            Some(ErrorKind::UserInputAlreadySettled) => {}
            Some(ErrorKind::UserInputAnswerInvalid) => {
                self.answers.remove(input_id);
                self.set_banner("Muse refused that answer; pick again.", None, cx);
            }
            _ => self.report(error, cx),
        }
    }
}
