//! One wire event at a time: fold it, then react to the few that are
//! more than transcript.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ---------------------------------------------------------------- events

    /// Fold one wire event and react to the few that are more than transcript.
    pub fn apply(&mut self, event: MuseEvent, cx: &mut Context<Self>) {
        if let MuseEvent::Closed(_) = &event {
            cx.emit(SessionEvent::Closed);
            self.running = None;
            self.submitting = false;
            self.ticker = None;
            cx.notify();
            return;
        }
        let mut unqueued: Option<String> = None;
        if let MuseEvent::Notification { method, params, session_id, .. } = &event {
            if session_id.as_deref().is_some_and(|id| id != self.session_id) {
                return;
            }
            match method.as_str() {
                "turn/started" => {
                    if let Some(turn_id) = params.get("turnId").and_then(|v| v.as_str()) {
                        self.running = Some(Running { turn_id: turn_id.to_owned(), started: crate::clock::now_instant() });
                        self.submitting = false;
                        self.last_tick_secs = None;
                        self.start_ticker(cx);
                    }
                }
                "turn/completed" => {
                    let ours = params.get("turnId").and_then(|v| v.as_str());
                    if self.running.as_ref().is_some_and(|r| Some(r.turn_id.as_str()) == ours) {
                        self.running = None;
                        self.ticker = None;
                        self.last_tick_secs = None;
                    }
                    self.submitting = false;
                    if let Some(turn_id) = ours {
                        self.plan_completed(turn_id, cx);
                    }
                    self.turn_failure(params, cx);
                }
                "turn/unqueued" => {
                    unqueued = params.get("turnId").and_then(|v| v.as_str()).map(str::to_owned);
                }
                _ => {}
            }
        }
        // View state the fold does not report: prompt text handed back below,
        // and whether a turn is running, all change what the frame shows.
        let was_running = self.running.is_some();
        let was_submitting = self.submitting;
        let completed_ours = matches!(&event, MuseEvent::Notification { method, .. } if method == "turn/completed");
        let changed = !self.fold.apply(event).is_empty();
        if completed_ours {
            self.record_created_files(cx);
        }
        // Restore a retracted prompt the moment the fold hands it back — unless
        // the unqueue was a Remove (the text is meant to be gone) or a Steer
        // (the text is going straight back out on the wire).
        let restored = self.fold.take_restored_prompt(&self.session_id);
        let unqueued_kind = unqueued.as_deref().and_then(|id| self.unqueueing.remove(id));
        if let Some(text) = restored {
            match unqueued_kind {
                Some(Unqueue::Remove) => {}
                Some(Unqueue::Steer) => self.steer_text(text, cx),
                _ => self.restore_prompt(text, cx),
            }
        }
        // Notify less (P1): a streaming delta that changed nothing visible
        // must not rebuild the whole transcript. Unchanged deltas arrive
        // constantly while a reply streams; only fold changes and view-state
        // changes earn a frame.
        let mut view_changed = unqueued.is_some()
            || was_running != self.running.is_some()
            || was_submitting != self.submitting;
        if changed {
            self.follow = true;
            view_changed = true;
        }
        // Every event can start or end a countdown; the clock is started and
        // stopped in one place rather than by each event that might matter.
        self.observe_clocks(cx);
        if std::mem::take(&mut self.refresh_pending) {
            self.refresh_pending_now(cx);
            view_changed = true;
        }
        if view_changed {
            cx.notify();
        }
    }

    /// A completed turn's created files, into the search index's `files_fts`.
    ///
    /// "Files we created" is what the search palette means by it: the
    /// workspace-relative targets of this session's write/edit tool calls.
    /// The scan over the in-memory fold is cheap; the sqlite insert runs on
    /// the background executor.
    pub(super) fn record_created_files(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.fold.session(&self.session_id) else { return };
        let mut records = Vec::new();
        for block in session.turns.iter().flat_map(|turn| turn.blocks()) {
            let aui_protocol::Block::ToolCall { kind, target, .. } = block else { continue };
            let Some(verb) = search::created_target(kind, target) else { continue };
            let Some(path) = search::relativize(&self.workspace, target) else { continue };
            records.push(search::FileRecord {
                path,
                session_id: self.session_id.clone(),
                kind: verb.to_owned(),
            });
        }
        if records.is_empty() {
            return;
        }
        let work = move || {
            if let Ok(connection) = search::open() {
                let _ = search::record_files(&connection, &records);
            }
        };
        self.wire_call(cx, work, |_this, (), _cx| {});
    }

    /// This workspace's prompt history, read off the UI thread (finding P4).
    ///
    /// Called once when the view opens; the whole-file JSON round-trip never
    /// runs on the UI thread any more.
    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let key = self.workspace_key.clone();
        self.wire_call(cx, move || history::read(&key), |this: &mut Self, entries, cx| {
            this.history.set(entries);
            cx.notify();
        });
    }

    /// Append a sent prompt to the history off the UI thread (finding P4).
    ///
    /// The cursor updates when the write lands; a prompt sent before that
    /// still reached the wire first — history is a convenience, not a record.
    pub(super) fn append_history(&mut self, text: String, cx: &mut Context<Self>) {
        let key = self.workspace_key.clone();
        self.wire_call(cx, move || history::append(&key, &text), |this: &mut Self, entries, cx| {
            this.history.set(entries);
            cx.notify();
        });
    }

    /// A `turn/completed` with `terminal: "failed"`: the fold already drew the
    /// error card, so all that is left is deciding whether the failure means
    /// the credential is gone (spec §3.2).
    pub(super) fn turn_failure(&mut self, params: &serde_json::Value, cx: &mut Context<Self>) {
        let Some(error) = params.get("error") else { return };
        let kind = error.get("kind").and_then(|v| v.as_str());
        let message = error.get("message").and_then(|v| v.as_str()).unwrap_or_default();
        if conn::looks_like_signed_out(kind, message) {
            cx.emit(SessionEvent::SignedOut { message: message.to_owned() });
        }
    }

    /// A retraction or an unqueue handed the prompt back; put it in the
    /// composer, where it came from.
    ///
    /// `set_value` needs a `Window`, which a wire event never holds, so the
    /// text is parked here and the next frame picks it up — the same trick the
    /// gallery's mock uses for deferred focus.
    pub(super) fn restore_prompt(&mut self, text: String, cx: &mut Context<Self>) {
        self.pending_prompt = Some(text);
        cx.notify();
    }

    /// Page the whole transcript in, oldest first, and fold it.
    ///
    /// `session/resume` is what attaches; the history itself comes through
    /// `view/page` from the beginning of the view, which is the one path that
    /// is contiguous, ordered and bounded. `view/page` never replays
    /// `item/delta`, so a backfilled message arrives whole and the fold takes
    /// it that way.
    pub fn backfill(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else { return };
        self.loading_history = true;
        cx.notify();
        let session_id = self.session_id.clone();
        self.wire_call(cx, move || page_all(&client, &session_id), |this, events, cx| {
            for event in events {
                this.fold.apply(event);
            }
            this.loading_history = false;
            this.follow = true;
            cx.notify();
            // The deferred switch's cue: the application swaps this view
            // in now, instead of having flashed the empty state (C2).
            cx.emit(SessionEvent::HistoryReady);
        });
    }
}
