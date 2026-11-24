//! The import journal (`import_job`, `import_item`) and the session
//! migration's journal (`migration_journal`). Each is read whole and
//! written whole, in one transaction.

use rusqlite::{params, Connection};

use crate::import::{Journal, JournalRun, JournalSession};

/// The import journal: every run not undone, oldest first.
pub fn read_import(conn: &Connection) -> rusqlite::Result<Journal> {
    super::check_writer("read import journal");
    let mut runs: Vec<JournalRun> = conn
        .prepare_cached("SELECT id, started_at FROM import_job ORDER BY started_at, rowid")?
        .query_map([], |row| Ok(JournalRun { id: row.get(0)?, at_ms: row.get(1)?, sessions: Vec::new() }))?
        .collect::<rusqlite::Result<_>>()?;
    let mut items = conn.prepare_cached(
        "SELECT provider_key, provider_session_id FROM import_item WHERE job_id = ?1 ORDER BY ordinal",
    )?;
    for run in &mut runs {
        run.sessions = items
            .query_map(params![run.id], |row| Ok(JournalSession { provider: row.get(0)?, session_id: row.get(1)? }))?
            .collect::<rusqlite::Result<_>>()?;
    }
    Ok(Journal { runs })
}

/// Replace the import journal with `journal`, in one transaction.
pub fn write_import(conn: &mut Connection, journal: &Journal) -> rusqlite::Result<()> {
    super::check_writer("write import journal");
    let tx = conn.transaction()?;
    put_import(&tx, journal)?;
    tx.commit()
}

/// [`write_import`] inside a caller's transaction (the importer's).
pub(crate) fn put_import(conn: &Connection, journal: &Journal) -> rusqlite::Result<()> {
    super::exec(conn, "DELETE FROM import_item", [])?;
    super::exec(conn, "DELETE FROM import_job", [])?;
    for run in &journal.runs {
        super::exec(conn, 
            "INSERT OR REPLACE INTO import_job(id, started_at) VALUES (?1, ?2)",
            params![run.id, run.at_ms],
        )?;
        for (ordinal, session) in run.sessions.iter().enumerate() {
            super::exec(conn, 
                "INSERT OR REPLACE INTO import_item(job_id, ordinal, provider_key, provider_session_id)
                 VALUES (?1, ?2, ?3, ?4)",
                params![run.id, ordinal as i64, session.provider, session.session_id],
            )?;
        }
    }
    Ok(())
}

/// One finished move: its key, when it landed (Unix seconds) and how.
pub type Done = (String, u64, String);
/// One move in flight: its key and when it started (Unix seconds).
pub type Live = (String, u64);

const DONE: &str = "done";
const LIVE: &str = "in_progress";

/// The migration journal: finished moves and moves in flight.
pub fn read_migration(conn: &Connection) -> rusqlite::Result<(Vec<Done>, Vec<Live>)> {
    super::check_writer("read migration journal");
    let mut done = Vec::new();
    let mut live = Vec::new();
    let mut statement = conn.prepare_cached("SELECT key, state, at, outcome FROM migration_journal ORDER BY key")?;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let key: String = row.get(0)?;
        let state: String = row.get(1)?;
        let at = row.get::<_, i64>(2)?.max(0) as u64;
        if state == DONE {
            let outcome: Option<String> = row.get(3)?;
            done.push((key, at, outcome.unwrap_or_else(|| "moved".into())));
        } else {
            live.push((key, at));
        }
    }
    Ok((done, live))
}

/// Replace the migration journal, in one transaction.
pub fn write_migration(conn: &mut Connection, done: &[Done], live: &[Live]) -> rusqlite::Result<()> {
    super::check_writer("write migration journal");
    let tx = conn.transaction()?;
    put_migration(&tx, done, live)?;
    tx.commit()
}

/// [`write_migration`] inside a caller's transaction (the importer's).
pub(crate) fn put_migration(conn: &Connection, done: &[Done], live: &[Live]) -> rusqlite::Result<()> {
    super::exec(conn, "DELETE FROM migration_journal", [])?;
    for (key, at, outcome) in done {
        super::exec(conn, 
            "INSERT OR REPLACE INTO migration_journal(key, state, at, outcome) VALUES (?1, ?2, ?3, ?4)",
            params![key, DONE, *at as i64, outcome],
        )?;
    }
    for (key, at) in live {
        // A finished move never goes back to in flight.
        super::exec(conn, 
            "INSERT OR IGNORE INTO migration_journal(key, state, at) VALUES (?1, ?2, ?3)",
            params![key, LIVE, *at as i64],
        )?;
    }
    Ok(())
}
