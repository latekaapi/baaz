//! Listing sessions: reading Muse's index, the sessions store and the
//! scripted fixture, joining provider records into rows (rejoin and
//! `merge_provider_rows`), the provisional boot rows, and the handoff
//! chain index the sidebar groups by.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

#[cfg(feature = "probe")]
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
    /// The row's branch, standing in for the index's derived one: what a
    /// pull request read is matched on (docs/82 7.3, `sidebar-pr`).
    #[serde(default)]
    branch: Option<String>,
}

#[cfg(feature = "probe")]
/// [`FixtureSession::status`] without the field: settled, never running.
fn fixture_settled() -> String {
    "idle".to_owned()
}

#[cfg(feature = "probe")]
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
    if let Some(branch) = row.branch.as_deref().map(str::trim).filter(|b| !b.is_empty()) {
        entry.branch = Some(branch.to_owned());
    }
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
        let first = !self.session_list.read(cx).index_loaded;
        // A refcount, not a copy: the index is shared (speed S4).
        let previous = std::sync::Arc::clone(&self.session_list.read(cx).index);
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
                if this.session_list.read(cx).fetch.saved.is_none() {
                    this.session_list.update(cx, |list, _| list.fetch.saved = listed);
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
                if index != *this.session_list.read(cx).index && !index.is_empty() {
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
            Some(ids) if self.session_list.read(cx).sessions_loaded && self.session_list.read(cx).index_loaded => {
                self.session_list.update(cx, |list, _| list.index = index);
                if ids.is_empty() {
                    return;
                }
                crate::log::boot_mark(&format!("index-reply rows={} changed={}", self.session_list.read(cx).index.len(), ids.len()));
                self.rejoin_scoped(Some(&ids), cx);
                self.rebuild_search_index_for(&ids, cx);
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
        self.session_list.update(cx, |list, _| list.index_loaded = true);
        self.session_list.update(cx, |list, _| list.index = index);
        let at = std::time::Instant::now();
        if self.session_list.read(cx).sessions_loaded {
            self.rejoin(cx);
        } else {
            // The wire has not answered yet: paint the sidebar from
            // the index now (provisional rows, no meta line) instead
            // of holding an empty column until `session/list` lands.
            // The list reply replaces them wholesale.
            self.install_provisional_rows(cx);
        }
        crate::log::boot_mark(&format!(
            "rejoin-done rows={} in={}ms",
            self.session_list.read(cx).sessions.len(),
            at.elapsed().as_millis()
        ));
        crate::log::boot_mark(&format!(
            "provisional-rows rows={}",
            self.session_list.read(cx).sessions.iter().filter(|e| e.provisional).count()
        ));
        self.rebuild_search_index(cx);
        crate::log::boot_mark("index-loaded");
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
        if !self.providers.shared_ready() {
            return;
        }
        let slot = self.providers.clone();
        if self.session_list.read(cx).fetch.in_flight {
            // A fetch is already running — at boot the probe answer's fetch
            // is still out when the boot session's `session/start` lands and
            // refreshes the list behind it. Note the refresh and let the
            // landing reply issue the one follow-up, instead of fetching
            // twice and applying the same reply twice (rejoining every row
            // and rebuilding the grouping and the search index for nothing).
            crate::log::boot_mark("session/list-coalesced");
            self.session_list.update(cx, |list, _| list.fetch.stale = true);
            return;
        }
        self.session_list.update(cx, |list, _| list.fetch.in_flight = true);
        crate::log::boot_mark("session/list-sent");
        let projects = self.session_list.read(cx).projects.clone();
        let cached = self.session_list.read(cx).fetch.saved.clone();
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
            this.session_list.update(cx, |list, _| list.fetch.in_flight = false);
            // The first reply replaces the provisional rows wholesale (see
            // `install_provisional_rows`); a later reply whose rows match
            // what is already shown changes nothing and must cost nothing —
            // no rejoin, no regroup, no title reads, no search rebuild.
            let already = this.session_list.read(cx).sessions_loaded;
            this.session_list.update(cx, |list, _| list.sessions_loaded = true);
            crate::log::boot_mark("sessions-loaded");
            if let Ok((sessions, branches, availability, mut canon, held)) = result {
                if held.is_some() {
                    this.session_list.update(cx, |list, _| list.fetch.saved = held);
                }
                this.session_list.update(cx, |list, _| list.projects.refresh_availability(&availability));
                let projects = this.session_list.read(cx).projects.clone();
                let join_at = std::time::Instant::now();
                // One cache over the whole loop, filled off the UI thread:
                // each distinct workspace root was canonicalized once, in
                // the work above.
                let wire: Vec<SessionEntry> = sessions
                    .iter()
                    .map(|s| {
                        let mut entry = SessionEntry::join_cached(
                            s,
                            this.session_list.read(cx).index.get(&s.session_id),
                            this.session_list.read(cx).overrides.get(&s.session_id),
                            &projects,
                            &mut canon,
                        );
                        // Pending rows read pending; side sessions read
                        // hidden — even before their override writes land.
                        crate::session_list::SessionList::apply_title_flags(&this.session_list.read(cx).titles_pending, &this.session_list.read(cx).side_sessions, &mut entry);
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
                let unchanged = already && sidebar::list_apply_unchanged(&this.session_list.read(cx).sessions, &wire);
                if unchanged {
                    // The rows match, but the branch answers are still fresh
                    // data: keep them (assigning invalidates nothing, so the
                    // grouping and the search index stay untouched).
                    this.session_list.update(cx, |list, _| list.branches = branches);
                    crate::log::boot_mark(&format!(
                        "list-applied rows={} unchanged in={}ms",
                        this.session_list.read(cx).sessions.len(),
                        reply_at.elapsed().as_millis()
                    ));
                } else {
                    this.invalidate_list(cx);
                    // Local rows whose id the reply does not contain survive;
                    // a local whose id is listed is replaced by its wire row.
                    // Provisional rows are never local: the first reply drops
                    // every one of them here. Provider-lane rows survive in
                    // the merge, and the record rebuilds any the reply's
                    // wholesale replace dropped.
                    this.session_list.update(cx, |list, _| list.sessions = sidebar::merge_session_list(wire, &list.sessions));
                    this.merge_provider_rows(cx);
                    // The wire's projection may say idle while a live view
                    // still holds its turn: lay the live truth back over
                    // every row with a view, so no busy row reads Settled.
                    this.sync_all_live_rows(cx);
                    this.session_list.update(cx, |list, _| list.branches = branches);
                    this.derive_titles(cx);
                    crate::log::boot_mark(&format!(
                        "list-applied rows={} in={}ms",
                        this.session_list.read(cx).sessions.len(),
                        reply_at.elapsed().as_millis()
                    ));
                }
            }
            this.open_boot_session(window, cx);
            // The rows reached the column through the list's own event; the
            // boot session's facts go in the same update as the data: the
            // column is a cached view, and waiting for the next root render
            // to push them adds a whole frame hop — one a quiescent window
            // may not take for a long while — before the first rows paint.
            this.push_sidebar_facts(cx);
            // The rows' states may have moved with the list (the first
            // reply is the first look): notifications and the Dock badge.
            this.check_attention(cx);
            cx.notify();
            if this.session_list.read(cx).fetch.stale {
                // A refresh asked while this fetch was running: one
                // follow-up, not a dropped update.
                this.session_list.update(cx, |list, _| list.fetch.stale = false);
                this.load_sessions(cx);
            }
        });
    }

    #[cfg(feature = "probe")]
    /// `--sidebar-fixture`: merge scripted rows into the session list as if
    /// the wire had listed them. A missing or unparseable file is no rows,
    /// like an empty list reply — a capture aid must never fail a boot.
    pub(in crate::app) fn apply_sidebar_fixture(&mut self, cx: &mut gpui::App) {
        let Some(path) = self.args.sidebar_fixture.clone() else { return };
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(rows) = serde_json::from_str::<Vec<FixtureSession>>(&text) else { return };
        let launch = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        for row in &rows {
            let entry = fixture_entry(row, &launch, &self.session_list.read(cx).projects);
            self.session_list.update(cx, |list, _| list.sessions.retain(|e| e.id != entry.id));
            self.session_list.update(cx, |list, _| list.sessions.push(entry));
        }
        self.collapse_handoff_chains(cx);
        self.invalidate_list(cx);
    }

    /// Paint the sidebar from the local index while `session/list` is still
    /// in flight ([`SessionList::install_provisional_rows`]).
    pub(crate) fn install_provisional_rows(&mut self, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.install_provisional_rows(cx));
    }

    /// Refresh one provider row from its record and the overrides
    /// ([`SessionList::rejoin_provider_row`]).
    pub(crate) fn rejoin_provider_row(&mut self, session_id: &str, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.rejoin_provider_row(session_id, cx));
    }

    /// Re-label every row ([`SessionList::rejoin`]).
    pub(crate) fn rejoin(&mut self, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.rejoin(cx));
    }

    /// Re-label the rows `only` names ([`SessionList::rejoin_scoped`]).
    pub(crate) fn rejoin_scoped(&mut self, only: Option<&std::collections::HashSet<String>>, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.rejoin_scoped(only, cx));
    }





    /// The handoff chain index for this list epoch ([`crate::session_list::SessionList::with_chain_index`]).
    pub(crate) fn with_chain_index<R>(&self, cx: &gpui::App, f: impl FnOnce(&sidebar::ChainIndex) -> R) -> R {
        self.session_list.read(cx).with_chain_index(f)
    }

    /// The chain index, cloned ([`crate::session_list::SessionList::chain_index`]).
    pub(crate) fn chain_index(&self, cx: &gpui::App) -> sidebar::ChainIndex {
        self.session_list.read(cx).chain_index()
    }

    /// `session_id`'s chain head ([`crate::session_list::SessionList::chain_head`]).
    pub(crate) fn chain_head(&self, session_id: &str, cx: &gpui::App) -> String {
        self.session_list.read(cx).chain_head(session_id)
    }

    /// Every member of `session_id`'s chain ([`crate::session_list::SessionList::chain_members`]).
    pub(crate) fn chain_members(&self, session_id: &str, cx: &gpui::App) -> Vec<String> {
        self.session_list.read(cx).chain_members(session_id)
    }

    #[cfg(feature = "probe")]
    /// Re-derive the chain heads when a link is in play
    /// ([`SessionList::collapse_handoff_chains`]).
    pub(crate) fn collapse_handoff_chains(&mut self, cx: &mut gpui::App) -> bool {
        self.session_list.update(cx, |list, cx| list.collapse_handoff_chains(cx))
    }

    /// Rebuild the rows the provider records own
    /// ([`SessionList::merge_provider_rows`]).
    pub(crate) fn merge_provider_rows(&mut self, cx: &mut gpui::App) -> bool {
        self.session_list.update(cx, |list, cx| list.merge_provider_rows(cx))
    }

    /// The session's project display name, for the empty state: the row's
    /// project resolved to a name, so a rename shows without reopening.
    pub(crate) fn project_name_for(&self, session_id: &str, cx: &gpui::App) -> Option<String> {
        // A provider draft still opening has no row yet: the drafts map
        // names its project.
        self.session_list.read(cx).sessions
            .iter()
            .find(|e| e.id == session_id)
            .and_then(|e| e.project.as_deref())
            .or_else(|| self.open_state.draft_project(session_id))
            .and_then(|id| self.session_list.read(cx).projects.find(id))
            .map(|p| p.name.clone())
    }
}
