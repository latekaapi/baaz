//! The two session-level operations that are neither a turn nor an
//! answer: `session/userShell` and `session/fork`.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ------------------------------------------------------------ user shell

    /// A draft starting with `!` is a shell command, not a turn (research
    /// §1.12).
    ///
    /// It is also the free way to raise a real approval, and free here means
    /// what it says: a user shell command is run by the **server**, not by the
    /// model, so no provider turn is started and nothing is billed on any
    /// provider. (`echo` itself is not a free provider — see the `main.rs`
    /// header — but this path never reaches one.) Under `promptUnmatched`,
    /// `!echo hi && ls` raises the two-stage approval that
    /// `fixtures/msp/transcript-approve.jsonl` records.
    pub fn run_user_shell(&mut self, command_text: String, cx: &mut Context<Self>) {
        if !self.user_shell {
            self.set_banner("Muse did not grant this build the userShell capability.", None, cx);
            return;
        }
        let Some(client) = self.wire_client(cx) else { return };
        // The lease is gone: the shell would run with no view to stream
        // back to. Refused like a send, with the notice re-shown.
        if let Some(notice) = self.lease_notice.clone() {
            self.banner = Some(notice);
            self.banner_action = None;
            cx.notify();
            return;
        }
        self.banner = None;
        self.banner_action = None;
        let command_id = new_command_id();
        // The shell item is filed under its own `commandId`, and so is any
        // approval it raises, so remembering the text here is what makes the
        // retry offer honest.
        self.fold.record_command(&self.session_id, &command_id, &format!("!{command_text}"));
        let params = SessionUserShellParams {
            command_id,
            command_text: command_text.clone(),
            session_id: self.session_id.clone(),
        };
        self.wire_call(cx, move || client.session_user_shell(&params), move |this, result, cx| {
            if let Err(error) = result {
                this.report_retryable(&error, BannerAction::RetryShell(command_text.clone()), cx);
            }
        });
        cx.notify();
    }

    // ------------------------------------------------------------------ fork

    /// `/fork`, and an assistant turn's "Fork from here".
    ///
    /// The cut point is a **turn id** of a completed turn: naming an in-progress
    /// one is `forkBoundaryInvalid`. Invoked from `/fork` with nothing named, it
    /// is the newest completed turn.
    pub fn fork(&mut self, last_turn_id: Option<String>, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        let cut_point =
            last_turn_id.or_else(|| self.newest_completed_turn()).map(|last_turn_id| ForkCutPoint { last_turn_id });
        let params = SessionForkParams {
            command_id: new_command_id(),
            cut_point,
            // The fork's history comes through `view/page`, like every other
            // attach in this app.
            exclude_items: Some(true),
            session_id: self.session_id.clone(),
        };
        self.wire_call(cx, move || client.session_fork(&params), |this, result, cx| match result {
            Ok(forked) => cx.emit(SessionEvent::Forked {
                session_id: forked.session.session_id.clone(),
                session: serde_json::to_value(&forked.session).unwrap_or_default(),
            }),
            Err(error) if error.kind() == Some(&ErrorKind::ForkBoundaryInvalid) => {
                this.set_banner("That turn is still running, so there is nothing to fork from yet.", None, cx);
            }
            Err(error) => this.report(&error, cx),
        });
    }

    /// The completed assistant turns, newest first: the rows of the `/fork`
    /// picker. Each row is the turn id, the first line of the user prompt that
    /// started the turn, and the turn's wall-clock time.
    ///
    /// The filter is the same one a fork may name: no running turn (naming an
    /// in-progress one is `forkBoundaryInvalid`), no client-authored marker or
    /// plan turn. A `Turn::User`'s id is the message item's, not a turn id, so
    /// user turns only lend their text to the row that follows them.
    pub fn fork_turns(&self) -> Vec<(String, String, String)> {
        let Some(session) = self.session() else { return Vec::new() };
        let running = self.running.as_ref().map(|r| r.turn_id.as_str());
        let mut prompt = String::new();
        let mut rows = Vec::new();
        for turn in session.turns.iter() {
            match turn {
                aui_protocol::Turn::User { text, .. } => prompt = text.clone(),
                aui_protocol::Turn::Assistant { id, blocks, meta } => {
                    if Some(id.as_str()) == running {
                        continue;
                    }
                    // The client authors two kinds of turn of its own — marker rows and
                    // plan cards — and neither is a turn the server could fork at.
                    if id.starts_with("marker:") || id.starts_with("plan-") {
                        continue;
                    }
                    rows.push((id.clone(), fork_label(&prompt, blocks), fork_time(meta)));
                }
            }
        }
        rows.reverse();
        rows
    }

    /// The nth newest completed turn, 1-based: what `/fork <n>` names.
    pub(super) fn nth_completed_turn(&self, n: usize) -> Option<String> {
        self.fork_turns().into_iter().nth(n.saturating_sub(1)).map(|(id, _, _)| id)
    }

    /// The newest turn the server has finished, which is the only boundary a
    /// fork may name.
    pub(super) fn newest_completed_turn(&self) -> Option<String> {
        self.nth_completed_turn(1)
    }

    /// `/fork <n>`: fork the nth newest completed turn with no picker. A
    /// number with no turn behind it is a banner, never a fork of whatever the
    /// server thinks is newest.
    pub(super) fn fork_nth(&mut self, n: usize, cx: &mut Context<Self>) {
        match self.nth_completed_turn(n) {
            Some(last_turn_id) => self.fork(Some(last_turn_id), cx),
            None => {
                self.set_banner(&format!("There is no completed turn #{n} to fork from yet."), None, cx);
            }
        }
    }
}
