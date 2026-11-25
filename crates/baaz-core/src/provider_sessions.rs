//! The local record of provider-lane sessions (W5).
//!
//! A provider's own list is not enough to find a session again after a
//! restart (Claude Code and Codex have none Baaz reads), so Baaz keeps a
//! record of every session it opened or imported: which provider serves it,
//! where it ran, when it last moved and how many turns it has. Everything
//! else about the session — its titles, project, hand-off links, flags —
//! lives in [`crate::sessions::SessionMeta`], the same row's other columns,
//! so a session's facts have one home.
//!
//! Each record is the session's row in `baaz.db` ([`crate::db`]); the
//! window holds them as a [`ProviderSessionStore`] and writes a snapshot
//! back ([`save`]).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::sessions::{Overrides, SessionMeta};

/// One provider-lane session Baaz has opened.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionRecord {
    /// The wire id that serves this session (`"claude-code"`, `"codex"`).
    pub provider: String,
    /// The session id the `OpenSession` ack minted.
    pub session_id: String,
    /// The workspace root it runs in: what project resolution reads, so a
    /// reopened session groups where it started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    /// When the session opened, as Unix milliseconds.
    pub created_ms: i64,
    /// When the session last moved (open, settled turn), as Unix
    /// milliseconds: what the sidebar sorts on.
    pub updated_ms: i64,
    /// Settled turns folded in this app, live and replayed: what the row's
    /// turn count and the empty filter read.
    #[serde(default)]
    pub turns: u64,
    /// Where the record came from when Baaz did not start the session:
    /// [`ORIGIN_IMPORTED`] for a Claude Code / Codex session imported from
    /// the owner's own homes (docs/72). An imported session's transcript
    /// stays where its CLI wrote it, so it always resumes pinned on the
    /// owner's home and is never moved (`session_migration::gate_resume`,
    /// `plan_migration`). `None` for every session Baaz started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// [`ProviderSessionRecord::origin`] of an imported session.
pub const ORIGIN_IMPORTED: &str = "imported";

impl ProviderSessionRecord {
    /// Whether this record was imported from the owner's homes (docs/72).
    pub fn is_imported(&self) -> bool {
        self.origin.as_deref() == Some(ORIGIN_IMPORTED)
    }
}

/// Every provider session this window knows, keyed by session id.
pub type ProviderSessionStore = HashMap<String, ProviderSessionRecord>;

/// Read every record from the database. Blocking: it waits for the writer,
/// behind every write queued before it.
///
/// A capture (`BAAZ_DETERMINISTIC=1`, the visual probe) reads none, the same
/// rule the keymap applies to the user file: a capture must not paint the
/// owner's real sessions into the sidebar, or every baseline depends on
/// whatever that machine happened to run. `BAAZ_MIGRATION_FIXTURE` lifts that
/// rule for the migration probe entry only (`scripts/uiprobe.py`'s
/// `migrate-sessions`): the records it reads are the fixture its temp state
/// dir was seeded with, never the owner's.
pub fn read() -> ProviderSessionStore {
    if crate::runtime::state_isolated() && std::env::var_os("BAAZ_MIGRATION_FIXTURE").is_none() {
        return ProviderSessionStore::new();
    }
    crate::db::read(crate::db::path(), |db| {
        crate::db::sessions::load_front(&db.conn, &mut db.front).map(|(_, records)| records)
    })
    .and_then(|loaded| loaded.map_err(|error| crate::baaz_log!("db: records do not read ({error})")).ok())
    .unwrap_or_default()
}

/// Write the records now: [`save`], then wait for it. Blocking (on the
/// writer): the quit flush and tests.
pub fn write(store: &ProviderSessionStore) {
    save(store);
    crate::db::flush();
}

/// Queue the records for the writer: the snapshot is taken now, so a later
/// one always wins. A capture never writes: its sessions are scripted, and
/// they must not land in a store.
pub fn save(store: &ProviderSessionStore) {
    if crate::runtime::state_isolated() {
        return;
    }
    let snapshot = store.clone();
    crate::db::run(crate::db::path(), move |db| {
        if let Err(error) = crate::db::sessions::save_records(&mut db.conn, &mut db.front, snapshot) {
            crate::baaz_log!("db: session records not saved ({error})");
            crate::store::report(crate::store::StoreProblem::WriteFailed { name: "baaz.db".into(), error: error.to_string() });
        }
    });
}

/// Land every queued write (quit, window close).
pub fn flush() {
    crate::db::flush();
}

fn trimmed(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Insert or refresh the record of a session a provider lists itself (a
/// Muse session), from its newest list row: the first open in Baaz of a
/// session made elsewhere writes it (docs/70 §3), and every later reopen
/// refreshes the turn count and title first — the CLI may have added
/// turns since — so the "no turns, open fresh" rule never drops a real
/// session. Facts already held and not said by the row are kept; the turn
/// count never goes down. The title, first prompt and project land on the
/// session's own facts (`overrides`). Returns whether anything changed.
#[allow(clippy::too_many_arguments)]
pub fn upsert_listed(
    store: &mut ProviderSessionStore,
    overrides: &mut Overrides,
    provider: &str,
    session_id: &str,
    workspace: Option<String>,
    project: Option<String>,
    title: Option<String>,
    first_prompt: Option<String>,
    turns: u64,
    created_ms: i64,
    updated_ms: i64,
) -> bool {
    let (title, first_prompt) = (trimmed(title), trimmed(first_prompt).map(|p| crate::sidebar::one_line(&p)));
    let changed = match store.get_mut(session_id) {
        Some(record) => {
            let before = record.clone();
            record.provider = provider.to_owned();
            if record.workspace.is_none() {
                record.workspace = workspace;
            }
            if turns > record.turns {
                record.turns = turns;
                record.updated_ms = record.updated_ms.max(updated_ms);
            }
            *record != before
        }
        None => {
            store.insert(
                session_id.to_owned(),
                ProviderSessionRecord {
                    provider: provider.to_owned(),
                    session_id: session_id.to_owned(),
                    workspace,
                    created_ms,
                    updated_ms,
                    turns,
                    origin: None,
                },
            );
            true
        }
    };
    let meta_before = overrides.get(session_id).cloned();
    let meta = overrides.entry(session_id.to_owned()).or_default();
    if meta.project.is_none() {
        meta.project = project;
    }
    if title.is_some() {
        meta.provider_title = title;
    }
    if meta.first_prompt.is_none() {
        meta.first_prompt = first_prompt;
    }
    // A row that says nothing about the session leaves no empty entry.
    if meta_before.is_none() && *meta == SessionMeta::default() {
        overrides.remove(session_id);
    }
    changed || overrides.get(session_id) != meta_before.as_ref()
}

/// Insert the row an `OpenSession` ack minted, or refresh the routing facts
/// when the session is already known (a reopen lands here too). The
/// project and the ack's title land on the session's own facts.
pub fn upsert_open(
    store: &mut ProviderSessionStore,
    overrides: &mut Overrides,
    provider: &str,
    session_id: &str,
    workspace: Option<String>,
    project: Option<String>,
    title: Option<String>,
) {
    let now = crate::usage::now_ms();
    store
        .entry(session_id.to_owned())
        .and_modify(|record| {
            record.provider = provider.to_owned();
            if workspace.is_some() {
                record.workspace = workspace.clone();
            }
            // A reopen is not activity: opening a row from the sidebar
            // must not move it to "now" and to the top of its group.
        })
        .or_insert_with(|| ProviderSessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            workspace,
            created_ms: now,
            updated_ms: now,
            turns: 0,
            origin: None,
        });
    if project.is_some() || title.is_some() {
        let meta = overrides.entry(session_id.to_owned()).or_default();
        if project.is_some() {
            meta.project = project;
        }
        if title.is_some() {
            meta.provider_title = title;
        }
    }
}

/// The session moved without settling a turn (an admission): the row
/// moves to now so recency sorts with the live session. Returns whether
/// the record exists.
pub fn touch(store: &mut ProviderSessionStore, session_id: &str) -> bool {
    let Some(record) = store.get_mut(session_id) else { return false };
    record.updated_ms = crate::usage::now_ms();
    true
}

/// One turn settled, counted as exchanges: the row moves to now and holds
/// the folded assistant-turn count, never less than it already held. A
/// reopen replays the same `TurnFinished`s the live session already
/// counted, so a blind increment read "2 turns" for one user+assistant
/// exchange — the fold's own count is idempotent across the replay.
/// Returns whether the record exists — a turn for an unknown session
/// names nothing, so the caller records nothing.
///
/// `live` is whether this settle ends a turn the window admitted. A replay
/// (a reopen folding history) may raise the count — the record's own may
/// be short: an import counts lines, a Muse list counts its own way — but
/// never moves the time: opening a session is not activity (QA
/// 2026-10-30, a 22 Oct row read "now" after an open).
pub fn note_settled_turn_counted(
    store: &mut ProviderSessionStore,
    session_id: &str,
    exchanges: u64,
    live: bool,
) -> bool {
    let Some(record) = store.get_mut(session_id) else { return false };
    // Only a turn the record has not counted yet is new activity: the
    // resume replays every settled turn, and stamping now on each replay
    // moved a merely-opened row to the top of the sidebar.
    if exchanges > record.turns {
        record.turns = exchanges;
        if live {
            record.updated_ms = crate::usage::now_ms();
        }
    }
    true
}

/// Remember the first prompt a recorded session sent, once: later turns
/// never overwrite it, so the title keeps the session's own words. A
/// session with no record notes nothing. Returns whether it changed.
pub fn note_first_prompt(
    store: &ProviderSessionStore,
    overrides: &mut Overrides,
    session_id: &str,
    prompt: &str,
) -> bool {
    let prompt = prompt.trim();
    if prompt.is_empty() || !store.contains_key(session_id) {
        return false;
    }
    let meta = overrides.entry(session_id.to_owned()).or_default();
    // A handoff pack is never a session's own words — but only a handoff
    // destination ever has a pack turn, so the text match applies solely
    // there. Any other session keeps its own words, pack-shaped or not
    // (Y2a3).
    if meta.handoff_from.is_some() && crate::sidebar::is_pack_text(prompt) {
        return false;
    }
    if meta.first_prompt.is_none() {
        meta.first_prompt = Some(crate::sidebar::one_line(prompt));
        return true;
    }
    false
}

/// Forget a provider session: the row leaves the list and the store.
/// Dropping the session's views (which shuts the child down) is the
/// caller's job — this only forgets the record.
pub fn remove(store: &mut ProviderSessionStore, session_id: &str) -> bool {
    store.remove(session_id).is_some()
}

/// A handoff activated between two provider-lane sessions: the source
/// points at the destination, and the destination points back at the
/// source with its wire provider id (what the sidebar row's "from
/// <Provider>" byline reads). Sessions with no record are skipped: a muse
/// session on either side is linked by the caller's own path.
pub fn note_handoff(
    store: &ProviderSessionStore,
    overrides: &mut Overrides,
    source: &str,
    source_provider: &str,
    dest: &str,
) {
    if store.contains_key(source) {
        overrides.entry(source.to_owned()).or_default().handoff_to = Some(dest.to_owned());
    }
    if store.contains_key(dest) {
        let meta = overrides.entry(dest.to_owned()).or_default();
        meta.handoff_from = Some(source.to_owned());
        meta.handoff_from_provider = Some(source_provider.to_owned());
    }
}

/// Undo [`note_handoff`]: a Failed or Cancelled handoff unlinks the two
/// sessions, so the source lists on its own again and the destination
/// stands alone. Only the link fields leave — titles, projects and row
/// state the hop carried stay where they landed.
pub fn clear_handoff(overrides: &mut Overrides, source: &str, dest: &str) {
    if let Some(meta) = overrides.get_mut(source) {
        if meta.handoff_to.as_deref() == Some(dest) {
            meta.handoff_to = None;
        }
    }
    if let Some(meta) = overrides.get_mut(dest) {
        if meta.handoff_from.as_deref() == Some(source) {
            meta.handoff_from = None;
            meta.handoff_from_provider = None;
            meta.handoff_title = None;
        }
    }
}

/// The `ResumeSession` command that reopens `record`: a full resume, not a
/// metadata peek, unless the provider pages its history — the adapter replays the transcript's deltas (Claude Code
/// replays its `~/.claude` jsonl, Codex its thread), which the lane folds
/// into the view. The caller mints `request_id` (a UUIDv7 command id, like
/// every seam command) so this module never reaches past the provider seam
/// for it. `mode` is the approval mode the reopened session runs in (see
/// [`provider::Command::ResumeSession`]); the folder is the record's own,
/// which an agent that re-opens a session only in its folder needs; `env`
/// is what its child is started with (a worktree session's variables,
/// docs/82 6.2).
///
/// `from_store`: Baaz's own transcript store holds a fresh copy of the
/// session (docs/85 11.1), so the provider sends no history either.
pub fn resume_command(
    record: &ProviderSessionRecord,
    request_id: &str,
    mode: Option<aui_protocol::PermissionMode>,
    env: Vec<(String, String)>,
    from_store: bool,
) -> provider::Command {
    // A provider that pages its history (Muse) re-attaches without it —
    // the view pages it in after the ack — and starts in the host's
    // posture unless a mode was picked, like its open.
    let pages = crate::providers::ProviderId::parse_known(&record.provider)
        .is_some_and(|id| id.has(provider::ProviderTrait::PagesHistory));
    provider::Command::ResumeSession {
        request_id: request_id.to_owned(),
        session_id: record.session_id.clone(),
        cursor: None,
        metadata_only: pages || from_store,
        mode,
        workspace: record.workspace.clone(),
        env,
    }
}

/// Send [`resume_command`] and return the resumed session id.
///
/// `Ack::Session` is the only success shape: anything else is a refusal
/// with its own reason, never a silent fallback onto a fresh session.
pub fn send_resume(
    provider: &provider::Provider,
    record: &ProviderSessionRecord,
    request_id: &str,
    mode: Option<aui_protocol::PermissionMode>,
    env: Vec<(String, String)>,
    from_store: bool,
) -> Result<String, provider::ProviderError> {
    match provider.send(resume_command(record, request_id, mode, env, from_store))? {
        provider::Ack::Session { session_id, .. } => Ok(session_id),
        other => Err(provider::ProviderError::Rejected {
            reason: format!("ResumeSession answered {other:?} instead of a session"),
        }),
    }
}

/// Whether `turn` is a compaction (docs/85 10.4): an assistant turn whose
/// only rows are "Context compacted" markers — what `/compact` folds to on
/// every provider. Not a person's turn: it is ledgered (as a compaction)
/// but titles, bylines, turn counts and unread marks never read it.
pub fn is_compaction_turn(turn: &aui_protocol::Turn) -> bool {
    match turn {
        aui_protocol::Turn::Assistant { blocks, .. } => {
            !blocks.is_empty()
                && blocks
                    .iter()
                    .all(|block| matches!(block, aui_protocol::Block::Marker { kind: aui_protocol::MarkerKind::ContextCompacted, .. }))
        }
        _ => false,
    }
}

/// The ledger row one settled provider turn writes: the same
/// [`crate::usage::row_from_finished`] a muse turn writes, tagged with the
/// serving provider. The `(session_id, turn_id)` key deduplicates replay:
/// reopening replays the same finished turns and the second write is a
/// no-op, so history never double-counts.
pub fn ledger_row(
    session_id: &str,
    provider: &str,
    view_cursor: &str,
    turn_id: &str,
    meta: &aui_protocol::TurnMeta,
) -> crate::usage::UsageRow {
    crate::usage::row_from_finished(session_id, view_cursor, turn_id, crate::usage::now_ms(), meta)
        .with_provider(provider)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Holds [`crate::store::test_env_lock`] while a test points
    /// `BAAZ_STATE_DIR` at a temp dir, restoring it after: the variable is
    /// process-global, so two such tests at once would read each other's
    /// state. Same shape as the keymap tests' `EnvLock`.
    struct EnvLock {
        _guard: std::sync::MutexGuard<'static, ()>,
        state_dir: Option<std::ffi::OsString>,
    }

    impl EnvLock {
        fn hold(name: &str) -> Self {
            let locked = Self {
                _guard: crate::store::test_env_lock(),
                state_dir: std::env::var_os("BAAZ_STATE_DIR"),
            };
            let dir = std::env::temp_dir().join(format!("baaz-provider-sessions-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).expect("provider sessions test state dir");
            std::env::set_var("BAAZ_STATE_DIR", &dir);
            locked
        }
    }

    impl Drop for EnvLock {
        fn drop(&mut self) {
            match &self.state_dir {
                Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
                None => std::env::remove_var("BAAZ_STATE_DIR"),
            }
        }
    }

    /// The store under a temp dir, with the env lock held.
    fn temp_store(name: &str) -> EnvLock {
        EnvLock::hold(name)
    }

    fn open_sample(store: &mut ProviderSessionStore, facts: &mut Overrides) {
        upsert_open(
            store,
            facts,
            "claude-code",
            "s-1",
            Some("/w/shop".into()),
            Some("p-shop".into()),
            Some("Fix the header".into()),
        );
    }

    #[test]
    fn an_open_round_trips_through_the_database() {
        let _env = temp_store("roundtrip");
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        write(&store);
        crate::sessions::write(&facts);
        let back = read();
        assert_eq!(back, store, "what was written reads back identical");
        let record = &back["s-1"];
        assert_eq!(record.provider, "claude-code");
        assert_eq!(record.workspace.as_deref(), Some("/w/shop"));
        assert_eq!(record.turns, 0);
        let facts = crate::sessions::read();
        assert_eq!(facts["s-1"].project.as_deref(), Some("p-shop"), "the project is the row's own fact");
        assert_eq!(facts["s-1"].provider_title.as_deref(), Some("Fix the header"));
        assert_eq!(facts["s-1"].title(), Some("Fix the header"), "and so is the title");
    }

    #[test]
    fn a_settled_turn_moves_the_row_and_counts() {
        let _env = temp_store("turn");
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        let before = store["s-1"].updated_ms;
        assert!(note_settled_turn_counted(&mut store, "s-1", 1, true));
        assert_eq!(store["s-1"].turns, 1);
        assert!(store["s-1"].updated_ms >= before);
        write(&store);
        assert_eq!(read()["s-1"].turns, 1, "the count survives a restart");
    }

    #[test]
    fn a_turn_for_an_unknown_session_records_nothing() {
        let mut store = ProviderSessionStore::new();
        assert!(!note_settled_turn_counted(&mut store, "s-gone", 1, true));
        assert!(store.is_empty());
    }

    #[test]
    fn replayed_settles_do_not_double_count_exchanges() {
        // One user+assistant exchange settles once live, then replays once
        // on reopen: the row must read 1 turn, not 2.
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        assert!(note_settled_turn_counted(&mut store, "s-1", 1, true));
        assert_eq!(store["s-1"].turns, 1);
        assert!(note_settled_turn_counted(&mut store, "s-1", 1, false), "the replayed settle");
        assert_eq!(store["s-1"].turns, 1, "a replay never double-counts");
        assert!(note_settled_turn_counted(&mut store, "s-1", 2, true), "a second live exchange");
        assert_eq!(store["s-1"].turns, 2);
    }

    #[test]
    fn the_first_prompt_sticks_and_later_turns_do_not_overwrite_it() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        assert!(note_first_prompt(&store, &mut facts, "s-1", "  Fix the header\nsecond line  "));
        assert_eq!(
            facts["s-1"].first_prompt.as_deref(),
            Some("Fix the header second line"),
            "stored the row's own way: one collapsed line"
        );
        assert!(!note_first_prompt(&store, &mut facts, "s-1", "Something else"));
        assert_eq!(
            facts["s-1"].first_prompt.as_deref(),
            Some("Fix the header second line"),
            "the session keeps its own first words"
        );
        assert!(!note_first_prompt(&store, &mut facts, "s-gone", "words"), "no record, no note");
        assert!(!facts.contains_key("s-gone"));
    }

    /// A Failed or Cancelled handoff unlinks both halves, and only the
    /// halves it linked: another chain's links are never touched.
    #[test]
    fn a_rolled_back_handoff_unlinks_only_its_own_halves() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        upsert_open(&mut store, &mut facts, "codex", "s-2", None, None, None);
        upsert_open(&mut store, &mut facts, "codex", "s-3", None, None, None);
        note_handoff(&store, &mut facts, "s-1", "claude-code", "s-2");
        note_handoff(&store, &mut facts, "s-2", "codex", "s-3");
        facts.get_mut("s-2").expect("dest").handoff_title = Some("Chain".into());
        clear_handoff(&mut facts, "s-1", "s-2");
        assert!(facts["s-1"].handoff_to.is_none(), "the source lists on its own again");
        assert!(facts["s-2"].handoff_from.is_none(), "the destination stands alone");
        assert!(facts["s-2"].handoff_from_provider.is_none());
        assert!(facts["s-2"].handoff_title.is_none(), "the carried chain title leaves with the link");
        assert_eq!(facts["s-2"].handoff_to.as_deref(), Some("s-3"), "the next hop's link is untouched");
        // Clearing a pair that was never linked changes nothing.
        clear_handoff(&mut facts, "s-1", "s-3");
        assert_eq!(facts["s-2"].handoff_to.as_deref(), Some("s-3"));
        // A session with no record is linked by its own path, not here.
        note_handoff(&store, &mut facts, "s-muse", "muse", "s-9");
        assert!(!facts.contains_key("s-muse") && !facts.contains_key("s-9"));
    }

    /// Y2a: a handoff pack is never a destination's own words — neither
    /// the full pack nor its bubble summary becomes the first prompt.
    /// Y2a3: the skip is gated on the destination — any other session
    /// keeps pack-shaped words like any other first prompt.
    #[test]
    fn the_handoff_pack_is_never_a_first_prompt() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        facts.get_mut("s-1").expect("sample").handoff_from = Some("s-0".into());
        note_first_prompt(&store, &mut facts, "s-1", "Continuing a session handed off from Muse: goal words");
        assert!(facts["s-1"].first_prompt.is_none(), "the pack turn records nothing");
        note_first_prompt(
            &store,
            &mut facts,
            "s-1",
            "Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)",
        );
        assert!(facts["s-1"].first_prompt.is_none(), "the bubble records nothing either");
    }

    /// Y2a3: a session that is no handoff destination has no pack turn —
    /// its own words land even when they start with "Handed off from ".
    #[test]
    fn a_non_destinations_pack_shaped_words_are_its_first_prompt() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        note_first_prompt(
            &store,
            &mut facts,
            "s-1",
            "Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)",
        );
        assert_eq!(
            facts["s-1"].first_prompt.as_deref(),
            Some("Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)"),
            "a non-destination keeps its own words, pack-shaped or not"
        );
    }

    /// A listed row's facts land on the session's own facts; a row that says
    /// nothing leaves no empty entry.
    #[test]
    fn a_listed_row_lands_its_title_on_the_sessions_facts() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        assert!(upsert_listed(&mut store, &mut facts, "muse", "m-1", None, None, Some(" Listed ".into()), Some("hi".into()), 2, 5, 6));
        assert_eq!(facts["m-1"].provider_title.as_deref(), Some("Listed"));
        assert_eq!(facts["m-1"].first_prompt.as_deref(), Some("hi"));
        assert!(!upsert_listed(&mut store, &mut facts, "muse", "m-1", None, None, Some("Listed".into()), None, 2, 5, 6), "nothing new");
        assert!(upsert_listed(&mut store, &mut facts, "muse", "m-2", None, None, None, None, 0, 5, 6), "a new record");
        assert!(!facts.contains_key("m-2"), "no empty facts");
    }

    #[test]
    fn removing_forgets_the_session() {
        let _env = temp_store("remove");
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        write(&store);
        assert!(remove(&mut store, "s-1"));
        assert!(!remove(&mut store, "s-1"), "a second delete reports nothing");
        write(&store);
        assert!(read().is_empty(), "the database forgets it too");
    }

    #[test]
    fn reopening_keeps_the_row_and_its_recency() {
        let (mut store, mut facts) = (ProviderSessionStore::new(), Overrides::new());
        open_sample(&mut store, &mut facts);
        note_settled_turn_counted(&mut store, "s-1", 1, true);
        let created = store["s-1"].created_ms;
        // An old row: a reopen and the resume's replayed settle must leave
        // it where it sorts, not stamp it "now" and move it to the top.
        store.get_mut("s-1").unwrap().updated_ms = 1_000;
        upsert_open(&mut store, &mut facts, "claude-code", "s-1", None, None, None);
        assert!(note_settled_turn_counted(&mut store, "s-1", 1, false), "the replayed settle");
        assert_eq!(store["s-1"].updated_ms, 1_000, "opening a session is not activity");
        assert!(note_settled_turn_counted(&mut store, "s-1", 3, false), "a replay that folds more than was counted");
        assert_eq!(store["s-1"].turns, 3, "the replay corrects the count");
        assert_eq!(store["s-1"].updated_ms, 1_000, "but a replay never moves the time");
        assert!(note_settled_turn_counted(&mut store, "s-1", 4, true), "a new live exchange");
        assert!(store["s-1"].updated_ms > 1_000, "a new turn is activity");
        assert_eq!(store["s-1"].created_ms, created, "a reopen is not a new session");
        assert_eq!(store["s-1"].turns, 4, "its history survives");
        assert_eq!(store["s-1"].workspace.as_deref(), Some("/w/shop"), "unset stays, not blanked");
        assert_eq!(facts["s-1"].project.as_deref(), Some("p-shop"), "unset stays, not blanked");
    }

    #[test]
    fn an_empty_database_reads_as_no_sessions() {
        let _env = temp_store("missing");
        assert!(read().is_empty());
    }

    /// A recording adapter: the reopen path's double, in the same style as
    /// the lane's own test double — the provider crate is untouched. The
    /// handle outlives the move into [`provider::Provider`], so the test
    /// still sees what the resume sent.
    #[derive(Clone)]
    struct RecordingAdapter {
        commands: std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
    }

    impl RecordingAdapter {
        fn new() -> (Self, std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>) {
            let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            (Self { commands: commands.clone() }, commands)
        }
    }

    impl provider::ProviderAdapter for RecordingAdapter {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Codex
        }

        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Codex,
                agent_name: "recording".into(),
                agent_version: "0.0.0".into(),
            })
        }

        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable {
                reason: "the recording double resumes sessions only".into(),
            };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::SubmitTurn, native.clone()),
                (Capability::ForkSession, off()),
                (Capability::CompactSession, off()),
                (Capability::SessionConfig, off()),
                (Capability::SessionShell, off()),
                (Capability::SteerTurn, off()),
                (Capability::TurnControl, off()),
                (Capability::ModelCatalog, off()),
                (Capability::Approvals, off()),
                (Capability::Questions, off()),
                (Capability::Transcript, off()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
                (Capability::ImageInput, native.clone()),
            ])
        }

        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            self.commands.lock().expect("commands").push(command.clone());
            match command {
                provider::Command::ResumeSession { session_id, .. } => {
                    Ok(provider::Ack::Session { session_id, title: Some("Old work".into()) })
                }
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "recording doubles only resume sessions",
                )),
            }
        }

        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            let (_tx, rx) = crossbeam_channel::unbounded();
            rx
        }

        fn shutdown(&mut self) {}
    }

    #[test]
    fn reopening_sends_resume_session_for_the_stored_id() {
        let mut store = ProviderSessionStore::new();
        upsert_open(&mut store, &mut Overrides::new(), "codex", "s-old", Some("/w".into()), None, None);
        let (adapter, commands) = RecordingAdapter::new();
        let provider = provider::Provider::new(adapter);
        let resumed = send_resume(&provider, &store["s-old"], "r-1", None, Vec::new(), false).expect("resume lands");
        assert_eq!(resumed, "s-old");
        let sent = commands.lock().expect("commands").clone();
        assert_eq!(sent.len(), 1, "exactly one command leaves the lane, drew {sent:?}");
        match &sent[0] {
            provider::Command::ResumeSession { session_id, cursor, metadata_only, .. } => {
                assert_eq!(session_id, "s-old");
                assert_eq!(*cursor, None);
                assert!(!metadata_only);
            }
            other => panic!("reopen must travel as ResumeSession, travelled as {other:?}"),
        }
    }

    #[test]
    fn resume_command_names_the_session_and_asks_for_the_transcript() {
        let mut store = ProviderSessionStore::new();
        upsert_open(&mut store, &mut Overrides::new(), "codex", "s-old", Some("/w".into()), None, None);
        match resume_command(&store["s-old"], "r-1", Some(aui_protocol::PermissionMode::AllowAll), Vec::new(), false) {
            provider::Command::ResumeSession { session_id, cursor, metadata_only, mode, .. } => {
                assert_eq!(session_id, "s-old");
                assert_eq!(mode, Some(aui_protocol::PermissionMode::AllowAll), "the mode rides along");
                assert_eq!(cursor, None, "resume from the start: no view holds a cursor yet");
                assert!(!metadata_only, "the view shows the transcript, not a preview");
            }
            other => panic!("reopen must travel as ResumeSession, travelled as {other:?}"),
        }
    }

    #[test]
    fn a_refused_resume_is_an_error_never_a_session() {
        struct Refusing;
        impl provider::ProviderAdapter for Refusing {
            fn id(&self) -> provider::ProviderId {
                aui_protocol::Provider::Codex
            }

            fn connect(&mut self, _c: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
                Err(provider::ProviderError::Unavailable { reason: "offline".into() })
            }

            fn capabilities(&self) -> provider::CapabilitySet {
                use provider::{Capability, CapabilityState};
                let off = || CapabilityState::Unavailable { reason: "no".into() };
                provider::CapabilitySet::new([
                    (Capability::SessionLifecycle, off()),
                    (Capability::SubmitTurn, off()),
                    (Capability::ForkSession, off()),
                    (Capability::CompactSession, off()),
                    (Capability::SessionConfig, off()),
                    (Capability::SessionShell, off()),
                    (Capability::SteerTurn, off()),
                    (Capability::TurnControl, off()),
                    (Capability::ModelCatalog, off()),
                    (Capability::Approvals, off()),
                    (Capability::Questions, off()),
                    (Capability::Transcript, off()),
                    (Capability::Account, off()),
                    (Capability::ClientTools, off()),
                    (Capability::ReasoningTraces, off()),
                    (Capability::SubagentTurns, off()),
                    (Capability::ImageInput, off()),
                ])
            }

            fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
                Err(provider::ProviderError::unsupported(command.capability(), "no"))
            }

            fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
                let (_tx, rx) = crossbeam_channel::unbounded();
                rx
            }

            fn shutdown(&mut self) {}
        }
        let mut store = ProviderSessionStore::new();
        upsert_open(&mut store, &mut Overrides::new(), "codex", "s-old", None, None, None);
        let provider = provider::Provider::new(Refusing);
        assert!(send_resume(&provider, &store["s-old"], "r-1", None, Vec::new(), false).is_err());
    }
}
