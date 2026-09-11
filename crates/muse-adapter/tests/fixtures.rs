//! Replay every wire capture through the fold and snapshot the result.
//!
//! `fixtures/msp/*.jsonl` are the ground truth: each line is `--> <json>`
//! (client→server) or `<-- <json>` (server→client). Only the server→client
//! lines are fed to the fold; the client→server lines are covered by
//! `muse-client`'s own round-trip test.
//!
//! Snapshots are plain pretty-printed JSON checked in under `tests/snapshots/`.
//! Regenerate them with `UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`, and
//! read the diff before committing it — a changed snapshot is a changed
//! transcript.

use std::path::{Path, PathBuf};

use muse_adapter::MuseFold;
use muse_client::{frame, MuseEvent};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp")
}

fn snapshots_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/snapshots")
}

fn captures() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .expect("fixtures/msp is readable")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty(), "no captures found in {}", fixtures_dir().display());
    paths
}

/// Fold one capture's server→client lines.
fn replay(path: &Path) -> MuseFold {
    let text = std::fs::read_to_string(path).expect("capture is readable");
    let mut fold = MuseFold::new();
    for (number, line) in text.lines().enumerate() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let parsed = frame::parse_line(body)
            .unwrap_or_else(|err| panic!("{}:{}: {err}", path.display(), number + 1));
        let Some(frame) = parsed else { continue };
        if let Some(event) = MuseEvent::from_frame(frame) {
            fold.apply(event);
        }
    }
    fold
}

/// Everything the fold produced for one capture, in a stable shape.
fn snapshot(fold: &MuseFold) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = fold
        .session_ids()
        .map(|id| {
            serde_json::json!({
                "sessionId": id,
                "session": fold.session(id).expect("session exists"),
                "side": fold.side(id).expect("side state exists"),
            })
        })
        .collect();
    serde_json::Value::Array(sessions)
}

#[test]
fn every_capture_folds_to_its_snapshot() {
    let update = std::env::var_os("UPDATE_SNAPSHOTS").is_some();
    let mut failures = Vec::new();
    for path in captures() {
        let stem = path.file_stem().expect("named file").to_string_lossy().into_owned();
        let actual = snapshot(&replay(&path));
        let actual = serde_json::to_string_pretty(&actual).expect("snapshot serializes");
        let expected_path = snapshots_dir().join(format!("{stem}.json"));
        if update {
            std::fs::create_dir_all(snapshots_dir()).expect("snapshots dir");
            std::fs::write(&expected_path, format!("{actual}\n")).expect("snapshot written");
            continue;
        }
        let Ok(expected) = std::fs::read_to_string(&expected_path) else {
            failures.push(format!(
                "{stem}: no snapshot at {} — run UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter",
                expected_path.display()
            ));
            continue;
        };
        if expected.trim() != actual.trim() {
            failures.push(format!("{stem}: folded session differs from its snapshot"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn no_capture_needs_a_generic_fallback() {
    // The phase-1 gate: every item kind in every capture folds to a real card.
    // `workflow` is the only kind allowed to fall back (`reminderChild` is
    // dropped by the fold before it could), and no capture contains one.
    for path in captures() {
        let fold = replay(&path);
        for id in fold.session_ids() {
            let session = fold.session(id).expect("session exists");
            for turn in &session.turns {
                for block in turn.blocks() {
                    if let aui_protocol::Block::Generic { kind, .. } = block {
                        assert!(
                            matches!(kind.as_str(), "workflow" | "reminderChild"),
                            "{}: item kind {kind} fell back to a generic card",
                            path.display()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn every_capture_produces_at_least_one_turn() {
    for path in captures() {
        // `transcript-account.jsonl` is protocol evidence for the account
        // surface, not a session transcript: no `session/start` ever opens,
        // so there is nothing to turn. It stays covered by the snapshot test
        // (which pins that it folds to nothing) and by `muse-client`'s
        // round-trip test (which types every one of its frames).
        // Neither carries a turn on purpose: `transcript-account.jsonl` is
        // protocol evidence for the account surface, and
        // `synthetic-modelrouteunserved.jsonl` exists only to pin a
        // side-state marker that fires with no turn ever open.
        if path.file_name().is_some_and(|name| {
            name == "transcript-account.jsonl" || name == "synthetic-modelrouteunserved.jsonl"
        }) {
            continue;
        }
        let fold = replay(&path);
        let turns: usize = fold
            .session_ids()
            .filter_map(|id| fold.session(id))
            .map(|session| session.turns.len())
            .sum();
        assert!(turns > 0, "{} folded to nothing", path.display());
    }
}

#[test]
fn a_server_request_and_its_notification_fold_once() {
    // `userInput/request` (a real JSON-RPC request) and `userInput/requested`
    // (the notification) carry identical params; the fold must not draw two
    // question cards.
    let fold = replay(&fixtures_dir().join("transcript-real.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let session = fold.session(&id).expect("session exists");
    let questions = session
        .turns
        .iter()
        .flat_map(|turn| turn.blocks())
        .filter(|block| matches!(block, aui_protocol::Block::Question { .. }))
        .count();
    assert_eq!(questions, 1, "the request and its notification folded twice");
}

#[test]
fn a_retraction_removes_the_turn_and_hands_the_prompt_back() {
    // `transcript-wire.jsonl` queues a second turn, unqueues it, then interrupts
    // the first with a retract. Both the user turn and the assistant turn must
    // leave the transcript, and the composer must get its text back.
    let path = fixtures_dir().join("transcript-wire.jsonl");
    let text = std::fs::read_to_string(&path).expect("capture is readable");
    let mut fold = MuseFold::new();
    let session = "01a081ee-3995-7820-b7d4-53491ce72f8d";
    let retracted_command = "01a081ee-70bb-719f-a35e-85cc42903110";
    // The app records what it sent; the wire never gives the text back.
    fold.record_command(session, retracted_command, "one");
    fold.record_queued(session, "01a081ee-70cd-7e64-b362-5922d6da6b51", "01a081ee-70cd-7e64-b362-5922d6da6b51", "two");

    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Some(frame) = frame::parse_line(body).expect("frame parses") else { continue };
        if let Some(event) = MuseEvent::from_frame(frame) {
            fold.apply(event);
        }
    }

    let side = fold.side(session).expect("side state exists");
    assert!(side.queued.is_empty(), "the unqueued turn stayed in the strip");
    assert_eq!(fold.take_restored_prompt(session).as_deref(), Some("one"));

    let folded = fold.session(session).expect("session exists");
    assert!(
        !folded.turns.iter().any(|turn| turn.id() == retracted_command),
        "the retracted turn is still in the transcript"
    );
    assert!(
        !folded.turns.iter().any(|turn| matches!(turn, aui_protocol::Turn::User { .. })),
        "the retracted turn's user message is still in the transcript"
    );
}

/// **client-adapter-1 / A-MECH-1.** `turn/retracted` removes a *middle*
/// turn (a user turn plus its assistant turn — two entries in the
/// transcript), which shifts every later turn's raw index down by two. The
/// last turn's agent-message item already has a cached `Slot` in the fold's
/// `items` map from its first revision; the revision-2 update after the
/// retraction must land on that same turn's text block, in place, not get
/// dropped (leaving a stale duplicate on the next push) or land on the wrong
/// turn. Before the `reindex` fix this failed: the cached slot's `turn`
/// stayed at its pre-removal raw index instead of being remapped through the
/// rebuilt id → position map, so the retained-by-raw-index check in the old
/// code either evicted the slot or matched the wrong turn.
#[test]
fn a_retraction_of_a_middle_turn_remaps_the_last_turns_cached_slot() {
    use aui_protocol::{Block, Turn};
    let fold = replay(&fixtures_dir().join("synthetic-turn-removed.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let session = fold.session(&id).expect("session exists");

    // The middle turn (t-syn-2) is gone: its user turn and its assistant
    // turn both left the transcript.
    assert!(
        !session.turns.iter().any(|turn| turn.id() == "t-syn-2"),
        "the retracted turn is still in the transcript"
    );
    assert!(
        !session.turns.iter().any(|turn| matches!(turn, Turn::User { text, .. } if text == "two")),
        "the retracted turn's user message is still in the transcript"
    );

    // What remains: user "one", assistant t-syn-1, user "three", assistant
    // t-syn-3 — in that order.
    let turn = assistant_turn(&fold, &id, "t-syn-3");
    let blocks = turn.blocks();
    assert_eq!(blocks.len(), 1, "the revision-2 update landed as a second block: {blocks:?}");
    assert_eq!(
        blocks[0],
        Block::Text { text: "Reply three final".into(), streaming: false },
        "the update did not land on t-syn-3's text block"
    );

    // The turn just before t-syn-3 must be t-syn-1's assistant turn, never
    // the retracted t-syn-2 nor a stray duplicate — confirms the whole list
    // reindexed rather than one slot silently vanishing. `turn/retracted`
    // appends its own marker turn after the removal, which is expected here.
    let ids: Vec<&str> = session.turns.iter().map(Turn::id).collect();
    assert_eq!(
        ids,
        vec!["i-syn-user-1", "t-syn-1", "i-syn-user-3", "t-syn-3", "marker:v:syn:13"]
    );
}

/// **client-adapter-3 / A-MECH-5.** A decode failure (a required field
/// missing, or a shape `serde` rejects) used to return an empty `Vec<Delta>`
/// with no counter, no log and no marker — a server shape-change was
/// invisible in the transcript. It must now be counted in `SideState`.
#[test]
fn a_malformed_item_notification_counts_as_a_decode_failure() {
    let mut fold = MuseFold::new();
    let event = |method: &str, params: serde_json::Value| MuseEvent::Notification {
        method: method.to_owned(),
        cursor: None,
        session_id: Some("s".to_owned()),
        params,
    };
    // A well-formed event first, so the session exists and the count starts
    // at zero.
    fold.apply(event(
        "item/completed",
        serde_json::json!({
            "sessionId": "s",
            "item": {"itemId": "i", "kind": "agentMessage", "turnId": "t",
                     "revision": 1, "status": "completed", "text": "hi"}
        }),
    ));
    assert_eq!(fold.side("s").expect("side state exists").decode_failures, 0);

    // `item/completed` with no `item` at all: the fold cannot decode it.
    fold.apply(event("item/completed", serde_json::json!({"sessionId": "s"})));
    assert_eq!(fold.side("s").expect("side state exists").decode_failures, 1);

    // An unknown method falls into the catch-all, which is the same kind of
    // blind spot.
    fold.apply(event("session/somethingNew", serde_json::json!({"sessionId": "s"})));
    assert_eq!(fold.side("s").expect("side state exists").decode_failures, 2);
}

#[test]
fn a_stale_revision_is_ignored() {
    // The apply rule is "replace iff the revision is higher". Feeding the same
    // item twice must change nothing the second time.
    let started = serde_json::json!({
        "sessionId": "s", "viewCursor": "v:s:1",
        "item": {"itemId": "i", "kind": "agentMessage", "turnId": "t",
                 "revision": 2, "status": "completed", "text": "final"}
    });
    let stale = serde_json::json!({
        "sessionId": "s", "viewCursor": "v:s:2",
        "item": {"itemId": "i", "kind": "agentMessage", "turnId": "t",
                 "revision": 1, "status": "inProgress", "text": "partial"}
    });
    let mut fold = MuseFold::new();
    let event = |method: &str, params: serde_json::Value| MuseEvent::Notification {
        method: method.to_owned(),
        cursor: params.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned),
        session_id: Some("s".to_owned()),
        params,
    };
    assert!(!fold.apply(event("item/completed", started)).is_empty());
    assert!(fold.apply(event("item/started", stale)).is_empty(), "a stale revision was applied");
    let session = fold.session("s").expect("session exists");
    let block = session.turns[0].blocks().first().expect("one block");
    assert_eq!(block, &aui_protocol::Block::Text { text: "final".into(), streaming: false });
}

// ---------------------------------------------------------------- todo & goal

/// The three transitions no wire capture contains, from the hand-written
/// `synthetic-todo-goal.jsonl`: a whole-list replace, an empty list, and a goal
/// set, pushed past 100 % and cleared with an explicit `null`.
#[test]
fn the_synthetic_capture_exercises_every_todo_and_goal_transition() {
    let path = fixtures_dir().join("synthetic-todo-goal.jsonl");
    let text = std::fs::read_to_string(&path).expect("capture is readable");
    let session = "synthetic-todo-goal";

    // Fold it a line at a time so the intermediate states can be asserted, not
    // just the last one: a card that is right at the end and wrong in the
    // middle is still wrong.
    let mut fold = MuseFold::new();
    let mut todos: Vec<usize> = Vec::new();
    let mut goals: Vec<Option<f64>> = Vec::new();
    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Some(frame) = frame::parse_line(body).expect("frame parses") else { continue };
        let Some(event) = MuseEvent::from_frame(frame) else { continue };
        let method = match &event {
            MuseEvent::Notification { method, .. } => method.clone(),
            _ => String::new(),
        };
        fold.apply(event);
        let blocks: Vec<&aui_protocol::Block> = fold
            .session(session)
            .expect("session exists")
            .turns
            .iter()
            .flat_map(|turn| turn.blocks())
            .collect();
        match method.as_str() {
            "session/todoListChanged" => todos.push(
                blocks
                    .iter()
                    .find_map(|b| match b {
                        aui_protocol::Block::Todo { items } => Some(items.len()),
                        _ => None,
                    })
                    .unwrap_or(0),
            ),
            "session/goalChanged" => goals.push(blocks.iter().find_map(|b| match b {
                aui_protocol::Block::Goal { percent_complete, .. } => {
                    Some(percent_complete.unwrap_or_default() as f64)
                }
                _ => None,
            })),
            _ => {}
        }
    }

    // Three items, then four (a whole-list replace), then none (the card goes),
    // then five when the agent picks the list back up.
    assert_eq!(todos, vec![3, 4, 0, 5], "todo transitions");
    // 40 %, then a provider-reported 120 % passed through verbatim, then no
    // goal at all, then a goal again.
    assert_eq!(goals, vec![Some(40.0), Some(120.0), None, Some(120.0)], "goal transitions");
}

/// A capture that names no session must still not panic the fold, and every
/// capture in the directory must open cleanly — which is what `--replay` does.
#[test]
fn every_capture_opens_without_panicking() {
    for path in captures() {
        // The fold already ran on the line above — a panic there fails this
        // test for every file, account capture included. Only the session
        // assert below is transcript-specific (see
        // `every_capture_produces_at_least_one_turn`).
        let fold = replay(&path);
        if path.file_name().is_some_and(|name| name == "transcript-account.jsonl") {
            continue;
        }
        assert!(fold.session_ids().next().is_some(), "{} folded to no session", path.display());
    }
}

// --------------------------------------------------------------- F3: parity

/// Fold one capture the way a **backfill** would serve it.
///
/// `view/page` replays a session out of its log, and a log holds finished
/// items: it hands each one over whole, so there is no `item/started` and no
/// `item/delta` in a page — only `item/completed` and the session-level
/// notifications (`turn/*`, `session/*`, `approval/*`, `userInput/*`), in
/// cursor order. That is exactly the filter here, applied to a capture of the
/// same session arriving live.
///
/// The JSON-RPC *results* are kept: `session/start`'s result is how the fold
/// learns the session exists, and reconnecting really does get one (from
/// `session/resume`). Only the streaming halves are dropped.
fn replay_as_backfill(path: &Path) -> MuseFold {
    let text = std::fs::read_to_string(path).expect("capture is readable");
    let mut fold = MuseFold::new();
    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Ok(Some(frame)) = frame::parse_line(body) else { continue };
        let Some(event) = MuseEvent::from_frame(frame) else { continue };
        if let MuseEvent::Notification { method, .. } = &event {
            if method == "item/started" || method == "item/delta" {
                continue;
            }
        }
        fold.apply(event);
    }
    fold
}

/// Whether the capture ends with an item that started and never completed.
fn has_an_unfinished_item(text: &str) -> bool {
    let mut open: std::collections::HashSet<String> = std::collections::HashSet::new();
    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else { continue };
        let method = value.get("method").and_then(|m| m.as_str()).unwrap_or_default();
        let Some(id) = value.pointer("/params/item/itemId").and_then(|v| v.as_str()) else { continue };
        match method {
            "item/started" => {
                open.insert(id.to_owned());
            }
            "item/completed" => {
                open.remove(id);
            }
            _ => {}
        }
    }
    !open.is_empty()
}

/// Strip what a backfill genuinely cannot know, so the comparison is about the
/// fold and not about the transport.
///
/// * **Streaming flags.** A live text block is `streaming: true` while its
///   deltas arrive and a live thinking block is mid-flight; a backfilled one
///   was never in either state. Both settle the same way on the turn's
///   terminal, so both are settled here.
/// * **Turn metas.** [`aui_protocol::TurnMeta`] is assembled from
///   `session/tokenUsage`, which a page does serve — but the model id and the
///   duration come off the live `turn/completed`, and a fold that never saw the
///   turn start has no clock. Defaulted on both sides.
fn normalised(fold: &MuseFold, session_id: &str) -> Vec<aui_protocol::Turn> {
    use aui_protocol::{Block, ThinkingState, Turn, TurnMeta};
    let mut turns = fold.session(session_id).expect("session exists").turns.clone();
    for turn in &mut turns {
        if let Turn::Assistant { blocks, meta, .. } = turn {
            *meta = TurnMeta::default();
            for block in blocks.iter_mut() {
                match block {
                    Block::Text { streaming, .. } => *streaming = false,
                    Block::Thinking { state, .. } => *state = ThinkingState::Done,
                    _ => {}
                }
            }
        }
    }
    turns
}

/// **F3 — live / backfill parity, offline.**
///
/// A transcript can arrive twice: live, as `item/started` → `item/delta` →
/// `item/completed`; or backfilled, as a `view/page` that never replays a delta
/// and hands each item over whole. If those disagree then reopening a session
/// changes what it says, which is the one thing a transcript may never do.
///
/// The live test in `muse-client/tests/live.rs` proves this against a running
/// server, but it **spends a real turn every run** (see its doc comment), so it
/// is not the gate. This is: the same real captures, folded both ways, for
/// nothing.
#[test]
fn a_live_fold_and_a_backfilled_fold_agree() {
    let mut exercised = 0usize;
    for path in captures() {
        let name = path.file_name().expect("named file").to_string_lossy().into_owned();
        // A capture with no streaming halves in it has nothing to say here: the
        // two folds would be fed the identical byte stream.
        let text = std::fs::read_to_string(&path).expect("capture is readable");
        if !text.contains("\"item/started\"") && !text.contains("\"item/delta\"") {
            continue;
        }
        // A capture cut off mid-item — `transcript-approve-stage1.jsonl` stops
        // at the pending approval, on purpose — has an item that never
        // completed. A backfill would never serve a half-finished item, so the
        // two folds legitimately differ on the tool card that is still running,
        // and comparing them here would test the scissors rather than the fold.
        if has_an_unfinished_item(&text) {
            continue;
        }
        exercised += 1;
        let live = replay(&path);
        let backfill = replay_as_backfill(&path);

        let live_ids: Vec<String> = live.session_ids().map(str::to_owned).collect();
        let backfill_ids: Vec<String> = backfill.session_ids().map(str::to_owned).collect();
        assert_eq!(live_ids, backfill_ids, "{name}: the two folds saw different sessions");

        for id in &live_ids {
            let a = normalised(&live, id);
            let b = normalised(&backfill, id);
            assert_eq!(
                a.len(),
                b.len(),
                "{name}/{id}: live folded {} turns, backfill folded {}",
                a.len(),
                b.len()
            );
            for (index, (live_turn, backfilled)) in a.iter().zip(&b).enumerate() {
                assert_eq!(
                    live_turn, backfilled,
                    "{name}/{id}: turn {index} folded differently from a page than from the stream"
                );
            }
        }
    }
    // The gate is only a gate while there is something to compare: the real
    // captures carry `item/delta`, and a directory that lost them would
    // otherwise pass this test by having nothing in it.
    assert!(exercised >= 4, "only {exercised} captures carry a streamed item; parity was barely tested");
}

/// The specific disagreement F3 found, pinned so it cannot come back.
///
/// In `transcript-approve.jsonl` a `userShell` item (log sequence 6) starts,
/// raises a two-stage approval (sequence 9), and only completes afterwards.
/// Live, the tool card is added at `item/started` and the approval lands after
/// it. Backfilled, there is no `item/started`: the approval arrives first and
/// the tool card only at `item/completed`. Ordering the blocks by the item's own
/// log sequence — the same number on both events — is what makes the two agree.
#[test]
fn a_tool_card_that_raised_an_approval_stays_above_it_in_both_folds() {
    use aui_protocol::Block;
    let path = fixtures_dir().join("transcript-approve.jsonl");
    let kinds = |fold: &MuseFold| -> Vec<&'static str> {
        let id = fold.session_ids().next().expect("one session").to_owned();
        fold.session(&id)
            .expect("session exists")
            .turns
            .iter()
            .flat_map(|turn| turn.blocks())
            .map(|block| match block {
                Block::ToolCall { .. } => "tool",
                Block::Approval { .. } => "approval",
                _ => "other",
            })
            .collect()
    };
    let live = kinds(&replay(&path));
    let backfill = kinds(&replay_as_backfill(&path));
    assert_eq!(live, vec!["tool", "approval"], "the live fold lost the log's order");
    assert_eq!(backfill, live, "the backfilled fold ordered the turn differently");
}

/// **client-adapter-12 / A-MECH-4.** `session/modelRouteUnserved` has no
/// `Delta` — the standing model selection does not change — so it must be
/// visible in side state rather than silently dropped through the fold's
/// untyped-method catch-all.
#[test]
fn model_route_unserved_is_kept_visible_in_side_state() {
    let fold = replay(&fixtures_dir().join("synthetic-modelrouteunserved.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let side = fold.side(&id).expect("side state exists");
    let marker = side.model_route_unserved.as_ref().expect("the notification was dropped");
    assert_eq!(marker.installed_provider_id, "meta");
    assert_eq!(marker.model_id, "muse-spark-1.3");
    assert_eq!(marker.provider_id.as_deref(), Some("openai"));
}

/// A truncated tool item keeps its `outputRef` for an `item/readOutput` fetch.
///
/// `synthetic-readoutput.jsonl` is the only capture that truncates: its `bash`
/// item completes with `truncated: true` and an `outputRef`, and the fold must
/// record that handle under the item's id — the id its `ToolCall` block
/// renders as — while an untruncated item records nothing.
#[test]
fn a_truncated_tool_item_keeps_its_output_ref() {
    let fold = replay(&fixtures_dir().join("synthetic-readoutput.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let stored = fold.stored_output(&id, "i-syn-bash-1").expect("truncated item keeps its outputRef");
    assert_eq!(stored.id, "out-syn-1");
    // The visible output is truncated, but the card still rendered: the fold
    // records the handle without changing the transcript.
    let session = fold.session(&id).expect("session exists");
    let tools = session
        .turns
        .iter()
        .flat_map(|turn| turn.blocks())
        .filter(|block| matches!(block, aui_protocol::Block::ToolCall { .. }))
        .count();
    assert_eq!(tools, 1, "the truncated tool item folded to no tool card");
}

// ------------------------------------------- Task B: structured presentation

/// The assistant turn with an id, or a panic naming the turns present.
fn assistant_turn<'a>(
    fold: &'a MuseFold,
    session_id: &str,
    turn_id: &str,
) -> &'a aui_protocol::Turn {
    let session = fold.session(session_id).expect("session exists");
    session.turns.iter().find(|turn| turn.id() == turn_id).unwrap_or_else(|| {
        panic!(
            "no turn {turn_id}; have {:?}",
            session.turns.iter().map(|turn| turn.id().to_owned()).collect::<Vec<_>>()
        )
    })
}

/// The structured shell envelope folds into the shell body: the command is
/// the title, the output text is the body, the status comes from the exit
/// code — never a raw JSON card.
#[test]
fn a_shell_json_envelope_folds_to_a_shell_card() {
    use aui_protocol::{ToolBody, ToolKind, ToolStatus};
    let fold = replay(&fixtures_dir().join("synthetic-toolshapes.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let turn = assistant_turn(&fold, &id, "t-syn-1");
    let call = turn
        .blocks()
        .iter()
        .filter_map(|block| block.as_tool_call())
        .find(|call| call.id == "i-syn-bash-1")
        .expect("the enveloped shell call folded to a tool card");
    assert_eq!(call.kind, ToolKind::Shell);
    assert_eq!(call.verb, "Ran");
    assert_eq!(call.target, "ls -la");
    assert_eq!(call.status, ToolStatus::Success);
    match call.body {
        ToolBody::Shell { output_lines, exit_code, live } => {
            assert_eq!(output_lines, vec!["README.md".to_owned(), "notes.txt".to_owned()]);
            assert_eq!(exit_code, Some(0));
            assert!(!live);
        }
        body => panic!("enveloped shell output folded to {body:?}"),
    }
}

/// A todo tool call folds its args into the session's todo card: no tool
/// card for the call itself, and its `{"ok": …}` result is shown nowhere.
#[test]
fn a_todo_tool_call_folds_to_the_todo_card() {
    use aui_protocol::{Block, TodoState};
    let fold = replay(&fixtures_dir().join("synthetic-toolshapes.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let session = fold.session(&id).expect("session exists");
    let mut todos = Vec::new();
    for turn in &session.turns {
        for block in turn.blocks() {
            if let Block::Todo { items } = block {
                todos.extend(items.iter().cloned());
            }
        }
    }
    let labels: Vec<(&str, TodoState)> =
        todos.iter().map(|item| (item.label.as_str(), item.state)).collect();
    assert_eq!(
        labels,
        vec![
            ("Sweep the workspace", TodoState::Running),
            ("Update the changelog", TodoState::Pending),
            ("File the notes", TodoState::Done),
        ]
    );
    let rendered = serde_json::to_string(session).expect("session serializes");
    assert!(!rendered.contains("i-syn-todo-1"), "the todo call drew a tool card");
    assert!(!rendered.contains(r#""items":3"#), "the todo result count leaked");
    assert!(!rendered.contains(r#""ok""#), "the todo result was shown");
}

/// Any other JSON object result folds pretty-printed with its args as
/// parameters, while a file read keeps its line-count body.
#[test]
fn other_json_results_fold_pretty_and_reads_keep_their_line_count() {
    use aui_protocol::ToolBody;
    let fold = replay(&fixtures_dir().join("synthetic-toolshapes.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let turn = assistant_turn(&fold, &id, "t-syn-1");
    let calls: Vec<aui_protocol::ToolCall> =
        turn.blocks().iter().filter_map(|block| block.as_tool_call()).collect();
    let estimate =
        calls.iter().find(|call| call.id == "i-syn-est-1").expect("the JSON call folded");
    match &estimate.body {
        ToolBody::Mcp { params, result_json } => {
            assert_eq!(
                *params,
                vec![
                    ("task".to_owned(), "tidy".to_owned()),
                    ("depth".to_owned(), "2".to_owned()),
                ]
            );
            let parsed: serde_json::Value =
                serde_json::from_str(result_json).expect("pretty result parses");
            assert_eq!(parsed["estimate_minutes"], 12);
            assert_eq!(*result_json, serde_json::to_string_pretty(&parsed).expect("pretty"));
        }
        body => panic!("a JSON object result folded to {body:?}"),
    }
    let read = calls.iter().find(|call| call.id == "i-syn-read-1").expect("the read folded");
    match &read.body {
        ToolBody::Read { lines } => assert_eq!(*lines, 3),
        body => panic!("a file read folded to {body:?}"),
    }
}

/// `reminderChild` items render as nothing at all: no generic card, no
/// block, no trace in the serialised transcript.
#[test]
fn reminder_children_render_as_nothing() {
    use aui_protocol::Block;
    let fold = replay(&fixtures_dir().join("synthetic-reminderchild.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let session = fold.session(&id).expect("session exists");
    for turn in &session.turns {
        for block in turn.blocks() {
            assert!(
                !matches!(block, Block::Generic { .. }),
                "a reminder child drew a generic card"
            );
        }
    }
    let turn = assistant_turn(&fold, &id, "t-syn-1");
    assert_eq!(turn.blocks().len(), 1, "reminder children left blocks behind");
    assert!(
        matches!(turn.blocks()[0], Block::Text { .. }),
        "the surviving block is not the reply text"
    );
    let rendered = serde_json::to_string(session).expect("session serializes");
    assert!(!rendered.contains("reminderChild"), "a reminder kind leaked");
    assert!(!rendered.contains("i-syn-rem-"), "a reminder item leaked");
}

/// A `reasoning` item with no `summary` falls back to its raw text so exposed
/// reasoning is never dropped; the collapsed line stays the first summary
/// part, and a summarised item is untouched by the fallback.
#[test]
fn reasoning_falls_back_to_raw_text_when_the_summary_is_empty() {
    use aui_protocol::Block;
    let fold = replay(&fixtures_dir().join("synthetic-reasoning-text.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let turn = assistant_turn(&fold, &id, "t-syn-1");
    let thinking: Vec<(&str, Option<&str>)> = turn
        .blocks()
        .iter()
        .filter_map(|block| match block {
            Block::Thinking { text, summary, .. } => {
                Some((text.as_str(), summary.as_deref()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        thinking,
        vec![
            ("Consider the workspace layout first, then list files.", None),
            ("Plan the sweep", Some("Plan the sweep")),
        ]
    );
}

/// Consecutive tool calls fold into one `ToolGroup` per run with a
/// verb-derived summary; thinking, approvals and errors each break the run,
/// and the failed and recovered calls stand alone.
#[test]
fn consecutive_tool_calls_fold_into_verb_summarised_groups() {
    use aui_protocol::{ActivityState, Block, ToolKind, ToolStatus};
    let fold = replay(&fixtures_dir().join("synthetic-toolgroup.jsonl"));
    let id = fold.session_ids().next().expect("one session").to_owned();
    let turn = assistant_turn(&fold, &id, "t-syn-1");
    let blocks = turn.blocks();
    assert_eq!(blocks.len(), 6, "unexpected blocks: {blocks:?}");
    match &blocks[0] {
        Block::ToolGroup { calls, summary, state } => {
            assert_eq!(summary, "Ran 3 commands");
            assert_eq!(*state, ActivityState::Done);
            assert_eq!(calls.len(), 3);
            assert!(calls.iter().all(|call| call.kind == ToolKind::Shell));
            assert!(calls.iter().all(|call| call.status == ToolStatus::Success));
        }
        other => panic!("the shell run did not group: {other:?}"),
    }
    assert!(matches!(blocks[1], Block::Thinking { .. }), "no thinking break: {:?}", blocks[1]);
    match &blocks[2] {
        Block::ToolGroup { calls, summary, state } => {
            assert_eq!(summary, "Read 2 files");
            assert_eq!(*state, ActivityState::Done);
            assert_eq!(calls.len(), 2);
            assert!(calls.iter().all(|call| call.kind == ToolKind::Read));
        }
        other => panic!("the read run did not group: {other:?}"),
    }
    match &blocks[3] {
        Block::Approval { tool, command, state, .. } => {
            assert_eq!(tool, "bash");
            assert_eq!(command, "cargo run -- --no-connect");
            assert!(matches!(state, aui_protocol::ApprovalState::Approving));
        }
        other => panic!("the approval did not break the run: {other:?}"),
    }
    let failed = blocks[4].as_tool_call().expect("the failed call folded");
    assert_eq!(failed.status, ToolStatus::Error);
    assert!(failed.target.contains("deny warnings"), "wrong card: {:?}", failed.target);
    let lone = blocks[5].as_tool_call().expect("the last call folded");
    assert_eq!(lone.status, ToolStatus::Success);
}

/// The group forms while streaming: the second started call joins the first
/// into a working group, and the next output chunk lands in the right member.
#[test]
fn a_group_forms_incrementally_while_streaming() {
    use aui_protocol::{ActivityState, Block};
    let path = fixtures_dir().join("synthetic-toolgroup.jsonl");
    let text = std::fs::read_to_string(&path).expect("capture is readable");
    let mut fold = MuseFold::new();
    let mut staged = 0;
    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Some(frame) = frame::parse_line(body).expect("frame parses") else { continue };
        let Some(event) = MuseEvent::from_frame(frame) else { continue };
        fold.apply(event);
        if body.contains("\"item/started\"") && body.contains("i-tg-2") {
            let turn = assistant_turn(&fold, "synthetic-toolgroup", "t-syn-1");
            assert_eq!(turn.blocks().len(), 1, "the run split while streaming");
            match &turn.blocks()[0] {
                Block::ToolGroup { calls, summary, state } => {
                    assert_eq!(calls.len(), 2);
                    assert_eq!(summary, "Ran 2 commands");
                    assert_eq!(*state, ActivityState::Working);
                }
                other => panic!("the second started call did not join: {other:?}"),
            }
            staged += 1;
        }
        if body.contains("\"item/delta\"") && body.contains("i-tg-1") {
            let turn = assistant_turn(&fold, "synthetic-toolgroup", "t-syn-1");
            match &turn.blocks()[0] {
                Block::ToolGroup { calls, .. } => {
                    let first =
                        calls.iter().find(|call| call.id == "i-tg-1").expect("member kept");
                    let output = match &first.body {
                        aui_protocol::ToolBody::Shell { output_lines, .. } => {
                            output_lines.join("\n")
                        }
                        body => panic!("streamed output left the shell body: {body:?}"),
                    };
                    assert!(output.contains("linking harness"), "delta lost: {output:?}");
                }
                other => panic!("the group broke on a delta: {other:?}"),
            }
            staged += 1;
        }
    }
    assert_eq!(staged, 2, "the streaming stages never ran");
}
