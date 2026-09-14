//! The session list's lifecycle: reading the index, listing, starting,
//! resuming and opening sessions, the immediate swap that answers a click on
//! its own frame, and the MRU of parked views that makes reopening instant.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// How many parked session views the MRU keeps (folds keep an MRU of eight
/// too, so a parked view's own fold never grows past it either).
const SESSION_CACHE_LIMIT: usize = 8;

/// What `new_session_in` does about the target project's draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DraftDecision {
    /// Reopen this draft session's view; nothing starts.
    Reuse(String),
    /// Start a session; first forget this stale draft name, if any.
    Start { stale: Option<String> },
}

/// The draft map's decision, in pure form: reuse the project's draft while
/// `live` still calls it unsent, else start — pruning a name the app can no
/// longer open.
pub(crate) fn draft_decision(
    drafts: &HashMap<String, String>,
    project: &str,
    live: impl Fn(&str) -> bool,
) -> DraftDecision {
    match drafts.get(project) {
        Some(named) if live(named) => DraftDecision::Reuse(named.clone()),
        Some(named) => DraftDecision::Start { stale: Some(named.clone()) },
        None => DraftDecision::Start { stale: None },
    }
}

/// Which parked ids survive the MRU cap, in order: drafts are never evicted,
/// everything else keeps most-recent-first up to `limit`.
pub(crate) fn evict_parked(
    ids: &[String],
    drafts: &std::collections::HashSet<String>,
    limit: usize,
) -> Vec<String> {
    let mut kept = 0usize;
    ids.iter()
        .filter(|id| {
            if drafts.contains(*id) {
                true
            } else {
                kept += 1;
                kept <= limit
            }
        })
        .cloned()
        .collect()
}

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
        branch: None,
        created_at: row.updated_at.clone(),
        first_user_prompt: None,
        forked_from: None,
        model_id: None,
        name: None,
        path: String::new(),
        provider_id: None,
        session_id: row.session_id.clone(),
        status: if row.status == "running" {
            muse_client::schema::SessionStatus::Running
        } else {
            muse_client::schema::SessionStatus::Idle
        },
        title: None,
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
                self.new_session(window, cx);
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

    /// `wheel:<dy>`: dispatch one synthetic wheel event at the window centre
    /// and log the transcript's pixel offset before and after — the
    /// palette-scroll instrument (owner round 4): over the open palette the
    /// palette's list scrolls and the transcript never moves, while over the
    /// bare transcript the same wheels move it. Free: no turn, no wire.
    pub(crate) fn step_wheel(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let dy: f32 = rest.trim().parse().unwrap_or(0.0);
        let before = self.active.as_ref().map(|view| view.read(cx).bench_list_px()).unwrap_or(0.0);
        let size = window.bounds().size;
        window.dispatch_event(
            PlatformInput::ScrollWheel(ScrollWheelEvent {
                position: point(size.width * 0.5, size.height * 0.5),
                delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                ..Default::default()
            }),
            cx,
        );
        // Owner round 4 §2: the handler only accumulates, so without this
        // the read below would always equal `before` and the log could no
        // longer tell a moved transcript from an occluded one. Apply the
        // frame's drain eagerly — with nothing pending it is gpui's own
        // early-return, so over the palette this still logs X->X.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, _| view.drain_pending_wheel());
        }
        let after = self.active.as_ref().map(|view| view.read(cx).bench_list_px()).unwrap_or(0.0);
        crate::harness_log!("wheel dy={dy} list_px={before}->{after}");
    }

    /// `centre`: the open-flicker instrument (owner round 6). Log
    /// `harness: centre hero=<hero> loading=<loading>`: the new-session
    /// hero vs loading-row paints since the last call. An existing session
    /// opened through `open:`/`click:` must read `hero=0`; a draft opened
    /// through `new` still reads `hero>0`. Free: no turn, no wire.
    pub(crate) fn step_centre(&mut self) {
        let (hero, loading) = crate::session::take_centre_paints();
        crate::harness_log!("centre hero={hero} loading={loading}");
    }

    /// `sidebar-wheel:<dy>[,n]`: the sidebar-scroll instrument (owner round
    /// 5 §A1.6). Dispatch n synthetic wheel events (default 1) at a sidebar
    /// point — x = 100 sits in the sessions list at the default width, y =
    /// 40 % of the window height — synchronously, so one step lands between
    /// two frames: the burst shape a trackpad really delivers, which
    /// `--steps wheel:` (window centre, transcript) can never hit. Logs the
    /// sidebar offset before/after plus the pane and root renders drained
    /// since the last call — pair with `wait:<ms>` and a trailing
    /// `sidebar-wheel:0,0` to read a burst's renders after its frames paint
    /// (renders land on frames, not inside the dispatch). Free: no turn, no
    /// wire.
    pub(crate) fn step_sidebar_wheel(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (dy_text, n_text) = rest.split_once(',').unwrap_or((rest, "1"));
        let dy: f32 = dy_text.trim().parse().unwrap_or(0.0);
        let n: usize = n_text.trim().parse().unwrap_or(1);
        // Drained first: what this line reports is everything since the
        // previous drain, i.e. the earlier burst's frames once a `wait:`
        // separates the two calls.
        let pane = crate::sidebar_view::take_sidebar_pane_renders();
        let root = crate::sidebar_view::take_harness_root_renders();
        let drains = crate::sidebar_view::take_sidebar_wheel_drains();
        let before = self.sidebar_list_top();
        let size = window.bounds().size;
        let at = point(px(100.0), size.height * 0.4);
        for _ in 0..n {
            window.dispatch_event(
                PlatformInput::ScrollWheel(ScrollWheelEvent {
                    position: at,
                    delta: ScrollDelta::Pixels(point(px(0.0), px(dy))),
                    ..Default::default()
                }),
                cx,
            );
        }
        let after = self.sidebar_list_top();
        let rows = self.visible_sessions(cx).len();
        let centre = crate::session::take_centre_renders();
        // The list walks down as `item_ix` grows; a click that moves
        // nothing reads the same index twice, an outside open the minimum
        // travel to its row — the probe semantics the div offset had.
        crate::harness_log!(
            "sbwheel dy={dy} n={n} sidebar_ix={bix}+{boff:.1}->{aix}+{aoff:.1} rows={rows} pane={pane} root={root} centre={centre} drains={drains}",
            bix = before.item_ix,
            boff = f32::from(before.offset_in_item),
            aix = after.item_ix,
            aoff = f32::from(after.offset_in_item),
        );
    }

    /// `resize-begin:<x>` / `resize-move:<x>` / `resize-end`: the scripted
    /// resize drag (owner round 6). They drive the same [`Harness`] handlers
    /// the divider strip calls — `begin_resize` / `drag_resize` /
    /// `end_resize` — so an overlap probe (a press with an outside open
    /// still armed, moves with frames interleaved) exercises the real press
    /// disarm and the in-flight install gate. Each logs `harness: rsdrag`
    /// with the width, the sidebar list index, the reveal arm, the scrolled
    /// flag and the drag: a probe the drag must not move keeps every
    /// line's `sidebar_ix` equal.
    /// `resize-sweep:<to_w,step_px>`: march the divider toward `to_w` one
    /// `step_px` per rendered frame (scripting only, owner round 6) — the
    /// display link's pace on a real display — logging
    /// `harness: rssweep w=<width> pane=<pane> root=<root> rehint=<0/1>` per
    /// tick. Like a press it disarms the reveal and owns the list while it
    /// runs; it never persists.
    pub(crate) fn step_resize_sweep(&mut self, rest: &str, cx: &mut Context<Self>) {
        let (to, step) = rest.split_once(',').unwrap_or((rest, "4"));
        let to: f32 = to.trim().parse().unwrap_or(aui::shell::SIDEBAR_WIDTH);
        let step: f32 = step.trim().parse().unwrap_or(4.0);
        self.reveal = None;
        self.sidebar_user_scrolled = true;
        self.resize.sweep = Some(crate::resize::ResizeSweep::new(to, step));
        self.resize.active = true;
        self.resize.scripted = true;
        cx.notify();
    }

    pub(crate) fn step_resize_drag(&mut self, phase: &str, rest: &str, cx: &mut Context<Self>) {
        let x: f32 = rest.trim().parse().unwrap_or(0.0);
        match phase {
            "begin" => {
                self.begin_resize(x, cx);
                // No pointer behind a scripted drag, so `on_frame`'s
                // release-outside-the-window rule must not end it between
                // steps (a headless window is never active).
                self.resize.scripted = true;
            }
            "move" => self.drag_resize(x, cx),
            _ => self.end_resize(cx),
        }
        // A probe the drag must not move keeps every line's index equal;
        // the list walks down as `item_ix` grows.
        let top = self.sidebar_list_top();
        crate::harness_log!(
            "rsdrag {phase} w={} sidebar_ix={}+{:.1} reveal={:?} scrolled={} ract={}",
            self.resize.width,
            top.item_ix,
            f32::from(top.offset_in_item),
            self.reveal,
            self.sidebar_user_scrolled,
            self.resize.active
        );
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

    /// `new`: the same as ⌘N, in the current project. `new:<project name>`:
    /// the group row's `+` for the project named — an unknown name only logs.
    pub(crate) fn step_new(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = rest.trim();
        if name.is_empty() {
            self.new_session(window, cx);
            return;
        }
        match self.projects.projects.iter().find(|p| p.name == name).map(|p| p.id.clone()) {
            Some(id) => self.new_session_in(Some(id), window, cx),
            None => crate::harness_log!("unknown project `{name}`"),
        }
    }

    /// `pin`: pin or unpin the active session.
    /// `pin`: toggle the active session's pin; `pin:<id>`: toggle that row's.
    /// The id form is for captures: the replay row lives in closed Other, so
    /// pinning it shows nothing — `pin:s-web-1` pins a row on screen.
    pub(crate) fn step_pin(&mut self, rest: &str, cx: &mut Context<Self>) {
        let id =
            if rest.is_empty() { self.active_id(cx) } else { Some(rest.to_owned()) };
        if let Some(session_id) = id {
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
    pub(crate) fn new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.current_project_id();
        self.new_session_in(current, window, cx);
    }

    /// `session/start` in `project`, which becomes current first so the new
    /// view, its workspace and the next ⌘N all agree about where it started.
    /// A `None` or unknown project is [`Self::new_session`] with no adoption:
    /// nothing starts.
    ///
    /// An unsent session has no sidebar row: while the target project's draft
    /// is still unsent this reopens that view instead of starting another
    /// session, so repeated clicks never create more than one session per
    /// project.
    pub(crate) fn new_session_in(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = project.as_deref().filter(|id| self.projects.find(id).is_some()) {
            self.projects.touch(id);
            self.projects.current = Some(id.to_owned());
            self.current_project = Some(id.to_owned());
            projects::write(&self.projects);
        }
        let current = self.current_project_id();
        // A live draft in the target project is the session: reopen its view
        // without touching the wire.
        if let Some(id) = current.clone() {
            match draft_decision(&self.drafts, &id, |candidate| self.is_live_draft(candidate, cx)) {
                DraftDecision::Reuse(draft_id) => {
                    if self.open_draft(&draft_id, window, cx) {
                        return;
                    }
                    // Named but viewless: the MRU never evicts a draft, so
                    // this is unreachable — drop the stale name and start
                    // below rather than strand a retarget.
                    self.drafts.remove(&id);
                }
                DraftDecision::Start { stale: Some(_) } => {
                    self.drafts.remove(&id);
                }
                DraftDecision::Start { stale: None } => {}
            }
        }
        let Some(client) = self.client.clone() else {
            // Scripted chrome (`--no-connect` / `--replay`): no child to
            // start a session on, so the draft opens as a local view. Its id
            // is deterministic per project so captures read the same.
            self.open_local_draft(window, cx);
            return;
        };
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
        let started_project = current.clone();
        self.load_menu_sources(std::path::PathBuf::from(self.workspace()), cx);
        let work = move || client.session_start(&params);
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(started) => {
                let session_id = started.session.session_id.clone();
                this.open(session_id.clone(), false, false, window, cx);
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
                // No row until the first send: the wire lists a session
                // only after its log flushes on `turn/completed`, and the
                // `turn/started` handler inserts the local row meanwhile.
                // The session groups under the project it started in, even
                // when its folder later proves to be a worktree of that root.
                this.set_override(&session_id, |meta| meta.project = current.clone(), cx);
                // The row does not exist when the view activates, so its
                // project name arrives now.
                let name = this.project_name_for(&session_id);
                if let Some(view) = this.active.clone() {
                    view.update(cx, |view, cx| {
                        view.set_project_name(name);
                        // The cached centre reuses a clean view: push the
                        // repaint with the name (owner round 6, part 4).
                        cx.notify();
                    });
                }
                // The project's effort rides along before the first turn.
                if let Some(effort) = effort {
                    if let Some(view) = this.active.clone() {
                        view.update(cx, |view, cx| view.set_initial_effort(Some(effort), cx));
                    }
                }
                // A retargeted draft lands in the session it was moved to.
                let moving = this.pending_draft.take();
                let retarget = moving.is_some();
                if let Some(view) =
                    this.active.clone().filter(|view| view.read(cx).session_id == session_id)
                {
                    Self::land_moving_draft(&view, moving, window, cx);
                } else if moving.is_some() {
                    this.pending_draft = moving;
                }
                if let Some(id) = started_project.clone() {
                    this.drafts.insert(id.clone(), session_id);
                    crate::harness_log!(
                        "session/start project={} reason={}",
                        id,
                        if retarget { "retarget" } else { "no-draft" }
                    );
                }
                this.load_sessions(cx);
            }
            Err(error) => this.report(&error, cx),
        });
    }

    /// Whether `session_id` still names an unsent draft: a row with zero
    /// turns and no name, or — drafts have no row — a live view whose fold
    /// holds no turns and nobody named. A replayed row is never a draft.
    /// (A turn in flight is checked by the callers that move content.)
    pub(crate) fn is_live_draft(&self, session_id: &str, cx: &gpui::App) -> bool {
        if let Some(entry) = self.sessions.iter().find(|e| e.id == session_id) {
            return entry.turns == 0 && !entry.named && !entry.replayed;
        }
        if self
            .overrides
            .get(session_id)
            .and_then(|m| m.name.clone())
            .is_some_and(|n| !n.trim().is_empty())
        {
            return false;
        }
        let in_centre =
            self.active.clone().filter(|view| view.read(cx).session_id == session_id);
        let parked = self
            .session_cache
            .iter()
            .find(|(id, _)| id == session_id)
            .map(|(_, view)| view.clone());
        let Some(view) = in_centre.or(parked) else { return false };
        view.read(cx).session().is_none_or(|session| session.turns.is_empty())
    }

    /// Reopen a live draft's view: the active one when it is already open,
    /// else the parked one out of the MRU. A retargeted draft in flight
    /// lands in it when it holds nothing. Returns whether the view was
    /// still around to open.
    fn open_draft(&mut self, draft_id: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let moving = self.pending_draft.take();
        if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == draft_id) {
            if let Some(view) = self.active.clone() {
                Self::land_moving_draft(&view, moving, window, cx);
            }
            return true;
        }
        let Some(view) = self.cache_take(draft_id) else {
            self.pending_draft = moving;
            return false;
        };
        Self::land_moving_draft(&view, moving, window, cx);
        self.activate(view, false, window, cx);
        true
    }

    /// Land a retargeted draft in its new session: moved content wins an
    /// empty composer, and a composer that already holds something keeps it —
    /// the moved text is dropped rather than clobbering what was there.
    fn land_moving_draft(
        view: &Entity<SessionView>,
        moving: Option<Draft>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(draft) = moving else { return };
        view.update(cx, |view, vc| {
            if view.draft_content_empty(vc) {
                view.put_draft(draft, window, vc);
            } else {
                crate::harness_log!(
                    "draft retarget dropped: the picked project's draft already holds text"
                );
            }
        });
    }

    /// The replay-only stand-in for `session/start`: open the current
    /// project's draft as a local view with no child behind it. Recorded in
    /// `drafts` like a started session, carrying the project's effort and a
    /// retargeted draft when one is in flight.
    fn open_local_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.current_project_id() else { return };
        if projects::start_params(
            &self.projects,
            Some(id.as_str()),
            &self.args.provider,
            self.args.approval_mode.clone(),
        )
        .is_none()
        {
            return;
        }
        let moving = self.pending_draft.take();
        let retarget = moving.is_some();
        let draft_id = format!("local-draft-{id}");
        let workspace = self.workspace();
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let host = SessionHost {
            provider_id: self.args.provider.clone(),
            workspace,
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
        };
        let view = cx.new(|cx| SessionView::new(draft_id.clone(), None, host, window, cx));
        view.update(cx, |view, cx| view.load_history(cx));
        self.activate(view, false, window, cx);
        // No row exists for a draft, so the empty state and the later
        // `turn/started` row read the project straight from the store.
        self.set_override(&draft_id, |meta| meta.project = Some(id.clone()), cx);
        let name = self.current_project().map(|p| p.name.clone());
        let effort = self
            .current_project()
            .and_then(|p| p.defaults.effort.as_deref())
            .and_then(projects::parse_effort);
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| {
                view.set_project_name(name);
                // The cached centre reuses a clean view: push the repaint
                // with the name (owner round 6, part 4).
                cx.notify();
            });
            if let Some(effort) = effort {
                view.update(cx, |view, vc| view.set_initial_effort(Some(effort), vc));
            }
            Self::land_moving_draft(&view, moving, window, cx);
        } else if moving.is_some() {
            self.pending_draft = moving;
        }
        self.drafts.insert(id.clone(), draft_id);
        crate::harness_log!(
            "session/start project={} reason={}",
            id,
            if retarget { "retarget" } else { "no-draft" }
        );
        cx.notify();
    }

    /// `session/resume`, then stream the transcript in.
    ///
    /// The target is recorded and shown at once: a cached view reopens
    /// instantly and tops up from its last cursor, otherwise a fresh view
    /// opens on its loading row and pages stream in behind it. Either way a
    /// failed `session/resume` keeps the new view and reports.
    ///
    /// This is the outside-the-sidebar path (palette, `open:` step, boot
    /// `--session`): it arms the one-shot reveal. A sidebar or rail click
    /// reaches [`Self::resume_quiet`] instead and never moves the list.
    pub(crate) fn resume(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.resume_inner(session_id, false, window, cx);
    }

    /// A sidebar or rail click on a session: [`Self::resume`] without the
    /// one-shot reveal. The clicked row is under the cursor, hence painted
    /// inside the viewport by definition, so there is nothing to ensure —
    /// and any stale arm from an earlier outside activation is dropped
    /// rather than served (owner round 6).
    pub(crate) fn resume_quiet(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.resume_inner(session_id, true, window, cx);
    }

    fn resume_inner(&mut self, session_id: String, quiet: bool, window: &mut Window, cx: &mut Context<Self>) {
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
        // The click's target first: the row highlights on the click's own
        // frame, even when there is no client to open with (a `--replay`
        // capture, the `open:` step's screenshots). `activate` below sets
        // the highlight again on the paths that reach it. Only an outside
        // activation arms the reveal; a quiet click clears any stale arm
        // instead (owner round 6).
        self.pending_id = Some(session_id.clone());
        if quiet {
            self.reveal = None;
        } else {
            self.reveal = Some(session_id.clone());
            // A fresh arm owns the list again: the next user scroll disarms
            // it (owner round 5 §A1).
            self.sidebar_user_scrolled = false;
        }
        let Some(client) = self.client.clone() else {
            // Scripted chrome (`--no-connect` / `--replay`) has no child to
            // resume from: the row opens as a local view, so a capture can
            // drive drafts and switching for nothing. Live reconnects never
            // land here — they keep their client-shaped early return below
            // because `args.offline` is false for them.
            if self.args.offline {
                if let Some(view) = self.cache_take(&session_id) {
                    self.activate(view, quiet, window, cx);
                } else {
                    self.open(session_id.clone(), false, quiet, window, cx);
                    // An existing session opens loading, never the hero —
                    // even with no client behind it (owner round 6).
                    if let Some(view) = self.active.clone() {
                        view.update(cx, |view, cx| view.mark_history_loading(cx));
                    }
                }
            }
            return;
        };
        crate::log::trace_reset();
        crate::log::trace_mark(&format!("resume {}", session_id.chars().take(8).collect::<String>()));
        if let Some(view) = self.cache_take(&session_id) {
            crate::log::trace_mark("cache-hit");
            self.activate(view, quiet, window, cx);
            crate::log::trace_mark("swap");
            crate::log::trace_arm_first_frame();
            self.top_up(cx);
            return;
        }
        crate::log::trace_mark("cache-miss");
        // No backfill yet: the first `view/page` must wait for the resume
        // round-trip below. The resume is the leased touch that regenerates a
        // stale `.msp-view-v1` sidecar (muse 1.2.1, #29473); a page fired
        // before it reads the stale generation and fails `-32603`.
        self.open(session_id.clone(), false, quiet, window, cx);
        // The fresh view is for an existing session: it renders the quiet
        // loading state on its very first frame — never the new-session
        // hero — until the resume ack below starts its backfill (owner
        // round 6). Same frame as the swap, so no paint lands between.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.mark_history_loading(cx));
        }
        crate::log::trace_mark("swap");
        crate::log::trace_arm_first_frame();
        let resumed = session_id.clone();
        let work = move || {
            client.session_resume(&SessionResumeParams {
                command_id: new_command_id(),
                session_id,
                // History comes through `view/page`, which is the contiguous,
                // ordered, bounded path; resume just attaches.
                exclude_items: Some(true),
                cursor: None,
                history: None,
                config: None,
            })
        };
        self.wire_call(cx, work, move |this, result, cx| {
            match &result {
                Ok(_) => crate::log::trace_mark("resume-ack"),
                // A stale sidecar is not a failure to show: the resume was
                // still the regenerating first touch, so the backfill below
                // pages a fresh sidecar. One log line, never a dialog.
                Err(error) if error.is_stale_sidecar() => {
                    crate::log::trace_mark("resume-ack-stale");
                    crate::harness_log!("resume of {resumed} hit a stale sidecar ({error}); paging anyway");
                }
                // A rejection about this session — another host holding its
                // lease — is not a failure of the wire: the view stays open
                // under the same lease notice the reconnect path raises,
                // read-only, with no dialog, and the sidebar row stays
                // selectable.
                Err(error) if conn::is_session_scoped(error) => {
                    crate::log::trace_mark("resume-ack-held");
                    let banner = conn::lease_banner(error);
                    crate::harness_log!("resume of {resumed} rejected ({error}); banner on the view, wire stays up");
                    let held =
                        this.active.clone().filter(|view| view.read(cx).session_id == resumed);
                    if let Some(view) = held {
                        view.update(cx, |view, cx| view.set_lease_lost(&banner, cx));
                    }
                }
                Err(error) => {
                    crate::log::trace_mark("resume-ack-err");
                    this.report(error, cx);
                }
            }
            // Only while the just-opened view is still the open one: a
            // further switch owns its own backfill, and a parked view must
            // not start one behind the new view's back.
            let still_open = this.active.clone().filter(|view| view.read(cx).session_id == resumed);
            if let Some(view) = still_open {
                view.update(cx, |view, cx| view.backfill(cx));
            }
            cx.notify();
        });
    }

    /// The root a session of this id runs in: its own row's workspace when
    /// the list knows it, else where the next session would go. Anything
    /// about a session reads this; anything about "where the next session
    /// goes" reads the current project directly.
    pub(crate) fn session_workspace(&self, session_id: &str) -> String {
        self.sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.workspace.clone())
            .unwrap_or_else(|| self.workspace())
    }

    /// A session view opened: the current project becomes that session's
    /// project when it has one. Made current and written, so the next boot
    /// and the next ⌘N start where this session is — but never touched:
    /// opening a session must not reorder the groups (owner round 4, O1).
    /// The touch that remains is in [`Self::new_session_in`] (a new session
    /// is itself the freshest thing about its project) and in `adopt_root`,
    /// and both feed only [`Projects::most_recent`](crate::projects::Projects::most_recent)
    /// now.
    fn adopt_session_project(&mut self, session_id: &str) {
        let project = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.clone())
            .filter(|id| self.projects.find(id).is_some());
        let Some(id) = project else { return };
        self.projects.current = Some(id.clone());
        self.current_project = Some(id);
        projects::write(&self.projects);
    }

    /// Put a fresh session view in the centre pane now and subscribe to what
    /// it needs help with. The swap is immediate — the view, or its loading
    /// row while the first page is still on the wire — never the old view
    /// held past its switch. `quiet` is the sidebar click (see
    /// [`Self::resume_quiet`): the swap happens, the reveal does not arm.
    pub(super) fn open(
        &mut self,
        session_id: String,
        backfill: bool,
        quiet: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // The client rides along when there is one; scripted chrome
        // (`--no-connect` / `--replay`) opens the row as a local view with
        // none, which is what lets a capture drive drafts and switching.
        let client = self.client.clone();
        let (provider, workspace) = (self.args.provider.clone(), self.session_workspace(&session_id));
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let overlays = self.overlays.clone();
        let host = SessionHost { provider_id: provider, workspace, overlays, capture: self.capture.clone() };
        let view = cx.new(|cx| SessionView::new(session_id.clone(), client.clone(), host, window, cx));
        view.update(cx, |view, cx| view.load_history(cx));
        self.activate(view, quiet, window, cx);
        self.adopt_session_project(&session_id);
        if backfill && client.is_some() {
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
    ///
    /// Every outside activation path (⌘N, a group `+`, the palette, the
    /// `open:` step, fork, boot `--session`) comes through here with
    /// `quiet == false`, so this is where the sidebar's one-shot reveal is
    /// armed: the next prepaint scrolls the least distance that brings the
    /// row (or, when its group is closed or folded past the cut, the group)
    /// into view, then consumes the flag (owner round 4, O6; owner round 6:
    /// `scrollIntoView({ block: "nearest" })`). A sidebar or rail click
    /// comes through here with `quiet == true` and never arms: the clicked
    /// row is under the cursor, hence visible, and any stale arm is dropped.
    fn activate(&mut self, view: Entity<SessionView>, quiet: bool, window: &mut Window, cx: &mut Context<Self>) {
        // The UI points at what is open: the row highlight and the header
        // label read this, never the view, so every swap refreshes it here
        // rather than at each call site.
        let session_id = view.read(cx).session_id.clone();
        self.pending_id = Some(session_id.clone());
        if quiet {
            self.reveal = None;
        } else {
            // The sidebar's one-shot reveal arms on the same swap — and a
            // fresh arm owns the list again: the next user scroll disarms it
            // (owner round 5 §A1).
            self.reveal = Some(session_id.clone());
            self.sidebar_user_scrolled = false;
        }
        let project_name = self.project_name_for(&session_id);
        view.update(cx, |view, _| view.set_project_name(project_name));
        self.park_active(cx);
        // A parked view's client predates a reconnect; the current child is
        // the one that can page.
        if let Some(client) = self.client.clone() {
            view.update(cx, |view, cx| view.reconnected(client, cx));
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
        // The swap must repaint the centre even when every push above was
        // `set` without `notify`: a parked view reuses its retained subtree
        // unless it is dirty (owner round 6, part 4).
        view.update(cx, |_, cx| cx.notify());
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
            // Views named in `drafts` are never evicted from the MRU: the
            // draft outlives whatever else was parked since.
            let draft_ids: std::collections::HashSet<String> =
                self.drafts.values().cloned().collect();
            let ids: Vec<String> =
                self.session_cache.iter().map(|(id, _)| id.clone()).collect();
            let surviving = evict_parked(&ids, &draft_ids, SESSION_CACHE_LIMIT);
            self.session_cache.retain(|(id, _)| surviving.contains(id));
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
                config: None,
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
                            // A later resume succeeding is what stands the
                            // lease notice down.
                            view.note_resumed(cx);
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
                        // Through `resume`, not `open`: the fresh view pages
                        // only after its own resume settles, so a stale
                        // sidecar regenerates before the first page reads it.
                        this.resume(topped.clone(), window, cx);
                        // `resume` parked the stale view; it must not come back.
                        this.session_cache.retain(|(id, _)| *id != topped);
                    }
                }
                // Any other rejection about this session banners the still-open
                // topped-up view like the reconnect path, never a dialog.
                Err(error) if conn::is_session_scoped(&error) => {
                    crate::log::trace_mark("resume-ack-held");
                    let banner = conn::lease_banner(&error);
                    crate::harness_log!("cached resume of {topped} rejected ({error}); banner on the view, wire stays up");
                    let still_open = this
                        .active
                        .clone()
                        .filter(|view| view.read(cx).session_id == topped);
                    if let Some(view) = still_open {
                        view.update(cx, |view, cx| view.set_lease_lost(&banner, cx));
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
            // `/clear` is emitted from the session view with no window of its
            // own: reopen through the window the update provides, like the
            // fork path below does.
            SessionEvent::NewSession => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.new_session(window, cx));
                }));
            }
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
                        this.open(session_id, true, false, window, cx);
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

    fn draft_map(names: &[(&str, &str)]) -> HashMap<String, String> {
        names.iter().map(|(p, s)| ((*p).to_owned(), (*s).to_owned())).collect()
    }

    #[test]
    fn a_project_without_a_draft_starts() {
        let drafts = draft_map(&[]);
        assert_eq!(draft_decision(&drafts, "p", |_| panic!("no name to check")), DraftDecision::Start {
            stale: None
        });
    }

    #[test]
    fn a_live_draft_reopens_and_a_dead_name_is_pruned() {
        let drafts = draft_map(&[("p", "s-draft")]);
        assert_eq!(draft_decision(&drafts, "p", |id| id == "s-draft"), DraftDecision::Reuse("s-draft".into()));
        assert_eq!(
            draft_decision(&drafts, "p", |_| false),
            DraftDecision::Start { stale: Some("s-draft".into()) }
        );
    }

    #[test]
    fn drafts_are_per_project() {
        // Two projects' drafts never answer for each other, and a first
        // turn clears only its own session's name.
        let mut drafts = draft_map(&[("a", "s-a"), ("b", "s-b")]);
        assert_eq!(draft_decision(&drafts, "a", |_| true), DraftDecision::Reuse("s-a".into()));
        assert_eq!(draft_decision(&drafts, "b", |_| true), DraftDecision::Reuse("s-b".into()));
        drafts.retain(|_, named| named != "s-a");
        assert_eq!(drafts.get("a"), None);
        assert_eq!(drafts.get("b").map(String::as_str), Some("s-b"));
    }

    #[test]
    fn parked_drafts_survive_the_mru_cap() {
        use std::collections::HashSet;
        let ids: Vec<String> = (0..10).map(|n| format!("s-{n}")).collect();
        let drafts: HashSet<String> = ["s-9"].iter().map(|s| (*s).to_owned()).collect();
        // The oldest non-draft past the cap of eight goes; the draft at the
        // tail stays, wherever it sits.
        assert_eq!(
            evict_parked(&ids, &drafts, 8),
            ["s-0", "s-1", "s-2", "s-3", "s-4", "s-5", "s-6", "s-7", "s-9"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>()
        );
        // With no drafts the cap is a plain truncate.
        assert_eq!(evict_parked(&ids, &HashSet::new(), 8), ids[..8].to_owned());
    }
}
