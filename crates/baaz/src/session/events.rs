//! What a session's history and prompts do besides the transcript: the
//! created-files index, the prompt history, restored prompts, and paging a
//! lane's history in.
//!
//! Part of [`SessionView`]; see [`crate::session`] for what
//! the entity owns and why these are its own files.

use super::*;

impl SessionView {
    // ---------------------------------------------------------------- events

    /// A completed turn's created files, into the search index's `files_fts`.
    ///
    /// "Files we created" is what the search palette means by it: the
    /// workspace-relative targets of this session's write/edit tool calls.
    /// The scan over the in-memory fold is cheap; the sqlite insert runs on
    /// the background executor.
    pub(super) fn record_created_files(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.store.session() else { return };
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

}

impl SessionView {
    /// Page a lane's history in, oldest first: the backfill for a session
    /// its provider opened without items (Muse resumes metadata-only and
    /// pages, so a long transcript draws page by page instead of after one
    /// huge answer). Each page's deltas arrive on the lane in order with the
    /// live stream; the ack only says where the next page starts. The first
    /// page cues the tail pin ([`SessionEvent::HistoryReady`]); a stale
    /// history is retried once, as the provider asks.
    pub fn backfill_lane(&mut self, cx: &mut Context<Self>) {
        if self.backfill_running {
            return;
        }
        self.loading_history = true;
        self.backfill_running = true;
        self.backfill_stale_retried = false;
        cx.notify();
        self.backfill_lane_page(None, FIRST_PAGE_LIMIT, true, cx);
    }

    fn backfill_lane_page(&mut self, cursor: Option<String>, limit: u32, first: bool, cx: &mut Context<Self>) {
        crate::log::trace_mark("page-request");
        let command = ProviderCommand::PageTranscript {
            session_id: self.session_id.clone(),
            after: cursor.clone(),
            limit,
            backward: false,
        };
        self.backfill_first = first;
        self.provider_send(command, cx, move |this, result, cx| {
            // The page itself lands on the lane, its deltas first and then
            // `TranscriptPaged` (see `lane_page_landed`): the ack only
            // reports a page that failed.
            let next = match result {
                Ok(_) => return,
                Err(error) => {
                    if error.hint() == provider::RefusalHint::StaleHistory && !this.backfill_stale_retried {
                        crate::baaz_log!("backfill page hit a stale history; retrying once");
                        this.backfill_stale_retried = true;
                        this.backfill_lane_page(cursor, limit, first, cx);
                        return;
                    }
                    if error.hint() == provider::RefusalHint::StaleHistory {
                        this.report_provider_error(&error, cx);
                    } else {
                        // What arrived so far stays; the loading row stands
                        // down and the tail pins.
                        crate::baaz_log!("backfill page failed: {error}");
                    }
                    None
                }
            };
            this.lane_page_landed(next, cx);
        });
    }

    /// A page of the lane's backfill has landed in the transcript: cue the
    /// tail pin after the first, then page on from `next`, or stand the
    /// loading row down at the end.
    pub(super) fn lane_page_landed(&mut self, next: Option<String>, cx: &mut Context<Self>) {
        if !self.backfill_running {
            return;
        }
        self.follow = true;
        if std::mem::take(&mut self.backfill_first) {
            crate::log::trace_mark("HistoryReady");
            cx.emit(SessionEvent::HistoryReady);
        }
        match next {
            Some(next) => self.backfill_lane_page(Some(next), PAGE_LIMIT, false, cx),
            None => {
                self.loading_history = false;
                self.backfill_running = false;
                let turns = self.session().map_or(0, |session| session.turns.len());
                crate::baaz_log!("history paged in: session={} turns={turns}", self.session_id);
            }
        }
        cx.notify();
    }
}

/// Capture loaders for the render-cache bench (`render.rs`), kept here so
/// the wire types stay in the files that already speak them (the seam
/// ratchet, `tests/seam_ratchet.rs`).
/// Capture loaders for the render-cache bench (`render.rs`): a capture's
/// raw lines, and one turn of it renamed, folded through the same
/// translation a Muse lane uses.
#[cfg(test)]
pub(crate) mod bench_support {
    use std::path::{Path, PathBuf};

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp").join(name)
    }

    /// The whole capture as the neutral events a lane would deliver.
    pub(crate) fn capture_events(name: &str) -> (provider_muse::replay::Folder, Vec<provider::ProviderEvent>) {
        let mut folder = provider_muse::replay::Folder::new();
        let text = std::fs::read_to_string(fixture(name)).expect("capture is readable");
        let events = text.lines().filter_map(|line| line.strip_prefix("<-- ")).flat_map(|body| folder.fold_line(body)).collect();
        (folder, events)
    }

    /// The raw `<--` bodies of one turn of a capture.
    pub(crate) fn last_turn_lines(name: &str, turn: &str) -> Vec<String> {
        let text = std::fs::read_to_string(fixture(name)).expect("capture is readable");
        let needle = format!("\"{turn}\"");
        text.lines().filter_map(|line| line.strip_prefix("<-- ")).filter(|body| body.contains(&needle)).map(str::to_owned).collect()
    }

    /// `lines` as a new turn `n` (every id the fold keys on made unique),
    /// folded on `folder`.
    pub(crate) fn renamed_turn(folder: &mut provider_muse::replay::Folder, lines: &[String], n: usize) -> Vec<provider::ProviderEvent> {
        lines
            .iter()
            .enumerate()
            .flat_map(|(j, body)| {
                let mut body = body
                    .replace("t-stress-300", &format!("t-bench-{n}"))
                    .replace("i-stress-", &format!("i-bench-{n}-"))
                    .replace("call_stress_300", &format!("call_bench_{n}"));
                if let Some(at) = body.find("\"v:stress:") {
                    let end = body[at + 1..].find('"').map(|e| at + 1 + e).unwrap_or(at);
                    body.replace_range(at..=end, &format!("\"v:bench:{n}:{j}\""));
                }
                folder.fold_line(&body)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    //! The queue strip's reclaims, steering and the billing guard, driven on
    //! a Muse lane over a stub `muse serve` that logs every request: the
    //! view's commands reach the real hub and client, and what the hub would
    //! report back (a queued row, a reclaimed prompt) is folded in as the
    //! neutral events it sends. No network, no sign-in, no model.
    use super::*;
    use gpui::Entity;
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
    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"experimentalApi": False, "grantedCapabilities": ["userShell"], "museHome": "/tmp/stub", "platformFamily": "unix", "platformOs": "macos", "schema": {"fingerprint": "stub", "version": 1}, "serverInfo": {"name": "stub-muse", "version": "1.4.11"}, "userAgent": "stub"}}
    elif method == "session/resume":
        session = {"sessionId": params.get("sessionId"), "path": "", "status": "idle", "activeTurnId": None, "createdAt": "2026-10-16T00:00:00Z", "updatedAt": "2026-10-16T00:00:00Z", "workspaceRoot": None, "providerId": None, "modelId": None, "turnCount": 1, "forkedFrom": None}
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"session": session, "viewCursor": "v:1", "history": {"mode": "none", "items": None, "noneReason": "excluded"}, "pendingRequests": []}}
    elif method == "turn/steer" and fail_steer:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": "commandRejected: missing_run", "data": {"kind": "commandRejected", "reason": "missing_run"}}}
    elif method == "turn/start":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"commandId": params.get("commandId"), "disposition": "started", "startedNewTurn": True, "status": "accepted", "turnId": "t-" + str(params.get("commandId"))}}
    elif method == "session/setApprovalMode":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"commandId": params.get("commandId"), "status": "accepted", "applyOutcome": "completed", "effectiveMode": {"mode": params.get("mode"), "source": "approvalReconfigure", "lastCommandId": params.get("commandId")}}}
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
                std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos()
            ));
            std::fs::create_dir_all(&dir).expect("stub dir");
            let prog = dir.join("stub-serve");
            std::fs::write(&prog, STUB_SERVE).expect("stub script");
            {
                use std::os::unix::fs::PermissionsExt as _;
                std::fs::set_permissions(&prog, std::fs::Permissions::from_mode(0o755)).expect("stub chmod");
            }
            let log = dir.join("wire.log");
            let mut args: Vec<String> = extra.iter().map(|s| (*s).to_owned()).collect();
            args.push(log.to_string_lossy().into_owned());
            let config = muse_client::MuseConfig { program: prog, trust_workspace: false, no_session_log: true, extra_args: args };
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
                .flat_map(|v| v.pointer("/params/input").and_then(|i| i.as_array()).cloned().unwrap_or_default())
                .filter_map(|p| p.get("text").and_then(|t| t.as_str()).map(str::to_owned))
                .collect()
        }

        /// The modes `session/setApprovalMode` was asked for, in order.
        fn modes(&self) -> Vec<String> {
            self.requests()
                .into_iter()
                .filter(|v| v.get("method").and_then(|m| m.as_str()) == Some("session/setApprovalMode"))
                .filter_map(|v| v.pointer("/params/mode").and_then(|m| m.as_str()).map(str::to_owned))
                .collect()
        }
    }

    /// A view on a Muse lane of a hub over the stub, attached to `s-1`.
    fn open_live_view(vc: &mut gpui::VisualTestContext, client: muse_client::MuseClient, workspace: &str) -> Entity<SessionView> {
        use provider::ProviderAdapter as _;
        let hub = provider_muse::MuseHub::from_client(client);
        let mut lane = hub.lane();
        lane.connect(&crate::conn::connect_info()).expect("the stub shakes hands");
        let provider = provider::Provider::new(lane);
        provider
            .send(provider::Command::ResumeSession {
                request_id: "r-1".into(),
                session_id: "s-1".into(),
                cursor: None,
                metadata_only: true,
                mode: None,
            })
            .expect("the stub resumes");
        let (provider, events) = crate::conn::gate_quiet(provider);
        vc.update(|window, cx| {
            let host = crate::session::SessionHost {
                provider_id: "meta".to_owned(),
                workspace: workspace.to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::shot::CaptureToken::default(),
                terminal_host: None,
            };
            cx.new(|cx| SessionView::new_on_provider("s-1".to_owned(), provider, events, host, window, cx))
        })
    }

    /// What the hub reports for `s-1`, folded into the view.
    fn report(view: &Entity<SessionView>, vc: &mut gpui::VisualTestContext, events: Vec<provider::ProviderEvent>) {
        vc.update(|_, cx| view.update(cx, |view, cx| view.fold_provider_events(events, cx)));
    }

    /// The hub's report of a reclaimed queued turn (`t-q`); `echoed` is
    /// whether the server's `turn/unqueued` carried the command id back.
    fn unqueued(echoed: bool) -> provider::ProviderEvent {
        provider::ProviderEvent::PromptRestored {
            session_id: "s-1".into(),
            turn_id: Some("t-q".into()),
            text: if echoed { "the queued words".into() } else { String::new() },
        }
    }

    /// The hub's report of a retracted turn handing its words back.
    fn retracted(text: &str) -> provider::ProviderEvent {
        provider::ProviderEvent::PromptRestored { session_id: "s-1".into(), turn_id: None, text: text.into() }
    }

    /// A turn running, one message queued behind it, its row's button clicked.
    fn queue_and_click(view: &Entity<SessionView>, vc: &mut gpui::VisualTestContext, why: Unqueue, queued_text: &str) {
        let mut side = provider::facts::SessionSide::default();
        side.queued.push(provider::facts::QueuedTurn { turn_id: "t-q".into(), request_id: "c-q".into() });
        vc.update(|_, cx| view.update(cx, |view, _| view.remember_command("c-q", queued_text)));
        report(
            view,
            vc,
            vec![
                provider::ProviderEvent::SessionFacts { session_id: "s-1".into(), side },
                provider::ProviderEvent::TurnRunning { session_id: "s-1".into(), turn_id: "t-run".into() },
            ],
        );
        vc.update(|_, cx| {
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

    /// The reported turn: queue a message, Steer it, the unqueue lands —
    /// `turn/steer` goes out with the exact text, and nothing lands back in
    /// the composer.
    #[gpui::test]
    fn steer_sends_the_reclaimed_text(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-steer");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![unqueued(true)]);
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(pending_prompt_of(&view, vc), None, "no duplicate in the composer");
        vc.update(|_, cx| assert_eq!(view.read(cx).banner, None));
    }

    /// The tick hazard: an unqueue without the command echo still steers,
    /// from the text captured at click time.
    #[gpui::test]
    fn steer_without_command_echo_still_sends(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer-noecho", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-steer-noecho");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![unqueued(false)]);
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(pending_prompt_of(&view, vc), None, "no duplicate in the composer");
    }

    /// The server rejects the steer: the words land back in the composer
    /// and the banner says why, instead of evaporating.
    #[gpui::test]
    fn rejected_steer_falls_back_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("steer-rejected", &["fail-steer"]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-steer-rejected");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![unqueued(true)]);
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
        vc.update(|_, cx| assert!(view.read(cx).banner.is_some(), "the person sees why"));
    }

    /// The turn ended mid-reclaim: the steer has nowhere to go, so the words
    /// go back in the composer with the reason — synchronously, no wire.
    #[gpui::test]
    fn ended_turn_steer_falls_back_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, _stub) = StubServe::start("steer-ended", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-steer-ended");
        vc.update(|_, cx| view.update(cx, |view, cx| view.steer_text("the queued words".to_owned(), cx)));
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the queued words"));
        vc.update(|_, cx| {
            let banner = view.read(cx).banner.clone().unwrap_or_default();
            assert!(banner.contains("ended"), "the banner says why, got: {banner}");
        });
    }

    /// Ordering hazard, both orders: the unqueue (without echo) and an
    /// unrelated retraction never cross-contaminate — the queued text is
    /// steered and the retracted text is composed.
    #[gpui::test]
    fn unqueued_then_retraction_keeps_both(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("order-a", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-order-a");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![unqueued(false)]);
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        report(&view, vc, vec![retracted("the retracted words")]);
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the retracted words"));
    }

    #[gpui::test]
    fn retraction_then_unqueued_keeps_both(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("order-b", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-order-b");
        queue_and_click(&view, vc, Unqueue::Steer, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![retracted("the retracted words")]);
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the retracted words"));
        report(&view, vc, vec![unqueued(false)]);
        wait_for_wire(&stub, vc, "turn/steer");
        assert_eq!(stub.steer_texts(), vec!["the queued words".to_owned()]);
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the retracted words"), "the steer leaves the composer alone");
    }

    /// Edit restores to the composer — via the captured text, so a missing
    /// echo cannot lose it — and never steers.
    #[gpui::test]
    fn edit_restores_to_composer(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("edit", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-edit");
        queue_and_click(&view, vc, Unqueue::Edit, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(&view, vc, vec![unqueued(false)]);
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("the queued words"));
        std::thread::sleep(Duration::from_millis(300));
        vc.run_until_parked();
        assert!(stub.saw("turn/steer").is_none(), "edit never steers");
    }

    /// Remove drops silently: nothing in the composer, no banner, the row
    /// gone, nothing more on the wire.
    #[gpui::test]
    fn remove_drops_silently(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("remove", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/unqueue-remove");
        queue_and_click(&view, vc, Unqueue::Remove, "the queued words");
        wait_for_wire(&stub, vc, "turn/unqueue");
        report(
            &view,
            vc,
            vec![
                unqueued(true),
                provider::ProviderEvent::SessionFacts { session_id: "s-1".into(), side: Default::default() },
            ],
        );
        assert_eq!(pending_prompt_of(&view, vc), None);
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).banner, None);
            assert_eq!(view.read(cx).store.side().queued.len(), 0, "the row leaves the strip");
        });
        std::thread::sleep(Duration::from_millis(300));
        vc.run_until_parked();
        assert!(stub.saw("turn/steer").is_none(), "remove never steers");
    }

    /// "Send anyway" is the send: the words the pay-as-you-go guard held back
    /// leave on the press, not on a second Enter (2026-10-17).
    #[gpui::test]
    fn send_anyway_sends_the_held_back_message(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("send-anyway", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/send-anyway");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.set_tier_banner(
                    Some(crate::session::TierBanner { text: "payg".to_owned(), blocking: true, checking: false }),
                    cx,
                );
                view.send_text("bill me".to_owned(), cx);
            })
        });
        assert_eq!(pending_prompt_of(&view, vc).as_deref(), Some("bill me"), "the guard holds the words back");
        assert!(stub.saw("turn/start").is_none(), "nothing left while the guard blocks");
        vc.update(|window, cx| view.update(cx, |view, cx| view.send_anyway(window, cx)));
        wait_for_wire(&stub, vc, "turn/start");
        let start = stub.saw("turn/start").expect("the press sent a turn");
        assert!(start.to_string().contains("bill me"), "the held-back words are the turn: {start}");
    }

    /// Quick picks never interleave on the wire, and the latest wins: the
    /// one made while another was in flight waits; one superseded while
    /// waiting never goes out.
    #[gpui::test]
    fn quick_mode_picks_run_one_at_a_time_and_the_latest_wins(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (client, stub) = StubServe::start("mode-serial", &[]);
        let vc = cx.add_empty_window();
        let view = open_live_view(vc, client, "/tmp/mode-serial");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.set_mode(PermissionMode::AllowAll, cx);
                view.set_mode(PermissionMode::PromptUnmatched, cx);
                view.set_mode(PermissionMode::DenyUnmatched, cx);
            })
        });
        let deadline = Instant::now() + Duration::from_secs(15);
        while stub.modes().len() < 2 {
            vc.run_until_parked();
            assert!(Instant::now() < deadline, "timed out: {:?}", stub.modes());
            std::thread::sleep(Duration::from_millis(10));
        }
        std::thread::sleep(Duration::from_millis(200));
        vc.run_until_parked();
        assert_eq!(stub.modes(), ["allowAll", "denyUnmatched"], "the superseded Ask pick never went out");
    }
}
