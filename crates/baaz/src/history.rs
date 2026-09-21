//! Prompt history: every sent prompt, per workspace, on disk.
//!
//! `~/Library/Application Support/baaz/history.json`, keyed by the canonical
//! workspace path — the same canonicalization `session/list` needs, so `/tmp/x`
//! and `/private/tmp/x` are one history and not two. The newest 200 are kept and
//! a prompt identical to the one before it is not appended twice.
//!
//! It is a convenience, not a record: every read tolerates a missing, truncated
//! or unreadable file by returning an empty history, and every write is
//! best-effort.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// How many prompts are remembered per workspace.
const CAP: usize = 200;

/// `~/Library/Application Support/baaz/history.json`, honouring
/// `BAAZ_STATE_DIR` like every other store.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("history.json")
}

fn read_all() -> BTreeMap<String, Vec<String>> {
    let Ok(text) = std::fs::read_to_string(path()) else { return BTreeMap::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Every prompt sent in `workspace`, oldest first.
pub fn read(workspace: &str) -> Vec<String> {
    read_all().remove(workspace).unwrap_or_default()
}

/// How many of the most recent entries a resend is checked against (finding
/// `support-6`): the old check compared only the newest entry, so
/// alternating A/B resends (A, B, A, B, …) grew the file to the cap with
/// duplicates instead of settling into two.
const RECENT_DEDUP_WINDOW: usize = 3;

/// Append `text` to `workspace`'s history and write the file back.
///
/// Returns the history as it now stands, so the caller does not have to read it
/// again. A prompt equal to any of the last few is not appended again: holding
/// Enter on the same message, or bouncing between a couple of prompts, should
/// not fill the history with repeats.
pub fn append(workspace: &str, text: &str) -> Vec<String> {
    let mut all = read_all();
    let entry = all.entry(workspace.to_owned()).or_default();
    if !is_recent_duplicate(entry, text) {
        entry.push(text.to_owned());
    }
    if entry.len() > CAP {
        let excess = entry.len() - CAP;
        entry.drain(..excess);
    }
    let out = entry.clone();
    write_all(&all);
    out
}

/// Whether `text` equals any of `entries`' last [`RECENT_DEDUP_WINDOW`]
/// prompts. Pure, so the window's boundary is unit-testable without
/// touching disk.
fn is_recent_duplicate(entries: &[String], text: &str) -> bool {
    let recent_len = entries.len().saturating_sub(RECENT_DEDUP_WINDOW);
    entries[recent_len..].iter().any(|prompt| prompt == text)
}

fn write_all(all: &BTreeMap<String, Vec<String>>) {
    let path = path();
    // Atomic like every other store write: a crash mid-send leaves the
    // previous history rather than half of the next one.
    if let Ok(text) = serde_json::to_string_pretty(all) {
        let _ = crate::store::write_atomic(&path, text.as_bytes());
    }
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
    fn the_file_lives_under_baaz_state_dir() {
        let dir = std::env::temp_dir().join(format!("baaz-history-{}", std::process::id()));
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        assert_eq!(path(), dir.join("history.json"));
        let entries = append("/work/a", "hello");
        assert_eq!(entries, vec!["hello".to_owned()]);
        assert_eq!(read("/work/a"), vec!["hello".to_owned()]);
        assert!(path().is_file());
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }
}
