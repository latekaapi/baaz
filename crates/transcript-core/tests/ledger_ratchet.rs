//! Source ratchet (docs/82 5.3): no provider fold keeps a position ledger
//! any more. Every block position lives in `transcript-core`'s builder; a
//! fold names cards by stable id. This scans every fold's production
//! code (comments and `#[cfg(test)]` items excluded) for the maps they used
//! to keep and for hand-built positional deltas, and fails on any.

use std::path::Path;

/// Positional deltas only the builder may build, and the index they carry.
const EVERY_FOLD: &[&str] = &[
    "block_index",
    "Delta::BlockAdded",
    "Delta::BlockUpdated",
    "Delta::BlockRemoved",
    "Delta::TextDelta",
    "Delta::ThinkingDelta",
    "Delta::ToolOutputDelta",
];

/// The old ledgers, by fold (docs/81 B F-3).
const FOLDS: &[(&str, &[&str])] = &[
    (
        "provider-codex/src/fold.rs",
        &["turn_blocks", "plan_cards", "approval_sites", "running_commands", "assistant_started", "CodexApprovalSite", "CommandSite"],
    ),
    ("provider-claude-code/src/fold.rs", &["emitted", "approval_sites", "ApprovalSite", "TodoCard", "decided", "started: HashMap"]),
    (
        "muse-adapter/src/fold.rs",
        &[
            "Slots", "Slot", "TurnKey", "turn_keys", "turn_index", "block_order", "shift_slots", "reindex", "push_block",
            "assistant_turns", "user_keys",
        ],
    ),
    // Written on the builder from the start (docs/82 5.7): no ledger of
    // its own to forbid, only the positional deltas every fold may not
    // build.
    ("provider-acp/src/fold.rs", &[]),
];

/// `source` with every `#[cfg(test)]` item and every comment line removed.
fn production(source: &str) -> Vec<(usize, String)> {
    let lines: Vec<&str> = source.lines().collect();
    let mut kept = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if lines[index].trim_start().starts_with("#[cfg(test)]") {
            let (mut depth, mut opened) = (0i64, false);
            index += 1;
            while index < lines.len() {
                let line = lines[index];
                depth += line.matches('{').count() as i64 - line.matches('}').count() as i64;
                opened |= line.contains('{');
                index += 1;
                if (opened && depth <= 0) || (!opened && line.trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
        }
        let line = lines[index];
        if !line.trim_start().starts_with("//") {
            kept.push((index + 1, line.to_owned()));
        }
        index += 1;
    }
    kept
}

/// Whether `token` occurs in `line` as a whole word.
fn has_word(line: &str, token: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_';
    line.match_indices(token).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        let after = line[at + token.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// Every `(line, token)` of `source`'s production code naming a forbidden token.
fn violations(source: &str, tokens: &[&str]) -> Vec<String> {
    production(source)
        .into_iter()
        .flat_map(|(number, line)| {
            EVERY_FOLD
                .iter()
                .chain(tokens)
                .filter(|token| has_word(&line, token))
                .map(|token| format!("{number}: `{token}` in `{}`", line.trim()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn no_fold_keeps_a_position_ledger() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut failures = Vec::new();
    for (fold, tokens) in FOLDS {
        let source = std::fs::read_to_string(crates.join(fold)).unwrap_or_else(|err| panic!("{fold}: {err}"));
        failures.extend(violations(&source, tokens).into_iter().map(|found| format!("{fold}:{found}")));
    }
    assert!(!crates.join("muse-adapter/src/slots.rs").exists(), "Muse's slot ledger is back");
    assert!(failures.is_empty(), "a fold keeps positions again:\n{}", failures.join("\n"));
}

#[test]
fn the_ratchet_fails_on_planted_ledgers_and_ignores_tests_and_comments() {
    let planted = "struct Fold {\n    turn_blocks: HashMap<String, usize>,\n}\n\
        fn f(d: &mut Vec<Delta>) {\n    d.push(Delta::BlockUpdated { turn_id, block_index: 3, block });\n}\n";
    let found = violations(planted, &["turn_blocks"]);
    assert_eq!(found.len(), 3, "{found:?}");
    let hidden = "// turn_blocks: the old map\n#[cfg(test)]\nmod tests {\n    fn t() { let turn_blocks = 1; }\n}\n";
    assert!(violations(hidden, &["turn_blocks"]).is_empty());
    assert!(violations("fn settle_running_commands() {}", &["running_commands"]).is_empty(), "whole words only");
}
