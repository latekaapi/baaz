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
//! * **A derived title.** A session with nothing to be called by reads the
//!   same as every other one: fourteen rows reading "New session" is what
//!   Phase 4's screenshots show (finding F10). The transcript's first user
//!   prompt is the honest label — the first `userShell` command only when
//!   the session has no user text at all — and reading the shell form costs
//!   a `session/read`, so the answer is cached here.
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
    /// The title derived from the transcript — the first user prompt, or the
    /// first `userShell` command when the session has no user text at all —
    /// cached so the `session/read` that found the shell form happens once
    /// (finding F10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_title: Option<String>,
    /// The title one cheap model call wrote for this session (auto-titles):
    /// 3–6 words harvested off a throwaway side session's `turn/completed`.
    /// Ranked directly under [`Self::name`] in the label order, and never
    /// written to the server — `session/rename` stays untouched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_title: Option<String>,
    /// A title generation already ran (or is running) for this session. The
    /// once-ever marker: set the moment a generation starts, persisted, so
    /// resume, reconnect, replay and restart never earn a second one, and a
    /// retry budget of one is enforced per session, not per run.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub title_attempted: bool,
    /// One of this app's throwaway title/summary side sessions. The explicit
    /// record behind the hide rule: written (with `hidden`) the moment the
    /// side id is minted — before `session/start` runs — so a crash between
    /// the start and the hide still hides by record after a restart, and a
    /// restart mid-flight still hides, counts, indexes and titles nothing.
    /// muse 1.3.0 rejects any `session/start` id that is not its own id
    /// shape, so the id carries no namespace to match on; this flag is the
    /// only recognition.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub side_session: bool,
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
    /// The owner's last request in this session, excerpted the free way and
    /// written beside [`Self::last_summary`] on every completed turn. The
    /// byline's ask half: together they are the row's two lines, free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ask: Option<String>,
    /// The project this session was started in: the adoption it groups
    /// under even when its folder is a worktree of the project's root.
    /// Written at `session/start`; always serialized, so a file that says
    /// nothing about a session says `"project":null` rather than staying
    /// silent about the question.
    #[serde(default)]
    pub project: Option<String>,
}

impl SessionMeta {
    /// Whether this entry still says anything, which is what decides if it is
    /// worth keeping in the file.
    fn is_empty(&self) -> bool {
        self.name.is_none()
            && !self.hidden
            && self.derived_title.is_none()
            && self.generated_title.is_none()
            && !self.title_attempted
            && !self.side_session
            && !self.pinned
            && !self.archived
            && self.last_summary.is_none()
            && self.last_ask.is_none()
            && self.project.is_none()
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

/// The transcript's own first user prompt, as a row's title: a session is
/// named after what the person said, never after a command the agent ran.
///
/// `submissions` are the fold's recorded `command_text`s, oldest first (the
/// map is keyed by time-ordered command id); `turns` are the folded turns in
/// wire order. The earliest recorded submission wins — at `turn/started`
/// the server has not echoed the `userMessage` yet, but `submit` already
/// recorded it — then the earliest folded user turn, which covers replays
/// and restarts that recorded nothing. A submission wearing the `!`/`$`
/// shell marker is a shell invocation, not prose, and is skipped: the
/// shell fallback ([`shell_title`]) names those sessions. First line only,
/// whitespace-collapsed, cut where a sidebar row would truncate it anyway
/// ([`crate::sidebar::one_line`]).
pub fn first_user_title(submissions: &[&str], turns: &[aui_protocol::Turn]) -> Option<String> {
    fn first_line(text: &str) -> Option<&str> {
        let line = text.lines().next().map(str::trim).unwrap_or("");
        (!line.is_empty()).then_some(line)
    }
    let said = submissions
        .iter()
        .map(|submission| submission.trim())
        .filter(|submission| !submission.is_empty() && !submission.starts_with(['!', '$']))
        .find_map(first_line)
        .or_else(|| {
            turns.iter().find_map(|turn| match turn {
                aui_protocol::Turn::User { text, .. } => first_line(text),
                _ => None,
            })
        })?;
    let title = crate::sidebar::one_line(said);
    (!title.is_empty()).then_some(title)
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
            project: Some("6d0e".into()),
            ..SessionMeta::default()
        };
        let text = serde_json::to_string(&meta).unwrap();
        assert!(text.contains("\"pinned\":true"));
        assert!(text.contains("\"archived\":true"));
        assert!(text.contains("\"lastSummary\":\"did a thing\""));
        assert!(text.contains("\"project\":\"6d0e\""));
        let back: SessionMeta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
        // Old files without the fields still read.
        let old: SessionMeta = serde_json::from_str("{\"hidden\":true}").unwrap();
        assert!(!old.pinned && !old.archived && old.last_summary.is_none() && old.project.is_none());
    }

    #[test]
    fn a_shell_command_loses_its_prompt_but_keeps_its_words() {
        assert_eq!(shell_title("!cargo test -q").as_deref(), Some("cargo test -q"));
        assert_eq!(shell_title("  $ ls  ").as_deref(), Some("ls"));
        assert_eq!(shell_title("  !  "), None);
    }

    fn user_turn(text: &str) -> aui_protocol::Turn {
        aui_protocol::Turn::User {
            id: "t-user".into(),
            text: text.into(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            timestamp: None,
        }
    }

    #[test]
    fn the_first_user_prompt_names_the_session_not_a_later_one() {
        // Oldest submission first, as the fold's map orders them: the title
        // is what the person said first, never the newest send.
        let title = first_user_title(&["Explain how this project is laid out", "and then refactor it"], &[]);
        assert_eq!(title.as_deref(), Some("Explain how this project is laid out"));
    }

    #[test]
    fn an_earlier_submission_beats_an_earlier_folded_turn() {
        let turns = vec![user_turn("folded words")];
        let title = first_user_title(&["typed words"], &turns);
        assert_eq!(title.as_deref(), Some("typed words"));
    }

    #[test]
    fn a_folded_first_turn_names_a_session_that_recorded_nothing() {
        // Replays and restarts record no submissions: the earliest folded
        // user turn is the title, first line only.
        let turns = vec![user_turn("Run the shell command `ls`\nthen describe README"), user_turn("second")];
        let title = first_user_title(&[], &turns);
        assert_eq!(title.as_deref(), Some("Run the shell command `ls`"));
    }

    #[test]
    fn shell_invocations_are_not_user_prose() {
        // `!ls` went through the shell hatch, not a turn: it must not win
        // the user slot (the shell fallback names that session instead).
        let title = first_user_title(&["!ls -la"], &[]);
        assert_eq!(title, None);
        // ...but a real first prompt ahead of one still does.
        let title = first_user_title(&["what does this do", "!ls -la"], &[]);
        assert_eq!(title.as_deref(), Some("what does this do"));
    }

    #[test]
    fn a_derived_title_is_one_row_long() {
        let long = "word ".repeat(60);
        let title = first_user_title(&[long.as_str()], &[]).expect("a title");
        // The row's own convention (sidebar `one_line`): 80 cells, elided.
        assert_eq!(title.chars().count(), 80);
        assert!(title.ends_with('\u{2026}'));
        // First line only, with the whitespace collapsed.
        let title = first_user_title(&["  why   does   this   panic  \nsecond line"], &[]);
        assert_eq!(title.as_deref(), Some("why does this panic"));
    }

    #[test]
    fn blank_submissions_and_silence_are_no_title() {
        assert_eq!(first_user_title(&["   "], &[]), None);
        assert_eq!(first_user_title(&[], &[]), None);
        assert_eq!(first_user_title(&[], &[user_turn("  ")]), None);
    }

}
