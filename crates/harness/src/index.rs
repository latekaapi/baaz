//! The local session index, read-only (spec §3.7).
//!
//! `session/list` gives identity and timestamps and nothing a person can read:
//! no title, no first prompt. Muse keeps those in
//! `~/.local/share/muse/session-index.db`, which the harness opens **read-only**
//! and treats as a cache — a missing file, a schema this build has never seen
//! and a database another process has locked are all ordinary, and every one of
//! them yields an empty map rather than an error the person has to see.
//!
//! Nothing here ever writes. Rename and hide land in the harness's own
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
pub fn read_at(path: &std::path::Path) -> HashMap<String, IndexEntry> {
    let Ok(connection) = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ) else {
        return HashMap::new();
    };
    let _ = connection.busy_timeout(BUSY_TIMEOUT);
    let sql = "SELECT session_id, session_name, title, first_user_prompt, search_text, updated_at_us FROM sessions";
    let Ok(mut statement) = connection.prepare(sql) else {
        return HashMap::new();
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
            },
        ))
    });
    let Ok(rows) = rows else { return HashMap::new() };
    rows.filter_map(Result::ok).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_database_is_an_empty_map_and_not_an_error() {
        assert!(read_at(std::path::Path::new("/nonexistent/session-index.db")).is_empty());
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
    }
}
