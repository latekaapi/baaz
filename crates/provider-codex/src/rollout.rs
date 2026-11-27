//! Stored Codex sessions on disk: the rollout files under a Codex home.
//!
//! Every thread an app-server child runs is written as
//!
//! ```text
//! <codex-home>/sessions/YYYY/MM/DD/rollout-<timestamp>-<thread-id>.jsonl
//! ```
//!
//! which is where Baaz's search reads a Codex session's words from (docs/50
//! P3): the adapter keeps no transcript of its own, and `thread/read` needs a
//! live child. Read-only; nothing here writes inside a home.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The thread id a rollout file name carries: everything after the
/// `rollout-<YYYY-MM-DDTHH-MM-SS>-` prefix, minus `.jsonl`. Codex thread
/// ids are UUIDs (five dash-separated groups), so the id is the last five
/// groups of the stem.
pub fn thread_id_of(file_name: &str) -> Option<&str> {
    let stem = file_name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let mut dashes = stem.rmatch_indices('-').map(|(at, _)| at);
    // The fifth dash from the right opens the UUID.
    let at = dashes.nth(4)?;
    let id = &stem[at + 1..];
    (!id.is_empty()).then_some(id)
}

/// Every rollout under `<codex-home>/sessions`, by thread id: one walk of
/// the date tree, so a search sync that needs many bodies lists it once.
/// An absent tree is an empty map.
pub fn stored_rollouts(codex_home: &Path) -> HashMap<String, PathBuf> {
    let mut out = HashMap::new();
    let mut stack = vec![codex_home.join("sessions")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                stack.push(path);
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if let Some(id) = thread_id_of(name) {
                out.insert(id.to_owned(), path.clone());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_thread_id_is_the_uuid_after_the_timestamp() {
        assert_eq!(
            thread_id_of("rollout-2026-10-17T14-28-09-01a14915-3803-7303-aa91-189ad43310e1.jsonl"),
            Some("01a14915-3803-7303-aa91-189ad43310e1")
        );
        assert_eq!(thread_id_of("rollout-2026-10-17T14-28-09-01a14915-3803-7303-aa91-189ad43310e1.json"), None);
        assert_eq!(thread_id_of("notes.jsonl"), None);
        assert_eq!(thread_id_of("rollout-a-b.jsonl"), None);
    }

    #[test]
    fn stored_rollouts_walks_the_date_tree() {
        let home = std::env::temp_dir().join("codex-rollout-walk-test");
        let _ = std::fs::remove_dir_all(&home);
        let day = home.join("sessions").join("2026").join("10").join("17");
        std::fs::create_dir_all(&day).expect("day dir");
        let id = "01a14915-3803-7303-aa91-189ad43310e1";
        std::fs::write(day.join(format!("rollout-2026-10-17T14-28-09-{id}.jsonl")), "{}\n").expect("plant");
        std::fs::write(day.join("other.txt"), "x").expect("plant other");
        let found = stored_rollouts(&home);
        assert_eq!(found.len(), 1);
        assert_eq!(found[id], day.join(format!("rollout-2026-10-17T14-28-09-{id}.jsonl")));
        assert!(stored_rollouts(&home.join("absent")).is_empty());
    }
}
