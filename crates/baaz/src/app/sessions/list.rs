//! Listing sessions: reading Muse's index, the sessions store and the
//! scripted fixture, joining provider records into rows (rejoin and
//! `merge_provider_rows`), the provisional boot rows, and the handoff
//! chain index the sidebar groups by.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// One `--sidebar-fixture` row: the wire's shape, spelled as JSON.
///
/// `label` stands in for the index title a live row would carry.
#[derive(serde::Deserialize)]
pub(super) struct FixtureSession {
    /// The Muse session id.
    #[serde(rename = "sessionId")]
    session_id: String,
    /// The session's workspace root; relative roots resolve against the
    /// launch directory at load.
    #[serde(rename = "workspaceRoot")]
    pub(super) workspace_root: String,
    /// The row's label.
    label: String,
    /// RFC3339 last activity.
    #[serde(rename = "updatedAt")]
    updated_at: String,
    /// Content-activity recency, spelled as the server sends it
    /// (`lastActivityAt`): what the row sorts and reads by — a resume
    /// stamps `updatedAt` but never this. Absent in older captures, where
    /// the row falls back to creation time.
    #[serde(rename = "lastActivityAt", default)]
    last_activity_at: Option<String>,
    /// Completed turns.
    #[serde(rename = "turnCount", default)]
    pub(super) turn_count: u64,
    /// `running` for a live session, anything else for a settled one.
    #[serde(default = "fixture_settled")]
    pub(super) status: String,
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
    /// A turn finished unread (task 3.1), standing in for the store's
    /// `unread` record: what `DoneUnread` stands on.
    #[serde(default)]
    unread: bool,
    /// Which provider serves the row, by its wire id (`claude-code`,
    /// `codex`, `muse`); absent or unknown keeps what the join resolved.
    #[serde(default)]
    provider: Option<String>,
}

/// [`FixtureSession::status`] without the field: settled, never running.
fn fixture_settled() -> String {
    "idle".to_owned()
}

/// A fixture row as the wire would have listed it: joined through
/// [`SessionEntry::join`] with a synthetic `Session`, so resolution and
/// grouping read exactly what a live list would have given them — then
/// labelled with the fixture's own label.
pub(super) fn fixture_entry(row: &FixtureSession, launch: &std::path::Path, projects: &Projects) -> SessionEntry {
    let root = std::path::PathBuf::from(&row.workspace_root);
    let root = if root.is_absolute() { root } else { launch.join(root) };
    let session = crate::list_cache::ListedRow {
        session_id: row.session_id.clone(),
        created_at: row.updated_at.clone(),
        last_activity_at: row.last_activity_at.clone(),
        updated_at: row.updated_at.clone(),
        status: Some(if row.status == "running" { "running" } else { "idle" }.to_owned()),
        turn_count: row.turn_count,
        workspace_root: Some(root.to_string_lossy().into_owned()),
        ..Default::default()
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
        match provider::facts::Attention::parse(flag) {
            provider::facts::Attention::Other(_) => {}
            known => entry.attention.push(known),
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
    entry.unread = row.unread;
    if let Some(provider) = row.provider.as_deref().and_then(crate::providers::ProviderId::parse_known) {
        entry.provider = provider;
    }
    entry
}

impl Harness {
    // -------------------------------------------------------------- sessions

    /// Read the local index; it is a cache, not a source of truth.
    ///
    /// Incremental (see [`index::read_changed`]): only rows whose stamp moved
    /// since the last read are fetched, and the first read of a launch
    /// starts from Baaz's own saved copy, so the sidebar's labels land
    /// without a scan of Muse's 700 MB table. That first read also queues
    /// one full read in the background, which catches anything Muse changed
    /// without bumping a row's stamp.
    pub(in crate::app) fn load_index(&mut self, cx: &mut Context<Self>) {
        crate::log::boot_mark("index-read-sent");
        let first = !self.index_loaded;
        // A refcount, not a copy: the index is shared (speed S4).
        let previous = std::sync::Arc::clone(&self.index);
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                crate::log::boot_mark("index-read-start");
                let cache = index::cache_path();
                let previous = if previous.is_empty() { std::sync::Arc::new(index::load_cache(&cache)) } else { previous };
                let map = index::read_changed(&index::index_path(), &previous);
                let changed = (!first).then(|| index::changed_ids(&previous, &map)).flatten();
                if changed.as_ref().is_none_or(|ids| !ids.is_empty()) && map != *previous {
                    index::save_cache(&cache, &map);
                }
                // The saved `session/list` too, on the first read: the boot
                // rows join it exactly as the list reply will, so nothing
                // (labels, search rows) changes when the reply lands.
                let listed: Option<std::sync::Arc<crate::list_cache::ListCache<crate::list_cache::ListedRow>>> = if first {
                    crate::list_cache::load(&crate::list_cache::cache_path()).map(std::sync::Arc::new)
                } else {
                    None
                };
                // Warm the run's canonical-root memo here, off the UI
                // thread: the provisional rows join on the UI thread and
                // asked the disk once per distinct root, cold — ~2,100
                // `realpath`s, 236–330 ms of UI thread at boot on the
                // owner's store (2026-10-31).
                if let Some(listed) = &listed {
                    let mut seen = std::collections::HashSet::new();
                    for root in listed.sessions.iter().filter_map(|s| s.workspace_root.as_deref()) {
                        if seen.insert(root) {
                            let _ = crate::projects::canonical_cached(root);
                        }
                    }
                }
                crate::log::boot_mark(&format!(
                    "index-read-done rows={} cached={} listed={} changed={} in={}ms",
                    map.len(),
                    previous.len(),
                    listed.as_ref().map_or(0, |l| l.sessions.len()),
                    changed.as_ref().map_or_else(|| "all".to_owned(), |ids| ids.len().to_string()),
                    at.elapsed().as_millis()
                ));
                (std::sync::Arc::new(map), changed, listed)
            },
            move |this, (index, changed, listed), cx| {
                if this.saved_list.is_none() {
                    this.saved_list = listed;
                }
                this.apply_index_delta(index, changed, cx);
                if first {
                    this.reconcile_index(cx);
                }
            },
        );
    }

    /// The full read behind [`Self::load_index`]'s first incremental one:
    /// applied (and saved) only when it differs, so a launch whose cache was
    /// right pays a background read and nothing on the UI thread.
    fn reconcile_index(&mut self, cx: &mut Context<Self>) {
        self.wire_call(
            cx,
            move || {
                let at = std::time::Instant::now();
                let map = index::read();
                crate::log::boot_mark(&format!(
                    "index-reconcile-done rows={} in={}ms",
                    map.len(),
                    at.elapsed().as_millis()
                ));
                map
            },
            |this, index, cx| {
                if index != *this.index && !index.is_empty() {
                    let cache = index::cache_path();
                    let index = std::sync::Arc::new(index);
                    let snapshot = std::sync::Arc::clone(&index);
                    cx.background_spawn(async move { index::save_cache(&cache, &snapshot) }).detach();
                    this.apply_index_delta(index, None, cx);
                }
            },
        );
    }

    /// Land an index read whose changed rows are known (`Some`, computed
    /// off the UI thread by [`index::changed_ids`]): a finished turn moves
    /// one row, so only that row rejoins and only its search row is
    /// rebuilt (speed S4: the whole-list rejoin, search-row build and
    /// visible-list rebuild were ~12 ms of UI thread per finished Muse
    /// turn at 2,600 rows). `None` — the first read, a removal, a large
    /// change — lands it whole through [`Self::apply_index`].
    pub(crate) fn apply_index_delta(
        &mut self,
        index: std::sync::Arc<std::collections::HashMap<String, index::IndexEntry>>,
        changed: Option<std::collections::HashSet<String>>,
        cx: &mut Context<Self>,
    ) {
        match changed {
            Some(ids) if self.sessions_loaded && self.index_loaded => {
                self.index = index;
                if ids.is_empty() {
                    return;
                }
                crate::log::boot_mark(&format!("index-reply rows={} changed={}", self.index.len(), ids.len()));
                self.rejoin_scoped(Some(&ids));
                self.rebuild_search_index_for(&ids, cx);
                let _ = self.sync_sidebar_pane(cx);
                cx.notify();
            }
            _ => self.apply_index(index, cx),
        }
    }

    /// Land an index read: labels rejoin, provisional rows paint before the
    /// wire answers, and the search index syncs around whatever changed.
    pub(crate) fn apply_index(
        &mut self,
        index: std::sync::Arc<std::collections::HashMap<String, index::IndexEntry>>,
        cx: &mut Context<Self>,
    ) {
        crate::log::boot_mark(&format!("index-reply rows={}", index.len()));
        // Settled, even on an empty read: row visibility comes from the
        // index, and the sidebar waits for it before debuting groups.
        self.index_loaded = true;
        self.index = index;
        let at = std::time::Instant::now();
        if self.sessions_loaded {
            self.rejoin();
        } else {
            // The wire has not answered yet: paint the sidebar from
            // the index now (provisional rows, no meta line) instead
            // of holding an empty column until `session/list` lands.
            // The list reply replaces them wholesale.
            self.install_provisional_rows();
        }
        crate::log::boot_mark(&format!(
            "rejoin-done rows={} in={}ms",
            self.sessions.len(),
            at.elapsed().as_millis()
        ));
        crate::log::boot_mark(&format!(
            "provisional-rows rows={}",
            self.sessions.iter().filter(|e| e.provisional).count()
        ));
        self.rebuild_search_index(cx);
        crate::log::boot_mark("index-loaded");
        // Same reason as the list reply below: re-arm the cached
        // pane in this update, not on some later frame's `on_frame`.
        let _ = self.sync_sidebar_pane(cx);
        cx.notify();
    }

    /// `session/list`, unfiltered and paged: one window over every
    /// workspace, so the filter that used to hide other workspaces' sessions
    /// is gone and the cursor is followed until the server says `None`.
    /// Incremental against the saved copy ([`crate::list_cache`]): rows
    /// newer than its newest, merged in, with a full listing when the copy
    /// is missing or a day old. The reply below always sees the whole list.
    /// Everything lands on the one background task behind this
    /// [`crate::wire::WireCall`].
    pub(crate) fn load_sessions(&mut self, cx: &mut Context<Self>) {
        if !self.muse_slot.is_ready() {
            return;
        }
        let slot = self.muse_slot.clone();
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
        let cached = self.saved_list.clone();
        let work = move || {
            let at = std::time::Instant::now();
            crate::log::boot_mark("session/list-work-start");
            // Only rows newer than the saved copy's newest, merged into it
            // (`list_cache`): one page instead of 27 at boot and after
            // every finished turn. The first listing of a launch may run
            // before the index read has loaded the copy; it reads it here.
            let cache_path = crate::list_cache::cache_path();
            let cached = cached.or_else(|| crate::list_cache::load(&cache_path).map(std::sync::Arc::new));
            let source = crate::index::index_path().to_string_lossy().into_owned();
            let now_ms = chrono::Utc::now().timestamp_millis();
            let lane = slot.control()?;
            let listing = crate::list_cache::list(
                cached.as_deref(),
                &source,
                now_ms,
                &mut |ask| {
                    match lane.send(provider::Command::ListSessions {
                        cursor: ask.cursor,
                        limit: Some(ask.limit),
                        workspace: None,
                        updated_after: ask.updated_after.map(str::to_owned),
                    })? {
                        provider::Ack::SessionIndex { sessions, next_cursor } => Ok(crate::list_cache::Page {
                            rows: sessions.into_iter().map(crate::list_cache::ListedRow::from).collect(),
                            next: next_cursor,
                        }),
                        other => Err(provider::ProviderError::Rejected {
                            reason: format!("a session listing answered {other:?}"),
                        }),
                    }
                },
                &mut |page, rows, took| {
                    crate::log::boot_mark(&format!(
                        "session/list-page pages={page} rows={rows} in={}ms",
                        took.as_millis()
                    ));
                },
            )?;
            crate::log::boot_mark(&format!(
                "session/list-work-done rows={} pages={} mode={:?} fetched={} in={}ms",
                listing.sessions.len(),
                listing.pages,
                listing.mode,
                listing.fetched,
                at.elapsed().as_millis()
            ));
            let held = match listing.save {
                Some(copy) => {
                    crate::list_cache::save(&cache_path, &copy);
                    Some(std::sync::Arc::new(copy))
                }
                None => cached,
            };
            let sessions = listing.sessions;
            // The branch behind every project group row, read while already
            // off the UI thread — and the root-exists answers beside them,
            // so the refresh rechecks every adoption without touching the
            // disk on the UI thread.
            let mut branches = HashMap::new();
            for project in &projects.projects {
                if let Some(branch) = crate::projects::branch_of(&project.root) {
                    branches.insert(project.id.clone(), branch);
                }
            }
            let availability = projects.measure_availability();
            // Every root the join below will resolve, canonicalized here
            // rather than on the UI thread: ~2,000 distinct roots made the
            // join itself 520–830 ms of frozen window per reply.
            let mut canon = crate::projects::CanonicalCache::default();
            for root in sessions.iter().filter_map(|s| s.workspace_root.as_deref()) {
                canon.get(root);
            }
            for project in &projects.projects {
                canon.get(&project.root.to_string_lossy());
            }
            Ok::<_, provider::ProviderError>((sessions, branches, availability, canon, held))
        };
        self.wire_call_in(cx, work, |this, result, window, cx| {
            let reply_at = std::time::Instant::now();
            let row_count = match &result {
                Ok((sessions, _, _, _, _)) => sessions.len(),
                Err(_) => 0,
            };
            crate::log::boot_mark(&format!("session/list-reply rows={row_count}"));
            // A failed listing still settles the first load (the sidebar
            // shows what it has), but never silently (docs/81 A F25).
            if let Err(error) = &result {
                crate::baaz_log!("session/list failed: {error}");
            }
            this.sessions_list_in_flight = false;
            // The first reply replaces the provisional rows wholesale (see
            // `install_provisional_rows`); a later reply whose rows match
            // what is already shown changes nothing and must cost nothing —
            // no rejoin, no regroup, no title reads, no search rebuild.
            let already = this.sessions_loaded;
            this.sessions_loaded = true;
            crate::log::boot_mark("sessions-loaded");
            if let Ok((sessions, branches, availability, mut canon, held)) = result {
                if held.is_some() {
                    this.saved_list = held;
                }
                this.projects.refresh_availability(&availability);
                let projects = this.projects.clone();
                let join_at = std::time::Instant::now();
                // One cache over the whole loop, filled off the UI thread:
                // each distinct workspace root was canonicalized once, in
                // the work above.
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
                // Storage keeps one row per session, chain members included:
                // the chain collapses at view time (`visible_sessions`), so
                // the unchanged check compares full member rows (Y2a).
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
                    // The wire's projection may say idle while a live view
                    // still holds its turn: lay the live truth back over
                    // every row with a view, so no busy row reads Settled.
                    this.sync_all_live_rows(cx);
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
            let _ = this.sync_sidebar_pane(cx);
            // The rows' states may have moved with the list (the first
            // reply is the first look): notifications and the Dock badge.
            this.check_attention(cx);
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
    pub(in crate::app) fn apply_sidebar_fixture(&mut self) {
        let Some(path) = self.args.sidebar_fixture.clone() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(rows) = serde_json::from_str::<Vec<FixtureSession>>(&text) else { return };
        let launch = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        for row in &rows {
            let entry = fixture_entry(row, &launch, &self.projects);
            self.sessions.retain(|e| e.id != entry.id);
            self.sessions.push(entry);
        }
        self.collapse_handoff_chains();
        self.invalidate_list();
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
        // Only local rows survive the swap below (`retain(local)`), so only
        // they keep their seat. Skipping every id already shown dropped the
        // whole sidebar on a second install — the index reconcile landing
        // before the list reply — because the first install's rows were
        // skipped here and then removed by the retain.
        let present: std::collections::HashSet<String> =
            self.sessions.iter().filter(|e| e.local).map(|e| e.id.clone()).collect();
        // The saved `session/list` first ([`crate::list_cache`]): each row
        // joins exactly as the list reply will join it, so its label — and
        // the search row built from it — does not change when the reply
        // lands. Before this, ~60 rows whose wire title differs from the
        // index label rewrote their search row on every launch. Only what
        // may have moved since the copy was saved stays at its zero value,
        // as for the index rows below: idle, no attention, no title read.
        let mut joined: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(listed) = self.saved_list.clone() {
            for session in &listed.sessions {
                if present.contains(&session.session_id) {
                    continue;
                }
                let mut entry = sidebar::SessionEntry::join_cached(
                    session,
                    self.index.get(&session.session_id),
                    self.overrides.get(&session.session_id),
                    &projects,
                    &mut canon,
                );
                entry.provisional = true;
                entry.running = false;
                entry.attention.clear();
                entry.needs_title = false;
                Self::apply_title_flags(&pending, &sides, &mut entry);
                joined.insert(entry.id.clone());
                rows.push(entry);
            }
        }
        for (session_id, index) in self.index.iter() {
            if present.contains(session_id) || joined.contains(session_id) {
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
        // A row its provider lists (Muse's) is the list's, labelled by the
        // list ladder in `rejoin`: the record only moves its counters — the
        // settled turn and its time — in place of the relist a settle used
        // to cost (2,661 rows joined, 64–144 ms).
        let before = self.sessions[ix].clone();
        if self.sessions[ix].provider.lists_sessions() {
            let entry = &mut self.sessions[ix];
            let updated = chrono::DateTime::from_timestamp_millis(record.updated_ms)
                .map(|at| at.with_timezone(&chrono::Local));
            let mut changed = false;
            if record.turns > entry.turns {
                entry.turns = record.turns;
                entry.turns_unknown = false;
                changed = true;
            }
            if let Some(updated) = updated.filter(|at| *at > entry.updated) {
                entry.updated = updated;
                changed = true;
            }
            if changed {
                self.invalidate_row(&before);
            }
            return;
        }
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
            self.invalidate_row(&before);
        }
        // A settled turn on a linked session re-derives its chain's head:
        // the one-row refresh above rebuilt member facts, and the view
        // model re-sums the chain (title, turns, flags) from them. An
        // unlinked settle stops at the O(1) refresh above (X3d).
        if sidebar::needs_collapse(session_id, &self.provider_sessions, &self.overrides) {
            self.collapse_handoff_chains();
        }
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed) or after an override changed.
    ///
    /// The same precedence [`SessionEntry::join`] documents, in one place.
    pub(crate) fn rejoin(&mut self) {
        self.rejoin_scoped(None);
    }

    /// [`Self::rejoin`] over the rows `only` names (`None`: every row).
    /// A scoped rejoin invalidates the visible list only when a row it
    /// touched actually changed, and resolves chains only when one of its
    /// rows is linked (speed S4: a finished turn rejoins its own row).
    pub(crate) fn rejoin_scoped(&mut self, only: Option<&std::collections::HashSet<String>>) {
        let wanted = |id: &str| only.is_none_or(|ids| ids.contains(id));
        let before: Vec<sidebar::SessionEntry> = match only {
            None => {
                self.invalidate_list();
                Vec::new()
            }
            Some(_) => self.sessions.iter().filter(|entry| wanted(&entry.id)).cloned().collect(),
        };
        // Provider-lane rows rejoin from their record, not from the wire
        // or the index: the generic ladder below knows neither.
        let provider_ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| !entry.provider.lists_sessions() && wanted(&entry.id))
            .map(|entry| entry.id.clone())
            .collect();
        for id in provider_ids {
            self.rejoin_provider_row(&id);
        }
        // One canonicalize per distinct root for the whole pass, not one
        // per row per project: this runs on the UI thread on every index
        // load, list reply and override edit (perf audit 2026-10-16).
        let mut canon = crate::projects::CanonicalCache::default();
        // The list rows by id, for the label ladder the list join reads
        // ([`sidebar::label_ladder`]): without them a rejoin fell back to the
        // index label, uncut, and the next list reply flipped it back.
        let listed = self.saved_list.clone();
        let wire: HashMap<&str, &crate::list_cache::ListedRow> = listed
            .as_deref()
            .map(|l| {
                l.sessions.iter().filter(|s| wanted(&s.session_id)).map(|s| (s.session_id.as_str(), s)).collect()
            })
            .unwrap_or_default();
        for entry in &mut self.sessions {
            if !entry.provider.lists_sessions() || !wanted(&entry.id) {
                continue;
            }
            // The adoption may have changed under the row: re-resolve every
            // entry, including local and replayed ones, whose labels keep
            // their own rules below.
            let meta = self.overrides.get(&entry.id);
            // A missing root resolves nowhere: the row falls back to "Other
            // workspaces" while the adoption stays in the store.
            let resolved = self.projects.resolve_available_cached(
                entry.workspace.as_deref(),
                meta.and_then(|m| m.project.as_deref()),
                &mut canon,
            );
            entry.project = resolved.as_ref().map(|p| p.id.clone());
            entry.project_name = resolved.as_ref().map(|p| p.name.clone());
            if entry.local {
                // A local row predates the wire list: no index or store
                // source speaks for it, so its label ("New session", or the
                // first prompt set on `turn/started`) stands until the wire
                // lists it. Only an explicit rename or a generated title
                // overrides it — the two store sources ranked above the
                // prompt, so the row and the header that falls back to it
                // read the title the moment it lands; the flags still
                // follow the store, so pin and hide work meanwhile.
                let meta = self.overrides.get(&entry.id);
                entry.hidden = meta.is_some_and(|m| m.hidden);
                entry.pinned = meta.is_some_and(|m| m.pinned);
                entry.archived = meta.is_some_and(|m| m.archived);
                let pick = |value: Option<&str>| value.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                if let Some(name) = pick(meta.and_then(|m| m.name.as_deref())) {
                    entry.label = name;
                    entry.named = true;
                } else if let Some(generated) = pick(meta.and_then(|m| m.generated_title.as_deref())) {
                    entry.label = generated;
                }
                continue;
            }
            let meta = self.overrides.get(&entry.id);
            let index = self.index.get(&entry.id);
            let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let label = sidebar::label_ladder(meta, wire.get(entry.id.as_str()).copied(), index);
            // Same rule as [`SessionEntry::join`]: a user-given name always
            // earns the first prompt below it, any other label only when it
            // does not already say it.
            let user_named = name.is_some();
            let text = label.unwrap_or(crate::sidebar::UNNAMED);
            entry.needs_title = label.is_none();
            // A replayed capture names its own row by file, and no source
            // speaks for it: keep that label rather than blanking it to the
            // fallback on every override write.
            match label {
                Some(label) => entry.label = sidebar::one_line(label),
                None if !entry.replayed => entry.label = crate::sidebar::UNNAMED.to_owned(),
                None => {}
            }
            // The wire-alone mark survives the rejoin: a side started by any
            // other state dir carries no override here, so its own row flag
            // is what keeps it hidden.
            // The index may land after the row joined: a session another
            // client made is marked here too (`made_elsewhere`).
            let root = wire.get(entry.id.as_str()).and_then(|w| w.workspace_root.as_deref());
            entry.side_marker = entry.side_marker || sidebar::made_elsewhere(index, root, meta);
            entry.hidden = meta.is_some_and(|m| m.hidden) || entry.side_marker;
            entry.pinned = meta.is_some_and(|m| m.pinned);
            entry.archived = meta.is_some_and(|m| m.archived);
            // A generation in flight still reads pending after the rejoin,
            // and a side session still reads hidden — even before its
            // override write lands (the persisted `hidden` flag carries a
            // restart).
            Self::apply_title_flags(&self.titles_pending, &self.side_sessions, entry);
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
                entry.unread = meta.is_some_and(|m| m.unread);
            }
            entry.named = name.is_some();
        }
        // A handoff chain is one row at view time: member rows stay in
        // storage, so a rejoin never resurfaces one — but the derived
        // head still re-reads, so invalidate when a link is in play.
        // Link-free rejoins keep the O(1) row refresh (X3d).
        let touched_link = self
            .sessions
            .iter()
            .filter(|entry| wanted(&entry.id))
            .any(|entry| sidebar::needs_collapse(&entry.id, &self.provider_sessions, &self.overrides));
        if touched_link {
            self.collapse_handoff_chains();
        }
        if only.is_some() {
            if touched_link {
                self.invalidate_list();
            } else {
                for row in &before {
                    self.invalidate_row(row);
                }
            }
        }
    }

    /// The handoff chains over full storage, built once per storage
    /// change and shared by every lookup on this frame. The build is keyed
    /// on `list_epoch`: each rebuild, rejoin or override edit invalidates
    /// the list, which retires the index with it — so repeated frames and
    /// repeated rows never rebuild (Y2a3).
    /// [`Self::chain_index`] borrowed rather than cloned: a reader that
    /// asks about a few ids pays no copy of every row's head (speed S4).
    pub(crate) fn with_chain_index<R>(&self, f: impl FnOnce(&sidebar::ChainIndex) -> R) -> R {
        {
            let cache = self.chain_index.borrow();
            if let Some((epoch, index)) = cache.as_ref() {
                if *epoch == self.list_epoch {
                    return f(index);
                }
            }
        }
        let _ = self.chain_index();
        let cache = self.chain_index.borrow();
        f(&cache.as_ref().expect("built just above").1)
    }

    pub(crate) fn chain_index(&self) -> sidebar::ChainIndex {
        let mut cache = self.chain_index.borrow_mut();
        if let Some((epoch, index)) = cache.as_ref() {
            if *epoch == self.list_epoch {
                return index.clone();
            }
        }
        let index = sidebar::ChainIndex::build(&self.provider_sessions, &self.overrides, &self.sessions);
        self.chain_index_builds.set(self.chain_index_builds.get() + 1);
        cache.replace((self.list_epoch, index.clone()));
        index
    }

    /// Follow `handoff_to` links across both stores to the chain head
    /// (`docs/22-handoff.md` §8): any member id opens the head instead. A
    /// dangling destination resolves to the id itself, so its source stays
    /// listed and openable. Reads the cached per-change index, never a
    /// fresh walk (Y2a3).
    pub(crate) fn chain_head(&self, session_id: &str) -> String {
        self.chain_index().head(session_id)
    }

    /// Every member of `session_id`'s chain, sorted: what archiving the head
    /// covers so no member resurfaces. Reads the cached per-change index,
    /// never a fresh walk (Y2a3).
    pub(crate) fn chain_members(&self, session_id: &str) -> Vec<String> {
        self.chain_index().members(session_id)
    }

    /// Storage keeps every chain member row; the one-row-per-chain view
    /// is derived in [`Self::visible_sessions`](crate::app::Harness::visible_sessions)
    /// (see [`sidebar::collapse_handoff_chains`]). Kept as a named step so
    /// call sites read the same — it only invalidates when a link is in
    /// play, never removes rows. Returns whether the list changed.
    pub(crate) fn collapse_handoff_chains(&mut self) -> bool {
        let touched = self
            .sessions
            .iter()
            .any(|entry| sidebar::needs_collapse(&entry.id, &self.provider_sessions, &self.overrides));
        if touched {
            self.invalidate_list();
        }
        touched
    }

    /// Rebuild the sidebar rows the provider record owns: one row per
    /// stored session, joined with the overrides, preserving each row's
    /// live facts (a running turn, pending approvals) across the rebuild.
    /// Rows whose record is gone (deleted) leave with it. Chain members
    /// stay as rows; the one-row-per-chain view derives in
    /// `visible_sessions` (Y2a). Returns whether the list changed.
    pub(crate) fn merge_provider_rows(&mut self) -> bool {
        // A deleted record that anchored a chain still needs the collapse:
        // its row leaves, the chain re-sums without it.
        let removed_linked = self.sessions.iter().any(|entry| {
            !entry.provider.lists_sessions()
                && !self.provider_sessions.contains_key(&entry.id)
                && sidebar::needs_collapse(&entry.id, &self.provider_sessions, &self.overrides)
        });
        self.sessions.retain(|entry| entry.provider.lists_sessions() || self.provider_sessions.contains_key(&entry.id));
        if self.provider_sessions.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut touched_link = removed_linked;
        // A provider that lists its own sessions (Muse) owns their rows:
        // its records route reopens and hold Baaz's facts, never a row.
        let mut records: Vec<crate::provider_sessions::ProviderSessionRecord> = self
            .provider_sessions
            .values()
            .filter(|record| !ProviderId::parse_known(&record.provider).is_some_and(|id| id.lists_sessions()))
            .cloned()
            .collect();
        records.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        for record in &records {
            touched_link |= sidebar::needs_collapse(&record.session_id, &self.provider_sessions, &self.overrides);
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
        // Restart before the wire: a muse-lane destination exists only as
        // an override yet — synthesise its row from the local stores so
        // the chain shows with the right title before `session/list`
        // arrives (Y2a). Named by the chain title, so the empty filter
        // keeps it. Only an id whose own override carries the destination
        // half (`handoff_from`) is one: an id merely *named* by another
        // session's link is a Claude Code / Codex member whose record is
        // its row — with the record missing (a state dir copied without
        // `provider-sessions.json`) it was drawn as a "New session · now"
        // ghost once any record existed (live check of 7ff7f92).
        {
            let missing: Vec<String> = self
                .overrides
                .iter()
                .filter(|(id, meta)| {
                    meta.handoff_from.as_deref().map(str::trim).is_some_and(|from| !from.is_empty())
                        && !self.sessions.iter().any(|e| e.id == **id)
                        && sidebar::needs_collapse(id, &self.provider_sessions, &self.overrides)
                })
                .map(|(id, _)| id.clone())
                .collect();
            if !missing.is_empty() {
                let now = crate::clock::now_local();
                for id in missing {
                    let meta = self.overrides.get(&id);
                    let title = meta
                        .and_then(|m| m.handoff_title.clone())
                        .or_else(|| meta.and_then(|m| m.name.clone()))
                        .unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
                    let named = meta.and_then(|m| m.handoff_title.clone()).is_some();
                    self.sessions.push(sidebar::SessionEntry {
                        id: id.clone(),
                        label: sidebar::one_line(&title),
                        updated: now,
                        created: now,
                        running: false,
                        turns: 0,
                        hidden: meta.is_some_and(|m| m.hidden),
                        pinned: meta.is_some_and(|m| m.pinned),
                        archived: meta.is_some_and(|m| m.archived),
                        description: String::new(),
                        replayed: false,
                        provider: ProviderId::Muse,
                        named,
                        needs_title: false,
                        side_marker: false,
                        title_pending: false,
                        last_ask: None,
                        local: false,
                        provisional: false,
                        turns_unknown: false,
                        workspace: None,
                        project: meta.and_then(|m| m.project.clone()),
                        project_name: None,
                        attention: Vec::new(),
                        approval_command: None,
                        pending_question: None,
                        turn_started: None,
                        last_error: None,
                        unread: false,
                        branch: None,
                        terminals_running: 0,
                    });
                    changed = true;
                }
            }
        }
        if changed {
            self.invalidate_list();
        }
        // A handoff chain is one row at view time: invalidate so the
        // derived head re-reads the rebuilt rows — but only when a merged
        // or removed record is link-adjacent, so link-free merges stop at
        // the row refresh (X3d).
        let collapsed = if touched_link { self.collapse_handoff_chains() } else { false };
        changed | collapsed
    }

    /// The session's project display name, for the empty state: the row's
    /// project resolved to a name, so a rename shows without reopening.
    pub(crate) fn project_name_for(&self, session_id: &str) -> Option<String> {
        // A provider draft still opening has no row yet: the drafts map
        // names its project.
        self.sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.as_deref())
            .or_else(|| self.open_state.draft_project(session_id))
            .and_then(|id| self.projects.find(id))
            .map(|p| p.name.clone())
    }
}
