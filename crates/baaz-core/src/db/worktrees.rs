//! The `worktree` table (docs/82 6.1): one row per worktree Baaz made for a
//! session, and the session row's `worktree_id` that ties the two.
//!
//! A row is written before its folder exists ([`reserve`]): that is what
//! hands out the port block, which must be unique among rows, and the
//! writer thread runs one job at a time, so two worktrees made at once
//! never draw the same block. A row whose `git worktree add` failed is
//! deleted again ([`delete`]). An archived worktree keeps its row (and its
//! port block) with its snapshot ([`mark_archived`], [`mark_restored`]).
//!
//! Keyed by session id: `worktree.created_by_session` (the bare id of the
//! session it was made for, set at [`link_session`]) and
//! `session.worktree_id`.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::worktrees::{Snapshot, Worktree, PORT_BASE, PORT_BLOCK, PORT_BLOCKS};

/// Insert `worktree` (its `port` ignored) with the lowest free port block,
/// and return that block's first port. `None` when every block is taken.
pub fn reserve(conn: &Connection, worktree: &Worktree) -> rusqlite::Result<Option<u16>> {
    super::check_writer("reserve worktree");
    let mut taken = conn.prepare("SELECT port FROM worktree WHERE port IS NOT NULL")?;
    let taken: std::collections::HashSet<i64> =
        taken.query_map([], |row| row.get::<_, i64>(0))?.collect::<rusqlite::Result<_>>()?;
    let Some(port) = (0..PORT_BLOCKS)
        .map(|block| PORT_BASE + block * PORT_BLOCK)
        .find(|port| !taken.contains(&i64::from(*port)))
    else {
        return Ok(None);
    };
    conn.execute(
        "INSERT INTO worktree(id, project_id, path, branch, created_by_session, project_root, base, port, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            worktree.id,
            worktree.project_id,
            worktree.path.to_string_lossy(),
            worktree.branch,
            worktree.created_by_session,
            worktree.project_root.to_string_lossy(),
            worktree.base,
            port,
            super::now_ms(),
        ],
    )?;
    Ok(Some(port))
}

/// Delete the row `id`, first clearing every session row that points at it.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    super::check_writer("delete worktree");
    conn.execute("UPDATE session SET worktree_id = NULL WHERE worktree_id = ?1", params![id])?;
    conn.execute("DELETE FROM worktree WHERE id = ?1", params![id])?;
    Ok(())
}

/// Stamp the row's setup as run: [`setup_ran`] reads it.
pub fn mark_setup(conn: &Connection, id: &str) -> rusqlite::Result<()> {
    super::check_writer("mark worktree setup");
    conn.execute("UPDATE worktree SET setup_at = ?2 WHERE id = ?1", params![id, super::now_ms()])?;
    Ok(())
}

/// Whether the row's setup script already ran (or the row is gone).
pub fn setup_ran(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    super::check_writer("read worktree setup");
    let stamp: Option<Option<i64>> =
        conn.query_row("SELECT setup_at FROM worktree WHERE id = ?1", params![id], |row| row.get(0)).optional()?;
    Ok(stamp.is_none_or(|stamp| stamp.is_some()))
}

/// Record that the worktree `id` is archived: its snapshot (docs/82 6.2).
/// Written, and committed, before its folder is removed, so a crash
/// between the two never loses where the work went.
pub fn mark_archived(conn: &Connection, id: &str, snapshot: &Snapshot) -> rusqlite::Result<()> {
    super::check_writer("archive worktree");
    let changed = conn.execute(
        "UPDATE worktree SET archived_at = ?2, preserved_ref = ?3, preserved_head = ?4 WHERE id = ?1",
        params![id, super::now_ms(), snapshot.reference, snapshot.head],
    )?;
    if changed == 0 {
        return Err(rusqlite::Error::QueryReturnedNoRows);
    }
    Ok(())
}

/// Record that the worktree `id` is back, at `path` (its old folder, or a
/// fresh one when that was taken): the snapshot columns clear.
pub fn mark_restored(conn: &Connection, id: &str, path: &Path) -> rusqlite::Result<()> {
    super::check_writer("restore worktree");
    conn.execute(
        "UPDATE worktree SET archived_at = NULL, preserved_ref = NULL, preserved_head = NULL, path = ?2 WHERE id = ?1",
        params![id, path.to_string_lossy()],
    )?;
    Ok(())
}

/// The uuid of the session row `(provider, id)`: what a preserved ref is
/// named by.
pub fn session_key(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<String>> {
    super::check_writer("read session key");
    super::sessions::read_row(conn, provider, session_id)
}

/// Tie the session `(provider, id)` to the worktree whose folder is
/// `path`: the session row's `worktree_id`, and the worktree's
/// `created_by_session` when it names nobody yet. `false` when either row
/// is missing (a folder that is not a worktree Baaz made).
pub fn link_session(conn: &Connection, provider: &str, session_id: &str, path: &Path) -> rusqlite::Result<bool> {
    super::check_writer("link worktree");
    let Some(worktree) = by_path(conn, path)? else { return Ok(false) };
    let Some(uuid) = super::sessions::read_row(conn, provider, session_id)? else { return Ok(false) };
    conn.execute("UPDATE session SET worktree_id = ?2 WHERE id = ?1", params![uuid, worktree.id])?;
    conn.execute(
        "UPDATE worktree SET created_by_session = COALESCE(created_by_session, ?2) WHERE id = ?1",
        params![worktree.id, session_id],
    )?;
    Ok(true)
}

const COLUMNS: &str =
    "id, project_id, path, branch, created_by_session, project_root, base, port, archived_at, preserved_ref, preserved_head";

fn from_row(row: &rusqlite::Row) -> rusqlite::Result<Worktree> {
    let path: String = row.get(2)?;
    let path = PathBuf::from(path);
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let root: Option<String> = row.get(5)?;
    Ok(Worktree {
        id: row.get(0)?,
        project_id: row.get(1)?,
        name,
        path,
        branch: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
        created_by_session: row.get(4)?,
        project_root: root.map(PathBuf::from).unwrap_or_default(),
        base: row.get::<_, Option<String>>(6)?.unwrap_or_default(),
        port: row.get::<_, Option<i64>>(7)?.and_then(|port| u16::try_from(port).ok()).unwrap_or(0),
        snapshot: match row.get::<_, Option<i64>>(8)? {
            Some(_) => Some(Snapshot { reference: row.get(9)?, head: row.get::<_, Option<String>>(10)?.unwrap_or_default() }),
            None => None,
        },
    })
}

/// The worktree whose folder is `path`.
pub fn by_path(conn: &Connection, path: &Path) -> rusqlite::Result<Option<Worktree>> {
    super::check_writer("read worktree");
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM worktree WHERE path = ?1"),
        params![path.to_string_lossy()],
        from_row,
    )
    .optional()
}

/// The worktree the session `(provider, id)` runs in.
pub fn by_session(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Option<Worktree>> {
    super::check_writer("read worktree");
    let Some(uuid) = super::sessions::read_row(conn, provider, session_id)? else { return Ok(None) };
    conn.query_row(
        &format!(
            "SELECT {} FROM worktree w JOIN session s ON s.worktree_id = w.id WHERE s.id = ?1",
            COLUMNS.split(", ").map(|column| format!("w.{column}")).collect::<Vec<_>>().join(", ")
        ),
        params![uuid],
        from_row,
    )
    .optional()
}

/// Every worktree row, oldest first.
pub fn list(conn: &Connection) -> rusqlite::Result<Vec<Worktree>> {
    super::check_writer("list worktrees");
    let mut statement = conn.prepare(&format!("SELECT {COLUMNS} FROM worktree ORDER BY created_at, id"))?;
    let rows = statement.query_map([], from_row)?.collect();
    rows
}
