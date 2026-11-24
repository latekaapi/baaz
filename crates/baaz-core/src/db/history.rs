//! The `prompt_history` table: every prompt sent, per workspace, oldest
//! first ([`crate::history`] decides what is kept).

use rusqlite::{params, Connection};

/// The prompts kept for `workspace`, oldest first.
pub fn read(conn: &Connection, workspace: &str) -> rusqlite::Result<Vec<String>> {
    super::check_writer("read prompt history");
    let mut statement =
        conn.prepare_cached("SELECT text FROM prompt_history WHERE workspace_path = ?1 ORDER BY ordinal")?;
    let rows = statement.query_map(params![workspace], |row| row.get(0))?;
    rows.collect()
}

/// Replace `workspace`'s prompts with `entries`, in one transaction.
pub fn replace(conn: &mut Connection, workspace: &str, entries: &[String]) -> rusqlite::Result<()> {
    super::check_writer("write prompt history");
    let tx = conn.transaction()?;
    write(&tx, workspace, entries)?;
    tx.commit()
}

/// Add one prompt to `workspace`'s history and drop what falls past `cap`
/// (docs/85 10.8): one row written, never the whole list again. Ordinals
/// only grow; reads order by them.
pub fn append_one(conn: &mut Connection, workspace: &str, text: &str, cap: usize) -> rusqlite::Result<()> {
    super::check_writer("append prompt history");
    let tx = conn.transaction()?;
    let next: i64 = tx
        .prepare_cached("SELECT COALESCE(MAX(ordinal) + 1, 0) FROM prompt_history WHERE workspace_path = ?1")?
        .query_row(params![workspace], |row| row.get(0))?;
    super::exec(&tx, "INSERT INTO prompt_history(workspace_path, ordinal, text) VALUES (?1, ?2, ?3)", params![workspace, next, text])?;
    super::exec(
        &tx,
        "DELETE FROM prompt_history WHERE workspace_path = ?1 AND ordinal <= ?2",
        params![workspace, next - cap as i64],
    )?;
    tx.commit()
}

/// Forget every workspace's prompts (Settings › Data › Prompt history,
/// docs/82 8.8). Returns how many were removed.
pub fn clear_all(conn: &Connection) -> rusqlite::Result<usize> {
    super::check_writer("clear prompt history");
    super::exec(conn, "DELETE FROM prompt_history", [])
}

/// [`replace`] inside a caller's transaction (the importer's).
pub(crate) fn write(conn: &Connection, workspace: &str, entries: &[String]) -> rusqlite::Result<()> {
    super::exec(conn, "DELETE FROM prompt_history WHERE workspace_path = ?1", params![workspace])?;
    let mut insert =
        conn.prepare_cached("INSERT INTO prompt_history(workspace_path, ordinal, text) VALUES (?1, ?2, ?3)")?;
    for (ordinal, text) in entries.iter().enumerate() {
        insert.execute(params![workspace, ordinal as i64, text])?;
    }
    Ok(())
}
