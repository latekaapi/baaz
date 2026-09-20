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
        // A wire fault belongs to the connection, not to a session, so the
        // transport raises it with no `sessionId` and the fold — which routes
        // by session — dropped it (finding `client-adapter-4`). The view that
        // is open is the one that has to show it, so it is filed here.
        let event = match event {
            MuseEvent::Notification { method, params, cursor, session_id: None }
                if method == muse_client::PROTOCOL_ERROR =>
            {
                MuseEvent::Notification {
                    method,
                    params,
                    cursor,
                    session_id: Some(self.session_id.clone()),
                }
            }
            other => other,
        };
        let mut unqueued: Option<String> = None;
        if let MuseEvent::Notification { method, params, session_id, .. } = &event {
            if session_id.as_deref().is_some_and(|id| id != self.session_id) {
                return;
            }
            match method.as_str() {
                "turn/started" => {
                    if let Some(turn_id) = params.get("turnId").and_then(|v| v.as_str()) {
                        // A start for a turn this view already saw complete
                        // is the re-attach replay (`session/resume` streams
                        // the suffix, which re-delivers the open turn's
                        // start), not new work: it folds below like any
                        // repeat, but it never marks running — otherwise a
                        // click that re-attaches an interrupted session
                        // shows a fresh `Working…` for a turn that is not
                        // running (item 3). Genuinely new work always
                        // carries a new turn id.
                        if !self.completed_turns.contains(turn_id) {
                            self.running = Some(Running { turn_id: turn_id.to_owned(), started: crate::clock::now_instant() });
                            self.submitting = false;
                            self.last_tick_secs = None;
                            self.start_ticker(cx);
                        }
                    }
                }
                "turn/completed" => {
                    let ours = params.get("turnId").and_then(|v| v.as_str());
                    if let Some(turn_id) = ours {
                        if self.completed_turns.len() >= super::MAX_COMPLETED_TURNS {
                            self.completed_turns.clear();
                        }
                        self.completed_turns.insert(turn_id.to_owned());
                    }
                    if self.running.as_ref().is_some_and(|r| Some(r.turn_id.as_str()) == ours) {
                        self.clear_running();
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
        // Route a reclaimed prompt by the intent captured when its row's
        // button was clicked — never by the fold's `commandId` echo, which
        // may be missing or arrive on another tick. The echo, when present,
        // is the same text and is drained so it cannot land twice.
        let restored = self.fold.take_restored_prompt(&self.session_id);
        if let Some(turn_id) = unqueued.as_deref() {
            if let Some(pending) = self.unqueueing.remove(turn_id) {
                let _ = restored;
                match pending.kind {
                    Unqueue::Remove => {}
                    Unqueue::Edit => self.restore_prompt(pending.text, cx),
                    Unqueue::Steer => self.steer_text(pending.text, cx),
                }
            } else if let Some(text) = restored {
                // Reclaimed by someone else (another window won the same
                // row): there is no intent to route by, so the words go back
                // in the composer rather than evaporating.
                self.restore_prompt(text, cx);
            }
        } else if let Some(text) = restored {
            // A retraction handed the prompt back; put it in the composer,
            // where it came from.
            self.restore_prompt(text, cx);
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
            let workspace = crate::projects::canonical_str(&self.workspace);
            records.push(search::FileRecord {
                path,
                session_id: self.session_id.clone(),
                kind: verb.to_owned(),
                // The folder's own name only: a session view knows its
                // workspace, not the projects store, and a rename is rare
                // enough not to be worth carrying the store down here.
                project: search::project_terms(Some(&workspace), None),
                workspace,
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

    /// Page the transcript in, oldest first, folding each page as it arrives.
    ///
    /// `session/resume` is what attaches; the history itself comes through
    /// `view/page` from the beginning of the view, which is the one path that
    /// is contiguous, ordered and bounded. `view/page` never replays
    /// `item/delta`, so a backfilled message arrives whole and the fold takes
    /// it that way. Each page folds in its own update, so frames interleave
    /// with the paging instead of waiting for the whole transcript; page 1
    /// already draws before page 2 is requested.
    /// A view opened for an existing session starts loading its history on
    /// its very first frame — never the new-session hero.
    /// Called synchronously on the open path before any paint (the resume
    /// ack and its `backfill` land frames later); the hero stays reachable
    /// only for proven-new sessions and for backfills that complete with
    /// zero turns.
    pub fn mark_history_loading(&mut self, cx: &mut Context<Self>) {
        self.loading_history = true;
        cx.notify();
    }

    pub fn backfill(&mut self, cx: &mut Context<Self>) {
        if self.backfill_running {
            // A chain is already in flight (the view was parked and reopened
            // mid-backfill): it keeps filling this same fold, so a second
            // chain would page everything twice.
            return;
        }
        self.loading_history = true;
        self.backfill_running = true;
        self.backfill_stale_retried = false;
        cx.notify();
        self.backfill_page(None, FIRST_PAGE_LIMIT, true, cx);
    }

    /// One page of a [`Self::backfill`] chain: fetch it on the background
    /// executor, fold it on the UI thread, then chain the next page. `first`
    /// marks the page the [`SessionEvent::HistoryReady`] cue fires after —
    /// the first, so the tail pins while later pages land.
    fn backfill_page(&mut self, cursor: Option<String>, limit: u32, first: bool, cx: &mut Context<Self>) {
        let Some(client) = self.wire_client(cx) else {
            self.loading_history = false;
            self.backfill_running = false;
            return;
        };
        let session_id = self.session_id.clone();
        crate::log::trace_mark("page-request");
        // Copies for the one permitted retry: the work closure below moves
        // its own.
        let retry_cursor = cursor.clone();
        self.wire_call(
            cx,
            move || {
                let page = client.view_page(&ViewPageParams {
                    session_id: session_id.clone(),
                    limit,
                    cursor: cursor.clone(),
                    direction: None,
                    anchor: None,
                });
                (session_id, page)
            },
            move |this, (session_id, result), cx| {
                match result {
                    Ok(page) => {
                        let events = page_events(&session_id, &page.events);
                        let n = events.len();
                        crate::log::trace_mark(&format!("page n={n}"));
                        for event in events {
                            this.fold.apply(event);
                        }
                        crate::log::trace_mark(&format!("folded n={n}"));
                        this.follow = true;
                        cx.notify();
                        if first {
                            // The cue for the tail pin, not for a swap: the
                            // view is already on screen.
                            crate::log::trace_mark("HistoryReady");
                            cx.emit(SessionEvent::HistoryReady);
                        }
                        match page.next_cursor {
                            Some(next) if n > 0 => this.backfill_page(Some(next), PAGE_LIMIT, false, cx),
                            _ => {
                                this.loading_history = false;
                                this.backfill_running = false;
                                this.follow = true;
                                cx.notify();
                            }
                        }
                    }
                    Err(error) => {
                        // A stale sidecar fails once, then reports: the
                        // resume that preceded this chain was the
                        // regenerating touch, so the same page usually serves
                        // on the second attempt — and only a second failure
                        // reaches the dialog.
                        if should_retry_stale_page(&error, this.backfill_stale_retried) {
                            crate::baaz_log!("backfill page hit a stale sidecar; retrying once");
                            this.backfill_stale_retried = true;
                            this.backfill_page(retry_cursor, limit, first, cx);
                            return;
                        }
                        if error.is_stale_sidecar() {
                            this.report(&error, cx);
                        } else {
                            // What arrived so far stays (the old
                            // whole-transcript backfill likewise kept its
                            // partial pages); the loading row stands down and
                            // the tail pins.
                            crate::baaz_log!("backfill page failed: {error}");
                        }
                        this.loading_history = false;
                        this.backfill_running = false;
                        this.follow = true;
                        cx.notify();
                        if first {
                            crate::log::trace_mark("HistoryReady");
                            cx.emit(SessionEvent::HistoryReady);
                        }
                    }
                }
            },
        );
    }
}

/// Whether a failed backfill page gets one more attempt:
/// exactly the stale-sidecar `-32603`, and only once per chain. A second
/// stale failure — and any other error — reports instead of looping.
fn should_retry_stale_page(error: &MuseError, retried: bool) -> bool {
    !retried && error.is_stale_sidecar()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stale_sidecar() -> MuseError {
        let object: muse_client::schema::ErrorObject = serde_json::from_value(serde_json::json!({
            "code": -32603,
            "message": "internal error: read materialized session view: stale sidecar generation",
            "data": {"kind": "internal"},
        }))
        .expect("error object decodes");
        MuseError::Rpc(Box::new(object))
    }

    fn other_internal() -> MuseError {
        let object: muse_client::schema::ErrorObject = serde_json::from_value(serde_json::json!({
            "code": -32603,
            "message": "internal error: something else broke",
            "data": {"kind": "internal"},
        }))
        .expect("error object decodes");
        MuseError::Rpc(Box::new(object))
    }

    /// Item 3: a `turn/started` for a turn this view already saw complete is
    /// the re-attach replay, not new work — clicking a session with an
    /// interrupted turn re-attaches (`session/resume` streams the suffix,
    /// re-delivering the turn's start), and the view must not mark running
    /// for it. A start with a new id still marks running, and its
    /// completion still stands the view down.
    #[gpui::test]
    fn a_redelivered_start_for_a_completed_turn_marks_nothing_running(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        vc.update(|window, cx| {
            let host = crate::session::SessionHost {
                provider_id: "echo".to_owned(),
                workspace: "/tmp/item3-probe".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::shot::CaptureToken::default(),
            };
            let view = cx.new(|cx| crate::session::SessionView::new("s-1".to_owned(), None, host, window, cx));
            let event = |method: &str, turn: &str| {
                muse_client::MuseEvent::Notification {
                    method: method.to_owned(),
                    params: serde_json::json!({"turnId": turn}),
                    cursor: None,
                    session_id: Some("s-1".to_owned()),
                }
            };
            // The interrupted turn completes, then its start is re-delivered
            // by a re-attach: still idle, never `Working`.
            view.update(cx, |view, cx| view.apply(event("turn/completed", "t-1"), cx));
            assert!(!view.read(cx).busy());
            view.update(cx, |view, cx| view.apply(event("turn/started", "t-1"), cx));
            assert!(!view.read(cx).busy(), "a completed turn's redelivered start is not work");
            // Genuinely new work still marks running, and its completion
            // still stands the view down.
            view.update(cx, |view, cx| view.apply(event("turn/started", "t-2"), cx));
            assert!(view.read(cx).busy(), "a new turn id still marks running");
            view.update(cx, |view, cx| view.apply(event("turn/completed", "t-2"), cx));
            assert!(!view.read(cx).busy());
        });
    }

    /// A stale page retries exactly once; any other error
    /// never retries.
    #[test]
    fn a_stale_page_retries_once_and_then_reports() {
        let stale = stale_sidecar();
        assert!(should_retry_stale_page(&stale, false));
        assert!(!should_retry_stale_page(&stale, true));
        assert!(!should_retry_stale_page(&other_internal(), false));
        assert!(!should_retry_stale_page(&MuseError::Closed, false));
    }

    // ------------------------------------------------- queued-row unqueues
    //
    // A stub `muse serve` that answers `turn/unqueue` and `turn/steer` (or
    // fails the steer on demand) and logs every request line, so the tests
    // below drive the real view — `unqueue()`, `apply()`, `steer_text()` —
    // against the real client with no network and no sign-in.
    use gpui::Entity;
    use std::sync::Arc as StdArc;
    use std::time::{Duration, Instant};

    const STUB_SERVE: &str = r#"#!/usr/bin/env python3
import sys, json
log = open(sys.argv[-1], "a", buffering=1)
fail_steer = "fail-steer" in sys.argv
for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    log.write(line + "\n")
    rid = req.get("id")
    if rid is None:
        continue
    method = req.get("method")
    params = req.get("params") or {}
    if method == "turn/steer" and fail_steer:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": "commandRejected: missing_run", "data": {"kind": "commandRejected", "reason": "missing_run"}}}
    elif method in ("turn/unqueue", "turn/steer"):
        tid = params.get("turnId", params.get("expectedTurnId"))
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"commandId": params.get("commandId"), "status": "accepted", "turnId": tid}}
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "result": {}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
"#;

    struct StubServe {
        dir: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    impl Drop for StubServe {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    impl StubServe {
        fn start(name: &str, extra: &[&str]) -> (muse_client::MuseClient, StubServe) {
            let dir = std::env::temp_dir().join(format!(
                "baaz-unqueue-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock")
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("stub dir");
            let prog = dir.join("stub-serve");
            std::fs::write(&prog, STUB_SERVE).expect("stub script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).expect("stub chmod");
            }
            let log = dir.join("wire.log");
            let mut args: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
            args.push(log.to_string_lossy().into_owned());
            let config = muse_client::MuseConfig {
                program: prog,
                trust_workspace: false,
                no_session_log: true,
                extra_args: args,
            };
            let client = muse_client::MuseClient::spawn(&config).expect("stub spawn");
            (client, StubServe { dir, log })
        }

        fn requests(&self) -> Vec<serde_json::Value> {
            let text = std::fs::read_to_string(&self.log).unwrap_or_default();
            text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
        }

        fn saw(&self, method: &str) -> Option<serde_json::Value> {
            self.requests().into_iter().find(|v| v.get("method").and_then(|m| m.as_str()) == Some(method))
        }

        /// The text parts a `turn/steer` request carried, in order.
        fn steer_texts(&self) -> Vec<String> {
            self.requests()
                .into_iter()
                .filter(|v| v.get("method").and_then(|m| m.as_str()) == Some("turn/steer"))
                .flat_map(|v| {
                    v.pointer("/params/input")
                        .and_then(|i| i.as_array())
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                .collect()
        }
    }

    fn unqueue_event(method: &str, params: serde_json::Value) -> muse_client::MuseEvent {
        muse_client::MuseEvent::Notification {
            method: method.to_owned(),
            params,
            cursor: None,
            session_id: Some("s-1".to_owned()),
        }
    }

    fn open_live_view(
        vc: &mut gpui::VisualTestContext,
        client: Option<muse_client::MuseClient>,
        workspace: &str,
    ) -> Entity<SessionView> {
        vc.update(|window, cx| {
            let host = crate::session::SessionHost {
                provider_id: "echo".to_owned(),
                workspace: workspace.to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::shot::CaptureToken::default(),
            };
            let client = client.map(StdArc::new);
            cx.new(|cx| crate::session::SessionView::new("s-1".to_owned(), client, host, window, cx))
        })
    }

    /// A turn running, one message queued behind it, its row's button clicked.
    fn queue_and_click(
        view: &Entity<SessionView>,
        vc: &mut gpui::VisualTestContext,
        why: Unqueue,
        queued_text: &str,
    ) {
        vc.update(|_, cx| {
            view.update(cx, |view, _| {
                view.fold.record_command("s-1", "c-q", queued_text);
                view.fold.record_queued("s-1", "t-q", "c-q", queued_text);
            });
            view.update(cx, |view, cx| {
                view.apply(unqueue_event("turn/started", serde_json::json!({"turnId": "t-run"})), cx)
            });
            assert!(view.read(cx).busy(), "the fixture turn is running");
            view.update(cx, |view, cx| view.unqueue("t-q", why, queued_text.to_owned(), cx));
        });
    }

    fn wait_for_wire(stub: &StubServe, vc: &mut gpui::VisualTestContext, method: &str) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            vc.run_until_parked();
            if stub.saw(method).is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for {method}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn pending_prompt_of(view: &Entity<SessionView>, vc: &mut gpui::VisualTestContext) -> Option<String> {
        vc.run_until_parked();
        vc.update(|_, cx| view.read(cx).pending_prompt.clone())
    }

    /// The reported turn: queue a message, Steer it, `turn/unqueued` lands —
    /// `turn/steer` goes out with the exact text, and nothing lands back in
    /// the composer.
    #[gpui::test]
    fn steer_sends_the_reclaimed_text(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-steer");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(
                    unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q", "commandId": "c-q"})),
                    cx,
                )
            })
        });
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(pending_prompt_of(&view, vc), None, "no duplicate in the composer");
        vc.update(|_, cx| assert_eq!(view.read(cx).banner, None));
    }

    /// The prime-suspect tick hazard: `turn/unqueued` without the `commandId`
    /// echo still steers, from the text captured at click time.
    #[gpui::test]
    fn steer_without_command_echo_still_sends(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer-noecho", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-steer-noecho");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q"})), cx)
            })
        });
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(pending_prompt_of(&view, vc), None, "no duplicate in the composer");
    }

    /// The failure path: the server rejects the steer — the words land back
    /// in the composer and the banner says why, instead of evaporating.
    #[gpui::test]
    fn rejected_steer_falls_back_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer-rejected", &["fail-steer"]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-steer-rejected");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(
                    unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q", "commandId": "c-q"})),
                    cx,
                )
            })
        });
        wait_for_wire(&stub, vc, "turn/steer");
        let deadline = Instant::now() + Duration::from_secs(15);
        let restored = loop {
            let found = pending_prompt_of(&view, vc);
            if found.is_some() || Instant::now() > deadline {
                break found;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(restored.as_deref(), Some("the queued words"));
        vc.update(|_, cx| assert!(view.read(cx).banner.is_some(), "the user sees why"));
    }

    /// The turn ended mid-reclaim: the steer has nowhere to go, so the words
    /// go back in the composer with the reason — synchronously, no wire.
    #[gpui::test]
    fn ended_turn_steer_falls_back_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, None, "/tmp/unqueue-steer-ended");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.steer_text("the queued words".to_owned(), cx))
        });
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the queued words"));
        vc.update(|_, cx| {
            let banner = view.read(cx).banner.clone().unwrap_or_default();
            assert!(banner.contains("ended"), "the banner says why, got: {banner}");
        });
    }

    /// Ordering hazard, both orders: the unqueue (without echo) and an
    /// unrelated retraction on different ticks never cross-contaminate — the
    /// queued text is steered and the retracted text is composed.
    #[gpui::test]
    fn unqueued_then_retraction_keeps_both(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("order-a", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-order-a");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, _| view.fold.record_command("s-1", "c-r", "the retracted words"));
            view.update(cx, |view, cx| {
                view.apply(unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q"})), cx)
            })
        });
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(
                    unqueue_event("turn/retracted", serde_json::json!({"turnId": "t-r", "commandId": "c-r"})),
                    cx,
                )
            })
        });
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the retracted words"));
    }

    #[gpui::test]
    fn retraction_then_unqueued_keeps_both(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("order-b", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-order-b");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, _| view.fold.record_command("s-1", "c-r", "the retracted words"));
            view.update(cx, |view, cx| {
                view.apply(
                    unqueue_event("turn/retracted", serde_json::json!({"turnId": "t-r", "commandId": "c-r"})),
                    cx,
                )
            })
        });
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the retracted words"));
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q"})), cx)
            })
        });
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(
            pending_prompt_of(&view, vc).as_deref(),
            Some("the retracted words"),
            "the steer leaves the composer's text alone"
        );
    }

    /// Edit still restores to the composer — via the captured text, so a
    /// missing echo cannot lose it either — and never steers.
    #[gpui::test]
    fn edit_restores_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("edit", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-edit");
        queue_and_click(&view, vc, Unqueue::Edit, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q"})), cx)
            })
        });
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the queued words"));
        vc.run_until_parked();
        std::thread::sleep(Duration::from_millis(300));
        vc.run_until_parked();
        assert!(stub.saw("turn/steer").is_none(), "edit never steers");
    }

    /// Remove still drops silently: nothing in the composer, no banner, the
    /// row gone, nothing on the wire.
    #[gpui::test]
    fn remove_drops_silently(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("remove", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, Some(client), "/tmp/unqueue-remove");
        queue_and_click(&view, vc, Unqueue::Remove, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(
                    unqueue_event("turn/unqueued", serde_json::json!({"turnId": "t-q", "commandId": "c-q"})),
                    cx,
                )
            })
        });
        assert_eq!(pending_prompt_of(&view, vc), None);
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).banner, None);
            let queued = view.read(cx).fold.side("s-1").map(|s| s.queued.len()).unwrap_or(999);
            assert_eq!(queued, 0, "the row leaves the strip");
        });
        vc.run_until_parked();
        std::thread::sleep(Duration::from_millis(300));
        vc.run_until_parked();
        assert!(stub.saw("turn/steer").is_none(), "remove never steers");
    }
}
