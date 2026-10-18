//! Full-text search over this workspace's sessions and the files they
//! created: opening the palette, re-querying off the UI thread, and the rows
//! the palette draws.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

#[cfg(test)]
static SEARCH_REBUILDS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

impl Harness {
    /// Open the full-text search palette and put the keyboard in its query.
    ///
    /// Reached from the sidebar search icon, Cmd+Shift+F and `/search`; the
    /// empty query lists recent sessions and recently created files, so the
    /// sidebar's quick-filter is still one keypress away.
    pub(crate) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = Some(Palette { kind: PaletteKind::Search, selected: 0 });
        });
        self.search_query.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.search_sessions.clear();
        self.search_files.clear();
        self.refresh_search(cx);
        window.focus(&self.search_query.focus_handle(cx), cx);
        cx.notify();
    }

    /// The view menu's "Search all projects": flip the scope the next query
    /// reads, persist it, and re-run an open palette.
    pub(crate) fn toggle_search_scope(&mut self, cx: &mut Context<Self>) {
        self.layout.search_all_projects = !self.layout.search_all_projects;
        layout::write(&self.layout);
        self.refresh_search(cx);
        cx.notify();
    }

    /// Re-query `search.db` off the UI thread, latest keystroke wins.
    ///
    /// A no-op unless the search palette is open: typing anywhere else must
    /// not touch the disk. When the Sessions view menu narrowed search to the
    /// current project, hits from every other workspace stay out.
    pub(super) fn refresh_search(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search) {
            return;
        }
        self.search_epoch += 1;
        let epoch = self.search_epoch;
        let query = self.search_query.read(cx).value().to_string();
        let scope: Option<String> = if self.layout.search_all_projects {
            None
        } else {
            self.current_project().map(|p| p.root.to_string_lossy().into_owned())
        };
        let work = move || match crate::search::open() {
            Ok(connection) => {
                let sessions: Vec<crate::search::SessionHit> =
                    crate::search::query_sessions(&connection, &query, crate::search::LIMIT)
                        .into_iter()
                        .filter(|hit| crate::search::matches_scope(hit.workspace.as_deref(), scope.as_deref()))
                        .collect();
                let files: Vec<crate::search::FileHit> =
                    crate::search::query_files(&connection, &query, crate::search::LIMIT)
                        .into_iter()
                        .filter(|hit| crate::search::matches_scope(hit.workspace.as_deref(), scope.as_deref()))
                        .collect();
                crate::log::boot_mark(&format!(
                    "search q={query:?} scope={scope:?} sessions={} files={}",
                    sessions.len(),
                    files.len()
                ));
                (sessions, files)
            }
            Err(error) => {
                crate::log::boot_mark(&format!("search db open failed: {error}"));
                (Vec::new(), Vec::new())
            }
        };
        self.wire_call(cx, work, move |this: &mut Self, (sessions, files), cx| {
            if this.search_epoch != epoch {
                return;
            }
            this.search_sessions = sessions;
            this.search_files = files;
            // The selection may point past the new list.
            this.overlays.update(cx, |overlays, _| {
                if let Some(palette) = overlays.palette.as_mut() {
                    palette.selected = 0;
                }
            });
            cx.notify();
        });
    }

    /// The session half's rows, one per handoff chain: the label and
    /// project terms the palette shows, every member's transcript text as
    /// the matchable body. Pure over the harness state so tests can pin
    /// the rows without a window; [`Self::rebuild_search_index`] writes
    /// them to `search.db` (`docs/22-handoff.md` §8.4).
    #[cfg(test)]
    pub(crate) fn search_session_rows(&self) -> Vec<crate::search::SessionRow> {
        self.search_sync_rows().into_iter().map(|(row, _, _)| row).collect()
    }

    /// [`Self::search_session_rows`] with what [`crate::search::sync_sessions`]
    /// needs beside each row: the index ids whose transcript text is the
    /// body (fetched only when the row is written), and a fingerprint of
    /// everything that decides what is written — the row's own words and
    /// each body source's `updated_at_us`. No body text is read or cloned
    /// here: this runs on the UI thread.
    pub(crate) fn search_sync_rows(&self) -> Vec<(crate::search::SessionRow, Vec<String>, i64)> {
        use std::collections::HashMap;
        // One cache over the whole build: each distinct index root is
        // canonicalized once, not once per row.
        let mut canon = crate::projects::CanonicalCache::default();
        // `is_side_session` scans the session list for a side marker; asked
        // per indexed session that was O(n²) — ~7 M id compares and ~30 ms
        // of UI thread per rebuild on 2,600 sessions (perf P1). The marked
        // ids are collected once instead.
        let marked: std::collections::HashSet<&str> =
            self.sessions.iter().filter(|entry| entry.side_marker).map(|entry| entry.id.as_str()).collect();
        let is_side = |id: &str| {
            self.side_sessions.contains(id)
                || self.overrides.get(id).is_some_and(|meta| meta.side_session)
                || marked.contains(id)
        };
        let mut per_session: HashMap<String, crate::search::SessionRow> = HashMap::new();
        for (session_id, entry) in self.index.iter() {
            // Title side sessions never reach the search palette: their
            // only transcript is the title prompt itself.
            if is_side(session_id) {
                continue;
            }
            let meta = self.overrides.get(session_id);
            let name =
                meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let derived = meta
                .and_then(|m| m.derived_title.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let label =
                name.or_else(|| entry.label()).or(derived).unwrap_or(crate::sidebar::UNNAMED);
            let workspace = entry.workspace_root.as_deref().map(|root| canon.get(root));
            // The adopted name too, when this root has one: a renamed
            // project is findable by the name on screen as well as by
            // the folder it still lives in.
            // `workspace` is canonical already; the project roots go
            // through the same cache, so the build reads each from disk
            // once rather than once per indexed session.
            let adopted = workspace
                .as_deref()
                .and_then(|root| self.projects.find_by_canonical_root(root, &mut canon))
                .map(|p| p.name.clone());
            per_session.insert(
                session_id.clone(),
                crate::search::SessionRow {
                    session_id: session_id.clone(),
                    label: label.to_owned(),
                    title: entry.title.clone(),
                    first_prompt: entry.first_user_prompt.clone().unwrap_or_default(),
                    body: entry.search_text.clone(),
                    project: crate::search::project_terms(workspace.as_deref(), adopted.as_deref()),
                    workspace,
                },
            );
        }
        // Claude Code and Codex sessions (docs/50 P3): Baaz's own registry
        // names them, and their words come from their stored transcripts
        // ([`crate::provider_search`]) when the row is written. A session
        // both stores know (never expected) keeps its index row.
        for (session_id, record) in &self.provider_sessions {
            if per_session.contains_key(session_id) || is_side(session_id) {
                continue;
            }
            let meta = self.overrides.get(session_id);
            let trimmed = |text: Option<&String>| text.map(|t| t.trim()).filter(|t| !t.is_empty()).map(str::to_owned);
            let title = trimmed(record.title.as_ref()).unwrap_or_default();
            let first_prompt = trimmed(record.first_prompt.as_ref()).unwrap_or_default();
            let label = trimmed(meta.and_then(|m| m.name.as_ref()))
                .or_else(|| trimmed(meta.and_then(|m| m.derived_title.as_ref())))
                .or_else(|| trimmed(record.handoff_title.as_ref()))
                .or_else(|| (!title.is_empty()).then(|| title.clone()))
                .or_else(|| (!first_prompt.is_empty()).then(|| first_prompt.clone()))
                .unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
            let workspace = record.workspace.as_deref().map(|root| canon.get(root));
            let adopted = workspace
                .as_deref()
                .and_then(|root| self.projects.find_by_canonical_root(root, &mut canon))
                .map(|p| p.name.clone());
            per_session.insert(
                session_id.clone(),
                crate::search::SessionRow {
                    session_id: session_id.clone(),
                    label,
                    title,
                    first_prompt,
                    body: String::new(),
                    project: crate::search::project_terms(workspace.as_deref(), adopted.as_deref()),
                    workspace,
                },
            );
        }
        // One row per chain: indexed members share the head's row, so a
        // three-member chain is one palette hit under the head id. The
        // head keeps its own label, title and first prompt; every other
        // member's words join the body, so a match in an early member's
        // turns finds the head. Opening already redirects to the head, so
        // nothing changes there.
        // One cached index build serves every head lookup below, never
        // a walk per indexed session (Y2a3).
        let chain_index = self.chain_index();
        // The sidebar's labels by id: one pass, not a scan of the list per
        // head (2,600 heads × 2,600 rows).
        let labels: HashMap<&str, &String> =
            self.sessions.iter().map(|entry| (entry.id.as_str(), &entry.label)).collect();
        let mut by_head: HashMap<String, Vec<String>> = HashMap::new();
        for id in per_session.keys() {
            let head = chain_index.head(id);
            by_head.entry(head).or_default().push(id.clone());
        }
        let mut heads: Vec<String> = by_head.keys().cloned().collect();
        heads.sort();
        let mut out = Vec::with_capacity(heads.len());
        for head in heads {
            let mut members = by_head.remove(&head).unwrap_or_default();
            members.sort();
            if is_side(&head) {
                // A chained side session stays unfindable, but its members
                // keep their own rows rather than vanishing with it.
                for member in members {
                    if let Some(row) = per_session.remove(&member) {
                        let body_ids = vec![member.clone()];
                        let fingerprint = self.search_fingerprint(&row, &body_ids);
                        out.push((row, body_ids, fingerprint));
                    }
                }
                continue;
            }
            // Every non-head member's words: label, title, first prompt
            // and transcript text alike.
            let mut extra: Vec<String> = Vec::new();
            for member in &members {
                if member == &head {
                    continue;
                }
                if let Some(row) = per_session.get(member) {
                    for text in [&row.label, &row.title, &row.first_prompt, &row.body] {
                        let text = text.trim();
                        if !text.is_empty() {
                            extra.push(text.to_owned());
                        }
                    }
                }
            }
            // The transcript sources, head first when it has a row, then
            // every other member in id order.
            let mut body_ids: Vec<String> = Vec::new();
            if per_session.contains_key(&head) {
                body_ids.push(head.clone());
            }
            for member in &members {
                if member != &head {
                    body_ids.push(member.clone());
                }
            }
            let mut base = match per_session.remove(&head) {
                Some(row) => row,
                // A head neither store knows (its record is gone):
                // scaffold its row from the first member, renamed to the
                // head (its words already joined `extra` above).
                None => {
                    let first = members[0].clone();
                    let mut row =
                        per_session.remove(&first).expect("chain groups derive from rowed ids");
                    row.session_id.clone_from(&head);
                    row
                }
            };
            // The chain's display title: the collapsed sidebar row when
            // the list has one, else the head's own index label (boot
            // orders vary).
            if let Some(label) = labels.get(head.as_str()) {
                base.label.clone_from(label);
            }
            if !extra.is_empty() {
                base.body = if base.body.is_empty() {
                    extra.join("\n")
                } else {
                    format!("{}\n{}", base.body, extra.join("\n"))
                };
            }
            let fingerprint = self.search_fingerprint(&base, &body_ids);
            out.push((base, body_ids, fingerprint));
        }
        out
    }

    /// A stable hash of what a search row would write: its words and each
    /// body source's last activity. Never 0, which means "always write".
    fn search_fingerprint(&self, row: &crate::search::SessionRow, body_ids: &[String]) -> i64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (&row.session_id, &row.label, &row.title, &row.first_prompt, &row.body, &row.project, &row.workspace)
            .hash(&mut hasher);
        for id in body_ids {
            id.hash(&mut hasher);
            self.index.get(id).and_then(|entry| entry.updated_at_us).hash(&mut hasher);
            // A provider transcript grows with each settled exchange, which
            // moves its record's turn count and stamp. Hashed only when there
            // is a record: a Muse row's fingerprint stays what it was before
            // provider rows existed, so an upgrade rewrites only the new rows
            // (hashing a `None` here rewrote all 2,612 rows once, 23 s).
            if let Some(record) = self.provider_sessions.get(id) {
                (record.turns, record.updated_ms).hash(&mut hasher);
            }
        }
        match hasher.finish() as i64 {
            0 => 1,
            fingerprint => fingerprint,
        }
    }

    /// How many times [`Self::rebuild_search_index`] ran in this process.
    /// Test builds only: the B7 pane-toggle test resets it, toggles, and
    /// asserts it stays put — right-pane state is not searchable, so a
    /// toggle must never rebuild.
    #[cfg(test)]
    pub(crate) fn search_rebuild_count() -> usize {
        SEARCH_REBUILDS.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Zero [`Self::search_rebuild_count`]; see above.
    #[cfg(test)]
    pub(crate) fn reset_search_rebuild_count() {
        SEARCH_REBUILDS.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// Rebuild the session half of `search.db` off the UI thread.
    ///
    /// Runs at boot and after each index refresh; the files half is never
    /// touched here, so recorded files survive a rebuild. When the rebuild
    /// lands while the palette is open, the open query runs again against
    /// the fresh index.
    ///
    /// Never runs for right-pane state (open/kind/width, Files
    /// expand/collapse/select, browser navigations): that state is not
    /// searchable, so those paths persist through the cheap debounced write
    /// instead (see [`Self::schedule_overrides_write`]).
    pub(crate) fn rebuild_search_index(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        SEARCH_REBUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let at = std::time::Instant::now();
        let rows = self.search_sync_rows();
        crate::log::boot_mark(&format!("search-rows-built rows={} in={}ms", rows.len(), at.elapsed().as_millis()));
        let index_path = crate::index::index_path();
        let provider_of = self.search_body_owners();
        let work = move || {
            // One rebuild at a time: two overlapping ones fought over the
            // database lock (8 s and 20 s at boot), and the second now finds
            // the first's fingerprint and writes nothing.
            static REBUILD: std::sync::Mutex<()> = std::sync::Mutex::new(());
            let _turn = REBUILD.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let at = std::time::Instant::now();
            let mut connection = match crate::search::open() {
                Ok(connection) => connection,
                Err(_) => return,
            };
            let synced: Vec<crate::search::SyncRow<'_>> = rows
                .iter()
                .map(|(row, body_ids, fingerprint)| crate::search::SyncRow {
                    row,
                    body_ids,
                    fingerprint: *fingerprint,
                })
                .collect();
            let stats = crate::search::sync_sessions(&mut connection, &synced, &|ids| {
                search_bodies(&index_path, &provider_of, ids)
            });
            crate::log::boot_mark(&format!(
                "search-sync-done rows={} {stats:?} in={}ms",
                rows.len(),
                at.elapsed().as_millis()
            ));
        };
        self.wire_call(cx, work, |this: &mut Self, (), cx| {
            this.refresh_search(cx);
        });
    }

    /// Which search body ids are provider-lane sessions, and whose: their
    /// words come from their stored transcripts, never from Muse's index.
    pub(crate) fn search_body_owners(&self) -> std::collections::HashMap<String, String> {
        self.provider_sessions
            .iter()
            .filter(|(id, _)| !self.index.contains_key(*id))
            .map(|(id, record)| (id.clone(), record.provider.clone()))
            .collect()
    }

    /// The badge a search row wears: its project's display name, or the
    /// folder name when no project holds the session.
    pub(crate) fn search_badge(&self, session_id: &str) -> Option<String> {
        let entry = self.sessions.iter().find(|e| e.id == session_id)?;
        match entry.project.as_deref().and_then(|id| self.projects.find(id)) {
            Some(project) => Some(project.name.clone()),
            None => entry.workspace.as_deref().map(crate::sidebar::workspace_folder),
        }
    }

    /// The search palette's rows: session hits, then file hits, in the order
    /// the palette draws them so the keyboard and the click agree.
    ///
    /// Each id carries its section (`s:<session>` or `f:<session>:<path>`).
    /// An empty query is recent sessions from the sidebar order plus recently
    /// recorded files.
    pub(crate) fn search_rows(&self, cx: &gpui::App) -> Vec<(SharedString, SharedString, SharedString)> {
        let mut rows = Vec::new();
        if self.search_query.read(cx).value().trim().is_empty() {
            for entry in self.visible_sessions(cx).iter().take(PALETTE_ROWS) {
                rows.push((
                    format!("s:{}", entry.id).into(),
                    entry.label.clone().into(),
                    SharedString::from("recent"),
                ));
            }
        } else {
            for hit in &self.search_sessions {
                let detail =
                    if hit.snippet.is_empty() { SharedString::from("match") } else { hit.snippet.clone().into() };
                rows.push((format!("s:{}", hit.session_id).into(), hit.label.clone().into(), detail));
            }
        }
        for hit in &self.search_files {
            let label = self
                .sessions
                .iter()
                .find(|entry| entry.id == hit.session_id)
                .map(|entry| entry.label.clone())
                .unwrap_or_else(|| "created file".to_owned());
            rows.push((format!("f:{}:{}", hit.session_id, hit.path).into(), hit.path.clone().into(), label.into()));
        }
        rows
    }
}

/// The transcript words for `ids`, whichever provider holds each: Muse's
/// from its index's `search_text`, Claude Code's and Codex's from their
/// stored transcripts (`provider_of`, from [`Harness::search_body_owners`]).
/// Blocking; the search sync calls it off the UI thread, for changed rows
/// only.
pub(crate) fn search_bodies(
    index_path: &std::path::Path,
    provider_of: &std::collections::HashMap<String, String>,
    ids: &[String],
) -> std::collections::HashMap<String, String> {
    let (lanes, muse): (Vec<&String>, Vec<&String>) = ids.iter().partition(|id| provider_of.contains_key(*id));
    let muse: Vec<String> = muse.into_iter().cloned().collect();
    let mut bodies = crate::index::read_bodies(index_path, &muse);
    let sources: Vec<crate::provider_search::BodySource> = lanes
        .into_iter()
        .map(|id| crate::provider_search::BodySource { session_id: id.clone(), provider: provider_of[id].clone() })
        .collect();
    bodies.extend(crate::provider_search::read_bodies(&sources));
    bodies
}
