//! `--perf-store <dir>`: the per-turn store costs, timed on a synthetic
//! store the size of the owner's (docs/51 step M).
//!
//! A finished turn costs Baaz an index refresh (Muse's `session-index.db`
//! re-read by stamp) and a search sync (`search.db` brought up to date for
//! the rows whose fingerprint moved). Both were measured at 34 s and 5.4 s
//! before P0 and at milliseconds after it; this mode re-times them on every
//! gate run so a regression fails `scripts/perf-budgets.py` instead of
//! reaching the owner as a hot fan.
//!
//! Offline and deterministic: it builds its own index and search database
//! under `<dir>` (fixed ids, fixed text, fixed stamps), reaches no model and
//! no provider, reads none of the owner's state, and prints one JSON object
//! on stdout. The sizes mirror the real store measured 2026-10-16 — 2,600
//! sessions — with a smaller body per session (16 KB against the real
//! 80 KB average) so a gate run stays seconds long; the costs that matter
//! here (the unchanged sync, the one-row sync, the stamp-only refresh) do
//! not scale with body size, which is what P0 made true.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use crate::index::IndexEntry;
use crate::search::{SessionRow, SyncRow};

/// Sessions in the synthetic store: the owner's count on 2026-10-16.
pub const SESSIONS: usize = 2_600;

/// Transcript text per synthetic session.
const BODY_BYTES: usize = 16 * 1024;

/// Provider-lane transcripts in the synthetic store: Claude Code and Codex
/// sessions searched beside Muse's (docs/50 P3). The owner had 91.
pub const PROVIDER_SESSIONS: usize = 120;

/// One deterministic body: words that vary by session so FTS has real
/// postings, padded to [`BODY_BYTES`].
fn body_for(index: usize) -> String {
    const WORDS: [&str; 16] = [
        "parser", "router", "sidebar", "terminal", "approval", "render", "stream", "search", "index", "commit",
        "branch", "cursor", "layout", "session", "provider", "handoff",
    ];
    let mut out = String::with_capacity(BODY_BYTES + 64);
    let mut n = index;
    while out.len() < BODY_BYTES {
        out.push_str(WORDS[n % WORDS.len()]);
        out.push(' ');
        out.push_str(&format!("token{} ", n % 997));
        n = n.wrapping_mul(31).wrapping_add(7);
    }
    out
}

fn session_id(index: usize) -> String {
    format!("s-perf-{index:05}")
}

/// Write a Muse-shaped `session-index.db` with [`SESSIONS`] rows and the
/// covering stamp index Muse carries (`idx_sessions_created`).
fn write_index(path: &Path) -> rusqlite::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut connection = rusqlite::Connection::open(path)?;
    connection.execute_batch(
        "CREATE TABLE sessions(session_id TEXT PRIMARY KEY, session_name TEXT, title TEXT, first_user_prompt TEXT, search_text TEXT, updated_at_us INTEGER, workspace_root TEXT);
         CREATE INDEX idx_sessions_created ON sessions(updated_at_us, session_id);",
    )?;
    let transaction = connection.transaction()?;
    {
        let mut insert = transaction.prepare("INSERT INTO sessions VALUES (?1, NULL, ?2, ?3, ?4, ?5, ?6)")?;
        for index in 0..SESSIONS {
            insert.execute(rusqlite::params![
                session_id(index),
                format!("Session {index} about the {}", ["parser", "router", "sidebar"][index % 3]),
                format!("please look at item {index}"),
                body_for(index),
                1_700_000_000_000_000_i64 + index as i64,
                format!("/work/project-{}", index % 12),
            ])?;
        }
    }
    transaction.commit()
}

fn rows_for(index: &HashMap<String, IndexEntry>) -> Vec<(SessionRow, Vec<String>, i64)> {
    let mut ids: Vec<&String> = index.keys().collect();
    ids.sort();
    ids.into_iter()
        .map(|id| {
            let entry = &index[id];
            let row = SessionRow {
                session_id: id.clone(),
                label: entry.title.clone(),
                title: entry.title.clone(),
                first_prompt: entry.first_user_prompt.clone().unwrap_or_default(),
                body: String::new(),
                project: crate::search::project_terms(entry.workspace_root.as_deref(), None),
                workspace: entry.workspace_root.clone(),
            };
            // The same shape the app's fingerprint has: the row's words and
            // the body source's stamp.
            let fingerprint = entry.updated_at_us.unwrap_or(1).max(1);
            (row, vec![id.clone()], fingerprint)
        })
        .collect()
}

fn sync(
    connection: &mut rusqlite::Connection,
    rows: &[(SessionRow, Vec<String>, i64)],
    index_path: &Path,
) -> crate::search::SyncStats {
    let synced: Vec<SyncRow<'_>> = rows
        .iter()
        .map(|(row, body_ids, fingerprint)| SyncRow { row, body_ids, fingerprint: *fingerprint })
        .collect();
    crate::search::sync_sessions(connection, &synced, &|ids| crate::index::read_bodies(index_path, ids))
        .unwrap_or_default()
}

fn ms(at: Instant) -> f64 {
    (at.elapsed().as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

/// Build the store under `dir`, time each per-turn cost, print one JSON
/// object, and return it. Every metric is milliseconds.
pub fn run(dir: &Path) -> serde_json::Value {
    let _ = std::fs::create_dir_all(dir);
    let index_path = dir.join("session-index.db");
    let search_path = dir.join("search.db");
    let cache_path = dir.join("index-cache.json");
    let _ = std::fs::remove_file(&search_path);
    let _ = std::fs::remove_file(dir.join("search.db-wal"));
    let _ = std::fs::remove_file(dir.join("search.db-shm"));
    if let Err(error) = write_index(&index_path) {
        eprintln!("perf-store: cannot write the synthetic index: {error}");
        std::process::exit(2);
    }
    let mut out = serde_json::Map::new();
    let mut put = |key: &str, value: f64| {
        out.insert(key.to_owned(), serde_json::json!(value));
    };

    // Boot: the full label read, then the cache the next boot paints from.
    let at = Instant::now();
    let mut index = crate::index::read_at(&index_path);
    put("index_full_read_ms", ms(at));
    crate::index::save_cache(&cache_path, &index);
    let at = Instant::now();
    let cached = crate::index::load_cache(&cache_path);
    put("index_cache_load_ms", ms(at));
    assert_eq!(cached.len(), SESSIONS, "the cache round-trips every row");

    // A finished turn with nothing new in Muse's index, then with one row
    // moved: the stamp-only refresh.
    let at = Instant::now();
    let refreshed = crate::index::read_changed(&index_path, &index);
    put("index_refresh_unchanged_ms", ms(at));
    assert_eq!(refreshed.len(), SESSIONS);
    {
        let connection = rusqlite::Connection::open(&index_path).expect("index reopen");
        connection
            .execute(
                "UPDATE sessions SET updated_at_us = updated_at_us + 1000, search_text = search_text || ' freshword' WHERE session_id = ?1",
                [session_id(SESSIONS / 2)],
            )
            .expect("bump one row");
    }
    let at = Instant::now();
    index = crate::index::read_changed(&index_path, &index);
    put("index_refresh_one_ms", ms(at));

    // The search sync: first run writes everything, a repeat writes
    // nothing, and a turn in one session writes one row.
    let mut connection = match crate::search::open_at(&search_path) {
        Ok(connection) => connection,
        Err(error) => {
            eprintln!("perf-store: cannot open the search db: {error}");
            std::process::exit(2);
        }
    };
    let mut rows = rows_for(&index);
    let at = Instant::now();
    let first = sync(&mut connection, &rows, &index_path);
    put("search_first_sync_ms", ms(at));
    assert_eq!(first.written, SESSIONS, "the first sync writes every row");
    let at = Instant::now();
    let again = sync(&mut connection, &rows, &index_path);
    put("search_sync_unchanged_ms", ms(at));
    assert_eq!(again.written, 0, "an unchanged sync writes nothing");
    let target = SESSIONS / 3;
    rows[target].2 += 1;
    let at = Instant::now();
    let one = sync(&mut connection, &rows, &index_path);
    put("search_sync_one_ms", ms(at));
    assert_eq!(one.written, 1, "one changed row writes one row");

    // The palette's query, against the synced store.
    let at = Instant::now();
    let hits = crate::search::query_sessions(&connection, "freshword", crate::search::LIMIT);
    put("search_query_ms", ms(at));
    assert!(hits.iter().any(|hit| hit.session_id == session_id(SESSIONS / 2)), "the bumped row is findable");

    // Claude Code and Codex transcripts read for search (docs/50 P3): the
    // body fetch a changed provider row pays.
    let (homes, providers) = crate::provider_search::write_synthetic(dir, PROVIDER_SESSIONS);
    let at = Instant::now();
    let bodies = crate::provider_search::read_bodies_in(&providers, &homes);
    put("provider_bodies_ms", ms(at));
    assert_eq!(bodies.len(), PROVIDER_SESSIONS, "every synthetic provider transcript yields text");

    // The launch listing (speed P1, docs/51 §8): a saved copy of every row,
    // then a boot that asks only for what moved since. The server is
    // scripted in-process — the wire is Muse's to time — so what is timed is
    // Baaz's own share: reading the copy and merging the newer rows in. The
    // page count is the guard: a listing that falls back to every page reads
    // 13 here and fails its budget of 1.
    let list_path = dir.join("session-list-cache.json");
    let mut server = ListServer { rows: (0..SESSIONS).map(list_row).collect(), pages: 0 };
    let full = crate::list_cache::list(None, "perf", 0, &mut |ask| server.page(ask), &mut |_, _, _| {})
        .expect("the scripted listing");
    let saved = full.save.expect("a full listing is saved");
    crate::list_cache::save(&list_path, &saved);
    let at = Instant::now();
    let loaded: crate::list_cache::ListCache<ListRow> =
        crate::list_cache::load(&list_path).expect("the copy reads back");
    put("list_cache_load_ms", ms(at));
    assert_eq!(loaded.sessions.len(), SESSIONS);
    server.rows[SESSIONS / 2].updated_at = "2026-10-22T00:00:00Z".into();
    server.rows[SESSIONS / 2].activity_at = "2026-10-22T00:00:00Z".into();
    server.pages = 0;
    let at = Instant::now();
    let newer = crate::list_cache::list(Some(&loaded), "perf", 1, &mut |ask| server.page(ask), &mut |_, _, _| {})
        .expect("the scripted listing");
    put("list_newer_ms", ms(at));
    put("list_newer_pages", server.pages as f64);
    assert_eq!(newer.sessions[0].id, session_id(SESSIONS / 2), "the moved row leads the merged list");

    let value = serde_json::Value::Object(out);
    println!("{value}");
    value
}

/// A list row the size of Muse's after [`crate::list_cache::Row::compact`]:
/// a 1,000-character title and first prompt.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
struct ListRow {
    id: String,
    updated_at: String,
    activity_at: String,
    title: String,
    first_user_prompt: String,
    workspace_root: String,
}

impl crate::list_cache::Row for ListRow {
    fn id(&self) -> &str {
        &self.id
    }
    fn updated_at(&self) -> &str {
        &self.updated_at
    }
    fn activity_at(&self) -> &str {
        &self.activity_at
    }
    fn compact(&mut self) {
        crate::list_cache::keep_prefix(&mut self.title);
        crate::list_cache::keep_prefix(&mut self.first_user_prompt);
    }
}

fn list_row(index: usize) -> ListRow {
    // Newest first, a minute apart, like the server's order.
    let at = chrono::DateTime::from_timestamp(1_790_000_000 - index as i64 * 60, 0)
        .expect("a valid stamp")
        .to_rfc3339_opts(chrono::SecondsFormat::Micros, true);
    ListRow {
        id: session_id(index),
        updated_at: at.clone(),
        activity_at: at,
        title: body_for(index).chars().take(1000).collect(),
        first_user_prompt: body_for(index + 1).chars().take(1000).collect(),
        workspace_root: format!("/work/project-{}", index % 12),
    }
}

/// `session/list` as muse 1.4.11 serves it: 200 a page, `updatedAfter`
/// strictly later.
struct ListServer {
    rows: Vec<ListRow>,
    pages: usize,
}

impl ListServer {
    fn page(&mut self, ask: crate::list_cache::Ask<'_>) -> Result<crate::list_cache::Page<ListRow>, String> {
        self.pages += 1;
        let after = ask.updated_after.and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
        let rows: Vec<&ListRow> = self
            .rows
            .iter()
            .filter(|row| {
                after.is_none_or(|after| {
                    chrono::DateTime::parse_from_rfc3339(&row.updated_at).is_ok_and(|at| at > after)
                })
            })
            .collect();
        let start: usize = ask.cursor.as_deref().and_then(|c| c.parse().ok()).unwrap_or(0);
        let end = (start + ask.limit as usize).min(rows.len());
        Ok(crate::list_cache::Page {
            rows: rows[start..end].iter().map(|row| (*row).clone()).collect(),
            next: (end < rows.len()).then(|| end.to_string()),
        })
    }
}
