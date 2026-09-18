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
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IndexEntry {
    /// The session's own name, when someone gave it one.
    pub session_name: Option<String>,
    /// The generated title.
    pub title: String,
    /// The first thing the person typed, which is the best fallback label.
    pub first_user_prompt: Option<String>,
    /// Everything the index made searchable, for the Phase 5 search field.
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
    /// clothes, and the reason fourteen rows read the same thing in Phase 4's
    /// screenshots (finding F10). It is treated as no title at all, so the
    /// caller goes on to the next fact it has.
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
        "SELECT session_id, session_name, title, first_user_prompt, search_text, updated_at_us, workspace_root FROM sessions"
    } else {
        "SELECT session_id, session_name, title, first_user_prompt, search_text, updated_at_us FROM sessions"
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
                search_text: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
                updated_at_us: row.get(5).ok(),
                workspace_root: if has_workspace { row.get(6).ok() } else { None },
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

/// Every distinct `workspace_root` the index knows, newest first: the root,
/// how many sessions ran in it, and the newest activity among them.
///
/// What the Projects palette adopts from: recent Muse workspaces, not yet
/// adopted, that still exist on disk (the existence check is the caller's —
/// this only reports what the index says).
///
/// Package 2's palette calls this; package 1 only reads the column.
#[allow(dead_code)]
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
}
