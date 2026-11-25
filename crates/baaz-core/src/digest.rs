//! Session digests (docs/85 12.4): when a session goes idle, its own
//! provider writes two lines about it plus its rules and decisions, on the
//! cheapest model, for search (12.3's `session_search` returns them and
//! matches them).
//!
//! * **When.** Ten minutes after a live turn ended with nothing running
//!   ([`IDLE`]), at most once per [`TURNS_PER_DIGEST`] new turns (the first
//!   digest needs one). Side sessions and browser chats never get one.
//! * **What it reads.** The handoff pack ([`crate::handoff::pack_text`]),
//!   capped at [`INPUT_TOKENS`] — never the transcript itself — scrubbed of
//!   obvious secrets before and after ([`crate::scrub`]).
//! * **Who pays.** Rule 27: only an account billed by subscription; the
//!   call goes through the utility-completion seam, so a test never reaches
//!   a CLI.
//! * **Where.** The transcript store's `digest` table, beside the turns
//!   (`transcript.db`): a table of its own, so a session's turns being
//!   rewritten never drops its digest.

use rusqlite::{params, Connection, OptionalExtension};

/// How long after a turn a session counts as idle.
pub const IDLE: std::time::Duration = std::time::Duration::from_secs(600);

/// New turns between digests.
pub const TURNS_PER_DIGEST: usize = 5;

/// The most of the pack the digest model reads, in tokens (~4 chars each).
pub const INPUT_TOKENS: usize = 8000;

/// Whether a digest is due: idle long enough, nothing running, not a side
/// session or browser chat, a subscription, and enough new turns since the
/// last digest (`digested_turns`: the turn count it was written at).
pub fn due(idle_ms: i64, running: bool, excluded: bool, subscription: bool, turns: usize, digested_turns: Option<usize>) -> bool {
    if running || excluded || !subscription || idle_ms < IDLE.as_millis() as i64 {
        return false;
    }
    match digested_turns {
        None => turns > 0,
        Some(at) => turns >= at + TURNS_PER_DIGEST,
    }
}

/// The prompt: the pack, capped and scrubbed.
pub fn prompt(pack_text: &str) -> String {
    let capped: String = crate::scrub::scrub(pack_text).chars().take(INPUT_TOKENS * 4).collect();
    // The record is fenced and the instruction comes after it: the pack is
    // written to hand a session on, and read first it was taken as one
    // ("Acknowledged — I've picked up the session…", seen live, Gate 12).
    format!(
        "You are writing an index entry about a past coding session. Do not continue the session and do not reply to it.\n\n\
         <session-record>\n{capped}\n</session-record>\n\n\
         Write only the digest of the session in <session-record>, for a search index:\n\
         line 1-2: what the session was about and where it ended, in plain words;\n\
         then `Rules:` with any standing instructions the user gave, one per line (or none);\n\
         then `Decisions:` with what was decided and why, one per line (or none).\n\
         No preamble, under 120 words, no secrets."
    )
}

/// The model's answer, cleaned: trimmed, scrubbed, capped at 1,200 chars.
pub fn clean(answer: &str) -> Option<String> {
    let text = crate::scrub::scrub(answer.trim());
    let text: String = text.chars().take(1200).collect();
    (!text.trim().is_empty()).then_some(text)
}

/// Make sure the store has its `digest` table (an older file gains it; its
/// turns stay).
pub fn ensure_table(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS digest(
             provider TEXT NOT NULL,
             session_id TEXT NOT NULL,
             text TEXT NOT NULL,
             turns INTEGER NOT NULL,
             written_at INTEGER NOT NULL,
             PRIMARY KEY(provider, session_id)) WITHOUT ROWID;",
    )
}

/// Keep `text` as the digest of `(provider, session_id)`, written at `turns`.
pub fn save(conn: &Connection, provider: &str, session_id: &str, text: &str, turns: usize, now_ms: i64) -> rusqlite::Result<()> {
    ensure_table(conn)?;
    conn.execute(
        "INSERT OR REPLACE INTO digest(provider, session_id, text, turns, written_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![provider, session_id, text, turns as i64, now_ms],
    )
    .map(|_| ())
}

/// The digest of a session and the turn count it was written at.
pub fn load(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<(String, usize)>> {
    ensure_table(conn)?;
    conn.query_row(
        "SELECT text, turns FROM digest WHERE provider = ?1 AND session_id = ?2",
        params![provider, session_id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)),
    )
    .optional()
}

/// Sessions whose digest holds every word of `query` (case-blind), as
/// search hits with the digest for snippet.
pub fn matching(conn: &Connection, query: &str, limit: usize) -> rusqlite::Result<Vec<crate::search::SessionHit>> {
    ensure_table(conn)?;
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).filter(|w| w.len() > 2).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement = conn.prepare_cached("SELECT session_id, text FROM digest ORDER BY written_at DESC")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut hits = Vec::new();
    for row in rows {
        let (session_id, text) = row?;
        let lower = text.to_lowercase();
        if words.iter().any(|word| lower.contains(word.as_str())) {
            hits.push(crate::search::SessionHit { session_id, label: String::new(), snippet: text, workspace: None });
            if hits.len() == limit {
                break;
            }
        }
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: i64 = 60_000;

    #[test]
    fn the_trigger_waits_for_idle_and_new_turns() {
        assert!(!due(9 * MIN, false, false, true, 3, None), "ten minutes idle first");
        assert!(due(10 * MIN, false, false, true, 1, None), "the first digest needs one turn");
        assert!(!due(10 * MIN, false, false, true, 0, None));
        assert!(!due(30 * MIN, true, false, true, 3, None), "never while running");
        assert!(!due(30 * MIN, false, true, true, 3, None), "never a side session or browser chat");
        assert!(!due(30 * MIN, false, false, false, 3, None), "rule 27: only on a subscription");
        assert!(!due(30 * MIN, false, false, true, 7, Some(3)), "one per five new turns");
        assert!(due(30 * MIN, false, false, true, 8, Some(3)));
    }

    #[test]
    fn the_input_and_the_answer_are_scrubbed_and_capped() {
        let pack = format!("token sk-ant-api03-AbCdEfGhIjKlMnOpQrStUv here\n{}", "x".repeat(100_000));
        let prompt = prompt(&pack);
        assert!(!prompt.contains("sk-ant-api03"), "scrubbed before it leaves");
        assert!(prompt.len() < INPUT_TOKENS * 4 + 1000, "capped at the pack's budget");
        assert_eq!(clean("  \n"), None);
        assert!(clean("We fixed it. password: hunter2hunter2").unwrap().contains("[secret removed]"));
    }

    /// The store gains the table in place (rule 22: an existing file keeps
    /// its turns), digests save, load, replace and match.
    #[test]
    fn an_existing_store_gains_digests_and_keeps_its_turns() {
        let dir = std::env::temp_dir().join(format!("baaz-digest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("transcript.db");
        let mut conn = crate::transcripts::open_at(&path).unwrap();
        crate::transcripts::write_from(&mut conn, "codex", "s-1", "src", &[crate::transcripts::synthetic_turn(0)], 0).unwrap();
        drop(conn);
        let conn = crate::transcripts::open_at(&path).unwrap();
        save(&conn, "codex", "s-1", "Fixed the GB postcode check.\nDecisions: never strip spaces", 4, 1).unwrap();
        assert!(crate::transcripts::read(&conn, "codex", "s-1").unwrap().is_some(), "the turns stay");
        assert_eq!(load(&conn, "codex", "s-1").unwrap().map(|(_, turns)| turns), Some(4));
        save(&conn, "codex", "s-1", "Later digest", 9, 2).unwrap();
        assert_eq!(load(&conn, "codex", "s-1").unwrap().unwrap().0, "Later digest");
        save(&conn, "codex", "s-2", "Postcode spaces decision", 2, 3).unwrap();
        let hits = matching(&conn, "postcode spaces", 5).unwrap();
        assert_eq!(hits.iter().map(|h| h.session_id.as_str()).collect::<Vec<_>>(), ["s-2"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
