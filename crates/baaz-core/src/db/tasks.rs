//! The `task` and `task_outbox` tables (docs/85 14.1): orchestration's
//! durable record ([`crate::tasks`]).
//!
//! A task row keeps the columns the app looks rows up by (lead, worker,
//! status, generation) and the whole record as JSON. Every change that
//! needs a side effect writes its [`crate::tasks::Effect`] in the same
//! transaction ([`save_with`]); the app marks it done when carried out
//! ([`mark_done`]), and the reopen reads what is still pending
//! ([`pending`]).
//!
//! Keyed by session id: `task.lead_session`, `task.worker_session`.

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::tasks::{Effect, OutboxItem, Task};

/// Version 6's tables.
pub(crate) const V6: &str = "
CREATE TABLE IF NOT EXISTS task(
    id TEXT PRIMARY KEY,
    lead_session TEXT NOT NULL,
    worker_session TEXT,
    status TEXT NOT NULL,
    generation INTEGER NOT NULL DEFAULT 0,
    record TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS task_lead ON task(lead_session);
CREATE INDEX IF NOT EXISTS task_worker ON task(worker_session);
CREATE TABLE IF NOT EXISTS task_outbox(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES task(id) ON DELETE CASCADE,
    effect TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    done_at INTEGER
);
CREATE INDEX IF NOT EXISTS task_outbox_pending ON task_outbox(done_at) WHERE done_at IS NULL;
";

fn record_json(task: &Task) -> String {
    serde_json::to_string(task).expect("a task serialises")
}

/// Write `task` (insert or replace) and, in the same transaction, queue
/// `effect` for it. Returns the outbox row's id when one was queued.
pub fn save_with(conn: &mut Connection, task: &Task, effect: Option<&Effect>) -> rusqlite::Result<Option<i64>> {
    super::check_writer("save task");
    let tx = conn.transaction()?;
    tx.execute(
        "INSERT INTO task(id, lead_session, worker_session, status, generation, record, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET worker_session = ?3, status = ?4, generation = ?5, record = ?6, updated_at = ?8",
        params![task.id, task.lead_session, task.worker_session, task.status.as_str(), task.generation, record_json(task), task.created_at, task.updated_at],
    )?;
    let queued = match effect {
        Some(effect) => {
            tx.execute(
                "INSERT INTO task_outbox(task_id, effect, created_at) VALUES (?1, ?2, ?3)",
                params![task.id, serde_json::to_string(effect).expect("an effect serialises"), super::now_ms()],
            )?;
            Some(tx.last_insert_rowid())
        }
        None => None,
    };
    tx.commit()?;
    Ok(queued)
}

/// Write `task` with no side effect.
pub fn save(conn: &mut Connection, task: &Task) -> rusqlite::Result<()> {
    save_with(conn, task, None).map(|_| ())
}

fn parse(record: String) -> Option<Task> {
    serde_json::from_str(&record).ok()
}

/// One task by id. A record this build cannot read is `None`.
pub fn load(conn: &Connection, id: &str) -> rusqlite::Result<Option<Task>> {
    super::check_writer("load task");
    let record: Option<String> = conn.prepare_cached("SELECT record FROM task WHERE id = ?1")?.query_row([id], |row| row.get(0)).optional()?;
    Ok(record.and_then(parse))
}

/// Every task, oldest first.
pub fn load_all(conn: &Connection) -> rusqlite::Result<Vec<Task>> {
    super::check_writer("load tasks");
    let mut rows = conn.prepare_cached("SELECT record FROM task ORDER BY created_at, id")?;
    let records = rows.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records.into_iter().filter_map(parse).collect())
}

/// A lead's tasks, oldest first.
pub fn for_lead(conn: &Connection, lead_session: &str) -> rusqlite::Result<Vec<Task>> {
    super::check_writer("load lead tasks");
    let mut rows = conn.prepare_cached("SELECT record FROM task WHERE lead_session = ?1 ORDER BY created_at, id")?;
    let records = rows.query_map([lead_session], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(records.into_iter().filter_map(parse).collect())
}

/// The task whose worker is `worker_session`, if any.
pub fn for_worker(conn: &Connection, worker_session: &str) -> rusqlite::Result<Option<Task>> {
    super::check_writer("load worker task");
    let record: Option<String> = conn
        .prepare_cached("SELECT record FROM task WHERE worker_session = ?1 ORDER BY updated_at DESC LIMIT 1")?
        .query_row([worker_session], |row| row.get(0))
        .optional()?;
    Ok(record.and_then(parse))
}

/// Outbox items not yet carried out, in the order they were queued. An
/// effect this build cannot read is skipped (and stays pending).
pub fn pending(conn: &Connection) -> rusqlite::Result<Vec<OutboxItem>> {
    super::check_writer("pending outbox");
    let mut rows = conn.prepare_cached("SELECT id, task_id, effect FROM task_outbox WHERE done_at IS NULL ORDER BY id")?;
    let items = rows
        .query_map([], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(items
        .into_iter()
        .filter_map(|(id, task_id, effect)| serde_json::from_str(&effect).ok().map(|effect| OutboxItem { id, task_id, effect }))
        .collect())
}

/// The outbox item `id` was carried out.
pub fn mark_done(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    super::check_writer("outbox done");
    super::exec(conn, "UPDATE task_outbox SET done_at = ?2 WHERE id = ?1", params![id, super::now_ms()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{reconcile, sample, Reconcile, Status};

    /// A task and its start are one write; the start stays pending until
    /// done; a reopen (a fresh connection to the same file) reads the task
    /// and the pending start back and the reconcile replays it.
    #[test]
    fn a_task_and_its_effect_land_together_and_survive_a_restart() {
        let dir = std::env::temp_dir().join(format!("baaz-tasks-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = super::super::path_in(&dir);
        super::super::as_writer(|| {
            let mut conn = super::super::open_at(&path).expect("opens");
            let mut task = sample("t-1", Status::Queued);
            task.move_to(Status::Running, 2).unwrap();
            task.worker_session = Some("w-1".into());
            let queued = save_with(&mut conn, &task, Some(&Effect::Start { generation: 0 })).unwrap();
            assert!(queued.is_some());
            let mut second = sample("t-2", Status::Waiting);
            second.after = vec!["t-1".into()];
            save(&mut conn, &second).unwrap();
            assert_eq!(for_lead(&conn, "lead").unwrap().len(), 2);
            assert_eq!(for_worker(&conn, "w-1").unwrap().map(|t| t.id), Some("t-1".into()));
        });
        super::super::close(path.clone());
        super::super::as_writer(|| {
            let conn = super::super::open_at(&path).expect("reopens");
            let tasks = load_all(&conn).unwrap();
            let pending = pending(&conn).unwrap();
            assert_eq!(pending.len(), 1);
            let out = reconcile(&tasks, &pending, &|_| false);
            assert_eq!(out, vec![Reconcile::Replay(pending[0].clone())], "the start replays; no second resume of the same task");
            mark_done(&conn, pending[0].id).unwrap();
            assert!(super::pending(&conn).unwrap().is_empty());
            let out = reconcile(&tasks, &[], &|_| false);
            assert_eq!(out, vec![Reconcile::Resume { task_id: "t-1".into() }], "the worker is gone: resume it");
        });
        super::super::close(path);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
