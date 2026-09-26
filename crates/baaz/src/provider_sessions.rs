//! The local record of provider-lane sessions (W5).
//!
//! `session/list` only knows muse sessions, so a Claude Code / Codex
//! session that survives a restart needs Baaz's own record: which provider
//! serves it, where it ran, and when it last moved. Titles, bylines,
//! archive and rename already have a home — [`crate::sessions`] keyed by
//! session id, written by the same writers that serve muse sessions — so
//! this file keeps only what that store cannot say: the owning provider,
//! the workspace root (for project grouping), and the recency counters
//! the sidebar sorts on.
//!
//! The file is `~/Library/Application Support/baaz/provider-sessions.json`,
//! written atomically through [`crate::store`], and every read is
//! best-effort like [`crate::sessions`]: a missing or unparseable file is
//! an empty map, which loses a row and never a session (the backend still
//! holds the transcript; reopening re-creates the row).

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

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
    /// The project adoption it groups under, when it started in one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    /// When the session opened, as Unix milliseconds.
    pub created_ms: i64,
    /// When the session last moved (open, settled turn), as Unix
    /// milliseconds: what the sidebar sorts on.
    pub updated_ms: i64,
    /// Settled turns folded in this app, live and replayed: what the row's
    /// turn count and the empty filter read.
    #[serde(default)]
    pub turns: u64,
    /// The provider's own display title from the `OpenSession` /
    /// `ResumeSession` ack, when it supplied one: the label ladder's
    /// fallback under a generated title, above the first prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The first prompt this app sent: the label ladder's fallback under
    /// the ack title, so a rejoin never blanks a row back to
    /// [`crate::sidebar::UNNAMED`] before the auto-title lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
}

/// Every provider session this window knows, keyed by session id.
pub type ProviderSessionStore = HashMap<String, ProviderSessionRecord>;

/// `~/Library/Application Support/baaz/provider-sessions.json`.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("provider-sessions.json")
}

/// Read the store. Blocking; call it off the UI thread.
pub fn read() -> ProviderSessionStore {
    crate::store::read_json(&path())
}

/// Write the store, atomically. Best-effort like [`crate::sessions::write`]:
/// a store that cannot be written loses a row, never a session.
pub fn write(store: &ProviderSessionStore) {
    if let Ok(text) = serde_json::to_vec_pretty(store) {
        let _ = crate::store::write_atomic(&path(), &text);
    }
}

/// Insert the row an `OpenSession` ack minted, or refresh the routing facts
/// when the session is already known (a reopen lands here too).
pub fn upsert_open(
    store: &mut ProviderSessionStore,
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
            if project.is_some() {
                record.project = project.clone();
            }
            if title.is_some() {
                record.title = title.clone();
            }
            record.updated_ms = now;
        })
        .or_insert_with(|| ProviderSessionRecord {
            provider: provider.to_owned(),
            session_id: session_id.to_owned(),
            workspace,
            project,
            created_ms: now,
            updated_ms: now,
            turns: 0,
            title,
            first_prompt: None,
        });
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
pub fn note_settled_turn_counted(
    store: &mut ProviderSessionStore,
    session_id: &str,
    exchanges: u64,
) -> bool {
    let Some(record) = store.get_mut(session_id) else { return false };
    record.turns = record.turns.max(exchanges);
    record.updated_ms = crate::usage::now_ms();
    true
}

/// Remember the first prompt a session sent, once: later turns never
/// overwrite it, so the label ladder keeps the session's own words.
pub fn note_first_prompt(store: &mut ProviderSessionStore, session_id: &str, prompt: &str) {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return;
    }
    if let Some(record) = store.get_mut(session_id) {
        if record.first_prompt.is_none() {
            record.first_prompt = Some(crate::sidebar::one_line(prompt));
        }
    }
}

/// Forget a provider session: the row leaves the list and the store.
/// Dropping the session's views (which shuts the child down) is the
/// caller's job — this only forgets the record.
pub fn remove(store: &mut ProviderSessionStore, session_id: &str) -> bool {
    store.remove(session_id).is_some()
}

/// The `ResumeSession` command that reopens `record`: a full resume, not a
/// metadata peek — the adapter replays the transcript's deltas (Claude Code
/// replays its `~/.claude` jsonl, Codex its thread), which the lane folds
/// into the view. The caller mints `request_id` (a UUIDv7 command id, like
/// every seam command) so this module never reaches past the provider seam
/// for it.
pub fn resume_command(record: &ProviderSessionRecord, request_id: &str) -> provider::Command {
    provider::Command::ResumeSession {
        request_id: request_id.to_owned(),
        session_id: record.session_id.clone(),
        cursor: None,
        metadata_only: false,
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
) -> Result<String, provider::ProviderError> {
    match provider.send(resume_command(record, request_id))? {
        provider::Ack::Session { session_id, .. } => Ok(session_id),
        other => Err(provider::ProviderError::Rejected {
            reason: format!("ResumeSession answered {other:?} instead of a session"),
        }),
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

    fn open_sample(store: &mut ProviderSessionStore) {
        upsert_open(
            store,
            "claude-code",
            "s-1",
            Some("/w/shop".into()),
            Some("p-shop".into()),
            Some("Fix the header".into()),
        );
    }

    #[test]
    fn an_open_round_trips_through_the_file() {
        let _env = temp_store("roundtrip");
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        write(&store);
        let back = read();
        assert_eq!(back, store, "what was written reads back identical");
        let record = &back["s-1"];
        assert_eq!(record.provider, "claude-code");
        assert_eq!(record.workspace.as_deref(), Some("/w/shop"));
        assert_eq!(record.project.as_deref(), Some("p-shop"));
        assert_eq!(record.title.as_deref(), Some("Fix the header"));
        assert_eq!(record.turns, 0);
    }

    #[test]
    fn a_settled_turn_moves_the_row_and_counts() {
        let _env = temp_store("turn");
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        let before = store["s-1"].updated_ms;
        assert!(note_settled_turn_counted(&mut store, "s-1", 1));
        assert_eq!(store["s-1"].turns, 1);
        assert!(store["s-1"].updated_ms >= before);
        write(&store);
        assert_eq!(read()["s-1"].turns, 1, "the count survives a restart");
    }

    #[test]
    fn a_turn_for_an_unknown_session_records_nothing() {
        let mut store = ProviderSessionStore::new();
        assert!(!note_settled_turn_counted(&mut store, "s-gone", 1));
        assert!(store.is_empty());
    }

    #[test]
    fn replayed_settles_do_not_double_count_exchanges() {
        // One user+assistant exchange settles once live, then replays once
        // on reopen: the row must read 1 turn, not 2.
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        assert!(note_settled_turn_counted(&mut store, "s-1", 1));
        assert_eq!(store["s-1"].turns, 1);
        assert!(note_settled_turn_counted(&mut store, "s-1", 1), "the replayed settle");
        assert_eq!(store["s-1"].turns, 1, "a replay never double-counts");
        assert!(note_settled_turn_counted(&mut store, "s-1", 2), "a second live exchange");
        assert_eq!(store["s-1"].turns, 2);
    }

    #[test]
    fn the_first_prompt_sticks_and_later_turns_do_not_overwrite_it() {
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        note_first_prompt(&mut store, "s-1", "  Fix the header\nsecond line  ");
        assert_eq!(
            store["s-1"].first_prompt.as_deref(),
            Some("Fix the header second line"),
            "stored the row's own way: one collapsed line"
        );
        note_first_prompt(&mut store, "s-1", "Something else");
        assert_eq!(
            store["s-1"].first_prompt.as_deref(),
            Some("Fix the header second line"),
            "the session keeps its own first words"
        );
    }

    #[test]
    fn removing_forgets_the_session() {
        let _env = temp_store("remove");
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        write(&store);
        assert!(remove(&mut store, "s-1"));
        assert!(!remove(&mut store, "s-1"), "a second delete reports nothing");
        write(&store);
        assert!(read().is_empty(), "the file forgets it too");
    }

    #[test]
    fn reopening_keeps_the_row_and_refreshes_recency() {
        let mut store = ProviderSessionStore::new();
        open_sample(&mut store);
        note_settled_turn_counted(&mut store, "s-1", 1);
        let created = store["s-1"].created_ms;
        upsert_open(&mut store, "claude-code", "s-1", None, None, None);
        assert_eq!(store["s-1"].created_ms, created, "a reopen is not a new session");
        assert_eq!(store["s-1"].turns, 1, "its history survives");
        assert_eq!(store["s-1"].workspace.as_deref(), Some("/w/shop"), "unset stays, not blanked");
    }

    #[test]
    fn a_missing_file_reads_as_no_sessions() {
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
        upsert_open(&mut store, "codex", "s-old", Some("/w".into()), None, None);
        let (adapter, commands) = RecordingAdapter::new();
        let provider = provider::Provider::new(adapter);
        let resumed = send_resume(&provider, &store["s-old"], "r-1").expect("resume lands");
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
        upsert_open(&mut store, "codex", "s-old", Some("/w".into()), None, None);
        match resume_command(&store["s-old"], "r-1") {
            provider::Command::ResumeSession { session_id, cursor, metadata_only, .. } => {
                assert_eq!(session_id, "s-old");
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
        upsert_open(&mut store, "codex", "s-old", None, None, None);
        let provider = provider::Provider::new(Refusing);
        assert!(send_resume(&provider, &store["s-old"], "r-1").is_err());
    }
}
