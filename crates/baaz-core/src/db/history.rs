//! The `prompt_history` table: every prompt sent, per workspace, oldest
//! first ([`crate::history`] decides what is kept).

use rusqlite::{params, Connection};

/// The prompts kept for `workspace`, oldest first.
pub fn read(conn: &Connection, workspace: &str) -> rusqlite::Result<Vec<String>> {
    super::check_writer("read prompt history");
    let mut statement =
        conn.prepare("SELECT text FROM prompt_history WHERE workspace_path = ?1 ORDER BY ordinal")?;
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

/// [`replace`] inside a caller's transaction (the importer's).
pub(crate) fn write(conn: &Connection, workspace: &str, entries: &[String]) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM prompt_history WHERE workspace_path = ?1", params![workspace])?;
    let mut insert =
        conn.prepare("INSERT INTO prompt_history(workspace_path, ordinal, text) VALUES (?1, ?2, ?3)")?;
    for (ordinal, text) in entries.iter().enumerate() {
        insert.execute(params![workspace, ordinal as i64, text])?;
    }
    Ok(())
}
