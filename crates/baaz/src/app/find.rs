//! Full-text search over this workspace's sessions and the files they
//! created: opening the palette, re-querying off the UI thread, and the rows
//! the palette draws.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

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

    /// The Sessions view menu's "Search all projects": flip the scope the
    /// next query reads, persist it, and re-run an open palette.
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
    pub(crate) fn search_session_rows(&self) -> Vec<crate::search::SessionRow> {
        use std::collections::HashMap;
        // One cache over the whole build: each distinct index root is
        // canonicalized once, not once per row.
        let mut canon = crate::projects::CanonicalCache::default();
        let mut per_session: HashMap<String, crate::search::SessionRow> = HashMap::new();
        for (session_id, entry) in self.index.iter() {
            // Title side sessions never reach the search palette: their
            // only transcript is the title prompt itself.
            if self.is_side_session(session_id) {
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
            let adopted = workspace
                .as_deref()
                .and_then(|root| self.projects.find_by_root(std::path::Path::new(root)))
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
        // One row per chain: indexed members share the head's row, so a
        // three-member chain is one palette hit under the head id. The
        // head keeps its own label, title and first prompt; every other
        // member's words join the body, so a match in an early member's
        // turns finds the head. Opening already redirects to the head, so
        // nothing changes there.
        let mut by_head: HashMap<String, Vec<String>> = HashMap::new();
        for id in per_session.keys() {
            let head = crate::sidebar::chain_head(id, &self.provider_sessions, &self.overrides, &self.sessions);
            by_head.entry(head).or_default().push(id.clone());
        }
        let mut heads: Vec<String> = by_head.keys().cloned().collect();
        heads.sort();
        let mut out = Vec::with_capacity(heads.len());
        for head in heads {
            let mut members = by_head.remove(&head).unwrap_or_default();
            members.sort();
            if self.is_side_session(&head) {
                // A chained side session stays unfindable, but its members
                // keep their own rows rather than vanishing with it.
                for member in members {
                    if let Some(row) = per_session.remove(&member) {
                        out.push(row);
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
            // Provider-lane members carry no index entry: their ack title
            // and first prompt still name the chain.
            for member in
                crate::sidebar::chain_members(&head, &self.provider_sessions, &self.overrides, &self.sessions)
            {
                if member == head || per_session.contains_key(&member) || self.is_side_session(&member) {
                    continue;
                }
                if let Some(record) = self.provider_sessions.get(&member) {
                    for text in [&record.title, &record.first_prompt].into_iter().flatten() {
                        let text = text.trim();
                        if !text.is_empty() {
                            extra.push(text.to_owned());
                        }
                    }
                }
            }
            let mut base = match per_session.remove(&head) {
                Some(row) => row,
                // A provider-lane head carries no index entry: scaffold
                // its row from the first indexed member, renamed to the
                // head (its words already joined `extra` above).
                None => {
                    let first = members[0].clone();
                    let mut row =
                        per_session.remove(&first).expect("chain groups derive from indexed ids");
                    row.session_id.clone_from(&head);
                    row
                }
            };
            // The chain's display title: the collapsed sidebar row when
            // the list has one, else the head's own index label (boot
            // orders vary).
            if let Some(entry) = self.sessions.iter().find(|entry| entry.id == head) {
                base.label.clone_from(&entry.label);
            }
            if !extra.is_empty() {
                base.body = format!("{}\n{}", base.body, extra.join("\n"));
            }
            out.push(base);
        }
        out
    }

    /// Rebuild the session half of `search.db` off the UI thread.
    ///
    /// Runs at boot and after each index refresh; the files half is never
    /// touched here, so recorded files survive a rebuild. When the rebuild
    /// lands while the palette is open, the open query runs again against
    /// the fresh index.
    pub(crate) fn rebuild_search_index(&mut self, cx: &mut Context<Self>) {
        let at = std::time::Instant::now();
        let rows = self.search_session_rows();
        crate::log::boot_mark(&format!(
            "search-rows-built rows={} in={}ms (per-row canonical_str N={})",
            rows.len(),
            at.elapsed().as_millis(),
            rows.len()
        ));
        let work = move || {
            let at = std::time::Instant::now();
            let mut connection = match crate::search::open() {
                Ok(connection) => connection,
                Err(_) => return,
            };
            let _ = crate::search::rebuild_sessions(&mut connection, &rows);
            crate::log::boot_mark(&format!(
                "search-rebuild-done rows={} in={}ms",
                rows.len(),
                at.elapsed().as_millis()
            ));
        };
        self.wire_call(cx, work, |this: &mut Self, (), cx| {
            this.refresh_search(cx);
        });
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
