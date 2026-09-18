//! Approvals: the decision a server-minted choice makes, the full
//! output a settled shell card can then fetch, and the retry a failed turn
//! offers.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    /// A server-minted choice was pressed: `approval/decide`.
    ///
    /// The `requirementId` is the guard MSP requires: it must equal the
    /// approval's **current** stage token, or the wire answers
    /// `approvalRequirementStale`. It is read from the pending request the fold
    /// keeps rather than from the card, because the block has no room for it and
    /// the choices change between stages.
    pub fn decide_approval(&mut self, approval_id: String, choice_id: String, feedback: Option<String>, cx: &mut Context<Self>) {
        let Some(requirement_id) = self
            .fold
            .side(&self.session_id)
            .and_then(|side| side.pending_approvals.get(&approval_id))
            .map(|request| request.current_requirement_id.clone())
        else {
            // Nothing pending under that id: the resolution already landed.
            // Ordinary, but not silent — a press that reaches here and says
            // nothing is indistinguishable from a press that never arrived,
            // which is precisely what made the "buttons do nothing" report
            // undiagnosable (D1).
            crate::baaz_log!("approval {approval_id}: nothing pending under that id; the resolution already landed");
            return;
        };
        let Some(client) = self.wire_client(cx) else {
            // `wire_client` raises the read-only banner on a replay; on a live
            // session with no client there is nothing on screen at all, so say
            // it here.
            crate::baaz_log!("approval {approval_id}: no wire to decide on");
            return;
        };
        // Every decision is logged as it goes out, so the wire log and the app
        // log agree about whether a press was ever turned into a call.
        crate::baaz_log!("approval {approval_id}: deciding {choice_id}");
        self.feedback_open = None;
        let params = ApprovalDecideParams {
            approval_id: approval_id.clone(),
            choice_id,
            command_id: new_command_id(),
            feedback,
            requirement_id,
            session_id: self.session_id.clone(),
        };
        self.wire_call(cx, move || client.approval_decide(&params), move |this, result, cx| {
            // The ack's `terminal` flag is admission only: what the card
            // shows next comes from `approval/updated` or
            // `approval/resolved`, never from here.
            if let Err(error) = result {
                this.decide_failed(&approval_id, &error, cx);
            }
        });
        cx.notify();
    }

    /// The four ways `approval/decide` can lose (research §1.14).
    pub(super) fn decide_failed(&mut self, approval_id: &str, error: &MuseError, cx: &mut Context<Self>) {
        match error.kind() {
            // Somebody else — a policy, the judge, another window — got there
            // first, and the error carries the winning resolution.
            Some(ErrorKind::ApprovalAlreadyResolved) => {
                self.fold.resolve_approval(&self.session_id, approval_id, resolution_of(error));
                self.follow = true;
                cx.notify();
            }
            // The choices moved under the press, which only happens between
            // stages; the update that moved them is already on its way, so
            // the person needs no banner. It still goes to the log: a press
            // that loses this race looks exactly like a press that did
            // nothing, and without a line here there is no way to tell the
            // two apart after the fact (D1).
            Some(ErrorKind::ApprovalRequirementStale) => {
                crate::baaz_log!("approval {approval_id}: the choices moved under the press; waiting for the update that moved them");
            }
            Some(ErrorKind::ApprovalChoiceInvalid) => {
                self.set_banner("That choice is no longer offered for this command.", None, cx);
            }
            Some(ErrorKind::ApprovalNotFound) => {
                self.fold.resolve_approval(&self.session_id, approval_id, None);
                self.follow = true;
                self.set_banner("Muse no longer knows about that approval.", None, cx);
            }
            _ => self.report(error, cx),
        }
    }

    /// Open (or close) the feedback field a choice asks for.
    pub fn toggle_feedback(&mut self, approval_id: String, choice_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback_open = choice_id.map(|choice| (approval_id, choice));
        self.feedback.update(cx, |state, cx| state.set_value("", window, cx));
        if self.feedback_open.is_some() {
            window.focus(&self.feedback.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Enter in an open feedback or clarification field; `false` when neither is
    /// open, so the caller can let the key mean what it usually means.
    pub fn confirm_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some((approval_id, choice_id)) = self.feedback_open.clone() {
            let text = self.feedback.read(cx).value().to_string();
            let feedback = (!text.trim().is_empty()).then_some(text);
            self.decide_approval(approval_id, choice_id, feedback, cx);
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

    // ---------------------------------------------------------- full output

    /// "Show full output" on a truncated tool card: page `item/readOutput` on
    /// a background task (02-app §3 — a page can block) and replace the card's
    /// body on the server's result (D4). A second press while pages are still
    /// arriving does nothing; a failed fetch reports its banner and leaves the
    /// truncated body alone.
    pub fn show_full_output(&mut self, block_id: String, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        let Some(output_ref) = self.fold.stored_output(&self.session_id, &block_id).cloned()
        else {
            return;
        };
        if matches!(self.full_outputs.get(&block_id), Some(full_output::Fetch::Fetching)) {
            return;
        }
        self.full_outputs.insert(block_id.clone(), full_output::Fetch::Fetching);
        self.refresh_render_cache();
        cx.notify();
        let session_id = self.session_id.clone();
        let output_ref = output_ref.id.clone();
        let fetch_id = block_id.clone();
        // Every MSP request can block, so the pages run here and the card is
        // replaced below, on the server's result.
        let work = move || full_output::fetch_full_output(&client, &session_id, &fetch_id, &output_ref);
        self.wire_call(cx, work, move |this, result, cx| match result {
            Ok(fetched) => {
                this.full_outputs
                    .insert(block_id, full_output::Fetch::Ready { lines: fetched.lines, capped: fetched.capped });
                this.refresh_render_cache();
                cx.notify();
            }
            Err(error) => {
                this.full_outputs.remove(&block_id);
                this.report(&error, cx);
            }
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
        self.fold.side(&self.session_id)?.command_text.get(turn_id).cloned()
    }

    /// Which failed turns the retry button may be offered on.
    pub(super) fn retryable_turns(&self) -> HashSet<String> {
        self.fold
            .side(&self.session_id)
            .map(|side| side.command_text.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// A failure the person can try again, with the button that would do it.
    ///
    /// `overloaded` and `backpressured` also retry themselves once, after the
    /// backoff: they are the wire saying "not now", and "not now" deserves one
    /// unattended attempt before it deserves a person's attention.
    pub(super) fn report_retryable(&mut self, error: &MuseError, action: BannerAction, cx: &mut Context<Self>) {
        let auto = matches!(error.kind(), Some(ErrorKind::Overloaded | ErrorKind::Backpressured));
        self.set_banner(&format!("{}. {error}", conn::title(error)), Some(action.clone()), cx);
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
            None => cx.notify(),
        }
    }

    /// One place that writes the banner, so its message and its action can never
    /// disagree.
    pub(super) fn set_banner(&mut self, message: &str, action: Option<BannerAction>, cx: &mut Context<Self>) {
        self.banner = Some(message.to_owned());
        self.banner_action = action;
        cx.notify();
    }
}
