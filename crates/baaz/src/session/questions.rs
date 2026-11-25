//! Structured questions: selection, previews, clarification and the
//! answer that settles one.
//!
//! Part of [`SessionRuntime`] (the model half: what it does to the
//! session) and [`SessionView`] (the drawing half: fields, menus, focus);
//! see [`crate::session`] for what each entity owns and why these are
//! their own files.

use super::*;

impl SessionView {
    // ------------------------------------------------------------- questions

    /// A radio or checkbox on a pending question. In a wizard (several
    /// questions, one at a time) a single choice advances on its own; a
    /// pick-all waits for Next (docs/41 §2.6).
    pub fn select_option(&mut self, block_id: String, index: usize, cx: &mut Context<Self>) {
        let pending = self.runtime.read(cx).question(&block_id);
        let multi = pending.as_ref().is_some_and(|q| q.multi);
        let selected = self.selections.entry(block_id.clone()).or_default();
        match (multi, selected.iter().position(|i| *i == index)) {
            (true, Some(at)) => {
                selected.remove(at);
            }
            (true, None) => selected.push(index),
            (false, _) => *selected = vec![index],
        }
        if !multi && pending.is_some_and(|q| q.questions > 1) {
            self.answer_question(block_id, cx);
            return;
        }
        cx.notify();
    }
}

impl SessionRuntime {
    /// The wizard steps of every pending several-question prompt (D4), by
    /// block id: which question it is, of how many, and whether it is the
    /// one showing — the first whose answer is not yet gathered.
    pub(super) fn wizards(&self) -> HashMap<String, (aui::transcript::Wizard, bool)> {
        let mut out = HashMap::new();
        let side = self.store.side();
        let agent = self.provider_kind().label();
        for (input_id, request) in side.pending_prompts.iter() {
            let total = request.questions.len();
            if total < 2 {
                continue;
            }
            let gathered = self.answers.get(input_id);
            let current = request.questions.iter().position(|q| !gathered.is_some_and(|g| g.contains_key(&q.id)));
            // The wire names no topic for a prompt: the first question's
            // header stands in, and the line under it says how many.
            let topic = request.questions.iter().find_map(|q| (!q.header.trim().is_empty()).then(|| q.header.clone())).unwrap_or_else(|| "Questions".to_owned());
            let why = format!("{agent} needs {total} answers");
            for (index, question) in request.questions.iter().enumerate() {
                let wizard = aui::transcript::Wizard { topic: topic.clone().into(), why: why.clone().into(), index, total };
                out.insert(format!("{input_id}:{}", question.id), (wizard, current == Some(index)));
            }
        }
        out
    }
}

impl SessionView {
    /// "Back" in a wizard: the previous question's gathered answer is taken
    /// back, so it shows again with its pick.
    pub fn wizard_back(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some((input_id, question_id)) = block_id.split_once(':') else { return };
        let Some(request) = self.runtime.read(cx).store.side().pending_prompts.get(input_id) else { return };
        let Some(at) = request.questions.iter().position(|q| q.id == question_id) else { return };
        let Some(previous) = at.checked_sub(1).and_then(|i| request.questions.get(i)).map(|q| q.id.clone()) else { return };
        // The borrow notifies the runtime as it drops, which this view
        // observes: the card redraws with the pick shown again.
        if let Some(gathered) = self.runtime.as_mut(cx).answers.get_mut(input_id) {
            gathered.remove(&previous);
        }
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
}

impl SessionRuntime {
    /// The pending question a block id names.
    ///
    /// The fold names a question block `"<userInputId>:<questionId>"`, because
    /// one MSP request may carry several questions and each is its own card.
    pub(super) fn question(&self, block_id: &str) -> Option<Pending> {
        let (input_id, question_id) = block_id.split_once(':')?;
        let request = self.store.side().pending_prompts.get(input_id)?;
        let question = request.questions.iter().find(|q| q.id == question_id)?;
        Some(Pending {
            input_id: input_id.to_owned(),
            question_id: question_id.to_owned(),
            multi: question.multi,
            labels: question.labels.clone(),
            questions: request.questions.len(),
        })
    }

    /// The pending provider-lane question a block id names, read off the
    /// folded transcript: the full prompt arrived as a delta, so its
    /// options and labels are what the card drew. Matches the block's own
    /// id, or a `turn:block` key suffixing it.
    pub(super) fn provider_question(&self, block_id: &str) -> Option<Pending> {
        // A prompt the provider reports beside the transcript (several
        // questions, one card each) is answered as the whole prompt.
        if let Some(pending) = self.question(block_id) {
            return Some(pending);
        }
        let session = self.session()?;
        for turn in session.turns.iter() {
            let Turn::Assistant { blocks, .. } = turn else { continue };
            for block in blocks {
                if let Block::Question { id, multi, options, .. } = block {
                    if id == block_id || block_id.split(':').next_back() == Some(id.as_str()) {
                        return Some(Pending {
                            input_id: id.clone(),
                            question_id: id.clone(),
                            multi: *multi,
                            labels: options.iter().map(|o| o.label.clone()).collect(),
                            questions: 1,
                        });
                    }
                }
            }
        }
        None
    }

    /// Whether the question lane may be attempted here: `Unavailable`
    /// refuses with the registry's own reason, so a stray press banners
    /// instead of sending a command the seam refuses.
    fn check_questions_gate(&mut self, cx: &mut Context<Self>) -> bool {
        if crate::providers::capability_state(self.provider_kind(), provider::Capability::Questions)
            .allows_attempt()
        {
            return true;
        }
        let reason =
            self.questions_gate().unwrap_or_else(|| "Questions are not available on this provider.".into());
        self.plain_banner(reason, cx);
        false
    }
}

impl SessionView {
    /// "Continue" on a question.
    ///
    /// MSP settles the whole prompt at once and keys answers on option
    /// **labels**, so a request with several questions gathers its answers here
    /// and sends one `userInput/answer` when the last one is answered.
    pub fn answer_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        if self.runtime.read(cx).escalation_of(&block_id).is_some() {
            let selected = self.selections.get(&block_id).cloned().unwrap_or_default();
            if selected.is_empty() {
                return;
            }
            self.runtime.update(cx, |rt, cx| rt.answer_escalation(&block_id, aui_protocol::Answer { selected, other: None }, cx));
            return;
        }
        self.answer_provider_question(block_id, cx);
    }

    /// "Continue" on a provider-lane question: one `AnswerQuestion` per
    /// answered card, keyed on the options' labels like the muse lane.
    /// The card settles only when the provider's delta answers it back —
    /// never on this press.
    fn answer_provider_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        if !self.runtime.update(cx, |rt, cx| rt.check_questions_gate(cx)) {
            return;
        }
        let Some(pending) = self.runtime.read(cx).provider_question(&block_id) else { return };
        let selected = self.selections.get(&block_id).cloned().unwrap_or_default();
        if selected.is_empty() {
            return;
        }
        let chosen: Vec<String> = selected.iter().filter_map(|i| pending.labels.get(*i).cloned()).collect();
        let answer = provider::QuestionAnswer {
            question_id: pending.question_id.clone(),
            selected_label: (!pending.multi).then(|| chosen.first().cloned()).flatten(),
            selected_labels: pending.multi.then(|| chosen.clone()),
            free_text: None,
            note: None,
        };
        let (input_id, questions) = (pending.input_id.clone(), pending.questions);
        self.runtime.update(cx, |rt, cx| rt.gather_answer(input_id, questions, answer, cx));
    }
}

impl SessionRuntime {
    /// One answered question of a provider prompt. A prompt of several
    /// questions (a wizard) settles once: answers wait here until its last
    /// question is answered, then go as one `AnswerQuestion`.
    fn gather_answer(&mut self, input_id: String, questions: usize, answer: provider::QuestionAnswer, cx: &mut Context<Self>) {
        let gathered = self.answers.entry(input_id.clone()).or_default();
        gathered.insert(answer.question_id.clone(), answer);
        if gathered.len() < questions {
            cx.emit(RuntimeEvent::QuestionsChanged);
            return;
        }
        let answers: Vec<provider::QuestionAnswer> =
            self.answers.remove(&input_id).unwrap_or_default().into_values().collect();
        let command = ProviderCommand::AnswerQuestion {
            request_id: provider::new_request_id(),
            session_id: self.session_id.clone(),
            question: input_id.clone(),
            answers,
        };
        self.provider_send(command, cx, move |this, result, cx| {
            if let Err(error) = result {
                this.settle_provider_failed(&input_id, &error, cx);
            }
        });
        cx.emit(RuntimeEvent::QuestionsChanged);
    }
}

impl SessionRuntime {
    /// docs/85 14.2: put an escalation card up — the worker's question to
    /// the person, as a question card of Baaz's own (its answer goes to the
    /// task, never to this session's provider).
    pub(crate) fn post_escalation(&mut self, card_id: &str, task_id: &str, header: &str, question: &str, options: Vec<String>, cx: &mut Context<Self>) {
        let block = Block::Question {
            id: card_id.to_owned(),
            header: header.to_owned(),
            prompt: question.to_owned(),
            subtitle: "A worker is waiting for your answer.".to_owned(),
            options: options.iter().enumerate().map(|(i, label)| aui_protocol::QuestionOption { label: label.clone(), description: String::new(), key: (i + 1).to_string(), preview: None }).collect(),
            multi: false,
            allow_other: true,
            answer: None,
            timeout_ms: None,
        };
        self.escalations.insert(card_id.to_owned(), (task_id.to_owned(), options));
        self.append_client_block(card_id, block);
        cx.emit(RuntimeEvent::EntriesAppended);
    }

    /// The task and options of the escalation card `block_id`, if it is one
    /// still waiting.
    pub(crate) fn escalation_of(&self, block_id: &str) -> Option<&(String, Vec<String>)> {
        self.escalations.get(block_id)
    }

    /// The escalation card still waiting here, if any (tests).
    #[cfg(feature = "probe")]
    #[cfg(test)]
    pub(crate) fn escalation_of_any(&self) -> Option<String> {
        self.escalations.keys().next().cloned()
    }

    /// The person answered the escalation card: it settles here, and the
    /// window hears it ([`SessionEvent::EscalationAnswered`]).
    pub(crate) fn answer_escalation(&mut self, card_id: &str, answer: aui_protocol::Answer, cx: &mut Context<Self>) {
        let Some((task_id, options)) = self.escalations.remove(card_id) else { return };
        let words = match (&answer.other, answer.selected.first()) {
            (Some(text), _) => text.clone(),
            (None, Some(i)) => options.get(*i).cloned().unwrap_or_default(),
            (None, None) => return,
        };
        self.settle_escalation_card(card_id, answer);
        cx.emit(RuntimeEvent::QuestionsChanged);
        cx.emit(SessionEvent::EscalationAnswered { task_id, card_id: card_id.to_owned(), answer: words });
    }

    /// Show the escalation card `card_id` answered (here, or in the other
    /// transcript that carries the same card).
    pub(crate) fn settle_escalation_card(&mut self, card_id: &str, answer: aui_protocol::Answer) {
        self.escalations.remove(card_id);
        let Some(Block::Question { header, prompt, subtitle, options, multi, allow_other, .. }) = self.client_block(card_id) else { return };
        let block = Block::Question { id: card_id.to_owned(), header, prompt, subtitle, options, multi, allow_other, answer: Some(answer), timeout_ms: None };
        self.replace_client_block(card_id, block);
    }
}

impl SessionRuntime {
    /// "Skip": `userInput/cancel`.
    pub fn skip_question(&mut self, block_id: String, cx: &mut Context<Self>) {
        if !self.check_questions_gate(cx) {
            return;
        }
        let Some(pending) = self.provider_question(&block_id) else { return };
        let input_id = pending.input_id.clone();
        let command = ProviderCommand::DismissQuestion {
            request_id: provider::new_request_id(),
            session_id: self.session_id.clone(),
            question: input_id,
            reason: Some("The person declined to answer.".to_owned()),
        };
        self.answers.remove(&pending.input_id);
        let input_id = pending.input_id.clone();
        self.provider_send(command, cx, move |this, result, cx| {
            if let Err(error) = result {
                this.settle_provider_failed(&input_id, &error, cx);
            }
        });
        cx.emit(RuntimeEvent::QuestionsChanged);
    }
}

impl SessionView {
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
        // An escalation's typed answer is the answer (docs/85 14.2).
        if self.runtime.read(cx).escalation_of(block_id).is_some() {
            let content = self.clarify.read(cx).value().trim().to_string();
            if content.is_empty() {
                return;
            }
            self.clarify_open = None;
            self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
            self.runtime.update(cx, |rt, cx| rt.answer_escalation(block_id, aui_protocol::Answer { selected: Vec::new(), other: Some(content) }, cx));
            cx.notify();
            return;
        }
        // The provider lane clarifies with free text instead of the offered
        // options — the "let me explain" path — gated like every question
        // command.
        if !self.runtime.update(cx, |rt, cx| rt.check_questions_gate(cx)) {
            return;
        }
        let Some(pending) = self.runtime.read(cx).provider_question(block_id) else { return };
        let content = self.clarify.read(cx).value().to_string();
        if content.trim().is_empty() {
            return;
        }
        self.clarify_open = None;
        self.clarify.update(cx, |state, cx| state.set_value("", window, cx));
        let command = ProviderCommand::ClarifyQuestion {
            request_id: provider::new_request_id(),
            session_id: self.runtime.read(cx).session_id.clone(),
            question: pending.input_id.clone(),
            text: content,
        };
        let input_id = pending.input_id.clone();
        self.runtime.update(cx, |rt, cx| rt.provider_send(command, cx, move |this, result, cx| {
            if let Err(error) = result {
                this.settle_provider_failed(&input_id, &error, cx);
            }
        }));
        cx.notify();
    }
}

impl SessionRuntime {
    /// The ways a question's settlement can lose, by the provider's hint:
    /// someone already settled it (the settlement is on its way), or the
    /// answer was refused (pick again).
    fn settle_provider_failed(&mut self, input_id: &str, error: &provider::ProviderError, cx: &mut Context<Self>) {
        match error.hint() {
            provider::RefusalHint::AlreadySettled => {}
            provider::RefusalHint::InvalidChoice => {
                self.answers.remove(input_id);
                let agent = self.provider_kind().label();
                self.set_banner(&format!("{agent} refused that answer; pick again."), None, cx);
            }
            _ => self.report_provider_error(error, cx),
        }
    }
}
