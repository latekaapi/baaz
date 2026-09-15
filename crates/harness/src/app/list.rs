//! What a person can do to a row: rename, pin, hide, archive, clear the
//! empty ones, and undo any of it.
//!
//! Every one of these is a local override written to the harness's own store
//! (`docs/03-composer.md` §5); Muse's storage is read-only, so a name or a
//! hidden flag never reaches the wire.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;

/// What the sidebar and the palette read out of the session list, held from
/// one change to the next instead of rebuilt per caller per frame
/// (findings `performance-5`, `support-2`, `performance-7`).
///
/// What `visible` was built for: the list epoch plus the open session's id
/// (the empty filter never hides the open session, so a switch changes the
/// rows) plus the click's target (the grouping rescues it past the fold, so
/// a click must rebuild) plus the grouping mode, the closed set and the
/// expanded set (a toggle regroups the same rows).
type ListKey = (u64, Option<String>, Option<String>, crate::layout::GroupBy, Vec<String>, Vec<String>);

/// Validity is six keys, not a timestamp (see [`ListKey`]), and the grouping
/// carries the minute it labelled its rows against.
#[derive(Default)]
pub(crate) struct ListCache {
    /// The [`ListKey`] `visible` was built for.
    key: Option<ListKey>,
    /// The sorted visible rows for that key.
    visible: Rc<Vec<SessionEntry>>,
    /// The grouping of those rows, and the minute its elapsed tags read.
    grouping: Option<(i64, Rc<sidebar::Grouping>)>,
}

impl Harness {
    // ---------------------------------------------- session operations (A2)

    /// Change one session's override and write the store.
    ///
    /// The write is synchronous, and deliberately: it is a few hundred bytes,
    /// it happens on a gesture rather than in a loop, and a background write
    /// can lose a rename to a window that closed a moment later — which is the
    /// one outcome a store exists to prevent.
    pub(crate) fn set_override(&mut self, session_id: &str, edit: impl FnOnce(&mut SessionMeta), cx: &mut Context<Self>) {
        edit(self.overrides.entry(session_id.to_owned()).or_default());
        self.settle_overrides(cx);
    }

    /// The same edit applied to a batch of sessions, settled once.
    ///
    /// Hiding forty empty sessions one at a time rejoined the whole list,
    /// rewrote `sessions.json` and rebuilt the search index forty times over
    /// (finding `support-7`); the visible result was identical and the work
    /// was quadratic in the batch. The edit still runs per row, because that
    /// is what an override is; everything downstream of it runs once.
    pub(super) fn set_overrides(
        &mut self,
        ids: &[String],
        mut edit: impl FnMut(&mut SessionMeta),
        cx: &mut Context<Self>,
    ) {
        if ids.is_empty() {
            return;
        }
        for session_id in ids {
            edit(self.overrides.entry(session_id.clone()).or_default());
        }
        self.settle_overrides(cx);
    }

    /// What every override edit costs once it is made: the rows rejoin, the
    /// store is written, and the search index is rebuilt around the change.
    fn settle_overrides(&mut self, cx: &mut Context<Self>) {
        self.rejoin();
        sessions::write(&self.overrides);
        self.rebuild_search_index(cx);
        cx.notify();
    }

    /// Forget `project_id` on every session that carries it, settled once:
    /// project removal re-resolves the rows by root, so a later re-add finds
    /// them again.
    pub(crate) fn clear_session_projects(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.project.as_deref() == Some(project_id))
            .map(|entry| entry.id.clone())
            .collect();
        self.set_overrides(&ids, |meta| meta.project = None, cx);
    }

    /// `/name`, and the row's inline field: rename the active session.
    pub(super) fn rename_session(&mut self, session_id: String, name: Option<String>, cx: &mut Context<Self>) {
        let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        self.set_override(&session_id, |meta| meta.name = name, cx);
        self.renaming = None;
    }

    /// Open the inline field on a row, seeded with what the row says now.
    pub(crate) fn start_rename(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .overrides
            .get(&session_id)
            .and_then(|m| m.name.clone())
            .or_else(|| self.sessions.iter().find(|e| e.id == session_id).map(|e| e.label.clone()))
            .unwrap_or_default();
        self.rename.update(cx, |state, cx| state.set_value(current, window, cx));
        self.renaming = Some(session_id);
        window.focus(&self.rename.focus_handle(cx), cx);
        cx.notify();
    }

    /// Commit whatever is in the rename field: the project when a project
    /// rename is active, else the session row.
    pub(crate) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming_project.is_some() {
            self.commit_project_rename(cx);
            let _ = window;
            return;
        }
        let Some(session_id) = self.renaming.clone() else { return };
        let text = self.rename.read(cx).value().to_string();
        self.rename_session(session_id, Some(text), cx);
        self.focus_composer = true;
        let _ = window;
    }

    /// `/hide` and the row's eye: take a session out of the list, with a way
    /// back for eight seconds.
    ///
    /// A hidden session is never loaded — the row is gone and so is the
    /// transcript — so the active one is closed when it is the one hidden.
    pub(super) fn hide_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.active.as_ref().is_some_and(|a| a.read(cx).session_id == session_id) {
            self.active = None;
        }
        self.hide_batch(
            vec![session_id],
            "Session hidden".to_owned(),
            "It is still on disk; Muse keeps its own list.",
            cx,
        );
    }

    /// Hide a batch of sessions with a way back for eight seconds: one
    /// toast, one Undo that restores the whole batch. One `/hide` is a
    /// batch of one; one "Clear empty" is a batch of everything it hid.
    pub(super) fn hide_batch(&mut self, ids: Vec<String>, title: String, detail: &str, cx: &mut Context<Self>) {
        // Hidden sessions are never cached: a parked view of one is dropped,
        // and reopening (after Undo, or under "Show hidden") pages it afresh.
        self.session_cache.retain(|(id, _)| !ids.contains(id));
        self.set_overrides(&ids, |meta| meta.hidden = true, cx);
        self.push_undo(
            UndoBatch::Hidden(ids),
            title,
            detail.to_owned(),
            cx,
        );
        cx.notify();
    }

    /// One toast with one Undo for one undoable batch, and a timer that takes
    /// both away together, so a press after the toast has gone does nothing.
    pub(super) fn push_undo(&mut self, batch: UndoBatch, title: String, detail: String, cx: &mut Context<Self>) {
        let toast =
            self.overlays.update(cx, |overlays, _| overlays.toast_with_action(title, &detail, "Undo"));
        let undo = batch.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&toast));
                this.undo_stack.retain(|batch| *batch != undo);
                cx.notify();
            });
        }));
        self.undo_stack.push(batch);
        cx.notify();
    }

    /// The toast's Undo: put the newest batch back — one hidden row, one
    /// "Clear empty" whole, or one archived session.
    pub(crate) fn undo_newest(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.undo_stack.pop() else { return };
        match batch {
            UndoBatch::Hidden(ids) => self.set_overrides(&ids, |meta| meta.hidden = false, cx),
            UndoBatch::Archived(ids) => self.set_overrides(&ids, |meta| meta.archived = false, cx),
        }
    }

    /// "Clear empty": hide every session with no turns, with a way back for
    /// eight seconds. Rows already hidden, archived rows, and the open
    /// session — none of which Clear must ever sweep — stay out of the
    /// batch, so Undo restores exactly what this hid and nothing it did not.
    ///
    /// The open session is excluded here, not by [`SessionEntry::is_empty`]
    /// (which no longer exempts it — v0.1 prep task 3): `hidden` is a sticky,
    /// explicit flag with no auto-clear on activity, so sweeping a person's
    /// live draft would hide it **permanently**, past its first send, until
    /// they noticed and unhid it by hand — worse than the no-row-yet state
    /// Clear Empty is trying to tidy away.
    pub(crate) fn clear_empty(&mut self, cx: &mut Context<Self>) {
        let active = self.active_id(cx);
        let cleared: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| {
                !entry.hidden
                    && !entry.archived
                    && entry.is_empty()
                    && !active.as_deref().is_some_and(|id| id == entry.id)
            })
            .map(|entry| entry.id.clone())
            .collect();
        if cleared.is_empty() {
            return;
        }
        let n = cleared.len();
        self.hide_batch(
            cleared,
            format!("{n} empty session{} hidden", if n == 1 { "" } else { "s" }),
            "They are still on disk; Muse keeps its own list.",
            cx,
        );
    }

    /// Pin or unpin a session. Purely local: the list regroups around it
    /// and the store keeps it.
    pub(crate) fn toggle_pin(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.set_override(&session_id, |meta| meta.pinned = !meta.pinned, cx);
    }

    /// Ask before archiving: a danger dialog carrying its target, so only its
    /// own Archive button can confirm it.
    pub(crate) fn open_archive_dialog(&mut self, session_id: String, cx: &mut Context<Self>) {
        let label = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| sidebar::UNNAMED.to_owned());
        self.set_dialog(cx, Dialog {
            title: format!("Archive \"{label}\"?"),
            detail: "Archived sessions stay on disk and can be shown from the Sessions menu.".into(),
            kind: DialogKind::Warning,
            primary: "Archive",
            action: DialogAction::Archive,
            archive_target: Some(session_id),
        });
    }

    /// The archive dialog's Archive button, or the `archive-confirm` step.
    pub(crate) fn confirm_archive_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.overlays.read(cx).dialog.as_ref().and_then(|d| {
            (d.action == DialogAction::Archive).then(|| d.archive_target.clone()).flatten()
        });
        let Some(session_id) = target else { return };
        self.close_dialog(cx);
        self.archive_session(session_id, Some(window), cx);
    }

    /// Archive a session out of the list, with a way back for eight seconds.
    ///
    /// An archived session is never loaded, so the active one closes when it
    /// is the one archived: the newest remaining visible session opens in its
    /// place, or the empty state when nothing remains.
    pub(super) fn archive_session(&mut self, session_id: String, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let was_active = self.active.as_ref().is_some_and(|a| a.read(cx).session_id == session_id);
        // Archived sessions are never cached: a parked view of one is
        // dropped, and reopening (after Undo) pages it afresh.
        self.session_cache.retain(|(id, _)| *id != session_id);
        self.set_override(&session_id, |meta| meta.archived = true, cx);
        if was_active {
            self.active = None;
        }
        self.push_undo(
            UndoBatch::Archived(vec![session_id]),
            "Session archived".to_owned(),
            "It is still on disk; show it again from the Sessions menu.".to_owned(),
            cx,
        );
        // The newest remaining visible session opens in place of the archived
        // one; with no window (a step, not a click) the empty state stays
        // until the person picks a session.
        if was_active {
            if let Some(window) = window {
                let next = self.visible_sessions(cx).first().map(|e| e.id.clone());
                if self.client.is_some() {
                    if let Some(id) = next {
                        self.resume(id, window, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Put a session back in the list (the Archive tray action on an archived
    /// row, or the toast's Undo through [`Self::undo_newest`]).
    pub(crate) fn unarchive_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.set_override(&session_id, |meta| meta.archived = false, cx);
    }

    /// A turn completed in this app: leave the first line of its last
    /// assistant text on the sidebar row. Free — the fold is in memory — and
    /// skipped when nothing new arrived, so the store is not rewritten on
    /// every completion.
    pub(super) fn record_last_summary(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let (session_id, summary) = (view.read(cx).session_id.clone(), view.read(cx).last_summary_text());
        let Some(summary) = summary else { return };
        if self.overrides.get(&session_id).and_then(|m| m.last_summary.as_deref()) == Some(summary.as_str()) {
            return;
        }
        self.set_override(&session_id, |meta| meta.last_summary = Some(summary), cx);
    }

    /// The open session's id, which the empty filter never applies to: a
    /// session just created has no turns yet and must stay visible.
    pub(crate) fn active_id(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|a| a.read(cx).session_id.clone())
    }

    /// The rows the sidebar should draw: hidden ones out unless asked for,
    /// archived ones out unless asked for, sessions with no turns out unless
    /// asked for. Text search lives in the search palette (⌘⇧F), never in a
    /// sidebar field, so no needle applies here. The open session is always
    /// drawn.
    /// Cached: the clone and the sort run once per change, not once per
    /// caller per frame (finding `performance-5`). `render_sidebar`, the
    /// sidebar's empty states, the Resume palette and the search rows all read
    /// the same `Rc`; [`Self::invalidate_list`] is what drops it.
    /// Which grouping the sidebar draws: the person's explicit choice, else
    /// Project once there is more than one adoption or a session that
    /// resolves to none, else Date — so one adopted project with everything
    /// in it draws exactly the date view it always did.
    ///
    /// Replayed rows are capture stand-ins, not sessions that failed to
    /// adopt: they never flip the default on their own, so a `--replay`
    /// capture draws as it always did whatever the store holds.
    pub(crate) fn effective_group_by(&self) -> crate::layout::GroupBy {
        if let Some(mode) = self.layout.group_by {
            return mode;
        }
        if self.projects.projects.len() >= 2 {
            return crate::layout::GroupBy::Project;
        }
        if self.sessions.iter().any(|e| !e.replayed && e.project.is_none()) {
            return crate::layout::GroupBy::Project;
        }
        crate::layout::GroupBy::Date
    }

    /// The Sessions view menu's "Group by project": persist the other mode
    /// and regroup. The first toggle is what persists the choice at all —
    /// until then the grouping follows the data.
    pub(crate) fn toggle_group_by(&mut self, cx: &mut Context<Self>) {
        let next = match self.effective_group_by() {
            crate::layout::GroupBy::Project => crate::layout::GroupBy::Date,
            crate::layout::GroupBy::Date => crate::layout::GroupBy::Project,
        };
        self.layout.group_by = Some(next);
        layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// Flip a project group's disclosure: its id moves in or out of
    /// `closed_groups` and the layout is written. "Other workspaces" reads
    /// the same set inverted (closed until opened), so the one flip rule
    /// serves both rows.
    pub(crate) fn toggle_group(&mut self, id: String, cx: &mut Context<Self>) {
        if self.layout.closed_groups.iter().any(|g| g == &id) {
            self.layout.closed_groups.retain(|g| g != &id);
        } else {
            self.layout.closed_groups.push(id);
        }
        layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    /// Flip a project group's "Show N more": its id moves in or out of
    /// `expanded_groups` (persisted exactly like `closed_groups`) and the
    /// list regroups. The group's own disclosure is untouched — an expanded
    /// group can stand closed and open folded.
    pub(crate) fn toggle_expanded(&mut self, id: String, cx: &mut Context<Self>) {
        if self.layout.expanded_groups.iter().any(|g| g == &id) {
            self.layout.expanded_groups.retain(|g| g != &id);
        } else {
            self.layout.expanded_groups.push(id);
        }
        layout::write(&self.layout);
        self.invalidate_list();
        cx.notify();
    }

    pub(crate) fn visible_sessions(&self, cx: &gpui::App) -> Rc<Vec<SessionEntry>> {
        let active = self.active_id(cx);
        let group_by = self.effective_group_by();
        let closed = self.layout.closed_groups.clone();
        let expanded = self.layout.expanded_groups.clone();
        // The click's target rides the key: the grouping rescues it past the
        // fold, so a click (or an `open:` step) that names a held-back row
        // must rebuild the grouping on its own frame.
        let pending = self.pending_id.clone();
        let mut cache = self.list_cache.borrow_mut();
        if cache.key.as_ref().is_some_and(|(epoch, id, awaited, cached_group, cached_closed, cached_expanded)| {
            *epoch == self.list_epoch
                && *id == active
                && *awaited == pending
                && *cached_group == group_by
                && *cached_closed == closed
                && *cached_expanded == expanded
        }) {
            return Rc::clone(&cache.visible);
        }
        let mut rows: Vec<SessionEntry> = self
            .sessions
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .filter(|entry| self.show_archived || !entry.archived)
            .filter(|entry| {
                // An archived row shown on request is explicitly asked for;
                // the empty filter must not swallow it back.
                (self.show_archived && entry.archived) || self.show_empty || !entry.is_empty()
            })
            .cloned()
            .collect();
        // Newest first. The sidebar's grouping sorts for itself; the palette
        // takes the head of this list, so the order has to be right here.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        cache.key = Some((self.list_epoch, active, pending, group_by, closed, expanded));
        cache.visible = Rc::new(rows);
        cache.grouping = None;
        Rc::clone(&cache.visible)
    }

    /// The sidebar's grouping for this frame, built once per change and once
    /// per minute (findings `performance-5`, `performance-6`, `support-2`).
    ///
    /// Per minute because that is the finest thing a row's elapsed tag says —
    /// `now`, `14m`, `2h`, `3d` — so a cached grouping can only go stale when
    /// the minute turns. Under `HARNESS_DETERMINISTIC=1` "now" comes from the
    /// data rather than the clock, so the key is constant and a capture is
    /// identical run to run.
    pub(crate) fn sidebar_grouping(&self, cx: &gpui::App) -> Rc<sidebar::Grouping> {
        let visible = self.visible_sessions(cx);
        let now = sidebar::grouping_now(&visible);
        let minute = now.timestamp().div_euclid(60);
        let mut cache = self.list_cache.borrow_mut();
        if let Some((cached, grouping)) = cache.grouping.as_ref() {
            if *cached == minute {
                return Rc::clone(grouping);
            }
        }
        let grouping = Rc::new(match self.effective_group_by() {
            crate::layout::GroupBy::Date => sidebar::grouping_at(&visible, now),
            crate::layout::GroupBy::Project if self.sessions_loaded && self.index_loaded => {
                let closed: std::collections::HashSet<String> =
                    self.layout.closed_groups.iter().cloned().collect();
                let expanded: std::collections::HashSet<String> =
                    self.layout.expanded_groups.iter().cloned().collect();
                let active = self.active_id(cx);
                let view = sidebar::GroupView {
                    closed: &closed,
                    expanded: &expanded,
                    active: active.as_deref(),
                    pending: self.pending_id.as_deref(),
                };
                sidebar::grouping_by_project(&visible, &self.projects, &self.branches, &view, &self.layout, now)
            }
            // The list or the index hasn't landed yet: a flat date view,
            // exactly what the window showed while loading before projects
            // existed. Row visibility comes from the index, so project
            // groups debut only with their rows — the collapse measures
            // content on its first frame instead of opening from an empty
            // box it never re-measures.
            crate::layout::GroupBy::Project => sidebar::grouping_at(&visible, now),
        });
        cache.grouping = Some((minute, Rc::clone(&grouping)));
        grouping
    }

    /// Drop the cached visible list and grouping: something they are derived
    /// from changed.
    ///
    /// Called from the four places that can change it — the `session/list`
    /// reply, [`Self::rejoin`] (which every override edit and the index read
    /// go through), the three view toggles, and the replay's single row.
    pub(crate) fn invalidate_list(&mut self) {
        self.list_epoch = self.list_epoch.wrapping_add(1);
    }

    /// F10. A session with no title anywhere: read its head and take the first
    /// `userShell` command as the row's name.
    ///
    /// `session/read` makes no model call, and the answer is cached in the
    /// store, so this costs one read per session, once, ever. It is also
    /// allowed to come back with nothing: the server decides what history it
    /// serves, and a session no host has loaded can serve none. A row that
    /// still has no title after this is honestly [`crate::sidebar::UNNAMED`].
    ///
    /// A stale sidecar (muse 1.2.1, #29473) is "no title this time": one log
    /// line, never a dialog, and the session stays untitled-marked so a later
    /// pass retries once the sidecar has regenerated under a lease.
    pub(super) fn derive_titles(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let wanted: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.needs_title && !self.titled.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .take(MAX_TITLE_READS)
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.titled.extend(wanted.iter().cloned());
        let work = move || {
            wanted
                .into_iter()
                .map(|session_id| {
                    let read = client.session_read(&muse_client::schema::SessionReadParams {
                        session_id: session_id.clone(),
                        exclude_items: Some(false),
                    });
                    match read {
                        Ok(read) => (session_id, first_shell_command(&read), false),
                        Err(error) => {
                            crate::harness_log!("session/read for a title failed: {error}");
                            (session_id, None, error.is_stale_sidecar())
                        }
                    }
                })
                .collect::<Vec<_>>()
        };
        self.wire_call(cx, work, |this, derived, cx| {
            for (session_id, title, stale) in derived {
                if stale {
                    this.titled.remove(&session_id);
                    continue;
                }
                let Some(title) = title else { continue };
                this.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
            }
        });
    }

    /// F10. The open session's transcript may name it when nothing else does.
    ///
    /// Free — the fold is in memory — and the one path that reaches a session
    /// whose history the server will not serve to a `session/read`.
    pub(super) fn title_from_transcript(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        // The row may not be in the list yet — `session/list` is a round-trip
        // and the transcript is already here — so the question is not "does the
        // row need a title" but "does this session have one".
        let has_title = self.overrides.get(&session_id).is_some_and(|m| m.name.is_some() || m.derived_title.is_some())
            || self.index.get(&session_id).and_then(IndexEntry::label).is_some();
        if has_title {
            return;
        }
        let Some(title) = view.read(cx).first_shell_title() else { return };
        self.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
    }
}
