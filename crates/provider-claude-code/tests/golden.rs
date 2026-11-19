//! Golden transcripts for the Claude Code fold (docs/82 5.3): every fixture
//! under `fixtures/claude-code/`, folded the way the adapter folds it, must
//! produce the session recorded in `tests/golden/` (replay timing) and
//! `tests/golden-live/` (live timing) byte for byte. See
//! `provider/tests/support/golden.rs` for what a golden holds.

#[path = "../../provider/tests/support/golden.rs"]
mod golden;

use std::path::Path;

use aui_protocol::Provider;
use golden::{Mask, Recorder};
use provider::ProviderEvent;
use provider_claude_code::fold::{step_line, ClaudeFold};

const SESSION: &str = "claude";

/// Fold one fixture through `step_line`. After every line the three ways a
/// child's turns end without a `result` run on copies: the child lost
/// (`abandon_live_turns`), Stop's fallback (`interrupt_live_turns`) and a
/// relaunch (`drop_pending_asks`); at the end, the stored-history close
/// (`finish_stored_turns`).
fn record(path: &Path, live: bool) -> Recorder {
    let mut fold = ClaudeFold::new();
    fold.set_live(live);
    let mut recorder = Recorder::new(Provider::Claude, if live { Mask::Live } else { Mask::Replay });
    let text = std::fs::read_to_string(path).expect("fixture reads");
    let mut last = 0;
    for (number, line) in text.lines().enumerate() {
        let mut deltas = Vec::new();
        step_line(&mut fold, line, &mut |event| {
            if let ProviderEvent::Deltas { deltas: more, .. } = event {
                deltas.extend(more);
            }
        });
        recorder.apply(SESSION, deltas);
        recorder.step(number + 1);
        let lost = fold.clone().abandon_live_turns("golden: the agent process exited");
        recorder.abandon(number + 1, vec![(SESSION.to_owned(), lost)]);
        let (stopped, _) = fold.clone().interrupt_live_turns();
        recorder.abandon(number + 1, vec![(SESSION.to_owned(), stopped)]);
        let relaunched = fold.clone().drop_pending_asks();
        recorder.abandon(number + 1, vec![(SESSION.to_owned(), relaunched)]);
        last = number + 1;
    }
    let stored = fold.clone().finish_stored_turns();
    recorder.abandon(last, vec![(SESSION.to_owned(), stored)]);
    recorder
}

fn check_all(live: bool, dir: &str) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for fixture in golden::fixtures(&root.join("../../fixtures/claude-code")) {
        let golden = golden::golden_path(&root.join(dir), &fixture);
        if let Some(failure) = record(&fixture, live).check(&golden) {
            failures.push(failure);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_claude_fixture_folds_to_its_golden_session() {
    check_all(false, "tests/golden");
}

#[test]
fn every_claude_fixture_folds_live_to_its_golden_session() {
    check_all(true, "tests/golden-live");
}
