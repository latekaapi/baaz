//! The `account` table (docs/85 16.1): one row per signed-in account a
//! provider reported — its email (the label), how it signed in (the lane),
//! its plan, and when it was last seen. Filled from the provider status
//! service; the Usage page reads the plan from here.
//!
//! Not keyed by session id: `account.id` is `<provider>:<label or lane>`.

use rusqlite::{params, Connection};

/// One account as a provider reported it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub provider: String,
    pub label: Option<String>,
    pub lane: Option<String>,
    pub plan: Option<String>,
    pub observed_at: i64,
}

impl Account {
    /// The row's id: the provider and who signed in.
    pub fn id(&self) -> String {
        format!("{}:{}", self.provider, self.label.as_deref().or(self.lane.as_deref()).unwrap_or("default"))
    }
}

/// Write `account` (insert or replace by its id). A provider the table
/// does not know (no `provider` row) is skipped.
pub fn record(conn: &Connection, account: &Account) -> rusqlite::Result<()> {
    super::check_writer("record account");
    let known: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM provider WHERE key = ?1)", params![account.provider], |row| row.get(0))?;
    if !known {
        return Ok(());
    }
    conn.execute(
        "INSERT INTO account(id, provider_key, lane, label, plan, observed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET lane = ?3, label = ?4, plan = COALESCE(?5, plan), observed_at = ?6",
        params![account.id(), account.provider, account.lane, account.label, account.plan, account.observed_at],
    )?;
    Ok(())
}

/// Every account, newest seen first.
pub fn all(conn: &Connection) -> rusqlite::Result<Vec<Account>> {
    let mut stmt = conn.prepare("SELECT provider_key, label, lane, plan, COALESCE(observed_at, 0) FROM account ORDER BY observed_at DESC")?;
    let rows = stmt.query_map([], |row| Ok(Account { provider: row.get(0)?, label: row.get(1)?, lane: row.get(2)?, plan: row.get(3)?, observed_at: row.get(4)? }))?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_account_is_recorded_once_and_keeps_its_plan() {
        let conn = crate::db::open_memory();
        let mut a = Account { provider: "codex".into(), label: Some("p@example.com".into()), lane: Some("chatgpt".into()), plan: Some("prolite".into()), observed_at: 5 };
        record(&conn, &a).unwrap();
        a.plan = None;
        a.observed_at = 9;
        record(&conn, &a).unwrap();
        let all = all(&conn).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!((all[0].plan.as_deref(), all[0].observed_at), (Some("prolite"), 9), "a reading without a plan keeps the known one");
        record(&conn, &Account { provider: "nobody".into(), label: None, lane: None, plan: None, observed_at: 1 }).unwrap();
        assert_eq!(super::all(&conn).unwrap().len(), 1, "an unknown provider is skipped");
    }
}
