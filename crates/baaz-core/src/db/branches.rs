//! The `branch` table (docs/85 16.3): which session is a branch of which,
//! and the user message it started at — what nests a branch under its
//! source in the sidebar and counts "2 branches" under the message.
//!
//! Keyed by session id: `branch.session` (the branch) and `branch.source`.

use rusqlite::{params, Connection};

/// Version 9's table.
pub(crate) const V9: &str = "
CREATE TABLE IF NOT EXISTS branch(
    session TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS branch_source ON branch(source);
";

/// Record `session` as a branch of `source` at `message`.
pub fn record(conn: &Connection, session: &str, source: &str, message: &str, at: i64) -> rusqlite::Result<()> {
    super::check_writer("record branch");
    conn.execute(
        "INSERT INTO branch(session, source, message, created_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session) DO UPDATE SET source = ?2, message = ?3",
        params![session, source, message, at],
    )?;
    Ok(())
}

/// Every branch as `(branch, source)`.
pub fn all(conn: &Connection) -> rusqlite::Result<Vec<(String, String)>> {
    let mut stmt = conn.prepare("SELECT session, source FROM branch ORDER BY created_at")?;
    let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

/// How many branches start at each of `source`'s messages.
pub fn counts(conn: &Connection, source: &str) -> rusqlite::Result<Vec<(String, u32)>> {
    let mut stmt = conn.prepare("SELECT message, COUNT(*) FROM branch WHERE source = ?1 GROUP BY message")?;
    let rows = stmt.query_map(params![source], |row| Ok((row.get(0)?, row.get(1)?)))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_record_and_count_per_message() {
        let conn = crate::db::open_memory();
        record(&conn, "b1", "s", "u-1", 1).unwrap();
        record(&conn, "b2", "s", "u-1", 2).unwrap();
        record(&conn, "b3", "s", "u-4", 3).unwrap();
        record(&conn, "b4", "other", "u-1", 4).unwrap();
        let mut c = counts(&conn, "s").unwrap();
        c.sort();
        assert_eq!(c, vec![("u-1".to_owned(), 2), ("u-4".to_owned(), 1)]);
        assert_eq!(all(&conn).unwrap().len(), 4);
    }
}
