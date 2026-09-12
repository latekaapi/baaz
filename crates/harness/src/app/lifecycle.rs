//! The session list's lifecycle: reading the index, listing, starting,
//! resuming and opening sessions, and the deferred swap that keeps a switch
//! from flashing an empty transcript.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

impl Harness {
    // -------------------------------------------------------------- sessions

    /// Read the local index once at boot; it is a cache, not a source of truth.
    pub(super) fn load_index(&mut self, cx: &mut Context<Self>) {
        self.wire_call(cx, index::read, |this, index, cx| {
            this.index = index;
            this.rejoin();
            this.rebuild_search_index(cx);
            cx.notify();
        });
    }

    /// `session/list`, filtered to this window's workspace.
    pub(crate) fn load_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let workspace = self.workspace();
        let work = move || {
            client.session_list(&SessionListParams {
                workspace_root: Some(workspace),
                ..Default::default()
            })
        };
        self.wire_call_in(cx, work, |this, result, window, cx| {
            if let Ok(list) = result {
                this.invalidate_list();
                this.sessions = list
                    .sessions
                    .iter()
                    .map(|s| {
                        SessionEntry::join(s, this.index.get(&s.session_id), this.overrides.get(&s.session_id))
                    })
                    .collect();
                this.derive_titles(cx);
            }
            this.open_boot_session(window, cx);
            cx.notify();
        });
    }

    /// `--session <id>` (or `latest`): open one session at boot, once the list
    /// has arrived. Consumed, so a later refresh does not re-open it.
    pub(super) fn open_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wanted) = self.args.session.take() else {
            // A scripted turn or a scripted capture with no session named needs
            // somewhere to go.
            if (self.args.send.is_some() || !self.args.steps.is_empty()) && self.active.is_none() {
                self.new_session(cx);
            }
            return;
        };
        let id = if wanted == "latest" {
            // The list arrives `updatedAt` descending, and the sidebar sorts on
            // the same field, so the newest row is the head of the grouping.
            self.sessions.iter().filter(|e| !e.hidden).max_by_key(|e| e.updated).map(|e| e.id.clone())
        } else {
            Some(wanted)
        };
        if let Some(id) = id {
            self.resume(id, window, cx);
        }
    }

    /// `--send <text>`: one scripted turn, once a session is open. The hook a
    /// screenshot of a live turn needs; it goes through the same `send` a key
    /// press does, never straight into the fold.
    pub(super) fn send_scripted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.args.send.take() else { return };
        let Some(view) = self.active.clone() else { return };
        view.update(cx, |view, cx| {
            view.set_draft(text, window, cx);
            view.send(window, cx);
        });
    }

    /// `--steps`: drive the open session from the command line so a screenshot
    /// is reproducible. Consumed, so a later refresh does not replay them.
    /// The verbs, and the loop that runs them, are [`crate::steps`].
    pub(super) fn run_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        crate::steps::run_steps(self, cx);
    }

    /// `--steps`, taken out of the arguments so a later refresh does not
    /// replay them.
    pub(crate) fn take_steps(&mut self) -> Vec<String> {
        std::mem::take(&mut self.args.steps)
    }

    // One handler per `--steps` verb that belongs to the window rather than to
    // a session and needs more than a single existing call. The verb table
    // that reaches them is [`crate::steps`].

    /// `search:<query>`: open the search palette, optionally on a query.
    pub(crate) fn step_search(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
        if !rest.is_empty() {
            self.search_query.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
        }
    }

    /// `rename:<name>`: open the active row's inline field, optionally filled.
    pub(crate) fn step_rename(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let session_id = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
        if let Some(session_id) = session_id {
            self.start_rename(session_id, window, cx);
            if !rest.is_empty() {
                self.rename.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
            }
        }
    }

    /// `hidden`: list hidden sessions anyway.
    pub(crate) fn step_toggle_hidden(&mut self, cx: &mut Context<Self>) {
        self.show_hidden = !self.show_hidden;
        self.invalidate_list();
        cx.notify();
    }

    /// `empty`: list sessions with no turns anyway.
    pub(crate) fn step_toggle_empty(&mut self, cx: &mut Context<Self>) {
        self.show_empty = !self.show_empty;
        self.invalidate_list();
        cx.notify();
    }

    /// `show-archived`: list archived sessions anyway.
    pub(crate) fn step_toggle_archived(&mut self, cx: &mut Context<Self>) {
        self.show_archived = !self.show_archived;
        self.invalidate_list();
        cx.notify();
    }

    /// `sidebar-width:<px>`: a scripted width for the resize screenshots,
    /// clamped and settled exactly like a released drag, minus the pointer.
    pub(crate) fn step_sidebar_width(&mut self, rest: &str, cx: &mut Context<Self>) {
        if let Ok(width) = rest.parse::<f32>() {
            self.resize.width = clamp_sidebar_width(width);
            self.resize.active = false;
            self.resize.persist();
        }
        cx.notify();
    }

    /// `pin`: pin or unpin the active session.
    pub(crate) fn step_pin(&mut self, cx: &mut Context<Self>) {
        if let Some(session_id) = self.active_id(cx) {
            self.toggle_pin(session_id, cx);
        }
    }

    /// `archive`: raise the active session's archive confirmation.
    pub(crate) fn step_archive(&mut self, cx: &mut Context<Self>) {
        if let Some(session_id) = self.active_id(cx) {
            self.open_archive_dialog(session_id, cx);
        }
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed) or after an override changed.
    ///
    /// The same precedence [`SessionEntry::join`] documents, in one place.
    pub(super) fn rejoin(&mut self) {
        self.invalidate_list();
        for entry in &mut self.sessions {
            let meta = self.overrides.get(&entry.id);
            let index = self.index.get(&entry.id);
            let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let derived = meta.and_then(|m| m.derived_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let label = name.or_else(|| index.and_then(IndexEntry::label)).or(derived);
            // Same rule as [`SessionEntry::join`]: a user-given name always
            // earns the first prompt below it, any other label only when it
            // does not already say it.
            let user_named = name.is_some()
                || index
                    .and_then(|i| i.session_name.as_deref())
                    .map(str::trim)
                    .is_some_and(|s| !s.is_empty());
            let text = label.unwrap_or(crate::sidebar::UNNAMED);
            entry.needs_title = label.is_none();
            // A replayed capture names its own row by file, and no source
            // speaks for it: keep that label rather than blanking it to the
            // fallback on every override write.
            match label {
                Some(label) => entry.label = label.to_owned(),
                None if !entry.replayed => entry.label = crate::sidebar::UNNAMED.to_owned(),
                None => {}
            }
            entry.hidden = meta.is_some_and(|m| m.hidden);
            entry.pinned = meta.is_some_and(|m| m.pinned);
            entry.archived = meta.is_some_and(|m| m.archived);
            entry.description = sidebar::describe(meta, index, text, user_named);
            entry.named = name.is_some();
        }
    }

    /// `session/start` in this workspace, on the configured provider.
    ///
    /// A new session is also the moment to re-walk the workspace: files come
    /// and go while the window is open, and the `@` picker should not offer a
    /// path that was deleted an hour ago.
    pub(crate) fn new_session(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.load_menu_sources(cx);
        let (workspace, provider) = (self.workspace(), self.args.provider.clone());
        // `session/start` is the only surface that declares a session's policy
        // up front; `session/setApprovalMode` afterwards is a different thing,
        // and on this server it does not reach `promptUnmatched`.
        let approval_mode = self.args.approval_mode.clone();
        let work = move || {
            client.session_start(&SessionStartParams {
                command_id: new_command_id(),
                workspace_root: Some(workspace),
                provider_id: Some(provider),
                approval_mode,
                ..Default::default()
            })
        };
        self.wire_call_in(cx, work, |this, result, window, cx| match result {
            Ok(started) => {
                this.open(started.session.session_id.clone(), false, window, cx);
                // The result carries the session object `session/started`
                // would have carried, and it is the only place a mode set
                // at start-up is reported: `session/start` with an
                // `approvalMode` raises no `session/approvalModeChanged`,
                // so a session started under one mode drew the chip of
                // another until this was folded.
                if let Some(view) = this.active.clone() {
                    if let Ok(envelope) = serde_json::to_value(&started.session) {
                        view.update(cx, |view, cx| view.seed_session(envelope, cx));
                    }
                }
                this.load_sessions(cx);
            }
            Err(error) => this.report(&error, cx),
        });
    }

    /// `session/resume`, then page the whole transcript in.
    pub(crate) fn resume(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        // A hidden session is never loaded. Hiding is a decision about this
        // window's list, and a list that still opened what it refuses to show
        // would be a list that means nothing. Archived sessions are the same.
        if self.overrides.get(&session_id).is_some_and(|m| m.hidden) && !self.show_hidden {
            return;
        }
        if self.overrides.get(&session_id).is_some_and(|m| m.archived) && !self.show_archived {
            return;
        }
        let Some(client) = self.client.clone() else { return };
        self.open(session_id.clone(), true, window, cx);
        let work = move || {
            client.session_resume(&SessionResumeParams {
                command_id: new_command_id(),
                session_id,
                // History comes through `view/page`, which is the contiguous,
                // ordered, bounded path; resume just attaches.
                exclude_items: Some(true),
                cursor: None,
                history: None,
            })
        };
        self.wire_call(cx, work, |this, result, cx| {
            if let Err(error) = result {
                // A failed switch keeps the old view (C2): only a boot
                // open with nothing behind it clears the centre pane.
                if this.pending_active.take().is_some() {
                    this.pending_ready = false;
                } else {
                    this.active = None;
                }
                this.report(&error, cx);
            }
            cx.notify();
        });
    }

    /// Put a session in the centre pane and subscribe to what it needs help
    /// with.
    pub(super) fn open(&mut self, session_id: String, backfill: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let (provider, workspace) = (self.args.provider.clone(), self.workspace());
        let overlays = self.overlays.clone();
        let host = SessionHost { provider_id: provider, workspace, overlays, capture: self.capture.clone() };
        let view = cx.new(|cx| SessionView::new(session_id, Some(client), host, window, cx));
        view.update(cx, |view, cx| view.load_history(cx));
        // A switch that pages history in does not swap synchronously: the
        // old view keeps rendering until the new view's first backfill batch
        // applies (C2), so no frame flashes the "New session" screen.
        // Backfill failure keeps the old view and reports (see `resume`).
        if backfill && self.active.is_some() {
            view.update(cx, |view, cx| view.backfill(cx));
            let titles: HashMap<String, String> =
                self.sessions.iter().map(|entry| (entry.id.clone(), entry.label.clone())).collect();
            let tier_banner = self.tier_banner();
            view.update(cx, |view, cx| {
                view.set_context(titles, self.user_shell);
                view.set_at_rest(self.still());
                view.set_tier_banner(tier_banner, cx);
            });
            self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
            self.pending_active = Some(view);
            self.pending_ready = false;
            cx.notify();
            return;
        }
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        if backfill {
            view.update(cx, |view, cx| view.backfill(cx));
        }
        let titles: HashMap<String, String> =
            self.sessions.iter().map(|entry| (entry.id.clone(), entry.label.clone())).collect();
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| {
            view.set_context(titles, self.user_shell);
            view.set_at_rest(self.still());
            view.set_tier_banner(tier_banner, cx);
        });
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        self.run_steps(window, cx);
        cx.notify();
    }

    /// Swap the deferred session view in once its backfill landed (C2).
    pub(super) fn swap_pending_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.pending_active.take() else {
            self.pending_ready = false;
            return;
        };
        self.pending_ready = false;
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        self.run_steps(window, cx);
        cx.notify();
    }

    /// What a session cannot decide for itself.
    pub(super) fn on_session_event(
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
            SessionEvent::SignedOut { message } => {
                self.set_dialog(cx, Dialog {
                    title: "Signed out of Muse".into(),
                    detail: format!("Muse refused the turn: {message}"),
                    kind: DialogKind::Warning,
                    primary: "Sign in",
                    action: DialogAction::SignIn,
                    archive_target: None,
                });
            }
            // The child's exit already reached `route`, which owns the reconnect.
            SessionEvent::Closed => {}
            // The deferred switch's first backfill batch applied: mark it and
            // let the next centre frame (which owns a `Window`) swap it in.
            SessionEvent::HistoryReady => {
                if self.pending_active.as_ref().is_some_and(|pending| *pending == view) {
                    self.pending_ready = true;
                    cx.notify();
                }
            }
            SessionEvent::NewSession => self.new_session(cx),
            // The fork result is a resume envelope for the **new** session, so
            // it is already attached: opening it and paging it in is all that
            // is left, and the sidebar re-reads itself because there is now one
            // more session in this workspace.
            SessionEvent::Forked { session_id, session } => {
                let (session_id, envelope) = (session_id.clone(), session.clone());
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open(session_id, true, window, cx);
                        let target = this.pending_active.clone().or_else(|| this.active.clone());
                        if let Some(view) = target {
                            view.update(cx, |view, cx| view.seed_session(envelope, cx));
                        }
                        this.load_sessions(cx);
                    });
                }));
            }
            SessionEvent::Logout => self.logout(cx),
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
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.rename_session(session_id, name.clone(), cx);
                }
            }
            SessionEvent::RenameStart => {
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.tasks.push(cx.spawn(async move |this, cx| {
                        let _ = this.update_in(cx, |this, window, cx| this.start_rename(session_id, window, cx));
                    }));
                }
            }
            SessionEvent::Hide => {
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.hide_session(session_id, cx);
                }
            }
            SessionEvent::ToggleEmpty => {
                self.show_empty = !self.show_empty;
                self.invalidate_list();
            }
            SessionEvent::Resume => self.open_palette(PaletteKind::Resume, cx),
            SessionEvent::ForkPicker => self.open_palette(PaletteKind::Fork, cx),
            SessionEvent::Search => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.open_search(window, cx));
                }));
            }
        }
        cx.notify();
    }

    /// A failed command that the application, rather than a session, issued.
    pub(super) fn report(&mut self, error: &MuseError, cx: &mut Context<Self>) {
        let title = conn::title(error);
        let dialog = Dialog {
            title,
            detail: error.to_string(),
            kind: DialogKind::Error,
            primary: match conn::severity(error) {
                Severity::Dialog => "Reconnect",
                Severity::Banner => "Dismiss",
            },
            action: match conn::severity(error) {
                Severity::Dialog => DialogAction::Reconnect,
                Severity::Banner => DialogAction::Dismiss,
            },
            archive_target: None,
        };
        self.set_dialog(cx, dialog);
    }
}
