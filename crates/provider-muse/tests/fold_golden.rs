//! Golden transcripts for the Muse fold (docs/82 5.3): every capture under
//! `fixtures/msp/`, folded the way the hub folds it, must produce the
//! sessions recorded in `tests/golden/` byte for byte — the sessions the
//! app builds from the deltas, not the fold's own mirror (that one is
//! `tests/snapshots/`). See `provider/tests/support/golden.rs` for what a
//! golden holds.

#[path = "../../provider/tests/support/golden.rs"]
mod golden;

use std::path::Path;

use aui_protocol::{Delta, Provider};
use golden::{Mask, Recorder};
use provider_muse::fold::MuseFold;
use provider_muse::client::{frame, MuseEvent};

/// The hub's child-loss settlement (`provider-muse` `abandon_live_turns`),
/// on whatever fold it is handed: every session's pending approvals
/// cancelled and its running turns completed `interrupted`.
fn abandon(fold: &mut MuseFold) -> Vec<(String, Vec<Delta>)> {
    let sessions: Vec<String> = fold.session_ids().map(str::to_owned).collect();
    let mut out = Vec::new();
    for session in sessions {
        let running = fold.running_turns(&session);
        let mut deltas = fold.cancel_pending_approvals(&session);
        for turn in running {
            deltas.extend(fold.apply(MuseEvent::Notification {
                method: "turn/completed".into(),
                params: serde_json::json!({ "sessionId": session, "turnId": turn, "terminal": "interrupted" }),
                cursor: None,
                session_id: Some(session.clone()),
            }));
        }
        out.push((session, deltas));
    }
    out
}

/// Fold one capture's server→client lines, routing each event's deltas to
/// the session the fold last touched (as the hub and the replay do). The
/// child is lost on a copy after every line (every tenth on the two stress
/// captures, which run past a thousand lines).
fn record(path: &Path) -> Recorder {
    let text = std::fs::read_to_string(path).expect("capture reads");
    let stride = if text.lines().count() > 400 { 10 } else { 1 };
    let mut fold = MuseFold::new();
    let mut recorder = Recorder::new(Provider::Muse, Mask::Replay);
    for (number, line) in text.lines().enumerate() {
        if let Some(body) = line.strip_prefix("<-- ") {
            let parsed = frame::parse_line(body)
                .unwrap_or_else(|err| panic!("{}:{}: {err}", path.display(), number + 1));
            if let Some(event) = parsed.and_then(MuseEvent::from_frame) {
                let deltas = fold.apply(event);
                if let Some(session) = fold.last_touched().map(str::to_owned) {
                    recorder.apply(&session, deltas);
                }
            }
        }
        recorder.step(number + 1);
        if number % stride == 0 {
            let mut lost = fold.clone();
            recorder.abandon(number + 1, abandon(&mut lost));
        }
    }
    recorder
}

#[test]
fn every_muse_capture_folds_to_its_golden_sessions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for fixture in golden::fixtures(&root.join("../../fixtures/msp")) {
        let golden = golden::golden_path(&root.join("tests/golden"), &fixture);
        if let Some(failure) = record(&fixture).check(&golden) {
            failures.push(failure);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
