//! What the harness knows about a session that MSP has no room for (spec §3.7,
//! Phase 5 A2).
//!
//! Three facts, and none of them are on the wire:
//!
//! * **A name.** MSP has `session_name` in Muse's own index and no command to
//!   set it, so `/name` writes here.
//! * **Hidden.** MSP has no archive, no delete and no `hidden` flag. Hiding is
//!   a decision about *this* window's list, so it lives in *this* window's
//!   store and never touches Muse's.
//! * **A derived title.** A session with no user prompt has nothing to be
//!   called: fourteen rows reading "New session" is what Phase 4's screenshots
//!   show (finding F10). The first `userShell` command is the honest label, and
//!   reading it costs a `session/read`, so the answer is cached here.
//!
//! The file is `~/Library/Application Support/harness/sessions.json`, written
//! atomically through [`crate::store`], and every read is best-effort: a
//! missing or unparseable file is an empty map, which loses an override and
//! never a session.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The harness's own facts about one session.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMeta {
    /// The name `/name` or the row's pencil gave it. `None` means "whatever
    /// the index calls it"; it is cleared rather than set to an empty string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Hidden from this window's list. A hidden session is never loaded.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub hidden: bool,
    /// The title derived from the session's first `userShell` command, cached
    /// so the `session/read` that found it happens once (finding F10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_title: Option<String>,
    /// Pinned to the top of the sidebar's date view, in its own group.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pinned: bool,
    /// Archived out of the sidebar's list (shown only from the Sessions menu).
    /// An archived session stays on disk and is never loaded while archived.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub archived: bool,
    /// The first line of the last assistant text block, written when a turn
    /// completes in this app. Free — the fold is already in memory — and the
    /// sidebar's description line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_summary: Option<String>,
}

impl SessionMeta {
    /// Whether this entry still says anything, which is what decides if it is
    /// worth keeping in the file.
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && !self.hidden
            && self.derived_title.is_none()
            && !self.pinned
            && !self.archived
            && self.last_summary.is_none()
    }
}

/// Every override this window knows, keyed by session id.
pub type Overrides = HashMap<String, SessionMeta>;

/// `~/Library/Application Support/harness/sessions.json`.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("sessions.json")
}

/// Read the store. Blocking; call it off the UI thread.
pub fn read() -> Overrides {
    crate::store::read_json(&path())
}

/// Write the store, atomically, dropping entries that no longer say anything.
///
/// Best-effort: a store that cannot be written loses an override, which is a
/// nuisance, and never a session, which would be a loss.
pub fn write(overrides: &Overrides) {
    let kept: Overrides =
        overrides.iter().filter(|(_, meta)| !meta.is_empty()).map(|(k, v)| (k.clone(), v.clone())).collect();
    if let Ok(text) = serde_json::to_vec_pretty(&kept) {
        let _ = crate::store::write_atomic(&path(), &text);
    }
}

/// A `userShell` command turned into a row's title (finding F10).
///
/// `!` is how the person typed it and `$` is how the transcript draws it;
/// neither belongs in a sidebar row, which has one line and wants the words.
pub fn shell_title(command: &str) -> Option<String> {
    let text = command.trim().trim_start_matches(['!', '$']).trim();
    if text.is_empty() {
        return None;
    }
    Some(text.to_owned())
}

/// Case-insensitive subsequence match: does `needle`'s characters appear in
/// `haystack`, in order?
///
/// The sidebar's filter. A subsequence rather than a substring because a
/// session called "fix the parser panic" should be found by typing `fxparse`,
/// which is what a person does when they half-remember a name.
pub fn matches(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let mut wanted = needle.chars().flat_map(char::to_lowercase).peekable();
    for c in haystack.chars().flat_map(char::to_lowercase) {
        match wanted.peek() {
            Some(next) if *next == c => {
                wanted.next();
            }
            Some(_) => {}
            None => return true,
        }
    }
    wanted.peek().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_that_says_nothing_is_not_written() {
        let mut overrides = Overrides::new();
        overrides.insert("empty".into(), SessionMeta::default());
        overrides.insert("named".into(), SessionMeta { name: Some("x".into()), ..SessionMeta::default() });
        let kept: Vec<&String> = overrides.iter().filter(|(_, m)| !m.is_empty()).map(|(k, _)| k).collect();
        assert_eq!(kept, vec!["named"]);
    }

    #[test]
    fn pin_archive_and_summary_all_keep_their_entry() {
        for meta in [
            SessionMeta { pinned: true, ..SessionMeta::default() },
            SessionMeta { archived: true, ..SessionMeta::default() },
            SessionMeta { last_summary: Some("did a thing".into()), ..SessionMeta::default() },
        ] {
            assert!(!meta.is_empty());
        }
    }

    #[test]
    fn the_new_fields_round_trip_as_camel_case() {
        let meta = SessionMeta {
            pinned: true,
            archived: true,
            last_summary: Some("did a thing".into()),
            ..SessionMeta::default()
        };
        let text = serde_json::to_string(&meta).unwrap();
        assert!(text.contains("\"pinned\":true"));
        assert!(text.contains("\"archived\":true"));
        assert!(text.contains("\"lastSummary\":\"did a thing\""));
        let back: SessionMeta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
        // Old files without the fields still read.
        let old: SessionMeta = serde_json::from_str("{\"hidden\":true}").unwrap();
        assert!(!old.pinned && !old.archived && old.last_summary.is_none());
    }

    #[test]
    fn a_shell_command_loses_its_prompt_but_keeps_its_words() {
        assert_eq!(shell_title("!cargo test -q").as_deref(), Some("cargo test -q"));
        assert_eq!(shell_title("  $ ls  ").as_deref(), Some("ls"));
        assert_eq!(shell_title("  !  "), None);
    }

    #[test]
    fn the_filter_is_a_case_insensitive_subsequence() {
        assert!(matches("fix the parser panic", "fxparse"));
        assert!(matches("Fix The Parser", "parser"));
        assert!(matches("anything", ""));
        assert!(!matches("fix the parser", "zebra"));
        // Order matters: a subsequence is not a bag of letters.
        assert!(!matches("abc", "cba"));
    }
}
