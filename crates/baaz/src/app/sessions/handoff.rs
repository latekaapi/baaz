//! Handoff between providers (H2), the application half: the confirm
//! dialog, opening the destination, acknowledging it and failing or
//! cancelling a run.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// Handoff between providers (H2): the application half. The machine
/// itself is [`crate::handoff::HandoffRun`]; here the runs are owned
/// (keyed by source session), the confirm dialog is shown, destinations
/// are opened, and acks advance the run to activation.
impl Harness {
    /// "Hand off to X…" with a confirm first: freeze the pack preview and
    /// show [`handoff_confirm`](aui::transcript::handoff_confirm). A
    /// session with no turns, or a pending question / approval / an
    /// uninterruptible turn, never reaches the dialog — the refusal lands
    /// on the transcript as a Refused card instead.
    pub(in crate::app) fn show_handoff_confirm(&mut self, source: String, to: ProviderId, cx: &mut Context<Self>) {
        let Some(view) = self.find_view(&source, cx) else { return };
        // The preview the dialog draws, frozen before it opens: `None`
        // when the session holds no turns to carry.
        let preview = {
            let view = view.read(cx);
            let session = view.session();
            let session = match session {
                Some(session) if !session.turns.is_empty() => Some(session),
                _ => None,
            };
            session.map(|session| {
                let pack = crate::handoff::build_pack(session, view.workspace_path());
                let carried = crate::handoff::carried_items(&pack);
                (view.provider_kind(), view.model_id(), view.handoff_blockers(), pack.tokens, carried)
            })
        };
        let Some((from, from_model, blockers, pack_tokens, carried)) = preview else {
            crate::baaz_log!("handoff refused {source}: nothing to hand off yet");
            return;
        };
        if let Some(refusal) = blockers {
            let epoch = self.handoff_state.next_epoch();
            let run = crate::handoff::HandoffRun::refused(
                source.clone(),
                epoch,
                from,
                to,
                from_model,
                refusal,
            );
            let card = run.card();
            let card_id = run.card_id.clone();
            self.handoff_state.runs.insert(source, run);
            view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
            return;
        }
        self.handoff_state.confirm = Some(crate::handoff::HandoffConfirmState {
            source_session: source,
            to,
            to_model: String::new(),
            carried,
            lost: crate::handoff::lost_items(),
            pack_tokens,
        });
        self.set_dialog(
            cx,
            Dialog {
                title: format!("Hand off to {}?", to.label()),
                detail: String::new(),
                kind: DialogKind::Warning,
                primary: "Hand off",
                action: DialogAction::HandoffConfirm,
                archive_target: None,
            },
        );
    }

    /// The confirm dialog's "Hand off": close it and start the run. The
    /// frozen facts are taken, so dismissing any other way confirms
    /// nothing afterwards.
    pub(crate) fn confirm_handoff(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(confirm) = self.handoff_state.confirm.take() else {
            self.close_dialog(cx);
            return;
        };
        let (source, to) = (confirm.source_session.clone(), confirm.to);
        self.close_dialog(cx);
        self.start_handoff(source, to, window, cx);
    }

    /// Run the machine from Requested to Prepared, then open the
    /// destination: interrupt a running turn, checkpoint the pack, and
    /// submit it as the fresh session's first turn when it lands.
    /// Failures at any step fail the run with the reason; the source
    /// stays usable.
    pub(crate) fn start_handoff(
        &mut self,
        source: String,
        to: ProviderId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Starting a handoff while a provider switch is pending cancels
        // the switch; the destination then opens normally.
        self.cancel_pending_switch(cx);
        let Some(view) = self.find_view(&source, cx) else { return };
        // A newer request supersedes an in-flight one for the same source:
        // the old run aborts (its destination shuts down when opened) and
        // the new epoch owns every later event.
        if let Some(mut old) = self.handoff_state.runs.remove(&source) {
            if old.cancellable() {
                let opened = old.cancel();
                if opened {
                    if let Some(dest) = old.destination_session.clone() {
                        self.clear_handoff_links(&source, &dest, cx);
                        self.close_view(&dest, cx);
                    }
                    self.handoff_state.drop_pending_for(&source);
                }
                let card = old.card();
                let card_id = old.card_id.clone();
                view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
            }
        }
        let epoch = self.handoff_state.next_epoch();
        // The facts the run needs, read off the source view at once.
        struct Facts {
            from: ProviderId,
            from_model: String,
            workspace: String,
            running: bool,
            has_turns: bool,
            blockers: Option<crate::handoff::HandoffRefusal>,
            pack: Option<crate::handoff::ContextPack>,
        }
        let facts = {
            let view = view.read(cx);
            let session = view.session();
            Facts {
                from: view.provider_kind(),
                from_model: view.model_id(),
                workspace: view.workspace_path().to_owned(),
                running: view.handoff_turn_running(),
                has_turns: view.has_turns(),
                blockers: view.handoff_blockers(),
                pack: session.map(|s| crate::handoff::build_pack(s, view.workspace_path())),
            }
        };
        let (question, approval, uninterruptible) = match facts.blockers {
            Some(crate::handoff::HandoffRefusal::QuestionPending) => (true, false, false),
            Some(crate::handoff::HandoffRefusal::ApprovalPending) => (false, true, false),
            Some(crate::handoff::HandoffRefusal::TurnUninterruptible) => (false, false, true),
            Some(crate::handoff::HandoffRefusal::SameProvider) | None => (false, false, false),
        };
        let mut run = match crate::handoff::HandoffRun::request(
            source.clone(),
            epoch,
            facts.from,
            to,
            facts.from_model.clone(),
            String::new(),
            question,
            approval,
            uninterruptible,
        ) {
            Ok(run) => run,
            Err(refusal) => {
                let refused = crate::handoff::HandoffRun::refused(
                    source.clone(),
                    epoch,
                    facts.from,
                    to,
                    facts.from_model,
                    refusal,
                );
                let card = refused.card();
                let card_id = refused.card_id.clone();
                self.handoff_state.runs.insert(source, refused);
                view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
                return;
            }
        };
        let Some(pack) = facts.pack.filter(|_| facts.has_turns) else {
            run.fail("nothing to hand off — the session has no turns".to_owned());
            let card = run.card();
            let card_id = run.card_id.clone();
            self.handoff_state.runs.insert(source, run);
            view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
            return;
        };
        let card_id = run.card_id.clone();
        self.handoff_state.runs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.append_handoff_card(&card_id, run.card(), cx));
        // Quiescing: a running turn is interrupted, no new sends accepted
        // (the composer refuses while the card reads Quiescing).
        if facts.running {
            view.update(cx, |view, cx| view.interrupt(cx));
        }
        run.note_quiescing();
        self.handoff_state.runs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.replace_handoff_card(&card_id, run.card(), cx));
        // Checkpointed: the pack is a point-in-time capture of the folded
        // transcript — an interrupt's late deltas may land just after it.
        run.note_checkpointed(pack);
        self.handoff_state.runs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.replace_handoff_card(&card_id, run.card(), cx));
        // The model summary earns a side session only when it could add
        // something over the extractive pack (see
        // [`crate::handoff::should_model_summary`]) — and it never delays
        // the destination: the run opens below with the extractive pack
        // either way. Landing stands the wait down (the submitted pack is
        // already fixed), so a summary arriving later is dropped, never
        // applied. Cancel during the wait abandons the side session; a
        // late answer lands on nothing.
        let summary_prompt = self
            .handoff_state.runs
            .get(&source)
            .and_then(|run| run.pack.as_ref())
            .filter(|pack| {
                crate::handoff::should_model_summary(
                    crate::layout::Layout::get(cx).handoff_model_summary,
                    self.utility_available(self.session_lane_provider(&source, cx)),
                    pack,
                )
            })
            .map(|pack| crate::handoff::summary_prompt(&crate::handoff::summary_input(pack)));
        if let Some(prompt) = summary_prompt {
            if let Some(run) = self.handoff_state.runs.get_mut(&source) {
                run.note_summary_pending();
            }
            if let Some(run) = self.handoff_state.runs.get(&source) {
                let card = run.card();
                let card_id = run.card_id.clone();
                view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
            }
            crate::baaz_log!("handoff requested epoch={epoch} {source} -> {} (summarising)", to.as_str());
            self.start_handoff_summary(source.clone(), epoch, prompt, cx);
        }
        crate::baaz_log!("handoff requested epoch={epoch} {source} -> {}", to.as_str());
        // Prepared next: the destination opens in the source's workspace,
        // and the pack submits when it lands. The source's visible turns
        // ride on the pending handoff, so landing applies them before the
        // destination's first render — no frame shows the empty
        // destination waiting for the pack's acknowledgement.
        // The current step's elapsed counter ticks off this refresh: once
        // a second while the run is in flight, re-armed only while live —
        // settled runs own no timer.
        self.arm_handoff_tick(source.clone(), epoch, cx);
        let prefix = view.update(cx, |view, cx| view.handoff_snapshot_turns(cx));
        self.handoff_state.pending =
            Some(crate::handoff::PendingHandoff { source_session: source, epoch, prefix });
        // Every destination opens the same way (docs/70): on its provider's
        // lane, in the source's workspace — a shared child started first.
        crate::baaz_log!("handoff from {} opens its destination on {}", run.source_session, to.as_str());
        self.ensure_provider_started(to, cx);
        let workspace = facts.workspace.clone();
        let project = self.session_list.read(cx).projects.resolve_available(Some(&workspace), None).map(|p| p.id.clone());
        self.open_on_provider(to, project, workspace, window, cx);
    }

    /// Carry the source's pane state onto a handoff destination that holds
    /// none (B7fix2): through the cheap debounced persist — never the
    /// session-list settle, so the landing rejoins no rows and rebuilds no
    /// search index. The restore that follows flushes the armed write, so
    /// the carried state still reaches `sessions.json` synchronously.
    pub(super) fn carry_right_state_for_handoff(
        &mut self,
        dest: &str,
        state: crate::sessions::RightState,
        cx: &mut Context<Self>,
    ) {
        self.store_right_state_cheap(dest, state, cx);
        self.restore_right_for_session(dest, cx);
    }

    /// The fresh session landed: when it is a handoff's destination under
    /// the current epoch, mark Prepared, show the source prefix above the
    /// divider before the first render, link the chain, and submit the
    /// pack as its first turn. Landing does everything activation does
    /// except the run-state change, so the acknowledgement adds no frame
    /// of its own. Anything else (an ordinary open, a superseded request)
    /// is left alone.
    pub(in crate::app) fn land_handoff_destination(&mut self, dest: String, cx: &mut Context<Self>) {
        let pending = self.handoff_state.pending.clone();
        let Some(pending) = pending else { return };
        let Some(run) = self.handoff_state.runs.get(&pending.source_session).cloned() else {
            self.handoff_state.pending = None;
            return;
        };
        if run.epoch != pending.epoch || !matches!(run.state, aui_protocol::HandoffState::Checkpointed) {
            return;
        }
        let Some(view) = self.find_view(&dest, cx) else { return };
        // The landing owns this run only on the requested lane: an
        // ordinary open racing the handoff keeps its session.
        let (lane, model) = {
            let view = view.read(cx);
            (view.provider_kind(), view.model_id())
        };
        if lane != run.to {
            return;
        }
        let mut run = run;
        run.note_prepared(dest.clone());
        // Landing stands the summary wait down (the submitted pack is
        // already the extractive one): a late answer lands on nothing.
        // The destination names its model once the lane reports it; until
        // then the card keeps the provider default the open used.
        if !model.is_empty() {
            run.to_model = model;
        }
        let pack = run.pack.clone();
        self.handoff_state.runs.insert(pending.source_session.clone(), run.clone());
        self.handoff_state.pending = None;
        if let Some(source) = self.find_view(&pending.source_session, cx) {
            let card = run.card();
            let card_id = run.card_id.clone();
            source.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        // Z2: the handoff swaps session ids, so the pane state moves with
        // it — the destination inherits what the source left, unless it
        // already holds its own. The destination activated (and restored
        // closed) before landing here, so restore again to show it.
        let carried = self.session_list.read(cx).overrides.get(&pending.source_session).and_then(|meta| meta.right.clone());
        if let Some(state) = carried {
            let existing = self.session_list.read(cx).overrides.get(&dest).and_then(|meta| meta.right.clone());
            if existing.is_none() {
                self.carry_right_state_for_handoff(&dest, state, cx);
            }
        }
        let Some(pack) = pack else {
            self.fail_handoff(dest, "the context pack was never built".to_owned(), cx);
            return;
        };
        let origin = crate::handoff::HandoffOrigin {
            source_session: pending.source_session.clone(),
            from: run.from,
            from_model: run.from_model.clone(),
        };
        // The prefix the destination renders from its first frame: stashed
        // at request time, or snapshot live when the stash is empty (a
        // pending from before the stash existed). Either way it lands
        // before the pack submits — no frame shows the empty destination.
        let mut prefix = pending.prefix.clone();
        if prefix.is_empty() {
            if let Some(source) = self.find_view(&pending.source_session, cx) {
                prefix = source.update(cx, |view, cx| view.handoff_snapshot_turns(cx));
            }
        }
        let text = crate::handoff::pack_text(&pack, run.from);
        let display = crate::handoff::display_text(&pack, run.from);
        let divider = crate::handoff_snapshot::divider_text(
            run.from,
            run.to,
            (!run.to_model.is_empty()).then(|| run.to_model.clone()).as_deref(),
            Some(pack.recent.len()),
        );
        view.update(cx, |view, cx| {
            view.show_handoff_prefix(
                origin,
                prefix,
                divider,
                Some(text.clone()),
                Some(display.clone()),
                cx,
            );
        });
        // The links land before the rows rebuild, so the sidebar row joins
        // the chain in this same frame — and the title guard already sees
        // a handoff destination when the pack turn starts, never an
        // untitled fresh session.
        self.persist_handoff_links(&pending.source_session, run.from, &dest, run.to, cx);
        // The pack is a live turn: the row reads running with a fresh time
        // from the submit, exactly like `ProviderTurnAccepted` (Y2a) — so
        // the head row shows "Working" and is never filtered as empty.
        if self.session_list.update(cx, |list, _| crate::provider_sessions::touch(&mut list.provider_sessions, &dest)) {
            self.persist_provider_sessions(cx);
        }
        let now = crate::clock::now_local();
        let mark = |list: &mut crate::session_list::SessionList| {
            list.sessions.iter_mut().find(|entry| entry.id == dest).map(|entry| {
                entry.running = true;
                entry.updated = now;
                entry.turn_started = Some(now);
                entry.last_error = None;
            })
        };
        if self.session_list.update(cx, |list, _| mark(list)).is_none() {
            self.merge_provider_rows(cx);
            self.session_list.update(cx, |list, _| mark(list));
        }
        view.update(cx, |view, cx| {
            view.submit_pack(text, display, cx);
        });
        self.invalidate_list(cx);
    }

    /// The pack's submit ack on the destination: the run holding that
    /// destination advances to Acknowledged under its own epoch — a stale
    /// ack (cancelled, failed, superseded) matches no Prepared run and is
    /// ignored — then activates at once.
    pub(in crate::app) fn acknowledge_handoff(&mut self, dest: String, cx: &mut Context<Self>) {
        let found = self
            .handoff_state.runs
            .iter()
            .find(|(_, run)| run.destination_session.as_deref() == Some(dest.as_str()))
            .map(|(source, run)| (source.clone(), run.clone()));
        let Some((source, mut run)) = found else { return };
        if !run.acknowledge(run.epoch) {
            return;
        }
        run.activate();
        // §8, the transcript half: snapshot the source's visible turns —
        // everything its transcript shows, prefix included but the handoff
        // card left out — for the destination, then show them there above
        // the one divider.
        let snap_turns = self
            .find_view(&source, cx)
            .map(|view| view.update(cx, |view, cx| view.handoff_snapshot_turns(cx)))
            .unwrap_or_default();
        let (pack_full, pack_display) = run
            .pack
            .as_ref()
            .map(|pack| {
                (
                    crate::handoff::pack_text(pack, run.from),
                    crate::handoff::display_text(pack, run.from),
                )
            })
            .unwrap_or_default();
        let turns_carried = run.pack.as_ref().map(|pack| pack.recent.len());
        let to_model = (!run.to_model.is_empty()).then(|| run.to_model.clone());
        crate::handoff_snapshot::write_snapshot(
            &dest,
            &crate::handoff_snapshot::HandoffSnapshot {
                version: crate::handoff_snapshot::SNAPSHOT_VERSION,
                from: run.from.as_str().to_owned(),
                to: run.to.as_str().to_owned(),
                source: source.clone(),
                to_model: to_model.clone(),
                activated_ms: crate::handoff_snapshot::now_ms(),
                pack_text: pack_full.clone(),
                pack_display: pack_display.clone(),
                turns: snap_turns.clone(),
                turns_carried,
            },
        );
        let card = run.card();
        let card_id = run.card_id.clone();
        let (from, to) = (run.from, run.to);
        let origin = crate::handoff::HandoffOrigin {
            source_session: source.clone(),
            from,
            from_model: run.from_model.clone(),
        };
        let divider = crate::handoff_snapshot::divider_text(from, to, to_model.as_deref(), turns_carried);
        self.handoff_state.runs.insert(source.clone(), run);
        if let Some(view) = self.find_view(&source, cx) {
            view.update(cx, |view, cx| {
                view.replace_handoff_card(&card_id, card, cx);
                view.retire_for_handoff(to, dest.clone(), cx);
            });
        }
        // Landing already shows the prefix from the destination's first
        // frame: re-showing it here would rebuild the transcript and lose
        // the scroll position, so the acknowledgement only fills in a
        // prefix landing never placed (a pending from before the stash).
        if let Some(view) = self.find_view(&dest, cx) {
            if !view.read(cx).has_handoff_prefix() {
                view.update(cx, |view, cx| {
                    view.show_handoff_prefix(
                        origin,
                        snap_turns,
                        divider,
                        (!pack_full.is_empty()).then(|| pack_full.clone()),
                        (!pack_display.is_empty()).then(|| pack_display.clone()),
                        cx,
                    );
                });
            }
        }
        self.persist_handoff_links(&source, from, &dest, to, cx);
        crate::baaz_log!("handoff activated {source} -> {dest}");
        // In place: activation leaves the window on the destination — the
        // same slot the source occupied, scrolled to the bottom (the prefix
        // update set follow) — with the composer focused and live. The
        // destination has been visible since it opened; only a user who
        // clicked away mid-flight still needs the swap.
        self.focus_composer = true;
        if let Some(view) = self.find_view(&dest, cx) {
            let current = self.active.as_ref().is_some_and(|active| *active == view);
            if !current {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.activate(view, false, window, cx);
                    });
                }));
            }
        }
        cx.notify();
    }

    /// The pack never landed — or its pack turn terminally failed while
    /// still Prepared: the run fails with the reason, the source stays
    /// usable (a Failed card sends again), the chain links roll back,
    /// and the destination row reads "Hand-off failed: <reason>". Past
    /// Prepared the move owns its state and this is a no-op (see
    /// [`crate::handoff::HandoffRun::fail_pack_turn`]). An event for an
    /// unknown destination is ignored — it belongs to no run.
    pub(in crate::app) fn fail_handoff(&mut self, dest: String, reason: String, cx: &mut Context<Self>) {
        let source = self
            .handoff_state.runs
            .iter()
            .find(|(_, run)| run.destination_session.as_deref() == Some(dest.as_str()))
            .map(|(source, _)| source.clone());
        let Some(source) = source else { return };
        let Some(mut run) = self.handoff_state.runs.get(&source).cloned() else { return };
        run.fail_pack_turn(reason.clone());
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoff_state.runs.insert(source.clone(), run);
        if let Some(view) = self.find_view(&source, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        self.clear_handoff_links(&source, &dest, cx);
        let row_reason = format!("Hand-off failed: {reason}");
        self.session_list.update(cx, |list, _| {
            if let Some(entry) = list.sessions.iter_mut().find(|entry| entry.id == dest) {
                entry.last_error = Some(row_reason.clone());
            }
        });
        self.set_override(&dest, |meta| meta.last_error = Some(row_reason.clone()), cx);
        crate::baaz_log!("handoff failed {source}: {reason}");
        cx.notify();
    }

    /// Undo what landing linked: the chain halves in both stores leave,
    /// so a Failed or Cancelled handoff lists the source on its own again
    /// and leaves the destination standing alone. Only this handoff's own
    /// halves unlink — another chain's links are never touched.
    fn clear_handoff_links(&mut self, source: &str, dest: &str, cx: &mut Context<Self>) {
        self.session_list.update(cx, |list, _| crate::provider_sessions::clear_handoff(&mut list.provider_sessions, source, dest));
        self.persist_provider_sessions(cx);
        let (source, dest) = (source.to_owned(), dest.to_owned());
        if let Some(meta) = self.session_list.read(cx).overrides.get(source.as_str()) {
            if meta.handoff_to.as_deref() == Some(dest.as_str()) {
                self.set_override(source.as_str(), |meta| meta.handoff_to = None, cx);
            }
        }
        if let Some(meta) = self.session_list.read(cx).overrides.get(dest.as_str()) {
            if meta.handoff_from.as_deref() == Some(source.as_str()) {
                self.set_override(dest.as_str(), |meta| {
                    meta.handoff_from = None;
                    meta.handoff_from_provider = None;
                    meta.handoff_title = None;
                }, cx);
            }
        }
        self.merge_provider_rows(cx);
        self.invalidate_list(cx);
    }

    /// A pack turn settled on a handoff destination: when the
    /// destination's folded transcript shows its pack turn terminally
    /// failed — the reply's last word is an error card, not a recoverable
    /// card inside a reply that answered past it — the run fails with the
    /// turn's reason instead of going silently Active. The pack is acked
    /// when its turn STARTS, so this judges it in Prepared, Acknowledged or
    /// Activated — once: after the pack turn is judged (completed or
    /// failed), reconciling stops for that run, so a later
    /// failed turn never fails an active handoff or unlinks its chain.
    /// Called after every settled turn on either lane (the muse lane's
    /// `turn/completed` lands in `maybe_rewrite_byline`, the provider
    /// lane's `TurnFinished` in its own arm); a no-op without a handoff.
    pub(crate) fn reconcile_pack_turns(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<(String, String)> = self
            .handoff_state.runs
            .iter()
            .filter(|(_, run)| run.pack_turn_can_fail())
            .filter_map(|(source, run)| Some((source.clone(), run.destination_session.clone()?)))
            .collect();
        for (source, dest) in pending {
            let Some(outcome) = self.find_view(&dest, cx).and_then(|view| view.read(cx).pack_turn_outcome())
            else {
                continue;
            };
            match outcome {
                Some(reason) => self.fail_handoff(dest, reason, cx),
                None => {
                    if let Some(run) = self.handoff_state.runs.get_mut(&source) {
                        run.note_pack_completed();
                    }
                }
            }
        }
    }

    /// The handoff destination went away before it acknowledged the pack
    /// — its view closed, or its session errored — with no
    /// `HandoffPackFailed` to say so: fail the still-Prepared run with a
    /// reason and roll the chain links back, so neither a Prepared run
    /// nor its links linger forever. Runs past Prepared own their state:
    /// acknowledged/activated moves stay, failed/cancelled ones keep
    /// their reason. A session that holds no Prepared run passes through
    /// untouched.
    pub(super) fn fail_handoff_for_lost_destination(&mut self, dest: &str, reason: String, cx: &mut Context<Self>) {
        let prepared = self.handoff_state.runs.iter().any(|(_, run)| {
            run.destination_session.as_deref() == Some(dest)
                && matches!(run.state, aui_protocol::HandoffState::Prepared)
        });
        if prepared {
            self.fail_handoff(dest.to_owned(), reason, cx);
        }
    }

    /// "Cancel" on a handoff card: aborts while cancellable and shuts the
    /// opened destination down. Past Acknowledged the move is done and
    /// the press is ignored.
    pub(in crate::app) fn cancel_handoff(&mut self, source: &str, card_id: &str, cx: &mut Context<Self>) {
        let Some(mut run) = self.handoff_state.runs.get(source).cloned() else { return };
        if run.card_id != card_id || !run.cancellable() {
            return;
        }
        // The wait is over: a late summary answer finds no waiting run and
        // lands on nothing.
        let opened = run.cancel();
        if opened {
            if let Some(dest) = run.destination_session.clone() {
                self.clear_handoff_links(source, &dest, cx);
                self.close_view(&dest, cx);
            }
        }
        self.handoff_state.drop_pending_for(source);
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoff_state.runs.insert(source.to_owned(), run);
        if let Some(view) = self.find_view(source, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        crate::baaz_log!("handoff cancelled {source}");
        cx.notify();
    }

    /// The destination never opened: fail the pending run with the open's
    /// reason, so the source card names it and the source stays usable.
    /// A call with nothing pending is a no-op.
    pub(in crate::app) fn fail_pending_handoff(&mut self, reason: String, cx: &mut Context<Self>) {
        let Some(pending) = self.handoff_state.pending.take() else { return };
        let Some(mut run) = self.handoff_state.runs.get(&pending.source_session).cloned() else { return };
        if run.epoch != pending.epoch {
            return;
        }
        // The wait is over with the open: a late summary answer finds no
        // waiting run and lands on nothing.
        run.fail(reason.clone());
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoff_state.runs.insert(pending.source_session.clone(), run);
        if let Some(view) = self.find_view(&pending.source_session, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        crate::baaz_log!("handoff failed {}: {reason}", pending.source_session);
        cx.notify();
    }

    /// Reopening a handoff destination (`docs/22-handoff.md` §8): prefix +
    /// divider from the snapshot, or the fallback divider alone when the
    /// chain predates snapshots. No-op for sessions with no handoff links,
    /// so fresh opens pass through untouched. No child is ever spawned for
    /// the source — history comes from the file.
    pub(super) fn attach_handoff_prefix(&mut self, view: &Entity<SessionView>, cx: &mut Context<Self>) {
        // Landing already shows the prefix from the destination's first
        // frame: reopening or re-attaching the same view must not wipe it
        // back to the fallback divider with no turns.
        if view.read(cx).has_handoff_prefix() {
            return;
        }
        let (session_id, to) = {
            let view = view.read(cx);
            (view.session_id.clone(), view.provider_kind())
        };
        if let Some(snapshot) = crate::handoff_snapshot::read_snapshot(&session_id) {
            let from = ProviderId::parse(&snapshot.from);
            let origin = crate::handoff::HandoffOrigin {
                source_session: snapshot.source.clone(),
                from,
                from_model: String::new(),
            };
            let divider = crate::handoff_snapshot::divider_text(
                from,
                to,
                snapshot.to_model.as_deref(),
                snapshot.turns_carried,
            );
            view.update(cx, |view, cx| {
                view.show_handoff_prefix(
                    origin,
                    snapshot.turns.clone(),
                    divider,
                    (!snapshot.pack_text.is_empty()).then(|| snapshot.pack_text.clone()),
                    (!snapshot.pack_display.is_empty()).then(|| snapshot.pack_display.clone()),
                    cx,
                );
            });
            return;
        }
        let linked = self
            .session_list.read(cx).provider_sessions
            .get(&session_id)
            .and_then(|record| {
                record.handoff_from.clone().map(|source| (source, record.handoff_from_provider.clone()))
            })
            .or_else(|| {
                self.session_list.read(cx).overrides.get(&session_id).and_then(|meta| {
                    meta.handoff_from.clone().map(|source| (source, meta.handoff_from_provider.clone()))
                })
            });
        let Some((source, from_wire)) = linked else { return };
        let from = ProviderId::parse(from_wire.as_deref().unwrap_or("muse"));
        let origin = crate::handoff::HandoffOrigin {
            source_session: source,
            from,
            from_model: String::new(),
        };
        let divider = crate::handoff_snapshot::fallback_text(from);
        view.update(cx, |view, cx| {
            let display = view.pack_display_fallback();
            view.show_handoff_prefix(origin, Vec::new(), divider, None, display, cx);
        });
    }

    /// Both halves of the link survive restart: the provider record for a
    /// lane session, the local row for a muse one.
    fn persist_handoff_links(
        &mut self,
        source: &str,
        from: ProviderId,
        dest: &str,
        to: ProviderId,
        cx: &mut Context<Self>,
    ) {
        let _ = to;
        // The chain's one title: the source's current display title (a
        // user name first, then its title — the row label already reads
        // that order), carried forward down chains of any length.
        let chain_title = self
            .session_list.read(cx).sessions
            .iter()
            .find(|e| e.id == source)
            .map(|e| e.label.clone())
            .or_else(|| {
                self.session_list.read(cx).overrides.get(source).and_then(|m| {
                    m.name
                        .clone()
                        .or_else(|| m.handoff_title.clone())
                        .or_else(|| m.generated_title.clone())
                        .or_else(|| m.derived_title.clone())
                })
            })
            .or_else(|| {
                self.session_list.read(cx).provider_sessions.get(source).and_then(|r| {
                    r.handoff_title
                        .clone()
                        .or_else(|| r.title.clone())
                        .or_else(|| r.first_prompt.clone())
                })
            })
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty());
        let source_meta = self.session_list.read(cx).overrides.get(source).cloned();
        let source_record = self.session_list.read(cx).provider_sessions.get(source).cloned();
        if self.session_list.read(cx).provider_sessions.contains_key(source) || self.session_list.read(cx).provider_sessions.contains_key(dest) {
            self.session_list.update(cx, |list, _| crate::provider_sessions::note_handoff(&mut list.provider_sessions, source, from.as_str(), dest));
            if let Some(title) = chain_title.clone() {
                self.session_list.update(cx, |list, _| {
                    if let Some(record) = list.provider_sessions.get_mut(dest) {
                        record.handoff_title = Some(title);
                    }
                });
            }
            // Row state rides to the destination: pinned, project, user
            // name and archived survive the hop (Y2a). Pinned/archived
            // live in the overrides even for lane sessions, so they land
            // there whatever the destination lane is.
            self.session_list.update(cx, |list, _| {
                if let Some(dest_record) = list.provider_sessions.get_mut(dest) {
                    if let Some(meta) = source_meta.as_ref() {
                        if dest_record.project.is_none() {
                            dest_record.project = meta.project.clone();
                        }
                    }
                    if let Some(record) = source_record.as_ref() {
                        if dest_record.project.is_none() {
                            dest_record.project = record.project.clone();
                        }
                    }
                }
            });
            if let Some(src) = source_meta.clone() {
                self.set_override(
                    dest,
                    |meta| {
                        if !meta.pinned {
                            meta.pinned = src.pinned;
                        }
                        if !meta.archived {
                            meta.archived = src.archived;
                        }
                        if meta.project.is_none() {
                            meta.project = src.project.clone();
                        }
                        if meta.name.is_none() {
                            meta.name = src.name.clone();
                        }
                    },
                    cx,
                );
            }
            self.persist_provider_sessions(cx);
        }
        let solo_source = !self.session_list.read(cx).provider_sessions.contains_key(source);
        let solo_dest = !self.session_list.read(cx).provider_sessions.contains_key(dest);
        if solo_source || solo_dest {
            if solo_source {
                self.set_override(source, |meta| meta.handoff_to = Some(dest.to_owned()), cx);
            }
            if solo_dest {
                let title = chain_title.clone();
                let src = source_meta.clone();
                let src_record = source_record.clone();
                self.set_override(
                    dest,
                    |meta| {
                        meta.handoff_from = Some(source.to_owned());
                        meta.handoff_from_provider = Some(from.as_str().to_owned());
                        if meta.handoff_title.is_none() {
                            meta.handoff_title = title.clone();
                        }
                        // A muse destination inherits the source's row
                        // state it does not already name itself.
                        if let Some(s) = src.as_ref() {
                            if !meta.pinned {
                                meta.pinned = s.pinned;
                            }
                            if !meta.archived {
                                meta.archived = s.archived;
                            }
                            if meta.project.is_none() {
                                meta.project = s.project.clone();
                            }
                            if meta.name.is_none() {
                                meta.name = s.name.clone();
                            }
                        }
                        if let Some(r) = src_record.as_ref() {
                            if meta.project.is_none() {
                                meta.project = r.project.clone();
                            }
                        }
                    },
                    cx,
                );
            }
            crate::sessions::write(&self.session_list.read(cx).overrides);
        }
        self.merge_provider_rows(cx);
    }
}
