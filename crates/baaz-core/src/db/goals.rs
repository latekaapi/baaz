//! The `goal` table (docs/85 15.1, 15.6): one goal per session, the whole
//! record as JSON ([`crate::goals::Goal`]).
//!
//! Keyed by session id: `goal.session`.

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::goals::Goal;

/// Version 7's table.
pub(crate) const V7: &str = "
CREATE TABLE IF NOT EXISTS goal(
    session TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    record TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
";

/// Write `goal` (insert or replace by its session).
pub fn save(conn: &Connection, goal: &Goal) -> rusqlite::Result<()> {
    super::check_writer("save goal");
    conn.execute(
        "INSERT INTO goal(session, state, record, updated_at) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(session) DO UPDATE SET state = ?2, record = ?3, updated_at = ?4",
        params![goal.session, goal.state.as_str(), serde_json::to_string(goal).expect("a goal serialises"), goal.updated_at],
    )?;
    Ok(())
}

/// The session's goal, if it has one.
pub fn load(conn: &Connection, session: &str) -> rusqlite::Result<Option<Goal>> {
    let record: Option<String> = conn.query_row("SELECT record FROM goal WHERE session = ?1", params![session], |row| row.get(0)).optional()?;
    Ok(record.and_then(|r| serde_json::from_str(&r).ok()))
}

/// Every goal still being pursued (running or waiting for a usage limit):
/// what a relaunch looks at (15.6).
pub fn active(conn: &Connection) -> rusqlite::Result<Vec<Goal>> {
    let mut stmt = conn.prepare("SELECT record FROM goal WHERE state IN ('running', 'usage_wait') ORDER BY updated_at")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.filter_map(Result::ok).filter_map(|r| serde_json::from_str(&r).ok()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::goals::{limits_for, GoalState};

    #[test]
    fn a_goal_round_trips_and_only_active_ones_list() {
        let conn = crate::db::open_memory();
        let mut g = Goal::new("s1", "codex", "tests pass", Some("cargo test".into()), true, limits_for(provider::BillingMode::Subscription), 5);
        save(&conn, &g).unwrap();
        assert_eq!(load(&conn, "s1").unwrap(), Some(g.clone()));
        assert_eq!(active(&conn).unwrap().len(), 1);
        g.state = GoalState::Met;
        g.updated_at = 9;
        save(&conn, &g).unwrap();
        assert_eq!(load(&conn, "s1").unwrap().unwrap().state, GoalState::Met);
        assert!(active(&conn).unwrap().is_empty());
        assert_eq!(load(&conn, "nope").unwrap(), None);
    }
}
