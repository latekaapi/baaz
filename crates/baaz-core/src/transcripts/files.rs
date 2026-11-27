//! Where Claude Code's and Codex's own session files are (docs/87 18.8):
//! Claude Code's `<config-dir>/projects/<slug>/<id>.jsonl` and Codex's
//! `<codex-home>/sessions/YYYY/MM/DD/rollout-…-<id>.jsonl`, in Baaz's own
//! homes first, then the owner's. The store's freshness check
//! ([`super::source_now`]) and its text-only fold ([`super::fill`]) read
//! them; nothing here writes. Blocking: each lookup lists folders.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        // The owner's homes as the import reads them (docs/72): the real
        // `HOME`, or the probe's fixture under `BAAZ_IMPORT_OWNER_HOME`.
        // An isolated run (a capture, a test state dir) never reads the
        // person's real `~/.claude` / `~/.codex`: only a probe's fixture home
        // (`BAAZ_IMPORT_OWNER_HOME`) counts as an owner there.
        let owner = crate::import::scan_allowed().then(crate::import::owner_home);
        let mut claude = vec![state.join("claude-home")];
        let mut codex = vec![provider_codex::home::home_dir(&state)];
        if let Some(owner) = owner {
            claude.push(owner.join(".claude"));
            codex.push(provider_codex::home::owner_home_dir(&owner));
        }
        Self { claude, codex }
    }
}

/// A provider's walk of its transcript folders: session id → file.
type StoreWalk = fn(&Path) -> HashMap<String, PathBuf>;

/// Where `(provider, session_id)`'s stored transcript is, when there is one
/// (docs/85 11.1: the transcript store's freshness check). Blocking: it
/// lists the homes' transcript folders.
pub fn stored_file(provider: &str, session_id: &str) -> Option<PathBuf> {
    stored_files_in(provider, &[session_id], &Homes::for_app()).remove(session_id)
}

/// [`stored_file`] for many sessions of one provider at once, against
/// explicit homes: each home's folders are listed once, and a later home
/// only for ids an earlier one did not have (the owner's tree is never
/// listed when Baaz's own covers every id). Ids with no file are absent.
pub fn stored_files_in(provider: &str, ids: &[&str], homes: &Homes) -> HashMap<String, PathBuf> {
    let (dirs, walk): (&[PathBuf], StoreWalk) = match provider {
        providers_ids::CLAUDE => (&homes.claude, provider_claude_code::history::stored_transcripts),
        providers_ids::CODEX => (&homes.codex, provider_codex::rollout::stored_rollouts),
        _ => return HashMap::new(),
    };
    let mut out = HashMap::new();
    let mut missing: Vec<&str> = ids.to_vec();
    for dir in dirs {
        if missing.is_empty() {
            break;
        }
        let found = walk(dir);
        missing.retain(|id| match found.get(*id) {
            Some(path) => {
                out.insert((*id).to_owned(), path.clone());
                false
            }
            None => true,
        });
    }
    out
}

/// The wire ids this module dispatches on.
mod providers_ids {
    pub const CLAUDE: &str = "claude-code";
    pub const CODEX: &str = "codex";
}

/// Write `count` deterministic provider transcripts under `dir` — half
/// Claude Code, half Codex — in each provider's real on-disk shape, and
/// return the homes that find them and each `(provider, session id)`. The
/// perf budget's fixture and the store's fold tests.
#[cfg(any(test, feature = "probe"))]
pub fn write_synthetic(dir: &Path, count: usize) -> (Homes, Vec<(String, String)>) {
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
            sources.push((providers_ids::CLAUDE.to_owned(), id));
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
            sources.push((providers_ids::CODEX.to_owned(), id));
        }
    }
    (Homes { claude: vec![claude_home], codex: vec![codex_home] }, sources)
}

