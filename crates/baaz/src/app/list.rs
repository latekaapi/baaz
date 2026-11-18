//! What a person can do to a row: rename, pin, hide, archive, clear the
//! empty ones, and undo any of it.
//!
//! Every one of these is a local override written to Baaz's own store
//! (`docs/03-composer.md` §5); Muse's storage is read-only, so a name or a
//! hidden flag never reaches the wire.
//!
//! Part of [`Harness`]; see [`crate::app`] for what the entity owns.

use super::*;
use crate::session_list::{ListFacts, SessionList};

impl Harness {
    /// Which grouping the sidebar draws ([`SessionList::effective_group_by`]).
    pub(crate) fn effective_group_by(&self, cx: &gpui::App) -> crate::layout::GroupBy {
        SessionList::effective_group_by(cx)
    }

    /// What a list read needs from the root: the open session, the awaited
    /// click, whether either is an unsent draft, and whose terminal tabs run
    /// a block ([`ListFacts`]). Read live on every call, so a read right
    /// after a switch sees the switch.
    pub(crate) fn list_facts(&self, cx: &gpui::App) -> ListFacts {
        let active = self.active_id(cx);
        let pending = self.open_state.pending_id.clone();
        ListFacts {
            active_unsent: active.as_deref().is_some_and(|id| self.open_state.is_unsent(id)),
            pending_unsent: pending.as_deref().is_some_and(|id| self.open_state.is_unsent(id)),
            active,
            pending,
            terminals: self.terminal_dock.read(cx).host().read(cx).busy_counts(cx),
        }
    }

    /// The rows the sidebar should draw ([`SessionList::visible`]) for this
    /// window's facts.
    pub(crate) fn visible_sessions(&self, cx: &gpui::App) -> Rc<Vec<SessionEntry>> {
        self.session_list.read(cx).visible(&self.list_facts(cx), cx)
    }

    /// The sidebar's grouping ([`SessionList::grouping`]) for this window's facts.
    pub(crate) fn sidebar_grouping(&self, cx: &gpui::App) -> Rc<sidebar::Grouping> {
        self.session_list.read(cx).grouping(&self.list_facts(cx), cx)
    }


    /// Something the list is derived from changed: the list says so
    /// ([`SessionList::invalidate`]) and its observers redraw.
    ///
    /// Called from the places that can change it — the `session/list`
    /// reply, [`Self::rejoin`] (which every override edit and the index read
    /// go through), the three view toggles, and the replay's single row.
    pub(crate) fn invalidate_list(&mut self, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.invalidate(cx));
    }


    /// One row changed from `before` ([`SessionList::invalidate_row`]).
    pub(crate) fn invalidate_row(&mut self, before: &SessionEntry, cx: &mut gpui::App) {
        self.session_list.update(cx, |list, cx| list.invalidate_row(before, cx));
    }

    // ---------------------------------------------- session operations (A2)

    /// Change one session's override and write the store.
    ///
    /// The write is synchronous, and deliberately: it is a few hundred bytes,
    /// it happens on a gesture rather than in a loop, and a background write
    /// can lose a rename to a window that closed a moment later — which is the
    /// one outcome a store exists to prevent.
    pub(crate) fn set_override(&mut self, session_id: &str, edit: impl FnOnce(&mut SessionMeta), cx: &mut Context<Self>) {
        let before = self.session_list.read(cx).overrides.get(session_id).cloned();
        self.session_list.update(cx, |list, _| edit(list.overrides.entry(session_id.to_owned()).or_default()));
        // An edit that changed nothing — a reopen re-stating the project the
        // session already has — settles nothing: the rejoin and search
        // rebuild it used to run were 75 ms of UI thread on every provider
        // reopen (perf P1, 2026-10-18), for a list and index that could not
        // differ.
        if before.as_ref() == self.session_list.read(cx).overrides.get(session_id) {
            return;
        }
        // A session with no row, no index entry and no chain link is
        // nothing a rejoin or the search index reads: a new session's
        // project lands here before either exists, and the full rejoin and
        // search rebuild cost 0.7–1.2 s of UI thread over ~2,600 rows for
        // a change neither could see. The store is still written.
        let inert = !self.session_list.read(cx).sessions.iter().any(|entry| entry.id == session_id)
            && !self.session_list.read(cx).index.contains_key(session_id)
            && !crate::sidebar::needs_collapse(session_id, &self.session_list.read(cx).provider_sessions, &self.session_list.read(cx).overrides);
        if inert {
            crate::sessions::save(&self.session_list.read(cx).overrides);
            cx.notify();
            return;
        }
        // One session's override: its row rejoins and its search row
        // re-syncs, not the whole list (speed S4 — the byline and the
        // generated title land here after every finished turn).
        let ids: std::collections::HashSet<String> = std::iter::once(session_id.to_owned()).collect();
        self.settle_overrides_scoped(Some(&ids), cx);
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
            self.session_list.update(cx, |list, _| edit(list.overrides.entry(session_id.clone()).or_default()));
        }
        self.settle_overrides(cx);
    }

    /// What every override edit costs once it is made: the rows rejoin, the
    /// store is written, and the search index is synced around the change.
    /// The snapshot is taken here and written off the UI thread (perf audit
    /// 2026-10-16: 110 KB of JSON per pin, rename or title); it replaces any
    /// queued pane-state snapshot, so stale state never lands over this
    /// edit, and the quit flush waits for it ([`crate::store::JsonStore`]).
    fn settle_overrides(&mut self, cx: &mut Context<Self>) {
        self.settle_overrides_scoped(None, cx);
    }

    /// [`Self::settle_overrides`] for the sessions `only` names (`None`:
    /// every row): the rejoin and the search sync are scoped alike.
    fn settle_overrides_scoped(&mut self, only: Option<&std::collections::HashSet<String>>, cx: &mut Context<Self>) {
        self.rejoin_scoped(only, cx);
        crate::sessions::save(&self.session_list.read(cx).overrides);
        match only {
            Some(ids) => self.rebuild_search_index_for(ids, cx),
            None => self.rebuild_search_index(cx),
        }
        cx.notify();
    }

    /// Write the provider records (`provider-sessions.json`) off the UI
    /// thread: the snapshot is taken here and queued on the store's writer,
    /// so a later snapshot always wins and the store can grow (a record per
    /// opened Muse session, docs/70) without costing a frame. The quit
    /// flush waits for it ([`Self::flush_stores`]).
    pub(crate) fn persist_provider_sessions(&mut self, cx: &mut Context<Self>) {
        crate::provider_sessions::save(&self.session_list.read(cx).provider_sessions);
    }

    /// Forget `project_id` on every session that carries it, settled once:
    /// project removal re-resolves the rows by root, so a later re-add finds
    /// them again.
    pub(crate) fn clear_session_projects(&mut self, project_id: &str, cx: &mut Context<Self>) {
        let ids: Vec<String> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|entry| entry.project.as_deref() == Some(project_id))
            .map(|entry| entry.id.clone())
            .collect();
        self.set_overrides(&ids, |meta| meta.project = None, cx);
    }

    /// `/name`, and the row's inline field: rename the active session.
    pub(crate) fn rename_session(&mut self, session_id: String, name: Option<String>, cx: &mut Context<Self>) {
        let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        self.set_override(&session_id, |meta| meta.name = name, cx);
        self.overlays.update(cx, |o, cx| {
            o.close(crate::overlay_stack::LayerKind::RenameSession, cx);
        });
    }

    /// The session whose row is being renamed in place (the overlay stack's
    /// `RenameSession` entry).
    pub(crate) fn renaming(&self, cx: &gpui::App) -> Option<String> {
        self.overlays.read(cx).stack.renaming().cloned()
    }

    /// The project being renamed through the header's rename field: the
    /// same `rename` field does it, and `ConfirmRename` commits the project
    /// while this stands rather than the session row.
    pub(crate) fn renaming_project(&self, cx: &gpui::App) -> Option<String> {
        self.overlays.read(cx).stack.renaming_project().cloned()
    }

    /// An inline rename on the overlay stack: Escape drops it and hands
    /// the keyboard to the composer.
    pub(crate) fn rename_entry(layer: crate::overlay_stack::Layer) -> crate::overlay_stack::Entry {
        crate::overlay_stack::Entry::new(layer).on_close(|_, baaz, cx| baaz.arm_composer_focus(cx))
    }

    /// Open the inline field on a row, seeded with what the row says now.
    pub(crate) fn start_rename(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .session_list.read(cx).overrides
            .get(&session_id)
            .and_then(|m| m.name.clone())
            .or_else(|| self.session_list.read(cx).sessions.iter().find(|e| e.id == session_id).map(|e| e.label.clone()))
            .unwrap_or_default();
        self.rename.update(cx, |state, cx| state.set_value(current, window, cx));
        let entry = Self::rename_entry(crate::overlay_stack::Layer::RenameSession(session_id));
        self.overlays.update(cx, |o, cx| o.push(entry, cx));
        window.focus(&self.rename.focus_handle(cx), cx);
    }

    /// Commit whatever is in the rename field: the project when a project
    /// rename is active, else the session row.
    pub(crate) fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.renaming_project(cx).is_some() {
            self.commit_project_rename(cx);
            let _ = window;
            return;
        }
        let Some(session_id) = self.renaming(cx) else { return };
        let text = self.rename.read(cx).value().to_string();
        self.rename_session(session_id, Some(text), cx);
        self.arm_composer_focus(cx);
        let _ = window;
    }

    /// `/hide` and the row's eye: take a session out of the list, with a way
    /// back for eight seconds.
    ///
    /// A hidden session is never loaded — the row is gone and so is the
    /// transcript — so the active one is closed when it is the one hidden.
    pub(super) fn hide_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.active.as_ref().is_some_and(|a| a.read(cx).rt(cx).session_id == session_id) {
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
        self.registry.forget_where(|id| ids.iter().any(|hidden| hidden == id));
        self.set_overrides(&ids, |meta| meta.hidden = true, cx);
        self.push_undo(
            UndoBatch::Hidden(ids),
            title,
            detail.to_owned(),
            cx,
        );
    }

    /// One toast with one Undo for one undoable batch, and a timer that takes
    /// both away together, so a press after the toast has gone does nothing.
    pub(crate) fn push_undo(&mut self, batch: UndoBatch, title: String, detail: String, cx: &mut Context<Self>) {
        let toast =
            self.overlays.update(cx, |overlays, cx| overlays.toast_with_action(title, &detail, "Undo", cx));
        let undo = batch.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, cx| overlays.dismiss_toast(&toast, cx));
                this.session_list.update(cx, |list, _| list.undo_stack.retain(|batch| *batch != undo));
            });
        }));
        self.session_list.update(cx, |list, _| list.undo_stack.push(batch));
    }

    /// The toast's Undo: put the newest batch back — one hidden row, one
    /// "Clear empty" whole, or one archived session.
    pub(crate) fn undo_newest(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.session_list.update(cx, |list, _| list.undo_stack.pop()) else { return };
        match batch {
            UndoBatch::Hidden(ids) => self.set_overrides(&ids, |meta| meta.hidden = false, cx),
            UndoBatch::Archived(ids) => self.set_overrides(&ids, |meta| meta.archived = false, cx),
            UndoBatch::Imported(run) => self.undo_import(&run, cx),
        }
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
            .session_list.read(cx).sessions
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
        let target = self.overlays.read(cx).dialog().and_then(|d| {
            (d.action == DialogAction::Archive).then(|| d.archive_target.clone()).flatten()
        });
        let Some(session_id) = target else { return };
        self.close_dialog(cx);
        self.archive_session(session_id, Some(window), cx);
    }

    /// Archive a session out of the list, with a way back for eight seconds.
    ///
    /// A handoff chain archives as one (`docs/22-handoff.md` §8): the head's
    /// row is the unit the person sees, and every member takes the flag so
    /// none resurfaces past it.
    ///
    /// An archived session is never loaded, so the active one closes when it
    /// is the one archived: the newest remaining visible session opens in its
    /// place, or the empty state when nothing remains.
    pub(super) fn archive_session(&mut self, session_id: String, window: Option<&mut Window>, cx: &mut Context<Self>) {
        // The whole chain, head included: archiving names the head, the
        // expansion covers the members.
        let members = self.chain_members(&session_id, cx);
        let was_active = self.active.as_ref().is_some_and(|a| a.read(cx).rt(cx).session_id == session_id);
        // Archived sessions are never cached: a parked view of one is
        // dropped, and reopening (after Undo) pages it afresh.
        self.registry.forget_where(|id| members.iter().any(|member| member == id));
        self.set_overrides(&members, |meta| meta.archived = true, cx);
        if was_active {
            self.active = None;
        }
        self.push_undo(
            UndoBatch::Archived(members),
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
                if self.muse_slot.is_ready() {
                    if let Some(id) = next {
                        self.resume(id, window, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Put a session back in the list (the Archive tray action on an archived
    /// row, or the toast's Undo through [`Self::undo_newest`]). A chain
    /// unarchives as one, symmetric with [`Self::archive_session`].
    pub(crate) fn unarchive_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let members = self.chain_members(&session_id, cx);
        self.set_overrides(&members, |meta| meta.archived = false, cx);
    }

    /// A turn completed in this app: leave the free byline excerpt on the
    /// sidebar row — the user's last request beside the first meaningful
    /// line of the latest reply. Free — the fold is already in memory — so
    /// it refreshes after every completed turn, and skipped when nothing new
    /// arrived, so the store is not rewritten on every completion.
    ///
    /// `view` is the session that finished — often a parked one, never
    /// assumed to be the open one (docs/81 A F3).
    pub(super) fn record_last_summary(&mut self, runtime: &Entity<SessionRuntime>, cx: &mut Context<Self>) {
        let (session_id, summary, ask) = (
            runtime.read(cx).session_id.clone(),
            runtime.read(cx).last_summary_text(),
            runtime.read(cx).last_user_text(),
        );
        if summary.is_none() && ask.is_none() {
            return;
        }
        // The pack turn and its acknowledgement never become the byline:
        // when the newest ask is the destination's pack, the newest
        // summary is its acknowledgement, so neither lands (Y2a). The
        // skip is gated on the destination: any other session's words
        // land untouched, even pack-shaped ones (Y2a3).
        let dest = crate::sidebar::is_handoff_dest(&session_id, &self.session_list.read(cx).provider_sessions, &self.session_list.read(cx).overrides);
        let newest_ask_is_pack = dest && ask.as_deref().is_some_and(crate::sidebar::is_pack_text);
        let (summary, ask) = crate::sidebar::byline_landable(summary, ask, newest_ask_is_pack);
        if summary.is_none() && ask.is_none() {
            return;
        }
        let current = self.session_list.read(cx).overrides.get(&session_id);
        if current.and_then(|m| m.last_summary.as_deref()) == summary.as_deref()
            && current.and_then(|m| m.last_ask.as_deref()) == ask.as_deref()
        {
            return;
        }
        self.set_override(&session_id, |meta| {
            if summary.is_some() {
                meta.last_summary = summary;
            }
            if ask.is_some() {
                meta.last_ask = ask;
            }
        }, cx);
    }

    /// Keep one row's live facts current without waiting for `session/list`.
    ///
    /// A started turn reads running with its start (and stands down any
    /// recorded failure — the new turn supersedes it); an approval or
    /// question event re-reads the live view's pending words, which the
    /// wire never carries as text; anything else re-reads the live view's
    /// running bit, so a completed turn stands the row down even before the
    /// list reply lands. The live view is authoritative for its own
    /// attention: once it holds no pending words, stale wire flags clear
    /// rather than pinning the row on `Needs approval` until the next
    /// listing. Side sessions never touch rows — hidden title/byline
    /// workers, not sidebar state. Call after the event reached the view,
    /// so the fold already holds the event's world.
    pub(crate) fn sync_row_live(&mut self, session_id: &str, started: bool, cx: &mut Context<Self>) {
        if self.is_side_session(session_id, cx) {
            return;
        }
        let Some(before) = self.session_list.read(cx).sessions.iter().find(|entry| entry.id == session_id).cloned() else {
            return;
        };
        // The open view applies its lane's events before the row syncs:
        // when it declined a re-delivered start for a turn it
        // already saw complete, the row declines it too instead of
        // manufacturing a fresh `Working · now` (item 3). A session with no
        // open view keeps the wire's word.
        let mut started_running = started;
        if started {
            if let Some(view) = self.active.clone() {
                if view.read(cx).rt(cx).session_id == session_id && !view.read(cx).rt(cx).working() {
                    started_running = false;
                }
            }
        }
        // The live runtime applies its lane's events before the row syncs:
        // a parked (or view-less) runtime's fold is as current as the
        // open one's, so its row re-reads it the same way (B3b).
        let view_truth = self.registry.find_runtime(self.active.as_ref(), session_id, cx).map(|runtime| {
            let runtime = runtime.read(cx);
            let (approval, question) = runtime.row_pending();
            (approval, question, runtime.working())
        });
        let changed = self.session_list.update(cx, |list, _| {
            let Some(entry) = list.sessions.iter_mut().find(|entry| entry.id == session_id) else {
                return false;
            };
            let mut changed = false;
            if started_running {
                let now = crate::clock::now_local();
                if !entry.running {
                    entry.running = true;
                }
                entry.turn_started = Some(now);
                entry.updated = now;
                changed = true;
                if entry.last_error.is_some() {
                    entry.last_error = None;
                    changed = true;
                }
            }
            if let Some((approval, question, running)) = view_truth {
                if entry.approval_command != approval {
                    entry.approval_command = approval;
                    changed = true;
                }
                if entry.pending_question != question {
                    entry.pending_question = question;
                    changed = true;
                }
                if !started && entry.running != running {
                    entry.running = running;
                    changed = true;
                }
                // The view just spoke: no pending words means no pending
                // attention, whatever the last listing said.
                if entry.approval_command.is_none() && entry.pending_question.is_none() && !entry.attention.is_empty() {
                    entry.attention.clear();
                    changed = true;
                }
            }
            changed
        });
        if changed {
            self.invalidate_row(&before, cx);
        }
    }

    /// Lay every live view's truth back over its row: a `session/list`
    /// reply rebuilds rows from the wire's projection, which may still say
    /// idle while a view holds a live turn (a command running after its
    /// approval emits nothing for minutes). Without this the open
    /// session's row flips to `Settled` until its next live event. Runs
    /// with `started: false`, so no row manufactures `Working` — a view
    /// that already stood down stays stood down.
    pub(crate) fn sync_all_live_rows(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.session_list.read(cx).sessions.iter().map(|entry| entry.id.clone()).collect();
        for id in ids {
            if self.find_view(&id, cx).is_some() {
                self.sync_row_live(&id, false, cx);
            }
        }
    }

    /// Record one turn's terminal on its row and in the store: a failed
    /// turn's message is what `Failed` stands on (taken from
    /// `turn/completed`'s `error`, the only place a mid-turn failure
    /// reaches the client — never as a JSON-RPC error), and any other
    /// terminal stands a recorded error down. Side sessions never touch
    /// rows. Skipped when nothing would change, so the store is not
    /// rewritten on every completion.
    pub(crate) fn record_turn_outcome(
        &mut self,
        session_id: &str,
        failed: bool,
        error: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if self.is_side_session(session_id, cx) {
            return;
        }
        let message = error.map(str::trim).filter(|s| !s.is_empty());
        let next = if failed { message.map(str::to_owned) } else { None };
        let changed_entry = self.session_list.update(cx, |list, _| {
            list.sessions.iter_mut().find(|entry| entry.id == session_id).is_some_and(|entry| {
                let before = entry.last_error.clone();
                entry.apply_turn_outcome(failed, error);
                entry.last_error != before
            })
        });
        // The turn's end is unread unless the person is reading this
        // session at its end right now (task 3.1).
        let unread = !self.reading_at_end(session_id, cx);
        let current = self.session_list.read(cx).overrides.get(session_id);
        let current_error = current.and_then(|m| m.last_error.clone());
        let current_unread = current.is_some_and(|m| m.unread);
        if current_error != next || current_unread != unread {
            self.set_override(
                session_id,
                |meta| {
                    meta.last_error = next;
                    meta.unread = unread;
                },
                cx,
            );
        } else if changed_entry {
            self.invalidate_list(cx);
        }
    }

    /// Whether `session_id` is the open session with its transcript at its
    /// end: the only place a finished turn is read as it lands.
    pub(crate) fn reading_at_end(&self, session_id: &str, cx: &gpui::App) -> bool {
        self.active.as_ref().is_some_and(|view| {
            let view = view.read(cx);
            view.rt(cx).session_id == session_id && view.at_transcript_end()
        })
    }

    /// The person read `session_id` to its end: its `DoneUnread` clears
    /// (task 3.1). A no-op for a row that is not unread, so the store is
    /// not rewritten on every arrival at the end.
    pub(crate) fn mark_read(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.session_list.read(cx).overrides.get(session_id).is_some_and(|m| m.unread) {
            self.set_override(session_id, |meta| meta.unread = false, cx);
        }
    }

    /// The open session's id. A just-created session has no turns yet
    /// and stays rowless until its first send reveals it; only a real
    /// handoff head keeps its row while still empty.
    pub(crate) fn active_id(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|a| a.read(cx).rt(cx).session_id.clone())
    }


    /// The Sessions view menu's "Group by project": persist the other mode
    /// and regroup. The first toggle is what persists the choice at all —
    /// until then the grouping follows the data.
    pub(crate) fn toggle_group_by(&mut self, cx: &mut Context<Self>) {
        let next = match self.effective_group_by(cx) {
            crate::layout::GroupBy::Project => crate::layout::GroupBy::Date,
            crate::layout::GroupBy::Date => crate::layout::GroupBy::Project,
        };
        crate::layout::Layout::update(cx, |layout| layout.group_by = Some(next));
        self.invalidate_list(cx);
    }

    /// The view menu's Group by submenu: persist `group` and regroup (a
    /// no-op when it already is the grouping).
    pub(crate) fn set_group_by(&mut self, group: crate::layout::GroupBy, cx: &mut Context<Self>) {
        if self.effective_group_by(cx) != group {
            self.toggle_group_by(cx);
        }
    }

    /// The view menu's Sort by submenu: persist the order and regroup.
    pub(crate) fn set_sort_by(&mut self, order: crate::layout::SortBy, cx: &mut Context<Self>) {
        if crate::layout::Layout::get(cx).sort_by == order {
            return;
        }
        crate::layout::Layout::update(cx, |layout| layout.sort_by = order);
        self.invalidate_list(cx);
    }

    /// The view menu's "Clear empty sessions": hide every session with no
    /// turns as one undoable batch, so Undo restores exactly what this hid.
    ///
    /// The open session is excluded here, not by [`SessionEntry::is_empty`]:
    /// `hidden` is a sticky, explicit flag with no auto-clear on activity, so
    /// sweeping a person's live draft would hide it permanently, past its
    /// first send. Provisional rows (turn count unknown) and provider-lane
    /// rows (turns live in the local record) are never swept either.
    pub(crate) fn clear_empty(&mut self, cx: &mut Context<Self>) {
        let active = self.active_id(cx);
        let cleared: Vec<String> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|entry| {
                !entry.hidden
                    && !entry.archived
                    && !entry.provisional
                    && entry.provider.lists_sessions()
                    && entry.is_empty()
                    && !active.as_deref().is_some_and(|id| id == entry.id)
            })
            .map(|entry| entry.id.clone())
            .collect();
        if cleared.is_empty() {
            self.overlays.update(cx, |overlays, cx| overlays.toast("No empty sessions", "Every listed session has at least one turn.", cx));
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


    /// Flip a group's disclosure: its id moves in or out of
    /// `closed_groups` and the layout is written. Every group, the unfiled
    /// one included, reads that set the same way.
    pub(crate) fn toggle_group(&mut self, id: String, cx: &mut Context<Self>) {
        crate::layout::Layout::update(cx, |layout| {
            if layout.closed_groups.iter().any(|g| g == &id) {
                layout.closed_groups.retain(|g| g != &id);
            } else {
                layout.closed_groups.push(id);
            }
        });
        self.invalidate_list(cx);
    }

    /// Flip a project group's "Show N more": its id moves in or out of
    /// `expanded_groups` (persisted exactly like `closed_groups`) and the
    /// list regroups. The group's own disclosure is untouched — an expanded
    /// group can stand closed and open folded.
    pub(crate) fn toggle_expanded(&mut self, id: String, cx: &mut Context<Self>) {
        crate::layout::Layout::update(cx, |layout| {
            if layout.expanded_groups.iter().any(|g| g == &id) {
                layout.expanded_groups.retain(|g| g != &id);
            } else {
                layout.expanded_groups.push(id);
            }
        });
        self.invalidate_list(cx);
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
        if !self.muse_slot.is_ready() {
            return;
        }
        let slot = self.muse_slot.clone();
        let wanted: Vec<String> = self
            .session_list.read(cx).sessions
            .iter()
            .filter(|entry| {
                entry.needs_title && !self.session_list.read(cx).titled.contains(&entry.id) && !self.is_side_session(&entry.id, cx)
            })
            .map(|entry| entry.id.clone())
            .take(MAX_TITLE_READS)
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.session_list.update(cx, |list, _| list.titled.extend(wanted.iter().cloned()));
        let work = move || {
            let Ok(lane) = slot.control() else { return Vec::new() };
            wanted
                .into_iter()
                .map(|session_id| {
                    let read = lane.send(provider::Command::ReadSession {
                        session_id: session_id.clone(),
                        metadata_only: false,
                    });
                    match read {
                        Ok(provider::Ack::SessionRead { first_shell_command, .. }) => (
                            session_id,
                            first_shell_command.as_deref().and_then(crate::sessions::shell_title),
                            false,
                        ),
                        Ok(_) => (session_id, None, false),
                        Err(error) => {
                            crate::baaz_log!("session/read for a title failed: {error}");
                            let stale = error.hint() == provider::RefusalHint::StaleHistory;
                            (session_id, None, stale)
                        }
                    }
                })
                .collect::<Vec<_>>()
        };
        self.wire_call(cx, work, |this, derived, cx| {
            for (session_id, title, stale) in derived {
                if stale {
                    this.session_list.update(cx, |list, _| list.titled.remove(&session_id));
                    continue;
                }
                let Some(title) = title else { continue };
                this.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
            }
        });
    }

    /// F10. A session's transcript may name it when nothing else does:
    /// the first thing the person said, never a command the agent ran. The
    /// first shell command is only the fallback for a session with no user
    /// text at all (a shell-only session).
    ///
    /// Free — the fold is in memory — and the one path that reaches a session
    /// whose history the server will not serve to a `session/read`. The
    /// answer lands in `derived_title`, so the next rejoin keeps the label
    /// a `turn/started` already gave the row (a fresh session reads "New
    /// session" with its reply as the preview until this runs).
    ///
    /// `view` is the session whose transcript speaks — the emitter of the
    /// event, which may be parked (docs/81 A F3).
    pub(super) fn title_from_transcript(&mut self, runtime: &Entity<SessionRuntime>, cx: &mut Context<Self>) {
        let session_id = runtime.read(cx).session_id.clone();
        // A handoff destination keeps the chain title: no rename from the
        // pack or anything after it (Y2a).
        if crate::sidebar::is_handoff_dest(&session_id, &self.session_list.read(cx).provider_sessions, &self.session_list.read(cx).overrides) {
            return;
        }
        // The row may not be in the list yet — `session/list` is a round-trip
        // and the transcript is already here — so the question is not "does the
        // row need a title" but "does this session have one".
        let has_title = self.session_list.read(cx).overrides.get(&session_id).is_some_and(|m| m.name.is_some() || m.derived_title.is_some())
            || self.session_list.read(cx).index.get(&session_id).and_then(IndexEntry::label).is_some();
        if has_title {
            return;
        }
        let view = runtime.read(cx);
        // Either form is cut the row's own way: overlong shell commands used
        // to fill the crumb and the row uncut.
        let title = view
            .first_user_title()
            .or_else(|| view.first_shell_title().map(|shell| crate::sidebar::one_line(&shell)));
        let Some(title) = title else { return };
        // Destinations return above, so no pack turn can reach here: only
        // a handoff destination ever has one, and any other session's own
        // words title it, pack-shaped or not (Y2a3).
        self.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
    }
}
