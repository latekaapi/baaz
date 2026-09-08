//! Prompt history: every sent prompt, per workspace, on disk.
//!
//! `~/Library/Application Support/harness/history.json`, keyed by the canonical
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

/// `~/Library/Application Support/harness/history.json`.
pub fn path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join("Library/Application Support/harness/history.json"))
}

fn read_all() -> BTreeMap<String, Vec<String>> {
    let Some(path) = path() else { return BTreeMap::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return BTreeMap::new() };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Every prompt sent in `workspace`, oldest first.
pub fn read(workspace: &str) -> Vec<String> {
    read_all().remove(workspace).unwrap_or_default()
}

/// Append `text` to `workspace`'s history and write the file back.
///
/// Returns the history as it now stands, so the caller does not have to read it
/// again. A prompt equal to the newest one is not appended: holding Enter on the
/// same message should not fill the history with it.
pub fn append(workspace: &str, text: &str) -> Vec<String> {
    let mut all = read_all();
    let entry = all.entry(workspace.to_owned()).or_default();
    if entry.last().map(String::as_str) != Some(text) {
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

fn write_all(all: &BTreeMap<String, Vec<String>>) {
    let Some(path) = path() else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(all) {
        let _ = std::fs::write(path, text);
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
}
