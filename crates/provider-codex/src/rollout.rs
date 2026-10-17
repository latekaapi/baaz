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

/// Bytes of transcript text [`search_text`] keeps, as for Claude Code: the
/// prompts and answers, never megabytes per session.
pub const SEARCH_TEXT_LIMIT: usize = 256 * 1024;

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

/// The searchable words of one rollout: every user prompt and assistant
/// message, in order, one per line.
///
/// Only `response_item` messages with role `user` or `assistant` count;
/// `developer` instructions, tool calls, reasoning and event lines are
/// left out, as is user text that is a context block Codex injects
/// (`<environment_context>`, `<user_instructions>`, `<recommended_plugins>`).
/// A line that does not parse is skipped. Cut at [`SEARCH_TEXT_LIMIT`].
pub fn search_text(rollout: &str) -> String {
    let mut out = String::new();
    for line in rollout.lines() {
        if out.len() >= SEARCH_TEXT_LIMIT {
            break;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        if value.get("type").and_then(|v| v.as_str()) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else { continue };
        if payload.get("type").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        let role = payload.get("role").and_then(|v| v.as_str());
        if !matches!(role, Some("user" | "assistant")) {
            continue;
        }
        let Some(blocks) = payload.get("content").and_then(|c| c.as_array()) else { continue };
        for block in blocks {
            let Some(text) = block.get("text").and_then(|v| v.as_str()).map(str::trim) else { continue };
            if text.is_empty() || (role == Some("user") && text.starts_with('<')) {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    if out.len() > SEARCH_TEXT_LIMIT {
        let mut at = SEARCH_TEXT_LIMIT;
        while !out.is_char_boundary(at) {
            at -= 1;
        }
        out.truncate(at);
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
    fn search_text_keeps_prompts_and_answers_only() {
        let rollout = [
            r#"{"type":"session_meta","payload":{"id":"t1","cwd":"/work"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"You are Codex"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>\n<cwd>/work</cwd>"},{"type":"input_text","text":"rename the flux capacitor"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{}"}}"#,
            r#"{"type":"event_msg","payload":{"type":"agent_message","message":"dup"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Renamed it to warp coil."}]}}"#,
            "{broken",
        ]
        .join("\n");
        assert_eq!(search_text(&rollout), "rename the flux capacitor\nRenamed it to warp coil.");
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
