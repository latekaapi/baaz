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
    // `workflow` and `reminderChild` are the two kinds allowed to fall back, and
    // no capture contains one.
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
        let fold = replay(&path);
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
