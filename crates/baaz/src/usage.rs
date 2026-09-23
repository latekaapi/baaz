//! Usage history: one row per finished turn, keyed on the turn (Task D1).
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
//! * The key is `(session_id, turn_id)`. An earlier schema keyed on
//!   `(session_id, view_cursor)` — the cursor of the turn's terminal event.
//!   That was wrong: the live path stamps a whole batch of finished turns
//!   with one cursor while the backfill path stamps each turn with its own
//!   event's cursor, so one turn recorded through both paths landed twice
//!   under two cursors (seen in a real database: 16 rows, 16 cursors,
//!   15 turns). Keying on the turn makes the two paths agree regardless of
//!   which cursor each chose: replaying the same turn twice is a no-op,
//!   because every insert is `ON CONFLICT DO NOTHING`. That is what makes
//!   the live write and the backfill safe to overlap — a session recorded
//!   live and then paged in again gains nothing twice.
//! * `view_cursor` is still stored on every row — it says where the turn sat
//!   in the view — but it is an ordinary data column now, not the key.
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

/// The ledger schema version, stamped in `meta`.
///
/// Version 1 keyed `usage_turns` on `(session_id, view_cursor)`; version 2
/// re-keys it on `(session_id, turn_id)` (Task D1). A version-1 database is
/// migrated in place (see [`migrate_v1_to_v2`]): no history is dropped —
/// rows already there are carried over, except that a turn stored twice
/// under two cursors collapses to one row (the smallest cursor wins). The
/// migration is all-or-nothing — rename, rebuild, copy, drop and the version
/// stamp run inside a single transaction — and a `usage_turns_v1` left over
/// from an interrupted attempt is finished on the next open rather than
/// erroring (rows already in the live table win; the backup only fills gaps).
/// A missing or unparseable stamp is never taken for a fresh install while a
/// ledger exists: the table's real primary key decides (v1 migrates, v2 is
/// re-stamped, anything else fails loudly). A database stamped with a version
/// this build has never seen is left alone and every write is skipped (with
/// a log line) until a build that knows that version arrives.
const SCHEMA_VERSION: u32 = 2;

/// How long a write waits for another process to let go of the database
/// before giving up and carrying on without the row.
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// One finished turn, ready to record.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageRow {
    /// The Muse session the turn ran in.
    pub session_id: String,
    /// Where the turn sat in the view — an ordinary data column, not the
    /// key. The live path and the backfill path legitimately disagree about
    /// this for one turn, which is why it must not key the row.
    pub view_cursor: String,
    /// The turn that finished — half of the primary key.
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
/// `turn_id`, carrying the `view_cursor` the writing path saw.
///
/// The `Option` cache counters pass through untouched — `None` stays `NULL`,
/// never zero — and the token counts are stored as-is: the cache counters
/// are not summable into `tokens_in` under every provider's convention.
/// The cursor is data, not identity: the row is keyed on
/// `(session_id, turn_id)`, so the same turn built with two different
/// cursors still writes one row.
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
    let mut connection = Connection::open(path)?;
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    connection.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);")?;
    let stored_raw: Option<String> = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok();
    let stored: Option<u32> = stored_raw
        .as_deref()
        .and_then(|value| value.parse().ok());
    match stored {
        Some(version) if version == SCHEMA_VERSION => {
            connection.execute_batch(LEDGER_DDL_V2)?;
            // A backup left beside a stamped v2 is merged back, never left
            // to sit beside the live table unread.
            drain_backup_table(&mut connection)?;
        }
        Some(1) => {
            migrate_v1_to_v2(&mut connection)?;
        }
        Some(0) | None => {
            // No readable stamp: a fresh install, or a stamp nobody can
            // parse. The table's real shape decides — never the assumption
            // that this is a fresh database.
            open_unstamped(&mut connection, stored_raw.as_deref())?;
        }
        Some(version) => {
            // A schema this build has never seen: leave the file alone and
            // let the caller treat usage history as unavailable, exactly as
            // `index.rs` treats a foreign session index.
            crate::baaz_log!(
                "usage database schema version {version} is newer than this build ({SCHEMA_VERSION}); usage history is paused"
            );
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unsupported baaz.db schema version {version}"
            )));
        }
    }
    Ok(connection)
}

/// Open a database whose `schema_version` is missing or does not parse.
///
/// An unreadable stamp on a file with no ledger is a fresh install. On a
/// file that already holds a `usage_turns` table it must never be taken for
/// one: stamping a version-1 table as version-2 is what once left the ledger
/// silently dead (every later `record_turn` failing on the untouched v1 key
/// while the file claimed health). So the table's real primary key decides:
/// a version-1 key migrates, a version-2 key is re-stamped, and anything
/// else fails loudly with the stamp untouched.
fn open_unstamped(
    connection: &mut Connection,
    stored_raw: Option<&str>,
) -> Result<(), rusqlite::Error> {
    if table_exists(connection, "usage_turns_v1")? {
        // A backup without a readable stamp: an interrupted migration is the
        // only thing that leaves one. Finish it.
        return migrate_v1_to_v2(connection);
    }
    match usage_turns_pk(connection)? {
        None => stamp_fresh_v2(connection),
        Some(pk) if is_v2_pk(&pk) => {
            connection.execute_batch(LEDGER_DDL_V2)?;
            stamp_v2(connection)
        }
        Some(pk) if is_v1_pk(&pk) => migrate_v1_to_v2(connection),
        Some(_) => Err(unreadable_version_error(stored_raw)),
    }
}

/// The loud failure for a stamp nobody can parse on a table of unknown
/// shape: the file is left exactly as found — in particular nothing is
/// stamped — so the next open fails the same way instead of going quietly
/// dark.
fn unreadable_version_error(stored_raw: Option<&str>) -> rusqlite::Error {
    let seen = stored_raw.unwrap_or("<missing>");
    crate::baaz_log!(
        "usage database schema_version {seen:?} is unreadable and usage_turns has an unknown shape; usage history is paused"
    );
    rusqlite::Error::InvalidParameterName(format!(
        "unreadable baaz.db schema_version {seen:?} on an unknown usage_turns shape"
    ))
}

/// Whether a table by that name exists.
fn table_exists(connection: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![name],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// The primary-key columns of `usage_turns`, in key order — `None` when the
/// table does not exist.
fn usage_turns_pk(connection: &Connection) -> Result<Option<Vec<String>>, rusqlite::Error> {
    if !table_exists(connection, "usage_turns")? {
        return Ok(None);
    }
    let mut statement =
        connection.prepare("SELECT name FROM pragma_table_info('usage_turns') WHERE pk > 0 ORDER BY pk")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(columns))
}

/// The version-1 key: `(session_id, view_cursor)`.
fn is_v1_pk(pk: &[String]) -> bool {
    pk.iter().map(String::as_str).collect::<Vec<_>>() == ["session_id", "view_cursor"]
}

/// The version-2 key: `(session_id, turn_id)`.
fn is_v2_pk(pk: &[String]) -> bool {
    pk.iter().map(String::as_str).collect::<Vec<_>>() == ["session_id", "turn_id"]
}

/// Stamp a fresh (or backup-only) file as version 2, with the schema build
/// in the same transaction.
fn stamp_fresh_v2(connection: &mut Connection) -> Result<(), rusqlite::Error> {
    let txn = connection.transaction()?;
    txn.execute_batch(LEDGER_DDL_V2)?;
    txn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    txn.commit()?;
    Ok(())
}

/// Stamp the version on an already-built version-2 ledger.
fn stamp_v2(connection: &Connection) -> Result<(), rusqlite::Error> {
    connection.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    Ok(())
}

/// Merge a leftover `usage_turns_v1` back into a stamped version-2 ledger:
/// the live table wins key conflicts (the copy is `INSERT OR IGNORE` into
/// it), the backup only fills gaps, and then the backup is dropped — all in
/// one transaction. Nothing in either table is lost.
fn drain_backup_table(connection: &mut Connection) -> Result<(), rusqlite::Error> {
    if !table_exists(connection, "usage_turns_v1")? {
        return Ok(());
    }
    let txn = connection.transaction()?;
    txn.execute(COPY_V1_TO_V2, [])?;
    txn.execute_batch("DROP TABLE usage_turns_v1;")?;
    txn.commit()?;
    Ok(())
}

/// The version-2 ledger: one row per `(session_id, turn_id)`, with the view
/// cursor kept as an ordinary column.
const LEDGER_DDL_V2: &str = "CREATE TABLE IF NOT EXISTS usage_turns(
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
    PRIMARY KEY (session_id, turn_id)
 ) WITHOUT ROWID;
 CREATE INDEX IF NOT EXISTS usage_turns_finished_at ON usage_turns(finished_at_ms);";

/// The copy half of the migration: every row of the version-1 backup lands
/// in the version-2 table, smallest cursor first so a turn the old key
/// stored twice collapses to one row, and rows already in the live table
/// keep their seats (`INSERT OR IGNORE` into it). Idempotent: re-running it
/// over an already-copied backup changes nothing.
const COPY_V1_TO_V2: &str = "INSERT OR IGNORE INTO usage_turns(
    session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
    tokens_in, tokens_out, reasoning_tokens,
    cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
 ) SELECT
    session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
    tokens_in, tokens_out, reasoning_tokens,
    cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
 FROM usage_turns_v1 ORDER BY session_id, view_cursor";

/// Re-key a version-1 ledger from `(session_id, view_cursor)` to
/// `(session_id, turn_id)`.
///
/// Nothing already there is dropped, with one deliberate exception: a turn
/// the old key stored twice under two cursors cannot survive twice under
/// the new one, so it collapses to a single row — the copy with the
/// smallest `view_cursor` wins (`INSERT OR IGNORE` over cursor order keeps
/// the first). Every other row is carried over byte-identical.
///
/// The whole swap — rename, rebuild, copy, drop — plus the version stamp
/// runs inside a single transaction, so an interruption at any statement
/// boundary leaves either the untouched version-1 database or the finished
/// version-2 one, never a half state. A `usage_turns_v1` left over from an
/// interrupted earlier attempt is finished rather than errored on: its rows
/// merge into the live table (which wins key conflicts, so no reachable row
/// is ever destroyed — the live rows of a half-migration are already copies
/// of the same backup data, or the table is still empty), the backup is
/// dropped, and the stamp lands in the same transaction.
fn migrate_v1_to_v2(connection: &mut Connection) -> Result<(), rusqlite::Error> {
    migrate_steps(connection, None)
}

/// Fail after the k-th migration statement instead of running the rest: the
/// test seam that simulates a kill between statements. Production always
/// passes `None`, which runs the migration to completion.
#[cfg(test)]
fn migrate_interrupted_after(
    connection: &mut Connection,
    after: u32,
) -> Result<(), rusqlite::Error> {
    migrate_steps(connection, Some(after))
}

/// The migration body shared by the real run and the interruption seam.
/// Statements are numbered 1–4 in the order the old code ran them —
/// rename, rebuild, copy, drop — so a test can stop after any one of them.
fn migrate_steps(
    connection: &mut Connection,
    interrupt_after: Option<u32>,
) -> Result<(), rusqlite::Error> {
    fn interrupted(after: Option<u32>, step: u32) -> Result<(), rusqlite::Error> {
        if after == Some(step) {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "simulated interruption after migration statement {step}"
            )));
        }
        Ok(())
    }
    if !table_exists(connection, "usage_turns_v1")? {
        // No half-migration in flight: look at the live table's shape.
        match usage_turns_pk(connection)? {
            None => {
                // Stamped 1 but the table never landed (a crash between the
                // stamp and the write, or a hand-made file): build the new
                // schema and stamp it, together.
                return stamp_fresh_v2(connection);
            }
            Some(pk) if is_v2_pk(&pk) => {
                // Already re-keyed with the stamp lost (a kill between the
                // old code's drop and its stamp): just stamp it.
                return stamp_v2(connection);
            }
            Some(_) => {
                // A version-1 key migrates below. Anything else reaches the
                // copy, which fails on unknown columns — loudly, and rolled
                // back to the untouched database.
            }
        }
    }
    // Whether the rename already happened decides which statements still
    // need running; everything below is one transaction either way.
    let renamed = table_exists(connection, "usage_turns_v1")?;
    let txn = connection.transaction()?;
    if !renamed {
        txn.execute_batch("ALTER TABLE usage_turns RENAME TO usage_turns_v1;")?;
        interrupted(interrupt_after, 1)?;
    }
    txn.execute_batch(LEDGER_DDL_V2)?;
    interrupted(interrupt_after, 2)?;
    txn.execute(COPY_V1_TO_V2, [])?;
    interrupted(interrupt_after, 3)?;
    txn.execute_batch("DROP TABLE usage_turns_v1;")?;
    interrupted(interrupt_after, 4)?;
    txn.execute(
        "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
        params![SCHEMA_VERSION.to_string()],
    )?;
    txn.commit()?;
    Ok(())
}

/// Record one finished turn.
///
/// `INSERT … ON CONFLICT DO NOTHING`: writing the same turn twice is a
/// no-op, not an error and not an update — the live path and the backfill
/// path may each record a turn under a different cursor, and the second
/// write changes nothing. Returns whether the row was new.
pub fn record_turn(connection: &Connection, row: &UsageRow) -> Result<bool, rusqlite::Error> {
    let changed = connection.execute(
        "INSERT INTO usage_turns(
            session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
            tokens_in, tokens_out, reasoning_tokens,
            cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT(session_id, turn_id) DO NOTHING",
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

/// The row for (`session_id`, `turn_id`), or `None` when it was never
/// recorded.
///
/// Best-effort, like every other read in this app: any failure yields `None`.
/// This is the query surface the tests (and, later, the usage page) read
/// through — nothing else needs to ask what a turn cost yet.
/// Test-only for now: the brief put a query API out of scope, so the
/// only reader of the ledger today is this crate's own tests.
#[cfg(test)]
pub fn find_turn(
    connection: &Connection,
    session_id: &str,
    turn_id: &str,
) -> Option<UsageRow> {
    let mut statement = connection
        .prepare(
            "SELECT session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
                    tokens_in, tokens_out, reasoning_tokens,
                    cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
             FROM usage_turns WHERE session_id = ?1 AND turn_id = ?2",
        )
        .ok()?;
    statement
        .query_row(params![session_id, turn_id], |row| {
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
/// Fire-and-forget: rows whose turn is already recorded are no-ops, so
/// overlapping a live-recorded session is safe and cheap — even though the
/// two paths stamp different cursors on the same turn. That idempotence is
/// what the turn as primary key buys.
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

    fn row(cursor: &str, turn: &str) -> UsageRow {
        row_from_finished("s-1", cursor, turn, 1_700_000_000_000, &meta())
    }

    #[test]
    fn a_finished_turn_writes_one_row_with_every_field() {
        let connection = memory();
        assert!(record_turn(&connection, &row("v:s:1", "t-1")).expect("record"));
        let read = find_turn(&connection, "s-1", "t-1").expect("row is there");
        assert_eq!(read, row("v:s:1", "t-1"));
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
        let read = find_turn(&connection, "s-1", "t-1").expect("row is there");
        assert_eq!(read.cache_read_tokens, None);
        assert_eq!(read.cache_write_tokens, None);
        assert_eq!(read.cached_tokens, 0);
    }

    #[test]
    fn writing_the_same_turn_twice_leaves_the_row_unchanged() {
        let connection = memory();
        assert!(record_turn(&connection, &row("v:s:1", "t-1")).expect("first record"));
        // The same turn re-recorded under a different cursor with different
        // contents: a replay, not an update. `ON CONFLICT DO UPDATE` would
        // rewrite the row; `DO NOTHING` must leave it byte-identical.
        let mut replay = row("v:s:2", "t-1");
        replay.model = "other-model".to_owned();
        replay.tokens_in = 999;
        replay.tokens_out = 999;
        replay.cache_read_tokens = Some(0);
        assert!(!record_turn(&connection, &replay).expect("replay records"));
        assert_eq!(count_for_session(&connection, "s-1"), 1);
        assert_eq!(
            find_turn(&connection, "s-1", "t-1"),
            Some(row("v:s:1", "t-1"))
        );
    }

    #[test]
    fn a_backfill_over_an_overlap_adds_only_the_new_rows() {
        let connection = memory();
        // Two turns recorded live as they finished.
        assert!(record_turn(&connection, &row("v:s:1", "t-1")).expect("live t1"));
        assert!(record_turn(&connection, &row("v:s:2", "t-2")).expect("live t2"));
        // The page carries the same two turns (re-recorded with different
        // payloads, as a fresh fold of the same events would) plus one new one.
        let mut again = row("v:s:1", "t-1");
        again.tokens_out = 5_000;
        let mut again2 = row("v:s:2", "t-2");
        again2.model = "drifted".to_owned();
        let fresh = row_from_finished("s-1", "v:s:3", "t-3", 1_700_000_001_000, &meta());
        for candidate in [&again, &again2, &fresh] {
            let _ = record_turn(&connection, candidate).expect("backfill records");
        }
        assert_eq!(count_for_session(&connection, "s-1"), 3);
        // The old rows are unchanged; only the new turn added a row.
        assert_eq!(
            find_turn(&connection, "s-1", "t-1"),
            Some(row("v:s:1", "t-1"))
        );
        assert_eq!(
            find_turn(&connection, "s-1", "t-2"),
            Some(row("v:s:2", "t-2"))
        );
        assert_eq!(find_turn(&connection, "s-1", "t-3"), Some(fresh));
    }

    #[test]
    fn the_same_turn_through_both_paths_writes_one_row() {
        // The owner's defect, replayed: one `turn_id` recorded through both
        // write paths, each choosing its cursor the way its call site does.
        // The live path (`session/events.rs` `record_usage`) takes ONE cursor
        // for a whole batch of deltas and stamps every finished turn with it;
        // the backfill path (the `view/page` handler in the same file) walks
        // events one at a time and uses each event's own cursor. For one turn
        // those are genuinely different cursors — and still one row.
        let connection = memory();
        let live = row_from_finished(
            "s-7",
            "v:live:42",
            "t-9",
            1_700_000_000_000,
            &TurnMeta {
                tokens_in: 22_633,
                ..meta()
            },
        );
        let backfilled = row_from_finished(
            "s-7",
            "v:page:915",
            "t-9",
            1_700_000_001_000,
            &TurnMeta {
                tokens_in: 22_633,
                ..meta()
            },
        );
        assert_ne!(
            live.view_cursor, backfilled.view_cursor,
            "the two paths must genuinely disagree about the cursor"
        );
        assert_eq!(live.turn_id, backfilled.turn_id);
        assert!(record_turn(&connection, &live).expect("live records"));
        assert!(
            !record_turn(&connection, &backfilled).expect("backfill records"),
            "the backfilled copy of a live-recorded turn is a no-op"
        );
        assert_eq!(count_for_session(&connection, "s-7"), 1);
        // The surviving row is the first write, named field by field: the
        // live cursor won, and the backfill changed nothing.
        let read = find_turn(&connection, "s-7", "t-9").expect("the row is there");
        assert_eq!(read.session_id, "s-7");
        assert_eq!(read.turn_id, "t-9");
        assert_eq!(read.view_cursor, "v:live:42");
        assert_eq!(read.finished_at_ms, 1_700_000_000_000);
        assert_eq!(read.model, "muse-spark");
        assert_eq!(read.duration_ms, 12_400);
        assert_eq!(read.tokens_in, 22_633);
        assert_eq!(read.tokens_out, 50);
        assert_eq!(read.reasoning_tokens, 7);
        assert_eq!(read.cache_read_tokens, Some(11));
        assert_eq!(read.cache_write_tokens, Some(13));
        assert_eq!(read.cached_tokens, 17);
    }

    #[test]
    fn summing_a_lived_and_backfilled_session_counts_each_turn_once() {
        // Three turns, each recorded live and then backfilled under a
        // different cursor — the overlap the old key double-counted.
        let connection = memory();
        let turns = [("t-1", 100_i64), ("t-2", 200_i64), ("t-3", 300_i64)];
        for (index, (turn, tokens_in)) in turns.iter().enumerate() {
            let live = row_from_finished(
                "s-9",
                &format!("v:live:{index}"),
                turn,
                1_700_000_000_000,
                &TurnMeta {
                    tokens_in: *tokens_in as u64,
                    ..meta()
                },
            );
            let backfilled = row_from_finished(
                "s-9",
                &format!("v:page:{}", index + 900),
                turn,
                1_700_000_001_000,
                &TurnMeta {
                    tokens_in: *tokens_in as u64,
                    ..meta()
                },
            );
            assert_ne!(live.view_cursor, backfilled.view_cursor);
            assert!(record_turn(&connection, &live).expect("live records"));
            let _ = record_turn(&connection, &backfilled).expect("backfill records");
        }
        assert_eq!(count_for_session(&connection, "s-9"), 3);
        let total: i64 = connection
            .query_row(
                "SELECT SUM(tokens_in) FROM usage_turns WHERE session_id = 's-9'",
                [],
                |row| row.get(0),
            )
            .expect("sum reads");
        assert_eq!(total, 600, "the sum counts each distinct turn exactly once");
    }

    #[test]
    fn a_version_1_database_migrates_without_losing_history() {
        // A ledger written by the old key: two cursors, two turns, plus one
        // turn stored twice — the owner's shape.
        let dir =
            std::env::temp_dir().join(format!("baaz-usage-migrate-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("baaz.db");
        {
            let connection = Connection::open(&path).expect("v1 file opens");
            connection
                .execute_batch(
                    "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                     INSERT INTO meta(key, value) VALUES ('schema_version', '1');
                     CREATE TABLE usage_turns(
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
                     ) WITHOUT ROWID;",
                )
                .expect("v1 schema builds");
            for (cursor, turn, tokens) in [
                ("v:s:1", "t-1", 100),
                ("v:s:2", "t-2", 200),
                ("v:s:3", "t-2", 200),
            ] {
                let meta = TurnMeta {
                    tokens_in: tokens,
                    ..meta()
                };
                let row = row_from_finished("s-1", cursor, turn, 1_700_000_000_000, &meta);
                connection
                    .execute(
                        "INSERT INTO usage_turns(
                            session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
                            tokens_in, tokens_out, reasoning_tokens,
                            cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
                         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                    )
                    .expect("v1 row writes");
            }
        }
        let connection = open_at(&path).expect("migration opens");
        let version: String = connection
            .query_row(
                "SELECT value FROM meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .expect("version is stamped");
        assert_eq!(version, "2");
        // The doubled turn collapsed to one row; the other history survived.
        assert_eq!(count_for_session(&connection, "s-1"), 2);
        let kept = find_turn(&connection, "s-1", "t-2").expect("t-2 survives");
        assert_eq!(kept.view_cursor, "v:s:2");
        assert_eq!(kept.tokens_in, 200);
        assert_eq!(
            find_turn(&connection, "s-1", "t-1"),
            Some(row("v:s:1", "t-1"))
        );
        // And new writes keep working on the migrated file.
        assert!(
            record_turn(&connection, &row("v:s:9", "t-9"))
                .expect("post-migration write records")
        );
        assert_eq!(count_for_session(&connection, "s-1"), 3);
        let _ = std::fs::remove_dir_all(&dir);
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
        let read_nulls = find_turn(&connection, "s-1", "t-1").expect("nulls row");
        let read_zeros = find_turn(&connection, "s-1", "t-2").expect("zeros row");
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
        record_at(&path, &[row("v:s:1", "t-1")]);
        record_at(&path, &[row("v:s:1", "t-1"), row("v:s:2", "t-2")]);
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

    /// A version-1 file with the owner's shape: one turn stored twice under
    /// two cursors, plus one single turn.
    fn build_v1_db(path: &std::path::Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("temp dir");
        }
        let connection = Connection::open(path).expect("v1 file opens");
        connection
            .execute_batch(
                "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                 INSERT INTO meta(key, value) VALUES ('schema_version', '1');
                 CREATE TABLE usage_turns(
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
                 ) WITHOUT ROWID;",
            )
            .expect("v1 schema builds");
        for (cursor, turn, tokens) in [("v:s:1", "t-1", 100), ("v:s:2", "t-2", 200), ("v:s:3", "t-2", 200)] {
            let meta = TurnMeta {
                tokens_in: tokens,
                ..meta()
            };
            let row = row_from_finished("s-1", cursor, turn, 1_700_000_000_000, &meta);
            connection
                .execute(
                    "INSERT INTO usage_turns(
                        session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
                        tokens_in, tokens_out, reasoning_tokens,
                        cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
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
                )
                .expect("v1 row writes");
        }
    }

    fn schema_version(connection: &Connection) -> Option<String> {
        connection
            .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
                row.get(0)
            })
            .ok()
    }

    /// The migrated ledger both original turns reach, the doubled turn
    /// collapsed smallest-cursor-first — and the ledger is alive: new writes
    /// land.
    fn assert_migrated_ledger(connection: &Connection) {
        assert_eq!(schema_version(connection).as_deref(), Some("2"));
        assert_eq!(count_for_session(connection, "s-1"), 2);
        assert_eq!(
            find_turn(connection, "s-1", "t-1"),
            Some(row("v:s:1", "t-1"))
        );
        let kept = find_turn(connection, "s-1", "t-2").expect("t-2 survives");
        assert_eq!(kept.view_cursor, "v:s:2");
        assert_eq!(kept.tokens_in, 200);
        let total: i64 = connection
            .query_row(
                "SELECT SUM(tokens_in) FROM usage_turns WHERE session_id = 's-1'",
                [],
                |row| row.get(0),
            )
            .expect("sum reads");
        assert_eq!(total, 300, "no turn counted twice, none lost");
        assert!(
            record_turn(connection, &row("v:s:9", "t-9")).expect("write after open"),
            "the ledger takes new writes afterwards"
        );
        assert_eq!(count_for_session(connection, "s-1"), 3);
    }

    #[test]
    fn an_interrupted_migration_at_any_statement_boundary_loses_no_history() {
        // The four statements the old code ran outside any transaction,
        // replayed directly: after each prefix the process "dies" (the
        // connection closes with the rest never run) and the next launch's
        // `open_at` must bring every original row back. The rename-only
        // prefix is the silent-loss case: an empty v2 stamped over orphaned
        // history must be unreachable.
        const STATEMENTS: [&str; 4] = [
            "ALTER TABLE usage_turns RENAME TO usage_turns_v1;",
            LEDGER_DDL_V2,
            COPY_V1_TO_V2,
            "DROP TABLE usage_turns_v1;",
        ];
        for (index, _) in STATEMENTS.iter().enumerate() {
            let dir = std::env::temp_dir().join(format!(
                "baaz-usage-d1fix-boundary-{}-{}",
                std::process::id(),
                index
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("baaz.db");
            build_v1_db(&path);
            {
                let connection = Connection::open(&path).expect("reopen");
                for statement in &STATEMENTS[..=index] {
                    connection.execute_batch(statement).expect("prefix replays");
                }
            }
            let connection =
                open_at(&path).expect("open_at recovers after a kill at any boundary");
            assert_migrated_ledger(&connection);
            assert!(
                !table_exists(&connection, "usage_turns_v1").expect("backup check"),
                "no backup table is left behind"
            );
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn an_interrupted_migration_leaves_the_v1_database_untouched() {
        // The seam stops the real migration after each of its four
        // statements. Because the swap runs in one transaction, the half
        // state never lands: no backup table, every original row still in
        // the live table, the stamp still 1 — and the retry completes.
        // Without the transaction this fails: the backup exists and the live
        // table is renamed away, empty, or half-copied.
        for after in 1..=4 {
            let dir = std::env::temp_dir().join(format!(
                "baaz-usage-d1fix-seam-{}-{}",
                std::process::id(),
                after
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("baaz.db");
            build_v1_db(&path);
            {
                let mut connection = Connection::open(&path).expect("reopen");
                migrate_interrupted_after(&mut connection, after)
                    .expect_err("the interruption fails the migration");
                assert!(
                    !table_exists(&connection, "usage_turns_v1").expect("backup check"),
                    "interruption after statement {after} leaves no backup table"
                );
                let count: i64 = connection
                    .query_row("SELECT COUNT(*) FROM usage_turns", [], |row| row.get(0))
                    .expect("the live table is still there");
                assert_eq!(count, 3, "every original row is still in place");
                assert_eq!(
                    schema_version(&connection).as_deref(),
                    Some("1"),
                    "the stamp never moved"
                );
            }
            let connection = open_at(&path).expect("the retry completes");
            assert_migrated_ledger(&connection);
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn a_leftover_backup_from_an_interrupted_attempt_recovers() {
        // The stuck states the old code left behind: both tables present
        // with a complete copy already in the live table, stamped 1 — every
        // future launch used to die on the leftover name. Now it merges and
        // moves on, with the row count right.
        let dir =
            std::env::temp_dir().join(format!("baaz-usage-d1fix-leftover-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("baaz.db");
        build_v1_db(&path);
        {
            let connection = Connection::open(&path).expect("reopen");
            connection
                .execute_batch("ALTER TABLE usage_turns RENAME TO usage_turns_v1;")
                .expect("rename");
            connection.execute_batch(LEDGER_DDL_V2).expect("rebuild");
            connection.execute(COPY_V1_TO_V2, []).expect("copy");
        }
        let connection = open_at(&path).expect("a leftover backup recovers, not errors");
        assert_migrated_ledger(&connection);
        assert!(
            !table_exists(&connection, "usage_turns_v1").expect("backup check"),
            "the backup is dropped after recovery"
        );
        let _ = std::fs::remove_dir_all(&dir);

        // The same leftover beside a stamped v2 drains into the live table:
        // the live row wins, the backup only fills gaps, nothing is lost.
        let dir2 =
            std::env::temp_dir().join(format!("baaz-usage-d1fix-drain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir2);
        let path2 = dir2.join("baaz.db");
        {
            let connection = open_at(&path2).expect("fresh v2");
            assert!(record_turn(&connection, &row("v:s:1", "t-1")).expect("t-1 records"));
            connection
                .execute_batch(
                    "CREATE TABLE usage_turns_v1 AS SELECT * FROM usage_turns WHERE 0;",
                )
                .expect("empty backup shell");
            let extra = row("v:s:2", "t-2");
            connection
                .execute(
                    "INSERT INTO usage_turns_v1(
                        session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms,
                        tokens_in, tokens_out, reasoning_tokens,
                        cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                    params![
                        extra.session_id,
                        extra.view_cursor,
                        extra.turn_id,
                        extra.finished_at_ms,
                        extra.model,
                        extra.duration_ms,
                        extra.tokens_in,
                        extra.tokens_out,
                        extra.reasoning_tokens,
                        extra.cache_read_tokens,
                        extra.cache_write_tokens,
                        extra.cached_tokens,
                        extra.cost_usd,
                    ],
                )
                .expect("backup row writes");
        }
        let connection = open_at(&path2).expect("a stamped v2 with a backup drains it");
        assert_eq!(find_turn(&connection, "s-1", "t-1"), Some(row("v:s:1", "t-1")));
        assert_eq!(find_turn(&connection, "s-1", "t-2"), Some(row("v:s:2", "t-2")));
        assert!(
            !table_exists(&connection, "usage_turns_v1").expect("backup check"),
            "the backup is dropped after draining"
        );
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[test]
    fn a_garbage_or_missing_schema_version_never_silently_kills_the_ledger() {
        // A populated v1 ledger whose stamp is garbage, and one whose stamp
        // row is missing entirely: `open_at` must migrate (so `record_turn`
        // works afterwards) — never take the fresh-install branch, stamp a
        // v2 over the v1 key, and leave the ledger silently dead.
        for (tag, stamp) in [("garbage", Some("garbage")), ("missing", None)] {
            let dir = std::env::temp_dir().join(format!(
                "baaz-usage-d1fix-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("baaz.db");
            build_v1_db(&path);
            {
                let connection = Connection::open(&path).expect("reopen");
                match stamp {
                    Some(value) => {
                        connection
                            .execute(
                                "UPDATE meta SET value = ?1 WHERE key = 'schema_version'",
                                params![value],
                            )
                            .expect("garbage stamp writes");
                    }
                    None => {
                        connection
                            .execute("DELETE FROM meta WHERE key = 'schema_version'", [])
                            .expect("stamp row deletes");
                    }
                }
            }
            let connection = open_at(&path)
                .expect("an unreadable stamp on a v1 ledger migrates, not silent death");
            assert_migrated_ledger(&connection);
            let _ = std::fs::remove_dir_all(&dir);
        }

        // A garbage stamp on a healthy v2 ledger re-stamps it: the rows are
        // intact and new writes land.
        let dir =
            std::env::temp_dir().join(format!("baaz-usage-d1fix-garbage-v2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("baaz.db");
        {
            let connection = open_at(&path).expect("fresh v2");
            assert!(record_turn(&connection, &row("v:s:1", "t-1")).expect("t-1 records"));
            connection
                .execute("UPDATE meta SET value = 'garbage' WHERE key = 'schema_version'", [])
                .expect("garbage stamp writes");
        }
        let connection = open_at(&path).expect("a garbage stamp on v2 re-stamps");
        assert_eq!(find_turn(&connection, "s-1", "t-1"), Some(row("v:s:1", "t-1")));
        assert!(
            record_turn(&connection, &row("v:s:2", "t-2")).expect("t-2 records"),
            "the ledger takes new writes afterwards"
        );
        assert_eq!(schema_version(&connection).as_deref(), Some("2"));
        let _ = std::fs::remove_dir_all(&dir);

        // A garbage stamp on a table of unknown shape fails loudly, with the
        // stamp untouched — never a silent v2 over a foreign table.
        let dir =
            std::env::temp_dir().join(format!("baaz-usage-d1fix-garbage-odd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("baaz.db");
        {
            std::fs::create_dir_all(&dir).expect("temp dir");
            let connection = Connection::open(&path).expect("odd file opens");
            connection
                .execute_batch(
                    "CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
                     INSERT INTO meta(key, value) VALUES ('schema_version', 'garbage');
                     CREATE TABLE usage_turns(id TEXT PRIMARY KEY, note TEXT);
                     INSERT INTO usage_turns(id, note) VALUES ('a', 'foreign');",
                )
                .expect("odd schema builds");
        }
        assert!(
            open_at(&path).is_err(),
            "an unreadable stamp on an unknown shape fails loudly"
        );
        {
            let connection = Connection::open(&path).expect("reopen");
            assert_eq!(
                schema_version(&connection).as_deref(),
                Some("garbage"),
                "the loud failure stamps nothing"
            );
            let count: i64 = connection
                .query_row("SELECT COUNT(*) FROM usage_turns", [], |row| row.get(0))
                .expect("foreign table intact");
            assert_eq!(count, 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
