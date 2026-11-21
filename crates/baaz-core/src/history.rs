//! Prompt history: every sent prompt, per workspace.
//!
//! Kept in `baaz.db`'s `prompt_history` ([`crate::db::history`]), keyed by
//! the canonical workspace path — the same canonicalization `session/list`
//! needs, so `/tmp/x` and `/private/tmp/x` are one history and not two. The
//! newest 200 are kept and a prompt identical to one of the last few is not
//! appended again.
//!
//! It is a convenience, not a record: a database that cannot answer reads
//! as an empty history, and every write is best-effort. Both calls block on
//! the database's writer: callers run them off the UI thread.

/// How many prompts are remembered per workspace.
const CAP: usize = 200;

/// Every prompt sent in `workspace`, oldest first.
pub fn read(workspace: &str) -> Vec<String> {
    let workspace = workspace.to_owned();
    crate::db::read(crate::db::path(), move |db| crate::db::history::read(&db.conn, &workspace))
        .and_then(|read| read.map_err(|error| crate::baaz_log!("db: prompt history does not read ({error})")).ok())
        .unwrap_or_default()
}

/// How many of the most recent entries a resend is checked against (finding
/// `support-6`): the old check compared only the newest entry, so
/// alternating A/B resends (A, B, A, B, …) grew the file to the cap with
/// duplicates instead of settling into two.
const RECENT_DEDUP_WINDOW: usize = 3;

/// Append `text` to `workspace`'s history and write it back, in one
/// transaction on the writer.
///
/// Returns the history as it now stands, so the caller does not have to read it
/// again. A prompt equal to any of the last few is not appended again: holding
/// Enter on the same message, or bouncing between a couple of prompts, should
/// not fill the history with repeats.
pub fn append(workspace: &str, text: &str) -> Vec<String> {
    let (workspace, text) = (workspace.to_owned(), text.to_owned());
    crate::db::call(crate::db::path(), move |db| {
        let mut entries = crate::db::history::read(&db.conn, &workspace).unwrap_or_default();
        let before = entries.clone();
        appended(&mut entries, &text);
        if entries != before {
            if let Err(error) = crate::db::history::replace(&mut db.conn, &workspace, &entries) {
                crate::baaz_log!("db: prompt history not saved ({error})");
            }
        }
        entries
    })
    .unwrap_or_default()
}

/// Forget every prompt kept, in every workspace, on the writer
/// (Settings › Data › Prompt history, docs/82 8.8): what ↑ in a composer
/// walks. Sessions and their transcripts are untouched. Blocks; returns how
/// many prompts went, `None` when the database did not answer.
pub fn clear_all() -> Option<usize> {
    crate::db::call(crate::db::path(), |db| crate::db::history::clear_all(&db.conn))
        .and_then(|cleared| cleared.map_err(|error| crate::baaz_log!("db: prompt history not cleared ({error})")).ok())
}

/// `entries` with `text` appended (unless it repeats one of the last few),
/// cut to the newest [`CAP`].
fn appended(entries: &mut Vec<String>, text: &str) {
    if !is_recent_duplicate(entries, text) {
        entries.push(text.to_owned());
    }
    if entries.len() > CAP {
        let excess = entries.len() - CAP;
        entries.drain(..excess);
    }
}

/// Whether `text` equals any of `entries`' last [`RECENT_DEDUP_WINDOW`]
/// prompts. Pure, so the window's boundary is unit-testable without
/// touching disk.
fn is_recent_duplicate(entries: &[String], text: &str) -> bool {
    let recent_len = entries.len().saturating_sub(RECENT_DEDUP_WINDOW);
    entries[recent_len..].iter().any(|prompt| prompt == text)
}

/// Where the composer is in the history it is walking.
///
/// The draft the person was typing is the newest slot, so walking up and back
/// down again returns exactly what they had. `None` means "on the draft".
#[derive(Debug, Default)]
pub struct Cursor {
    entries: Vec<String>,
    at: Option<usize>,
    draft: String,
}

impl Cursor {
    /// A cursor over `entries`, sitting on the draft.
    pub fn new(entries: Vec<String>) -> Self {
        Self { entries, at: None, draft: String::new() }
    }

    /// Replace the entries after a send.
    pub fn set(&mut self, entries: Vec<String>) {
        self.entries = entries;
        self.reset();
    }

    /// Back to the draft slot; called whenever the person types.
    pub fn reset(&mut self) {
        self.at = None;
        self.draft.clear();
    }

    /// Whether the cursor has left the draft.
    pub fn walking(&self) -> bool {
        self.at.is_some()
    }

    /// Step to the older prompt, keeping `draft` as the newest slot. `None`
    /// means there is nothing older and the composer should not change.
    pub fn prev(&mut self, draft: &str) -> Option<String> {
        if self.entries.is_empty() {
            return None;
        }
        let next = match self.at {
            None => {
                self.draft = draft.to_owned();
                self.entries.len() - 1
            }
            Some(0) => return None,
            Some(at) => at - 1,
        };
        self.at = Some(next);
        self.entries.get(next).cloned()
    }

    /// Step to the newer prompt, or back to the draft.
    // A cursor step, not an iterator: public since the core crate split
    // (docs/82 4.11), and `prev` is its pair.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<String> {
        let at = self.at?;
        if at + 1 >= self.entries.len() {
            self.at = None;
            return Some(std::mem::take(&mut self.draft));
        }
        self.at = Some(at + 1);
        self.entries.get(at + 1).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor() -> Cursor {
        Cursor::new(vec!["one".into(), "two".into(), "three".into()])
    }

    #[test]
    fn up_walks_backwards_from_the_newest() {
        let mut c = cursor();
        assert_eq!(c.prev("draft").as_deref(), Some("three"));
        assert_eq!(c.prev("draft").as_deref(), Some("two"));
        assert_eq!(c.prev("draft").as_deref(), Some("one"));
        assert_eq!(c.prev("draft"), None);
    }

    #[test]
    fn down_comes_back_to_the_draft() {
        let mut c = cursor();
        c.prev("draft");
        c.prev("draft");
        assert_eq!(c.next().as_deref(), Some("three"));
        assert_eq!(c.next().as_deref(), Some("draft"));
        assert_eq!(c.next(), None);
        assert!(!c.walking());
    }

    #[test]
    fn an_empty_history_does_nothing() {
        let mut c = Cursor::new(Vec::new());
        assert_eq!(c.prev("draft"), None);
        assert_eq!(c.next(), None);
    }

    /// **support-6 / A-MECH-16.** The old check compared only the newest
    /// entry, so an alternating A/B resend grew without bound; the last
    /// `RECENT_DEDUP_WINDOW` entries are checked now.
    #[test]
    fn a_resend_within_the_recent_window_is_not_a_duplicate_append() {
        let entries: Vec<String> = vec!["a".into(), "b".into()];
        assert!(is_recent_duplicate(&entries, "a"), "an alternating resend must be caught");
        assert!(is_recent_duplicate(&entries, "b"), "the newest entry is still caught");
        assert!(!is_recent_duplicate(&entries, "c"), "a genuinely new prompt is not a duplicate");
    }

    #[test]
    fn a_repeat_older_than_the_window_is_not_a_duplicate() {
        let entries: Vec<String> =
            vec!["old".into(), "a".into(), "b".into(), "c".into()];
        // "old" is now four back — outside the 3-entry window — so it may
        // reappear.
        assert!(!is_recent_duplicate(&entries, "old"));
        assert!(is_recent_duplicate(&entries, "c"));
    }

    #[test]
    fn the_history_lives_in_the_state_dirs_database() {
        let dir = std::env::temp_dir().join(format!("baaz-history-{}", std::process::id()));
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let entries = append("/work/a", "hello");
        assert_eq!(entries, vec!["hello".to_owned()]);
        assert_eq!(append("/work/a", "hello"), vec!["hello".to_owned()], "a repeat is not appended");
        assert_eq!(read("/work/a"), vec!["hello".to_owned()]);
        assert!(read("/work/b").is_empty(), "each workspace has its own");
        assert!(crate::db::path_in(&dir).is_file(), "the prompts are in baaz.db");
        assert!(!dir.join(crate::db::json_import::HISTORY).exists(), "nothing writes the old file");
        crate::db::close(crate::db::path_in(&dir));
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    #[test]
    fn the_newest_two_hundred_are_kept() {
        let mut entries = Vec::new();
        for n in 0..205 {
            appended(&mut entries, &format!("p{n}"));
        }
        assert_eq!(entries.len(), CAP);
        assert_eq!(entries.first().map(String::as_str), Some("p5"));
    }
}
