//! Muse's listing and index facts, kept in the `session` table (docs/85
//! 11.2): baaz.db is the one list source for every provider. A Muse session
//! the person never opened in Baaz has a row of its own here, carrying what
//! its `session/list` row said (`listing`, JSON, in the server's order) and
//! what Muse's index knows of it (`index_facts`, JSON; `created_by` as a
//! column: the made-elsewhere rule). `session-list-cache.json` and
//! `index-cache.json` are no longer written: their contents live here, and
//! the old files are imported once and removed.

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::index::IndexEntry;
use crate::list_cache::{ListCache, ListedRow};

const PROVIDER: &str = "muse";

fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    super::exec(conn, "INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", params![key, value]).map(|_| ())
}

fn meta_get(conn: &Connection, key: &str) -> Option<String> {
    super::schema::meta(conn, key).ok().flatten()
}

/// Keep `listing` as the Muse list: every row upserted under its session
/// (made when missing), in order; a session no longer listed loses its
/// listing. One transaction.
pub fn save_listing(conn: &mut Connection, listing: &ListCache<ListedRow>) -> rusqlite::Result<()> {
    super::check_writer("save listing");
    let tx = conn.transaction()?;
    super::exec(&tx, "UPDATE session SET listing = NULL, listing_order = NULL WHERE listing IS NOT NULL", [])?;
    for (order, row) in listing.sessions.iter().enumerate() {
        let uuid = super::sessions::ensure_session(&tx, PROVIDER, &row.session_id, super::sessions::ORIGIN_LISTED)?;
        let json = serde_json::to_string(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        super::exec(&tx, "UPDATE session SET listing = ?1, listing_order = ?2 WHERE id = ?3", params![json, order as i64, uuid])?;
    }
    meta_set(&tx, "listing_source", &listing.source)?;
    meta_set(&tx, "listing_full_at", &listing.full_at_ms.to_string())?;
    meta_set(&tx, "listing_version", &listing.version.to_string())?;
    tx.commit()
}

/// The kept Muse list, `None` when none was ever kept (or by another
/// version).
pub fn load_listing(conn: &Connection) -> rusqlite::Result<Option<ListCache<ListedRow>>> {
    super::check_writer("load listing");
    let Some(version) = meta_get(conn, "listing_version").and_then(|v| v.parse::<u32>().ok()) else { return Ok(None) };
    if version != crate::list_cache::VERSION {
        return Ok(None);
    }
    let mut statement = conn.prepare_cached("SELECT listing FROM session WHERE listing IS NOT NULL ORDER BY listing_order")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut sessions = Vec::new();
    for json in rows {
        if let Ok(row) = serde_json::from_str::<ListedRow>(&json?) {
            sessions.push(row);
        }
    }
    Ok(Some(ListCache {
        version,
        source: meta_get(conn, "listing_source").unwrap_or_default(),
        full_at_ms: meta_get(conn, "listing_full_at").and_then(|v| v.parse().ok()).unwrap_or(0),
        sessions,
    }))
}

/// Keep `index` as Muse's index facts: each session's entry (made when
/// missing), `created_by` beside it; a session no longer in the index
/// loses its facts. One transaction.
pub fn save_index(conn: &mut Connection, index: &HashMap<String, IndexEntry>) -> rusqlite::Result<()> {
    super::check_writer("save index");
    let tx = conn.transaction()?;
    super::exec(&tx, "UPDATE session SET index_facts = NULL WHERE index_facts IS NOT NULL", [])?;
    for (id, entry) in index {
        let uuid = super::sessions::ensure_session(&tx, PROVIDER, id, super::sessions::ORIGIN_LISTED)?;
        let mut kept = entry.clone();
        kept.search_text.clear();
        let json = serde_json::to_string(&kept).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        super::exec(&tx, "UPDATE session SET index_facts = ?1, created_by = ?2 WHERE id = ?3", params![json, entry.created_by, uuid])?;
    }
    meta_set(&tx, "index_kept", "1")?;
    tx.commit()
}

/// The kept index facts, keyed by session id (empty when none were kept).
pub fn load_index(conn: &Connection) -> rusqlite::Result<HashMap<String, IndexEntry>> {
    super::check_writer("load index");
    let mut statement = conn.prepare_cached("SELECT provider_session_id, index_facts FROM session WHERE index_facts IS NOT NULL")?;
    let rows = statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?;
    let mut out = HashMap::new();
    for row in rows {
        let (id, json) = row?;
        if let Ok(entry) = serde_json::from_str::<IndexEntry>(&json) {
            out.insert(id, entry);
        }
    }
    Ok(out)
}

/// What a boot paints the sidebar from, in one query over the session table
/// (docs/85 11.2): the kept Muse list (`None` when none was kept) and the
/// index facts.
pub fn load_both(conn: &Connection) -> rusqlite::Result<(Option<ListCache<ListedRow>>, HashMap<String, IndexEntry>)> {
    super::check_writer("load list and index");
    let version = meta_get(conn, "listing_version").and_then(|v| v.parse::<u32>().ok());
    let mut statement = conn.prepare_cached(
        "SELECT provider_session_id, listing, index_facts FROM session
         WHERE listing IS NOT NULL OR index_facts IS NOT NULL
         ORDER BY listing IS NULL, listing_order",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?))
    })?;
    let mut sessions = Vec::new();
    let mut index = HashMap::new();
    for row in rows {
        let (id, listing, facts) = row?;
        if let Some(row) = listing.and_then(|json| serde_json::from_str::<ListedRow>(&json).ok()) {
            sessions.push(row);
        }
        if let Some(entry) = facts.and_then(|json| serde_json::from_str::<IndexEntry>(&json).ok()) {
            index.insert(id, entry);
        }
    }
    let listing = (version == Some(crate::list_cache::VERSION)).then(|| ListCache {
        version: crate::list_cache::VERSION,
        source: meta_get(conn, "listing_source").unwrap_or_default(),
        full_at_ms: meta_get(conn, "listing_full_at").and_then(|v| v.parse().ok()).unwrap_or(0),
        sessions,
    });
    Ok((listing, index))
}

/// The one-time import of the old cache files next to the database (they
/// were caches: imported here, then removed).
pub(crate) fn import_old_files(conn: &mut Connection, state_dir: &std::path::Path) -> rusqlite::Result<()> {
    let list_file = state_dir.join("session-list-cache.json");
    if let Some(listing) = crate::list_cache::load::<ListedRow>(&list_file) {
        save_listing(conn, &listing)?;
    }
    let index_file = state_dir.join("index-cache.json");
    let index = crate::index::load_cache(&index_file);
    if !index.is_empty() {
        save_index(conn, &index)?;
    }
    let _ = std::fs::remove_file(list_file);
    let _ = std::fs::remove_file(index_file);
    Ok(())
}
