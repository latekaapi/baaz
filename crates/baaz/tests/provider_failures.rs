//! docs/82 5.5 (docs/81 F D5): a Claude Code or Codex turn that fails —
//! a rate limit included — leaves an error card in its transcript that
//! offers Retry, as a Muse one always has, not only a "Failed" row.
//!
//! Replays the hand-written failure fixtures (`tests/fixtures/conformance/
//! *-failed-turn.jsonl`, `*-rate-limited-turn.jsonl`) through the same folds
//! the live pumps use. The app-side Retry button is pinned by the lane
//! tests (`session::lane`).

use aui_protocol::{Block, Delta};
use provider::ProviderEvent;

fn hand_written(name: &str) -> Vec<String> {
    let path = format!("{}/tests/fixtures/conformance/{name}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path)
        .unwrap_or_else(|_| panic!("fixture reads: {path}"))
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

fn claude_deltas(lines: &[String]) -> Vec<Delta> {
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    let mut deltas = Vec::new();
    for line in lines {
        provider_claude_code::fold::step_line(&mut fold, line, &mut |event| {
            if let ProviderEvent::Deltas { deltas: more, .. } = event {
                deltas.extend(more);
            }
        });
    }
    deltas
}

fn codex_deltas(lines: &[String]) -> Vec<Delta> {
    let mut fold = provider_codex::fold::CodexFold::new();
    let mut deltas = Vec::new();
    for line in lines {
        let (_, frame) = provider_codex::frame::decode_envelope(line).expect("fixture decodes");
        deltas.extend(fold.apply(&frame));
    }
    deltas
}

/// The error cards the deltas add, with the turn each lands in, and
/// whether that turn finished after its card.
fn error_cards(deltas: &[Delta]) -> Vec<(String, String, String, bool, bool)> {
    deltas
        .iter()
        .enumerate()
        .filter_map(|(at, delta)| match delta {
            Delta::BlockAdded { turn_id, block: Block::Error { title, detail, retryable } } => {
                let finished_after = deltas[at..]
                    .iter()
                    .any(|later| matches!(later, Delta::TurnFinished { turn_id: done, .. } if done == turn_id));
                Some((turn_id.clone(), title.clone(), detail.clone(), *retryable, finished_after))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn a_failed_claude_code_turn_cards_its_failure_with_retry() {
    let cards = error_cards(&claude_deltas(&hand_written("claude-failed-turn.jsonl")));
    assert_eq!(cards.len(), 1, "{cards:?}");
    let (turn, title, detail, retryable, finished_after) = &cards[0];
    assert_eq!(turn, "msg_011CfLVhLCyjfyqnKyfUGHpa", "on the turn that failed");
    assert_eq!(title, "The turn failed");
    assert!(detail.contains("stream closed"), "the CLI's own words: {detail}");
    assert!(*retryable && *finished_after, "Retry offered, then the turn closes");
}

#[test]
fn a_rate_limited_claude_code_turn_says_so() {
    let cards = error_cards(&claude_deltas(&hand_written("claude-rate-limited-turn.jsonl")));
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].1, "Rate limit reached");
    assert!(cards[0].2.contains("rate_limit_error"), "{}", cards[0].2);
    assert!(cards[0].3, "a rate limit can be retried");
}

#[test]
fn a_failed_codex_turn_cards_its_failure_with_retry() {
    let cards = error_cards(&codex_deltas(&hand_written("codex-failed-turn.jsonl")));
    assert_eq!(cards.len(), 1, "{cards:?}");
    let (turn, title, detail, retryable, finished_after) = &cards[0];
    assert_eq!(turn, "01a0d344-5a47-7c80-91d9-0105775b5f40", "on the turn that failed");
    assert_eq!(title, "Service unavailable", "the typed codexErrorInfo names it");
    assert!(detail.contains("stream disconnected"), "{detail}");
    assert!(*retryable && *finished_after);
}

#[test]
fn a_rate_limited_codex_turn_says_so() {
    let cards = error_cards(&codex_deltas(&hand_written("codex-rate-limited-turn.jsonl")));
    assert_eq!(cards.len(), 1, "{cards:?}");
    assert_eq!(cards[0].1, "Rate limit reached", "usageLimitExceeded is a rate limit");
    assert!(cards[0].2.contains("usage limit"), "{}", cards[0].2);
    assert!(cards[0].3);
}

/// The person's own Stop is not a failure (docs/82 0.8): the interrupted
/// turns card nothing. Claude Code's interrupted turn ends in an error
/// `result` with no words, so what tells it apart is the Stop the adapter
/// noted before sending the interrupt — noted here as the adapter does.
#[test]
fn an_interrupted_turn_cards_no_failure() {
    let path = |kind: &str| format!("{}/../../fixtures/{kind}/interrupt.jsonl", env!("CARGO_MANIFEST_DIR"));
    let read = |kind: &str| -> Vec<String> {
        std::fs::read_to_string(path(kind)).expect("fixture reads").lines().map(str::to_owned).collect()
    };
    let mut fold = provider_claude_code::fold::ClaudeFold::new();
    fold.note_stopping();
    let mut deltas = Vec::new();
    for line in read("claude-code").iter().filter(|line| !line.contains("\"host->cli\"")) {
        provider_claude_code::fold::step_line(&mut fold, line, &mut |event| {
            if let ProviderEvent::Deltas { deltas: more, .. } = event {
                deltas.extend(more);
            }
        });
    }
    assert!(error_cards(&deltas).is_empty(), "{:?}", error_cards(&deltas));
    assert!(error_cards(&codex_deltas(&read("codex"))).is_empty());
}
