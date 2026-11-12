//! R4 Chrome import (board R4Daily 2a–2d): bookmarks and history from a
//! Chrome profile on this Mac into `browser.db`.
//!
//! Chrome is only ever read: its `Bookmarks` JSON is read in place and its
//! `History` SQLite is copied to a temp file first (Chrome holds a lock on it
//! while it runs) and opened read-only. Every row an import writes carries
//! the import's `source`, so Undo removes exactly that import and nothing of
//! the person's own. Passwords are not imported: CEF's own password store is
//! not something Baaz can write to (owner 11 Nov: password import was a spike;
//! the sheet leaves the row out). Cookies stay Chrome's.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OpenFlags};
use serde_json::Value;

use super::bookmarks;

/// History older than this is not imported (Baaz keeps 90 days).
const HISTORY_DAYS: u64 = super::history::KEEP_DAYS;
/// Chrome's timestamps: microseconds since 1601-01-01.
const WEBKIT_EPOCH_OFFSET_US: u64 = 11_644_473_600_000_000;

/// One Chrome profile found on this Mac.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChromeProfile {
    /// The profile folder (`Default`, `Profile 1`, …).
    pub dir: PathBuf,
    /// The name Chrome shows for it.
    pub name: String,
}

/// What a profile holds, counted before importing (the sheet's numbers).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub bookmarks: u64,
    pub history: u64,
}

/// What an import wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Imported {
    /// The batch id rows carry (`import-<ms>`): Undo's key.
    pub source: String,
    pub bookmarks: u64,
    pub history: u64,
}

/// Chrome's data folder: `~/Library/Application Support/Google/Chrome`.
pub(crate) fn chrome_root() -> Option<PathBuf> {
    // A capture or test never reads the person's Chrome: only a root it
    // names itself (`BAAZ_CHROME_ROOT`, a synthetic profile).
    if let Some(root) = std::env::var_os("BAAZ_CHROME_ROOT") {
        let root = PathBuf::from(root);
        return root.is_dir().then_some(root);
    }
    if crate::clock::deterministic() || cfg!(test) {
        return None;
    }
    let home = std::env::var_os("HOME")?;
    let root = PathBuf::from(home).join("Library/Application Support/Google/Chrome");
    root.is_dir().then_some(root)
}

/// The profiles under `root`, from `Local State` (names as Chrome shows
/// them), falling back to the folders that have a `Bookmarks` or `History`.
pub(crate) fn profiles(root: &Path) -> Vec<ChromeProfile> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string(root.join("Local State")) {
        if let Ok(state) = serde_json::from_str::<Value>(&text) {
            if let Some(cache) = state.pointer("/profile/info_cache").and_then(Value::as_object) {
                for (dir, info) in cache {
                    let path = root.join(dir);
                    if path.is_dir() {
                        let name = info.get("name").and_then(Value::as_str).unwrap_or(dir).to_owned();
                        out.push(ChromeProfile { dir: path, name });
                    }
                }
            }
        }
    }
    if out.is_empty() {
        for dir in ["Default", "Profile 1", "Profile 2", "Profile 3"] {
            let path = root.join(dir);
            if BOOKMARK_FILES.iter().any(|name| path.join(name).is_file()) || path.join("History").is_file() {
                out.push(ChromeProfile { dir: path, name: dir.to_owned() });
            }
        }
    }
    // Default first, then by name.
    out.sort_by(|a, b| {
        let da = a.dir.file_name().is_some_and(|f| f == "Default");
        let db = b.dir.file_name().is_some_and(|f| f == "Default");
        db.cmp(&da).then(a.name.cmp(&b.name))
    });
    out
}

/// One node of Chrome's bookmark tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Url { name: String, url: String, added_ms: u64 },
    Folder { name: String, children: Vec<Node> },
}

fn webkit_to_ms(value: &Value) -> u64 {
    let us: u64 = match value {
        Value::String(s) => s.parse().unwrap_or(0),
        Value::Number(n) => n.as_u64().unwrap_or(0),
        _ => 0,
    };
    us.saturating_sub(WEBKIT_EPOCH_OFFSET_US) / 1000
}

fn parse_node(value: &Value) -> Option<Node> {
    let name = value.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
    match value.get("type").and_then(Value::as_str)? {
        "url" => {
            let url = value.get("url").and_then(Value::as_str)?.to_owned();
            let added_ms = value.get("date_added").map(webkit_to_ms).unwrap_or(0);
            Some(Node::Url { name, url, added_ms })
        }
        "folder" => {
            let children = value.get("children").and_then(Value::as_array).map(|c| c.iter().filter_map(parse_node).collect()).unwrap_or_default();
            Some(Node::Folder { name, children })
        }
        _ => None,
    }
}

/// The profile's bookmark roots as folders: Bookmarks bar, Other, Mobile
/// (an empty root is left out).
/// The files a profile keeps bookmarks in, same JSON shape: `Bookmarks` (on
/// this Mac only) and `AccountBookmarks` (a signed-in account's, which recent
/// Chrome keeps apart — the owner's 4,257 bookmarks were all there and
/// `Bookmarks` did not exist, so the import found none, 12 Nov 2026).
const BOOKMARK_FILES: [&str; 2] = ["Bookmarks", "AccountBookmarks"];

pub(crate) fn read_bookmarks(profile: &Path) -> Vec<Node> {
    let files: Vec<Value> = BOOKMARK_FILES
        .iter()
        .filter_map(|name| std::fs::read_to_string(profile.join(name)).ok())
        .filter_map(|text| serde_json::from_str::<Value>(&text).ok())
        .collect();
    let mut out = Vec::new();
    for (key, label) in [("bookmark_bar", "Bookmarks bar"), ("other", "Other"), ("synced", "Mobile")] {
        // One folder per root, both files' children in it; a URL in both
        // lands once (the store never adds a duplicate).
        let children: Vec<Node> = files
            .iter()
            .filter_map(|json| json.pointer(&format!("/roots/{key}")))
            .filter_map(parse_node)
            .flat_map(|node| match node {
                Node::Folder { children, .. } => children,
                Node::Url { .. } => Vec::new(),
            })
            .collect();
        if !children.is_empty() {
            out.push(Node::Folder { name: label.to_owned(), children });
        }
    }
    out
}

fn count_urls(nodes: &[Node]) -> u64 {
    nodes
        .iter()
        .map(|n| match n {
            Node::Url { url, .. } => u64::from(super::history::recordable(url)),
            Node::Folder { children, .. } => count_urls(children),
        })
        .sum()
}

/// A read-only copy of the profile's `History` (Chrome locks the original).
fn history_copy(profile: &Path) -> Option<(tempfile_path::TempPath, Connection)> {
    let source = profile.join("History");
    if !source.is_file() {
        return None;
    }
    let copy = tempfile_path::TempPath::new("baaz-chrome-history")?;
    std::fs::copy(&source, copy.path()).ok()?;
    let conn = Connection::open_with_flags(copy.path(), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    Some((copy, conn))
}

/// History rows from the last [`HISTORY_DAYS`]: `(url, title, last_ms, visits)`.
fn read_history(conn: &Connection, now_ms: u64) -> Vec<(String, String, u64, u64)> {
    let cutoff_us = (now_ms.saturating_sub(HISTORY_DAYS * 86_400_000)) * 1000 + WEBKIT_EPOCH_OFFSET_US;
    let Ok(mut stmt) = conn.prepare(
        "SELECT url, title, last_visit_time, visit_count FROM urls
         WHERE last_visit_time >= ?1 AND hidden = 0 ORDER BY last_visit_time DESC",
    ) else {
        return Vec::new();
    };
    stmt.query_map(params![cutoff_us as i64], |row| {
        let url: String = row.get(0)?;
        let title: String = row.get::<_, Option<String>>(1)?.unwrap_or_default();
        let last: i64 = row.get(2)?;
        let visits: i64 = row.get(3)?;
        Ok((url, title, (last.max(0) as u64).saturating_sub(WEBKIT_EPOCH_OFFSET_US) / 1000, visits.max(0) as u64))
    })
    .map(|rows| rows.flatten().filter(|(url, ..)| super::history::recordable(url)).collect())
    .unwrap_or_default()
}

/// The sheet's numbers for `profile`.
pub(crate) fn count(profile: &Path, now_ms: u64) -> Counts {
    let bookmarks = count_urls(&read_bookmarks(profile));
    let history = history_copy(profile).map(|(_copy, conn)| read_history(&conn, now_ms).len() as u64).unwrap_or(0);
    Counts { bookmarks, history }
}

fn import_nodes(conn: &Connection, nodes: &[Node], parent: i64, source: &str) -> u64 {
    let mut n = 0;
    for node in nodes {
        match node {
            Node::Url { name, url, added_ms } => {
                if super::history::recordable(url) && bookmarks::add_from(conn, url, name, parent, *added_ms, source).is_some() {
                    n += 1;
                }
            }
            Node::Folder { name, children } => {
                if let Some(id) = bookmarks::add_folder(conn, name, Some(parent), source) {
                    n += import_nodes(conn, children, id, source);
                }
            }
        }
    }
    n
}

/// Import `profile` into `db`: bookmarks into a new "Imported from Chrome"
/// folder (when `bookmarks`), history pages not already known (when
/// `history`, never a site the person excluded). `progress(done, total)` is
/// called as history lands. Runs in one transaction per part.
pub(crate) fn import(
    db: &super::history::BrowserDb,
    profile: &Path,
    want_bookmarks: bool,
    want_history: bool,
    now_ms: u64,
    mut progress: impl FnMut(u64, u64),
) -> Imported {
    let conn = db.conn();
    let source = format!("import-{now_ms}");
    let mut out = Imported { source: source.clone(), ..Default::default() };
    if want_bookmarks {
        let nodes = read_bookmarks(profile);
        if !nodes.is_empty() {
            let _ = conn.execute_batch("BEGIN");
            if let Some(root) = bookmarks::add_folder(conn, "Imported from Chrome", None, &source) {
                out.bookmarks = import_nodes(conn, &nodes, root, &source);
            }
            let _ = conn.execute_batch("COMMIT");
        }
    }
    if want_history {
        if let Some((_copy, chrome)) = history_copy(profile) {
            let rows = read_history(&chrome, now_ms);
            let total = rows.len() as u64;
            let excluded = db.excluded();
            for (i, chunk) in rows.chunks(500).enumerate() {
                let _ = conn.execute_batch("BEGIN");
                for (url, title, last_ms, visits) in chunk {
                    let host = super::history::host_of(url);
                    if excluded.iter().any(|site| host == *site || host.ends_with(&format!(".{site}"))) {
                        continue;
                    }
                    let added = conn
                        .execute(
                            "INSERT OR IGNORE INTO visits(url, title, host, first_ms, last_ms, count, source) VALUES (?1, ?2, ?3, ?4, ?4, ?5, ?6)",
                            params![url, title, host, *last_ms as i64, (*visits).max(1) as i64, source],
                        )
                        .unwrap_or(0);
                    out.history += added as u64;
                }
                let _ = conn.execute_batch("COMMIT");
                progress(((i + 1) * 500).min(rows.len()) as u64, total);
            }
        }
    }
    out
}

/// Undo one import: its bookmarks, folders and history rows go; the
/// person's own never do.
pub(crate) fn undo(db: &super::history::BrowserDb, source: &str) {
    if !source.starts_with("import-") {
        return;
    }
    let conn = db.conn();
    bookmarks::remove_source(conn, source);
    let _ = conn.execute("DELETE FROM pages_fts WHERE url IN (SELECT url FROM visits WHERE source = ?1)", params![source]);
    let _ = conn.execute("DELETE FROM visits WHERE source = ?1", params![source]);
}

/// A temp file path removed on drop (the History copy).
mod tempfile_path {
    use std::path::{Path, PathBuf};

    pub(crate) struct TempPath(PathBuf);

    impl TempPath {
        pub(crate) fn new(stem: &str) -> Option<Self> {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
            Some(Self(std::env::temp_dir().join(format!("{stem}-{}-{nanos}.db", std::process::id()))))
        }

        pub(crate) fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;

    fn webkit(ms: u64) -> u64 {
        ms * 1000 + WEBKIT_EPOCH_OFFSET_US
    }

    /// A throwaway Chrome profile (never the owner's): Bookmarks + History.
    fn fake_profile() -> PathBuf {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("baaz-fake-chrome-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Default")).unwrap();
        let bookmarks = serde_json::json!({
            "roots": {
                "bookmark_bar": { "type": "folder", "name": "Bookmarks bar", "children": [
                    { "type": "url", "name": "Rust", "url": "https://www.rust-lang.org/", "date_added": webkit(NOW - 1000).to_string() },
                    { "type": "folder", "name": "Work", "children": [
                        { "type": "url", "name": "Linear", "url": "https://linear.app/", "date_added": "0" },
                        { "type": "url", "name": "JS", "url": "javascript:alert(1)", "date_added": "0" }
                    ]}
                ]},
                "other": { "type": "folder", "name": "Other bookmarks", "children": [
                    { "type": "url", "name": "HN", "url": "https://news.ycombinator.com/", "date_added": "0" }
                ]},
                "synced": { "type": "folder", "name": "Mobile bookmarks", "children": [] }
            }
        });
        std::fs::write(dir.join("Default/Bookmarks"), bookmarks.to_string()).unwrap();
        let history = Connection::open(dir.join("Default/History")).unwrap();
        history
            .execute_batch("CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT, visit_count INTEGER, typed_count INTEGER, last_visit_time INTEGER, hidden INTEGER DEFAULT 0);")
            .unwrap();
        for (url, title, ms) in [
            ("https://docs.rs/serde", "serde - Rust", NOW - 86_400_000),
            ("https://bank.example/login", "Bank", NOW - 1000),
            ("https://old.example/", "Old", NOW - 200 * 86_400_000),
            ("chrome://settings", "Settings", NOW),
        ] {
            history.execute("INSERT INTO urls(url, title, visit_count, typed_count, last_visit_time) VALUES (?1, ?2, 3, 0, ?3)", params![url, title, webkit(ms) as i64]).unwrap();
        }
        std::fs::write(dir.join("Local State"), r#"{"profile":{"info_cache":{"Default":{"name":"Person 1"}}}}"#).unwrap();
        dir
    }

    #[test]
    fn profiles_come_from_local_state_with_their_names() {
        let root = fake_profile();
        let found = profiles(&root);
        assert_eq!(found, vec![ChromeProfile { dir: root.join("Default"), name: "Person 1".into() }]);
    }

    /// Recent Chrome keeps a signed-in account's bookmarks in
    /// `AccountBookmarks`; the owner's profile had no `Bookmarks` at all.
    #[test]
    fn account_bookmarks_are_read_alone_and_beside_local_ones() {
        let root = fake_profile();
        let profile = root.join("Default");
        let local = std::fs::read_to_string(profile.join("Bookmarks")).unwrap();
        let before = count_urls(&read_bookmarks(&profile));
        // Only the account file.
        std::fs::rename(profile.join("Bookmarks"), profile.join("AccountBookmarks")).unwrap();
        assert_eq!(count_urls(&read_bookmarks(&profile)), before, "AccountBookmarks alone is read");
        assert!(profiles(&root).iter().any(|p| p.dir.ends_with("Default")), "a profile with only account bookmarks counts");
        // Both: each root's children from both files, in one folder each.
        std::fs::write(profile.join("Bookmarks"), &local).unwrap();
        let both = read_bookmarks(&profile);
        assert_eq!(count_urls(&both), before * 2, "both files read (the store drops repeats)");
        let names: Vec<_> = both.iter().map(|n| match n { Node::Folder { name, .. } => name.clone(), Node::Url { .. } => String::new() }).collect();
        assert_eq!(names.iter().filter(|n| *n == "Bookmarks bar").count(), 1, "one bar, not two");
    }

    #[test]
    fn counts_skip_non_web_and_old_pages() {
        let root = fake_profile();
        let counts = count(&root.join("Default"), NOW);
        assert_eq!(counts, Counts { bookmarks: 3, history: 2 });
    }

    #[test]
    fn import_then_undo_touches_only_its_own_rows() {
        let root = fake_profile();
        let db = super::super::history::BrowserDb::memory();
        db.record_visit("https://docs.rs/serde", "mine", NOW);
        db.exclude("bank.example");
        bookmarks::add(db.conn(), "https://mine.example/", "Mine", bookmarks::BAR, NOW);
        let mut seen = Vec::new();
        let done = import(&db, &root.join("Default"), true, true, NOW, |a, b| seen.push((a, b)));
        assert_eq!(done.bookmarks, 3);
        assert_eq!(done.history, 0, "docs.rs was already known; bank.example is excluded");
        assert_eq!(seen, vec![(2, 2)], "progress counts every row read, kept or skipped");
        let all = bookmarks::folders(db.conn());
        let work = all.iter().find(|f| f.name == "Work").expect("the tree is kept");
        assert_eq!(bookmarks::folder_path(&all, work.id), "Imported from Chrome › Bookmarks bar › Work");
        assert_eq!(bookmarks::count(db.conn()), 4);
        undo(&db, &done.source);
        assert_eq!(bookmarks::count(db.conn()), 1, "only the person's own bookmark is left");
        assert_eq!(db.search("serde", 5).len(), 1, "the person's own history stays");
        assert!(bookmarks::folders(db.conn()).iter().all(|f| f.name != "Imported from Chrome"));
    }

    #[test]
    fn history_pages_import_with_their_titles_and_undo_removes_them() {
        let root = fake_profile();
        let db = super::super::history::BrowserDb::memory();
        let done = import(&db, &root.join("Default"), false, true, NOW, |_, _| {});
        assert_eq!(done.history, 2);
        assert_eq!(db.search("serde", 5)[0].title, "serde - Rust");
        undo(&db, &done.source);
        assert_eq!(db.len(), 0);
    }
}
