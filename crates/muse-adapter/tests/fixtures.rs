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
