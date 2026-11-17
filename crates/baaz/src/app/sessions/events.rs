//! The single sink for [`SessionEvent`]s: what a session view tells the
//! root (rows, records, usage, titles, bylines, handoffs).
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

impl Harness {
    /// What a session cannot decide for itself.
    pub(in crate::app) fn on_session_event(
        &mut self,
        view: Entity<SessionView>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionEvent::Dialog { title, detail } => {
                self.set_dialog(cx, Dialog {
                    title: title.clone(),
                    detail: detail.clone(),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                });
            }
            // A typed credential failure from any provider (Claude Code's
            // `authentication_failed`, Codex's `unauthorized`/401, Muse's
            // refusal): the one quiet banner above the composer, and a
            // re-probe so every surface agrees (D5 B5). No dialog.
            SessionEvent::SignedOut => {
                let id = view.read(cx).provider_kind();
                self.provider_banners.insert(id, crate::connect::signed_out_banner_text(id));
                self.typed_signed_out.insert(id);
                crate::provider_status::reprobe_provider(id);
                cx.notify();
            }
            // The child's exit already reached the hub's connection stream,
            // whose handler owns the reconnect.
            // A destination that errors out before it acknowledged the pack
            // never sends `HandoffPackFailed`: fail its still-Prepared run
            // here, so the links roll back instead of lingering with it.
            // The lane's child died twice in a row (docs/71 §2): the view
            // grows its own lost banner into the typed failure, under its
            // transcript; the window only notes it.
            SessionEvent::ChildFailed { message, stderr_excerpt } => {
                let session_id = view.read(cx).session_id.clone();
                crate::baaz_log!(
                    "session {session_id}: the agent died twice in a row ({message}; {} stderr lines)",
                    stderr_excerpt.len()
                );
                // Baaz stopped restarting it: the row reads `Failed` until
                // a turn succeeds again (task 3.1 — a lost child used to
                // leave the row idle).
                let message = message.clone();
                self.record_turn_outcome(&session_id, true, Some(&message), cx);
            }
            // The grown lost banner's typed action: the D5 flow of that
            // name for this session's provider.
            SessionEvent::FailureAction(action) => {
                let provider = view.read(cx).provider_kind();
                let action = *action;
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.run_failure_action(provider, action, window, cx);
                    });
                }));
            }
            // A lane's turn ended: its row's "Failed" follows the outcome,
            // for every provider (docs/71 §6).
            SessionEvent::ProviderTurnOutcome { session_id, failed, error } => {
                let (session_id, failed, error) = (session_id.clone(), *failed, error.clone());
                self.record_turn_outcome(&session_id, failed, error.as_deref(), cx);
            }
            SessionEvent::Closed => {
                let closed_id = view.read(cx).session_id.clone();
                self.fail_handoff_for_lost_destination(
                    &closed_id,
                    "the destination session closed before it acknowledged the pack".to_owned(),
                    cx,
                );
            }
            // The backfill's first page applied: pin the tail while later
            // pages land. Anything but the open view is a parked chain
            // warming the cache, and its own completion already pinned it.
            SessionEvent::HistoryReady => {
                if self.active.as_ref().is_some_and(|active| *active == view) {
                    view.update(cx, |view, cx| view.follow_tail(cx));
                    let session_id = view.read(cx).session_id.clone();
                    self.mark_read(&session_id, cx);
                }
            }
            // The open session was read to its end (task 3.1).
            SessionEvent::SeenToEnd => {
                if self.active.as_ref().is_some_and(|active| *active == view) {
                    let session_id = view.read(cx).session_id.clone();
                    self.mark_read(&session_id, cx);
                }
            }
            // `/clear` is emitted from the session view with no window of its
            // own: reopen through the window the update provides, like the
            // fork path below does.
            SessionEvent::NewSession => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.new_session(window, cx));
                }));
            }
            // A provider lane's fork, minted by `ForkSession`: connect a
            // fresh child on the same provider and resume the fork there,
            // opening it as a new lane view — the source view's lane is
            // never disturbed.
            SessionEvent::ForkedOnProvider { session_id, provider } => {
                let (session_id, provider) = (session_id.clone(), *provider);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        let workspace = this.workspace();
                        this.open_forked_on_provider(provider, session_id, workspace, window, cx);
                    });
                }));
            }
            // A provider lane admitted a turn: the row goes visible now
            // (titled from the prompt when it carries no name of its own),
            // the session may earn a generated title, and the record moves
            // to now — the lane's `turn/started`.
            SessionEvent::ProviderTurnAccepted { session_id, prompt, turn_id, .. } => {
                self.on_turn_accepted(view, session_id, prompt, turn_id, cx)
            }
            // A provider lane landed or settled an approval card: the
            // row re-reads the open view's pending words, so it stands
            // on the needs-you state while the approval waits.
            // The reply is whole and the provider is only saving memories:
            // the row stops reading `Working`. Titles and bylines still wait
            // for the turn's own settle.
            SessionEvent::ReplyComplete { session_id } => {
                self.sync_row_live(session_id, false, cx);
                cx.notify();
            }
            SessionEvent::ProviderApprovalsChanged { session_id } => {
                self.sync_row_live(session_id, false, cx);
                cx.notify();
            }
            // A provider lane settled a turn: the free byline lands, the
            // ledger gains its tagged row, and the record counts the turn —
            // the lane's `turn/completed`.
            SessionEvent::ProviderTurnFinished { session_id, turn_id, meta } => {
                self.on_turn_finished(view, session_id, turn_id, meta, cx)
            }
            SessionEvent::Logout => self.logout(cx),
            // "Enable" on a disabled-provider banner: Settings on the
            // Providers page, where the switch lives.
            SessionEvent::OpenProviders => {
                let sections = self.settings_sections(cx);
                let section = crate::settings::settings_section_index(
                    &sections,
                    crate::settings_providers::PROVIDERS_SECTION_ID,
                );
                self.open_settings(section, cx);
            }
            SessionEvent::Status { detail } => {
                // What the login is entitled to belongs at the top of
                // `/status` and `/usage`: it is the first thing that decides
                // what the next turn costs.
                let plan = self.tier.as_ref().map(Tier::status_lines).unwrap_or_else(|| "Plan: probing\u{2026}".to_owned());
                self.set_dialog(cx, Dialog {
                    title: "Session status".into(),
                    detail: format!("{plan}\n\n{detail}"),
                    kind: DialogKind::Info,
                    primary: "Done",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                });
                // `/usage` is a person asking; take the reading again behind
                // the dialog rather than serving a cache they just doubted.
                self.probe_tier(true, cx);
            }
            SessionEvent::TierOverride => {
                self.send_anyway = true;
                self.push_tier(cx);
            }
            SessionEvent::TierRecheck => self.probe_tier(true, cx),
            SessionEvent::Rename { name } => {
                let session_id = view.read(cx).session_id.clone();
                self.rename_session(session_id, name.clone(), cx);
            }
            // The person picked a model, effort or approval mode in a
            // session: it becomes the last pick, so the next session — in
            // any project — starts with it. Scripted `setmodel:` /
            // `setmode:` steps and plan-mode toggles never emit these, so
            // scripted and transient choices stay out of the defaults.
            SessionEvent::ModelSelected { model_id } => {
                let provider = view.read(cx).provider_kind();
                self.note_pick(|defaults| {
                    defaults.note_model(provider.as_str(), model_id);
                });
            }
            SessionEvent::EffortSelected { effort } => {
                let provider = view.read(cx).provider_kind();
                self.note_pick(|defaults| {
                    defaults.note_effort(provider.as_str(), effort.clone());
                });
            }
            SessionEvent::ModeSelected { mode } => {
                self.note_pick(|defaults| {
                    defaults.approval_mode = Some(*mode);
                });
            }
            // A fresh session's composer chip picked another provider:
            // nothing was ever sent, so the pick becomes the default and a
            // new session starts on it — on the provider lane when the pick
            // is not muse. Y2b: the old view stays active and drawn (composer
            // locked, chip already on the pick) until the replacement
            // activates in the same update — no empty-state frame between.
            SessionEvent::SwitchProvider { provider } => self.on_switch_provider(view, provider, cx),
            // "Hand off to X…" on a session with turns: the lossy
            // re-prompt (see `crate::handoff`). The dialog (or the step
            // verb's headless flag) decides whether a confirm comes
            // first; opening the destination needs a window, so both
            // rejoin through one like the paths above.
            SessionEvent::HandoffRequested { provider, headless } => {
                let (source, provider, headless) =
                    (view.read(cx).session_id.clone(), *provider, *headless);
                if headless {
                    self.tasks.push(cx.spawn(async move |this, cx| {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.start_handoff(source, provider, window, cx)
                        });
                    }));
                } else {
                    self.show_handoff_confirm(source, provider, cx);
                }
            }
            // "Open the new session" on a handoff card: show the destination
            // (the chain head, so the redirect is a no-op). Rejoins through
            // a window like the rename path above.
            SessionEvent::HandoffOpenSession { destination } => {
                let destination = destination.clone();
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.resume(destination, window, cx));
                }));
            }
            // "Open the source session" on a fallback divider: show the
            // retired source read-only, bypassing the head redirect.
            SessionEvent::HandoffOpenSource { source } => {
                let source = source.clone();
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.resume_source_read_only(source, window, cx)
                    });
                }));
            }
            // "Cancel" on a handoff card: abort while cancellable,
            // shutting the opened destination down.
            SessionEvent::HandoffCancel { card_id } => {
                let (source, card_id) = (view.read(cx).session_id.clone(), card_id.clone());
                self.cancel_handoff(&source, &card_id, cx);
            }
            // The pack's submit ack on the destination: the run holding
            // that destination advances, epoch-fenced.
            SessionEvent::HandoffPackAccepted { session_id } => {
                self.acknowledge_handoff(session_id.clone(), cx);
            }
            // The pack never landed: the run fails, the source stays usable.
            SessionEvent::HandoffPackFailed { session_id, reason } => {
                self.fail_handoff(session_id.clone(), reason.clone(), cx);
            }
            SessionEvent::RenameStart => {
                let session_id = view.read(cx).session_id.clone();
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.start_rename(session_id, window, cx));
                }));
            }
            SessionEvent::Hide => {
                let session_id = view.read(cx).session_id.clone();
                self.hide_session(session_id, cx);
            }
            SessionEvent::ToggleEmpty => {
                self.show_empty = !self.show_empty;
                self.invalidate_list();
            }
            SessionEvent::Resume => self.open_palette(PaletteKind::Resume, cx),
            SessionEvent::ForkPicker => self.open_palette(PaletteKind::Fork, cx),
            SessionEvent::Projects => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.open_projects(false, window, cx));
                }));
            }
            SessionEvent::Search => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.open_search(window, cx));
                }));
            }
            // Rejoins through a window the way the paths above do; the event
            // carries no window of its own.
            SessionEvent::WindowCommand(command) => {
                let command = *command;
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.run_window_command(command, window, cx);
                    });
                }));
            }
            // A transcript skill row's tap (D63): open the Skills page on
            // that skill. The open re-lists behind the previous catalog, so
            // the selection lands on what the page already shows.
            SessionEvent::OpenSkill { name } => {
                let name = name.clone();
                self.with_skills_page(cx, |page, cx| {
                    page.open_skills(cx);
                    page.select_skill(name, cx);
                });
            }
            // A transcript or terminal link (B9): `http(s)` opens in
            // Baaz's Browser pane (the navigation lands on the next
            // render); anything external rides the system browser.
            SessionEvent::OpenUrl { url, external } => {
                if *external {
                    cx.open_url(url);
                } else {
                    self.open_url_in_browser(url.clone(), cx);
                }
            }
            // A transcript path link (B9): the existence check runs off
            // the UI thread, and the reveal lands through the Files pane's
            // own path — folders select and scroll, files preview banded.
            SessionEvent::RevealPath { abs, highlight } => {
                let abs = abs.clone();
                let highlight = highlight.clone();
                let root = self.right_project().map(|(root, _)| root);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let probe = abs.clone();
                    let is_dir = cx
                        .background_executor()
                        .spawn(async move { std::fs::metadata(&probe).ok().map(|meta| meta.is_dir()) })
                        .await;
                    let _ = this.update(cx, |this: &mut Self, cx| {
                        let owned = root.or_else(|| this.right_project().map(|(root, _)| root));
                        if let Some(owned) = owned {
                            this.reveal_link_path(&owned, &abs, is_dir, highlight, cx);
                        }
                    });
                }));
            }
            // B12: a turn fold's `+N −M` chip: Changes for the session.
            // Per-turn checkpoints do not exist yet, so the chip opens the
            // session-scoped surface rather than pretending a scope. The
            // id is kept for that filter — read here so the shape stays.
            // D6 (docs/43 §2.9–2.10, §4): the views the transcript opens.
            SessionEvent::OpenOutput(out) => self.open_tool_output(out.clone(), cx),
            SessionEvent::OpenImage(path) => self.show_image(path.clone(), cx),
            // D6 (docs/43 §4): the chip opens Changes on that turn's files,
            // open, scrolled to, flashing once.
            SessionEvent::OpenTurnChanges { turn_id } => {
                let files = self.active.as_ref().map(|view| view.read(cx).turn_edits(turn_id)).unwrap_or_default();
                self.open_turn_changes(files, cx);
            }
        }
        cx.notify();
    }

    /// A provider lane admitted a turn: the row goes visible now
    /// (titled from the prompt when it carries no name of its own),
    /// the session may earn a generated title, and the record moves
    /// to now — the lane's `turn/started`.
    fn on_turn_accepted(&mut self, view: Entity<SessionView>, session_id: &str, prompt: &str, turn_id: &str, cx: &mut Context<Self>) {
        // Every provider's turn, Muse's included (docs/70): rows,
        // titles and drafts follow the view's own lane.
        let provider_kind = view.read(cx).provider_kind();
        let (session_id, prompt, turn_id) =
            (session_id.to_owned(), prompt.to_owned(), turn_id.to_owned());
        // A turn that started is a session made real: it is no
        // draft any more, whether it already had a row or not.
        self.open_state.admit_turn(&session_id);
        let handoff_dest =
            sidebar::is_handoff_dest(&session_id, &self.provider_sessions, &self.overrides);
        // A handoff destination's first words are the pack, then the
        // person's next message on the same chain: neither is the
        // session's own first prompt (Y2a).
        if !handoff_dest {
            crate::provider_sessions::note_first_prompt(
                &mut self.provider_sessions,
                &session_id,
                &prompt,
            );
        }
        if crate::provider_sessions::touch(&mut self.provider_sessions, &session_id) {
            self.persist_provider_sessions(cx);
        }
        // A generated title outranks the prompt, even on a retry
        // of a first turn that never settled (`send_title`).
        let title = sidebar::send_title(&session_id, Some(&prompt), &self.overrides);
        if !self.sessions.iter().any(|entry| entry.id == session_id) {
            // No row yet (a restart between the open and this
            // send): build it now, titled from the prompt — from
            // the record, or for a provider that lists its own
            // sessions, as a local row until the list shows it.
            self.merge_provider_rows();
            if provider_kind.lists_sessions() {
                let project = self.overrides.get(&session_id).and_then(|m| m.project.clone());
                let workspace = self.session_workspace(&session_id);
                self.ensure_listed_row(&session_id, project, workspace);
                // A handoff destination's rowless first turn is the
                // pack: the row carries the chain title (Y2a).
                if handoff_dest {
                    let label = sidebar::handoff_title_of(&session_id, &self.provider_sessions, &self.overrides)
                        .map(|t| sidebar::one_line(&t));
                    if let (Some(label), Some(entry)) =
                        (label, self.sessions.iter_mut().find(|entry| entry.id == session_id))
                    {
                        entry.label = label;
                    }
                }
            }
        }
        if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == session_id) {
            if sidebar::first_send_update(entry, title, crate::clock::now_local(), handoff_dest) {
                self.invalidate_list();
                // The row just went from invisible to visible: a
                // reveal armed when this session was opened may
                // already have given up on it (it was filtered as
                // empty), so re-arm it as an outside activation
                // does — only for the session this window has open.
                if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
                    self.reveal = Some(session_id.clone());
                    self.reveal_unknown = None;
                }
            }
        }
        // A first send may earn a generated title: the same cheap
        // muse side session the muse lane uses — never a second
        // provider child.
        self.maybe_start_title(&session_id, Some(prompt), cx);
        self.title_from_transcript(&view, cx);
        // A late admission — the ack arriving after its turn
        // already finished — is bookkeeping only (row, record,
        // title above): the row declines it as a start and
        // re-reads the settled view instead of manufacturing a
        // fresh `Working · now` (W8c).
        let stale = view.read(cx).provider_turn_finished(&turn_id);
        self.sync_row_live(&session_id, !stale, cx);
        cx.notify();
    }

    /// A provider lane settled a turn: the free byline lands, the
    /// ledger gains its tagged row, and the record counts the turn —
    /// the lane's `turn/completed`.
    fn on_turn_finished(
        &mut self,
        view: Entity<SessionView>,
        session_id: &str,
        turn_id: &str,
        meta: &aui_protocol::TurnMeta,
        cx: &mut Context<Self>,
    ) {
        let (session_id, turn_id, meta) = (session_id.to_owned(), turn_id.to_owned(), meta.clone());
        // Every provider's turn, Muse's included (docs/70): the
        // ledger row is tagged with its provider, and the row,
        // record and byline move here — one row, never a relist.
        let provider_kind = view.read(cx).provider_kind();
        let provider = provider_kind.as_str().to_owned();
        // A Claude Code report seeds the next fresh session's
        // chip: display-only (the argv never sees it — see
        // `claude_code_seed_model`), persisted beside the other
        // stores with their hermeticity rule.
        // ...except an internal sub-call's (see
        // `should_seed_claude_code_model`): only a Haiku report
        // against a non-Haiku pick is ever dropped — any other
        // report updates the seed as before.
        let known = view.read(cx).model_id();
        if should_seed_claude_code_model(provider_kind, &meta.model, &known) {
            crate::providers::write_claude_code_last_reported_model(&meta.model);
        }
        let cursor =
            view.read(cx).last_cursor().unwrap_or_else(|| turn_id.clone());
        let row = crate::provider_sessions::ledger_row(
            &session_id,
            &provider,
            &cursor,
            &turn_id,
            &meta,
        );
        // Fire-and-forget on the background executor, like the muse
        // lane's recorder: usage history is a ledger, not a feature
        // anything blocks on. The `(session_id, turn_id)` key makes
        // a replayed turn a no-op insert rather than a duplicate.
        self.wire_call(cx, move || crate::usage::record_backfilled(&[row]), |_this, (), _cx| {});
        // Count exchanges, as muse rows do: the folded assistant
        // turns, never a blind increment — a reopen replays the
        // same `TurnFinished`s, and incrementing again read "2
        // turns" for one user+assistant exchange.
        let exchanges = view
            .read(cx)
            .session()
            .map(|session| {
                session
                    .turns
                    .iter()
                    .filter(|turn| matches!(turn, aui_protocol::Turn::Assistant { .. }))
                    .count() as u64
            })
            .unwrap_or(0);
        let counted_before = self.provider_sessions.get(&session_id).map(|record| record.turns);
        // Only a turn this window admitted is activity: a reopen's
        // replay may correct the count, never the row's time.
        let live = self.open_state.turns_admitted.contains(&session_id);
        if crate::provider_sessions::note_settled_turn_counted(
            &mut self.provider_sessions,
            &session_id,
            exchanges,
            live,
        ) {
            self.persist_provider_sessions(cx);
        }
        self.rejoin_provider_row(&session_id);
        // A replayed settle — a reopen folding history the record
        // already counted — counted nothing. It is not new work, so
        // it starts no model work either (an imported session has
        // no stored byline, and its reopen fired a paid rewrite on
        // its own provider, 2026-10-30).
        let counted = self.provider_sessions.get(&session_id).map(|record| record.turns) != counted_before;
        // A new exchange is new searchable words (docs/50 P3): the
        // sync re-reads this one transcript and skips every other
        // row on its fingerprint. A replayed settle counts nothing
        // and syncs nothing.
        if counted {
            let ids: std::collections::HashSet<String> = std::iter::once(session_id.clone()).collect();
            self.rebuild_search_index_for(&ids, cx);
        }
        self.sync_row_live(&session_id, false, cx);
        // The free excerpt lands only for a turn this window ran.
        // A reopen's replay settles too, and landing there rewrote
        // the row's second line just because it was clicked
        // (owner, 2026-11-12): a session made in the CLI swapped its
        // first prompt for its last reply on open.
        if live {
            self.record_last_summary(&view, cx);
        }
        // A settled pack turn fails its handoff here, with its
        // reason on the card and the row — never a silent Active.
        self.reconcile_pack_turns(cx);
        if counted {
            self.title_from_transcript(&view, cx);
            // The free excerpt just landed; a poor one may earn one
            // debounced rewrite — idle sessions only, never a
            // running turn — through the same side session as a
            // title.
            self.maybe_rewrite_byline(&view, cx);
        }
        // The lane settled: repaint on this path even if the
        // view's own notify raced the event delivery (W8c).
        cx.notify();
    }

    /// A fresh session's composer chip picked another provider:
    /// nothing was ever sent, so the pick becomes the default and a
    /// new session starts on it — on the provider lane when the pick
    /// is not muse. Y2b: the old view stays active and drawn (composer
    /// locked, chip already on the pick) until the replacement
    /// activates in the same update — no empty-state frame between.
    fn on_switch_provider(&mut self, view: Entity<SessionView>, provider: &ProviderId, cx: &mut Context<Self>) {
        let old = view.read(cx).session_id.clone();
        // Y2b2: every switch stamps a fresh generation. A second
        // switch while one is pending supersedes the first: its
        // in-flight open is orphaned (it lands stale — child shut
        // down, nothing parked) and the still-queued first task
        // sees the moved epoch and starts nothing.
        if self.open_state.is_replacing() {
            self.open_state.next_open();
        } else {
            // Q4b: a provider reopen may be pending instead (no
            // replace marked, the old view still active) — invalidate
            // it like any other navigation does, so its late
            // landing never steals this switch's replacement.
            self.cancel_pending_switch(cx);
        }
        // The replacement starts on a task, but the switch is
        // already in flight as far as scripted verbs are concerned:
        // claiming it now keeps a following `send:` waiting for the
        // lane instead of failing on the closed draft — and the
        // claim is what lets the start through the one-session
        // guard (see [`OpenState::begin_switch`]).
        let carry = view.read(cx).draft_settings();
        let epoch = self.open_state.begin_switch(old.clone(), carry);
        self.select_new_provider(*provider, cx);
        view.update(cx, |v, cx| {
            v.set_input_locked(true, cx);
            v.set_switching_to(Some(*provider), cx);
        });
        if let Some(active) = self.active.clone().filter(|v| v.read(cx).session_id == old) {
            active.update(cx, |v, cx| {
                v.set_input_locked(true, cx);
                v.set_switching_to(Some(*provider), cx);
            });
        }
        self.tasks.push(cx.spawn(async move |this, cx| {
            let _ = this.update_in(cx, |this, window, cx| {
                this.new_session_for_switch(epoch, window, cx)
            });
        }));
    }
}
