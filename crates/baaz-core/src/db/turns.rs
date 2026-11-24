//! The `turn` table: the usage ledger (assistant turns) and the bubble text
//! a submit showed (user turns), each keyed by its session's row and the
//! provider's turn id.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::usage::UsageRow;

/// The `turn.role` of a compaction's ledger row (docs/85 10.4): its cost
/// counts in totals; per-turn readers, which ask for `assistant`, skip it.
pub const ROLE_COMPACTION: &str = "compaction";

/// Where a turn's `cost_usd` came from.
pub const COST_REPORTED: &str = "reported";
/// The provider never said what a turn cost: `cost_usd` is `NULL`.
pub const COST_UNKNOWN: &str = "unknown";

/// Whether a ledger row's cost is the provider's word: Claude Code reports
/// every turn's cost (a `0.0` there is free); Codex and Muse never do, so
/// their `0.0` is unknown (D-7).
fn cost_of(row: &UsageRow) -> (Option<f64>, &'static str) {
    if row.provider == crate::import::CLAUDE || row.cost_usd != 0.0 {
        (Some(row.cost_usd), COST_REPORTED)
    } else {
        (None, COST_UNKNOWN)
    }
}

/// Record one finished turn under its session's row (made when missing).
/// The first write wins: replaying a recorded turn is a no-op. Returns
/// whether the row was new.
pub fn record_ledger_row(conn: &Connection, row: &UsageRow) -> rusqlite::Result<bool> {
    super::check_writer("record turn");
    let uuid = super::sessions::ensure_session(conn, &row.provider, &row.session_id, super::sessions::ORIGIN_LISTED)?;
    let (cost, source) = cost_of(row);
    let changed = super::exec(conn, 
        "INSERT INTO turn(session_id, turn_id, role, view_cursor, finished_at, model, duration_ms, tokens_in,
            tokens_out, reasoning, cache_read, cache_write, cached_tokens, cost_usd, cost_source, provider)
         VALUES (?1, ?2, ?16, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
         ON CONFLICT(session_id, turn_id) DO NOTHING",
        params![
            uuid,
            row.turn_id,
            row.view_cursor,
            row.finished_at_ms,
            row.model,
            row.duration_ms,
            row.tokens_in,
            row.tokens_out,
            row.reasoning_tokens,
            row.cache_read_tokens,
            row.cache_write_tokens,
            row.cached_tokens,
            cost,
            source,
            row.provider,
            if row.compaction { ROLE_COMPACTION } else { "assistant" },
        ],
    )?;
    Ok(changed == 1)
}

/// Record finished turns, all in one transaction.
pub fn record_ledger(conn: &mut Connection, rows: &[UsageRow]) -> rusqlite::Result<usize> {
    let tx = conn.transaction()?;
    let mut added = 0;
    for row in rows {
        added += usize::from(record_ledger_row(&tx, row)?);
    }
    tx.commit()?;
    Ok(added)
}

/// The ledger row for the bare `session_id`'s turn `turn_id`.
pub fn find_ledger(conn: &Connection, session_id: &str, turn_id: &str) -> Option<UsageRow> {
    conn.query_row(
        "SELECT s.provider_session_id, t.view_cursor, t.turn_id, t.finished_at, t.model, t.duration_ms, t.tokens_in,
                t.tokens_out, t.reasoning, t.cache_read, t.cache_write, t.cached_tokens, t.cost_usd, t.provider
         FROM turn t JOIN session s ON s.id = t.session_id
         WHERE s.provider_session_id = ?1 AND t.turn_id = ?2 AND t.role = 'assistant'",
        params![session_id, turn_id],
        |row| {
            Ok(UsageRow {
                session_id: row.get(0)?,
                view_cursor: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                turn_id: row.get(2)?,
                finished_at_ms: row.get::<_, Option<i64>>(3)?.unwrap_or(0),
                model: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                duration_ms: row.get::<_, Option<i64>>(5)?.unwrap_or(0),
                tokens_in: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                tokens_out: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                reasoning_tokens: row.get::<_, Option<i64>>(8)?.unwrap_or(0),
                cache_read_tokens: row.get(9)?,
                cache_write_tokens: row.get(10)?,
                cached_tokens: row.get::<_, Option<i64>>(11)?.unwrap_or(0),
                cost_usd: row.get::<_, Option<f64>>(12)?.unwrap_or(0.0),
                provider: row.get(13)?,
                compaction: false,
            })
        },
    )
    .ok()
}

/// A turn's cost as stored: `(cost_usd, cost_source)`.
pub fn cost(conn: &Connection, session_id: &str, turn_id: &str) -> Option<(Option<f64>, String)> {
    conn.query_row(
        "SELECT t.cost_usd, t.cost_source FROM turn t JOIN session s ON s.id = t.session_id
         WHERE s.provider_session_id = ?1 AND t.turn_id = ?2",
        params![session_id, turn_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .ok()
}

/// How many ledger turns the bare `session_id` has.
pub fn count_ledger(conn: &Connection, session_id: &str) -> usize {
    conn.query_row(
        "SELECT COUNT(*) FROM turn t JOIN session s ON s.id = t.session_id
         WHERE s.provider_session_id = ?1 AND t.role = 'assistant'",
        params![session_id],
        |row| row.get::<_, i64>(0),
    )
    .unwrap_or(0)
    .max(0) as usize
}

/// The bubble texts a session's submits showed: by the user turn's id, and,
/// for rows recorded before the turn was known (imported), by the text the
/// provider echoes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Displays {
    /// User turn id → bubble text.
    pub by_turn: HashMap<String, String>,
    /// Echoed text → bubble text, for rows with no real turn id.
    pub by_text: HashMap<String, String>,
}

/// The prefix of a turn id that stands for no real turn: a display text
/// imported from before turns were known.
pub const LEGACY_TURN_PREFIX: &str = "legacy-display-";

/// Record the bubble text `display` for the user turn `turn_id`, whose
/// model-visible text was `input_text`.
pub fn note_display(
    conn: &Connection,
    provider: &str,
    session_id: &str,
    turn_id: &str,
    input_text: &str,
    display: &str,
) -> rusqlite::Result<()> {
    super::check_writer("note display text");
    let uuid = super::sessions::ensure_session(conn, provider, session_id, super::sessions::ORIGIN_LISTED)?;
    super::exec(conn, 
        "INSERT INTO turn(session_id, turn_id, role, input_text, display_text, finished_at)
         VALUES (?1, ?2, 'user', ?3, ?4, ?5)
         ON CONFLICT(session_id, turn_id) DO UPDATE SET input_text = excluded.input_text,
            display_text = excluded.display_text",
        params![uuid, turn_id, input_text, display, super::now_ms()],
    )?;
    Ok(())
}

/// Every bubble text recorded for `(provider, session_id)`.
pub fn displays(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Displays> {
    super::check_writer("read display texts");
    let mut out = Displays::default();
    let Some(uuid) = super::sessions::read_row(conn, provider, session_id)? else { return Ok(out) };
    let mut statement = conn.prepare_cached(
        "SELECT turn_id, input_text, display_text FROM turn
         WHERE session_id = ?1 AND role = 'user' AND display_text IS NOT NULL",
    )?;
    let mut rows = statement.query(params![uuid])?;
    while let Some(row) = rows.next()? {
        let turn_id: String = row.get(0)?;
        let input: Option<String> = row.get(1)?;
        let display: String = row.get(2)?;
        if turn_id.starts_with(LEGACY_TURN_PREFIX) {
            if let Some(input) = input {
                out.by_text.insert(input, display);
            }
        } else {
            out.by_turn.insert(turn_id, display);
        }
    }
    Ok(out)
}

/// Whether a turn row exists (tests and the importer's count).
pub fn has_turn(conn: &Connection, session_uuid: &str, turn_id: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM turn WHERE session_id = ?1 AND turn_id = ?2",
        params![session_uuid, turn_id],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}
