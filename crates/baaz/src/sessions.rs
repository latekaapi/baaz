//! What Baaz knows about a session that MSP has no room for (spec §3.7).
//!
//! Three facts, and none of them are on the wire:
//!
//! * **A name.** MSP has `session_name` in Muse's own index and no command to
//!   set it, so `/name` writes here.
//! * **Hidden.** MSP has no archive, no delete and no `hidden` flag. Hiding is
//!   a decision about *this* window's list, so it lives in *this* window's
//!   store and never touches Muse's.
//! * **A derived title.** A session with nothing to be called by reads the
//!   same as every other one: fourteen rows can read "New session" at once.
//!   The transcript's first user
//!   prompt is the honest label — the first `userShell` command only when
//!   the session has no user text at all — and reading the shell form costs
//!   a `session/read`, so the answer is cached here.
//!
//! The file is `~/Library/Application Support/baaz/sessions.json`, written
//! atomically through [`crate::store`], and every read is best-effort: a
//! missing or unparseable file is an empty map, which loses an override and
//! never a session.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::layout::RightKind;

/// The right pane as one session left it: whether it stood open, on which
/// kind, and the Files sub-state (open preview, selection, expansion) for
/// that session. A session with no stored state shows the pane closed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct RightState {
    /// Whether the pane stood open. Closed by default: the pane is asked
    /// for, never assumed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub open: bool,
    /// Which kind the pane showed. Files when nothing was ever stored, or
    /// when a newer build stored a kind this one does not know: only this
    /// field falls back, never the entry or the file (docs/81 A F8).
    #[serde(default, deserialize_with = "crate::store::lenient")]
    pub kind: RightKind,
    /// The tree id of the open Files preview, if one stood open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_preview: Option<String>,
    /// The last selected file id, kept when the preview closes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_selected: Option<String>,
    /// The directory ids standing open in the Files tree.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_expanded: Vec<String>,
    /// The Browser pane's last URL for this session (Z7a): written on real
    /// navigations, read when the session's webview is first created. `None`
    /// is "never navigated": a fresh webview opens on `about:blank`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_url: Option<String>,
}

/// Baaz's own facts about one session.
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
    /// A browser chat (docs/27 §6, R3): a session of kind `browser` in the
    /// hidden browser workspace, reached from the line, the card and its
    /// task folder, never from the Sessions sidebar.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser_chat: bool,
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
    /// The user's last request in this session, excerpted the free way and
    /// written beside [`Self::last_summary`] on every completed turn. The
    /// byline's ask half: together they are the row's two lines, free.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_ask: Option<String>,
    /// The last turn's terminal error message, written on `turn/completed`
    /// with terminal `"failed"` and cleared by the next turn's start or its
    /// success. What a closed session's `Failed` row stands on — the wire
    /// carries no per-session error, so Baaz remembers it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The project this session was started in: the adoption it groups
    /// under even when its folder is a worktree of the project's root.
    /// Written at `session/start`; always serialized, so a file that says
    /// nothing about a session says `"project":null` rather than staying
    /// silent about the question.
    #[serde(default)]
    pub project: Option<String>,
    /// The handoff that retired this session: the fresh destination
    /// session id, written on activation. The destination's row carries
    /// the other half, so both survive restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_to: Option<String>,
    /// The handoff this session was born from: the source session id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from: Option<String>,
    /// The source session's wire provider id: what the sidebar row's
    /// "from <Provider>" byline reads without joining another record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_from_provider: Option<String>,
    /// The chain's title, written on the destination at activation from the
    /// source's current display title (`docs/22-handoff.md` §8). Ranked
    /// directly under a user rename of the head, above the session's own
    /// title, so the chain keeps one title across providers and restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handoff_title: Option<String>,
    /// The right pane as this session left it (Z2): open, kind, and the
    /// Files preview/selection/expansion. `None` shows the pane closed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<RightState>,
}

/// Every override this window knows, keyed by session id.
pub type Overrides = HashMap<String, SessionMeta>;

/// The person's own facts about each session — name, hidden, pinned,
/// archived, project, chain links — in `session-meta.json`; everything
/// that can be regenerated — titles, byline, last error, pane state — in
/// `session-cache.json` (docs/82 1.7, docs/81 A F11). Before the split one
/// `sessions.json` held both and was rewritten whole (about 110 KB) after
/// every finished turn and every pane navigation, which exposed the part
/// that cannot be regenerated to every one of those writes.
const FACT_FIELDS: &[&str] = &[
    "name",
    "hidden",
    "sideSession",
    "browserChat",
    "pinned",
    "archived",
    "project",
    "handoffTo",
    "handoffFrom",
    "handoffFromProvider",
    "handoffTitle",
];

static FACTS: crate::store::JsonStore<Overrides> =
    crate::store::JsonStore::new("session-meta.json").with_encoding(encode_facts);
static CACHE: crate::store::JsonStore<Overrides> =
    crate::store::JsonStore::new("session-cache.json").with_encoding(encode_cache);

/// The file that held both halves before the split: read once when the
/// split files do not exist yet, and left on disk as it was.
const LEGACY: &str = "sessions.json";

/// The entries' fields `keep` names, as JSON; entries that say nothing are
/// dropped.
fn project(overrides: &Overrides, keep: impl Fn(&str) -> bool) -> Option<Vec<u8>> {
    let mut out = serde_json::Map::new();
    for (id, meta) in overrides {
        let Ok(serde_json::Value::Object(fields)) = serde_json::to_value(meta) else { continue };
        let kept: serde_json::Map<String, serde_json::Value> =
            fields.into_iter().filter(|(key, value)| keep(key) && !value.is_null()).collect();
        if !kept.is_empty() {
            out.insert(id.clone(), serde_json::Value::Object(kept));
        }
    }
    serde_json::to_vec_pretty(&out).ok()
}

fn encode_facts(overrides: &Overrides) -> Option<Vec<u8>> {
    project(overrides, |key| FACT_FIELDS.contains(&key))
}

fn encode_cache(overrides: &Overrides) -> Option<Vec<u8>> {
    project(overrides, |key| !FACT_FIELDS.contains(&key))
}

/// Both halves' paths, facts first.
pub fn paths() -> [PathBuf; 2] {
    [FACTS.path(), CACHE.path()]
}

/// Whether either half is on disk.
pub fn stored() -> bool {
    paths().iter().any(|path| path.exists())
}

/// Read the store: both halves merged entry by entry, each entry lenient
/// ([`crate::store::read_json_map`]). With neither half on disk, the
/// pre-split `sessions.json` is read instead. Blocking; call it off the UI
/// thread.
pub fn read() -> Overrides {
    if !stored() {
        let legacy = crate::store::support_dir().join(LEGACY);
        if legacy.exists() {
            return crate::store::read_json_map(&legacy);
        }
        return Overrides::new();
    }
    let mut merged: HashMap<String, serde_json::Map<String, serde_json::Value>> = HashMap::new();
    for path in paths() {
        let half: HashMap<String, serde_json::Value> = crate::store::read_json_map(&path);
        for (id, value) in half {
            if let serde_json::Value::Object(fields) = value {
                merged.entry(id).or_default().extend(fields);
            }
        }
    }
    merged
        .into_iter()
        .filter_map(|(id, fields)| {
            match serde_json::from_value::<SessionMeta>(serde_json::Value::Object(fields)) {
                Ok(meta) => Some((id, meta)),
                Err(error) => {
                    crate::baaz_log!("store: session {id:?} does not parse ({error}); dropped");
                    None
                }
            }
        })
        .collect()
}

/// Write both halves now, in order with every queued save. Blocking; the
/// quit flush and tests. A failed write is reported by the store and the
/// file on disk stays the previous one.
pub fn write(overrides: &Overrides) {
    let _ = FACTS.write_now(overrides);
    let _ = CACHE.write_now(overrides);
}

/// Queue both halves for the store's writer thread: the snapshot is taken
/// now, so a later edit always wins (docs/81 A F10).
pub fn save(overrides: &Overrides) {
    FACTS.save(overrides.clone());
    CACHE.save(overrides.clone());
}

/// [`save`] after the store's delay, coalescing a burst of pane changes
/// into one write. Only the cache half changes in such a burst.
pub fn save_soon(overrides: &Overrides) {
    CACHE.save_soon(overrides.clone());
}

/// Land every queued write (quit, window close).
pub fn flush() {
    FACTS.flush();
    CACHE.flush();
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

    fn written(encode: fn(&Overrides) -> Option<Vec<u8>>, overrides: &Overrides) -> serde_json::Map<String, serde_json::Value> {
        serde_json::from_slice(&encode(overrides).expect("encodes")).expect("an object")
    }

    #[test]
    fn an_entry_that_says_nothing_is_not_written() {
        let mut overrides = Overrides::new();
        overrides.insert("empty".into(), SessionMeta::default());
        overrides.insert("named".into(), SessionMeta { name: Some("x".into()), ..SessionMeta::default() });
        let facts = written(encode_facts, &overrides);
        assert_eq!(facts.keys().collect::<Vec<_>>(), vec!["named"]);
        assert!(written(encode_cache, &overrides).is_empty(), "a name is not cache");
    }

    /// docs/82 1.7: the person's facts and the regenerable cache go to
    /// different files, and reading them back merges them.
    #[test]
    fn facts_and_cache_split_and_merge_back() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-split-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let mut overrides = Overrides::new();
        overrides.insert(
            "a".into(),
            SessionMeta {
                name: Some("Kept".into()),
                pinned: true,
                last_summary: Some("did a thing".into()),
                ..SessionMeta::default()
            },
        );
        overrides.insert("b".into(), SessionMeta { archived: true, ..SessionMeta::default() });
        let facts = written(encode_facts, &overrides);
        let cache = written(encode_cache, &overrides);
        assert!(facts["a"].get("lastSummary").is_none() && cache["a"].get("name").is_none());
        assert_eq!(facts["b"]["archived"], serde_json::Value::Bool(true));
        assert!(cache.get("b").is_none());
        write(&overrides);
        assert!(paths().iter().all(|path| path.exists()));
        assert_eq!(read(), overrides);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A store from before the split is read once, and left where it was.
    #[test]
    fn a_pre_split_file_is_read_and_kept() {
        let _guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        let dir = std::env::temp_dir().join(format!("baaz-presplit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let legacy = dir.join("sessions.json");
        std::fs::write(&legacy, r#"{"a":{"name":"Old","lastSummary":"s"}}"#).unwrap();
        let back = read();
        assert_eq!(back["a"].name.as_deref(), Some("Old"));
        assert_eq!(back["a"].last_summary.as_deref(), Some("s"));
        write(&back);
        assert_eq!(read(), back, "the split files read the same");
        assert!(legacy.exists(), "the old file is never deleted");
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        let _ = std::fs::remove_dir_all(&dir);
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
    fn the_right_pane_state_round_trips_as_camel_case() {
        let meta = SessionMeta {
            right: Some(RightState {
                open: true,
                kind: crate::layout::RightKind::Changes,
                files_preview: Some("Cargo.toml".into()),
                files_selected: Some("Cargo.toml".into()),
                files_expanded: vec!["src".into()],
                browser_url: Some("https://example.com".into()),
            }),
            ..SessionMeta::default()
        };
        assert!(written(encode_cache, &Overrides::from([("s".to_owned(), meta.clone())])).contains_key("s"), "a stored pane keeps its entry");
        let text = serde_json::to_string(&meta).unwrap();
        assert!(text.contains("\"right\":{"));
        assert!(text.contains("\"open\":true"));
        assert!(text.contains("\"kind\":\"changes\""));
        assert!(text.contains("\"filesPreview\":\"Cargo.toml\""));
        assert!(text.contains("\"filesSelected\":\"Cargo.toml\""));
        assert!(text.contains("\"filesExpanded\":[\"src\"]"));
        assert!(text.contains("\"browserUrl\":\"https://example.com\""));
        let back: SessionMeta = serde_json::from_str(&text).unwrap();
        assert_eq!(back, meta);
        // Old files without the pane still read, showing the pane closed.
        let old: SessionMeta = serde_json::from_str("{\"hidden\":true}").unwrap();
        assert!(old.right.is_none());
    }

    /// docs/81 A F8: an entry a newer build wrote with a pane kind this
    /// build does not know loses only that field; every other entry and the
    /// rest of that entry load. Before, the whole file read as empty.
    #[test]
    fn an_unknown_pane_kind_loses_only_that_field() {
        let _guard = crate::store::test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-unknown-kind-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");
        std::fs::write(
            &path,
            r#"{"a":{"name":"Kept","pinned":true,"right":{"open":true,"kind":"quantum"}},"b":{"name":"Other"}}"#,
        )
        .unwrap();
        let overrides: Overrides = crate::store::read_json_map(&path);
        assert_eq!(overrides.len(), 2, "no entry is lost");
        let a = &overrides["a"];
        assert_eq!(a.name.as_deref(), Some("Kept"));
        assert!(a.pinned);
        let right = a.right.as_ref().expect("the pane state stays");
        assert!(right.open, "the rest of the pane state stays");
        assert_eq!(right.kind, crate::layout::RightKind::Files, "the unknown kind falls back to the default");
        assert_eq!(overrides["b"].name.as_deref(), Some("Other"));
        assert!(path.exists(), "a file that parses is never set aside");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An entry that does not parse at all drops only itself, and the file
    /// is copied aside first so nothing it said is lost.
    #[test]
    fn a_broken_entry_drops_only_itself() {
        let _guard = crate::store::test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-broken-entry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sessions.json");
        std::fs::write(&path, r#"{"a":{"name":"Kept"},"b":{"name":42}}"#).unwrap();
        let _ = crate::store::take_problems();
        let overrides: Overrides = crate::store::read_json_map(&path);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides["a"].name.as_deref(), Some("Kept"));
        let problems = crate::store::take_problems();
        assert!(
            problems.iter().any(|p| matches!(p, crate::store::StoreProblem::EntriesDropped { count: 1, .. })),
            "{problems:?}"
        );
        let copies = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().starts_with("sessions.json.corrupt-")).count();
        assert_eq!(copies, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_pane_kinds_load_and_resave_as_changes() {
        // B8b: a sessions.json written with the pre-merge `diff`/`git` kinds
        // loads onto the merged Changes view, and re-saving writes
        // `changes` — the legacy kinds never reach the file again.
        for slug in ["diff", "git"] {
            let meta: SessionMeta =
                serde_json::from_str(&format!("{{\"right\":{{\"open\":true,\"kind\":{slug:?}}}}}")).unwrap();
            assert_eq!(
                meta.right.as_ref().map(|right| right.kind),
                Some(crate::layout::RightKind::Changes),
                "{slug} loads as Changes"
            );
            let text = serde_json::to_string(&meta).unwrap();
            assert!(text.contains("\"kind\":\"changes\""), "{slug} re-saves as changes: {text}");
        }
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
