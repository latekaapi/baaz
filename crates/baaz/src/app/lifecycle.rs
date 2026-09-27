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

/// Everything one provider `OpenSession` admitted, carried from the
/// background turn that connected it to the UI thread that opens its view.
struct ProviderOpen {
    provider_id: ProviderId,
    provider: provider::Provider,
    events: futures::channel::mpsc::UnboundedReceiver<provider::ProviderEvent>,
    session_id: String,
    project: Option<String>,
    workspace: String,
    /// The provider's own display title from the ack, when it supplied
    /// one: the label ladder's fallback under a generated title.
    title: Option<String>,
    /// Which open this is ([`Harness::provider_open_epoch`]): `finish`
    /// lands only the newest ask, so a late-finishing earlier open never
    /// steals focus or the send.
    epoch: u64,
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
    /// That list starts its own session (`new` up front): booting one
    /// eagerly ahead of it opened two sessions for one action.
    pub steps_begin_with_new: bool,
    /// The session list has landed at least once.
    pub sessions_loaded: bool,
}

/// The boot-session decision, in pure form: at most one attempt, only on a
/// live wire, and `latest` alone waits for the list — everything else the
/// wire and a project can already answer. A `--steps` list headed by `new`
/// starts its own session, so booting one ahead of it is never wanted:
/// boot plus the head's `new` opened two sessions (two `provider lane open`
/// lines, two sidebar rows) for one action.
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
    if state.session_arg.is_none() && state.steps_begin_with_new {
        return BootDecision::Idle;
    }
    BootDecision::Open
}

/// The `--steps` readiness decision, in pure form: a live run needs the wire
/// and an open session; a `--replay` or `--no-connect` run has no wire, so
/// an open session alone is enough. Never the session list (see
/// [`Harness::steps_ready`]).
///
/// `window_only` covers two cases: a script that only touches the window,
/// and a mixed script whose head runs without a session (see
/// [`crate::steps::head_runs_without_session`]). Offline with `--login
/// signed-in` never opens a boot session, so requiring a session before the
/// head runs meant `new;effort` could never start: the `new` that would have
/// created the session never ran, and the capture drained an unrun list as
/// `ran=2 failed=0` over the empty state.
pub(crate) fn steps_ready_for(
    steps_pending: bool,
    replay: bool,
    offline: bool,
    connected: bool,
    session_open: bool,
    window_only: bool,
    switch_pending: bool,
) -> bool {
    if !steps_pending {
        return false;
    }
    // A script that only touches the window needs no session. Offline with
    // `--login signed-in` never opens one, so requiring a session there meant
    // every window verb — the right pane, the menus, the sidebar flags —
    // silently never ran: the capture came out clean and the run still exited
    // 0. That is how five right-pane entries were written, gated green, and
    // produced five byte-identical screenshots of a shell with no pane in it.
    // The mixed-script head folds in here through the caller's `window_only`
    // for the same reason: holding `new;effort` for a session its own head
    // would create deadlocks the script before it starts.
    if window_only {
        return true;
    }
    // A provider lane opens on a background round-trip exactly like
    // `session/start` does, and session verbs must wait for it the same
    // way: `new;setprovider:codex;send:` failed with `no open session`
    // because `send` ran before the lane's open finished. Window verbs
    // never touch the session, so they still run — the capture's bound,
    // not this gate, is what releases a switch that never lands.
    if switch_pending {
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
                    // every one of them here. Provider-lane rows survive in
                    // the merge, and the record rebuilds any the reply's
                    // wholesale replace dropped.
                    this.sessions = sidebar::merge_session_list(wire, &this.sessions);
                    this.merge_provider_rows();
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
            steps_begin_with_new: crate::steps::steps_begin_with_new(&self.args.steps),
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
            // A mixed script headed by a window verb can start: `new`
            // opens the session the later verbs need, so holding the whole
            // list for a session the head would create deadlocks
            // `new;effort` offline into "never became ready" (see
            // [`crate::steps::head_runs_without_session`]).
            crate::steps::all_window_steps(&self.args.steps)
                || crate::steps::head_runs_without_session(&self.args.steps),
            // An in-flight open — `session/start` or a provider lane —
            // holds session verbs until it lands; window verbs run on.
            self.session_switch_pending,
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
        // A restart lands here before `session/list`: the provider rows
        // install from the local record now, so the sidebar shows them
        // with no wire round-trip.
        self.merge_provider_rows();
    }

    /// Refresh one provider row from its record and the overrides,
    /// keeping its live facts. [`Self::rejoin`] refreshes every such row
    /// through here; the turn handlers use the one-row version directly.
    pub(crate) fn rejoin_provider_row(&mut self, session_id: &str) {
        let Some(record) = self.provider_sessions.get(session_id).cloned() else { return };
        let Some(ix) = self.sessions.iter().position(|entry| entry.id == session_id) else { return };
        let mut row = sidebar::SessionEntry::provider_row(
            &record,
            self.overrides.get(session_id),
            &self.projects,
        );
        // The row's live facts survive the rejoin, like in
        // [`Self::merge_provider_rows`].
        row.running = self.sessions[ix].running;
        row.turn_started = self.sessions[ix].turn_started;
        row.approval_command = self.sessions[ix].approval_command.clone();
        row.pending_question = self.sessions[ix].pending_question.clone();
        row.attention = self.sessions[ix].attention.clone();
        Harness::apply_title_flags(&self.titles_pending, &self.side_sessions, &mut row);
        if self.sessions[ix] != row {
            self.sessions[ix] = row;
            self.invalidate_list();
        }
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed) or after an override changed.
    ///
    /// The same precedence [`SessionEntry::join`] documents, in one place.
    pub(crate) fn rejoin(&mut self) {
        self.invalidate_list();
        // Provider-lane rows rejoin from their record, not from the wire
        // or the index: the generic ladder below knows neither.
        let provider_ids: Vec<String> =
            self.sessions.iter().filter(|entry| entry.provider.is_some()).map(|entry| entry.id.clone()).collect();
        for id in provider_ids {
            self.rejoin_provider_row(&id);
        }
        for entry in &mut self.sessions {
            if entry.provider.is_some() {
                continue;
            }
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

    /// `session/start` in the current project, on the switcher's provider.
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
    /// The terminal relay's route for a new muse session (T2 route 1): mint
    /// the session id and register it with the service BEFORE the start
    /// runs. `None` when `initialize` did not grant `sessionMcp` — no
    /// route, and the session opens exactly as it always did (logged once
    /// per process, not once per session). The caller carries the id onto
    /// the start with `provider_muse::terminal::attach_start` (the
    /// schema spelling lives in the provider crate; the relay only names
    /// the neutral bridge spec).
    fn muse_terminal_session(&mut self, root: &std::path::Path) -> Option<String> {
        if !self.session_mcp {
            crate::terminal::relay::note_muse_route_ungranted();
            return None;
        }
        let session_id = muse_client::new_command_id();
        self.register_terminal_session(&session_id, root.to_path_buf(), "muse");
        Some(session_id)
    }

    /// The terminal relay's route for resuming a muse session: re-register
    /// the stored id (harmless when already registered; refreshes the root)
    /// and attach the bridge to `params`. Without the `sessionMcp` grant
    /// the resume runs exactly as it always did.
    pub(crate) fn muse_terminal_resume(
        &mut self,
        params: &mut muse_client::schema::SessionResumeParams,
    ) {
        if !self.session_mcp {
            crate::terminal::relay::note_muse_route_ungranted();
            return;
        }
        let session_id = params.session_id.clone();
        let root = std::path::PathBuf::from(self.session_workspace(&session_id));
        self.register_terminal_session(&session_id, root, "muse");
        let socket = self.terminal_service.socket_path().to_path_buf();
        let spec = crate::terminal::relay::bridge_spec(
            &crate::terminal::relay::bridge_path(),
            &socket,
            &session_id,
        );
        provider_muse::terminal::attach_resume(params, &session_id, &spec.command, &spec.args);
    }

    pub(crate) fn new_session_in(
        &mut self,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // One session per `new`: a switch already in flight owns the next
        // session — a second `new` (boot racing the head's `new`, a double
        // ⌘N, a scripted repeat) starts nothing, unless a provider switch
        // claimed it for its replacement (see `switch_claim`).
        if self.session_switch_pending && self.switch_claim.take().is_none() {
            crate::baaz_log!("new: a session is already opening; keeping it");
            return;
        }
        if let Some(id) = project.as_deref().filter(|id| self.projects.find_available(id).is_some()) {
            self.projects.touch(id);
            self.projects.current = Some(id.to_owned());
            self.current_project = Some(id.to_owned());
            projects::write(&self.projects);
            // A new session in another project moves the page's root (D55).
            if self.skills.open {
                self.relist_skills(cx);
            }
        }
        // A missing root is nowhere to start: fall back to the effective
        // current (a missing current is never current), or start nothing.
        let current = self.current_project().map(|project| project.id.clone());
        // A live draft in the target project is the session: reopen its view
        // without touching the wire.
        if let Some(id) = current.clone() {
            match draft_decision(&self.drafts, &id, |candidate| self.is_live_draft(candidate, cx)) {
                DraftDecision::Reuse(draft_id) => {
                    // A draft serves only the lane it was created on: a muse
                    // draft under a provider pick (or the reverse) is stale,
                    // dropped below, and started fresh on the pick.
                    let wanted = ProviderId::parse(&self.new_provider);
                    if self.draft_serves(&draft_id, wanted, cx) && self.open_draft(&draft_id, window, cx)
                    {
                        return;
                    }
                    // Named but viewless, or riding the wrong lane: the MRU
                    // never evicts a draft, so a missing view is unreachable
                    // — drop the stale name and start below rather than
                    // strand a retarget.
                    self.drafts.remove(&id);
                }
                DraftDecision::Start { stale: Some(_) } => {
                    self.drafts.remove(&id);
                }
                DraftDecision::Start { stale: None } => {}
            }
        }
        // The switcher's pick is not muse: the session opens on the provider
        // lane, and `session/start` to muse never fires for it.
        let wanted = ProviderId::parse(&self.new_provider);
        if wanted != ProviderId::Muse {
            let workspace = current
                .as_deref()
                .and_then(|id| self.projects.find_available(id))
                .map(|project| project.root.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.workspace());
            self.open_on_provider(wanted, current.clone(), workspace, window, cx);
            return;
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
        let Some(mut params) =
            projects::start_params(&self.projects, current.as_deref(), &self.new_provider, self.args.approval_mode.clone())
        else {
            if current.is_none() {
                // No project to start in, which is an ordinary state and not
                // a failure: the session goes to the default workspace, so a
                // first launch can ask something before adopting anything.
                // That folder is not a project, so the session files itself
                // as unfiled.
                let root = projects::default_workspace();
                crate::baaz_log!("new: no project; starting in the default workspace");
                self.new_session_in_root(&root, window, cx);
                return;
            }
            // `current.is_some()` here would mean the id `current_project()`
            // just resolved through `find_available` no longer names a
            // project at all — a race this single-threaded flow does not
            // have today. Logged, because a scripted `new` that lands here
            // used to read as a script that ran — exit 0, screenshot
            // written — while having started nothing.
            crate::baaz_log!("new: current project is unavailable; starting nothing");
            return;
        };
        let effort = current
            .as_deref()
            .and_then(|id| self.projects.find(id))
            .and_then(|p| p.defaults.effort.as_deref())
            .and_then(projects::parse_effort);
        let started_project = current.clone();
        self.load_menu_sources(std::path::PathBuf::from(self.workspace()), cx);
        // The terminal relay's route (T2 route 1): the minted session id is
        // registered with the service BEFORE the start runs, and the start
        // carries the bridge — but only on the `sessionMcp` grant.
        if let Some(root) = params.workspace_root.clone() {
            if let Some(session_id) = self.muse_terminal_session(std::path::Path::new(&root)) {
                let socket = self.terminal_service.socket_path().to_path_buf();
                let spec = crate::terminal::relay::bridge_spec(
                    &crate::terminal::relay::bridge_path(),
                    &socket,
                    &session_id,
                );
                provider_muse::terminal::attach_start(
                    &mut params,
                    &session_id,
                    &spec.command,
                    &spec.args,
                );
            }
        }
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
                // A handoff to muse lands here: the pack submits onto the
                // fresh view, which the lane check above just built.
                this.land_handoff_destination(session_id.clone(), cx);
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
                this.fail_pending_handoff(error.to_string(), cx);
                this.report(&error, cx);
            }
        });
    }

    /// `session/start` in `root`, which is never adopted: no project
    /// defaults, no `projects.current` pointer, no `drafts` entry. The
    /// session groups nowhere (`Projects::resolve` finds no adoption for
    /// this root) and shows under Unfiled, exactly like any
    /// session whose folder was never adopted.
    ///
    /// This is [`Self::new_session_in`]'s sibling for a folder that is not,
    /// and is not becoming, a project — the folder panel's target when
    /// "New session" has nowhere current to start (see
    /// [`Self::new_session_in`]'s `None` arm) and [`Self::step_new_in`]'s
    /// headless twin. There is no live-draft reuse here: an unadopted root
    /// has no project id to key a draft on, so every call starts a fresh
    /// session.
    pub(crate) fn new_session_in_root(&mut self, root: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        // Same one-session rule as [`Self::new_session_in`]: a switch in
        // flight owns the next session unless a provider switch claimed it.
        if self.session_switch_pending && self.switch_claim.take().is_none() {
            crate::baaz_log!("new: a session is already opening; keeping it");
            return;
        }
        let root = root.to_path_buf();
        // Same lane branch as `new_session_in`: an unadopted root on a
        // provider pick opens on the provider lane, never as `session/start`.
        let wanted = ProviderId::parse(&self.new_provider);
        if wanted != ProviderId::Muse {
            self.open_on_provider(wanted, None, root.to_string_lossy().into_owned(), window, cx);
            return;
        }
        let Some(client) = self.client.clone() else {
            // Scripted chrome (`--no-connect` / `--replay`): no child to
            // start a session on, so the draft opens as a local view.
            self.open_local_draft(window, cx);
            return;
        };
        let mut params = projects::start_params_for_root(&root, &self.new_provider, self.args.approval_mode.clone());
        // Walked eagerly, like `new_session_in` does for a project's root:
        // the `@` picker for the session about to open should not wait on
        // the sessions list to learn where it lives.
        self.load_menu_sources(root.clone(), cx);
        // The terminal relay's route, as in `new_session_in`: registered
        // before the start, carried on it, grant-gated.
        if let Some(session_id) = self.muse_terminal_session(&root) {
            let socket = self.terminal_service.socket_path().to_path_buf();
            let spec = crate::terminal::relay::bridge_spec(
                &crate::terminal::relay::bridge_path(),
                &socket,
                &session_id,
            );
            provider_muse::terminal::attach_start(
                &mut params,
                &session_id,
                &spec.command,
                &spec.args,
            );
        }
        self.session_switch_pending = true;
        let work = move || client.session_start(&params);
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(started) => {
                let session_id = started.session.session_id.clone();
                // Before `open`, which asks `session_workspace` where this
                // session lives and has nothing else to go on.
                this.starting_root =
                    Some((session_id.clone(), projects::canonical_str(&root.to_string_lossy())));
                this.open(session_id.clone(), false, false, window, cx);
                if let Some(view) = this.active.clone() {
                    if let Ok(envelope) = serde_json::to_value(&started.session) {
                        view.update(cx, |view, cx| view.seed_session(envelope, cx));
                    }
                }
                // No project override: a `meta.project` of `None` is exactly
                // what files this session under Unfiled.
                let moving = this.pending_draft.take();
                if let Some(view) =
                    this.active.clone().filter(|view| view.read(cx).session_id == session_id)
                {
                    Self::land_moving_draft(&view, moving, window, cx);
                } else if moving.is_some() {
                    this.pending_draft = moving;
                }
                crate::baaz_log!("session/start root (no project)");
                this.load_sessions(cx);
            }
            Err(error) => {
                crate::baaz_log!("new-in: session/start failed: {error}");
                this.session_switch_pending = false;
                this.report(&error, cx);
            }
        });
    }

    /// `new-in:<path>`: [`Self::new_session_in_root`] against a named root,
    /// so a capture can put a session in a folder of its own rather than in
    /// the default workspace. Relative paths resolve against the process's
    /// own directory, like `project:<path>` already does for adoption.
    pub(crate) fn step_new_in(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let rest = rest.trim();
        if rest.is_empty() {
            return;
        }
        let path = std::path::PathBuf::from(rest);
        let path =
            if path.is_absolute() { path } else { std::env::current_dir().unwrap_or_default().join(path) };
        self.new_session_in_root(&path, window, cx);
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

    /// Whether the draft's view already rides `wanted`'s lane: a muse draft
    /// serves a muse pick, and a provider draft serves its own provider's
    /// pick. Anything else (or a view that is gone) is stale — the start
    /// below drops the name and opens fresh on the pick rather than
    /// reopening the wrong lane.
    fn draft_serves(&self, draft_id: &str, wanted: ProviderId, cx: &gpui::App) -> bool {
        let in_centre = self.active.clone().filter(|view| view.read(cx).session_id == draft_id);
        let parked =
            self.session_cache.iter().find(|(id, _)| id == draft_id).map(|(_, view)| view.clone());
        let Some(view) = in_centre.or(parked) else { return false };
        view.read(cx).provider_kind() == wanted
            && view.read(cx).is_provider_lane() == (wanted != ProviderId::Muse)
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
            &self.new_provider,
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
            provider_id: self.new_provider.clone(),
            workspace,
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host.clone()),
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
        self.adopt_session_project(&session_id, cx);
        // Selecting a session returns from the Skills page (D54).
        self.skills.open = false;
        self.skills.detail_focused = false;
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
        // A provider-lane session: a parked view just activates — the
        // click switches to the live session — and otherwise the stored
        // record reopens over `ResumeSession`, with its history replayed.
        // muse sessions continue below.
        if let Some(record) = self.provider_sessions.get(&session_id).cloned() {
            if let Some(view) = self.cache_take(&session_id) {
                self.activate(view, quiet, window, cx);
                return;
            }
            self.reopen_provider(record, window, cx);
            return;
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
        let mut resume_params = SessionResumeParams {
            command_id: new_command_id(),
            session_id,
            // History comes through `view/page`, which is the contiguous,
            // ordered, bounded path; resume just attaches.
            exclude_items: Some(true),
            cursor: None,
            history: None,
            config: None,
        };
        // The terminal relay's route: re-register the stored id and carry
        // the bridge — grant-gated, like the start.
        self.muse_terminal_resume(&mut resume_params);
        let work = move || client.session_resume(&resume_params);
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
            // A session started outside a project is not in the list yet and
            // has no project to speak for it; its root came back with
            // `session/start` and is held until the list catches up.
            .or_else(|| {
                self.starting_root
                    .as_ref()
                    .filter(|(id, _)| id == session_id)
                    .map(|(_, root)| root.clone())
            })
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
    fn adopt_session_project(&mut self, session_id: &str, cx: &mut Context<Self>) {
        let project = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.clone())
            .filter(|id| self.projects.find(id).is_some());
        let Some(id) = project else { return };
        let moved = self.current_project.as_deref() != Some(id.as_str());
        self.projects.current = Some(id.clone());
        self.current_project = Some(id);
        projects::write(&self.projects);
        // The project moved under the open Skills page: re-list (D55).
        if moved && self.skills.open {
            self.relist_skills(cx);
        }
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
        let (provider, workspace) = (self.new_provider.clone(), self.session_workspace(&session_id));
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        let overlays = self.overlays.clone();
        let host = SessionHost { provider_id: provider, workspace, overlays, capture: self.capture.clone(), terminal_host: Some(self.terminal_host.clone()) };
        let view = cx.new(|cx| SessionView::new(session_id.clone(), client.clone(), host, window, cx));
        view.update(cx, |view, cx| view.load_history(cx));
        self.activate(view, quiet, window, cx);
        self.adopt_session_project(&session_id, cx);
        if backfill && client.is_some() {
            if let Some(view) = self.active.clone() {
                view.update(cx, |view, cx| view.backfill(cx));
            }
        }
        cx.notify();
    }

    /// Open a new Claude Code / Codex session: connect and `OpenSession` on
    /// the background executor (both block), then construct the lane view and
    /// make it the visible session. What `new_session_in` calls when the
    /// switcher's pick is not muse.
    ///
    /// A failure surfaces the same error dialog a failed `session/start`
    /// gets, titled with the provider's name — and opens nothing, never a
    /// silent muse fallback.
    pub(crate) fn open_on_provider(
        &mut self,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        // The switch lands on the round-trip below, like the muse path: the
        // session verbs wait for it instead of acting on the session that is
        // still open.
        self.session_switch_pending = true;
        // Whichever open the user asked for last wins: this ask's epoch
        // travels into the background work, and `finish_provider_open`
        // drops any finish that is no longer current.
        self.provider_open_epoch = self.provider_open_epoch.wrapping_add(1);
        let epoch = self.provider_open_epoch;
        // Scripted chrome (`--no-connect` / `--replay`): no child to spawn,
        // so the lane opens over a connected scripted provider — the same
        // stand-in `open_local_draft` uses for muse, and what lets
        // `new;setprovider:claude-code` end on a provider lane offline.
        if self.client.is_none() {
            self.open_scripted_lane(provider_id, project, workspace, epoch, window, cx);
            return;
        }
        let factory = self.provider_factory.clone();
        let workspace_bg = workspace.clone();
        // The terminal relay's route: the request id is the session id the
        // bridge will answer for (Claude Code takes it as `--session-id`;
        // Codex's bridge answers for it until the minted thread id lands) —
        // so it is registered with the service BEFORE the open runs.
        let request_id = new_command_id();
        self.register_terminal_session(
            &request_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
        );
        let work = move || -> Result<ProviderOpen, provider::ProviderError> {
            let provider = factory(provider_id)?;
            #[cfg(not(test))]
            let (provider, events) = conn::gate(provider);
            let ack = provider.send(provider::Command::OpenSession {
                request_id,
                workspace: Some(workspace_bg),
                model: None,
                model_provider: None,
            })?;
            // Tests drain after the send, synchronously on the test
            // executor: no forwarding thread ever wakes the lane task.
            #[cfg(test)]
            let (provider, events) = conn::gate_sync(provider);
            match ack {
                provider::Ack::Session { session_id, title, .. } => Ok(ProviderOpen {
                    provider_id,
                    provider,
                    events,
                    session_id,
                    project,
                    workspace,
                    title,
                    epoch,
                }),
                other => Err(provider::ProviderError::Rejected {
                    reason: format!("OpenSession answered {other:?} instead of a session"),
                }),
            }
        };
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(open) => {
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                // No switch is coming: release the session verbs waiting on
                // it, like the failed `session/start` arm does.
                crate::baaz_log!("provider open failed ({}): {error}", provider_id.label());
                this.session_switch_pending = false;
                this.fail_pending_handoff(error.to_string(), cx);
                this.set_dialog(
                    cx,
                    Dialog {
                        title: format!("Couldn't start {}", provider_id.label()),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                );
            }
        });
    }

    /// The `--no-connect` half of [`Self::open_on_provider`]: a connected
    /// scripted provider stands in for the CLI child, synchronously — a
    /// scripted provider never blocks, so no background turn is needed.
    fn open_scripted_lane(
        &mut self,
        provider_id: ProviderId,
        project: Option<String>,
        workspace: String,
        epoch: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use provider::ProviderAdapter as _;
        let mut adapter = provider::scripted::ScriptedProvider::new();
        let bridged = adapter
            .connect(&conn::connect_info())
            .map_err(|error| error.to_string())
            .and_then(|_| {
                let provider = provider::Provider::new(adapter);
                let ack = provider
                    .send(provider::Command::OpenSession {
                        request_id: new_command_id(),
                        workspace: Some(workspace.clone()),
                        model: None,
                        model_provider: None,
                    })
                    .map_err(|error| error.to_string())?;
                match ack {
                    provider::Ack::Session { session_id, title, .. } => Ok((provider, session_id, title)),
                    other => Err(format!("OpenSession answered {other:?} instead of a session")),
                }
            })
            .map(|(provider, session_id, title)| {
                // The send above already ran: drain it synchronously in
                // tests so no forwarding thread wakes the lane task.
                #[cfg(test)]
                let (provider, events) = conn::gate_sync(provider);
                #[cfg(not(test))]
                let (provider, events) = conn::gate(provider);
                (provider, events, session_id, title)
            });
        match bridged {
            Ok((provider, events, session_id, title)) => {
                self.finish_provider_open(
                    ProviderOpen { provider_id, provider, events, session_id, project, workspace, title, epoch },
                    window,
                    cx,
                );
            }
            Err(reason) => {
                crate::baaz_log!("scripted provider open failed ({}): {reason}", provider_id.label());
                self.session_switch_pending = false;
                self.fail_pending_handoff(reason.clone(), cx);
                self.set_dialog(
                    cx,
                    Dialog {
                        title: format!("Couldn't start {}", provider_id.label()),
                        detail: reason,
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                );
            }
        }
    }

    /// Land a connected provider session: construct its lane view and
    /// register it everywhere [`Self::open`] registers muse views — the
    /// drafts map, the project override, the local record with its sidebar
    /// row, and the centre pane with the composer focused — so it becomes
    /// the visible session.
    fn finish_provider_open(&mut self, open: ProviderOpen, window: &mut Window, cx: &mut Context<Self>) {
        let ProviderOpen { provider_id, mut provider, events, session_id, project, workspace, title, epoch } =
            open;
        // Last ask wins: an earlier open finishing after a newer one was
        // asked for never steals focus or the send — its child is shut
        // down and nothing else is touched. The pending switch belongs to
        // the current open, so it stays set; no record, no row, no view.
        if epoch != self.provider_open_epoch {
            crate::baaz_log!(
                "provider lane superseded provider={} session={session_id}",
                provider_id.as_str()
            );
            provider.shutdown();
            return;
        }
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        // The terminal relay's route lands here too: the ack's session id
        // is what the lane serves from now on — for Codex that is the
        // server-minted thread id, which no pre-registration could name.
        self.register_terminal_session(
            &session_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
        );
        let overlays = self.overlays.clone();
        let host = SessionHost {
            provider_id: provider_id.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays,
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host.clone()),
        };
        let view = cx.new(|cx| {
            SessionView::new_on_provider(session_id.clone(), provider, events, host, window, cx)
        });
        // Like the muse path: the session groups under the project it
        // started in, and the drafts map names it while it is unsent so a
        // repeated ⌘N reopens it instead of spawning another child.
        if let Some(id) = project.clone() {
            self.set_override(&session_id, |meta| meta.project = Some(id.clone()), cx);
            self.drafts.insert(id, session_id.clone());
        }
        // The local record: `session/list` only knows muse sessions, so
        // this row is what brings the lane back after a restart — the
        // sidebar reads it, and the click reopens it. Written before the
        // row below is built, so the two never disagree.
        crate::provider_sessions::upsert_open(
            &mut self.provider_sessions,
            provider_id.as_str(),
            &session_id,
            Some(workspace.clone()),
            project.clone(),
            title,
        );
        crate::provider_sessions::write(&self.provider_sessions);
        // No row with content yet: like a muse draft, the session earns
        // its row on the first send (`ProviderTurnAccepted`), titled from
        // the prompt — but the record above already exists, so a restart
        // before that still reopens it.
        self.merge_provider_rows();
        // The swap that makes it the visible session: parks the outgoing
        // view, points the event subscription at the new one, focuses the
        // composer — everything `open` does for muse.
        self.activate(view, false, window, cx);
        // A handoff's destination lands here: the pack submits onto the
        // fresh view. Anything else leaves `pending_handoff` alone.
        self.land_handoff_destination(session_id.clone(), cx);
        // The scriptable check: a `--steps` run greps the log for this line
        // to prove the pick ended on a provider lane, not a muse session.
        // (`baaz_log!` already carries the `baaz: ` prefix; spelling it
        // here doubled it to `baaz: baaz:`.)
        crate::baaz_log!(
            "provider lane open provider={} session={session_id}",
            provider_id.as_str()
        );
    }

    /// Rebuild the sidebar rows the provider record owns: one row per
    /// stored session, joined with the overrides, preserving each row's
    /// live facts (a running turn, pending approvals) across the rebuild.
    /// Rows whose record is gone (deleted) leave with it. Returns whether
    /// the list changed.
    pub(crate) fn merge_provider_rows(&mut self) -> bool {
        self.sessions.retain(|entry| entry.provider.is_none() || self.provider_sessions.contains_key(&entry.id));
        if self.provider_sessions.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut records: Vec<crate::provider_sessions::ProviderSessionRecord> =
            self.provider_sessions.values().cloned().collect();
        records.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        for record in &records {
            let mut row = sidebar::SessionEntry::provider_row(
                record,
                self.overrides.get(&record.session_id),
                &self.projects,
            );
            // The row's live facts survive the rebuild: a running turn,
            // pending words and the title-pending flag belong to the view
            // and the titler, not the store.
            if let Some(existing) = self.sessions.iter().find(|entry| entry.id == record.session_id) {
                row.running = existing.running;
                row.turn_started = existing.turn_started;
                row.approval_command = existing.approval_command.clone();
                row.pending_question = existing.pending_question.clone();
                row.attention = existing.attention.clone();
            }
            Harness::apply_title_flags(&self.titles_pending, &self.side_sessions, &mut row);
            match self.sessions.iter().position(|entry| entry.id == record.session_id) {
                Some(ix) if self.sessions[ix] == row => {}
                Some(ix) => {
                    self.sessions[ix] = row;
                    changed = true;
                }
                None => {
                    self.sessions.push(row);
                    changed = true;
                }
            }
        }
        if changed {
            self.invalidate_list();
        }
        changed
    }

    /// Reopen a stored provider session after a restart: connect a fresh
    /// child through the factory and `ResumeSession` it there, then land
    /// it like any lane open. The adapter replays the transcript's deltas
    /// (Claude Code its `~/.claude` jsonl, Codex its thread), which the
    /// lane folds into the view — except a record with no settled turns,
    /// which holds no history anywhere: it opens fresh and the stale
    /// record leaves with it. A refusal dialogs with the reason and
    /// opens nothing — never a silent muse fallback, never a fresh empty
    /// session under the old id.
    pub(crate) fn reopen_provider(
        &mut self,
        record: crate::provider_sessions::ProviderSessionRecord,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session_switch_pending = true;
        // A reopen is an ask like any other: it joins the last-wins epoch
        // so a newer open is never stolen by its late finish.
        self.provider_open_epoch = self.provider_open_epoch.wrapping_add(1);
        let epoch = self.provider_open_epoch;
        let provider_id = ProviderId::parse(&record.provider);
        if provider_id == ProviderId::Muse {
            crate::baaz_log!("provider reopen refused: unknown provider {:?}", record.provider);
            self.session_switch_pending = false;
            self.set_dialog(
                cx,
                Dialog {
                    title: "Couldn't reopen session".into(),
                    detail: format!("Unknown provider {:?}", record.provider),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                    archive_target: None,
                },
            );
            return;
        }
        // An untouched draft — a record with no settled turns and no
        // prompt ever sent — holds no history anywhere: resuming it would
        // replay nothing, so it opens fresh instead, and the stale record
        // leaves with it so the sidebar never accumulates dead rows. (A
        // record whose only turn was cut off carries `firstPrompt` with
        // `turns == 0`: that prompt must resume, not be dropped.)
        if record.turns == 0 && record.first_prompt.is_none() {
            crate::baaz_log!(
                "provider reopen: {} has no turns, opening fresh",
                record.session_id
            );
            crate::provider_sessions::remove(&mut self.provider_sessions, &record.session_id);
            crate::provider_sessions::write(&self.provider_sessions);
            self.merge_provider_rows();
            let workspace = record.workspace.clone().unwrap_or_else(|| self.workspace());
            self.open_on_provider(provider_id, record.project.clone(), workspace, window, cx);
            return;
        }
        // Scripted chrome (`--no-connect` / `--replay`): the stand-in
        // answers `OpenSession` but no resume — the reopen still goes
        // through `ResumeSession` so the failure is the honest one.
        if self.client.is_none() {
            use provider::ProviderAdapter as _;
            let mut resumed = provider::scripted::ScriptedProvider::new();
            let bridged = resumed
                .connect(&conn::connect_info())
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    let provider = provider::Provider::new(resumed);
                    let session_id = crate::provider_sessions::send_resume(&provider, &record, &new_command_id())
                        .map_err(|error| error.to_string())?;
                    #[cfg(test)]
                    let (provider, events) = conn::gate_sync(provider);
                    #[cfg(not(test))]
                    let (provider, events) = conn::gate(provider);
                    Ok((provider, events, session_id))
                });
            match bridged {
                Ok((provider, events, session_id)) => {
                    let workspace =
                        record.workspace.clone().unwrap_or_else(|| self.workspace());
                    self.finish_provider_open(
                        ProviderOpen {
                            provider_id,
                            provider,
                            events,
                            session_id,
                            project: record.project.clone(),
                            workspace,
                            title: record.title.clone(),
                            epoch,
                        },
                        window,
                        cx,
                    );
                }
                Err(reason) => {
                    crate::baaz_log!("scripted provider resume failed ({}): {reason}", provider_id.label());
                    self.session_switch_pending = false;
                    self.set_dialog(
                        cx,
                        Dialog {
                            title: format!("Couldn't start {}", provider_id.label()),
                            detail: reason,
                            kind: DialogKind::Error,
                            primary: "Dismiss",
                            action: DialogAction::Dismiss,
                            archive_target: None,
                        },
                    );
                }
            }
            return;
        }
        let factory = self.provider_factory.clone();
        let workspace = record.workspace.clone().unwrap_or_else(|| self.workspace());
        // The terminal relay's route: the resumed session's bridge answers
        // for the stored id, so it is (re-)registered BEFORE the resume
        // runs — refreshing the root it drives.
        self.register_terminal_session(
            &record.session_id,
            std::path::PathBuf::from(workspace.clone()),
            record.provider.as_str(),
        );
        let work = move || -> Result<ProviderOpen, provider::ProviderError> {
            let provider = factory(provider_id)?;
            #[cfg(not(test))]
            let (provider, events) = conn::gate(provider);
            let session_id = crate::provider_sessions::send_resume(&provider, &record, &new_command_id())?;
            // Tests drain after the resume, synchronously on the test
            // executor: the replayed deltas are buffered before the lane
            // starts, with no forwarding thread.
            #[cfg(test)]
            let (provider, events) = conn::gate_sync(provider);
            Ok(ProviderOpen {
                provider_id,
                provider,
                events,
                session_id,
                project: record.project.clone(),
                workspace: record.workspace.clone().unwrap_or(workspace),
                title: record.title.clone(),
                epoch,
            })
        };
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(open) => {
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                crate::baaz_log!("provider resume failed ({}): {error}", provider_id.label());
                this.session_switch_pending = false;
                this.set_dialog(
                    cx,
                    Dialog {
                        title: format!("Couldn't reopen {}", provider_id.label()),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                );
            }
        });
    }

    /// Forget a provider session for good: the record, its row and its
    /// views leave together. Dropping a view hangs its child up (see
    /// `SessionView::drop`), so deleting the open or parked session also
    /// shuts the child down — no orphaned `claude` / `codex` process.
    /// muse sessions never reach here: their list is the server's, and
    /// Baaz only ever hides or archives them.
    pub(crate) fn delete_provider_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.active.as_ref().is_some_and(|view| view.read(cx).session_id == session_id) {
            self.active = None;
        }
        self.session_cache.retain(|(id, _)| id != session_id);
        self.sessions.retain(|entry| entry.id != session_id);
        self.overrides.remove(session_id);
        if crate::provider_sessions::remove(&mut self.provider_sessions, session_id) {
            crate::provider_sessions::write(&self.provider_sessions);
        }
        crate::sessions::write(&self.overrides);
        self.rejoin();
        self.invalidate_list();
        cx.notify();
    }

    /// Open a fork minted by `ForkSession` as a new provider-lane view on
    /// the same provider: connect a fresh child through the factory and
    /// resume the fork there, then land it like any lane open. A refused
    /// resume dialogs with the reason and opens nothing — never a silent
    /// muse fallback, never the source session.
    pub(crate) fn open_forked_on_provider(
        &mut self,
        provider_id: ProviderId,
        session_id: String,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.load_menu_sources(std::path::PathBuf::from(workspace.clone()), cx);
        self.session_switch_pending = true;
        // A fork is an ask like any other: it joins the last-wins epoch.
        self.provider_open_epoch = self.provider_open_epoch.wrapping_add(1);
        let epoch = self.provider_open_epoch;
        // Scripted chrome (`--no-connect` / `--replay`): the stand-in
        // answers `OpenSession` but no resume — the fork still goes
        // through `ResumeSession` so the failure is the honest one.
        if self.client.is_none() {
            use provider::ProviderAdapter as _;
            let mut resumed = provider::scripted::ScriptedProvider::new();
            let bridged = resumed
                .connect(&conn::connect_info())
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    let provider = provider::Provider::new(resumed);
                    let ack = provider
                        .send(provider::Command::ResumeSession {
                            request_id: new_command_id(),
                            session_id: session_id.clone(),
                            cursor: None,
                            metadata_only: false,
                        })
                        .map_err(|error| error.to_string())?;
                    match ack {
                        provider::Ack::Session { session_id, title, .. } => {
                            #[cfg(test)]
                            let (provider, events) = conn::gate_sync(provider);
                            #[cfg(not(test))]
                            let (provider, events) = conn::gate(provider);
                            Ok((provider, events, session_id, title))
                        }
                        other => Err(format!("ResumeSession answered {other:?} instead of a session")),
                    }
                });
            match bridged {
                Ok((provider, events, session_id, title)) => {
                    self.finish_provider_open(
                        ProviderOpen {
                            provider_id,
                            provider,
                            events,
                            session_id,
                            project: None,
                            workspace,
                            title,
                            epoch,
                        },
                        window,
                        cx,
                    );
                }
                Err(reason) => {
                    crate::baaz_log!("scripted fork resume failed ({}): {reason}", provider_id.label());
                    self.session_switch_pending = false;
                    self.set_dialog(
                        cx,
                        Dialog {
                            title: format!("Couldn't start {}", provider_id.label()),
                            detail: reason,
                            kind: DialogKind::Error,
                            primary: "Dismiss",
                            action: DialogAction::Dismiss,
                            archive_target: None,
                        },
                    );
                }
            }
            return;
        }
        let factory = self.provider_factory.clone();
        let workspace_bg = workspace.clone();
        // The terminal relay's route: the fork's bridge answers for the
        // fork-minted id, registered BEFORE the resume runs.
        self.register_terminal_session(
            &session_id,
            std::path::PathBuf::from(workspace.clone()),
            provider_id.as_str(),
        );
        let work = move || -> Result<ProviderOpen, provider::ProviderError> {
            let provider = factory(provider_id)?;
            #[cfg(not(test))]
            let (provider, events) = conn::gate(provider);
            let ack = provider.send(provider::Command::ResumeSession {
                request_id: new_command_id(),
                session_id: session_id.clone(),
                cursor: None,
                metadata_only: false,
            })?;
            // Tests drain after the resume, synchronously on the test
            // executor: no forwarding thread wakes the lane task.
            #[cfg(test)]
            let (provider, events) = conn::gate_sync(provider);
            match ack {
                provider::Ack::Session { session_id, title, .. } => Ok(ProviderOpen {
                    provider_id,
                    provider,
                    events,
                    session_id,
                    project: None,
                    workspace: workspace_bg,
                    title,
                    epoch,
                    // The fork groups under no project: it is a new session
                    // the sidebar names when its first turn lands.
                }),
                other => Err(provider::ProviderError::Rejected {
                    reason: format!("ResumeSession answered {other:?} instead of a session"),
                }),
            }
        };
        self.wire_call_in(cx, work, move |this, result, window, cx| match result {
            Ok(open) => {
                this.finish_provider_open(open, window, cx);
            }
            Err(error) => {
                crate::baaz_log!("provider fork resume failed ({}): {error}", provider_id.label());
                this.session_switch_pending = false;
                this.set_dialog(
                    cx,
                    Dialog {
                        title: format!("Couldn't start {}", provider_id.label()),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                );
            }
        });
    }

    /// Forget `session_id`'s view wherever it lives — open or parked. A
    /// provider lane's child is hung up first, so a discarded session leaves
    /// no orphaned `claude` / `codex` process; a muse draft has no child to
    /// hang up. What `SwitchProvider` calls for the empty draft it replaces,
    /// so the replacement (not a parking) is what survives.
    pub(crate) fn close_view(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // The next `activate` re-points the event subscription at the new
        // view; the dropped view's subscription fires nothing after this,
        // so this only hangs up children and forgets handles — never while
        // an event for the closed view is still dispatching through them.
        if self.active.clone().is_some_and(|view| view.read(cx).session_id == session_id) {
            if let Some(view) = self.active.take() {
                view.update(cx, |view, _| view.shutdown_lane());
            }
            self.pending_id = None;
        }
        if let Some(view) = self.cache_take(session_id) {
            view.update(cx, |view, _| view.shutdown_lane());
        }
        // A provider draft that never sent leaves no transcript anywhere —
        // no live turns, no replayed ones, not even a first prompt — so its
        // record leaves with its view instead of lingering as a row the
        // empty filter hides forever. Anything with history keeps its
        // record: only the view is forgotten.
        let unsent_draft = self
            .provider_sessions
            .get(session_id)
            .is_some_and(|record| record.turns == 0 && record.first_prompt.is_none());
        if unsent_draft {
            self.delete_provider_session(session_id, cx);
            return;
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
        // the one that can page. Provider-lane views own their own child,
        // so the muse reconnect refuses on them — skip it outright.
        if let Some(client) = self.client.clone() {
            view.update(cx, |view, cx| {
                if !view.is_provider_lane() {
                    view.reconnected(client, cx);
                }
            });
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
        self.active = Some(view.clone());
        self.focus_composer = true;
        // "Ask Muse to write one" (A5): the fresh session opens with the
        // `/create-skill ` draft and the composer focused. Never sent. The
        // arm survives a non-muse activation untouched — the draft is a muse
        // skill prompt, so it waits for a muse session.
        if self.pending_create_skill && view.read(cx).provider_kind() == crate::providers::ProviderId::Muse {
            self.pending_create_skill = false;
            let draft = crate::skills_page::create_skill_draft().to_owned();
            view.update(cx, |view, cx| {
                view.set_draft(draft, window, cx);
                view.focus_composer(window, cx);
            });
        }
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
        let mut resume_params = SessionResumeParams {
            command_id: new_command_id(),
            session_id,
            cursor,
            exclude_items: Some(true),
            history: None,
            config: None,
        };
        // The terminal relay's route, as in `open`: re-registered and
        // carried, grant-gated.
        self.muse_terminal_resume(&mut resume_params);
        let work = move || client.session_resume(&resume_params);
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
                let (session_id, prompt, turn_id) =
                    (session_id.clone(), prompt.clone(), turn_id.clone());
                // A turn that started is a session made real: it is no
                // draft any more, whether it already had a row or not.
                self.drafts.retain(|_, named| named != &session_id);
                crate::provider_sessions::note_first_prompt(
                    &mut self.provider_sessions,
                    &session_id,
                    &prompt,
                );
                if crate::provider_sessions::touch(&mut self.provider_sessions, &session_id) {
                    crate::provider_sessions::write(&self.provider_sessions);
                }
                if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == session_id) {
                    if sidebar::first_send_update(entry, Some(&prompt), crate::clock::now_local()) {
                        self.invalidate_list();
                    }
                } else {
                    // No row yet (a restart between the open and this
                    // send): build it now, titled from the prompt.
                    self.merge_provider_rows();
                    if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == session_id) {
                        sidebar::first_send_update(entry, Some(&prompt), crate::clock::now_local());
                        self.invalidate_list();
                    }
                }
                // A first send may earn a generated title: the same cheap
                // muse side session the muse lane uses — never a second
                // provider child.
                self.maybe_start_title(&session_id, Some(prompt), cx);
                self.title_from_transcript(cx);
                // A late admission — the ack arriving after its turn
                // already finished — is bookkeeping only (row, record,
                // title above): the row declines it as a start and
                // re-reads the settled view instead of manufacturing a
                // fresh `Working · now` (W8c).
                let stale = view.read(cx).provider_turn_finished(&turn_id);
                self.sync_row_live(&session_id, !stale, cx);
                cx.notify();
            }
            // A provider lane landed or settled an approval card: the
            // row re-reads the open view's pending words, so it stands
            // on the needs-you state while the approval waits.
            SessionEvent::ProviderApprovalsChanged { session_id } => {
                self.sync_row_live(session_id, false, cx);
                cx.notify();
            }
            // A provider lane settled a turn: the free byline lands, the
            // ledger gains its tagged row, and the record counts the turn —
            // the lane's `turn/completed`.
            SessionEvent::ProviderTurnFinished { session_id, turn_id, meta } => {
                let (session_id, turn_id, meta) = (session_id.clone(), turn_id.clone(), meta.clone());
                let provider = view.read(cx).provider_kind().as_str().to_owned();
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
                if crate::provider_sessions::note_settled_turn_counted(
                    &mut self.provider_sessions,
                    &session_id,
                    exchanges,
                ) {
                    crate::provider_sessions::write(&self.provider_sessions);
                }
                self.rejoin_provider_row(&session_id);
                self.sync_row_live(&session_id, false, cx);
                self.record_last_summary(cx);
                self.title_from_transcript(cx);
                // The free excerpt just landed; a poor one may earn one
                // debounced rewrite — idle sessions only, never a running
                // turn — through the same side session as a title.
                self.maybe_rewrite_byline(cx);
                // The lane settled: repaint on this path even if the
                // view's own notify raced the event delivery (W8c).
                cx.notify();
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
            // A fresh session's composer chip picked another provider:
            // nothing was ever sent, so the pick becomes the default and a
            // new session starts on it — on the provider lane when the pick
            // is not muse. The abandoned draft's view is closed outright
            // (hanging up its child when it owns one) so the replacement
            // below never parks — and never leaks — the view just left; it
            // also stops being this project's draft, without which the
            // start below would reopen that very view.
            SessionEvent::SwitchProvider { provider } => {
                let old = view.read(cx).session_id.clone();
                self.select_new_provider(*provider, cx);
                self.drafts.retain(|_, id| *id != old);
                self.close_view(&old, cx);
                // The replacement starts on a task, but the switch is
                // already in flight as far as scripted verbs are concerned:
                // claiming it now keeps a following `send:` waiting for the
                // lane instead of failing on the closed draft — and the
                // claim is what lets the start through the one-session
                // guard above.
                self.session_switch_pending = true;
                self.switch_claim = Some(old);
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.new_session(window, cx));
                }));
            }
            // A session with turns picked "New session on X": this session
            // keeps the lane it was created on, and a new one starts on the
            // pick, which also becomes the default for later sessions.
            SessionEvent::NewSessionOnProvider { provider } => {
                self.select_new_provider(*provider, cx);
                // Same claim as the switch above: the new session starts on
                // a task, and session verbs wait for it rather than acting
                // on the session that is still open.
                self.session_switch_pending = true;
                self.switch_claim = Some(view.read(cx).session_id.clone());
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.new_session(window, cx));
                }));
            }
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
            // "Open the new session" on a handoff card, or the
            // destination marker's back-link: show that session. Rejoins
            // through a window like the rename path above.
            SessionEvent::HandoffOpenSession { destination } => {
                let destination = destination.clone();
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.resume(destination, window, cx));
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
            // A D49 play button: local-only, so no project means nowhere
            // to run rather than an error. The focus needs a window, which
            // the event does not carry, so this rejoins through one like
            // the rename and palette paths do.
            SessionEvent::RunInTerminal { command, send_enter } => {
                let (command, send_enter) = (command.clone(), *send_enter);
                if let Some(root) = self.current_project().map(|project| project.root.clone()) {
                    self.tasks.push(cx.spawn(async move |this, cx| {
                        let _ = this.update_in(cx, |this, window, cx| {
                            this.run_in_terminal(&root, &command, send_enter, window, cx);
                        });
                    }));
                }
            }
            // A D51 "Open terminal" press: local-only like the run above —
            // the dock opens and the card's tab takes focus.
            SessionEvent::OpenTerminal { tab } => {
                let tab = tab.clone();
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open_terminal_tab(tab, window, cx);
                    });
                }));
            }
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
                self.open_skills(cx);
                self.select_skill(name, cx);
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

/// Handoff between providers (H2): the application half. The machine
/// itself is [`crate::handoff::HandoffRun`]; here the runs are owned
/// (keyed by source session), the confirm dialog is shown, destinations
/// are opened, and acks advance the run to activation.
impl Harness {
    /// The view for `session_id`: the active one, else a parked one.
    fn find_view(&self, session_id: &str, cx: &gpui::App) -> Option<Entity<SessionView>> {
        if let Some(view) = self.active.clone() {
            if view.read(cx).session_id == session_id {
                return Some(view);
            }
        }
        self.session_cache.iter().find(|(id, _)| id == session_id).map(|(_, view)| view.clone())
    }

    /// "Hand off to X…" with a confirm first: freeze the pack preview and
    /// show [`handoff_confirm`](aui::transcript::handoff_confirm). A
    /// session with no turns, or a pending question / approval / an
    /// uninterruptible turn, never reaches the dialog — the refusal lands
    /// on the transcript as a Refused card instead.
    pub(super) fn show_handoff_confirm(&mut self, source: String, to: ProviderId, cx: &mut Context<Self>) {
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
            self.handoff_epoch = self.handoff_epoch.wrapping_add(1);
            let run = crate::handoff::HandoffRun::refused(
                source.clone(),
                self.handoff_epoch,
                from,
                to,
                from_model,
                refusal,
            );
            let card = run.card();
            let card_id = run.card_id.clone();
            self.handoffs.insert(source, run);
            view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
            return;
        }
        self.handoff_confirm = Some(crate::handoff::HandoffConfirmState {
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
        let Some(confirm) = self.handoff_confirm.take() else {
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
        let Some(view) = self.find_view(&source, cx) else { return };
        // A newer request supersedes an in-flight one for the same source:
        // the old run aborts (its destination shuts down when opened) and
        // the new epoch owns every later event.
        if let Some(mut old) = self.handoffs.remove(&source) {
            if old.cancellable() {
                let opened = old.cancel();
                if opened {
                    if let Some(dest) = old.destination_session.clone() {
                        self.close_view(&dest, cx);
                    }
                    if self.pending_handoff.as_ref().is_some_and(|p| p.source_session == source) {
                        self.pending_handoff = None;
                    }
                }
                let card = old.card();
                let card_id = old.card_id.clone();
                view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
            }
        }
        self.handoff_epoch = self.handoff_epoch.wrapping_add(1);
        let epoch = self.handoff_epoch;
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
                self.handoffs.insert(source, refused);
                view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
                return;
            }
        };
        let Some(pack) = facts.pack.filter(|_| facts.has_turns) else {
            run.fail("nothing to hand off — the session has no turns".to_owned());
            let card = run.card();
            let card_id = run.card_id.clone();
            self.handoffs.insert(source, run);
            view.update(cx, |view, cx| view.append_handoff_card(&card_id, card, cx));
            return;
        };
        let card_id = run.card_id.clone();
        self.handoffs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.append_handoff_card(&card_id, run.card(), cx));
        // Quiescing: a running turn is interrupted, no new sends accepted
        // (the composer refuses while the card reads Quiescing).
        if facts.running {
            view.update(cx, |view, cx| view.interrupt(cx));
        }
        run.note_quiescing();
        self.handoffs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.replace_handoff_card(&card_id, run.card(), cx));
        // Checkpointed: the pack is a point-in-time capture of the folded
        // transcript — an interrupt's late deltas may land just after it.
        run.note_checkpointed(pack);
        self.handoffs.insert(source.clone(), run.clone());
        view.update(cx, |view, cx| view.replace_handoff_card(&card_id, run.card(), cx));
        crate::baaz_log!("handoff requested epoch={epoch} {source} -> {}", to.as_str());
        // Prepared next: the destination opens in the source's workspace,
        // and the pack submits when it lands.
        self.pending_handoff = Some(crate::handoff::PendingHandoff { source_session: source, epoch });
        if to == ProviderId::Muse {
            self.select_new_provider(ProviderId::Muse, cx);
            self.session_switch_pending = true;
            self.switch_claim = Some(run.source_session.clone());
            self.new_session(window, cx);
        } else {
            let workspace = facts.workspace.clone();
            let project =
                self.projects.resolve_available(Some(&workspace), None).map(|p| p.id.clone());
            self.open_on_provider(to, project, workspace, window, cx);
        }
    }

    /// The fresh session landed: when it is a handoff's destination under
    /// the current epoch, mark Prepared, leave the origin marker, and
    /// submit the pack as its first turn. Anything else (an ordinary open,
    /// a superseded request) is left alone.
    pub(super) fn land_handoff_destination(&mut self, dest: String, cx: &mut Context<Self>) {
        let pending = self.pending_handoff.clone();
        let Some(pending) = pending else { return };
        let Some(run) = self.handoffs.get(&pending.source_session).cloned() else {
            self.pending_handoff = None;
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
        // The destination names its model once the lane reports it; until
        // then the card keeps the provider default the open used.
        if !model.is_empty() {
            run.to_model = model;
        }
        let pack = run.pack.clone();
        self.handoffs.insert(pending.source_session.clone(), run.clone());
        self.pending_handoff = None;
        if let Some(source) = self.find_view(&pending.source_session, cx) {
            let card = run.card();
            let card_id = run.card_id.clone();
            source.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
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
        let text = crate::handoff::pack_text(&pack, run.from);
        let display = crate::handoff::display_text(&pack, run.from);
        view.update(cx, |view, cx| {
            view.append_handoff_origin(origin, cx);
            view.submit_pack(text, display, cx);
        });
    }

    /// The pack's submit ack on the destination: the run holding that
    /// destination advances to Acknowledged under its own epoch — a stale
    /// ack (cancelled, failed, superseded) matches no Prepared run and is
    /// ignored — then activates at once.
    pub(super) fn acknowledge_handoff(&mut self, dest: String, cx: &mut Context<Self>) {
        let found = self
            .handoffs
            .iter()
            .find(|(_, run)| run.destination_session.as_deref() == Some(dest.as_str()))
            .map(|(source, run)| (source.clone(), run.clone()));
        let Some((source, mut run)) = found else { return };
        if !run.acknowledge(run.epoch) {
            return;
        }
        run.activate();
        let card = run.card();
        let card_id = run.card_id.clone();
        let (from, to) = (run.from, run.to);
        self.handoffs.insert(source.clone(), run);
        if let Some(view) = self.find_view(&source, cx) {
            view.update(cx, |view, cx| {
                view.replace_handoff_card(&card_id, card, cx);
                view.retire_for_handoff(to, dest.clone(), cx);
            });
        }
        self.persist_handoff_links(&source, from, &dest, to, cx);
        crate::baaz_log!("handoff activated {source} -> {dest}");
        cx.notify();
    }

    /// The pack never landed: the run fails with the reason, the source
    /// stays usable (a Failed card sends again). An event for an unknown
    /// destination is ignored — it belongs to no run.
    pub(super) fn fail_handoff(&mut self, dest: String, reason: String, cx: &mut Context<Self>) {
        let source = self
            .handoffs
            .iter()
            .find(|(_, run)| run.destination_session.as_deref() == Some(dest.as_str()))
            .map(|(source, _)| source.clone());
        let Some(source) = source else { return };
        let Some(mut run) = self.handoffs.get(&source).cloned() else { return };
        run.fail(reason.clone());
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoffs.insert(source.clone(), run);
        if let Some(view) = self.find_view(&source, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        crate::baaz_log!("handoff failed {source}: {reason}");
        cx.notify();
    }

    /// "Cancel" on a handoff card: aborts while cancellable and shuts the
    /// opened destination down. Past Acknowledged the move is done and
    /// the press is ignored.
    pub(super) fn cancel_handoff(&mut self, source: &str, card_id: &str, cx: &mut Context<Self>) {
        let Some(mut run) = self.handoffs.get(source).cloned() else { return };
        if run.card_id != card_id || !run.cancellable() {
            return;
        }
        let opened = run.cancel();
        if opened {
            if let Some(dest) = run.destination_session.clone() {
                self.close_view(&dest, cx);
            }
        }
        if self.pending_handoff.as_ref().is_some_and(|p| p.source_session == source) {
            self.pending_handoff = None;
        }
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoffs.insert(source.to_owned(), run);
        if let Some(view) = self.find_view(source, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        crate::baaz_log!("handoff cancelled {source}");
        cx.notify();
    }

    /// The destination never opened: fail the pending run with the open's
    /// reason, so the source card names it and the source stays usable.
    /// A call with nothing pending is a no-op.
    pub(super) fn fail_pending_handoff(&mut self, reason: String, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_handoff.take() else { return };
        let Some(mut run) = self.handoffs.get(&pending.source_session).cloned() else { return };
        if run.epoch != pending.epoch {
            return;
        }
        run.fail(reason.clone());
        let card = run.card();
        let card_id = run.card_id.clone();
        self.handoffs.insert(pending.source_session.clone(), run);
        if let Some(view) = self.find_view(&pending.source_session, cx) {
            view.update(cx, |view, cx| view.replace_handoff_card(&card_id, card, cx));
        }
        crate::baaz_log!("handoff failed {}: {reason}", pending.source_session);
        cx.notify();
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
        if self.provider_sessions.contains_key(source) || self.provider_sessions.contains_key(dest) {
            crate::provider_sessions::note_handoff(&mut self.provider_sessions, source, from.as_str(), dest);
            crate::provider_sessions::write(&self.provider_sessions);
        }
        let solo_source = !self.provider_sessions.contains_key(source);
        let solo_dest = !self.provider_sessions.contains_key(dest);
        if solo_source || solo_dest {
            if solo_source {
                self.set_override(source, |meta| meta.handoff_to = Some(dest.to_owned()), cx);
            }
            if solo_dest {
                self.set_override(
                    dest,
                    |meta| {
                        meta.handoff_from = Some(source.to_owned());
                        meta.handoff_from_provider = Some(from.as_str().to_owned());
                    },
                    cx,
                );
            }
            crate::sessions::write(&self.overrides);
        }
        self.merge_provider_rows();
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
            steps_begin_with_new: false,
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
    fn a_script_headed_by_new_boots_no_session() {
        // `new;setprovider:codex` starts its own session: booting one
        // eagerly ahead of it opened two (two lane-open lines, two rows).
        // Remove the arm and boot opens again beside the head's `new`.
        assert_eq!(
            boot_decision(BootState { steps_begin_with_new: true, ..boot_state() }),
            BootDecision::Idle
        );
        // Any other script still boots: the session has to come from
        // somewhere.
        assert_eq!(boot_decision(boot_state()), BootDecision::Open);
        // An explicit `--session` resumes even when the steps begin with
        // `new`: the head's `new` then reuses or replaces, never doubles
        // the boot.
        assert_eq!(
            boot_decision(BootState {
                session_arg: Some("s-1"),
                steps_begin_with_new: true,
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
        assert!(steps_ready_for(true, false, false, true, true, false, false));
        assert!(!steps_ready_for(true, false, false, true, false, false, false));
        assert!(!steps_ready_for(true, false, false, false, true, false, false));
        assert!(!steps_ready_for(true, false, false, false, false, false, false));
        // Replay and offline have no wire: the open session alone decides,
        // connected or not.
        assert!(steps_ready_for(true, true, false, false, true, false, false));
        assert!(steps_ready_for(true, false, true, false, true, false, false));
        assert!(!steps_ready_for(true, true, false, false, false, false, false));
        assert!(!steps_ready_for(true, false, true, false, false, false, false));
        // A window-only script needs no session at all. This is the arm that
        // was missing: `--login signed-in --no-connect` opens no session, so
        // every window verb was silently skipped and the capture still
        // exited 0. Five right-pane entries were written against that and
        // produced five identical screenshots of an empty shell.
        assert!(steps_ready_for(true, false, true, false, false, true, false));
        assert!(steps_ready_for(true, false, false, false, false, true, false));
        assert!(steps_ready_for(true, true, false, false, false, true, false));
        // An in-flight open — `session/start` or a provider lane — holds
        // session verbs until it lands, exactly like a missing session
        // does: `new;setprovider:codex;send:` failed with `no open
        // session` because `send` ran before the lane finished. Window
        // verbs never touch the session, so they run on regardless.
        assert!(!steps_ready_for(true, false, false, true, true, false, true));
        assert!(!steps_ready_for(true, false, true, false, true, false, true));
        assert!(steps_ready_for(true, false, true, false, false, true, true));
        assert!(steps_ready_for(true, false, false, true, false, true, true));
        // Still nothing to do with no steps pending, whatever else holds.
        assert!(!steps_ready_for(false, false, true, false, false, true, false));
        // No script left, nowhere: never ready, in every mode.
        assert!(!steps_ready_for(false, false, false, true, true, false, false));
        assert!(!steps_ready_for(false, true, false, false, true, false, false));
        assert!(!steps_ready_for(false, false, true, false, true, false, false));
    }

    #[test]
    fn the_second_drain_finds_nothing_to_do() {
        let mut holder = vec!["new".to_owned(), "wait:3000".to_owned()];
        assert_eq!(drain_steps(&mut holder), vec!["new".to_owned(), "wait:3000".to_owned()]);
        assert!(holder.is_empty());
        assert!(drain_steps(&mut holder).is_empty());
    }

    // ------------------------------------------------- W2 provider-lane open

    /// A bootable [`crate::Args`] pointed at a hermetic state dir, mirroring
    /// the app tests' helper: no store read or write escapes the temp dir.
    fn lane_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            bench_out: None,
            sidebar_fixture: None,
            no_project: false,
        }
    }

    fn lane_state(
        name: &str,
    ) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-w2-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn lane_restore(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf),
    ) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    /// A connected scripted provider through the [`crate::providers::ProviderFactory`]
    /// seam: what the lane tests drive without spawning a child.
    fn scripted_factory() -> crate::providers::ProviderFactory {
        use provider::ProviderAdapter as _;
        std::sync::Arc::new(|_: ProviderId| {
            let mut adapter = provider::scripted::ScriptedProvider::new();
            adapter.connect(&conn::connect_info())?;
            Ok(provider::Provider::new(adapter))
        })
    }

    /// A factory whose provider answers `ResumeSession` by echoing the
    /// session: what the forked-open path drives without spawning a child.
    fn resumable_factory() -> crate::providers::ProviderFactory {
        use provider::ProviderAdapter as _;
        struct Resumable {
            rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
            connected: bool,
        }
        impl provider::ProviderAdapter for Resumable {
            fn id(&self) -> provider::ProviderId {
                aui_protocol::Provider::Codex
            }
            fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
                self.connected = true;
                Ok(provider::Handshake {
                    provider: aui_protocol::Provider::Codex,
                    agent_name: "resumable".into(),
                    agent_version: "0.0.0".into(),
                })
            }
            fn capabilities(&self) -> provider::CapabilitySet {
                use provider::{Capability, CapabilityState};
                let native = CapabilityState::Native;
                let off = || CapabilityState::Unavailable {
                    reason: "the resumable double opens and resumes sessions only".into(),
                };
                provider::CapabilitySet::new([
                    (Capability::SessionLifecycle, native.clone()),
                    (Capability::ForkSession, off()),
                    (Capability::CompactSession, off()),
                    (Capability::SessionConfig, off()),
                    (Capability::SessionShell, off()),
                    (Capability::SubmitTurn, off()),
                    (Capability::SteerTurn, off()),
                    (Capability::TurnControl, off()),
                    (Capability::ModelCatalog, native.clone()),
                    (Capability::Approvals, off()),
                    (Capability::Questions, off()),
                    (Capability::Transcript, off()),
                    (Capability::Account, off()),
                    (Capability::ClientTools, off()),
                    (Capability::ReasoningTraces, off()),
                    (Capability::SubagentTurns, off()),
                ])
            }
            fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
                if !self.connected {
                    return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
                }
                match command {
                    provider::Command::OpenSession { .. } => {
                        Ok(provider::Ack::Session { session_id: "s-open".into(), title: None })
                    }
                    provider::Command::ResumeSession { session_id, .. } => {
                        Ok(provider::Ack::Session { session_id, title: None })
                    }
                    provider::Command::ListModels { .. } => {
                        Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "resumable".into() })
                    }
                    provider::Command::ListPending { .. } => {
                        Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                    }
                    other => Err(provider::ProviderError::unsupported(
                        other.capability(),
                        "the resumable double opens and resumes sessions only",
                    )),
                }
            }
            fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
                self.rx.clone()
            }
            fn shutdown(&mut self) {}
        }
        std::sync::Arc::new(|_: ProviderId| {
            let (_, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
            let mut adapter = Resumable { rx, connected: false };
            adapter.connect(&conn::connect_info())?;
            Ok(provider::Provider::new(adapter))
        })
    }

    /// A factory that never connects: the failed-open path.
    fn failing_factory(reason: &str) -> crate::providers::ProviderFactory {
        let reason = reason.to_owned();
        std::sync::Arc::new(move |_: ProviderId| {
            Err(provider::ProviderError::Unavailable { reason: reason.clone() })
        })
    }

    /// A real child handle with no session behind it: `true` exits at once,
    /// so nothing can answer on it — which is exactly what proves the lane
    /// path issues no muse `session/start`: had it tried, the dead child
    /// would have failed it into a dialog instead of a lane view.
    fn dead_client() -> std::sync::Arc<MuseClient> {
        std::sync::Arc::new(
            MuseClient::spawn(&muse_client::MuseConfig {
                program: std::path::PathBuf::from("true"),
                trust_workspace: false,
                no_session_log: false,
                extra_args: Vec::new(),
            })
            .expect("a throwaway child spawns"),
        )
    }

    fn lane_harness(
        vc: &mut gpui::VisualTestContext,
        dir: &std::path::Path,
    ) -> gpui::Entity<Harness> {
        vc.update(|window, cx| {
            cx.new(|cx| Harness::new(lane_args(dir), crate::shot::CaptureToken::default(), window, cx))
        })
    }

    #[gpui::test]
    fn picking_a_non_muse_provider_opens_a_lane_and_starts_no_muse_session(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("open");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = scripted_factory();
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the provider session is open");
            assert!(
                view.read(cx).is_provider_lane(),
                "the pick opens a provider lane, not a muse session"
            );
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
            assert!(!harness.session_switch_pending, "the switch landed");
            assert!(
                harness.overlays.read(cx).dialog.is_none(),
                "no failure dialog: no muse session/start was ever issued at the dead child"
            );
            let open_id = view.read(cx).session_id.clone();
            assert!(
                harness.provider_sessions.contains_key(&open_id),
                "the lane open writes the local record"
            );
            assert!(
                harness
                    .sessions
                    .iter()
                    .any(|entry| entry.id == open_id && entry.provider.as_deref() == Some("codex")),
                "a provider sidebar row names the lane session"
            );
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn a_failed_provider_connect_surfaces_an_error_and_opens_no_view(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("fail");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = failing_factory("no child here");
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::ClaudeCode, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.active.is_none(), "a failed connect opens no view");
            assert!(!harness.session_switch_pending, "the verbs waiting on the switch are released");
            let dialog = harness.overlays.read(cx).dialog.as_ref().expect("the failure is dialogued");
            assert_eq!(dialog.title, "Couldn't start Claude Code");
            assert!(dialog.detail.contains("no child here"), "the reason survives: {}", dialog.detail);
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn opening_a_fork_resumes_it_on_a_new_lane_of_the_same_provider(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("fork");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = resumable_factory();
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_forked_on_provider(
                    ProviderId::Codex,
                    "s-fork".to_owned(),
                    workspace,
                    window,
                    cx,
                );
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the fork is open");
            assert!(view.read(cx).is_provider_lane(), "the fork opens a provider lane");
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex, "on the same provider");
            assert_eq!(view.read(cx).session_id, "s-fork", "resuming the fork, not a fresh session");
            assert!(!harness.session_switch_pending, "the switch landed");
            assert!(
                harness.overlays.read(cx).dialog.is_none(),
                "no failure dialog: the resume answered a session"
            );
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn new_session_in_routes_a_provider_pick_to_the_lane_offline(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("route");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        // No client: scripted chrome. The pick alone must route past muse.
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.select_new_provider(ProviderId::Codex, cx));
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.new_session_in(None, window, cx));
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("a session opened");
            assert!(
                view.read(cx).is_provider_lane(),
                "`new` on a provider pick ends on a provider lane"
            );
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn switch_provider_replaces_the_fresh_draft_without_leaking_its_view(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("switch");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        // A fresh muse draft first: scripted chrome opens it locally.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.new_session_in(None, window, cx));
        });
        let old = vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the draft opened");
            assert!(!view.read(cx).is_provider_lane(), "the draft starts on the muse lane");
            view.read(cx).session_id.clone()
        });
        // The `SwitchProvider` handler's own sequence: remember the pick,
        // close the empty draft's view, start on the pick.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.select_new_provider(ProviderId::Codex, cx);
                harness.drafts.retain(|_, id| *id != old);
                harness.close_view(&old, cx);
                harness.new_session(window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the replacement opened");
            assert_ne!(view.read(cx).session_id, old, "the draft did not survive the switch");
            assert!(view.read(cx).is_provider_lane(), "the replacement rides the provider lane");
            assert!(
                !harness.session_cache.iter().any(|(id, _)| id == &old),
                "the discarded draft was closed, never parked"
            );
            assert!(
                !harness.drafts.values().any(|id| id == &old),
                "the discarded draft names no project's draft anymore"
            );
        });
        lane_restore(state);
    }

    // ------------------------------------------------- W6 one session per new

    /// A factory that counts every child it spawns, around the scripted
    /// stand-in: the W6 tests measure provider opens with it instead of
    /// grepping stderr.
    fn counting_factory() -> (crate::providers::ProviderFactory, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use provider::ProviderAdapter as _;
        let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let factory_opens = opens.clone();
        let factory: crate::providers::ProviderFactory = std::sync::Arc::new(move |_: ProviderId| {
            factory_opens.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut adapter = provider::scripted::ScriptedProvider::new();
            adapter.connect(&conn::connect_info())?;
            Ok(provider::Provider::new(adapter))
        });
        (factory, opens)
    }

    fn opens_of(opens: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
        opens.load(std::sync::atomic::Ordering::Relaxed)
    }

    #[gpui::test]
    fn a_second_new_while_a_switch_is_in_flight_starts_nothing(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("w6-new-twice");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        let (factory, opens) = counting_factory();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = factory;
                harness.client = Some(dead_client());
                harness.new_provider = ProviderId::Codex.as_str().to_owned();
            });
        });
        // Two `new`s back to back: the second lands while the first open
        // is still in flight. Remove the guard and the factory spawns
        // twice for one action.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.new_session(window, cx);
                harness.new_session(window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(opens_of(&opens), 1, "one `new` pair spawns one child");
            let view = harness.active.clone().expect("the session opened");
            assert!(view.read(cx).is_provider_lane(), "it rides the provider lane");
            assert!(!harness.session_switch_pending, "the switch landed");
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn switch_provider_through_the_handler_leaves_one_row_and_one_open(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("w6-switch");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        let (factory, opens) = counting_factory();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = factory;
                harness.client = Some(dead_client());
                harness.new_provider = ProviderId::Codex.as_str().to_owned();
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.new_session(window, cx));
        });
        vc.run_until_parked();
        let draft = vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(opens_of(&opens), 1, "the draft opens one child");
            harness.active.clone().expect("the draft opened").read(cx).session_id.clone()
        });
        // Through the real `SwitchProvider` event, not the handler's steps
        // by hand: the close is synchronous, the replacement starts on a
        // task. Remove the claim (or the synchronous pending) and the
        // replacement never starts — or a following verb fails on the
        // closed draft.
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                let view = harness.active.clone().expect("the draft is open");
                harness.on_session_event(
                    view,
                    &SessionEvent::SwitchProvider { provider: ProviderId::ClaudeCode },
                    cx,
                );
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(opens_of(&opens), 2, "draft plus exactly one replacement");
            let view = harness.active.clone().expect("the replacement opened");
            assert_eq!(view.read(cx).provider_kind(), ProviderId::ClaudeCode);
            // (No id inequality: the scripted double mints one fixed id,
            // so draft and replacement share it; the lane change above is
            // what proves the replacement.)
            let _ = draft;
            let rows: Vec<_> =
                harness.sessions.iter().filter(|entry| entry.provider.is_some()).collect();
            assert_eq!(rows.len(), 1, "exactly one provider row names the lane");
            assert_eq!(rows[0].id, view.read(cx).session_id, "the row is the replacement");
            assert!(!harness.session_switch_pending, "the switch landed");
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn a_late_finishing_earlier_open_never_steals_the_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("w6-epoch");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        let (factory, opens) = counting_factory();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = factory;
                harness.client = Some(dead_client());
            });
        });
        // Two asks back to back on different lanes; whichever background
        // open finishes first, the Codex ask is stale. Remove the epoch
        // and both finishes land: two rows, and the active lane is
        // whichever happened to finish last.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace.clone(), window, cx);
                harness.open_on_provider(ProviderId::ClaudeCode, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(opens_of(&opens), 2, "both asks spawned");
            let view = harness.active.clone().expect("a lane opened");
            assert_eq!(
                view.read(cx).provider_kind(),
                ProviderId::ClaudeCode,
                "the last ask wins, however the finishes order"
            );
            let rows: Vec<_> =
                harness.sessions.iter().filter(|entry| entry.provider.is_some()).collect();
            assert_eq!(rows.len(), 1, "the stale finish leaves no row");
            assert_eq!(rows[0].id, view.read(cx).session_id, "the row is the winner");
            assert!(!harness.session_switch_pending, "the switch landed");
        });
        lane_restore(state);
    }

    #[gpui::test]
    fn closing_a_provider_view_hangs_up_its_child(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("close");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.select_new_provider(ProviderId::Codex, cx);
                harness.new_session_in(None, window, cx);
            });
        });
        let open_id = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.close_view(&open_id, cx));
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.active.is_none(), "the closed view is gone from the centre");
            assert!(
                !harness.session_cache.iter().any(|(id, _)| id == &open_id),
                "the closed view is gone from the MRU too"
            );
        });
        // The shutdown itself is proven in the lane tests (dropping a lane
        // view hangs up its child: a later command is refused as shut
        // down); here the close path above called it outright.
        lane_restore(state);
    }

    /// A factory that records what it was asked and replays one history
    /// turn on resume: the reopen path's double, beside `resumable_factory`.
    /// The replayed user turn plus its finished assistant turn is what a
    /// real adapter replays from its backend store.
    fn recording_resumable_factory() -> (
        crate::providers::ProviderFactory,
        std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
    ) {
        use provider::ProviderAdapter as _;
        struct RecordingResumable {
            tx: crossbeam_channel::Sender<provider::ProviderEvent>,
            rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
            connected: bool,
            commands: std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
        }
        impl provider::ProviderAdapter for RecordingResumable {
            fn id(&self) -> provider::ProviderId {
                aui_protocol::Provider::Codex
            }
            fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
                self.connected = true;
                Ok(provider::Handshake {
                    provider: aui_protocol::Provider::Codex,
                    agent_name: "recording-resumable".into(),
                    agent_version: "0.0.0".into(),
                })
            }
            fn capabilities(&self) -> provider::CapabilitySet {
                use provider::{Capability, CapabilityState};
                let native = CapabilityState::Native;
                let off = || CapabilityState::Unavailable {
                    reason: "the recording double opens and resumes sessions only".into(),
                };
                provider::CapabilitySet::new([
                    (Capability::SessionLifecycle, native.clone()),
                    (Capability::SubmitTurn, native.clone()),
                    (Capability::ModelCatalog, native.clone()),
                    (Capability::ForkSession, off()),
                    (Capability::CompactSession, off()),
                    (Capability::SessionConfig, off()),
                    (Capability::SessionShell, off()),
                    (Capability::SteerTurn, off()),
                    (Capability::TurnControl, off()),
                    (Capability::Approvals, off()),
                    (Capability::Questions, off()),
                    (Capability::Transcript, off()),
                    (Capability::Account, off()),
                    (Capability::ClientTools, off()),
                    (Capability::ReasoningTraces, off()),
                    (Capability::SubagentTurns, off()),
                ])
            }
            fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
                if !self.connected {
                    return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
                }
                self.commands.lock().expect("commands").push(command.clone());
                match command {
                    provider::Command::OpenSession { .. } => {
                        Ok(provider::Ack::Session { session_id: "s-open".into(), title: None })
                    }
                    provider::Command::ResumeSession { session_id, .. } => {
                        // The backend's stored transcript, replayed as
                        // deltas the way a real resume replays them.
                        let _ = self.tx.send(provider::ProviderEvent::Deltas {
                            session_id: Some(session_id.clone()),
                            deltas: vec![
                                aui_protocol::Delta::TurnStarted {
                                    turn: aui_protocol::Turn::User {
                                        id: "u-old".into(),
                                        text: "Restore the header".into(),
                                        attachments: Vec::new(),
                                        mentions: Vec::new(),
                                        timestamp: None,
                                    },
                                },
                                aui_protocol::Delta::TurnStarted {
                                    turn: aui_protocol::Turn::Assistant {
                                        id: "a-old".into(),
                                        blocks: Vec::new(),
                                        meta: aui_protocol::TurnMeta::default(),
                                        timestamp: None,
                                    },
                                },
                                aui_protocol::Delta::BlockAdded {
                                    turn_id: "a-old".into(),
                                    block: aui_protocol::Block::Text {
                                        text: "The header is restored".into(),
                                        streaming: false,
                                    },
                                },
                                aui_protocol::Delta::TurnFinished {
                                    turn_id: "a-old".into(),
                                    meta: aui_protocol::TurnMeta {
                                        model: "codex-mini".into(),
                                        tokens_in: 1200,
                                        tokens_out: 300,
                                        cost_usd: 0.04,
                                        ..Default::default()
                                    },
                                },
                            ],
                        });
                        Ok(provider::Ack::Session { session_id, title: None })
                    }
                    provider::Command::ListModels { .. } => {
                        Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "recording".into() })
                    }
                    provider::Command::ListPending { .. } => {
                        Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                    }
                    other => Err(provider::ProviderError::unsupported(
                        other.capability(),
                        "the recording double opens and resumes sessions only",
                    )),
                }
            }
            fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
                self.rx.clone()
            }
            fn shutdown(&mut self) {}
        }
        let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let factory_commands = commands.clone();
        let factory = std::sync::Arc::new(move |_: ProviderId| {
            let (tx, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
            let mut adapter = RecordingResumable {
                tx,
                rx,
                connected: false,
                commands: factory_commands.clone(),
            };
            adapter.connect(&conn::connect_info())?;
            Ok(provider::Provider::new(adapter))
        });
        (factory, commands)
    }

    /// The W5 reopen arm, end to end at the seam: a provider session
    /// opened, abandoned (its views dropped like a restart drops them),
    /// reinstalled from the record and clicked — travels as
    /// `ResumeSession` for the stored id, lands a lane view showing the
    /// replayed transcript, and records the replayed turn once.
    #[gpui::test]
    fn a_provider_session_reopens_with_its_history_after_a_restart(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("reopen");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        let (factory, commands) = recording_resumable_factory();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = factory;
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        let open_id = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
        });
        assert_eq!(open_id, "s-open");
        // One turn settles, so the record counts history: a reopened
        // draft with no turns opens fresh instead (see the next test).
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                crate::provider_sessions::note_settled_turn_counted(
                    &mut harness.provider_sessions,
                    &open_id,
                    1,
                );
                crate::provider_sessions::write(&harness.provider_sessions);
            });
        });
        // The restart: the harness forgets its views (active and parked
        // alike) and its rows, but the record the open wrote survives on
        // disk. The forgotten views stay alive in this local until
        // teardown: lanes now bridge synchronously in tests (see
        // `conn::gate_sync`), so dropping them is harmless — a real
        // restart drops them with the process instead.
        let mut forgotten = Vec::new();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                if let Some(view) = harness.active.take() {
                    forgotten.push(view);
                }
                forgotten.extend(harness.session_cache.drain(..).map(|(_, view)| view));
                harness.sessions.clear();
                harness.provider_sessions = crate::provider_sessions::read();
                harness.merge_provider_rows();
            });
        });
        let _forgotten = forgotten;
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(
                harness.provider_sessions.contains_key(&open_id),
                "the record survives the restart"
            );
            assert!(
                harness
                    .sessions
                    .iter()
                    .any(|entry| entry.id == open_id && entry.provider.as_deref() == Some("codex")),
                "the sidebar reinstalls the provider row from the record"
            );
        });
        // The click: no parked view, so the record reopens.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.resume_quiet(open_id.clone(), window, cx));
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the resumed lane is open");
            assert!(view.read(cx).is_provider_lane(), "the click reopens a lane, not a muse view");
            assert_eq!(view.read(cx).session_id, open_id);
            assert!(!harness.session_switch_pending, "the switch landed");
            assert!(
                harness.overlays.read(cx).dialog.is_none(),
                "no failure dialog on a clean resume"
            );
            let texts: Vec<String> = view
                .read(cx)
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .flat_map(|turn| turn.blocks())
                        .filter_map(|block| match block {
                            aui_protocol::Block::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                texts.iter().any(|text| text.contains("The header is restored")),
                "the view shows the replayed transcript, drew {texts:?}"
            );
        });
        let sent = commands.lock().expect("commands").clone();
        assert!(
            sent.iter().any(|command| matches!(
                command,
                provider::Command::ResumeSession { session_id, metadata_only: false, .. }
                if session_id == &open_id
            )),
            "the reopen travels as ResumeSession for the stored id, drew {sent:?}"
        );
        // The replayed turn lands in the ledger once, tagged with its
        // lane and the adapter-reported cost.
        vc.update(|_, _| {
            let connection = crate::usage::open_at(&crate::usage::db_path()).expect("ledger opens");
            let kept = crate::usage::find_turn(&connection, &open_id, "a-old").expect("replayed turn recorded");
            assert_eq!(kept.provider, "codex");
            assert_eq!(kept.cost_usd, 0.04);
            assert_eq!(crate::usage::count_for_session(&connection, &open_id), 1);
        });
        lane_restore(state);
    }

    /// The W5b draft arm: a record with no settled turns holds no history
    /// anywhere, so clicking it opens fresh (`OpenSession`, never
    /// `ResumeSession`) and the stale record leaves with it — the sidebar
    /// never accumulates dead rows. Resuming it would replay nothing.
    #[gpui::test]
    fn a_provider_draft_with_no_turns_opens_fresh_instead_of_resuming(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("draft-reopen");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        let (factory, commands) = recording_resumable_factory();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = factory;
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        let open_id = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
        });
        assert_eq!(open_id, "s-open");
        // The restart, with no turn ever settling: the record counts zero.
        let mut forgotten = Vec::new();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                if let Some(view) = harness.active.take() {
                    forgotten.push(view);
                }
                forgotten.extend(harness.session_cache.drain(..).map(|(_, view)| view));
                harness.sessions.clear();
                harness.provider_sessions = crate::provider_sessions::read();
                harness.merge_provider_rows();
            });
        });
        let _forgotten = forgotten;
        vc.update(|_, cx| {
            assert_eq!(
                baaz.read(cx).provider_sessions.get(&open_id).map(|record| record.turns),
                Some(0),
                "the untouched draft counts no turns"
            );
        });
        // The click: the draft opens fresh, it is never resumed.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.resume_quiet(open_id.clone(), window, cx));
        });
        vc.run_until_parked();
        let sent = commands.lock().expect("commands").clone();
        assert!(
            sent.iter().any(|command| matches!(
                command,
                provider::Command::OpenSession { .. }
            )),
            "the draft opens fresh, drew {sent:?}"
        );
        assert!(
            !sent.iter().any(|command| matches!(
                command,
                provider::Command::ResumeSession { .. }
            )),
            "nothing is resumed for a draft with no history, drew {sent:?}"
        );
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().expect("the fresh lane is open");
            assert!(view.read(cx).is_provider_lane(), "a lane opens, not a muse view");
            assert!(
                harness.overlays.read(cx).dialog.is_none(),
                "no failure dialog on a fresh open"
            );
            assert_eq!(
                harness.provider_sessions.get(&open_id).map(|record| record.turns),
                Some(0),
                "the reopened record is the fresh one"
            );
        });
        lane_restore(state);
    }

    /// The W5 title arm through the real rejoin: a generated title the
    /// muse side session wrote lands on the provider row's label.
    #[gpui::test]
    fn a_generated_title_lands_on_a_provider_row(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("retitle");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = scripted_factory();
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        let open_id = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
        });
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.set_override(&open_id, |meta| {
                    meta.generated_title = Some("Shiny generated".into());
                }, cx);
            });
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let row = harness.sessions.iter().find(|entry| entry.id == open_id).expect("the row");
            assert_eq!(row.label, "Shiny generated", "the auto-title names the provider row");
        });
        lane_restore(state);
    }

    /// The W5 delete arm: forgetting a provider session drops its record,
    /// its row and its views together — and dropping the views hangs the
    /// child up, so nothing is orphaned.
    #[gpui::test]
    fn deleting_a_provider_session_forgets_it_entirely(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let state = lane_state("delete");
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                harness.provider_factory = scripted_factory();
                harness.client = Some(dead_client());
            });
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace();
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        let open_id = vc.update(|_, cx| {
            baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
        });
        // The deleted view stays alive in this local until teardown
        // (lanes bridge synchronously in tests, see `conn::gate_sync`);
        // the harness-side removal below is what this proves, and the
        // lane tests prove a dropped view shuts its child down.
        let held = vc.update(|_, cx| baaz.read(cx).active.clone());
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.delete_provider_session(&open_id, cx));
        });
        let _held = held;
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(!harness.provider_sessions.contains_key(&open_id), "the record is gone");
            assert!(
                !harness.sessions.iter().any(|entry| entry.id == open_id),
                "the row is gone"
            );
            assert!(harness.active.is_none(), "the open view is gone");
            assert!(
                !harness.session_cache.iter().any(|(id, _)| id == &open_id),
                "no parked view survives to reopen it"
            );
            assert!(
                !crate::provider_sessions::read().contains_key(&open_id),
                "the file forgets it too"
            );
        });
        lane_restore(state);
    }
}
