//! Baaz's own transcript store (docs/85 11.1, W5): each session's folded
//! turns, kept in `<state>/transcript.db` for the UI — reopen, paging,
//! search, digests. The provider's own files stay the authority for resume
//! and fork (rule 18): this store never feeds a provider, and it never keeps
//! a second raw copy, only the folded turns the transcript draws.
//!
//! A separate file from `baaz.db`: transcripts are large and rebuildable,
//! while `baaz.db` is small, backed up before every migration, and not
//! rebuildable (the size test is `a_long_session_is_measured`).
//!
//! **Freshness.** Each session row keeps the provider file's fingerprint
//! (`source`: length and a hash of the tail of Claude's jsonl or Codex's
//! rollout, [`file_fingerprint`]; Muse's index stamp) as of the last write. A reopen serves from the store
//! only when the provider's current fingerprint matches; otherwise it takes
//! the provider path and rewrites the row — so a session continued in the
//! terminal is never shown stale.
//!
//! **Sessions Baaz never opened** (docs/87 18.7): an agent's `session_read`
//! of one folds the provider's own file to text-only turns on demand
//! ([`read_or_fold`]) and keeps them under a [`LITE`] fingerprint. A reopen
//! never serves those (its fingerprint never equals a [`LITE`] one), so the
//! first real reopen rebuilds the row from the provider in full.
//!
//! **Format.** [`FORMAT`] is stamped in `meta`; a different stamp drops every
//! row (the store is rebuilt lazily from the providers as sessions reopen).
//! Deleting the file is the same rebuild.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension as _};

/// The store's format: bump it when a stored turn's shape changes.
pub const FORMAT: u32 = 1;

/// `<state>/transcript.db`.
pub fn db_path(state: &Path) -> PathBuf {
    state.join("transcript.db")
}

/// Open (creating) the store at `path`, dropping every row when its format
/// is not [`FORMAT`].
pub fn open_at(path: &Path) -> rusqlite::Result<Connection> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; PRAGMA journal_size_limit=8388608;")?;
    let _ = conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
    conn.execute_batch(
        "PRAGMA synchronous=NORMAL;
         CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE IF NOT EXISTS session(
             provider TEXT NOT NULL,
             session_id TEXT NOT NULL,
             source TEXT NOT NULL,
             turns INTEGER NOT NULL,
             updated_at INTEGER NOT NULL,
             PRIMARY KEY(provider, session_id)) WITHOUT ROWID;
         CREATE TABLE IF NOT EXISTS turn(
             provider TEXT NOT NULL,
             session_id TEXT NOT NULL,
             seq INTEGER NOT NULL,
             turn_id TEXT NOT NULL,
             body TEXT NOT NULL,
             PRIMARY KEY(provider, session_id, seq)) WITHOUT ROWID;",
    )?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    let stamped: Option<String> =
        conn.query_row("SELECT value FROM meta WHERE key = 'format'", [], |row| row.get(0)).optional()?;
    if stamped.as_deref() != Some(&FORMAT.to_string()) {
        conn.execute_batch("DELETE FROM turn; DELETE FROM session;")?;
        conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES ('format', ?1)", params![FORMAT.to_string()])?;
    }
    Ok(conn)
}

/// The kept connection to this run's store, opened on first use (and again
/// when the state dir moved or the file went).
static KEPT: Mutex<Option<(PathBuf, Connection)>> = Mutex::new(None);

/// Run `work` on the kept connection to `<state>/transcript.db`. Blocking:
/// call it off the UI thread.
pub fn with_store<R>(state: &Path, work: impl FnOnce(&mut Connection) -> R) -> rusqlite::Result<R> {
    let path = db_path(state);
    let mut kept = KEPT.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let fresh = kept.as_ref().is_some_and(|(at, _)| *at == path && path.exists());
    if !fresh {
        *kept = None;
        *kept = Some((path.clone(), open_at(&path)?));
    }
    let (_, conn) = kept.as_mut().expect("just kept");
    Ok(work(conn))
}

/// What the store holds for one session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The provider file's fingerprint as of the last write.
    pub source: String,
    /// How many turns are kept.
    pub turns: usize,
}

/// The row for `(provider, session_id)`, when kept.
pub fn stored(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<Stored>> {
    conn.prepare_cached("SELECT source, turns FROM session WHERE provider = ?1 AND session_id = ?2")?
        .query_row(params![provider, session_id], |row| Ok(Stored { source: row.get(0)?, turns: row.get::<_, i64>(1)? as usize }))
        .optional()
}

/// Write `turns` as the session's whole transcript from position `from` on
/// (earlier rows stand; later rows past the new end go), with the provider
/// file's fingerprint `source`. One transaction.
pub fn write_from(
    conn: &mut Connection,
    provider: &str,
    session_id: &str,
    source: &str,
    turns: &[aui_protocol::Turn],
    from: usize,
) -> rusqlite::Result<()> {
    write_tail(conn, provider, session_id, source, from, turns.get(from..).unwrap_or_default())
}

/// Write `tail` as the session's turns from position `start` on, so the
/// transcript is `start + tail.len()` turns long (the rows before `start`
/// stand). One transaction.
pub fn write_tail(
    conn: &mut Connection,
    provider: &str,
    session_id: &str,
    source: &str,
    start: usize,
    tail: &[aui_protocol::Turn],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.prepare_cached("DELETE FROM turn WHERE provider = ?1 AND session_id = ?2 AND seq >= ?3")?
        .execute(params![provider, session_id, start as i64])?;
    {
        let mut insert = tx.prepare_cached("INSERT INTO turn(provider, session_id, seq, turn_id, body) VALUES (?1, ?2, ?3, ?4, ?5)")?;
        for (offset, turn) in tail.iter().enumerate() {
            let body = serde_json::to_string(turn).map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            insert.execute(params![provider, session_id, (start + offset) as i64, turn.id(), body])?;
        }
    }
    tx.prepare_cached(
        "INSERT INTO session(provider, session_id, source, turns, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(provider, session_id) DO UPDATE SET source = ?3, turns = ?4, updated_at = ?5",
    )?
    .execute(params![provider, session_id, source, (start + tail.len()) as i64, crate::usage::now_ms()])?;
    tx.commit()
}

/// What a settled turn (or a reopen's replayed history) writes: the
/// session's turns from `start` on (`tail`), with the provider's fingerprint
/// taken now. When the store does not already hold every turn before
/// `start`, the row is forgotten instead — a partial transcript is never
/// served, and the next reopen rebuilds it from the provider. Blocking.
pub fn record_settled(state: &Path, provider: &str, session_id: &str, muse_stamp: Option<i64>, start: usize, tail: &[aui_protocol::Turn]) {
    let source = source_now(provider, session_id, muse_stamp);
    let written = with_store(state, |conn| -> rusqlite::Result<()> {
        let row = stored(conn, provider, session_id)?;
        // A text-only fold ([`LITE`]) is never the head of a real transcript.
        let held = row.filter(|row| !row.source.starts_with(LITE)).map_or(0, |row| row.turns);
        if start == 0 || held >= start {
            write_tail(conn, provider, session_id, &source, start, tail)
        } else {
            forget(conn, provider, session_id)
        }
    });
    match written {
        Ok(Ok(())) => {}
        Ok(Err(error)) | Err(error) => crate::baaz_log!("transcripts: {provider} {session_id} not kept ({error})"),
    }
}

/// The session's kept turns in order, or `None` when it is not kept. A row
/// that no longer parses (a format the stamp missed) reads as not kept.
pub fn read(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<Vec<aui_protocol::Turn>>> {
    if stored(conn, provider, session_id)?.is_none() {
        return Ok(None);
    }
    let mut statement = conn.prepare_cached("SELECT body FROM turn WHERE provider = ?1 AND session_id = ?2 ORDER BY seq")?;
    let rows = statement.query_map(params![provider, session_id], |row| row.get::<_, String>(0))?;
    let mut turns = Vec::new();
    for body in rows {
        match serde_json::from_str::<aui_protocol::Turn>(&body?) {
            Ok(turn) => turns.push(turn),
            Err(_) => return Ok(None),
        }
    }
    Ok(Some(turns))
}

/// The kept turns of `(provider, session_id)` when they are as fresh as the
/// provider's own record ([`source_now`] equals the kept fingerprint):
/// what a reopen shows instead of asking the provider for its history.
/// `None` when not kept, stale, or unreadable. Blocking.
pub fn kept_if_fresh(state: &Path, provider: &str, session_id: &str, muse_stamp: Option<i64>) -> Option<Vec<aui_protocol::Turn>> {
    let source = source_now(provider, session_id, muse_stamp);
    if source.is_empty() {
        return None;
    }
    with_store(state, |conn| -> rusqlite::Result<Option<Vec<aui_protocol::Turn>>> {
        match stored(conn, provider, session_id)? {
            Some(row) if row.source == source && row.turns > 0 => read(conn, provider, session_id),
            _ => Ok(None),
        }
    })
    .ok()?
    .ok()?
}

/// The fingerprint prefix of a text-only fold of a provider file
/// ([`lite_turns`]): what `session_read` keeps for a session Baaz never
/// opened. Never equal to [`source_now`], so a reopen never serves it.
pub const LITE: &str = "lite:";

/// `session_read`'s turns (docs/87 18.7): the store's, when Baaz keeps the
/// session; else a text-only fold of the provider's own file (`file`: its
/// jsonl or rollout), kept under a [`LITE`] fingerprint and refolded when
/// the file has changed since. `None` when neither has turns (Muse keeps no
/// file Baaz can read). Blocking.
pub fn read_or_fold(conn: &mut Connection, provider: &str, session_id: &str, file: Option<&Path>) -> rusqlite::Result<Option<Vec<aui_protocol::Turn>>> {
    let row = stored(conn, provider, session_id)?;
    if row.as_ref().is_some_and(|row| !row.source.starts_with(LITE)) {
        return read(conn, provider, session_id);
    }
    let Some(file) = file else { return Ok(None) };
    let lite = format!("{LITE}{}", file_fingerprint(file));
    if lite == LITE {
        return Ok(None);
    }
    if row.is_some_and(|row| row.source == lite) {
        return read(conn, provider, session_id);
    }
    let Ok(text) = std::fs::read_to_string(file) else { return Ok(None) };
    let turns = lite_turns(provider, &text);
    if turns.is_empty() {
        return Ok(None);
    }
    write_from(conn, provider, session_id, &lite, &turns, 0)?;
    Ok(Some(turns))
}

/// A provider file folded to text-only turns: each real prompt a user turn,
/// the replies after it one assistant turn of text blocks (tool calls,
/// thinking and injected context are left out). Claude Code's jsonl: a
/// prompt is [`crate::import::claude_prompt`], a reply an `assistant` line's
/// `text` blocks (sidechains skipped). Codex's rollout: a prompt is
/// [`crate::import::codex_prompt`], a reply an assistant `response_item`'s
/// `output_text`. Anything else folds to nothing.
pub fn lite_turns(provider: &str, text: &str) -> Vec<aui_protocol::Turn> {
    use aui_protocol::{Block, Turn};
    let mut turns: Vec<Turn> = Vec::new();
    let push_reply = |turns: &mut Vec<Turn>, reply: &str| {
        let reply = reply.trim();
        if reply.is_empty() {
            return;
        }
        match turns.last_mut() {
            Some(Turn::Assistant { blocks, .. }) => blocks.push(Block::text(reply)),
            _ => {
                let id = format!("lite-{}", turns.len());
                turns.push(Turn::Assistant { id, blocks: vec![Block::text(reply)], meta: Default::default(), timestamp: None });
            }
        }
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let prompt = match provider {
            "claude-code" => crate::import::claude_prompt(&value),
            "codex" => crate::import::codex_prompt(&value),
            _ => return Vec::new(),
        };
        if let Some(prompt) = prompt {
            let id = format!("lite-{}", turns.len());
            turns.push(Turn::User { id, text: prompt.to_owned(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None });
            continue;
        }
        let replies: Vec<&str> = match provider {
            "claude-code" if value.get("type").and_then(|v| v.as_str()) == Some("assistant")
                && value.get("isSidechain").and_then(|v| v.as_bool()) != Some(true) =>
            {
                value
                    .pointer("/message/content")
                    .and_then(|v| v.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("text"))
                            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            "codex" if value.get("type").and_then(|v| v.as_str()) == Some("response_item")
                && value.pointer("/payload/type").and_then(|v| v.as_str()) == Some("message")
                && value.pointer("/payload/role").and_then(|v| v.as_str()) == Some("assistant") =>
            {
                value
                    .pointer("/payload/content")
                    .and_then(|v| v.as_array())
                    .map(|blocks| {
                        blocks
                            .iter()
                            .filter(|b| b.get("type").and_then(|v| v.as_str()) == Some("output_text"))
                            .filter_map(|b| b.get("text").and_then(|v| v.as_str()))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            _ => Vec::new(),
        };
        for reply in replies {
            push_reply(&mut turns, reply);
        }
    }
    turns
}

/// How long an archived session keeps its stored turns (docs/85 11.4).
pub const KEEP_ARCHIVED_DAYS: i64 = 183;

/// Retention (docs/85 11.4): forget the stored turns of `sessions` (each
/// archived for longer than [`KEEP_ARCHIVED_DAYS`], the caller's choice) —
/// but only those whose provider record is still there to reopen from
/// ([`source_now`] names it); any other keeps its turns. Their digest and
/// search rows are not touched. Returns how many were pruned. Blocking.
pub fn prune_archived(state: &Path, sessions: &[(String, String, Option<i64>)]) -> usize {
    let reopenable: Vec<&(String, String, Option<i64>)> =
        sessions.iter().filter(|(provider, id, stamp)| !source_now(provider, id, *stamp).is_empty()).collect();
    if reopenable.is_empty() {
        return 0;
    }
    with_store(state, |conn| {
        let mut pruned = 0;
        for (provider, id, _) in reopenable {
            if stored(conn, provider, id).ok().flatten().is_some() && forget(conn, provider, id).is_ok() {
                pruned += 1;
            }
        }
        pruned
    })
    .unwrap_or(0)
}

/// Forget one session's turns (it is rebuilt from its provider next time).
pub fn forget(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM turn WHERE provider = ?1 AND session_id = ?2", params![provider, session_id])?;
    conn.execute("DELETE FROM session WHERE provider = ?1 AND session_id = ?2", params![provider, session_id])?;
    Ok(())
}

/// The fingerprint of `(provider, session_id)`'s own record right now:
/// Claude Code's jsonl and Codex's rollout by file ([`file_fingerprint`]),
/// Muse by its index stamp (`muse_stamp`). Empty when nothing is known —
/// never equal to a kept one, so the provider path is taken. Blocking.
pub fn source_now(provider: &str, session_id: &str, muse_stamp: Option<i64>) -> String {
    match provider {
        "muse" => muse_stamp.map(|stamp| format!("muse:{stamp}")).unwrap_or_default(),
        _ => crate::provider_search::stored_file(provider, session_id).map(|path| file_fingerprint(&path)).unwrap_or_default(),
    }
}

/// A provider file's fingerprint: its length and a hash of its last
/// [`FINGERPRINT_TAIL`] bytes. Empty for a file that is not there (never
/// equal to a kept one).
///
/// Not the modified time: Claude Code re-stamps its jsonl's mtime on every
/// resume without changing a byte (found at Gate 11: 203,898 bytes before
/// and after, a new mtime each open), so an mtime fingerprint missed the
/// store on every second reopen. These files only grow, so a turn written
/// since always moves the length; the tail hash catches a rewrite in place.
pub fn file_fingerprint(path: &Path) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return String::new() };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else { return String::new() };
    let mut tail = Vec::new();
    let start = len.saturating_sub(FINGERPRINT_TAIL);
    if file.seek(SeekFrom::Start(start)).is_err() || file.take(FINGERPRINT_TAIL).read_to_end(&mut tail).is_err() {
        return String::new();
    }
    // FNV-1a, 64-bit: stable across builds and platforms.
    let hash = tail.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3));
    format!("{len}:{hash:016x}")
}

/// How much of a provider file's end its fingerprint reads.
pub const FINGERPRINT_TAIL: u64 = 64 * 1024;

/// One turn of a synthetic long session for the size test and the perf
/// budget: an ask, or an answer with a paragraph and a finished tool card —
/// about the shape of a real coding session's turns.
pub fn synthetic_turn(n: usize) -> aui_protocol::Turn {
    use aui_protocol::{Block, Turn, TurnMeta};
    if n % 2 == 0 {
        Turn::User { id: format!("u{n}"), text: format!("Please look at module {n} and fix the failing test there."), attachments: Vec::new(), mentions: Vec::new(), timestamp: None }
    } else {
        Turn::Assistant {
            id: format!("a{n}"),
            blocks: vec![
                Block::Text { text: "I read the module, found the off-by-one in the loop bound, and fixed it; the test passes now. ".repeat(4), streaming: false },
                Block::Text { text: format!("Changed src/module_{n}.rs:42 — the range now stops before len."), streaming: false },
            ],
            meta: TurnMeta { model: "claude-sonnet-5".into(), tokens_in: 12_000, tokens_out: 400, ..TurnMeta::default() },
            timestamp: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::{Block, Turn, TurnMeta};

    fn turn(n: usize) -> Turn {
        if n % 2 == 0 {
            Turn::User { id: format!("u{n}"), text: format!("ask {n}"), attachments: Vec::new(), mentions: Vec::new(), timestamp: None }
        } else {
            Turn::Assistant {
                id: format!("a{n}"),
                blocks: vec![Block::Text { text: format!("answer {n}"), streaming: false }],
                meta: TurnMeta { tokens_in: n as u64, ..TurnMeta::default() },
                timestamp: None,
            }
        }
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-transcripts-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    fn texts(turns: &[Turn]) -> Vec<String> {
        turns.iter().map(crate::session_tools::turn_text).collect()
    }

    /// docs/87 18.7: a Claude Code session Baaz never opened (a real stored
    /// jsonl, no CLI) reads as its prompts and replies, kept under a lite
    /// fingerprint a reopen never serves.
    #[test]
    fn a_never_opened_claude_session_reads_from_its_file() {
        let dir = temp("lite-claude");
        let file = dir.join("af18b5bb.jsonl");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/claude-code/stored-history.jsonl"), &file).expect("fixture");
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        let turns = read_or_fold(&mut conn, "claude-code", "af18b5bb", Some(&file)).expect("read").expect("turns");
        let lines = texts(&turns);
        assert_eq!(lines.first().map(String::as_str), Some("User: Say R1 and nothing else"), "{lines:?}");
        assert!(lines.iter().any(|t| t == "Assistant: R1"), "{lines:?}");
        assert!(lines.iter().all(|t| !t.contains("<system-reminder>")), "no injected context: {lines:?}");
        let row = stored(&conn, "claude-code", "af18b5bb").expect("row").expect("kept");
        assert!(row.source.starts_with(LITE), "{}", row.source);
        assert_ne!(row.source, file_fingerprint(&file), "a reopen never takes the lite copy as fresh");
        // Read again: served from the store, the same turns.
        assert_eq!(read_or_fold(&mut conn, "claude-code", "af18b5bb", Some(&file)).expect("read"), Some(turns.clone()));
        // The file grew (the session went on in the terminal): refolded.
        let mut more = std::fs::read_to_string(&file).expect("read");
        more.push_str(r#"{"type":"user","isSidechain":false,"message":{"role":"user","content":[{"type":"text","text":"Say R2"}]},"uuid":"x"}"#);
        more.push('\n');
        std::fs::write(&file, more).expect("grow");
        let again = read_or_fold(&mut conn, "claude-code", "af18b5bb", Some(&file)).expect("read").expect("turns");
        assert_eq!(texts(&again).last().map(String::as_str), Some("User: Say R2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/87 18.7: the same for a Codex session (a real rollout from the
    /// Stage 17 bench, trimmed): the typed prompt, not the injected context,
    /// and the agent's replies.
    #[test]
    fn a_never_opened_codex_session_reads_from_its_rollout() {
        let dir = temp("lite-codex");
        let file = dir.join("rollout.jsonl");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/codex/rollout-stored.ndjson"), &file).expect("fixture");
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        let turns = read_or_fold(&mut conn, "codex", "01a216a3", Some(&file)).expect("read").expect("turns");
        let lines = texts(&turns);
        assert_eq!(turns.len(), 2, "one prompt, one reply turn: {lines:?}");
        assert!(lines[0].starts_with("User: The tests in this repo fail. Find the bug in calc.py"), "{lines:?}");
        assert!(lines[1].contains("All tests pass: 2/2."), "{lines:?}");
        assert!(lines.iter().all(|t| !t.contains("<environment_context>")), "{lines:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A session Baaz keeps for real is read from the store, never refolded;
    /// a lite copy never becomes the head of a real transcript; no file and
    /// no row is no turns.
    #[test]
    fn the_real_store_wins_and_a_lite_copy_never_heads_a_transcript() {
        let dir = temp("lite-real");
        let file = dir.join("rollout.jsonl");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/codex/rollout-stored.ndjson"), &file).expect("fixture");
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        write_from(&mut conn, "codex", "kept", "real-source", &[turn(0), turn(1)], 0).expect("write");
        assert_eq!(read_or_fold(&mut conn, "codex", "kept", Some(&file)).expect("read"), Some(vec![turn(0), turn(1)]));
        assert_eq!(read_or_fold(&mut conn, "codex", "none", None).expect("read"), None);
        assert_eq!(read_or_fold(&mut conn, "muse", "m", Some(&file)).expect("read"), None, "no lite fold for Muse");
        drop(conn);
        // A lite copy, then a settled tail that does not start at 0: the
        // partial transcript is forgotten, never served.
        let lite = with_store(&dir, |conn| read_or_fold(conn, "codex", "lite", Some(&file))).expect("store").expect("read");
        assert_eq!(lite.map(|turns| turns.len()), Some(2), "a two-turn lite copy");
        record_settled(&dir, "codex", "lite", None, 2, &[turn(2)]);
        assert_eq!(with_store(&dir, |conn| stored(conn, "codex", "lite")).expect("store").expect("row"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn turns_round_trip_and_a_turn_end_rewrites_only_the_tail() {
        let dir = temp("roundtrip");
        let mut conn = open_at(&db_path(&dir)).expect("opens");
        let mut turns: Vec<Turn> = (0..6).map(turn).collect();
        write_from(&mut conn, "claude-code", "s-1", "10:1", &turns, 0).expect("writes");
        assert_eq!(read(&conn, "claude-code", "s-1").unwrap(), Some(turns.clone()));
        assert_eq!(stored(&conn, "claude-code", "s-1").unwrap(), Some(Stored { source: "10:1".into(), turns: 6 }));
        // A turn finished: the last assistant turn grew, two more came.
        turns[5] = turn(105);
        turns.push(turn(6));
        turns.push(turn(7));
        write_from(&mut conn, "claude-code", "s-1", "20:2", &turns, 5).expect("tail");
        assert_eq!(read(&conn, "claude-code", "s-1").unwrap(), Some(turns.clone()));
        assert_eq!(stored(&conn, "claude-code", "s-1").unwrap().unwrap().turns, 8);
        assert_eq!(read(&conn, "codex", "s-1").unwrap(), None, "keyed by provider too");
        forget(&conn, "claude-code", "s-1").unwrap();
        assert_eq!(read(&conn, "claude-code", "s-1").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The size test that chose a file of its own: a 1,000-turn session.
    #[test]
    fn a_long_session_is_measured() {
        let dir = temp("size");
        let path = db_path(&dir);
        let mut conn = open_at(&path).expect("opens");
        let turns: Vec<Turn> = (0..1000).map(synthetic_turn).collect();
        write_from(&mut conn, "claude-code", "long", "1:1", &turns, 0).expect("writes");
        let _ = conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()));
        let bytes = std::fs::metadata(&path).unwrap().len();
        eprintln!("transcript.db with one 1,000-turn session: {bytes} bytes");
        assert!(bytes > 400_000, "a long session is large next to baaz.db's ~700 KB: {bytes}");
        let at = std::time::Instant::now();
        assert_eq!(read(&conn, "claude-code", "long").unwrap().map(|t| t.len()), Some(1000));
        eprintln!("read 1,000 turns in {:?}", at.elapsed());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/85 11.4: a long-archived session gives up its turns only when
    /// its provider record is there to reopen from.
    #[test]
    fn pruning_keeps_what_the_provider_could_not_give_back() {
        let dir = temp("prune");
        with_store(&dir, |conn| {
            write_from(conn, "muse", "m-old", "muse:7", &[turn(0)], 0).unwrap();
            write_from(conn, "codex", "c-gone", "1:1", &[turn(0)], 0).unwrap();
        })
        .unwrap();
        let pruned = prune_archived(&dir, &[("muse".into(), "m-old".into(), Some(7)), ("codex".into(), "c-gone".into(), None)]);
        assert_eq!(pruned, 1, "the Muse one (its index still names it) goes; the Codex one has no file and stays");
        with_store(&dir, |conn| {
            assert!(read(conn, "muse", "m-old").unwrap().is_none());
            assert!(read(conn, "codex", "c-gone").unwrap().is_some());
        })
        .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_format_drops_every_row_and_a_deleted_file_starts_empty() {
        let dir = temp("format");
        let path = db_path(&dir);
        {
            let mut conn = open_at(&path).expect("opens");
            write_from(&mut conn, "codex", "t-1", "1:1", &[turn(0)], 0).expect("writes");
            conn.execute("UPDATE meta SET value = '0' WHERE key = 'format'", []).unwrap();
        }
        let conn = open_at(&path).expect("reopens");
        assert_eq!(read(&conn, "codex", "t-1").unwrap(), None, "a format change rebuilds");
        drop(conn);
        std::fs::remove_file(&path).unwrap();
        let conn = open_at(&path).expect("recreated");
        assert_eq!(read(&conn, "codex", "t-1").unwrap(), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Gate 11: a resume that re-stamps the file's mtime without writing
    /// keeps the fingerprint; a turn appended, or a byte rewritten in the
    /// tail, moves it.
    #[test]
    fn a_touched_file_keeps_its_fingerprint_and_a_written_one_does_not() {
        let dir = std::env::temp_dir().join(format!("baaz-fingerprint-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("s.jsonl");
        std::fs::write(&path, "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n").unwrap();
        let first = file_fingerprint(&path);
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(60);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert_eq!(file_fingerprint(&path), first, "a touch is not a change");
        std::fs::write(&path, "{\"type\":\"user\"}\n{\"type\":\"assistanT\"}\n").unwrap();
        let rewritten = file_fingerprint(&path);
        assert_ne!(rewritten, first, "a rewrite in place moves it");
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"user\"}\n");
        std::fs::write(&path, text).unwrap();
        assert_ne!(file_fingerprint(&path), rewritten, "an appended turn moves it");
        assert_eq!(file_fingerprint(&dir.join("missing.jsonl")), "", "no file, no fingerprint");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
