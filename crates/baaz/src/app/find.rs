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
    /// next query reads, persist it, and re-run an open palette. Package 2's
    /// palette reads the scope; the toggle lands in this package.
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
                (sessions, files)
            }
            Err(_) => (Vec::new(), Vec::new()),
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

    /// Rebuild the session half of `search.db` off the UI thread.
    ///
    /// Runs at boot and after each index refresh; the files half is never
    /// touched here, so recorded files survive a rebuild. When the rebuild
    /// lands while the palette is open, the open query runs again against
    /// the fresh index.
    pub(crate) fn rebuild_search_index(&mut self, cx: &mut Context<Self>) {
        let at = std::time::Instant::now();
        // One cache over the whole build: each distinct index root is
        // canonicalized once, not once per row.
        let mut canon = crate::projects::CanonicalCache::default();
        let rows: Vec<crate::search::SessionRow> = self
            .index
            .iter()
            // Title side sessions never reach the search palette: their
            // only transcript is the title prompt itself.
            .filter(|(session_id, _)| !self.is_side_session(session_id))
            .map(|(session_id, entry)| {
                let meta = self.overrides.get(session_id);
                let name =
                    meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
                let derived = meta
                    .and_then(|m| m.derived_title.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let label =
                    name.or_else(|| entry.label()).or(derived).unwrap_or(crate::sidebar::UNNAMED);
                crate::search::SessionRow {
                    session_id: session_id.clone(),
                    label: label.to_owned(),
                    title: entry.title.clone(),
                    first_prompt: entry.first_user_prompt.clone().unwrap_or_default(),
                    body: entry.search_text.clone(),
                    workspace: entry.workspace_root.as_deref().map(|root| canon.get(root)),
                }
            })
            .collect();
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
