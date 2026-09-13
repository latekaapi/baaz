//! The session list's lifecycle: reading the index, listing, starting,
//! resuming and opening sessions, the immediate swap that answers a click on
//! its own frame, and the MRU of parked views that makes reopening instant.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// How many parked session views the MRU keeps (folds keep an MRU of eight
/// too, so a parked view's own fold never grows past it either).
const SESSION_CACHE_LIMIT: usize = 8;

/// One `--sidebar-fixture` row: the wire's shape, spelled as JSON.
///
/// `label` stands in for the index title a live row would carry.
#[derive(serde::Deserialize)]
struct FixtureSession {
    /// The Muse session id.
    #[serde(rename = "sessionId")]
    session_id: String,
    /// The session's workspace root; relative roots resolve against the
    /// launch directory at load.
    #[serde(rename = "workspaceRoot")]
    workspace_root: String,
    /// The row's label.
    label: String,
    /// RFC3339 last activity.
    #[serde(rename = "updatedAt")]
    updated_at: String,
    /// Completed turns.
    #[serde(rename = "turnCount", default)]
    turn_count: u64,
    /// `running` for a live session, anything else for a settled one.
    #[serde(default = "fixture_settled")]
    status: String,
}

/// [`FixtureSession::status`] without the field: settled, never running.
fn fixture_settled() -> String {
    "idle".to_owned()
}

/// A fixture row as the wire would have listed it: joined through
/// [`SessionEntry::join`] with a synthetic `Session`, so resolution and
/// grouping read exactly what a live list would have given them — then
/// labelled with the fixture's own label.
fn fixture_entry(row: &FixtureSession, launch: &std::path::Path, projects: &Projects) -> SessionEntry {
    let root = std::path::PathBuf::from(&row.workspace_root);
    let root = if root.is_absolute() { root } else { launch.join(root) };
    let session = muse_client::schema::Session {
        active_turn_id: None,
        approval_mode: None,
        created_at: row.updated_at.clone(),
        forked_from: None,
        model_id: None,
        path: String::new(),
        provider_id: None,
        session_id: row.session_id.clone(),
        status: if row.status == "running" {
            muse_client::schema::SessionStatus::Running
        } else {
            muse_client::schema::SessionStatus::Idle
        },
        turn_count: row.turn_count,
        updated_at: row.updated_at.clone(),
        workspace_root: Some(root.to_string_lossy().into_owned()),
    };
    let mut entry = SessionEntry::join(&session, None, None, projects);
    // The fixture's own label, kept like a replayed row's: no index or
    // store source speaks for a scripted id, so a later `rejoin` must not
    // blank it back to the fallback.
    entry.label = row.label.clone();
    entry.replayed = true;
    entry
}

impl Harness {
    // -------------------------------------------------------------- sessions

    /// Read the local index once at boot; it is a cache, not a source of truth.
    pub(super) fn load_index(&mut self, cx: &mut Context<Self>) {
        self.wire_call(cx, index::read, |this, index, cx| {
            // Settled, even on an empty read: row visibility comes from the
            // index, and the sidebar waits for it before debuting groups.
            this.index_loaded = true;
            this.index = index;
            this.rejoin();
            this.rebuild_search_index(cx);
            cx.notify();
        });
    }

    /// `session/list`, unfiltered and paged: one window over every
    /// workspace, so the filter that used to hide other workspaces' sessions
    /// is gone and the cursor is followed until the server says `None`.
    /// Everything lands on the one background task behind this
    /// [`crate::wire::WireCall`].
    pub(crate) fn load_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let projects = self.projects.clone();
        let work = move || {
            let mut sessions = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let page = client.session_list(&SessionListParams {
                    cursor,
                    limit: Some(200),
                    ..Default::default()
                })?;
                sessions.extend(page.sessions);
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            // The branch behind every project group row, read while already
            // off the UI thread.
            let mut branches = HashMap::new();
            for project in &projects.projects {
                if let Some(branch) = crate::projects::branch_of(&project.root) {
                    branches.insert(project.id.clone(), branch);
                }
            }
            Ok::<_, MuseError>((sessions, branches))
        };
        self.wire_call_in(cx, work, |this, result, window, cx| {
            this.sessions_loaded = true;
            if let Ok((sessions, branches)) = result {
                this.invalidate_list();
                let projects = this.projects.clone();
                let wire: Vec<SessionEntry> = sessions
                    .iter()
                    .map(|s| {
                        SessionEntry::join(
                            s,
                            this.index.get(&s.session_id),
                            this.overrides.get(&s.session_id),
                            &projects,
                        )
                    })
                    .collect();
                // Local rows whose id the reply does not contain survive;
                // a local whose id is listed is replaced by its wire row.
                this.sessions = sidebar::merge_session_list(wire, &this.sessions);
                this.branches = branches;
                this.derive_titles(cx);
            }
            this.open_boot_session(window, cx);
            cx.notify();
        });
    }

    /// `--sidebar-fixture`: merge scripted rows into the session list as if
    /// the wire had listed them. A missing or unparseable file is no rows,
    /// like an empty list reply — a capture aid must never fail a boot.
    pub(super) fn apply_sidebar_fixture(&mut self) {
        let Some(path) = self.args.sidebar_fixture.clone() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(rows) = serde_json::from_str::<Vec<FixtureSession>>(&text) else { return };
        let launch = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        for row in &rows {
            let entry = fixture_entry(row, &launch, &self.projects);
            self.sessions.retain(|e| e.id != entry.id);
            self.sessions.push(entry);
        }
        self.invalidate_list();
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
    pub(crate) fn rejoin(&mut self) {
        self.invalidate_list();
        for entry in &mut self.sessions {
            // The adoption may have changed under the row: re-resolve every
            // entry, including local and replayed ones, whose labels keep
            // their own rules below.
            let meta = self.overrides.get(&entry.id);
            entry.project = self
                .projects
                .resolve(entry.workspace.as_deref(), meta.and_then(|m| m.project.as_deref()))
                .map(|p| p.id.clone());
            if entry.local {
                // A local row predates the wire list: no index or store
                // source speaks for it, so its label ("New session", or the
                // first prompt set on `turn/started`) stands until the wire
                // lists it. Only an explicit rename overrides it; the flags
                // still follow the store, so pin and hide work meanwhile.
                let meta = self.overrides.get(&entry.id);
                entry.hidden = meta.is_some_and(|m| m.hidden);
                entry.pinned = meta.is_some_and(|m| m.pinned);
                entry.archived = meta.is_some_and(|m| m.archived);
                if let Some(name) =
                    meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty())
                {
                    entry.label = name.to_owned();
                    entry.named = true;
                }
                continue;
            }
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

    /// `session/start` in the current project, on the configured provider.
    ///
    /// A new session is also the moment to re-walk the workspace: files come
    /// and go while the window is open, and the `@` picker should not offer a
    /// path that was deleted an hour ago. With no adoption there is nowhere
    /// to start, so nothing starts — package 2's hero owns that state.
    pub(crate) fn new_session(&mut self, cx: &mut Context<Self>) {
        let current = self.current_project_id();
        self.new_session_in(current, cx);
    }

    /// `session/start` in `project`, which becomes current first so the new
    /// view, its workspace and the next ⌘N all agree about where it started.
    /// A `None` or unknown project is [`Self::new_session`] with no adoption:
    /// nothing starts.
    pub(crate) fn new_session_in(&mut self, project: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = project.as_deref().filter(|id| self.projects.find(id).is_some()) {
            self.projects.touch(id);
            self.projects.current = Some(id.to_owned());
            self.current_project = Some(id.to_owned());
            projects::write(&self.projects);
        }
        let Some(client) = self.client.clone() else { return };
        let current = self.current_project_id();
        // `session/start` is the only surface that declares a session's
        // policy up front; `session/setApprovalMode` afterwards is a
        // different thing, and on this server it does not reach
        // `promptUnmatched`. The params carry the project's root and its
        // defaults, with the command line's approval mode winning.
        let Some(params) =
            projects::start_params(&self.projects, current.as_deref(), &self.args.provider, self.args.approval_mode.clone())
        else {
            return;
        };
        let effort = current
            .as_deref()
            .and_then(|id| self.projects.find(id))
            .and_then(|p| p.defaults.effort.as_deref())
            .and_then(projects::parse_effort);
        self.load_menu_sources(std::path::PathBuf::from(self.workspace()), cx);
        let work = move || client.session_start(&params);
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(started) => {
                let session_id = started.session.session_id.clone();
                this.open(session_id.clone(), false, window, cx);
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
                // The wire lists a session only after its log flushes on
                // `turn/completed`, so its row appears at once as a local
                // one — labelled "New session" until the first
                // `turn/started` titles it from the prompt — and the next
                // `load_sessions` keeps it until the wire lists its id.
                let mut local = SessionEntry::join(&started.session, None, None, &this.projects);
                local.local = true;
                this.sessions.retain(|entry| entry.id != local.id);
                this.sessions.push(local);
                this.invalidate_list();
                // The session groups under the project it started in, even
                // when its folder later proves to be a worktree of that root.
                this.set_override(&session_id, |meta| meta.project = current.clone(), cx);
                // The local row did not exist when the view activated, so
                // its project name arrives now.
                let name = this.project_name_for(&session_id);
                if let Some(view) = this.active.clone() {
                    view.update(cx, |view, _| view.set_project_name(name));
                }
                // The project's effort rides along before the first turn.
                if let Some(effort) = effort {
                    if let Some(view) = this.active.clone() {
                        view.update(cx, |view, cx| view.set_initial_effort(Some(effort), cx));
                    }
                }
                this.load_sessions(cx);
            }
            Err(error) => this.report(&error, cx),
        });
    }

    /// `session/resume`, then stream the transcript in.
    ///
    /// The target is recorded and shown at once: a cached view reopens
    /// instantly and tops up from its last cursor, otherwise a fresh view
    /// opens on its loading row and pages stream in behind it. Either way a
    /// failed `session/resume` keeps the new view and reports.
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
        self.adopt_session_project(&session_id);
        let Some(client) = self.client.clone() else { return };
        crate::log::trace_reset();
        crate::log::trace_mark(&format!("resume {}", session_id.chars().take(8).collect::<String>()));
        self.pending_id = Some(session_id.clone());
        if let Some(view) = self.cache_take(&session_id) {
            crate::log::trace_mark("cache-hit");
            self.activate(view, window, cx);
            crate::log::trace_mark("swap");
            crate::log::trace_arm_first_frame();
            self.top_up(cx);
            return;
        }
        crate::log::trace_mark("cache-miss");
        self.open(session_id.clone(), true, window, cx);
        crate::log::trace_mark("swap");
        crate::log::trace_arm_first_frame();
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
            match &result {
                Ok(_) => crate::log::trace_mark("resume-ack"),
                Err(error) => {
                    crate::log::trace_mark("resume-ack-err");
                    this.report(error, cx);
                }
            }
            cx.notify();
        });
    }

    /// The root a session of this id runs in: its own row's workspace when
    /// the list knows it, else where the next session would go. Anything
    /// about a session reads this; anything about "where the next session
    /// goes" reads the current project directly.
    fn session_workspace(&self, session_id: &str) -> String {
        self.sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.workspace.clone())
            .unwrap_or_else(|| self.workspace())
    }

    /// A session view opened: the current project becomes that session's
    /// project when it has one. Touched and written, so the next boot and
    /// the next ⌘N start where this session is.
    fn adopt_session_project(&mut self, session_id: &str) {
        let project = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.clone())
            .filter(|id| self.projects.find(id).is_some());
        let Some(id) = project else { return };
        self.projects.touch(&id);
        self.projects.current = Some(id.clone());
        self.current_project = Some(id);
        projects::write(&self.projects);
    }

    /// Put a fresh session view in the centre pane now and subscribe to what
    /// it needs help with. The swap is immediate — the view, or its loading
    /// row while the first page is still on the wire — never the old view
    /// held past its switch.
    pub(super) fn open(&mut self, session_id: String, backfill: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let (provider, workspace) = (self.args.provider.clone(), self.session_workspace(&session_id));
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let overlays = self.overlays.clone();
        let host = SessionHost { provider_id: provider, workspace, overlays, capture: self.capture.clone() };
        let view = cx.new(|cx| SessionView::new(session_id.clone(), Some(client), host, window, cx));
        view.update(cx, |view, cx| view.load_history(cx));
        self.pending_id = Some(session_id.clone());
        self.activate(view, window, cx);
        self.adopt_session_project(&session_id);
        if backfill {
            if let Some(view) = self.active.clone() {
                view.update(cx, |view, cx| view.backfill(cx));
            }
        }
        cx.notify();
    }

    /// The session's project display name, for the empty state: the row's
    /// project resolved to a name, so a rename shows without reopening.
    pub(crate) fn project_name_for(&self, session_id: &str) -> Option<String> {
        self.sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.as_deref())
            .and_then(|id| self.projects.find(id))
            .map(|p| p.name.clone())
    }

    /// Make `view` the centre pane now: park the outgoing view in the MRU,
    /// point the event subscription at the new one, refresh its context, and
    /// run anything scripted. The frame after this draws the new view.
    fn activate(&mut self, view: Entity<SessionView>, window: &mut Window, cx: &mut Context<Self>) {
        let project_name = {
            let id = view.read(cx).session_id.clone();
            self.project_name_for(&id)
        };
        view.update(cx, |view, _| view.set_project_name(project_name));
        self.park_active(cx);
        // A parked view's client predates a reconnect; the current child is
        // the one that can page.
        if let Some(client) = self.client.clone() {
            view.update(cx, |view, _| view.reconnected(client));
        }
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
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

    /// Park the outgoing view in the MRU: its event subscription drops with
    /// `subscriptions`, while its fold, scroll position and draft ride along
    /// in the entity. Hidden, archived and replayed views are never cached.
    fn park_active(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.take() else { return };
        let session_id = view.read(cx).session_id.clone();
        let cacheable = !view.read(cx).is_replay()
            && !self.overrides.get(&session_id).is_some_and(|m| m.hidden)
            && !self.overrides.get(&session_id).is_some_and(|m| m.archived);
        if cacheable {
            self.session_cache.retain(|(id, _)| *id != session_id);
            self.session_cache.insert(0, (session_id, view));
            self.session_cache.truncate(SESSION_CACHE_LIMIT);
        }
    }

    /// Take a parked view back out of the MRU.
    fn cache_take(&mut self, session_id: &str) -> Option<Entity<SessionView>> {
        let ix = self.session_cache.iter().position(|(id, _)| id == session_id)?;
        Some(self.session_cache.remove(ix).1)
    }

    /// Top a reopened cached view up from its last cursor: re-attach with the
    /// reconnect procedure (`docs/01-transport.md` §3), which serves
    /// `history.mode: "none"` and streams only the suffix. The view is
    /// already on screen, so the suffix folds in behind it through the live
    /// stream; no `view/page` is needed, and none would serve: a forward page
    /// anchored at the cached head answers `notFound/missingAnchor`.
    fn top_up(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        let cursor = view.read(cx).last_cursor();
        let topped = session_id.clone();
        let work = move || {
            client.session_resume(&SessionResumeParams {
                command_id: new_command_id(),
                session_id,
                cursor,
                exclude_items: Some(true),
                history: None,
            })
        };
        self.wire_call_in(cx, work, move |this, result, window, cx| {
            match result {
                Ok(resumed) => {
                    crate::log::trace_mark(&format!("resume-ack mode={:?}", resumed.history.mode));
                    // Only when the topped-up view is still the open one; a
                    // further switch parked it, and its own top-up owns it.
                    // The streamed suffix is already folding through the live
                    // stream; all that is left is the tail pin.
                    let still_open = this
                        .active
                        .clone()
                        .filter(|view| view.read(cx).session_id == topped);
                    if let Some(view) = still_open {
                        view.update(cx, |view, cx| {
                            crate::log::trace_mark("HistoryReady");
                            view.follow_tail(cx);
                        });
                    }
                }
                // The server no longer knows the cached cursor (`-32011
                // notFound`, "unknown cursor anchor"): the parked view is
                // stale, not the person's problem. Drop it and open the
                // session afresh — the loading row, then the pages — instead
                // of a dialog.
                Err(error) if error.kind() == Some(&muse_client::schema::ErrorKind::NotFound) => {
                    crate::log::trace_mark("resume-ack-stale");
                    crate::harness_log!("cached view of {topped} is stale ({error}); reopening");
                    let still_open = this.active.as_ref().is_some_and(|view| view.read(cx).session_id == topped);
                    if still_open {
                        this.open(topped.clone(), true, window, cx);
                        // `open` parked the stale view; it must not come back.
                        this.session_cache.retain(|(id, _)| *id != topped);
                    }
                }
                Err(error) => {
                    crate::log::trace_mark("resume-ack-err");
                    this.report(&error, cx);
                }
            }
            cx.notify();
        });
    }

    /// Store a picked default on a session's project: resolve the session to
    /// its project — its stored id first, then its own workspace's root —
    /// edit that adoption's defaults and persist. A session that resolves to
    /// no project changes nothing.
    fn note_project_default(&mut self, session_id: &str, edit: impl FnOnce(&mut crate::projects::ProjectDefaults)) {
        let project = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| self.projects.resolve(e.workspace.as_deref(), e.project.as_deref()))
            .map(|p| p.id.clone());
        let Some(id) = project else { return };
        if let Some(project) = self.projects.projects.iter_mut().find(|p| p.id == id) {
            edit(&mut project.defaults);
        }
        projects::write(&self.projects);
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
            // The backfill's first page applied: pin the tail while later
            // pages land. Anything but the open view is a parked chain
            // warming the cache, and its own completion already pinned it.
            SessionEvent::HistoryReady => {
                if self.active.as_ref().is_some_and(|active| *active == view) {
                    view.update(cx, |view, cx| view.follow_tail(cx));
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
                        // `open` swaps synchronously, so the fork view is the
                        // active one by the time its envelope is seeded.
                        this.open(session_id, true, window, cx);
                        if let Some(view) = this.active.clone() {
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
            // The person picked a model, effort or approval mode in a
            // session: it becomes that session's project defaults, so the
            // next session there starts with it. Scripted `setmodel:` /
            // `setmode:` steps and plan-mode toggles never emit these, so
            // scripted and transient choices stay out of the defaults.
            SessionEvent::ModelSelected { model_id } => {
                let session_id = view.read(cx).session_id.clone();
                self.note_project_default(&session_id, |defaults| {
                    defaults.model_id = Some(model_id.clone());
                });
            }
            SessionEvent::EffortSelected { effort } => {
                let session_id = view.read(cx).session_id.clone();
                self.note_project_default(&session_id, |defaults| {
                    defaults.effort = effort.clone();
                });
            }
            SessionEvent::ModeSelected { mode } => {
                let session_id = view.read(cx).session_id.clone();
                self.note_project_default(&session_id, |defaults| {
                    defaults.approval_mode = Some(mode.clone());
                });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_row() -> FixtureSession {
        serde_json::from_str(
            r#"{"sessionId": "s1", "workspaceRoot": "fixtures/ws/acme-web", "label": "Fix the header",
                "updatedAt": "2026-09-13T10:00:00Z", "turnCount": 3, "status": "running"}"#,
        )
        .expect("the documented fixture shape parses")
    }

    #[test]
    fn a_fixture_row_joins_like_a_wire_row() {
        let row = fixture_row();
        assert_eq!(row.turn_count, 3);
        assert_eq!(row.status, "running");
        let launch = std::env::temp_dir().join(format!("harness-fixture-{}", std::process::id()));
        let mut projects = Projects::default();
        let root = launch.join("fixtures/ws/acme-web");
        std::fs::create_dir_all(&root).expect("temp workspace");
        let id = projects.add(&root).id.clone();
        let entry = fixture_entry(&row, &launch, &projects);
        // The label is the fixture's own; the workspace and project are
        // what `join` resolved, exactly as for a listed session.
        assert_eq!(entry.id, "s1");
        assert_eq!(entry.label, "Fix the header");
        assert!(entry.running);
        assert_eq!(entry.turns, 3);
        assert_eq!(entry.project.as_deref(), Some(id.as_str()));
        // Kept like a replayed row's, so a later `rejoin` keeps it.
        assert!(entry.replayed);
        let _ = std::fs::remove_dir_all(&launch);
    }

    #[test]
    fn an_absolute_root_survives_and_a_relative_one_joins_the_launch_dir() {
        let mut row = fixture_row();
        row.workspace_root = "/private/tmp/h4ws".into();
        let entry = fixture_entry(&row, std::path::Path::new("/repo"), &Projects::default());
        assert_eq!(entry.workspace.as_deref(), Some("/private/tmp/h4ws"));
        assert!(entry.project.is_none());
    }
}
