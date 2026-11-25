//! Keeping memory honest (docs/85 12.5, from the LLM wiki).
//!
//! * **Lint.** When a project goes idle — at most once a day
//!   ([`lint_due`]) — a cheap model reads its notes, with the paths they
//!   name that no longer exist ([`missing_paths`]), and answers with one
//!   fix: a note to rewrite or remove, or none ([`parse_fix`]). The fix
//!   goes through [`crate::memory::edit`], so the old text lands in the
//!   log. One note at a time; rule 27 applies to the call.
//! * **Use.** A note counts as used when a session reads its file. When
//!   five sessions have used one, Baaz suggests **once**, in one line,
//!   adding it to `AGENTS.md`; a yes is a normal visible edit Baaz never
//!   commits (rule 21). Counts live in the transcript store.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection};

/// Sessions that must have used a note before the suggestion.
pub const SUGGEST_AFTER: usize = 5;

/// The lint runs at most this often per project.
pub const LINT_EVERY: std::time::Duration = std::time::Duration::from_secs(24 * 3600);

/// The paths a note names (in backticks) that do not exist under `repo`:
/// a note pointing at a file that is gone is stale.
pub fn missing_paths(body: &str, repo: &Path) -> Vec<String> {
    body.split('`')
        .skip(1)
        .step_by(2)
        .map(|candidate| candidate.split(':').next().unwrap_or(candidate).trim())
        .filter(|candidate| {
            !candidate.contains(' ')
                && candidate.contains('/')
                && !candidate.starts_with("http")
                && !candidate.starts_with('~')
                && candidate.rsplit('/').next().is_some_and(|last| last.contains('.'))
        })
        .filter(|candidate| {
            let path = Path::new(candidate);
            !(if path.is_absolute() { path.exists() } else { repo.join(path).exists() })
        })
        .map(str::to_owned)
        .collect()
}

/// The lint's prompt: every note (title, file, body, capped), the missing
/// paths beside the notes that name them, and the one answer wanted.
pub fn lint_prompt(dir: &Path, repo: &Path) -> Option<String> {
    let notes = crate::memory::list(dir);
    if notes.is_empty() {
        return None;
    }
    let mut out = String::from(
        "These are a project's memory notes. Find at most ONE problem: a note that is stale (it names a file that no longer \
         exists, or describes what is no longer true), or two notes that contradict each other (keep the newer one's meaning). \
         Answer with exactly one line of JSON and nothing else:\n\
         {\"action\":\"none\"} or {\"action\":\"delete\",\"title\":\"…\",\"why\":\"…\"} or \
         {\"action\":\"rewrite\",\"title\":\"…\",\"body\":\"…\",\"why\":\"…\"}\n\n",
    );
    for (title, file, _) in notes {
        let body = std::fs::read_to_string(dir.join(&file)).map(|text| crate::memory::parse(&text, &file).body).unwrap_or_default();
        let body: String = body.chars().take(1500).collect();
        out.push_str(&format!("### {title} ({file})\n{body}\n"));
        let missing = missing_paths(&body, repo);
        if !missing.is_empty() {
            out.push_str(&format!("(these paths no longer exist: {})\n", missing.join(", ")));
        }
        out.push('\n');
        if out.len() > 24_000 {
            break;
        }
    }
    Some(crate::scrub::scrub(&out))
}

/// One fix the lint asked for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Fix {
    /// Remove the note.
    Delete { title: String },
    /// Replace the note's body.
    Rewrite { title: String, body: String },
}

/// Read the lint's answer; `None` for "none" or anything unreadable.
pub fn parse_fix(answer: &str) -> Option<Fix> {
    let start = answer.find('{')?;
    let end = answer.rfind('}')?;
    let value: serde_json::Value = serde_json::from_str(&answer[start..=end]).ok()?;
    let title = value.get("title").and_then(serde_json::Value::as_str).map(str::to_owned);
    match value.get("action").and_then(serde_json::Value::as_str)? {
        "delete" => Some(Fix::Delete { title: title? }),
        "rewrite" => Some(Fix::Rewrite { title: title?, body: value.get("body")?.as_str()?.to_owned() }),
        _ => None,
    }
}

/// Apply one fix (its old text goes to the log).
pub fn apply(dir: &Path, fix: &Fix) -> Result<String, String> {
    match fix {
        Fix::Delete { title } => crate::memory::edit(dir, title, &crate::memory::Change { delete: true, ..Default::default() }),
        Fix::Rewrite { title, body } => {
            crate::memory::edit(dir, title, &crate::memory::Change { body: Some(body.clone()), ..Default::default() })
        }
    }
}

/// Whether the lint is due for a project, by its stamp file's age; the
/// caller writes the stamp when it runs.
pub fn lint_due(stamp: &Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_none_or(|age| age >= LINT_EVERY)
}

/// The lint stamp for a memory folder: in Baaz's own state, never in the
/// shared folder.
pub fn stamp_path(state: &Path, dir: &Path) -> PathBuf {
    let name: String = dir.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    state.join("memory-lint").join(name)
}

fn ensure_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS note_use(note TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY(note, session_id)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS note_suggested(note TEXT PRIMARY KEY) WITHOUT ROWID;",
    )
}

/// Record that `session` used the note at `note`; true when this use makes
/// the suggestion due (the fifth session, never suggested before), which is
/// then marked as made.
pub fn record_use(conn: &Connection, note: &Path, session: &str) -> rusqlite::Result<bool> {
    ensure_tables(conn)?;
    let note = note.to_string_lossy();
    conn.execute("INSERT OR IGNORE INTO note_use(note, session_id) VALUES (?1, ?2)", params![note, session])?;
    let uses: i64 = conn.query_row("SELECT COUNT(*) FROM note_use WHERE note = ?1", params![note], |row| row.get(0))?;
    if (uses as usize) < SUGGEST_AFTER {
        return Ok(false);
    }
    Ok(conn.execute("INSERT OR IGNORE INTO note_suggested(note) VALUES (?1)", params![note])? == 1)
}

/// The notes in `dir` a turn read (its Read calls' targets).
pub fn notes_read(turn: &aui_protocol::Turn, dir: &Path) -> Vec<PathBuf> {
    let aui_protocol::Turn::Assistant { blocks, .. } = turn else { return Vec::new() };
    blocks
        .iter()
        .filter_map(|block| block.as_tool_call())
        .filter(|call| call.kind == aui_protocol::ToolKind::Read)
        .map(|call| PathBuf::from(call.target.split(':').next().unwrap_or(&call.target)))
        .filter(|path| path.starts_with(dir) && path.extension().is_some_and(|ext| ext == "md") && path.file_name().is_some_and(|n| n != crate::memory::INDEX))
        .collect()
}

/// The text a yes appends to the repo's `AGENTS.md`: the note under its
/// title.
pub fn agents_md_section(title: &str, body: &str) -> String {
    format!("\n## {title}\n\n{}\n", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{save, Note};

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-lint-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A stale note (it names a file that is gone) and two that contradict:
    /// the prompt carries both and flags the missing path; one fix is
    /// applied, one note at a time, and the old text is in the log.
    #[test]
    fn planted_stale_and_contradicting_notes_are_flagged_and_one_is_fixed() {
        let repo = temp("repo");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join("src/lib.rs"), "").unwrap();
        let dir = temp("notes");
        let note = |title: &str, body: &str| Note { title: title.into(), description: title.into(), kind: "project".into(), body: body.into() };
        save(&dir, &note("Postcode code", "The check lives in `src/postcode.rs` and `src/lib.rs`.")).unwrap();
        save(&dir, &note("Strip spaces", "Strip spaces from GB postcodes before checking.")).unwrap();
        save(&dir, &note("Never strip spaces", "Never strip spaces from GB postcodes: the API rejects them.")).unwrap();
        assert_eq!(missing_paths("see `src/postcode.rs`, `src/lib.rs`, `https://x.io/a.b` and `a b/c.d`", &repo), ["src/postcode.rs"]);
        let prompt = lint_prompt(&dir, &repo).expect("notes");
        assert!(prompt.contains("(these paths no longer exist: src/postcode.rs)"), "{prompt}");
        assert!(prompt.contains("### Strip spaces") && prompt.contains("### Never strip spaces"));
        // The cheap model's answer: remove the contradicted, older note.
        let fix = parse_fix("Sure: {\"action\":\"delete\",\"title\":\"Strip spaces\",\"why\":\"contradicted\"}").expect("a fix");
        assert_eq!(fix, Fix::Delete { title: "Strip spaces".into() });
        apply(&dir, &fix).unwrap();
        assert_eq!(crate::memory::list(&dir).len(), 2, "one note at a time");
        let log = std::fs::read_to_string(dir.join(crate::memory::LOG)).unwrap();
        assert!(log.contains("· removed · Strip spaces") && log.contains("> Strip spaces from GB postcodes"), "{log}");
        assert_eq!(parse_fix("{\"action\":\"none\"}"), None);
        assert_eq!(parse_fix("no json"), None);
        let rewrite = parse_fix("{\"action\":\"rewrite\",\"title\":\"Postcode code\",\"body\":\"It lives in `src/lib.rs`.\"}").unwrap();
        apply(&dir, &rewrite).unwrap();
        assert!(lint_prompt(&dir, &repo).unwrap().contains("It lives in `src/lib.rs`.") && !lint_prompt(&dir, &repo).unwrap().contains("(these paths no longer exist"));
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_suggestion_comes_once_at_the_fifth_session() {
        let conn = Connection::open_in_memory().unwrap();
        let note = Path::new("/m/gb.md");
        for n in 1..SUGGEST_AFTER {
            assert!(!record_use(&conn, note, &format!("s-{n}")).unwrap(), "use {n}");
        }
        assert!(!record_use(&conn, note, "s-1").unwrap(), "the same session twice is one use");
        assert!(record_use(&conn, note, "s-5").unwrap(), "the fifth session");
        assert!(!record_use(&conn, note, "s-6").unwrap(), "once, never again");
    }

    #[test]
    fn the_lint_runs_at_most_daily_and_a_turns_reads_are_its_uses() {
        let state = temp("stamp");
        let stamp = stamp_path(&state, Path::new("/m/notes"));
        assert!(lint_due(&stamp));
        std::fs::create_dir_all(stamp.parent().unwrap()).unwrap();
        std::fs::write(&stamp, "").unwrap();
        assert!(!lint_due(&stamp), "not again the same day");
        use aui_protocol::{Block, ToolBody, ToolKind, ToolStatus, Turn, TurnMeta};
        let read = |target: &str| Block::ToolCall {
            id: target.into(), kind: ToolKind::Read, verb: "Read".into(), target: target.into(),
            status: ToolStatus::Success, duration_ms: None, body: ToolBody::None, diff_stat: None,
        };
        let turn = Turn::Assistant { id: "a".into(), blocks: vec![read("/m/notes/gb.md"), read("/m/notes/MEMORY.md"), read("/repo/src/lib.rs")], meta: TurnMeta::default(), timestamp: None };
        assert_eq!(notes_read(&turn, Path::new("/m/notes")), [PathBuf::from("/m/notes/gb.md")]);
        assert_eq!(agents_md_section("GB rule", "Never strip.\n"), "\n## GB rule\n\nNever strip.\n");
        let _ = std::fs::remove_dir_all(&state);
    }
}
