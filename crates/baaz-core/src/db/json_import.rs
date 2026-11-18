//! The first-launch import: the JSON stores the database replaces, read
//! once, written as rows in one transaction.
//!
//! This is the only reader of those files (`crates/baaz/tests/db_ratchet.rs`
//! fails if another appears). Every file is read and never touched: no
//! rename, no rewrite, no delete, not even for a file that will not parse
//! (its `.bak` is read instead when there is one, and the problem is
//! reported). They stay on disk as the backup.
//!
//! The marker (`meta.json_import`) is written in the same transaction as
//! the rows, so the import runs exactly once: a second launch sees the
//! marker and reads nothing, and a crash or an error mid-import rolls the
//! whole transaction back, leaving no row and no marker for the next
//! launch to trip over.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;

use crate::sent::SentItem;
use crate::sessions::SessionMeta;

/// Baaz's facts per session, split in two (docs/82 1.7).
pub const SESSION_FACTS: &str = "session-meta.json";
/// The regenerable half of the split.
pub const SESSION_CACHE: &str = "session-cache.json";
/// The file both halves came from before the split.
pub const SESSIONS_LEGACY: &str = "sessions.json";
/// The records of Claude Code, Codex and opened Muse sessions.
pub const PROVIDER_SESSIONS: &str = "provider-sessions.json";
/// The prompt history.
pub const HISTORY: &str = "history.json";
/// The folder of `<session>.json` sent-turn indexes (and their pictures,
/// which stay).
pub const SENT_DIR: &str = "sent";
/// The import journal.
pub const IMPORT_JOURNAL: &str = "import-journal.json";
/// The session migration's journal.
pub const MIGRATION_JOURNAL: &str = "session-migration-journal.json";

/// Every file name this module retires.
pub const RETIRED: [&str; 7] =
    [SESSION_FACTS, SESSION_CACHE, SESSIONS_LEGACY, PROVIDER_SESSIONS, HISTORY, IMPORT_JOURNAL, MIGRATION_JOURNAL];

/// A step of the import, for the test that fails one on purpose.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    Sessions,
    Turns,
    History,
    Sent,
    Journals,
}

/// What the import wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub sessions: usize,
    pub display_texts: usize,
    pub prompts: usize,
    pub sent_turns: usize,
    pub import_runs: usize,
    pub migration_entries: usize,
    /// Facts the two session files both held, with different values: the
    /// record's won.
    pub conflicts: usize,
}

/// One provider record as `provider-sessions.json` held it, every field
/// it ever had (titles and links moved to the session's own facts).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct LegacyRecord {
    provider: String,
    session_id: String,
    workspace: Option<String>,
    project: Option<String>,
    created_ms: i64,
    updated_ms: i64,
    turns: u64,
    title: Option<String>,
    first_prompt: Option<String>,
    handoff_to: Option<String>,
    handoff_from: Option<String>,
    handoff_from_provider: Option<String>,
    handoff_title: Option<String>,
    display_texts: HashMap<String, String>,
    origin: Option<String>,
}

/// Whether `state_dir` holds any store the import reads.
pub fn has_stores(state_dir: &Path) -> bool {
    RETIRED.iter().any(|name| state_dir.join(name).exists()) || !sent_indexes(&state_dir.join(SENT_DIR)).is_empty()
}

/// Import the JSON stores into `conn` unless that was done already.
pub fn run_once(conn: &mut Connection, state_dir: &Path) -> rusqlite::Result<()> {
    if super::schema::meta(conn, super::schema::IMPORTED_KEY)?.is_some() {
        return Ok(());
    }
    let report = import(conn, state_dir, None)?;
    if report != Report::default() {
        crate::baaz_log!("db: imported the JSON stores from {}: {report:?}", state_dir.display());
    }
    Ok(())
}

/// Read every retired store under `state_dir` and write it into `conn`,
/// with the marker, in one transaction. `fail_after` makes the import fail
/// right after that step (the test that proves nothing half-lands).
pub fn import(conn: &mut Connection, state_dir: &Path, fail_after: Option<Step>) -> rusqlite::Result<Report> {
    super::check_writer("import JSON stores");
    let fail = |step: Step| -> rusqlite::Result<()> {
        if fail_after == Some(step) {
            return Err(rusqlite::Error::InvalidParameterName(format!("import stopped after {step:?} (test)")));
        }
        Ok(())
    };
    let mut report = Report::default();
    let tx = conn.transaction()?;

    // Sessions: both files, merged per id into one row each.
    let meta = read_session_facts(state_dir);
    let records: BTreeMap<String, LegacyRecord> = read_map(&state_dir.join(PROVIDER_SESSIONS));
    let mut ids: Vec<&String> = meta.keys().chain(records.keys()).collect::<HashSet<_>>().into_iter().collect();
    ids.sort();
    let mut providers: HashMap<String, String> = HashMap::new();
    for id in ids {
        let record = records.get(id);
        let mut facts = meta.get(id).cloned();
        if let Some(record) = record {
            let facts = facts.get_or_insert_with(SessionMeta::default);
            report.conflicts += merge_record_facts(facts, record);
        }
        let provider = record.map(|r| r.provider.as_str()).unwrap_or("");
        let origin = match record {
            Some(r) => r.origin.as_deref().unwrap_or(super::sessions::ORIGIN_BAAZ),
            None => super::sessions::ORIGIN_LISTED,
        };
        let uuid = super::sessions::ensure_session(&tx, provider, id, origin)?;
        if let Some(facts) = facts.as_ref().filter(|facts| meta.contains_key(id) || **facts != SessionMeta::default()) {
            super::sessions::write_meta(&tx, &uuid, Some(facts))?;
        }
        if let Some(r) = record {
            let front = crate::provider_sessions::ProviderSessionRecord {
                provider: r.provider.clone(),
                session_id: id.clone(),
                workspace: r.workspace.clone(),
                created_ms: r.created_ms,
                updated_ms: r.updated_ms,
                turns: r.turns,
                origin: r.origin.clone(),
            };
            super::sessions::write_record(&tx, &uuid, Some(&front))?;
        }
        providers.insert(id.clone(), provider.to_owned());
        report.sessions += 1;
    }
    fail(Step::Sessions)?;

    // The bubble texts submits showed, keyed by the echoed text: no turn id
    // was ever recorded for them.
    for (id, record) in &records {
        let mut texts: Vec<(&String, &String)> = record.display_texts.iter().collect();
        texts.sort();
        for (n, (input, display)) in texts.into_iter().enumerate() {
            let turn_id = format!("{}{n}", super::turns::LEGACY_TURN_PREFIX);
            super::turns::note_display(&tx, &record.provider, id, &turn_id, input, display)?;
            report.display_texts += 1;
        }
    }
    fail(Step::Turns)?;

    // Prompt history per workspace.
    let history: BTreeMap<String, Vec<String>> = read_value(&state_dir.join(HISTORY))
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default();
    for (workspace, entries) in &history {
        super::history::write(&tx, workspace, entries)?;
        report.prompts += entries.len();
    }
    fail(Step::History)?;

    // What each turn was sent with: `sent/<stem>.json`, the stem being the
    // session id made safe for a file name.
    let stems: HashMap<String, String> =
        providers.keys().map(|id| (crate::sent::file_stem(id), id.clone())).collect();
    for (stem, path) in sent_indexes(&state_dir.join(SENT_DIR)) {
        let Some(items) = read_value(&path).and_then(|value| serde_json::from_value::<Vec<SentItem>>(value).ok())
        else {
            continue;
        };
        let id = stems.get(&stem).cloned().unwrap_or(stem);
        let provider = providers.get(&id).cloned().unwrap_or_default();
        let uuid = super::sessions::ensure_session(&tx, &provider, &id, super::sessions::ORIGIN_LISTED)?;
        for (ordinal, item) in items.iter().enumerate() {
            super::attachments::insert_item(&tx, &uuid, ordinal as i64, item, state_dir)?;
            report.sent_turns += 1;
        }
    }
    fail(Step::Sent)?;

    // The two journals.
    if let Some(journal) = read_value(&state_dir.join(IMPORT_JOURNAL))
        .and_then(|value| serde_json::from_value::<crate::import::Journal>(value).ok())
    {
        report.import_runs = journal.runs.len();
        super::journals::put_import(&tx, &journal)?;
    }
    if let Some(value) = read_value(&state_dir.join(MIGRATION_JOURNAL)) {
        let (done, live) = crate::session_migration::parse_journal(&value);
        report.migration_entries = done.len() + live.len();
        super::journals::put_migration(&tx, &done, &live)?;
    }
    fail(Step::Journals)?;

    super::schema::set_meta(&tx, super::schema::IMPORTED_KEY, &super::now_ms().to_string())?;
    tx.commit()?;
    Ok(report)
}

/// The record's facts that now live on the session's own facts: titles,
/// project and hand-off links. A fact both files held keeps the value the
/// window used to read first (the record's hand-off links and titles; the
/// facts' project). Returns how many differed.
fn merge_record_facts(facts: &mut SessionMeta, record: &LegacyRecord) -> usize {
    let mut conflicts = 0;
    let mut take = |slot: &mut Option<String>, value: &Option<String>, record_wins: bool| {
        let value = value.as_ref().filter(|v| !v.trim().is_empty());
        match (slot.as_ref(), value) {
            (_, None) => {}
            (None, Some(value)) => *slot = Some(value.clone()),
            (Some(held), Some(value)) if held == value => {}
            (Some(_), Some(value)) => {
                conflicts += 1;
                if record_wins {
                    *slot = Some(value.clone());
                }
            }
        }
    };
    take(&mut facts.project, &record.project, false);
    take(&mut facts.handoff_to, &record.handoff_to, true);
    take(&mut facts.handoff_from, &record.handoff_from, true);
    take(&mut facts.handoff_from_provider, &record.handoff_from_provider, true);
    take(&mut facts.handoff_title, &record.handoff_title, true);
    take(&mut facts.provider_title, &record.title, true);
    take(&mut facts.first_prompt, &record.first_prompt, true);
    conflicts
}

/// Baaz's facts per session: the two halves merged entry by entry, or,
/// with neither on disk, the file from before the split.
fn read_session_facts(state_dir: &Path) -> BTreeMap<String, SessionMeta> {
    let halves = [state_dir.join(SESSION_FACTS), state_dir.join(SESSION_CACHE)];
    if !halves.iter().any(|path| path.exists()) {
        return read_map(&state_dir.join(SESSIONS_LEGACY));
    }
    let mut merged: BTreeMap<String, serde_json::Map<String, serde_json::Value>> = BTreeMap::new();
    for path in &halves {
        let Some(serde_json::Value::Object(entries)) = read_value(path) else { continue };
        for (id, value) in entries {
            if let serde_json::Value::Object(fields) = value {
                merged.entry(id).or_default().extend(fields);
            }
        }
    }
    let mut out = BTreeMap::new();
    for (id, fields) in merged {
        match serde_json::from_value::<SessionMeta>(serde_json::Value::Object(fields)) {
            Ok(meta) => {
                out.insert(id, meta);
            }
            Err(error) => crate::baaz_log!("db import: session {id:?} does not parse ({error}); skipped"),
        }
    }
    out
}

/// A JSON map of independent entries: an entry that does not parse is
/// skipped (and counted in the log), never the whole file.
fn read_map<V: serde::de::DeserializeOwned>(path: &Path) -> BTreeMap<String, V> {
    let Some(serde_json::Value::Object(entries)) = read_value(path) else { return BTreeMap::new() };
    let mut out = BTreeMap::new();
    let mut skipped = 0usize;
    for (key, value) in entries {
        match serde_json::from_value(value) {
            Ok(entry) => {
                out.insert(key, entry);
            }
            Err(_) => skipped += 1,
        }
    }
    if skipped > 0 {
        crate::baaz_log!("db import: {skipped} entries of {} do not parse; skipped", path.display());
        crate::store::report(crate::store::StoreProblem::EntriesSkipped { name: file_name(path), count: skipped });
    }
    out
}

fn file_name(path: &Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// A file's JSON, read and never touched. A missing file is `None`,
/// quietly. A file that does not parse is reported and its `.bak` read in
/// its place when that parses; the file itself stays exactly as it is.
fn read_value(path: &Path) -> Option<serde_json::Value> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::baaz_log!("db import: cannot read {}: {error}", path.display());
            return None;
        }
    };
    if let Ok(value) = serde_json::from_str(&text) {
        return Some(value);
    }
    let backup = std::fs::read_to_string(crate::store::backup_path(path))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    crate::baaz_log!("db import: {} does not parse; {}", path.display(), if backup.is_some() { "read its .bak" } else { "skipped" });
    crate::store::report(crate::store::StoreProblem::Unreadable { name: file_name(path), restored: backup.is_some() });
    backup
}

/// `sent/<stem>.json` files: (stem, path).
fn sent_indexes(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let stem = path.file_name()?.to_str()?.strip_suffix(".json")?.to_owned();
            path.is_file().then_some((stem, path))
        })
        .collect();
    out.sort();
    out
}
