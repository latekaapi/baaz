//! The two session-level operations that are neither a turn nor an
//! answer: `session/userShell` and `session/fork`.
//!
//! Part of [`SessionRuntime`]; see [`crate::session`] for what the
//! entity owns and why these are its own files.

use super::*;

impl SessionRuntime {
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
        // The provider lane runs the shell command out-of-turn through the
        // lane, gated by the registry mirror: Claude Code is `Native`,
        // Codex `Unverified` (attempted, errors surfaced).
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::SessionShell)
            .allows_attempt()
        {
            let reason = crate::providers::gate(self.provider_kind(), provider::Capability::SessionShell)
                .unwrap_or_else(|| "Shell commands are not available on this provider.".into());
            self.plain_banner(reason, cx);
            return;
        }
        if let Some(notice) = self.lease_notice.clone() {
            self.plain_banner(notice, cx);
            return;
        }
        self.banner = None;
        self.banner_action = None;
        let command_id = provider::new_request_id();
        self.remember_command(&command_id, &format!("!{command_text}"));
        let command = ProviderCommand::RunShell {
            request_id: command_id,
            session_id: self.session_id.clone(),
            command: command_text.clone(),
        };
        let retry = command_text.clone();
        self.provider_send(command, cx, move |this, result, cx| {
            if let Err(error) = result {
                this.report_provider_retryable(&error, BannerAction::RetryShell(retry), cx);
            }
        });
        cx.emit(RuntimeEvent::TurnChanged);
    }

    // ------------------------------------------------------------------ fork

    /// `/fork`, and an assistant turn's "Fork from here".
    ///
    /// The cut point is a **turn id** of a completed turn: naming an in-progress
    /// one is `forkBoundaryInvalid`. Invoked from `/fork` with nothing named, it
    /// is the newest completed turn.
    pub fn fork(&mut self, last_turn_id: Option<String>, cx: &mut Context<Self>) {
        // The provider lane forks through the seam: `ForkSession` where
        // the capability is anything but `Unavailable`, the fork opening
        // as a new provider-lane view on the same provider — and a refusal
        // banners its reason rather than going quiet.
        if !crate::providers::capability_state(self.provider_kind(), provider::Capability::ForkSession)
            .allows_attempt()
        {
            let reason = crate::providers::gate(
                self.provider_kind(),
                provider::Capability::ForkSession,
            )
            .unwrap_or_else(|| "Forking is not available on this provider.".into());
            self.plain_banner(reason, cx);
            return;
        }
        let through_turn =
            last_turn_id.or_else(|| self.newest_completed_turn());
        let command = ProviderCommand::ForkSession {
            request_id: provider::new_request_id(),
            session_id: self.session_id.clone(),
            through_turn,
            metadata_only: false,
            mode: Some(self.mode()),
        };
        let provider = self.provider_kind();
        self.provider_send(command, cx, move |this, result, cx| match result {
            Ok(provider::Ack::Session { session_id, .. }) => {
                cx.emit(SessionEvent::ForkedOnProvider { session_id, provider });
            }
            Ok(_) => {
                this.set_banner("The fork answered without a session.", None, cx);
            }
            Err(error) if error.hint() == provider::RefusalHint::BadBoundary => {
                this.set_banner("That turn is still running, so there is nothing to fork from yet.", None, cx);
            }
            Err(error) => this.report_provider_error(&error, cx),
        });
    }

    /// The newest user message's turn id (captures, docs/85 16.3).
    #[cfg(feature = "probe")]
    pub(crate) fn last_user_turn_id(&self) -> Option<String> {
        self.session()?.turns.iter().rev().find_map(|t| match t {
            aui_protocol::Turn::User { id, .. } => Some(id.clone()),
            _ => None,
        })
    }

    /// docs/85 16.3, "Branch from here" on the user message `message`: a
    /// fork through that message's answer (the assistant turn after it),
    /// opened as its own session beside this one; the app records the
    /// branch and nests it under this session. A message with no finished
    /// answer yet says so.
    pub(crate) fn branch_from(&mut self, message: &str, cx: &mut Context<Self>) {
        // Rule 13: a fork that copies the whole session is not a branch.
        if !self.provider_kind().has(provider::ProviderTrait::ForkAtTurn) {
            self.plain_banner(format!("{} cannot branch from a message yet.", self.provider_kind().label()), cx);
            return;
        }
        let answer = self.session().and_then(|session| {
            let at = session.turns.iter().position(|t| matches!(t, aui_protocol::Turn::User { id, .. } if id == message))?;
            session.turns[at + 1..].iter().find_map(|t| match t {
                aui_protocol::Turn::Assistant { id, .. } => Some(id.clone()),
                _ => None,
            })
        });
        let running = self.running.as_ref().map(|r| r.turn_id.clone());
        let Some(through) = answer.filter(|id| Some(id) != running.as_ref()) else {
            self.plain_banner("This message has no finished answer yet, so there is nothing to branch from.".to_owned(), cx);
            return;
        };
        let command = ProviderCommand::ForkSession {
            request_id: provider::new_request_id(),
            session_id: self.session_id.clone(),
            through_turn: Some(through),
            metadata_only: false,
            mode: Some(self.mode()),
        };
        let (provider, source, message) = (self.provider_kind(), self.session_id.clone(), message.to_owned());
        self.provider_send(command, cx, move |this, result, cx| match result {
            Ok(provider::Ack::Session { session_id, .. }) => {
                *this.branch_counts.entry(message.clone()).or_insert(0) += 1;
                cx.emit(SessionEvent::Branched { branch: session_id.clone(), source: source.clone(), message: message.clone() });
                cx.emit(SessionEvent::ForkedOnProvider { session_id, provider });
                cx.emit(RuntimeEvent::BannerChanged);
            }
            Ok(_) => this.set_banner("The branch answered without a session.", None, cx),
            Err(error) => this.report_provider_error(&error, cx),
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
                aui_protocol::Turn::Assistant { id, blocks, meta, .. } => {
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
