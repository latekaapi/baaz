//! The `usage_daily` table (docs/85 16.1): the `turn` ledger rolled up by
//! day, provider, model and project, for the Usage page's 7/30/90-day
//! tables. The ledger stays the truth; the roll-up is refreshed from it
//! ([`refresh`]: every day on first fill, then the last two days, which are
//! the only ones a new turn can land in).
//!
//! Not keyed by session id: rows are per day × provider × model × project.

use rusqlite::{params, Connection};

/// Version 8's table.
pub(crate) const V8: &str = "
CREATE TABLE IF NOT EXISTS usage_daily(
    day TEXT NOT NULL,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    project TEXT NOT NULL,
    turns INTEGER NOT NULL,
    tokens_in INTEGER NOT NULL,
    tokens_out INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cost_usd REAL NOT NULL,
    PRIMARY KEY(day, provider, model, project)
);
";

/// A day as the table keys it (`2026-11-26`, local time), from ms.
pub fn day_of(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_millis_opt(ms).single().map(|t| t.format("%Y-%m-%d").to_string()).unwrap_or_default()
}

/// Bring the roll-up up to date with the ledger as of `now` (ms): all days
/// when it is empty, else the last two days.
pub fn refresh(conn: &Connection, now: i64) -> rusqlite::Result<()> {
    super::check_writer("refresh usage_daily");
    let empty: bool = conn.query_row("SELECT NOT EXISTS(SELECT 1 FROM usage_daily)", [], |row| row.get(0))?;
    let from = if empty { String::new() } else { day_of(now - 86_400_000) };
    conn.execute("DELETE FROM usage_daily WHERE day >= ?1", params![from])?;
    // Cached input is cache_read on Claude Code and cached_tokens on Codex;
    // either counts as cache.
    conn.execute(
        "INSERT INTO usage_daily(day, provider, model, project, turns, tokens_in, tokens_out, cache_read, cost_usd)
         SELECT date(t.finished_at / 1000, 'unixepoch', 'localtime') AS day,
                COALESCE(NULLIF(t.provider, ''), s.provider_key, '') AS provider,
                COALESCE(t.model, '') AS model,
                COALESCE(s.project_id, '') AS project,
                COUNT(*), COALESCE(SUM(t.tokens_in), 0), COALESCE(SUM(t.tokens_out), 0),
                COALESCE(SUM(COALESCE(t.cache_read, 0) + COALESCE(t.cached_tokens, 0)), 0),
                COALESCE(SUM(t.cost_usd), 0)
         FROM turn t LEFT JOIN session s ON s.id = t.session_id
         WHERE t.role = 'assistant' AND t.finished_at IS NOT NULL
           AND date(t.finished_at / 1000, 'unixepoch', 'localtime') >= ?1
         GROUP BY 1, 2, 3, 4",
        params![from],
    )?;
    Ok(())
}

/// What a table on the page is split by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum By {
    Day,
    Model,
    Project,
    Provider,
}

/// One row of a table: its key and its totals.
#[derive(Clone, Debug, PartialEq)]
pub struct Totals {
    pub key: String,
    pub turns: i64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_read: i64,
    pub cost_usd: f64,
}

impl Totals {
    /// Cached input as a share of all input, 0–100.
    pub fn cache_pct(&self) -> u32 {
        let all = self.tokens_in.max(self.cache_read).max(1);
        ((self.cache_read as f64 / all as f64) * 100.0).round().min(100.0) as u32
    }
}

/// The last `days` days (today included) split `by`, largest first (newest
/// first for days).
pub fn totals(conn: &Connection, by: By, days: u32, now: i64) -> rusqlite::Result<Vec<Totals>> {
    let from = day_of(now - (days.saturating_sub(1) as i64) * 86_400_000);
    let (key, order) = match by {
        By::Day => ("day", "day DESC"),
        By::Model => ("model", "SUM(tokens_in + tokens_out) DESC"),
        By::Project => ("project", "SUM(tokens_in + tokens_out) DESC"),
        By::Provider => ("provider", "SUM(tokens_in + tokens_out) DESC"),
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT {key}, SUM(turns), SUM(tokens_in), SUM(tokens_out), SUM(cache_read), SUM(cost_usd)
         FROM usage_daily WHERE day >= ?1 GROUP BY {key} ORDER BY {order}"
    ))?;
    let rows = stmt.query_map(params![from], |row| {
        Ok(Totals { key: row.get(0)?, turns: row.get(1)?, tokens_in: row.get(2)?, tokens_out: row.get(3)?, cache_read: row.get(4)?, cost_usd: row.get(5)? })
    })?;
    rows.collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(conn: &Connection, session: &str, id: &str, at: i64, provider: &str, model: &str, tin: i64, tout: i64, cache: i64, cost: f64) {
        conn.execute(
            "INSERT INTO turn(session_id, turn_id, finished_at, model, tokens_in, tokens_out, cache_read, cost_usd, provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![session, id, at, model, tin, tout, cache, cost, provider],
        )
        .unwrap();
    }

    #[test]
    fn the_rollup_sums_the_ledger_by_day_model_project_and_provider() {
        let conn = crate::db::open_memory();
        conn.execute_batch(
            "INSERT INTO session(provider_key, provider_session_id, id, project_id) VALUES ('codex', 'a', 's-a', 'p-web'), ('claude-code', 'b', 's-b', 'p-api');",
        )
        .unwrap();
        let day = 86_400_000;
        let now = 1_795_600_000_000; // 25 Nov 2026
        turn(&conn, "s-a", "t1", now, "codex", "gpt", 1000, 100, 800, 0.0);
        turn(&conn, "s-a", "t2", now - 60_000, "codex", "gpt", 500, 50, 0, 0.0);
        turn(&conn, "s-b", "t3", now - 3 * day, "claude-code", "sonnet", 200, 20, 600, 0.25);
        turn(&conn, "s-b", "t4", now - 40 * day, "claude-code", "sonnet", 999, 9, 0, 1.0);
        refresh(&conn, now).unwrap();
        let by_provider = totals(&conn, By::Provider, 7, now).unwrap();
        assert_eq!(by_provider.iter().map(|t| (t.key.as_str(), t.turns)).collect::<Vec<_>>(), vec![("codex", 2), ("claude-code", 1)]);
        assert_eq!(by_provider[0].tokens_in, 1500);
        let by_project = totals(&conn, By::Project, 90, now).unwrap();
        let api = by_project.iter().find(|t| t.key == "p-api").unwrap();
        assert_eq!((api.turns, api.cost_usd), (2, 1.25), "90 days reach the 40-day-old turn");
        assert_eq!(totals(&conn, By::Day, 7, now).unwrap().len(), 2, "two days with turns in the last week");
        assert_eq!(totals(&conn, By::Model, 30, now).unwrap()[0].key, "gpt");
        // A later turn today lands on refresh; older days are not rebuilt.
        turn(&conn, "s-a", "t5", now + 1_000, "codex", "gpt", 10, 1, 0, 0.0);
        refresh(&conn, now + 1_000).unwrap();
        assert_eq!(totals(&conn, By::Provider, 7, now).unwrap()[0].turns, 3);
        assert_eq!(Totals { key: String::new(), turns: 1, tokens_in: 200, tokens_out: 0, cache_read: 600, cost_usd: 0.0 }.cache_pct(), 100);
        assert_eq!(by_provider[0].cache_pct(), 53);
    }
}
