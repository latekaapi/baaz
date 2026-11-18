//! What Baaz knows about a session that the providers have no room for
//! (spec §3.7).
//!
//! * **A name.** No provider lets a client name a session, so `/name`
//!   writes here.
//! * **Hidden, pinned, archived.** Decisions about *this* window's list,
//!   kept here and never sent to a provider.
//! * **Every title** ([`SessionMeta::title`]): the name, the hand-off chain's
//!   title, the generated title, the provider's own title, the first prompt
//!   and the title derived from the transcript, one row's columns.
//! * **The byline, the last error, the right pane's state**: regenerable,
//!   kept so a restart shows what the window showed.
//!
//! Each session's facts are its `session` row in `baaz.db` ([`crate::db`]);
//! the window holds them as [`Overrides`], keyed by session id, and writes a
//! snapshot back ([`save`]): only the rows that changed are written, off the
//! UI thread, in one transaction.

use std::collections::HashMap;

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
    /// A turn finished while nobody read it to the end (task 3.1): what a
    /// row's `DoneUnread` stands on across restarts. In the cache half: it
    /// is rewritten on finished turns, and losing it reads as "nothing
    /// unread", which is harmless.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unread: bool,
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
    /// The provider's own display title from the `OpenSession` /
    /// `ResumeSession` ack, or a listed row's title, when it supplied one:
    /// the ladder's rung under the generated title, above the first prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_title: Option<String>,
    /// The first prompt this app sent (or an import read): the ladder's rung
    /// under the provider's title, so a rejoin never blanks a row back to
    /// [`crate::sidebar::UNNAMED`] before the auto-title lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
}

impl SessionMeta {
    /// The session's title from its own row, best first: the name someone
    /// gave it, the hand-off chain's title, the generated title, the
    /// provider's title, the first prompt sent, the title derived from the
    /// transcript. Blank values are skipped. `None`: nothing to call it by.
    pub fn title(&self) -> Option<&str> {
        fn pick(value: &Option<String>) -> Option<&str> {
            value.as_deref().map(str::trim).filter(|s| !s.is_empty())
        }
        pick(&self.name)
            .or_else(|| pick(&self.handoff_title))
            .or_else(|| pick(&self.generated_title))
            .or_else(|| pick(&self.provider_title))
            .or_else(|| pick(&self.first_prompt))
            .or_else(|| pick(&self.derived_title))
    }
}

/// Every override this window knows, keyed by session id.
pub type Overrides = HashMap<String, SessionMeta>;

/// Whether this run's database holds any session facts.
pub fn stored() -> bool {
    crate::db::read(crate::db::path(), |db| {
        db.conn
            .query_row("SELECT EXISTS(SELECT 1 FROM session WHERE has_meta = 1)", [], |row| row.get::<_, bool>(0))
            .unwrap_or(false)
    })
    .unwrap_or(false)
}

/// Read every session's facts from the database. Blocking: it waits for
/// the writer, behind every write queued before it.
pub fn read() -> Overrides {
    take_soon();
    crate::db::read(crate::db::path(), |db| {
        crate::db::sessions::load_front(&db.conn, &mut db.front).map(|(meta, _)| meta)
    })
    .and_then(|loaded| {
        loaded.map_err(|error| crate::baaz_log!("db: sessions do not read ({error})")).ok()
    })
    .unwrap_or_default()
}

/// Write the facts now: [`save`], then wait for it. Blocking (on the writer,
/// never on SQLite here): the quit flush and tests.
pub fn write(overrides: &Overrides) {
    save(overrides);
    crate::db::flush();
}

/// Queue the facts for the writer: the snapshot is taken now, so a later
/// edit always wins (docs/81 A F10), and it replaces a [`save_soon`]
/// snapshot still waiting.
pub fn save(overrides: &Overrides) {
    SOON.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    queue(crate::db::path(), overrides.clone());
}

fn queue(path: std::path::PathBuf, snapshot: Overrides) {
    crate::db::run(path, move |db| {
        if let Err(error) = crate::db::sessions::save_meta(&mut db.conn, &mut db.front, snapshot) {
            crate::baaz_log!("db: session facts not saved ({error})");
            crate::store::report(crate::store::StoreProblem::WriteFailed { name: "baaz.db".into(), error: error.to_string() });
        }
    });
}

/// The snapshot [`save_soon`] holds until its delay ends.
static SOON: std::sync::Mutex<Option<(std::path::PathBuf, Overrides)>> = std::sync::Mutex::new(None);

/// How long [`save_soon`] waits for more changes.
const SOON_DELAY: std::time::Duration = std::time::Duration::from_millis(750);

/// [`save`] after a short delay, coalescing a burst of pane changes into one
/// write; a change already waiting keeps its earlier due time, so a steady
/// stream still lands every delay. Nothing is written synchronously.
pub fn save_soon(overrides: &Overrides) {
    let first = {
        let mut slot = SOON.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let first = slot.is_none();
        *slot = Some((crate::db::path(), overrides.clone()));
        first
    };
    if first {
        let _ = std::thread::Builder::new().name("sessions:soon".into()).spawn(|| {
            std::thread::sleep(SOON_DELAY);
            take_soon();
        });
    }
}

/// Queue the waiting [`save_soon`] snapshot now, if any.
fn take_soon() {
    let waiting = SOON.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    if let Some((path, snapshot)) = waiting {
        queue(path, snapshot);
    }
}

/// Land every queued write (quit, window close).
pub fn flush() {
    take_soon();
    crate::db::flush();
}

/// What forgetting a session clears (D-8): the files Baaz kept for it, and
/// the provider transcript Baaz's own home holds, which is only ever
/// offered for the Trash.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Forgotten {
    /// Removed with the record: the session's sent pictures (and the index
    /// it had before the database) and its hand-off snapshot.
    pub removed: Vec<std::path::PathBuf>,
    /// The transcript (and its companion folder) in `claude-home` /
    /// `codex-home`. Never removed here: the person is asked, and a yes
    /// moves it to the Trash.
    pub transcript: Vec<std::path::PathBuf>,
}

/// What forgetting `provider`'s session `session_id` would clear under
/// `state_dir`. Pure reads. An imported session's transcript lives in the
/// person's own home (docs/72) and is never offered; a Muse session's lives
/// with Muse.
pub fn forget_plan(state_dir: &std::path::Path, provider: &str, session_id: &str, imported: bool) -> Forgotten {
    let mut removed = crate::sent::files_of(state_dir, session_id);
    let snapshot = state_dir.join("handoff").join(format!("{}.json", crate::sent::file_stem(session_id)));
    if snapshot.is_file() {
        removed.push(snapshot);
    }
    let transcript = if imported { Vec::new() } else { transcript_in_home(state_dir, provider, session_id) };
    Forgotten { removed, transcript }
}

/// The transcript files Baaz's own home for `provider` holds for
/// `session_id`: real files and folders only (a link out of the home is
/// never followed), and only under that home.
fn transcript_in_home(state_dir: &std::path::Path, provider: &str, session_id: &str) -> Vec<std::path::PathBuf> {
    let safe = !session_id.is_empty() && !session_id.contains(['/', '\\']) && session_id != "." && session_id != "..";
    if !safe {
        return Vec::new();
    }
    let real = |path: &std::path::Path| std::fs::symlink_metadata(path).ok().filter(|meta| !meta.file_type().is_symlink());
    let mut out = Vec::new();
    if provider == crate::import::CLAUDE {
        let projects = provider_claude_code::home::home_dir(state_dir).join("projects");
        let Some(true) = real(&projects).map(|meta| meta.is_dir()) else { return out };
        for slug in std::fs::read_dir(&projects).into_iter().flatten().flatten() {
            if !real(&slug.path()).is_some_and(|meta| meta.is_dir()) {
                continue;
            }
            let file = slug.path().join(format!("{session_id}.jsonl"));
            if real(&file).is_some_and(|meta| meta.is_file()) {
                out.push(file);
            }
            let companion = slug.path().join(session_id);
            if real(&companion).is_some_and(|meta| meta.is_dir()) {
                out.push(companion);
            }
        }
    } else if provider == crate::import::CODEX {
        fn walk(dir: &std::path::Path, suffix: &str, depth: usize, out: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let Ok(kind) = entry.file_type() else { continue };
                let path = entry.path();
                if kind.is_dir() && depth > 0 {
                    walk(&path, suffix, depth - 1, out);
                } else if kind.is_file() && path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("rollout-") && name.ends_with(suffix)) {
                    out.push(path);
                }
            }
        }
        let sessions = provider_codex::home::home_dir(state_dir).join("sessions");
        if real(&sessions).is_some_and(|meta| meta.is_dir()) {
            walk(&sessions, &format!("-{session_id}.jsonl"), 3, &mut out);
        }
    }
    out.sort();
    out
}

/// Forget `provider`'s session `session_id` for good (D-8): its row and
/// everything hanging off it leave the database, and its sent files and
/// hand-off snapshot leave the disk. Returns what was cleared; the
/// transcript in it is for the caller to offer for the Trash. Blocking (the
/// file walk and the writer): call it off the UI thread.
pub fn forget(provider: &str, session_id: &str, imported: bool) -> Forgotten {
    let state_dir = crate::store::support_dir();
    let (key, id) = (provider.to_owned(), session_id.to_owned());
    crate::db::read(crate::db::path_in(&state_dir), move |db| {
        if let Err(error) = crate::db::sessions::forget(&mut db.conn, &mut db.front, &key, &id) {
            crate::baaz_log!("db: forgetting a session failed ({error})");
        }
    });
    let plan = forget_plan(&state_dir, provider, session_id, imported);
    for path in &plan.removed {
        let gone = if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
        if let Err(error) = gone {
            crate::baaz_log!("forget: could not remove {}: {error}", path.display());
        }
    }
    plan
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

    /// A temp state dir for one test, the env lock held, restored after.
    struct TempState {
        _guard: std::sync::MutexGuard<'static, ()>,
        old: Option<std::ffi::OsString>,
        dir: std::path::PathBuf,
    }

    impl TempState {
        fn new(tag: &str) -> Self {
            let guard = crate::store::test_env_lock();
            let old = std::env::var_os("BAAZ_STATE_DIR");
            let dir = std::env::temp_dir().join(format!("baaz-sessions-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            std::env::set_var("BAAZ_STATE_DIR", &dir);
            Self { _guard: guard, old, dir }
        }
    }

    impl Drop for TempState {
        fn drop(&mut self) {
            crate::db::close(crate::db::path_in(&self.dir));
            match self.old.take() {
                Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
                None => std::env::remove_var("BAAZ_STATE_DIR"),
            }
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Every fact, the person's and the regenerable, round-trips through
    /// the session's row; an edit writes only that row, and an entry that
    /// leaves the snapshot leaves the table.
    #[test]
    fn facts_round_trip_through_the_session_rows() {
        let state = TempState::new("roundtrip");
        let mut overrides = Overrides::new();
        overrides.insert(
            "a".into(),
            SessionMeta {
                name: Some("Kept".into()),
                pinned: true,
                last_summary: Some("did a thing".into()),
                provider_title: Some("Provider title".into()),
                first_prompt: Some("first".into()),
                right: Some(RightState { open: true, kind: crate::layout::RightKind::Changes, ..RightState::default() }),
                ..SessionMeta::default()
            },
        );
        overrides.insert("b".into(), SessionMeta { archived: true, ..SessionMeta::default() });
        write(&overrides);
        assert!(stored());
        assert_eq!(read(), overrides);
        overrides.remove("b");
        overrides.get_mut("a").unwrap().name = Some("Renamed".into());
        write(&overrides);
        assert_eq!(read(), overrides, "the edit landed and the removed entry left");
        assert!(crate::db::path_in(&state.dir).is_file());
    }

    /// `save_soon` writes nothing at once; a flush lands it.
    #[test]
    fn a_delayed_save_lands_on_flush_and_not_before() {
        let _state = TempState::new("soon");
        let overrides = Overrides::from([("s".to_owned(), SessionMeta { pinned: true, ..SessionMeta::default() })]);
        save_soon(&overrides);
        crate::db::flush();
        assert!(!stored(), "nothing is written synchronously");
        flush();
        assert_eq!(read(), overrides);
    }

    /// docs/82 4.12: the title ladder reads one row — name, chain title,
    /// generated title, the provider's title, the first prompt, the derived
    /// title — and skips blanks.
    #[test]
    fn the_title_ladder_reads_one_row() {
        let full = SessionMeta {
            name: Some("Name".into()),
            handoff_title: Some("Chain".into()),
            generated_title: Some("Generated".into()),
            provider_title: Some("Provider".into()),
            first_prompt: Some("First prompt".into()),
            derived_title: Some("Derived".into()),
            ..SessionMeta::default()
        };
        assert_eq!(full.title(), Some("Name"));
        let rungs = ["Chain", "Generated", "Provider", "First prompt", "Derived"];
        let mut meta = SessionMeta { name: Some("   ".into()), ..full.clone() };
        for (rung, expected) in rungs.iter().enumerate() {
            assert_eq!(meta.title(), Some(*expected), "rung {rung}");
            match rung {
                0 => meta.handoff_title = None,
                1 => meta.generated_title = None,
                2 => meta.provider_title = None,
                3 => meta.first_prompt = None,
                _ => meta.derived_title = None,
            }
        }
        assert_eq!(meta.title(), None);
        // And the whole ladder survives the row's round trip.
        let _state = TempState::new("ladder");
        write(&Overrides::from([("t".to_owned(), full.clone())]));
        assert_eq!(read()["t"].title(), Some("Name"));
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

    #[test]
    fn legacy_pane_kinds_load_and_resave_as_changes() {
        // B8b: session facts written with the pre-merge `diff`/`git` kinds
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
