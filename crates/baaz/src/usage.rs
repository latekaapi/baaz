//! Usage history: one row per finished turn, keyed on the view cursor (Task C3).
//!
//! Baaz records no transcript of its own — Muse stays the system of record —
//! but beside it Baaz keeps a ledger in its own `baaz.db` under the support
//! dir, next to `search.db` rather than inside it. A search index is a
//! rebuildable cache with its own schema churn; a usage ledger is
//! append-only and must never be dropped by an unrelated migration, so the
//! two do not share a file (or a lock).
//!
//! One row per finished turn:
//!
//! * The key is `(session_id, view_cursor)`, the cursor of the turn's
//!   terminal event. The cursor is opaque and strictly monotonic and a
//!   `view/page` is contiguous, so replaying the same events twice is a
//!   no-op: every insert is `ON CONFLICT DO NOTHING`. That is what makes the
//!   live write and the backfill safe to overlap — a session recorded live
//!   and then paged in again gains nothing twice.
//! * `cache_read_tokens` / `cache_write_tokens` are `NULL` when the provider
//!   never said — not zero. `None` in [`aui_protocol::TurnMeta`] stays `NULL`
//!   in the row; a later sum may `COALESCE` them, but the write must not.
//!
//! Every write is fire-and-forget: [`record_backfilled`] opens the
//! database, writes, and swallows every failure
//! with a one-line log, exactly as `index.rs` reads do. A locked database, a
//! missing directory and a schema this build has never seen are all ordinary,
//! and none of them may ever be visible to the person. Usage history is a
//! ledger, not a feature anything blocks on.

use std::path::PathBuf;
use std::time::Duration;

use aui_protocol::TurnMeta;
use rusqlite::{params, Connection};

/// `~/Library/Application Support/baaz/baaz.db` — a new file beside
/// `search.db`, not a table inside it (see the module docs for why).
pub fn db_path() -> PathBuf {
    crate::store::support_dir().join("baaz.db")
}

/// The ledger schema version, stamped in `meta`. This is the first schema,
/// so there is no migration: a database stamped with anything else is left
/// alone and every write is skipped (with a log line) until a build that
/// knows that version arrives.
const SCHEMA_VERSION: u32 = 1;

/// How long a write waits for another process to let go of the database
/// before giving up and carrying on without the row.
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// One finished turn, ready to record.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    /// The Muse session the turn ran in.
    pub session_id: String,
    /// The cursor of the turn's terminal event — half of the primary key.
    pub view_cursor: String,
    /// The turn that finished.
    pub turn_id: String,
    /// When Baaz recorded the row, as Unix milliseconds.
    pub finished_at_ms: i64,
    /// Model that produced the turn.
    pub model: String,
    /// Wall-clock time for the turn, in milliseconds.
    pub duration_ms: i64,
    /// Prompt tokens billed.
    pub tokens_in: i64,
    /// Completion tokens billed.
    pub tokens_out: i64,
    /// Reasoning tokens billed.
    pub reasoning_tokens: i64,
    /// Cache read tokens, or `None` when the provider never told us.
    pub cache_read_tokens: Option<i64>,
    /// Cache write tokens, or `None` when the provider never told us.
    pub cache_write_tokens: Option<i64>,
    /// Provider-reported cache tokens.
    pub cached_tokens: i64,
    /// Cost of the turn in US dollars.
    pub cost_usd: f64,
}

/// Build the row a finished turn writes: the fold's final [`TurnMeta`] for
/// `turn_id`, keyed on the terminal event's `view_cursor`.
///
/// The `Option` cache counters pass through untouched — `None` stays `NULL`,
/// never zero — and the token counts are stored as-is: the cache counters
/// are not summable into `tokens_in` under every provider's convention.
pub fn row_from_finished(
    session_id: &str,
    view_cursor: &str,
    turn_id: &str,
    finished_at_ms: i64,
    meta: &TurnMeta,
) -> UsageRow {
    UsageRow {
        session_id: session_id.to_owned(),
        view_cursor: view_cursor.to_owned(),
        turn_id: turn_id.to_owned(),
        finished_at_ms,
        model: meta.model.clone(),
        duration_ms: meta.duration_ms as i64,
        tokens_in: meta.tokens_in as i64,
        tokens_out: meta.tokens_out as i64,
        reasoning_tokens: meta.reasoning_tokens as i64,
        cache_read_tokens: meta.cache_read_tokens.map(|v| v as i64),
        cache_write_tokens: meta.cache_write_tokens.map(|v| v as i64),
        cached_tokens: meta.cached_tokens as i64,
        cost_usd: meta.cost_usd,
    }
}

/// This moment as Unix milliseconds, for [`UsageRow::finished_at_ms`].
///
/// Backfilled rows carry the time they were recorded, not the time the turn
/// ran: a `view/page` carries the turn's duration but no wall-clock for its
/// terminal, so the recording time is the honest stamp.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Open (creating) the usage database, ensuring the schema.
///
/// Modelled on `search::open_at`: the parent directory is created, and the
/// caller decides what an error means — the fire-and-forget recorders below
/// log it and carry on.
/// Unused until something reads the ledger: the writes go through
/// [`record_at`], which opens by path, and the tests use [`open_at`].
/// Kept because a reader is the next thing to be built and this is the
/// one place that should decide where the real database lives.
#[allow(dead_code)]
pub fn open() -> Result<Connection, rusqlite::Error> {
    open_at(&db_path())
}

/// [`open`] against an explicit path, which is what the tests use
/// (`:memory:` included).
pub fn open_at(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let connection = Connection::open(path)?;
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    connection.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);")?;
    let stored: u32 = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if stored != SCHEMA_VERSION {
        if stored != 0 {
            // A schema this build has never seen: leave the file alone and
            // let the caller treat usage history as unavailable, exactly as
            // `index.rs` treats a foreign session index.
            crate::baaz_log!(
                "usage database schema version {stored} is newer than this build ({SCHEMA_VERSION}); usage history is paused"
            );
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unsupported baaz.db schema version {stored}"
            )));
        }
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_turns(
                session_id TEXT NOT NULL,
                view_cursor TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                finished_at_ms INTEGER NOT NULL,
                model TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                tokens_in INTEGER NOT NULL,
                tokens_out INTEGER NOT NULL,
                reasoning_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                cached_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                PRIMARY KEY (session_id, view_cursor)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS usage_turns_finished_at ON usage_turns(finished_at_ms);",
        )?;
        connection.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    } else {
        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS usage_turns(
                session_id TEXT NOT NULL,
                view_cursor TEXT NOT NULL,
                turn_id TEXT NOT NULL,
                finished_at_ms INTEGER NOT NULL,
                model TEXT NOT NULL,
                duration_ms INTEGER NOT NULL,
                tokens_in INTEGER NOT NULL,
                tokens_out INTEGER NOT NULL,
                reasoning_tokens INTEGER NOT NULL,
                cache_read_tokens INTEGER,
                cache_write_tokens INTEGER,
                cached_tokens INTEGER NOT NULL,
                cost_usd REAL NOT NULL,
                PRIMARY KEY (session_id, view_cursor)
             ) WITHOUT ROWID;
             CREATE INDEX IF NOT EXISTS usage_turns_finished_at ON usage_turns(finished_at_ms);",
        )?;
    }
    Ok(connection)
}

/// Record one finished turn.
///
/// `INSERT … ON CONFLICT DO NOTHING`: writing the same cursor twice is a
/// no-op, not an error and not an update — replay the same events twice and
/// the second write changes nothing. Returns whether the row was new.
pub fn record_turn(connection: &Connection, row: &UsageRow) -> Result<bool, rusqlite::Error> {
    let changed = connection.execute(
        "INSERT INTO usage_turns(
            session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
            tokens_in, tokens_out, reasoning_tokens,
            cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(session_id, view_cursor) DO NOTHING",
        params![
            row.session_id,
            row.view_cursor,
            row.turn_id,
            row.finished_at_ms,
            row.model,
            row.duration_ms,
            row.tokens_in,
            row.tokens_out,
            row.reasoning_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.cached_tokens,
            row.cost_usd,
        ],
    )?;
    Ok(changed == 1)
}

/// The row for (`session_id`, `view_cursor`), or `None` when it was never
/// recorded.
///
/// Best-effort, like every other read in this app: any failure yields `None`.
/// This is the query surface the tests (and, later, the usage page) read
/// through — nothing else needs to ask what a cursor cost yet.
/// Test-only for now: the brief put a query API out of scope, so the
/// only reader of the ledger today is this crate's own tests.
#[cfg(test)]
pub fn find_turn(
    connection: &Connection,
    session_id: &str,
    view_cursor: &str,
) -> Option<UsageRow> {
    let mut statement = connection
        .prepare(
            "SELECT session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
                    tokens_in, tokens_out, reasoning_tokens,
                    cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
             FROM usage_turns WHERE session_id = ?1 AND view_cursor = ?2",
        )
        .ok()?;
    statement
        .query_row(params![session_id, view_cursor], |row| {
            Ok(UsageRow {
                session_id: row.get(0)?,
                view_cursor: row.get(1)?,
                turn_id: row.get(2)?,
                finished_at_ms: row.get(3)?,
                model: row.get(4)?,
                duration_ms: row.get(5)?,
                tokens_in: row.get(6)?,
                tokens_out: row.get(7)?,
                reasoning_tokens: row.get(8)?,
                cache_read_tokens: row.get(9)?,
                cache_write_tokens: row.get(10)?,
                cached_tokens: row.get(11)?,
                cost_usd: row.get(12)?,
            })
        })
        .ok()
}

/// How many turns `session_id` has recorded, or `0` when the database cannot
/// say. Best-effort, like [`find_turn`].
/// Test-only for now: the brief put a query API out of scope, so the
/// only reader of the ledger today is this crate's own tests.
#[cfg(test)]
pub fn count_for_session(connection: &Connection, session_id: &str) -> usize {
    connection
        .query_row(
            "SELECT COUNT(*) FROM usage_turns WHERE session_id = ?1",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
        .max(0) as usize
}

/// Record finished turns against an explicit database path, swallowing every
/// failure with a log line: a locked database, a missing directory and a
/// schema this build has never seen are all ordinary, and none of them may
/// reach the person. The public recorders fix the path to [`db_path`]; the
/// test for the unopenable database calls this directly.
fn record_at(path: &std::path::Path, rows: &[UsageRow]) {
    let connection = match open_at(path) {
        Ok(connection) => connection,
        Err(error) => {
            crate::baaz_log!("usage history unavailable ({error}); turn not recorded");
            return;
        }
    };
    for row in rows {
        if let Err(error) = record_turn(&connection, row) {
            crate::baaz_log!("usage history write failed ({error}); turn not recorded");
        }
    }
}

/// Record finished turns — the live path and a backfilled page both come
/// through here, a live turn simply arriving as a one-element slice.
///
/// Fire-and-forget: rows whose cursor is already recorded are no-ops, so
/// overlapping a live-recorded session is safe and cheap. That idempotence
/// is what the view cursor as primary key buys.
pub fn record_backfilled(rows: &[UsageRow]) {
    if rows.is_empty() {
        return;
    }
    record_at(&db_path(), rows);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        open_at(std::path::Path::new(":memory:")).expect("in-memory usage db")
    }

    fn meta() -> TurnMeta {
        TurnMeta {
            model: "muse-spark".to_owned(),
            duration_ms: 12_400,
            tokens_in: 100,
            tokens_out: 50,
            reasoning_tokens: 7,
            cost_usd: 0.0,
            cache_read_tokens: Some(11),
            cache_write_tokens: Some(13),
            cached_tokens: 17,
        }
    }

    fn row(cursor: &str) -> UsageRow {
        row_from_finished("s-1", cursor, "t-1", 1_700_000_000_000, &meta())
    }

    #[test]
    fn a_finished_turn_writes_one_row_with_every_field() {
        let connection = memory();
        assert!(record_turn(&connection, &row("v:s:1")).expect("record"));
        let read = find_turn(&connection, "s-1", "v:s:1").expect("row is there");
        assert_eq!(read, row("v:s:1"));
        assert_eq!(read.model, "muse-spark");
        assert_eq!(read.duration_ms, 12_400);
        assert_eq!(read.tokens_in, 100);
        assert_eq!(read.tokens_out, 50);
        assert_eq!(read.reasoning_tokens, 7);
        assert_eq!(read.cache_read_tokens, Some(11));
        assert_eq!(read.cache_write_tokens, Some(13));
        assert_eq!(read.cached_tokens, 17);
        assert_eq!(count_for_session(&connection, "s-1"), 1);
    }

    #[test]
    fn a_turn_the_provider_said_nothing_about_writes_null_caches() {
        let connection = memory();
        let mut unknown = meta();
        unknown.cache_read_tokens = None;
        unknown.cache_write_tokens = None;
        // "Said nothing" means all three, not two of three: `cached_tokens`
        // is a separate u64 counter, so the fixture has to zero it too or the
        // row under test is not the row this test's name describes.
        unknown.cached_tokens = 0;
        let row = row_from_finished("s-1", "v:s:1", "t-1", 1_700_000_000_000, &unknown);
        assert!(record_turn(&connection, &row).expect("record"));
        let read = find_turn(&connection, "s-1", "v:s:1").expect("row is there");
        assert_eq!(read.cache_read_tokens, None);
        assert_eq!(read.cache_write_tokens, None);
        assert_eq!(read.cached_tokens, 0);
    }

    #[test]
    fn writing_the_same_cursor_twice_leaves_the_row_unchanged() {
        let connection = memory();
        assert!(record_turn(&connection, &row("v:s:1")).expect("first record"));
        // The same cursor re-recorded with different contents: a replay, not
        // an update. `ON CONFLICT DO UPDATE` would rewrite the row; `DO
        // NOTHING` must leave it byte-identical.
        let mut replay = row("v:s:1");
        replay.turn_id = "t-2".to_owned();
        replay.model = "other-model".to_owned();
        replay.tokens_in = 999;
        replay.tokens_out = 999;
        replay.cache_read_tokens = Some(0);
        assert!(!record_turn(&connection, &replay).expect("replay records"));
        assert_eq!(count_for_session(&connection, "s-1"), 1);
        assert_eq!(find_turn(&connection, "s-1", "v:s:1"), Some(row("v:s:1")));
    }

    #[test]
    fn a_backfill_over_an_overlap_adds_only_the_new_rows() {
        let connection = memory();
        // Two turns recorded live as they finished.
        assert!(record_turn(&connection, &row("v:s:1")).expect("live c1"));
        assert!(record_turn(&connection, &row("v:s:2")).expect("live c2"));
        // The page carries the same two cursors (re-recorded with different
        // payloads, as a fresh fold of the same events would) plus one new one.
        let mut again = row("v:s:1");
        again.tokens_out = 5_000;
        let mut again2 = row("v:s:2");
        again2.model = "drifted".to_owned();
        let fresh = row_from_finished("s-1", "v:s:3", "t-3", 1_700_000_001_000, &meta());
        for candidate in [&again, &again2, &fresh] {
            let _ = record_turn(&connection, candidate).expect("backfill records");
        }
        assert_eq!(count_for_session(&connection, "s-1"), 3);
        // The old rows are unchanged; only the new cursor added a row.
        assert_eq!(find_turn(&connection, "s-1", "v:s:1"), Some(row("v:s:1")));
        assert_eq!(find_turn(&connection, "s-1", "v:s:2"), Some(row("v:s:2")));
        assert_eq!(find_turn(&connection, "s-1", "v:s:3"), Some(fresh));
    }

    #[test]
    fn null_cache_tokens_survive_and_differ_from_zero() {
        let connection = memory();
        let mut unknown = meta();
        unknown.cache_read_tokens = None;
        unknown.cache_write_tokens = None;
        let nulls = row_from_finished("s-1", "v:s:1", "t-1", 1_700_000_000_000, &unknown);
        let mut zeroed = meta();
        zeroed.cache_read_tokens = Some(0);
        zeroed.cache_write_tokens = Some(0);
        let zeros = row_from_finished("s-1", "v:s:2", "t-2", 1_700_000_000_000, &zeroed);
        assert!(record_turn(&connection, &nulls).expect("nulls record"));
        assert!(record_turn(&connection, &zeros).expect("zeros record"));
        let read_nulls = find_turn(&connection, "s-1", "v:s:1").expect("nulls row");
        let read_zeros = find_turn(&connection, "s-1", "v:s:2").expect("zeros row");
        assert_eq!(read_nulls.cache_read_tokens, None);
        assert_eq!(read_zeros.cache_read_tokens, Some(0));
        assert_ne!(read_nulls.cache_read_tokens, read_zeros.cache_read_tokens);
        assert_ne!(read_nulls.cache_write_tokens, read_zeros.cache_write_tokens);
    }

    #[test]
    fn an_unopenable_database_records_nothing_and_raises_nothing() {
        // The parent is a file, not a directory, so no database can be
        // created there. The recorder must log and carry on — no panic, no
        // error, and nothing for the caller to handle.
        let dir = std::env::temp_dir().join(format!("baaz-usage-unopenable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let blocker = dir.join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("blocker file");
        let path = blocker.join("baaz.db");
        record_at(&path, &[row("v:s:1")]);
        record_at(&path, &[row("v:s:1"), row("v:s:2")]);
        assert!(!path.exists(), "no database was created where none can live");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_schema_this_build_has_never_seen_stays_unreadable() {
        let dir = std::env::temp_dir().join(format!("baaz-usage-future-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("baaz.db");
        {
            let connection = open_at(&path).expect("current schema opens");
            connection
                .execute("UPDATE meta SET value = '999' WHERE key = 'schema_version'", [])
                .expect("stamp a future version");
        }
        assert!(open_at(&path).is_err(), "an unknown schema refuses to open");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
