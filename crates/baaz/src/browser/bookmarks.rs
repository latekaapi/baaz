//! R4 bookmarks (board R4Daily 1a–1c), in `browser.db` beside history.
//!
//! Two built-in folders always exist, "Bookmarks bar" and "Other" (Chrome's
//! names, so an import lands where the person expects); an import adds its
//! own folder tree under "Imported from Chrome". Nothing draws a bookmarks
//! bar: bookmarks are found in the Field and browsed in its scoped view.
//!
//! Every row carries a `source`: `baaz` for the person's own, or an import
//! batch id, so Undo removes exactly what one import wrote.

// The import (R4 step 7) and its Undo use the rest of this API.
#![allow(dead_code)]

use rusqlite::{params, Connection, OptionalExtension};

/// The built-in folders' ids.
pub(crate) const BAR: i64 = 1;
pub(crate) const OTHER: i64 = 2;

/// A bookmark.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub folder: i64,
    pub added_ms: u64,
}

/// A folder, with how many bookmarks and folders sit directly in it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Folder {
    pub id: i64,
    pub name: String,
    pub parent: Option<i64>,
    pub count: u64,
}

/// Create the tables and the two built-in folders. Idempotent.
pub(crate) fn ensure_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS bm_folders(
             id INTEGER PRIMARY KEY,
             name TEXT NOT NULL,
             parent INTEGER,
             position INTEGER NOT NULL DEFAULT 0,
             source TEXT NOT NULL DEFAULT 'baaz');
         CREATE TABLE IF NOT EXISTS bookmarks(
             id INTEGER PRIMARY KEY,
             url TEXT NOT NULL,
             title TEXT NOT NULL DEFAULT '',
             folder INTEGER NOT NULL,
             added_ms INTEGER NOT NULL,
             position INTEGER NOT NULL DEFAULT 0,
             source TEXT NOT NULL DEFAULT 'baaz');
         CREATE INDEX IF NOT EXISTS bookmarks_url ON bookmarks(url);
         CREATE INDEX IF NOT EXISTS bookmarks_folder ON bookmarks(folder);
         INSERT OR IGNORE INTO bm_folders(id, name, parent, position) VALUES (1, 'Bookmarks bar', NULL, 0);
         INSERT OR IGNORE INTO bm_folders(id, name, parent, position) VALUES (2, 'Other', NULL, 1);",
    )
}

fn bookmark_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Bookmark> {
    Ok(Bookmark {
        id: row.get(0)?,
        url: row.get(1)?,
        title: row.get(2)?,
        folder: row.get(3)?,
        added_ms: row.get::<_, i64>(4)? as u64,
    })
}

const COLS: &str = "id, url, title, folder, added_ms";

/// The person's bookmark for `url`, if any (the newest when several).
pub(crate) fn find(conn: &Connection, url: &str) -> Option<Bookmark> {
    conn.query_row(
        &format!("SELECT {COLS} FROM bookmarks WHERE url = ?1 ORDER BY added_ms DESC LIMIT 1"),
        params![url],
        bookmark_row,
    )
    .optional()
    .ok()
    .flatten()
}

/// Bookmark `url` into `folder` (⌘D: the Bookmarks bar). An already
/// bookmarked page is returned as it is, never duplicated.
pub(crate) fn add(conn: &Connection, url: &str, title: &str, folder: i64, now: u64) -> Option<Bookmark> {
    if let Some(existing) = find(conn, url) {
        return Some(existing);
    }
    add_from(conn, url, title, folder, now, "baaz")
}

/// Insert a bookmark from `source` (an import batch keeps duplicates out
/// itself, by folder).
pub(crate) fn add_from(conn: &Connection, url: &str, title: &str, folder: i64, now: u64, source: &str) -> Option<Bookmark> {
    let position: i64 = conn
        .query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM bookmarks WHERE folder = ?1", params![folder], |r| r.get(0))
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO bookmarks(url, title, folder, added_ms, position, source) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![url, title, folder, now as i64, position, source],
    )
    .ok()?;
    let id = conn.last_insert_rowid();
    Some(Bookmark { id, url: url.to_owned(), title: title.to_owned(), folder, added_ms: now })
}

/// Rename a bookmark and move it to `folder`.
pub(crate) fn update(conn: &Connection, id: i64, title: &str, folder: i64) {
    let _ = conn.execute("UPDATE bookmarks SET title = ?2, folder = ?3 WHERE id = ?1", params![id, title, folder]);
}

/// Remove one bookmark.
pub(crate) fn remove(conn: &Connection, id: i64) {
    let _ = conn.execute("DELETE FROM bookmarks WHERE id = ?1", params![id]);
}

/// Every folder, in tree order (parents before children, by position).
pub(crate) fn folders(conn: &Connection) -> Vec<Folder> {
    let Ok(mut stmt) = conn.prepare_cached(
        "SELECT f.id, f.name, f.parent,
                (SELECT count(*) FROM bookmarks b WHERE b.folder = f.id)
              + (SELECT count(*) FROM bm_folders c WHERE c.parent = f.id)
         FROM bm_folders f ORDER BY COALESCE(f.parent, 0), f.position, f.id",
    ) else {
        return Vec::new();
    };
    stmt.query_map([], |row| {
        Ok(Folder { id: row.get(0)?, name: row.get(1)?, parent: row.get(2)?, count: row.get::<_, i64>(3)? as u64 })
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// The folders directly inside `parent` (`None`: the top level).
pub(crate) fn subfolders(conn: &Connection, parent: Option<i64>) -> Vec<Folder> {
    folders(conn).into_iter().filter(|folder| folder.parent == parent).collect()
}

/// A folder's path for a row's hint: `Other › Learning`.
pub(crate) fn folder_path(all: &[Folder], id: i64) -> String {
    let mut names = Vec::new();
    let mut at = Some(id);
    let mut guard = 0;
    while let Some(current) = at {
        let Some(folder) = all.iter().find(|f| f.id == current) else { break };
        names.push(folder.name.clone());
        at = folder.parent;
        guard += 1;
        if guard > 32 {
            break;
        }
    }
    names.reverse();
    names.join(" › ")
}

/// Make a folder (an import's tree, or the person's own).
pub(crate) fn add_folder(conn: &Connection, name: &str, parent: Option<i64>, source: &str) -> Option<i64> {
    let position: i64 = conn
        .query_row(
            "SELECT COALESCE(MAX(position), -1) + 1 FROM bm_folders WHERE parent IS ?1",
            params![parent],
            |r| r.get(0),
        )
        .unwrap_or(0);
    conn.execute(
        "INSERT INTO bm_folders(name, parent, position, source) VALUES (?1, ?2, ?3, ?4)",
        params![name, parent, position, source],
    )
    .ok()?;
    Some(conn.last_insert_rowid())
}

/// The bookmarks directly in `folder`, by position.
pub(crate) fn in_folder(conn: &Connection, folder: i64) -> Vec<Bookmark> {
    let Ok(mut stmt) = conn.prepare_cached(&format!("SELECT {COLS} FROM bookmarks WHERE folder = ?1 ORDER BY position, id")) else {
        return Vec::new();
    };
    stmt.query_map(params![folder], bookmark_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// The newest bookmarks.
pub(crate) fn recent(conn: &Connection, limit: usize) -> Vec<Bookmark> {
    let Ok(mut stmt) = conn.prepare_cached(&format!("SELECT {COLS} FROM bookmarks ORDER BY added_ms DESC, id DESC LIMIT ?1")) else {
        return Vec::new();
    };
    stmt.query_map(params![limit as i64], bookmark_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// Bookmarks whose title or address contains `query`, newest first.
pub(crate) fn search(conn: &Connection, query: &str, limit: usize) -> Vec<Bookmark> {
    let q = query.trim();
    if q.is_empty() {
        return Vec::new();
    }
    let like = format!("%{}%", q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
    let Ok(mut stmt) = conn.prepare_cached(&format!(
        "SELECT {COLS} FROM bookmarks WHERE title LIKE ?1 ESCAPE '\\' OR url LIKE ?1 ESCAPE '\\'
         ORDER BY added_ms DESC LIMIT ?2"
    )) else {
        return Vec::new();
    };
    stmt.query_map(params![like, limit as i64], bookmark_row).map(|rows| rows.flatten().collect()).unwrap_or_default()
}

/// How many bookmarks there are.
pub(crate) fn count(conn: &Connection) -> u64 {
    conn.query_row("SELECT count(*) FROM bookmarks", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as u64
}

/// Undo one import: its bookmarks and folders go, nothing else.
pub(crate) fn remove_source(conn: &Connection, source: &str) {
    if source == "baaz" {
        return;
    }
    let _ = conn.execute("DELETE FROM bookmarks WHERE source = ?1", params![source]);
    let _ = conn.execute("DELETE FROM bm_folders WHERE source = ?1", params![source]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        ensure_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn the_two_built_in_folders_exist_once() {
        let conn = db();
        ensure_schema(&conn).unwrap();
        let names: Vec<String> = folders(&conn).into_iter().map(|f| f.name).collect();
        assert_eq!(names, vec!["Bookmarks bar", "Other"]);
    }

    #[test]
    fn bookmarking_twice_keeps_one() {
        let conn = db();
        let a = add(&conn, "https://rust-lang.org/learn", "Learn Rust", BAR, 1).unwrap();
        let b = add(&conn, "https://rust-lang.org/learn", "Learn Rust again", OTHER, 2).unwrap();
        assert_eq!(a, b);
        assert_eq!(count(&conn), 1);
        assert_eq!(find(&conn, "https://rust-lang.org/learn").unwrap().title, "Learn Rust");
    }

    #[test]
    fn edit_move_and_remove() {
        let conn = db();
        let a = add(&conn, "https://a.example/", "A", BAR, 1).unwrap();
        update(&conn, a.id, "Renamed", OTHER);
        let found = find(&conn, "https://a.example/").unwrap();
        assert_eq!((found.title.as_str(), found.folder), ("Renamed", OTHER));
        assert!(in_folder(&conn, BAR).is_empty());
        remove(&conn, a.id);
        assert!(find(&conn, "https://a.example/").is_none());
    }

    #[test]
    fn search_paths_and_counts() {
        let conn = db();
        let learning = add_folder(&conn, "Learning", Some(OTHER), "baaz").unwrap();
        add(&conn, "https://doc.rust-lang.org/book", "The Rust Programming Language", BAR, 1);
        add(&conn, "https://doc.rust-lang.org/rust-by-example", "Rust by Example", learning, 2);
        add(&conn, "https://news.ycombinator.com/", "Hacker News", OTHER, 3);
        let hits = search(&conn, "rust", 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Rust by Example", "newest first");
        let all = folders(&conn);
        assert_eq!(folder_path(&all, learning), "Other › Learning");
        let other = all.iter().find(|f| f.id == OTHER).unwrap();
        assert_eq!(other.count, 2, "one bookmark and one folder");
        assert!(search(&conn, "%", 10).is_empty(), "% is literal");
        assert_eq!(recent(&conn, 1)[0].title, "Hacker News");
        assert_eq!(subfolders(&conn, Some(OTHER)).len(), 1);
    }

    #[test]
    fn undo_an_import_removes_only_what_it_wrote() {
        let conn = db();
        add(&conn, "https://mine.example/", "Mine", BAR, 1);
        let root = add_folder(&conn, "Imported from Chrome", None, "import-1").unwrap();
        add_from(&conn, "https://chrome.example/", "From Chrome", root, 2, "import-1");
        assert_eq!(count(&conn), 2);
        remove_source(&conn, "import-1");
        assert_eq!(count(&conn), 1);
        assert!(folders(&conn).iter().all(|f| f.name != "Imported from Chrome"));
        remove_source(&conn, "baaz");
        assert_eq!(count(&conn), 1, "the person's own never go with an undo");
    }
}
