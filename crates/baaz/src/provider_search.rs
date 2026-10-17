//! Searchable text for Claude Code and Codex sessions (docs/50 P3).
//!
//! Muse's sessions reach `search.db` through Muse's own `search_text`
//! column ([`crate::index::read_bodies`]). The other two providers keep no
//! index, only their stored transcripts, so the search sync reads those:
//! Claude Code's `<config-dir>/projects/<slug>/<id>.jsonl` and Codex's
//! `<codex-home>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`. What counts as a
//! word (prompts and answers; never tool output, thinking or injected
//! context) is the adapter's call — [`provider_claude_code::history::search_text`]
//! and [`provider_codex::rollout::search_text`] — so this module only finds
//! files and dispatches.
//!
//! Blocking: it runs inside the search sync's background closure, and only
//! for the rows whose fingerprint moved, never on the UI thread.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A provider-lane session whose words the search sync wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BodySource {
    /// The session id (Claude's session uuid, Codex's thread id).
    pub session_id: String,
    /// The wire provider id: `"claude-code"` or `"codex"`.
    pub provider: String,
}

/// Where stored transcripts live, searched in order: Baaz's own homes
/// first (every session Baaz opened since B4), then the owner's (a legacy
/// session that was never moved resumes from there, so it is searched
/// from there too). Read-only.
#[derive(Clone, Debug, Default)]
pub struct Homes {
    /// Claude config dirs (each holds `projects/`).
    pub claude: Vec<PathBuf>,
    /// Codex homes (each holds `sessions/`).
    pub codex: Vec<PathBuf>,
}

impl Homes {
    /// The homes this run's children write to, then the owner's.
    pub fn for_app() -> Self {
        let state = provider::child_env::state_dir();
        let owner = std::env::var_os("HOME").map(PathBuf::from);
        let mut claude = vec![state.join("claude-home")];
        let mut codex = vec![provider_codex::home::home_dir(&state)];
        if let Some(owner) = owner {
            claude.push(owner.join(".claude"));
            codex.push(provider_codex::home::owner_home_dir(&owner));
        }
        Self { claude, codex }
    }
}

/// The words of every `sources` session that has a stored transcript, keyed
/// by session id; a session with no file (or no words) is absent.
pub fn read_bodies(sources: &[BodySource]) -> HashMap<String, String> {
    read_bodies_in(sources, &Homes::for_app())
}

/// [`read_bodies`] against explicit homes: what the tests and the perf
/// budget use.
pub fn read_bodies_in(sources: &[BodySource], homes: &Homes) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let wanted = |provider: &str| -> Vec<&str> {
        sources.iter().filter(|s| s.provider == provider).map(|s| s.session_id.as_str()).collect()
    };
    let claude = wanted(providers_ids::CLAUDE);
    let codex = wanted(providers_ids::CODEX);
    let mut files: Vec<(String, PathBuf, &'static str)> = Vec::new();
    collect(&claude, &homes.claude, provider_claude_code::history::stored_transcripts, providers_ids::CLAUDE, &mut files);
    collect(&codex, &homes.codex, provider_codex::rollout::stored_rollouts, providers_ids::CODEX, &mut files);
    for (id, path, provider) in files {
        let Ok(raw) = std::fs::read_to_string(&path) else { continue };
        let text = if provider == providers_ids::CLAUDE {
            provider_claude_code::history::search_text(&raw)
        } else {
            provider_codex::rollout::search_text(&raw)
        };
        if !text.is_empty() {
            out.insert(id, text);
        }
    }
    out
}

/// The wire ids this module dispatches on.
mod providers_ids {
    pub const CLAUDE: &str = "claude-code";
    pub const CODEX: &str = "codex";
}

/// Find each id's file, walking a later home only for ids an earlier one
/// did not have (the owner's tree is never listed when Baaz's own covers
/// every id).
fn collect(
    ids: &[&str],
    homes: &[PathBuf],
    walk: fn(&Path) -> HashMap<String, PathBuf>,
    provider: &'static str,
    out: &mut Vec<(String, PathBuf, &'static str)>,
) {
    let mut missing: Vec<&str> = ids.to_vec();
    for home in homes {
        if missing.is_empty() {
            break;
        }
        let found = walk(home);
        missing.retain(|id| match found.get(*id) {
            Some(path) => {
                out.push(((*id).to_owned(), path.clone(), provider));
                false
            }
            None => true,
        });
    }
}

/// Write `count` deterministic provider transcripts under `dir` — half
/// Claude Code, half Codex — in each provider's real on-disk shape, and
/// return the homes and sources that find them. The perf budget's fixture.
pub fn write_synthetic(dir: &Path, count: usize) -> (Homes, Vec<BodySource>) {
    let claude_home = dir.join("claude-home");
    let codex_home = dir.join("codex-home");
    let _ = std::fs::remove_dir_all(&claude_home);
    let _ = std::fs::remove_dir_all(&codex_home);
    let slug = claude_home.join("projects").join("-work-project");
    let day = codex_home.join("sessions").join("2026").join("10").join("17");
    let _ = std::fs::create_dir_all(&slug);
    let _ = std::fs::create_dir_all(&day);
    let mut sources = Vec::with_capacity(count);
    for index in 0..count {
        let turns = 40;
        if index % 2 == 0 {
            let id = format!("0000{index:04}-aaaa-7000-8000-00000000c1a0");
            let mut text = String::new();
            for turn in 0..turns {
                text.push_str(&serde_json::json!({"type":"user","message":{"role":"user","content":format!("claude prompt {index} turn {turn} about the sidebar")}}).to_string());
                text.push('\n');
                text.push_str(&serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","name":"Read","input":{"path":"src/lib.rs"}}]}}).to_string());
                text.push('\n');
                text.push_str(&serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x".repeat(2048)}]}}).to_string());
                text.push('\n');
                text.push_str(&serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":format!("answer {turn} for session {index}")}]}}).to_string());
                text.push('\n');
            }
            let _ = std::fs::write(slug.join(format!("{id}.jsonl")), text);
            sources.push(BodySource { session_id: id, provider: providers_ids::CLAUDE.to_owned() });
        } else {
            let id = format!("0000{index:04}-bbbb-7000-8000-00000000c0de");
            let mut text = serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":"/work/project"}}).to_string();
            text.push('\n');
            for turn in 0..turns {
                for (role, kind, body) in [
                    ("user", "input_text", format!("codex prompt {index} turn {turn} about the router")),
                    ("assistant", "output_text", format!("codex answer {turn} for session {index}")),
                ] {
                    text.push_str(&serde_json::json!({"type":"response_item","payload":{"type":"message","role":role,"content":[{"type":kind,"text":body}]}}).to_string());
                    text.push('\n');
                }
                text.push_str(&serde_json::json!({"type":"response_item","payload":{"type":"function_call_output","output":"y".repeat(2048)}}).to_string());
                text.push('\n');
            }
            let _ = std::fs::write(day.join(format!("rollout-2026-10-17T10-00-00-{id}.jsonl")), text);
            sources.push(BodySource { session_id: id, provider: providers_ids::CODEX.to_owned() });
        }
    }
    (Homes { claude: vec![claude_home], codex: vec![codex_home] }, sources)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-provider-search-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("tmp");
        dir
    }

    #[test]
    fn both_providers_bodies_come_from_their_own_files() {
        let dir = tmp("both");
        let (homes, sources) = write_synthetic(&dir, 4);
        let bodies = read_bodies_in(&sources, &homes);
        assert_eq!(bodies.len(), 4);
        let claude = &bodies[&sources[0].session_id];
        assert!(claude.contains("claude prompt 0 turn 3 about the sidebar"));
        assert!(claude.contains("answer 3 for session 0"));
        assert!(!claude.contains("xxxx"), "tool results are not searchable words");
        let codex = &bodies[&sources[1].session_id];
        assert!(codex.contains("codex prompt 1 turn 0 about the router"));
        assert!(codex.contains("codex answer 39 for session 1"));
        assert!(!codex.contains("yyyy"), "tool output is not searchable words");
    }

    #[test]
    fn a_later_home_is_read_only_for_ids_the_first_lacks() {
        let first = tmp("first");
        let second = tmp("second");
        let (mut homes, sources) = write_synthetic(&first, 2);
        let (owner, owner_sources) = write_synthetic(&second, 2);
        // The same ids live in both trees; Baaz's own wins.
        assert_eq!(sources, owner_sources);
        let claude_id = &sources[0].session_id;
        std::fs::write(
            owner.claude[0].join("projects").join("-work-project").join(format!("{claude_id}.jsonl")),
            serde_json::json!({"type":"user","message":{"content":"owner copy"}}).to_string(),
        )
        .expect("owner copy");
        homes.claude.extend(owner.claude.clone());
        homes.codex.extend(owner.codex.clone());
        let bodies = read_bodies_in(&sources, &homes);
        assert!(!bodies[claude_id].contains("owner copy"));
        // An id only the owner's tree holds is found there.
        std::fs::remove_dir_all(first.join("claude-home")).expect("drop own tree");
        let bodies = read_bodies_in(&sources, &homes);
        assert_eq!(bodies[claude_id], "owner copy");
    }

    #[test]
    fn unknown_providers_and_missing_files_are_absent() {
        let dir = tmp("missing");
        let (homes, _) = write_synthetic(&dir, 2);
        let sources = vec![
            BodySource { session_id: "nope".into(), provider: "claude-code".into() },
            BodySource { session_id: "nope".into(), provider: "codex".into() },
            BodySource { session_id: "m1".into(), provider: "muse".into() },
        ];
        assert!(read_bodies_in(&sources, &homes).is_empty());
    }
}
