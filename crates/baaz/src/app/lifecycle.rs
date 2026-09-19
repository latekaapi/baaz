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

/// What [`Harness::ensure_boot_session`] should do about the boot session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BootDecision {
    /// Open the boot session now: `--session <id>` resumes and anything
    /// else starts, all without waiting for `session/list`.
    Open,
    /// `--session latest` cannot resolve until the list lands: wait for it.
    WaitForList,
    /// Nothing to do: a session is open or on its way, the boot was already
    /// attempted, the wire is down, or nothing scripted wants a session.
    Idle,
}

/// The inputs [`boot_decision`] reads, bundled so the decision stays a plain
/// function of named state.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BootState<'a> {
    /// A session view is already open.
    pub active: bool,
    /// A `session/start` round-trip is still in flight.
    pub switch_pending: bool,
    /// The boot session was already attempted once.
    pub attempted: bool,
    /// The wire child exists.
    pub connected: bool,
    /// The `--session` argument, if given.
    pub session_arg: Option<&'a str>,
    /// A `--send` turn is waiting for a session.
    pub send: bool,
    /// A `--steps` list is waiting for a session.
    pub steps_pending: bool,
    /// The session list has landed at least once.
    pub sessions_loaded: bool,
}

/// The boot-session decision, in pure form: at most one attempt, only on a
/// live wire, and `latest` alone waits for the list — everything else the
/// wire and a project can already answer.
pub(crate) fn boot_decision(state: BootState<'_>) -> BootDecision {
    if state.active || state.switch_pending || state.attempted {
        return BootDecision::Idle;
    }
    if !state.connected {
        return BootDecision::Idle;
    }
    if state.session_arg == Some("latest") && !state.sessions_loaded {
        return BootDecision::WaitForList;
    }
    if state.session_arg.is_none() && !state.send && !state.steps_pending {
        return BootDecision::Idle;
    }
    BootDecision::Open
}

/// The `--steps` readiness decision, in pure form: a live run needs the wire
/// and an open session; a `--replay` or `--no-connect` run has no wire, so
/// an open session alone is enough. Never the session list (see
/// [`Harness::steps_ready`]).
pub(crate) fn steps_ready_for(
    steps_pending: bool,
    replay: bool,
    offline: bool,
    connected: bool,
    session_open: bool,
) -> bool {
    if !steps_pending {
        return false;
    }
    if replay || offline {
        return session_open;
    }
    connected && session_open
}

/// Take a scripted list out of its holder, so a later pass finds nothing to
/// do: the drain-once rule behind `--steps` and `--login-steps`. Both
/// runners take through here, so the second caller — a later frame, a later
/// activation — always sees an empty list.
pub(crate) fn drain_steps(steps: &mut Vec<String>) -> Vec<String> {
    std::mem::take(steps)
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
    /// A generated title in flight: the row reads the pending placeholder.
    #[serde(rename = "titlePending", default)]
    title_pending: bool,
    /// The row's preview line, standing in for the last summary a live row
    /// would carry.
    #[serde(default)]
    summary: Option<String>,
    /// The row's ask line, standing in for the user's last request: with
    /// `summary` it draws the two-line byline.
    #[serde(default)]
    ask: Option<String>,
    /// Wire attention flags, spelled as the server sends them
    /// (`approvalPending`, `inputPending`): what `Needs approval` and the
    /// bare `Asked` stand on for rows whose view is not open. Unknown
    /// spellings are ignored, like the client ignores them.
    #[serde(default)]
    attention: Vec<String>,
    /// The pending approval's exact command, standing in for the open
    /// view's fold: the context line's first priority.
    #[serde(default)]
    approval: Option<String>,
    /// The pending question's prompt, standing in for the open view's
    /// fold: the context line and the quoted `Asked` words.
    #[serde(default)]
    question: Option<String>,
    /// The last turn's terminal error message, standing in for the
    /// `turn/completed` record: what `Failed` stands on.
    #[serde(default)]
    error: Option<String>,
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
        attention: None,
        branch: None,
        created_at: row.updated_at.clone(),
        first_user_prompt: None,
        forked_from: None,
        last_activity_at: None,
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
    entry.title_pending = row.title_pending;
    if let Some(summary) = row.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        entry.description = summary.to_owned();
    }
    if let Some(ask) = row.ask.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        entry.last_ask = Some(ask.to_owned());
    }
    for flag in &row.attention {
        match flag.as_str() {
            "approvalPending" => entry.attention.push(muse_client::schema::AttentionFlag::ApprovalPending),
            "inputPending" => entry.attention.push(muse_client::schema::AttentionFlag::InputPending),
            _ => {}
        }
    }
    if let Some(approval) = row.approval.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        entry.approval_command = Some(approval.to_owned());
    }
    if let Some(question) = row.question.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        entry.pending_question = Some(question.to_owned());
    }
    if let Some(error) = row.error.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        entry.last_error = Some(error.to_owned());
    }
    entry
}

impl Harness {
    // -------------------------------------------------------------- sessions

    /// Read the local index once at boot; it is a cache, not a source of truth.
    pub(super) fn load_index(&mut self, cx: &mut Context<Self>) {
        crate::log::boot_mark("index-read-sent");
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                crate::log::boot_mark("index-read-start");
                let map = index::read();
                crate::log::boot_mark(&format!(
                    "index-read-done rows={} in={}ms",
                    map.len(),
                    at.elapsed().as_millis()
                ));
                map
            },
            |this, index, cx| {
                crate::log::boot_mark(&format!("index-reply rows={}", index.len()));
                // Settled, even on an empty read: row visibility comes from the
                // index, and the sidebar waits for it before debuting groups.
                this.index_loaded = true;
                this.index = index;
                let at = std::time::Instant::now();
                if this.sessions_loaded {
                    this.rejoin();
                } else {
                    // The wire has not answered yet: paint the sidebar from
                    // the index now (provisional rows, no meta line) instead
                    // of holding an empty column until `session/list` lands.
                    // The list reply replaces them wholesale.
                    this.install_provisional_rows();
                }
                crate::log::boot_mark(&format!(
                    "rejoin-done rows={} in={}ms",
                    this.sessions.len(),
                    at.elapsed().as_millis()
                ));
                crate::log::boot_mark(&format!(
                    "provisional-rows rows={}",
                    this.sessions.iter().filter(|e| e.provisional).count()
                ));
                this.rebuild_search_index(cx);
                crate::log::boot_mark("index-loaded");
                // Same reason as the list reply below: re-arm the cached
                // pane in this update, not on some later frame's `on_frame`.
                this.sync_sidebar_pane(cx);
                cx.notify();
            },
        );
    }

    /// `session/list`, unfiltered and paged: one window over every
    /// workspace, so the filter that used to hide other workspaces' sessions
    /// is gone and the cursor is followed until the server says `None`.
    /// Everything lands on the one background task behind this
    /// [`crate::wire::WireCall`].
    pub(crate) fn load_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        if self.sessions_list_in_flight {
            // A fetch is already running — at boot the probe answer's fetch
            // is still out when the boot session's `session/start` lands and
            // refreshes the list behind it. Note the refresh and let the
            // landing reply issue the one follow-up, instead of fetching
            // twice and applying the same reply twice (rejoining every row
            // and rebuilding the grouping and the search index for nothing).
            crate::log::boot_mark("session/list-coalesced");
            self.sessions_list_stale = true;
            return;
        }
        self.sessions_list_in_flight = true;
        crate::log::boot_mark("session/list-sent");
        let projects = self.projects.clone();
        let work = move || {
            let at = std::time::Instant::now();
            crate::log::boot_mark("session/list-work-start");
            let mut sessions = Vec::new();
            let mut cursor: Option<String> = None;
            let mut pages = 0usize;
            loop {
                let page_at = std::time::Instant::now();
                let page = client.session_list(&SessionListParams {
                    cursor,
                    limit: Some(200),
                    ..Default::default()
                })?;
                pages += 1;
                crate::log::boot_mark(&format!(
                    "session/list-page pages={} rows={} in={}ms",
                    pages,
                    page.sessions.len(),
                    page_at.elapsed().as_millis()
                ));
                sessions.extend(page.sessions);
                cursor = page.next_cursor;
                if cursor.is_none() {
                    break;
                }
            }
            crate::log::boot_mark(&format!(
                "session/list-work-done rows={} pages={} in={}ms",
                sessions.len(),
                pages,
                at.elapsed().as_millis()
            ));
            // The branch behind every project group row, read while already
            // off the UI thread — and the root-exists answers beside them,
            // so the refresh rechecks every adoption without touching the
            // disk on the UI thread.
            let mut branches = HashMap::new();
            let mut availability = HashMap::new();
            for project in &projects.projects {
                if let Some(branch) = crate::projects::branch_of(&project.root) {
                    branches.insert(project.id.clone(), branch);
                }
                availability.insert(project.id.clone(), project.root.is_dir());
            }
            Ok::<_, MuseError>((sessions, branches, availability))
        };
        self.wire_call_in(cx, work, |this, result, window, cx| {
            let reply_at = std::time::Instant::now();
            let row_count = match &result {
                Ok((sessions, _, _)) => sessions.len(),
                Err(_) => 0,
            };
            crate::log::boot_mark(&format!("session/list-reply rows={row_count}"));
            this.sessions_list_in_flight = false;
            // The first reply replaces the provisional rows wholesale (see
            // `install_provisional_rows`); a later reply whose rows match
            // what is already shown changes nothing and must cost nothing —
            // no rejoin, no regroup, no title reads, no search rebuild.
            let already = this.sessions_loaded;
            this.sessions_loaded = true;
            crate::log::boot_mark("sessions-loaded");
            if let Ok((sessions, branches, availability)) = result {
                this.projects.refresh_availability(&availability);
                let projects = this.projects.clone();
                let join_at = std::time::Instant::now();
                // One cache over the whole loop: each distinct workspace
                // root is canonicalized once, not once per row per project.
                let mut canon = crate::projects::CanonicalCache::default();
                let wire: Vec<SessionEntry> = sessions
                    .iter()
                    .map(|s| {
                        let mut entry = SessionEntry::join_cached(
                            s,
                            this.index.get(&s.session_id),
                            this.overrides.get(&s.session_id),
                            &projects,
                            &mut canon,
                        );
                        // Pending rows read pending; side sessions read
                        // hidden — even before their override writes land.
                        Harness::apply_title_flags(&this.titles_pending, &this.side_sessions, &mut entry);
                        entry
                    })
                    .collect();
                crate::log::boot_mark(&format!(
                    "join-done rows={} distinct_roots={} in={}ms (per-row SessionEntry::join N={})",
                    wire.len(),
                    canon.len(),
                    join_at.elapsed().as_millis(),
                    wire.len()
                ));
                // Provisional rows are never local, so they never survive
                // this comparison: the first reply always applies wholesale.
                // (`SessionEntry` equality includes the provisional flag.)
                let unchanged = already && sidebar::list_apply_unchanged(&this.sessions, &wire);
                if unchanged {
                    // The rows match, but the branch answers are still fresh
                    // data: keep them (assigning invalidates nothing, so the
                    // grouping and the search index stay untouched).
                    this.branches = branches;
                    crate::log::boot_mark(&format!(
                        "list-applied rows={} unchanged in={}ms",
                        this.sessions.len(),
                        reply_at.elapsed().as_millis()
                    ));
                } else {
                    this.invalidate_list();
                    // Local rows whose id the reply does not contain survive;
                    // a local whose id is listed is replaced by its wire row.
                    // Provisional rows are never local: the first reply drops
                    // every one of them here.
                    this.sessions = sidebar::merge_session_list(wire, &this.sessions);
                    this.branches = branches;
                    this.derive_titles(cx);
                    crate::log::boot_mark(&format!(
                        "list-applied rows={} in={}ms",
                        this.sessions.len(),
                        reply_at.elapsed().as_millis()
                    ));
                }
            }
            this.open_boot_session(window, cx);
            // Re-arm the pane in the same update as the data: the column is
            // a cached view, and waiting for the next root render's `on_frame`
            // to notice the new key adds a whole frame hop — one a quiescent
            // window may not take for a long while — before the first rows paint.
            this.sync_sidebar_pane(cx);
            cx.notify();
            if this.sessions_list_stale {
                // A refresh asked while this fetch was running: one
                // follow-up, not a dropped update.
                this.sessions_list_stale = false;
                this.load_sessions(cx);
            }
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
    /// Open the boot session without waiting for `session/list`: a scripted
    /// run's new session needs only the wire and a project, and an explicit
    /// `--session <id>` resumes without the list too. Only `--session
    /// latest` waits for it. Runs at most once per boot: the named session
    /// is consumed, and the unnamed path is guarded by the in-flight switch.
    pub(super) fn ensure_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match boot_decision(BootState {
            active: self.active.is_some(),
            switch_pending: self.session_switch_pending,
            attempted: self.boot_session_attempted,
            connected: self.client.is_some(),
            session_arg: self.args.session.as_deref(),
            send: self.args.send.is_some(),
            steps_pending: !self.args.steps.is_empty(),
            sessions_loaded: self.sessions_loaded,
        }) {
            BootDecision::Idle | BootDecision::WaitForList => return,
            BootDecision::Open => {}
        }
        self.boot_session_attempted = true;
        self.open_boot_session(window, cx);
    }

    /// `--session <id>` (or `latest`): open one session at boot, once the list
    /// has arrived. Consumed, so a later refresh does not re-open it.
    pub(super) fn open_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wanted) = self.args.session.take() else {
            // A scripted turn or a scripted capture with no session named needs
            // somewhere to go — unless one is already on its way: the list
            // reply calls this too, and must not start a second session
            // behind the one [`Self::ensure_boot_session`] issued.
            if (self.args.send.is_some() || !self.args.steps.is_empty())
                && self.active.is_none()
                && !self.session_switch_pending
            {
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

    /// Whether a scripted `--steps` list may run now: there is one left, and
    /// the app can execute it. A live run needs the wire and an open
    /// session; a `--replay` or `--no-connect` run has no wire, so an open
    /// session alone is enough.
    ///
    /// Deliberately not the session list: no verb needs it — the boot
    /// session opens without it ([`Self::ensure_boot_session`]), and only
    /// `--session latest` waits for it, via no open session yet. Gating the
    /// script on the list hung every scripted run behind a `session/list`
    /// that never answered, while the capture mistook the stalled boot for
    /// a finished script.
    pub(crate) fn steps_ready(&self) -> bool {
        steps_ready_for(
            !self.args.steps.is_empty(),
            self.args.replay.is_some(),
            self.args.offline,
            self.client.is_some(),
            self.active.is_some(),
        )
    }

    /// `--steps`: drive the open session from the command line so a screenshot
    /// is reproducible. Runs once, when [`Self::steps_ready`] holds: the
    /// drain inside ([`Harness::take_steps`]) consumes the list, so a second
    /// call — a later frame, a later activation — finds nothing to do.
    /// The verbs, and the loop that runs them, are [`crate::steps`].
    ///
    /// Called every frame ([`Harness::on_frame`]) and after the replay fold
    /// ([`Harness::open_replay`]): those are the two transitions that can
    /// make the predicate true, and the take makes the double call harmless.
    /// Session activations deliberately do not call this: `activate` fires
    /// on every swap, including swaps the script itself causes, so draining
    /// there raced the boot it was meant to follow.
    pub(super) fn maybe_run_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let _ = window;
        if !self.steps_ready() {
            return;
        }
        self.capture.set_steps_ready(true);
        crate::steps::run_steps(self, cx);
    }

    /// `--steps`, taken out of the arguments so a later refresh does not
    /// replay them. Drains through [`drain_steps`]: the first caller gets
    /// the script, every later caller gets nothing.
    pub(crate) fn take_steps(&mut self) -> Vec<String> {
        drain_steps(&mut self.args.steps)
    }

    // One handler per `--steps` verb that belongs to the window rather than to
    // a session and needs more than a single existing call. The verb table
    // that reaches them is [`crate::steps`].

    /// `search:<query>`: open the search palette, optionally on a query.
    pub(crate) fn step_search(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_search(window, cx);
        if !rest.is_empty() {
            self.search_query.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
            // `set_value` does not raise `InputEvent::Change`, which is what
            // typing goes through, so the query has to be run by hand here.
            // Without this the step opened the palette, filled the field and
            // left the empty-query result on screen — a capture of a search
            // that never ran, which is worse than no capture at all.
            self.refresh_search(cx);
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
    /// palette-scroll instrument: over the open palette the
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
        // The handler only accumulates, so without this
        // the read below would always equal `before` and the log could no
        // longer tell a moved transcript from an occluded one. Apply the
        // frame's drain eagerly — with nothing pending it is gpui's own
        // early-return, so over the palette this still logs X->X.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, _| view.drain_pending_wheel());
        }
        let after = self.active.as_ref().map(|view| view.read(cx).bench_list_px()).unwrap_or(0.0);
        crate::baaz_log!("wheel dy={dy} list_px={before}->{after}");
    }

    /// `centre`: the open-flicker instrument. Log
    /// `baaz: centre hero=<hero> loading=<loading>`: the new-session
    /// hero vs loading-row paints since the last call. An existing session
    /// opened through `open:`/`click:` must read `hero=0`; a draft opened
    /// through `new` still reads `hero>0`. Free: no turn, no wire.
    pub(crate) fn step_centre(&mut self) {
        let (hero, loading) = crate::session::take_centre_paints();
        crate::baaz_log!("centre hero={hero} loading={loading}");
    }

    /// `sidebar-wheel:<dy>[,n]`: the sidebar-scroll instrument. Dispatch
    /// n synthetic wheel events (default 1) at a sidebar
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
        let root = crate::sidebar_view::take_baaz_root_renders();
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
        // The flattened row count (heads + folds + sessions), not the
        // session count `visible_sessions(cx).len()` used to log here
        //: `item_ix` walks the
        // flattened model, so a probe comparing it against `rows=` needs
        // the same count or its bound is off by the header/fold rows.
        let rows = self.prev_sidebar_rows.len();
        let centre = crate::session::take_centre_renders();
        // The list walks down as `item_ix` grows; a click that moves
        // nothing reads the same index twice, an outside open the minimum
        // travel to its row — the probe semantics the div offset had.
        crate::baaz_log!(
            "sbwheel dy={dy} n={n} sidebar_ix={bix}+{boff:.1}->{aix}+{aoff:.1} rows={rows} pane={pane} root={root} centre={centre} drains={drains}",
            bix = before.item_ix,
            boff = f32::from(before.offset_in_item),
            aix = after.item_ix,
            aoff = f32::from(after.offset_in_item),
        );
    }

    /// `resize-begin:<x>` / `resize-move:<x>` / `resize-end`: the scripted
    /// resize drag. They drive the same [`Harness`] handlers
    /// the divider strip calls — `begin_resize` / `drag_resize` /
    /// `end_resize` — so an overlap probe (a press with an outside open
    /// still armed, moves with frames interleaved) exercises the real press
    /// disarm and the in-flight install gate. Each logs `baaz: rsdrag`
    /// with the width, the sidebar list index, the reveal arm, the scrolled
    /// flag and the drag: a probe the drag must not move keeps every
    /// line's `sidebar_ix` equal.
    /// `resize-sweep:<to_w,step_px>`: march the divider toward `to_w` one
    /// `step_px` per rendered frame (scripting only) — the
    /// display link's pace on a real display — logging
    /// `baaz: rssweep w=<width> pane=<pane> root=<root> rehint=<0/1>` per
    /// tick. Like a press it disarms the reveal and owns the list while it
    /// runs; it never persists.
    pub(crate) fn step_resize_sweep(&mut self, rest: &str, cx: &mut Context<Self>) {
        let (to, step) = rest.split_once(',').unwrap_or((rest, "4"));
        let to: f32 = to.trim().parse().unwrap_or(aui::shell::SIDEBAR_WIDTH);
        let step: f32 = step.trim().parse().unwrap_or(4.0);
        self.reveal = None;
        self.reveal_unknown = None;
        self.sidebar_user_scrolled = true;
        self.resize.sweep = Some(crate::resize::ResizeSweep::new(to, step));
        self.resize.active = true;
        self.resize.scripted = true;
        cx.notify();
    }

    /// `sidebar-scroll-sweep:<dy,finger_ticks,tail_ticks>`: a frame-paced
    /// sidebar wheel gesture — `dy` held for
    /// `finger_ticks` rendered frames, then decaying to 5% of `dy` over
    /// `tail_ticks` more, one push per tick via
    /// [`Self::push_sidebar_scroll_sweep`]. The
    /// in-process fallback for a real `CGEvent` gesture: where the session
    /// cannot post or confirm a real event (no activatable app, no readable
    /// screencapture), a posted event's landing window cannot be confirmed
    /// (see `docs/02-app.md`). Pair with
    /// `BAAZ_FRAME_TRACE=1` and read `scripts/frame-trace.py --metric
    /// sidebar`; free, no turn, no wire.
    pub(crate) fn step_sidebar_scroll_sweep(&mut self, rest: &str, cx: &mut Context<Self>) {
        let mut parts = rest.split(',');
        let dy: f32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(-20.0);
        let finger: u32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(60);
        let tail: u32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(40);
        self.sidebar_scroll_sweep = Some(crate::resize::ScrollSweep::new(dy, finger, tail));
        cx.notify();
    }

    /// `transcript-scroll-sweep:<dy,finger_ticks,tail_ticks>`: the
    /// transcript's twin of `sidebar-scroll-sweep:`, pushing into the
    /// active session's own wheel accumulator
    /// (`SessionView::push_wheel`) once per rendered frame. Same shape,
    /// same fallback reason, same free/no-turn/no-wire guarantee.
    pub(crate) fn step_transcript_scroll_sweep(&mut self, rest: &str, cx: &mut Context<Self>) {
        let mut parts = rest.split(',');
        let dy: f32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(-20.0);
        let finger: u32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(60);
        let tail: u32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(40);
        self.transcript_scroll_sweep = Some(crate::resize::ScrollSweep::new(dy, finger, tail));
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
        crate::baaz_log!(
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

    /// `row-detail:<session_id>`: capture aid — pin the hover card open for
    /// one row, seated at the selected row's bounds (pair with `click:` on
    /// the same id). Empty clears the pin. Free: no pointer, no turn.
    pub(crate) fn step_row_detail(&mut self, rest: &str, cx: &mut Context<Self>) {
        let id = rest.trim();
        self.forced_detail = if id.is_empty() { None } else { Some(id.to_owned()) };
        cx.notify();
    }

    /// `hover:<session_id>`: capture aid — deliver the selected row's own
    /// hover report, exactly what the row's hover event sends: arms the
    /// card's delay and seats it from the row's bounds, the same side seat
    /// as a real pointer (pair with `click:` on the same id and a `wait:`
    /// past the delay; empty means the selected row). A real mouse-move
    /// event cannot be dispatched from a step — steps run inside a `Harness`
    /// update, and the row's hover handler updates the entity, which panics
    /// re-entrantly (the wheel steps survive it only because their handlers
    /// never touch the entity) — so this makes the report's own call. The
    /// pointer-to-report half lives in the
    /// `hover_report_seats_the_card_without_a_pane_render` test, which
    /// sweeps a real pointer over the real pane. Free: no turn, no wire.
    pub(crate) fn step_hover(&mut self, rest: &str, cx: &mut Context<Self>) {
        let id = rest.trim();
        let Some((known, bounds)) = self.selected_row_bounds.clone() else {
            crate::baaz_log!(
                "hover: no selected row bounds yet (pair with `click:` and a `wait:`)"
            );
            return;
        };
        if !id.is_empty() && known != id {
            crate::baaz_log!(
                "hover: `{id}` is not the selected row (`{known}`); pair with `click:{id}` first"
            );
            return;
        }
        self.note_row_hover(known.clone(), true, Some(bounds), cx);
        match crate::sidebar_view::row_detail_seat(&self.sidebar_bounds, &bounds) {
            Some(seat) => crate::baaz_log!(
                "hover id={known} seat={:.0},{:.0}",
                f32::from(seat.x),
                f32::from(seat.y)
            ),
            None => crate::baaz_log!("hover id={known} no-sidebar-edge"),
        }
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
            None => crate::baaz_log!("unknown project `{name}`"),
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

    /// Paint the sidebar from the local index while `session/list` is still
    /// in flight: one provisional row per labelled index entry, through
    /// [`SessionEntry::provisional`]. Only when the wire has not answered
    /// yet — the list reply replaces these wholesale
    /// ([`sidebar::merge_session_list`] keeps local rows only, and
    /// provisional rows are never local), so the end state is exactly what
    /// the wire joined. Rows already present (a local row placed at
    /// `session/start` before anything landed) keep their seat: an index
    /// entry for an id already in the list builds nothing.
    pub(crate) fn install_provisional_rows(&mut self) {
        self.invalidate_list();
        let mut canon = crate::projects::CanonicalCache::default();
        let projects = self.projects.clone();
        let pending = self.titles_pending.clone();
        let sides = self.side_sessions.clone();
        let mut rows = Vec::new();
        for (session_id, index) in &self.index {
            if self.sessions.iter().any(|e| e.id == *session_id) {
                continue;
            }
            let Some(mut entry) = sidebar::SessionEntry::provisional(
                session_id,
                index,
                self.overrides.get(session_id),
                &projects,
                &mut canon,
            ) else {
                continue;
            };
            // Pending rows read pending; side sessions read hidden — the
            // same flags the joined wire rows get, before their overrides
            // land.
            Self::apply_title_flags(&pending, &sides, &mut entry);
            rows.push(entry);
        }
        // Newest first, like [`Self::visible_sessions`]: the grouping reads
        // this order, and the palette takes its head from it.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        self.sessions.retain(|entry| entry.local);
        self.sessions.extend(rows);
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
            // A missing root resolves nowhere: the row falls back to "Other
            // workspaces" while the adoption stays in the store.
            let resolved = self
                .projects
                .resolve_available(entry.workspace.as_deref(), meta.and_then(|m| m.project.as_deref()));
            entry.project = resolved.as_ref().map(|p| p.id.clone());
            entry.project_name = resolved.as_ref().map(|p| p.name.clone());
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
            let generated =
                meta.and_then(|m| m.generated_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let derived = meta.and_then(|m| m.derived_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let label = name.or(generated).or_else(|| index.and_then(IndexEntry::label)).or(derived);
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
            // A generation in flight still reads pending after the rejoin,
            // and a side session still reads hidden — even before its
            // override write lands (the persisted `hidden` flag carries a
            // restart).
            Self::apply_title_flags(&self.titles_pending.clone(), &self.side_sessions.clone(), entry);
            // A fixture or replayed row names its own preview the way it
            // names its label: no store or index source speaks for a
            // scripted id, so a rejoin must not blank it either.
            if !entry.replayed {
                entry.description = sidebar::describe(meta, index, text, user_named);
                entry.last_ask = meta
                    .and_then(|m| m.last_ask.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                entry.last_error = meta
                    .and_then(|m| m.last_error.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
            }
            entry.named = name.is_some();
        }
    }

    /// `session/start` in the current project, on the configured provider.
    ///
    /// A new session is also the moment to re-walk the workspace: files come
    /// and go while the window is open, and the `@` picker should not offer a
    /// path that was deleted an hour ago. With no adoption there is nowhere
    /// to start, so nothing starts.
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
        if let Some(id) = project.as_deref().filter(|id| self.projects.find_available(id).is_some()) {
            self.projects.touch(id);
            self.projects.current = Some(id.to_owned());
            self.current_project = Some(id.to_owned());
            projects::write(&self.projects);
        }
        // A missing root is nowhere to start: fall back to the effective
        // current (a missing current is never current), or start nothing.
        let current = self.current_project().map(|project| project.id.clone());
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
            // Nowhere to start: no current project, or its adoption is gone.
            // Logged, because a scripted `new` that lands here used to read
            // as a script that ran — exit 0, screenshot written — while
            // having started nothing.
            crate::baaz_log!("new: no current project; starting nothing");
            return;
        };
        let effort = current
            .as_deref()
            .and_then(|id| self.projects.find(id))
            .and_then(|p| p.defaults.effort.as_deref())
            .and_then(projects::parse_effort);
        let started_project = current.clone();
        self.load_menu_sources(std::path::PathBuf::from(self.workspace()), cx);
        // The switch lands on the round-trip below, after the following
        // steps would run: session verbs wait for it (see `run_steps`)
        // instead of acting on the session that is still open.
        self.session_switch_pending = true;
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
                // No row until the first send. Under muse 1.2.1 this held
                // because the wire itself listed a session only after its
                // log flushed on `turn/completed`; muse 1.3.0 lists a
                // zero-turn session like any other, so as of v0.1 prep
                // task 3 `SessionEntry::is_empty` enforces the rule itself
                // instead of assuming the wire will — the `turn/started`
                // handler still inserts the local row that makes the first
                // row appear. The session groups under the project it
                // started in, even when its folder later proves to be a
                // worktree of that root.
                this.set_override(&session_id, |meta| meta.project = current.clone(), cx);
                // The row does not exist when the view activates, so its
                // project name arrives now.
                let name = this.project_name_for(&session_id);
                if let Some(view) = this.active.clone() {
                    view.update(cx, |view, cx| {
                        view.set_project_name(name);
                        // The cached centre reuses a clean view: push the
                        // repaint with the name.
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
                    crate::baaz_log!(
                        "session/start project={} reason={}",
                        id,
                        if retarget { "retarget" } else { "no-draft" }
                    );
                }
                this.load_sessions(cx);
            }
            Err(error) => {
                // No switch is coming: release the session verbs waiting on
                // it rather than holding them for the whole bound. Logged as
                // well as dialogued: a headless run's stderr is the only
                // place this failure would otherwise appear.
                crate::baaz_log!("session/start failed: {error}");
                this.session_switch_pending = false;
                this.report(&error, cx);
            }
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
                crate::baaz_log!(
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
        let Some(id) = self.current_project_id() else {
            crate::baaz_log!("new: no current project; starting nothing");
            return;
        };
        if projects::start_params(
            &self.projects,
            Some(id.as_str()),
            &self.args.provider,
            self.args.approval_mode.clone(),
        )
        .is_none()
        {
            crate::baaz_log!("new: no current project; starting nothing");
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
                // with the name.
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
        crate::baaz_log!(
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
    /// rather than served.
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
        // instead.
        self.pending_id = Some(session_id.clone());
        if quiet {
            self.reveal = None;
            self.reveal_unknown = None;
        } else {
            self.reveal = Some(session_id.clone());
            self.reveal_unknown = None;
            // A fresh arm owns the list again: the next user scroll disarms
            // it.
            self.sidebar_user_scrolled = false;
        }
        let Some(client) = self.client.clone() else {
            // Scripted chrome (`--no-connect` / `--replay`) has no child to
            // resume from: the row opens as a local view, so a capture can
            // drive drafts and switching for nothing. Live reconnects never
            // land here — they keep their client-shaped early return below
            // because `args.offline` is false for them.
            if self.args.offline {
                // The row is already open on this view (a `--replay` window
                // whose capture is folded): keep it, so a click on the open
                // row selects without wiping the folded transcript for a
                // blank local view. The highlight above already moved.
                if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
                    return;
                }
                if let Some(view) = self.cache_take(&session_id) {
                    self.activate(view, quiet, window, cx);
                } else {
                    self.open(session_id.clone(), false, quiet, window, cx);
                    // An existing session opens loading, never the hero —
                    // even with no client behind it.
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
                    crate::baaz_log!("resume of {resumed} hit a stale sidecar ({error}); paging anyway");
                }
                // A rejection about this session — another host holding its
                // lease — is not a failure of the wire: the view stays open
                // under the same lease notice the reconnect path raises,
                // read-only, with no dialog, and the sidebar row stays
                // selectable.
                Err(error) if conn::is_session_scoped(error) => {
                    crate::log::trace_mark("resume-ack-held");
                    let banner = conn::lease_banner(error);
                    crate::baaz_log!("resume of {resumed} rejected ({error}); banner on the view, wire stays up");
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
    /// opening a session must not reorder the groups.
    /// The touch that remains is in [`Self::new_session_in`] (a new session
    /// is itself the freshest thing about its project) and in `adopt_root`,
    /// and both feed only the boot and removal fallbacks
    /// ([`Projects::most_recent_available`](crate::projects::Projects::most_recent_available))
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
    /// into view, then consumes the flag (`scrollIntoView({ block:
    /// "nearest" })` semantics). A sidebar or rail click
    /// comes through here with `quiet == true` and never arms: the clicked
    /// row is under the cursor, hence visible, and any stale arm is dropped.
    fn activate(&mut self, view: Entity<SessionView>, quiet: bool, window: &mut Window, cx: &mut Context<Self>) {
        // Whatever switch the scripts were waiting for has landed: session
        // verbs run against this view from here on.
        self.session_switch_pending = false;
        // The UI points at what is open: the row highlight and the header
        // label read this, never the view, so every swap refreshes it here
        // rather than at each call site.
        let session_id = view.read(cx).session_id.clone();
        self.pending_id = Some(session_id.clone());
        if quiet {
            self.reveal = None;
            self.reveal_unknown = None;
        } else {
            // The sidebar's one-shot reveal arms on the same swap — and a
            // fresh arm owns the list again: the next user scroll disarms it.
            self.reveal = Some(session_id.clone());
            self.reveal_unknown = None;
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
        // unless it is dirty.
        view.update(cx, |_, cx| cx.notify());
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        // No `maybe_run_steps` here: activations fire on every swap,
        // including swaps the script itself causes, and draining the list
        // from the swap raced the boot it was meant to follow. The frame
        // gate ([`Harness::on_frame`]) runs the script once it is ready.
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

    /// Whether any session this window has open — the active view, or one
    /// parked in the MRU cache — has a turn in flight
    /// ([`SessionView::is_sending`]: submitted and no `turn/started` yet, or
    /// the server says it runs).
    ///
    /// What a `--screenshot` run checks, bounded, before it quits
    /// ([`crate::shot::capture_and_quit`]): quitting under a running turn
    /// kills its `muse` child and orphans it. A cached view only reflects
    /// whatever it last knew — live events reach `self.active` alone
    /// ([`Harness::route`]) — so this is best-effort for a session parked
    /// mid-turn, same as every other read of parked state in this module.
    pub(crate) fn any_turn_running(&self, cx: &App) -> bool {
        self.active.as_ref().is_some_and(|view| view.read(cx).is_sending())
            || self.session_cache.iter().any(|(_, view)| view.read(cx).is_sending())
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
                    crate::baaz_log!("cached view of {topped} is stale ({error}); reopening");
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
                    crate::baaz_log!("cached resume of {topped} rejected ({error}); banner on the view, wire stays up");
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
        let launch = std::env::temp_dir().join(format!("baaz-fixture-{}", std::process::id()));
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

    /// The whole matrix behind `ensure_boot_session`, from a scripted boot
    /// with no session named and no list yet.
    fn boot_state() -> BootState<'static> {
        BootState {
            active: false,
            switch_pending: false,
            attempted: false,
            connected: true,
            session_arg: None,
            send: false,
            steps_pending: true,
            sessions_loaded: false,
        }
    }

    #[test]
    fn the_boot_session_opens_once_on_a_live_wire() {
        // Steps with no session named: open without the list.
        assert_eq!(boot_decision(boot_state()), BootDecision::Open);
        // A scripted turn wants its session the same way.
        assert_eq!(boot_decision(BootState { send: true, steps_pending: false, ..boot_state() }), BootDecision::Open);
        // An explicit id resumes without the list too.
        assert_eq!(
            boot_decision(BootState { session_arg: Some("s-1"), ..boot_state() }),
            BootDecision::Open
        );
        // The second frame, the second caller, the late list reply: idle.
        assert_eq!(
            boot_decision(BootState { active: true, attempted: true, sessions_loaded: true, ..boot_state() }),
            BootDecision::Idle
        );
        assert_eq!(
            boot_decision(BootState { switch_pending: true, attempted: true, ..boot_state() }),
            BootDecision::Idle
        );
        assert_eq!(
            boot_decision(BootState { attempted: true, sessions_loaded: true, ..boot_state() }),
            BootDecision::Idle
        );
    }

    #[test]
    fn only_latest_waits_for_the_list() {
        assert_eq!(
            boot_decision(BootState { session_arg: Some("latest"), ..boot_state() }),
            BootDecision::WaitForList
        );
        assert_eq!(
            boot_decision(BootState {
                session_arg: Some("latest"),
                sessions_loaded: true,
                ..boot_state()
            }),
            BootDecision::Open
        );
    }

    #[test]
    fn an_unscripted_boot_opens_nothing() {
        assert_eq!(
            boot_decision(BootState { steps_pending: false, sessions_loaded: true, ..boot_state() }),
            BootDecision::Idle
        );
        // And nothing opens while the wire is down, however scripted.
        assert_eq!(
            boot_decision(BootState { connected: false, ..boot_state() }),
            BootDecision::Idle
        );
        assert_eq!(
            boot_decision(BootState {
                connected: false,
                session_arg: Some("s-1"),
                sessions_loaded: true,
                ..boot_state()
            }),
            BootDecision::Idle
        );
    }

    #[test]
    fn steps_run_live_only_with_a_wire_and_a_session() {
        // Live: both.
        assert!(steps_ready_for(true, false, false, true, true));
        assert!(!steps_ready_for(true, false, false, true, false));
        assert!(!steps_ready_for(true, false, false, false, true));
        assert!(!steps_ready_for(true, false, false, false, false));
        // Replay and offline have no wire: the open session alone decides,
        // connected or not.
        assert!(steps_ready_for(true, true, false, false, true));
        assert!(steps_ready_for(true, false, true, false, true));
        assert!(!steps_ready_for(true, true, false, false, false));
        assert!(!steps_ready_for(true, false, true, false, false));
        // No script left, nowhere: never ready, in every mode.
        assert!(!steps_ready_for(false, false, false, true, true));
        assert!(!steps_ready_for(false, true, false, false, true));
        assert!(!steps_ready_for(false, false, true, false, true));
    }

    #[test]
    fn the_second_drain_finds_nothing_to_do() {
        let mut holder = vec!["new".to_owned(), "wait:3000".to_owned()];
        assert_eq!(drain_steps(&mut holder), vec!["new".to_owned(), "wait:3000".to_owned()]);
        assert!(holder.is_empty());
        assert!(drain_steps(&mut holder).is_empty());
    }
}
