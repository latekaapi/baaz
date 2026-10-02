//! The handoff card's view half: refusal checks, card writes, the pack
//! submit and the read-only retirement.
//!
//! Part of [`SessionView`](super::SessionView); the state machine itself
//! lives in [`crate::handoff`], owned by the application. This file only
//! touches this view's own state, which is why it lives here.

use super::*;
use crate::handoff::{self, HandoffRefusal};

impl SessionView {
    /// The workspace root this session runs in: what the pack carries and
    /// what the destination opens in.
    pub(crate) fn workspace_path(&self) -> &str {
        &self.workspace
    }

    /// Whether this view is still running a turn the handoff must quiet.
    pub(crate) fn handoff_turn_running(&self) -> bool {
        self.running.is_some()
    }

    /// Why a handoff cannot start here right now: a pending question or
    /// approval, or a running turn this lane cannot interrupt. `None` means
    /// the run may start. Mirrors
    /// [`crate::handoff::HandoffRun::request`]'s own checks, which stay
    /// authoritative — this is the early read for the confirm dialog.
    pub(crate) fn handoff_blockers(&self) -> Option<HandoffRefusal> {
        let (approvals, questions) = self.waiting_on_you().unwrap_or((0, 0));
        if questions > 0 || !self.external_questions.is_empty() {
            return Some(HandoffRefusal::QuestionPending);
        }
        if approvals > 0 {
            return Some(HandoffRefusal::ApprovalPending);
        }
        if self.running.is_some() && self.turn_gate().is_some() {
            return Some(HandoffRefusal::TurnUninterruptible);
        }
        None
    }

    /// Whether this view is mid-handoff and takes no sends: a card in
    /// Quiescing, Checkpointed or Prepared. Requested just opened the card
    /// and still sends; terminal states never block again.
    pub(crate) fn handoff_quiescing(&self) -> bool {
        match &self.handoff_card {
            Some(Block::Handoff { state, .. }) => matches!(
                state,
                aui_protocol::HandoffState::Quiescing
                    | aui_protocol::HandoffState::Checkpointed
                    | aui_protocol::HandoffState::Prepared
            ),
            _ => false,
        }
    }

    /// The composer's read-only notice once this session retired into a
    /// handoff's destination: "Handed off to <Provider> — open the new
    /// session". `None` while the session takes sends.
    pub(crate) fn handoff_readonly_notice(&self) -> Option<String> {
        let (to, _) = self.handed_off_to.as_ref()?;
        Some(format!("Handed off to {} — open the new session", to.label()))
    }

    /// Append the run's card to the transcript, in a turn of its own — the
    /// same client-authored write the plan card uses.
    pub(crate) fn append_handoff_card(&mut self, card_id: &str, card: Block, cx: &mut Context<Self>) {
        self.handoff_card = Some(card.clone());
        self.fold.append_client_block(&self.session_id, card_id, card);
        self.follow = true;
        cx.notify();
    }

    /// Refresh the card on a transition. A no-op when the card never
    /// landed (a refused request appends nothing).
    pub(crate) fn replace_handoff_card(&mut self, card_id: &str, card: Block, cx: &mut Context<Self>) {
        if self.handoff_card.is_none() {
            return;
        }
        self.handoff_card = Some(card.clone());
        self.fold.replace_client_block(&self.session_id, card_id, card);
        self.follow = true;
        cx.notify();
    }

    /// Retire the composer: sends refuse with the read-only notice from
    /// now on, and the sidebar row keeps the destination.
    pub(crate) fn retire_for_handoff(&mut self, to: ProviderId, destination: String, cx: &mut Context<Self>) {
        self.handed_off_to = Some((to, destination));
        self.banner = self.handoff_readonly_notice();
        self.banner_action = None;
        cx.notify();
    }

    /// Remember the pack's texts when it submits, so the view can hide the
    /// destination's first user turn (the divider stands for it) live and
    /// after a replay, where the bubble shows the summary instead.
    pub(crate) fn note_handoff_pack(&mut self, pack: String, display: String, cx: &mut Context<Self>) {
        self.handoff_pack_full = Some(pack);
        self.handoff_pack_display = Some(display);
        self.refresh_render_cache();
        cx.notify();
    }

    /// Whether the destination already shows the source prefix above its
    /// divider. Landing sets it before the pack submits, so the
    /// acknowledgement never redraws the transcript to add it — the user
    /// perceives only the divider joining turns that were already there.
    pub(crate) fn has_handoff_prefix(&self) -> bool {
        !self.handoff_prefix.is_empty()
    }

    /// The pack turn's terminal on this destination: when the first user
    /// turn matching the submitted pack is followed by an assistant turn
    /// carrying a failure card, that card's reason. The match takes
    /// either pack text — lanes echo the full pack, muse folds the short
    /// display form its wire echoed. Only the pack's own reply counts: a
    /// later turn failing is that turn's business, never the handoff's.
    /// `None` while the pack turn is still running or answered cleanly.
    pub(crate) fn pack_turn_failed_reason(&self) -> Option<String> {
        let (full, display) = match (&self.handoff_pack_full, &self.handoff_pack_display) {
            (Some(full), display) => (full.as_str(), display.as_ref().map(String::as_str)),
            _ => return None,
        };
        let Some(session) = self.fold.session(&self.session_id) else { return None };
        let mut turns = session.turns.iter();
        for turn in turns.by_ref() {
            let is_pack = match turn {
                Turn::User { text, .. } => text == full || display.is_some_and(|d| text == d),
                _ => false,
            };
            if !is_pack {
                continue;
            }
            // The pack submits once: the first user turn matching it is
            // the pack turn, and its own reply is the first assistant
            // turn behind it.
            for following in turns.by_ref() {
                if let Turn::Assistant { blocks, .. } = following {
                    for block in blocks {
                        if let Block::Error { title, detail, .. } = block {
                            let reason = detail.trim();
                            if reason.is_empty() {
                                return Some(title.clone());
                            }
                            return Some(reason.to_owned());
                        }
                    }
                    return None;
                }
            }
            return None;
        }
        None
    }

    /// Show the handoff prefix at activation (or on reopen): the source
    /// transcript's turns above the divider, then this session's own turns.
    /// The caller builds the divider text — the activation divider names the
    /// destination model when known, the fallback divider names the previous
    /// session instead.
    pub(crate) fn show_handoff_prefix(
        &mut self,
        origin: handoff::HandoffOrigin,
        prefix: Vec<Turn>,
        divider_text: String,
        pack_full: Option<String>,
        pack_display: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let to = self.provider_kind();
        let divider = crate::handoff_snapshot::divider_turn(
            &format!("handoff-divider-{}", self.session_id),
            origin.from,
            to,
            divider_text,
        );
        self.handoff_origin = Some(origin);
        self.handoff_prefix = Rc::new(prefix.into_iter().map(Rc::new).collect());
        self.handoff_divider = Some(Rc::new(divider));
        if pack_full.is_some() {
            self.handoff_pack_full = pack_full;
        }
        if pack_display.is_some() {
            self.handoff_pack_display = pack_display;
        }
        self.follow = true;
        self.refresh_render_cache();
        cx.notify();
    }

    /// The source transcript as this view shows it: prefix, divider, own
    /// turns with the pack bubble and its acknowledgement hidden — and never
    /// the handoff card, which is still Prepared here and would freeze that
    /// way in the destination's history (the divider stands for the
    /// handoff). What activation snapshots, so a chain's next hop already
    /// carries this hop's divider. The live source view keeps its card; only
    /// these returned turns leave it out.
    pub(crate) fn handoff_snapshot_turns(&mut self) -> Vec<Turn> {
        self.refresh_render_cache();
        let turns: Vec<Turn> = self.cached_turns.iter().map(|turn| (**turn).clone()).collect();
        crate::handoff_snapshot::without_handoff_cards(turns)
    }

    /// A pack-derived bubble text from before snapshots existed: the display
    /// map's value whose full text starts with the pack header. What hides
    /// the pack bubble for a pair made before this change, where no snapshot
    /// names the pack.
    pub(crate) fn pack_display_fallback(&mut self) -> Option<String> {
        self.ensure_display_overrides();
        self.display_overrides.as_ref().and_then(|map| {
            map.iter()
                .find(|(full, _)| full.starts_with(crate::handoff_snapshot::PACK_HEAD_PREFIX))
                .map(|(_, display)| display.clone())
        })
    }

    /// Submit the pack as the destination's first turn: the full pack text
    /// goes to the model, the user bubble shows only the short summary.
    /// Both lanes; the ack comes back as [`SessionEvent::HandoffPackAccepted`]
    /// (or [`SessionEvent::HandoffPackFailed`]), never through the generic
    /// turn-accepted paths, so only the pack's own ack can acknowledge the run.
    pub(crate) fn submit_pack(&mut self, pack: String, display: String, cx: &mut Context<Self>) {
        if self.is_provider_lane() {
            self.submit_pack_on_provider(pack, display, cx);
            return;
        }
        self.submit_pack_on_muse(pack, display, cx);
    }

    /// The provider-lane half of [`Self::submit_pack`]: one `SubmitInput`
    /// with the pack as its text part and the short summary as
    /// `display_text`. No attachments ride a pack — it is text by
    /// construction (see [`crate::handoff`]).
    fn submit_pack_on_provider(&mut self, pack: String, display: String, cx: &mut Context<Self>) {
        self.banner = None;
        self.submitting = true;
        let request_id = new_command_id();
        self.fold.record_command(&self.session_id, &request_id, &display);
        // The provider echoes the whole pack; the bubble shows the short
        // summary, including across a restart (the record mirrors the map
        // for the replayed history).
        self.remember_display_text(pack.clone(), display.clone());
        let command = ProviderCommand::SubmitInput {
            request_id,
            session_id: self.session_id.clone(),
            parts: vec![provider::SubmissionPart::Text(pack)],
            display_text: Some(display),
            effort: crate::projects::effort_string(self.effort),
        };
        let session_id = self.session_id.clone();
        self.provider_send(command, cx, move |this, result, cx| {
            match result {
                Ok(provider::Ack::TurnAccepted { .. }) => {
                    // X1b: busy until the provider speaks, like a typed
                    // send — the ack only admits the pack, so `submitting`
                    // stays true until the first event hands over to
                    // `running` exactly as before. The pack folds no
                    // optimistic bubble, so its echo lands the one user
                    // turn (the short summary) with nothing to duplicate.
                    this.adopt_open_provider_turn(cx);
                    cx.emit(crate::session::SessionEvent::HandoffPackAccepted { session_id });
                }
                Ok(_) => {
                    // No turn carries the pack, so nothing will ever speak:
                    // idle here rather than sticking busy.
                    this.submitting = false;
                    cx.emit(crate::session::SessionEvent::HandoffPackFailed {
                        session_id,
                        reason: "the provider answered the pack with no turn".to_owned(),
                    });
                }
                Err(error) => {
                    // The pack never left: idle, and say why.
                    this.submitting = false;
                    cx.emit(crate::session::SessionEvent::HandoffPackFailed {
                        session_id,
                        reason: error.to_string(),
                    });
                }
            }
            cx.notify();
        });
        cx.notify();
    }

    /// The muse-lane half of [`Self::submit_pack`]: `turn/start` with the
    /// pack as input and the short summary as `display_text`.
    fn submit_pack_on_muse(&mut self, pack: String, display: String, cx: &mut Context<Self>) {
        if self.wire_client(cx).is_none() {
            cx.emit(crate::session::SessionEvent::HandoffPackFailed {
                session_id: self.session_id.clone(),
                reason: "no connection to start the destination turn".to_owned(),
            });
            return;
        }
        self.banner = None;
        self.submitting = true;
        let command_id = new_command_id();
        self.fold.record_command(&self.session_id, &command_id, &display);
        let params = TurnStartParams {
            command_id,
            session_id: self.session_id.clone(),
            input: self.parts(pack),
            display_text: Some(display),
            reasoning_effort: self.effort.map(effort_wire),
            ..Default::default()
        };
        let Some(client) = self.wire_client(cx) else { return };
        let session_id = self.session_id.clone();
        self.wire_call(cx, move || client.turn_start(&params), move |this, result, cx| {
            this.submitting = false;
            match result {
                Ok(_) => {
                    cx.emit(crate::session::SessionEvent::HandoffPackAccepted { session_id });
                }
                Err(error) => {
                    cx.emit(crate::session::SessionEvent::HandoffPackFailed {
                        session_id,
                        reason: error.to_string(),
                    });
                }
            }
        });
        cx.notify();
    }

    /// `handoff:<provider>`: the step verb for live runs. The same
    /// [`SessionView::pick_provider`] path the menu uses, forced onto the
    /// handoff row, and headless — the command line already asked, so no
    /// confirm dialog. Unknown ids record a step failure and change
    /// nothing, like `setprovider:`.
    pub(crate) fn step_handoff(&mut self, rest: &str, cx: &mut Context<Self>) {
        let id = rest.trim();
        let Some(picked) = ProviderId::all().iter().find(|p| p.as_str() == id).copied() else {
            crate::steps::record_step_failure(&format!("handoff:{rest}"));
            crate::baaz_log!("unknown provider `{rest}`; known providers are muse, claude-code, codex");
            return;
        };
        if picked == self.provider_kind() {
            crate::steps::record_step_failure(&format!("handoff:{rest}"));
            crate::baaz_log!("`handoff:{id}` names this session's own provider: a model change, not a handoff");
            return;
        }
        if !self.has_turns() {
            crate::steps::record_step_failure(&format!("handoff:{rest}"));
            crate::baaz_log!("`handoff:{id}` needs turns to carry: this session has none");
            return;
        }
        cx.emit(SessionEvent::HandoffRequested { provider: picked, headless: true });
    }
}
