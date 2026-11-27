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

pub mod files;
pub use files::{stored_file, stored_files_in, Homes};

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

/// `transcript.db` and its WAL under `state`, in bytes (Settings › Data).
pub fn store_bytes(state: &Path) -> u64 {
    ["transcript.db", "transcript.db-wal"].iter().map(|name| std::fs::metadata(state.join(name)).map(|m| m.len()).unwrap_or(0)).sum()
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

/// Where one session's words come from when the store fills it
/// ([`fill`]): Claude Code's and Codex's own file, or Muse's index text
/// (Muse keeps no file Baaz can fold; its index carries the words) with the
/// index stamp they belong to.
pub enum Words<'a> {
    /// The jsonl or rollout ([`files::stored_file`]); `None` when it is gone.
    File(Option<&'a Path>),
    /// Muse's index `search_text` and its `updated_at_us`.
    Muse { text: &'a str, stamp: Option<i64> },
}

/// Make sure the store holds `(provider, session_id)`'s words (docs/87 18.7,
/// 18.8): a real transcript as fresh as its source stands; anything else
/// (no row, a lite copy of an older file, a real row the provider has moved
/// past) is replaced by a text-only copy under a [`LITE`] fingerprint, which
/// a reopen never serves and which never heads a real transcript. With
/// nothing to copy from (the file is gone, no Muse text), what is kept
/// stays. Blocking.
pub fn fill(conn: &mut Connection, provider: &str, session_id: &str, words: Words<'_>) -> rusqlite::Result<()> {
    let row = stored(conn, provider, session_id)?;
    let (fresh, lite) = match &words {
        Words::File(Some(file)) => {
            let fresh = file_fingerprint(file);
            if fresh.is_empty() {
                return Ok(());
            }
            let lite = format!("{LITE}{fresh}");
            (fresh, lite)
        }
        Words::File(None) => return Ok(()),
        Words::Muse { text, stamp } => {
            if text.trim().is_empty() {
                return Ok(());
            }
            let fresh = source_now("muse", session_id, *stamp);
            let lite = format!("{LITE}{fresh}");
            (fresh, lite)
        }
    };
    if row.as_ref().is_some_and(|row| (row.source == fresh && row.turns > 0) || row.source == lite) {
        return Ok(());
    }
    let turns = match words {
        Words::File(Some(file)) => match std::fs::read_to_string(file) {
            Ok(text) => lite_turns(provider, &text),
            Err(_) => return Ok(()),
        },
        Words::File(None) => return Ok(()),
        Words::Muse { text, .. } => vec![aui_protocol::Turn::Assistant {
            id: "lite-0".into(),
            blocks: vec![aui_protocol::Block::text(text.trim())],
            meta: Default::default(),
            timestamp: None,
        }],
    };
    if turns.is_empty() {
        return Ok(());
    }
    write_from(conn, provider, session_id, &lite, &turns, 0)
}

/// `session_read`'s turns (docs/87 18.7): the store's, after [`fill`] has
/// brought a session Baaz never opened (or one the provider has moved past)
/// in from its own file (`file`: its jsonl or rollout). `None` when the
/// store holds nothing for it. Blocking.
pub fn read_or_fold(conn: &mut Connection, provider: &str, session_id: &str, file: Option<&Path>) -> rusqlite::Result<Option<Vec<aui_protocol::Turn>>> {
    if provider != "muse" {
        fill(conn, provider, session_id, Words::File(file))?;
    }
    read(conn, provider, session_id)
}

/// How much of one session's words search keeps (as the providers' own
/// search text was capped before 18.8).
pub const WORDS_LIMIT: usize = 256 * 1024;

/// The searchable words of the session the store holds (docs/87 18.8): each
/// ask and each reply's text, one per line, capped at [`WORDS_LIMIT`] on a
/// character boundary. Tool cards, thinking and notices are not words.
/// `None` when the store holds nothing for it.
pub fn words(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<String>> {
    use aui_protocol::{Block, Turn};
    let Some(turns) = read(conn, provider, session_id)? else { return Ok(None) };
    let mut out = String::new();
    let mut push = |text: &str| {
        let text = text.trim();
        if text.is_empty() || out.len() >= WORDS_LIMIT {
            return;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(text);
    };
    for turn in &turns {
        match turn {
            Turn::User { text, .. } => push(text),
            Turn::Assistant { blocks, .. } => {
                for block in blocks {
                    if let Block::Text { text, .. } = block {
                        push(text);
                    }
                }
            }
        }
    }
    if out.len() > WORDS_LIMIT {
        let mut at = WORDS_LIMIT;
        while !out.is_char_boundary(at) {
            at -= 1;
        }
        out.truncate(at);
    }
    Ok(Some(out))
}

/// The search sync's bodies (docs/87 18.8): the words for `ids`, whichever
/// provider holds each, read from the store after [`fill`]ing it — Muse's
/// from its index's `search_text` at `index_path` (copied in), Claude
/// Code's and Codex's from their own files under `homes` (folded to text).
/// A session the store already keeps as fresh as its source is not read
/// again. `provider_of` names the Claude Code and Codex sessions (the app's
/// `search_body_owners`); every other id is Muse's. Ids with no words are
/// absent. Blocking: the sync calls it off the UI thread, on its own
/// connection, for changed rows only.
pub fn search_words(
    conn: &mut Connection,
    index_path: &Path,
    provider_of: &std::collections::HashMap<String, String>,
    homes: &Homes,
    ids: &[String],
) -> std::collections::HashMap<String, String> {
    let muse: Vec<String> = ids.iter().filter(|id| !provider_of.contains_key(*id)).cloned().collect();
    for (id, (text, stamp)) in crate::index::read_bodies(index_path, &muse) {
        if let Err(error) = fill(conn, "muse", &id, Words::Muse { text: &text, stamp }) {
            crate::baaz_log!("search: muse {id} not kept ({error})");
        }
    }
    for provider in ["claude-code", "codex"] {
        let wanted: Vec<&str> =
            ids.iter().filter(|id| provider_of.get(*id).is_some_and(|p| p == provider)).map(String::as_str).collect();
        if wanted.is_empty() {
            continue;
        }
        let found = stored_files_in(provider, &wanted, homes);
        for id in wanted {
            if let Err(error) = fill(conn, provider, id, Words::File(found.get(id).map(PathBuf::as_path))) {
                crate::baaz_log!("search: {provider} {id} not kept ({error})");
            }
        }
    }
    ids.iter()
        .filter_map(|id| {
            let provider = provider_of.get(id).map_or("muse", String::as_str);
            words(conn, provider, id).ok().flatten().filter(|text| !text.is_empty()).map(|text| (id.clone(), text))
        })
        .collect()
}

/// A provider file folded to text-only turns: each real prompt a user turn,
/// the replies after it one assistant turn of text blocks (tool calls,
/// thinking and injected context are left out). Claude Code's jsonl: a
/// prompt is [`crate::import::claude_prompt`], a reply an `assistant` line's
/// `text` blocks (sidechains skipped). Codex's rollout: a prompt is
/// [`crate::import::codex_prompt`] — or, in a rollout that has no
/// `user_message` events (an older Codex), a user `response_item`'s text
/// that is not injected context — a reply an assistant `response_item`'s
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
    let codex_events = provider == "codex" && text.contains("\"user_message\"");
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let prompt = match provider {
            "claude-code" => crate::import::claude_prompt(&value).map(str::to_owned),
            "codex" if codex_events => crate::import::codex_prompt(&value).map(str::to_owned),
            "codex" => codex_item_prompt(&value),
            _ => return Vec::new(),
        };
        if let Some(prompt) = prompt {
            let id = format!("lite-{}", turns.len());
            turns.push(Turn::User { id, text: prompt, attachments: Vec::new(), mentions: Vec::new(), timestamp: None });
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

/// A user `response_item`'s typed text in a rollout with no `user_message`
/// events: its text blocks, less the context blocks Codex injects (they
/// open with `<`), one per line. `None` when nothing is left.
fn codex_item_prompt(value: &serde_json::Value) -> Option<String> {
    if value.get("type").and_then(|v| v.as_str()) != Some("response_item")
        || value.pointer("/payload/type").and_then(|v| v.as_str()) != Some("message")
        || value.pointer("/payload/role").and_then(|v| v.as_str()) != Some("user")
    {
        return None;
    }
    let typed: Vec<&str> = value
        .pointer("/payload/content")?
        .as_array()?
        .iter()
        .filter_map(|block| block.get("text").and_then(|v| v.as_str()).map(str::trim))
        .filter(|text| !text.is_empty() && !text.starts_with('<'))
        .collect();
    (!typed.is_empty()).then(|| typed.join("\n"))
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
        _ => files::stored_file(provider, session_id).map(|path| file_fingerprint(&path)).unwrap_or_default(),
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

    /// A session Baaz keeps for real and as fresh as its file is read from
    /// the store, never refolded; one the file has moved past is refolded
    /// (docs/87 18.8: a session continued in the terminal is found by its
    /// new words); a lite copy never becomes the head of a real transcript;
    /// no file and no row is no turns.
    #[test]
    fn the_real_store_wins_and_a_lite_copy_never_heads_a_transcript() {
        let dir = temp("lite-real");
        let file = dir.join("rollout.jsonl");
        std::fs::copy(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/codex/rollout-stored.ndjson"), &file).expect("fixture");
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        write_from(&mut conn, "codex", "kept", &file_fingerprint(&file), &[turn(0), turn(1)], 0).expect("write");
        assert_eq!(read_or_fold(&mut conn, "codex", "kept", Some(&file)).expect("read"), Some(vec![turn(0), turn(1)]));
        write_from(&mut conn, "codex", "stale", "an-older-file", &[turn(0), turn(1)], 0).expect("write");
        let refolded = read_or_fold(&mut conn, "codex", "stale", Some(&file)).expect("read").expect("turns");
        assert!(texts(&refolded)[1].contains("All tests pass: 2/2."), "the file's newer words: {:?}", texts(&refolded));
        assert!(stored(&conn, "codex", "stale").expect("row").expect("kept").source.starts_with(LITE));
        // The file gone: what is kept stays.
        assert_eq!(read_or_fold(&mut conn, "codex", "stale", None).expect("read"), Some(refolded));
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

    /// A Muse index with `rows` of `(id, search_text, updated_at_us)`.
    fn muse_index(path: &Path, rows: &[(&str, &str, i64)]) {
        let _ = std::fs::remove_file(path);
        let conn = Connection::open(path).expect("index");
        conn.execute_batch("CREATE TABLE sessions(session_id TEXT, session_name TEXT, title TEXT, first_user_prompt TEXT, search_text TEXT, updated_at_us INTEGER, workspace_root TEXT)")
            .expect("schema");
        for (id, text, stamp) in rows {
            conn.execute("INSERT INTO sessions(session_id, search_text, updated_at_us) VALUES (?1, ?2, ?3)", params![id, text, stamp]).expect("row");
        }
    }

    /// docs/87 18.8: search's words come from the store, filled first for
    /// every provider — Muse's index text copied in (a session Baaz never
    /// opened is found), Claude Code's and Codex's own files folded to text
    /// (prompts and replies, never tool output) — and a Muse session that
    /// moved on in the CLI is found by its new words.
    #[test]
    fn search_words_fill_the_store_for_every_provider() {
        let dir = temp("search-words");
        let (homes, sources) = files::write_synthetic(&dir, 4);
        let index = dir.join("session-index.db");
        muse_index(&index, &[("m1", "muse words about the kestrel", 10), ("m-empty", "", 10)]);
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        let provider_of: std::collections::HashMap<String, String> = sources.iter().map(|(p, id)| (id.clone(), p.clone())).collect();
        let mut ids: Vec<String> = sources.iter().map(|(_, id)| id.clone()).collect();
        ids.extend(["m1".to_owned(), "m-empty".to_owned(), "gone".to_owned()]);
        let words = search_words(&mut conn, &index, &provider_of, &homes, &ids);
        assert_eq!(words.len(), 5, "four provider files and one Muse text: {:?}", words.keys().collect::<Vec<_>>());
        let claude = &words[&sources[0].1];
        assert!(claude.contains("claude prompt 0 turn 3 about the sidebar") && claude.contains("answer 3 for session 0"), "{claude}");
        assert!(!claude.contains("xxxx"), "tool results are not words");
        let codex = &words[&sources[1].1];
        assert!(codex.contains("codex prompt 1 turn 0 about the router") && codex.contains("codex answer 39 for session 1"));
        assert!(!codex.contains("yyyy"), "tool output is not words");
        assert_eq!(words["m1"], "muse words about the kestrel");
        for (provider, id) in [("muse", "m1"), (sources[0].0.as_str(), sources[0].1.as_str())] {
            assert!(stored(&conn, provider, id).expect("row").expect("kept in the store").source.starts_with(LITE));
        }
        // The session went on in the Muse CLI: a new stamp, new words.
        muse_index(&index, &[("m1", "muse words about the kestrel and the heron", 11)]);
        let again = search_words(&mut conn, &index, &provider_of, &homes, &["m1".to_owned()]);
        assert!(again["m1"].ends_with("the heron"), "{again:?}");
        // A Muse session Baaz keeps for real at that stamp is read from its turns.
        write_from(&mut conn, "muse", "m1", &source_now("muse", "m1", Some(11)), &[turn(0), turn(1)], 0).expect("write");
        assert_eq!(search_words(&mut conn, &index, &provider_of, &homes, &["m1".to_owned()])["m1"], "ask 0\nanswer 1");
        drop(conn);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The owner's tree is read only for ids Baaz's own homes lack, and
    /// Baaz's own copy wins (moved from provider_search, docs/50 P3).
    #[test]
    fn a_later_home_is_read_only_for_ids_the_first_lacks() {
        let first = temp("homes-first");
        let second = temp("homes-second");
        let (mut homes, sources) = files::write_synthetic(&first, 2);
        let (owner, owner_sources) = files::write_synthetic(&second, 2);
        assert_eq!(sources, owner_sources, "the same ids live in both trees");
        let claude_id = sources[0].1.as_str();
        let owner_copy = owner.claude[0].join("projects").join("-work-project").join(format!("{claude_id}.jsonl"));
        homes.claude.extend(owner.claude.clone());
        let found = stored_files_in("claude-code", &[claude_id], &homes);
        assert!(found[claude_id].starts_with(&first), "Baaz's own copy wins");
        std::fs::remove_dir_all(first.join("claude-home")).expect("drop own tree");
        let found = stored_files_in("claude-code", &[claude_id, "nope"], &homes);
        assert_eq!(found.get(claude_id), Some(&owner_copy), "an id only the owner's tree holds is found there");
        assert!(!found.contains_key("nope"));
        assert!(stored_files_in("muse", &["m1"], &homes).is_empty(), "Muse keeps no file");
        let _ = std::fs::remove_dir_all(&first);
        let _ = std::fs::remove_dir_all(&second);
    }

    #[test]
    fn words_are_capped_on_a_char_boundary() {
        let dir = temp("words-cap");
        let mut conn = open_at(&dir.join("transcript.db")).expect("store");
        let long = "é".repeat(WORDS_LIMIT);
        let user = Turn::User { id: "u".into(), text: long, attachments: Vec::new(), mentions: Vec::new(), timestamp: None };
        write_from(&mut conn, "codex", "long", "1:1", &[user], 0).expect("write");
        let text = words(&conn, "codex", "long").expect("read").expect("kept");
        assert!(text.len() <= WORDS_LIMIT && text.chars().all(|c| c == 'é'));
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
