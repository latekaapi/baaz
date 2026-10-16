//! The local session index, read-only (spec §3.7).
//!
//! `session/list` gives identity and timestamps and nothing a person can read:
//! no title, no first prompt. Muse keeps those in
//! `~/.local/share/muse/session-index.db`, which Baaz opens **read-only**
//! and treats as a cache — a missing file, a schema this build has never seen
//! and a database another process has locked are all ordinary, and every one of
//! them yields an empty map rather than an error the person has to see.
//!
//! Nothing here ever writes. Rename and hide land in Baaz's own
//! `sessions.json` in a later phase; the muse index stays Muse's.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};

/// How long to wait for another process to let go of the database before
/// giving up and rendering the sidebar without titles.
const BUSY_TIMEOUT: Duration = Duration::from_millis(250);

/// What the index knows about one session, beyond what the wire says.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct IndexEntry {
    /// The session's own name, when someone gave it one.
    pub session_name: Option<String>,
    /// The generated title.
    pub title: String,
    /// The first thing the person typed, which is the best fallback label.
    pub first_user_prompt: Option<String>,
    /// Transcript text to index inline. [`read`] leaves it empty: Muse's
    /// `search_text` averages 80 KB a row (210 MB over 2.6k sessions,
    /// measured 2026-10-16), so the sidebar's read never carries it and the
    /// search sync fetches bodies per changed session with [`read_bodies`].
    /// Fixtures that never touch the database set it directly.
    #[serde(skip)]
    pub search_text: String,
    /// Last activity, in epoch microseconds.
    pub updated_at_us: Option<i64>,
    /// The workspace the session ran in, when the index records one.
    pub workspace_root: Option<String>,
}

impl IndexEntry {
    /// The label the sidebar should show, best first.
    ///
    /// Muse writes the literal string `"New session"` into `title` for a
    /// session it could not name — which is a placeholder wearing a title's
    /// clothes, and the reason fourteen rows can read the same thing at
    /// once. It is treated as no title at all, so the caller goes on to the
    /// next fact it has.
    pub fn label(&self) -> Option<&str> {
        fn pick(value: &Option<String>) -> Option<&str> {
            value.as_deref().map(str::trim).filter(|s| !s.is_empty())
        }
        let title = Some(self.title.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| !s.eq_ignore_ascii_case(crate::sidebar::UNNAMED));
        pick(&self.session_name).or(title).or_else(|| pick(&self.first_user_prompt))
    }
}

/// `~/.local/share/muse/session-index.db`, honouring `XDG_DATA_HOME`.
pub fn index_path() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("muse").join("session-index.db")
}

/// Read every row the index has, keyed by session id.
///
/// Blocking (it touches the disk), so call it off the UI thread. Returns an
/// empty map for every failure mode; the sidebar degrades to wire-only labels.
pub fn read() -> HashMap<String, IndexEntry> {
    read_at(&index_path())
}

/// [`read`] against an explicit path, which is what the test uses.
///
/// Every failure mode degrades to an empty map (the sidebar falls back to
/// wire-only labels) rather than an error the person has to see — but a
/// silent one is indistinguishable from "Muse never wrote an index yet",
/// which is the common, harmless case. A schema drift or a locked database
/// are not: they mean titles are missing for a session that has them, so
/// each of the three failure points logs a one-line warning naming which it
/// was (finding `support-5`).
pub fn read_at(path: &std::path::Path) -> HashMap<String, IndexEntry> {
    let Ok(connection) = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        // A missing file is the ordinary case (no index has ever been
        // written); anything else — permissions, a lock SQLite could not
        // clear within `BUSY_TIMEOUT`, a corrupt file — is worth a line.
        if path.exists() {
            crate::baaz_log!(
                "session index at {} could not be opened (locked or unreadable); sidebar titles will be blank",
                path.display()
            );
        }
        return HashMap::new();
    };
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    // `workspace_root` is additive evolution: an older Muse index has no such
    // column, and selecting it unconditionally would turn every title blank
    // on a schema-drift error. Ask what the table holds first and select the
    // column only when it is there, so an older index still yields titles.
    // A table this build never saw at all still fails at the second prepare
    // and yields the empty map, exactly as before.
    let has_workspace: bool = connection
        .prepare("PRAGMA table_info(sessions)")
        .and_then(|mut pragma| {
            pragma
                .query_map([], |row| row.get::<_, String>(1))
                .map(|names| names.filter_map(Result::ok).any(|name| name == "workspace_root"))
        })
        .unwrap_or(false);
    let sql = if has_workspace {
        "SELECT session_id, session_name, substr(title, 1, 1000), substr(first_user_prompt, 1, 1000), updated_at_us, workspace_root FROM sessions"
    } else {
        "SELECT session_id, session_name, substr(title, 1, 1000), substr(first_user_prompt, 1, 1000), updated_at_us FROM sessions"
    };
    let mut statement = match connection.prepare(sql) {
        Ok(statement) => statement,
        Err(error) => {
            // rusqlite's own message already says "no such table" or "no
            // such column", which is exactly the table-vs-column distinction
            // the finding asks for — passed through rather than re-guessed.
            crate::baaz_log!(
                "session index schema drift ({error}); sidebar titles will be blank"
            );
            return HashMap::new();
        }
    };
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            IndexEntry {
                session_name: row.get(1).ok(),
                title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                first_user_prompt: row.get(3).ok(),
                search_text: String::new(),
                updated_at_us: row.get(4).ok(),
                workspace_root: if has_workspace { row.get(5).ok() } else { None },
            },
        ))
    });
    let rows = match rows {
        Ok(rows) => rows,
        Err(error) => {
            crate::baaz_log!("session index query failed ({error}); sidebar titles will be blank");
            return HashMap::new();
        }
    };
    rows.filter_map(Result::ok).collect()
}

/// [`read_at`] without a table scan: the rows whose `updated_at_us` differs
/// from `previous` (or that `previous` lacks) are read by key, the rest are
/// carried over, and rows Muse no longer has drop out.
///
/// Why it exists: Muse's `sessions` row stores `search_text` (80 KB on
/// average) before `updated_at_us` and `session_name`, so any scan of the
/// table walks every row's overflow pages — 3.9 s cold, 0.27 s warm on a
/// 700 MB index (measured 2026-10-16), and it ran at boot and after every
/// finished turn. The stamps come from `idx_sessions_created`, which covers
/// `(updated_at_us, session_id)`, so the scan touches the index alone.
///
/// `title` and `first_user_prompt` are cut to their first 1,000 characters,
/// here and in [`read_at`]: a pasted prompt can be 128 KB, the two columns
/// held 105 MB each across 2.6k sessions, and a label is one line. Their
/// words stay searchable through `search_text`.
///
/// A change Muse makes without bumping `updated_at_us` is missed until the
/// next full [`read`]; the app runs one in the background after boot.
pub fn read_changed(path: &std::path::Path, previous: &HashMap<String, IndexEntry>) -> HashMap<String, IndexEntry> {
    let Ok(connection) = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return HashMap::new();
    };
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    let stamps: Vec<(String, Option<i64>)> = match connection.prepare("SELECT session_id, updated_at_us FROM sessions") {
        Ok(mut statement) => match statement.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get(1).ok()))) {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(_) => return read_at(path),
        },
        // An index this build has never seen: the full read knows how to
        // degrade.
        Err(_) => return read_at(path),
    };
    let mut out = HashMap::with_capacity(stamps.len());
    let mut need: Vec<String> = Vec::new();
    for (id, updated) in stamps {
        match previous.get(&id) {
            Some(entry) if entry.updated_at_us == updated && updated.is_some() => {
                out.insert(id, entry.clone());
            }
            _ => need.push(id),
        }
    }
    if need.is_empty() {
        return out;
    }
    let has_workspace = has_column(&connection, "workspace_root");
    let columns = if has_workspace {
        "session_id, session_name, substr(title, 1, 1000), substr(first_user_prompt, 1, 1000), updated_at_us, workspace_root"
    } else {
        "session_id, session_name, substr(title, 1, 1000), substr(first_user_prompt, 1, 1000), updated_at_us"
    };
    for chunk in need.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("SELECT {columns} FROM sessions WHERE session_id IN ({marks})");
        let Ok(mut statement) = connection.prepare(&sql) else { return read_at(path) };
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                IndexEntry {
                    session_name: row.get(1).ok(),
                    title: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    first_user_prompt: row.get(3).ok(),
                    search_text: String::new(),
                    updated_at_us: row.get(4).ok(),
                    workspace_root: if has_workspace { row.get(5).ok() } else { None },
                },
            ))
        });
        if let Ok(rows) = rows {
            out.extend(rows.filter_map(Result::ok));
        }
    }
    out
}

/// Whether Muse's `sessions` table has `column`.
fn has_column(connection: &Connection, column: &str) -> bool {
    connection
        .prepare("PRAGMA table_info(sessions)")
        .and_then(|mut pragma| {
            pragma
                .query_map([], |row| row.get::<_, String>(1))
                .map(|names| names.filter_map(Result::ok).any(|name| name == column))
        })
        .unwrap_or(false)
}

/// Where Baaz keeps its last read of the index: labels and stamps only, so
/// the sidebar can paint from it before Muse's database is touched.
pub fn cache_path() -> PathBuf {
    crate::store::support_dir().join("index-cache.json")
}

/// The last saved read, or an empty map (no file yet, or one this build
/// cannot parse — the next read rewrites it).
pub fn load_cache(path: &std::path::Path) -> HashMap<String, IndexEntry> {
    std::fs::read(path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Save a read for the next boot. Blocking; call it off the UI thread.
pub fn save_cache(path: &std::path::Path, index: &HashMap<String, IndexEntry>) {
    if let Ok(bytes) = serde_json::to_vec(index) {
        let _ = crate::store::write_atomic(path, &bytes);
    }
}

/// Muse's `search_text` for `ids`, keyed by session id; ids with no row (or
/// no text) are absent.
///
/// Blocking, so call it off the UI thread. This is the only read of the
/// transcript text, and the search sync asks only for sessions whose
/// fingerprint changed — after boot that is the session a turn just
/// finished in, not all of them. Every failure mode is an empty map.
pub fn read_bodies(path: &std::path::Path, ids: &[String]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if ids.is_empty() {
        return out;
    }
    let Ok(connection) = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return out;
    };
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    // SQLite's default bound on host parameters is 32 766; stay well under.
    for chunk in ids.chunks(500) {
        let marks = vec!["?"; chunk.len()].join(",");
        let sql = format!("SELECT session_id, search_text FROM sessions WHERE session_id IN ({marks})");
        let Ok(mut statement) = connection.prepare(&sql) else { return out };
        let rows = statement.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        });
        if let Ok(rows) = rows {
            for (id, text) in rows.filter_map(Result::ok) {
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    out.insert(id, text);
                }
            }
        }
    }
    out
}

/// Every distinct `workspace_root` the index knows, newest first: the root,
/// how many sessions ran in it, and the newest activity among them.
///
/// What the Projects palette adopts from: recent Muse workspaces, not yet
/// adopted, that still exist on disk (the existence check is the caller's —
/// this only reports what the index says).
pub fn workspaces(index: &HashMap<String, IndexEntry>) -> Vec<(String, usize, Option<i64>)> {
    let mut by_root: HashMap<&str, (usize, Option<i64>)> = HashMap::new();
    for entry in index.values() {
        let Some(root) = entry.workspace_root.as_deref() else { continue };
        let slot = by_root.entry(root).or_insert((0, None));
        slot.0 += 1;
        slot.1 = slot.1.max(entry.updated_at_us);
    }
    let mut out: Vec<(String, usize, Option<i64>)> =
        by_root.into_iter().map(|(root, (count, newest))| (root.to_owned(), count, newest)).collect();
    // Newest activity first; a workspace with no activity sorts last.
    out.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_an_empty_map_and_not_an_error() {
        assert!(read_at(std::path::Path::new("/nonexistent/session-index.db")).is_empty());
    }

    fn db_with(path: &std::path::Path, with_workspace: bool) {
        let connection = rusqlite::Connection::open(path).expect("temp index db");
        let schema = if with_workspace {
            "CREATE TABLE sessions(session_id TEXT, session_name TEXT, title TEXT, first_user_prompt TEXT, search_text TEXT, updated_at_us INTEGER, workspace_root TEXT)"
        } else {
            "CREATE TABLE sessions(session_id TEXT, session_name TEXT, title TEXT, first_user_prompt TEXT, search_text TEXT, updated_at_us INTEGER)"
        };
        connection.execute_batch(schema).expect("schema");
        if with_workspace {
            connection
                .execute(
                    "INSERT INTO sessions VALUES ('s1', NULL, 'Fix the parser', 'why panic', 'body one', 300, '/work/a'), ('s2', NULL, 'Other', NULL, 'body two', 100, '/work/b'), ('s3', NULL, 'More', NULL, 'body three', 200, '/work/a')",
                    [],
                )
                .expect("rows");
        } else {
            connection
                .execute(
                    "INSERT INTO sessions VALUES ('s1', NULL, 'Fix the parser', 'why panic', 'body one', 300)",
                    [],
                )
                .expect("rows");
        }
    }

    #[test]
    fn a_newer_index_yields_workspaces_and_an_older_one_still_yields_titles() {
        let dir = std::env::temp_dir().join(format!("baaz-index-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let newer = dir.join("newer.db");
        db_with(&newer, true);
        let index = read_at(&newer);
        assert_eq!(index.len(), 3);
        assert_eq!(index["s1"].workspace_root.as_deref(), Some("/work/a"));
        assert_eq!(
            workspaces(&index),
            vec![
                ("/work/a".to_owned(), 2, Some(300)),
                ("/work/b".to_owned(), 1, Some(100)),
            ]
        );
        let older = dir.join("older.db");
        db_with(&older, false);
        // No `workspace_root` column: titles still arrive rather than an
        // empty map, and no workspace is reported.
        let index = read_at(&older);
        assert_eq!(index["s1"].label(), Some("Fix the parser"));
        assert_eq!(index["s1"].workspace_root, None);
        assert!(workspaces(&index).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_foreign_schema_is_still_an_empty_map() {
        let dir = std::env::temp_dir().join(format!("baaz-index-foreign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("foreign.db");
        let connection = rusqlite::Connection::open(&path).expect("temp index db");
        connection.execute_batch("CREATE TABLE sessions(id TEXT)").expect("foreign schema");
        assert!(read_at(&path).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_label_prefers_a_name_then_a_title_then_the_first_prompt() {
        let mut entry = IndexEntry {
            title: "Fix the parser".into(),
            first_user_prompt: Some("why does this panic".into()),
            ..IndexEntry::default()
        };
        assert_eq!(entry.label(), Some("Fix the parser"));
        entry.session_name = Some("parser".into());
        assert_eq!(entry.label(), Some("parser"));
        let bare = IndexEntry { first_user_prompt: Some("hello".into()), ..IndexEntry::default() };
        assert_eq!(bare.label(), Some("hello"));
        assert_eq!(IndexEntry::default().label(), None);
        // Muse's own placeholder is not a title (finding F10).
        let placeholder = IndexEntry { title: "New session".into(), ..IndexEntry::default() };
        assert_eq!(placeholder.label(), None);
        let shout = IndexEntry { title: "NEW SESSION".into(), ..IndexEntry::default() };
        assert_eq!(shout.label(), None);
    }

    #[test]
    fn the_sidebar_read_carries_no_bodies_and_bodies_come_per_id() {
        let dir = std::env::temp_dir().join(format!("baaz-index-bodies-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("bodies.db");
        let _ = std::fs::remove_file(&path);
        db_with(&path, true);
        let index = read_at(&path);
        assert!(index.values().all(|entry| entry.search_text.is_empty()), "no body rides the sidebar read");
        let bodies = read_bodies(&path, &["s1".to_owned(), "s3".to_owned(), "missing".to_owned()]);
        assert_eq!(bodies.len(), 2, "only the asked-for rows that exist");
        assert_eq!(bodies["s1"], "body one");
        assert_eq!(bodies["s3"], "body three");
        assert!(read_bodies(&path, &[]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_changed_read_refetches_only_rows_whose_stamp_moved() {
        let dir = std::env::temp_dir().join(format!("baaz-index-changed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("changed.db");
        let _ = std::fs::remove_file(&path);
        db_with(&path, true);
        let full = read_at(&path);
        // A stale carried-over title proves s2 is not refetched; s1's stamp
        // moved, so its fresh title comes back; s3 is gone from `previous`
        // and is fetched.
        let mut previous = full.clone();
        previous.get_mut("s2").expect("s2").title = "carried over".into();
        previous.get_mut("s1").expect("s1").updated_at_us = Some(1);
        previous.get_mut("s1").expect("s1").title = "stale".into();
        previous.remove("s3");
        previous.insert("deleted".into(), IndexEntry { updated_at_us: Some(5), ..Default::default() });
        let changed = read_changed(&path, &previous);
        assert_eq!(changed.len(), 3, "rows Muse no longer has drop out");
        assert_eq!(changed["s1"].title, "Fix the parser", "a moved stamp is refetched");
        assert_eq!(changed["s2"].title, "carried over", "an unchanged stamp is carried, not read");
        assert_eq!(changed["s3"], full["s3"], "a row the cache lacks is read");
        let cache = dir.join("cache.json");
        save_cache(&cache, &changed);
        assert_eq!(load_cache(&cache), changed, "the cache round-trips");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Timing on a real copy (`BAAZ_PERF_INDEX`), not a gate.
    #[test]
    #[ignore]
    fn perf_changed_read_against_a_real_copy() {
        let Some(path) = std::env::var_os("BAAZ_PERF_INDEX") else { return };
        let path = std::path::PathBuf::from(path);
        let at = std::time::Instant::now();
        let full = read_at(&path);
        eprintln!("PERF full read rows={} in={:?}", full.len(), at.elapsed());
        let at = std::time::Instant::now();
        let cold = read_changed(&path, &HashMap::new());
        eprintln!("PERF changed read, no cache rows={} in={:?}", cold.len(), at.elapsed());
        let at = std::time::Instant::now();
        let warm = read_changed(&path, &full);
        eprintln!("PERF changed read, cache current rows={} in={:?}", warm.len(), at.elapsed());
        let mut one_stale = full.clone();
        if let Some(entry) = one_stale.values_mut().next() {
            entry.updated_at_us = Some(1);
        }
        let at = std::time::Instant::now();
        let _ = read_changed(&path, &one_stale);
        eprintln!("PERF changed read, one row moved in={:?}", at.elapsed());
        let cache = std::env::temp_dir().join("baaz-perf-index-cache.json");
        let at = std::time::Instant::now();
        save_cache(&cache, &full);
        eprintln!("PERF save cache bytes={} in={:?}", std::fs::metadata(&cache).map(|m| m.len()).unwrap_or(0), at.elapsed());
        let at = std::time::Instant::now();
        let loaded = load_cache(&cache);
        eprintln!("PERF load cache rows={} in={:?}", loaded.len(), at.elapsed());
        assert_eq!(cold, full, "the changed read with no cache equals the full read");
        assert_eq!(warm, full);
    }
}
