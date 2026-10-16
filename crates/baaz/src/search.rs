//! Full-text search over sessions and created files (Task E).
//!
//! Two FTS5 tables in a Baaz-owned `search.db` under the support dir:
//!
//! * `sessions_fts(session_id UNINDEXED, label, title, first_prompt, body,
//!   project, workspace UNINDEXED)` —
//!   fed from [`crate::index::read`] (Muse's `search_text`, which carries
//!   transcript text) plus the `sessions.json` overrides that name the rows.
//!   It is a cache: Muse's index stays the freshness source and this db is
//!   rebuilt off the UI thread at boot and after each index refresh.
//! * `files_fts(path, session_id UNINDEXED, kind, project, workspace
//!   UNINDEXED)` — "files we created", i.e.
//!   workspace-relative targets of tool calls with write/edit verbs, recorded
//!   when a turn completes. A path the agent only read is not a file we
//!   created, so reads, searches, shells and web tools never land here.
//!
//! `rusqlite` with `bundled` already ships FTS5, so no new dependency. Every
//! function that touches the disk blocks and belongs on `background_spawn`;
//! the pure helpers ([`snippet_for`], [`created_target`], [`relativize`]) are
//! what the unit tests pin.

use std::path::PathBuf;

use aui_protocol::ToolKind;
use rusqlite::{params, Connection};

/// `~/Library/Application Support/baaz/search.db`.
pub fn db_path() -> PathBuf {
    crate::store::support_dir().join("search.db")
}

/// How many rows one palette section shows.
pub const LIMIT: usize = 12;

/// The FTS schema version, stamped in `meta`. FTS5 cannot add a column, so
/// a version bump drops and recreates both tables: `sessions_fts` is rebuilt
/// from the list anyway, and `files_fts` starts empty (a completed turn
/// re-records its session's files on the next completion).
///
/// 3 added the indexed `project` column. Before it, the only reason typing a
/// project's name found anything was that its path happened to appear in
/// transcript text, so it worked in some sessions and not others for no
/// reason a person could see.
const SCHEMA_VERSION: u32 = 3;

/// One session row to index.
pub struct SessionRow {
    /// The Muse session id.
    pub session_id: String,
    /// The sidebar label (name, index label or derived title).
    pub label: String,
    /// The index's generated title.
    pub title: String,
    /// The index's first user prompt.
    pub first_prompt: String,
    /// The index's `search_text`: transcript text Muse made searchable.
    pub body: String,
    /// What a person would type to mean this row's project — see
    /// [`project_terms`], which is what fills it.
    pub project: String,
    /// The session's workspace, canonicalized when it exists. Not indexed:
    /// it is how a hit is scoped to a project, never how one is found.
    pub workspace: Option<String>,
}

/// One created file to record.
pub struct FileRecord {
    /// Workspace-relative path, e.g. `src/main.rs`.
    pub path: String,
    /// The session whose turn wrote it.
    pub session_id: String,
    /// `"write"` or `"edit"`: the verb family that produced it.
    pub kind: String,
    /// What a person would type to mean this file's project — see
    /// [`project_terms`].
    pub project: String,
    /// The session's workspace, canonicalized when it exists. Not indexed:
    /// it is how a hit is scoped to a project, never how one is found.
    pub workspace: String,
}

/// A session matching the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHit {
    /// The Muse session id.
    pub session_id: String,
    /// The sidebar label.
    pub label: String,
    /// One line of `body` around the first match (see [`snippet_for`]).
    pub snippet: String,
    /// The session's workspace.
    pub workspace: Option<String>,
}

/// A created file matching the query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHit {
    /// Workspace-relative path.
    pub path: String,
    /// The session whose turn wrote it.
    pub session_id: String,
    /// The session's workspace.
    pub workspace: Option<String>,
}

/// Whether a hit belongs in a scoped query: an unscoped query takes
/// everything, and a query scoped to a root takes only the hits recorded
/// under it.
pub fn matches_scope(hit_workspace: Option<&str>, scope: Option<&str>) -> bool {
    match scope {
        None => true,
        Some(root) => hit_workspace == Some(root),
    }
}

/// Open (creating) the database, ensuring the schema and FTS5 work.
///
/// The `USING fts5` smoke query runs at open: a build whose sqlite has no
/// FTS5 fails here rather than serving an empty palette half-way through a
/// search.
pub fn open() -> Result<Connection, rusqlite::Error> {
    open_at(&db_path())
}

/// [`open`] against an explicit path, which is what the tests use
/// (`:memory:` included).
pub fn open_at(path: &std::path::Path) -> Result<Connection, rusqlite::Error> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    let connection = Connection::open(path)?;
    // WAL: readers (the palette) never wait on a writer (a sync), and a
    // commit is one append instead of a rollback journal of every page it
    // touched — the old full rebuild journalled ~1 GB (measured 2026-10-16).
    // `:memory:` answers "memory" and that is fine.
    let _ = connection.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()));
    connection.execute_batch("PRAGMA synchronous=NORMAL;")?;
    let _ = connection.busy_timeout(std::time::Duration::from_secs(2));
    // FTS5 is a property of the linked SQLite, not of the file: prove it
    // once per process on a scratch connection rather than with a schema
    // write (CREATE + DROP) on every open, which used to run per palette
    // keystroke and invalidate every other connection's statements.
    fts5_available()?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);")?;
    let stored: u32 = connection
        .query_row("SELECT value FROM meta WHERE key = 'schema_version'", [], |row| {
            row.get::<_, String>(0)
        })
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if stored != SCHEMA_VERSION {
        // FTS5 cannot add a column: drop both tables and recreate them with
        // `workspace UNINDEXED`. `sessions_fts` is rebuilt from the list on
        // the next refresh; `files_fts` starts empty and fills again as turns
        // complete.
        connection.execute_batch(
            "DROP TABLE IF EXISTS sessions_fts;
             DROP TABLE IF EXISTS files_fts;
             DROP TABLE IF EXISTS files_seen;
             DROP TABLE IF EXISTS sessions_state;
             CREATE VIRTUAL TABLE sessions_fts USING fts5(session_id UNINDEXED, label, title, first_prompt, body, project, workspace UNINDEXED);
             CREATE VIRTUAL TABLE files_fts USING fts5(path, session_id UNINDEXED, kind, project, workspace UNINDEXED);
             CREATE TABLE files_seen (path TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY (path, session_id)) WITHOUT ROWID;",
        )?;
        connection.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
        // The fingerprint described the table just dropped.
        connection.execute("DELETE FROM meta WHERE key = 'sessions_fingerprint'", [])?;
    } else {
        connection.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(session_id UNINDEXED, label, title, first_prompt, body, project, workspace UNINDEXED);
             CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(path, session_id UNINDEXED, kind, project, workspace UNINDEXED);",
        )?;
    }
    // Dedupe for `files_fts`, which cannot hold a unique index of its own:
    // an FTS5 table has no constraints, so `(path, session_id)` used to be
    // checked with a `SELECT` per record — a full-text scan each time
    // (finding `support-4`). This ordinary table carries the key, and
    // `INSERT OR IGNORE` on it both dedupes and says whether the row was new.
    let had_seen = table_exists(&connection, "files_seen");
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS files_seen (path TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY (path, session_id)) WITHOUT ROWID;",
    )?;
    if !had_seen {
        // An index written before this table existed still has its rows;
        // seeding from them keeps the first run after an upgrade from
        // recording every known file a second time. Only on creation: it is
        // a full scan of `files_fts`, and it used to run on every open.
        connection.execute_batch(
            "INSERT OR IGNORE INTO files_seen(path, session_id) SELECT path, session_id FROM files_fts;",
        )?;
    }
    // What [`sync_sessions`] last wrote per session: the FTS rowid to delete
    // by (an FTS5 `WHERE session_id = ?` is a full scan of the table) and the
    // fingerprint that says whether the row needs writing at all.
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions_state (session_id TEXT PRIMARY KEY, fts_rowid INTEGER NOT NULL, fingerprint INTEGER NOT NULL) WITHOUT ROWID;",
    )?;
    Ok(connection)
}

/// Whether this process's SQLite has FTS5, proven once on a scratch
/// in-memory connection.
fn fts5_available() -> Result<(), rusqlite::Error> {
    static OK: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let ok = *OK.get_or_init(|| {
        Connection::open_in_memory()
            .and_then(|scratch| scratch.execute_batch("CREATE VIRTUAL TABLE smoke USING fts5(x);"))
            .is_ok()
    });
    if ok {
        Ok(())
    } else {
        Err(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("no fts5 in this sqlite".into()),
        ))
    }
}

/// Whether `name` is a table in `connection`'s main schema.
fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1", params![name], |_| Ok(()))
        .is_ok()
}

/// Replace the whole session index with `rows`.
///
/// Deletes-then-inserts in one transaction: a crash leaves the previous
/// index or the next one, never half of each. Tests only: the app keeps
/// the index current with [`sync_sessions`].
#[cfg(test)]
pub fn rebuild_sessions(connection: &mut Connection, rows: &[SessionRow]) -> Result<(), rusqlite::Error> {
    {
        let transaction = connection.transaction()?;
        transaction.execute("DELETE FROM sessions_fts", [])?;
        transaction.execute("DELETE FROM sessions_state", [])?;
        transaction.commit()?;
    }
    let synced: Vec<SyncRow<'_>> =
        rows.iter().map(|row| SyncRow { row, body_ids: &[], fingerprint: 0 }).collect();
    sync_sessions(connection, &synced, &|_| std::collections::HashMap::new()).map(|_| ())
}

/// One session row for [`sync_sessions`]: the row's own words, the index
/// sessions whose transcript text becomes its body, and a fingerprint of
/// everything that would change what is written.
pub struct SyncRow<'a> {
    /// The row: label, title, first prompt, inline body, project, workspace.
    pub row: &'a SessionRow,
    /// Muse index ids whose `search_text` is fetched (only when the row is
    /// written) and placed before the inline body, in this order.
    pub body_ids: &'a [String],
    /// Equal to the last write's fingerprint means the row is skipped.
    pub fingerprint: i64,
}

/// What one [`sync_sessions`] did, for the trace line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    /// Rows written (new or changed).
    pub written: usize,
    /// Rows deleted because their session is gone.
    pub removed: usize,
    /// Rows whose fingerprint matched: nothing written.
    pub unchanged: usize,
}

/// Bring `sessions_fts` in line with `rows`, writing only what changed.
///
/// A row whose fingerprint matches the last write is skipped; a changed or
/// new one is replaced by rowid; a session no longer in `rows` is deleted.
/// Bodies are fetched with `bodies` for the written rows only, so a sync
/// after one finished turn reads one transcript, not 210 MB of them (the
/// old full rebuild: 34 s, 13.6 s CPU, ~1 GB journalled per run, twice per
/// turn — measured on a copy 2026-10-16). One transaction.
pub fn sync_sessions(
    connection: &mut Connection,
    rows: &[SyncRow<'_>],
    bodies: &dyn Fn(&[String]) -> std::collections::HashMap<String, String>,
) -> Result<SyncStats, rusqlite::Error> {
    use std::collections::{HashMap, HashSet};
    let mut state: HashMap<String, (i64, i64)> = HashMap::new();
    {
        let mut select = connection.prepare("SELECT session_id, fts_rowid, fingerprint FROM sessions_state")?;
        let found = select.query_map([], |row| Ok((row.get::<_, String>(0)?, (row.get(1)?, row.get(2)?))))?;
        for (id, entry) in found.filter_map(Result::ok) {
            state.insert(id, entry);
        }
    }
    let mut stats = SyncStats::default();
    let changed: Vec<&SyncRow<'_>> = rows
        .iter()
        .filter(|sync| match state.get(&sync.row.session_id) {
            Some((_, fingerprint)) if *fingerprint == sync.fingerprint && sync.fingerprint != 0 => {
                stats.unchanged += 1;
                false
            }
            _ => true,
        })
        .collect();
    let wanted: HashSet<&str> = rows.iter().map(|sync| sync.row.session_id.as_str()).collect();
    let gone: Vec<(String, i64)> = state
        .iter()
        .filter(|(id, _)| !wanted.contains(id.as_str()))
        .map(|(id, (rowid, _))| (id.clone(), *rowid))
        .collect();
    let mut fetch: Vec<String> = changed.iter().flat_map(|sync| sync.body_ids.iter().cloned()).collect();
    fetch.sort();
    fetch.dedup();
    let fetched = if fetch.is_empty() { HashMap::new() } else { bodies(&fetch) };
    let transaction = connection.transaction()?;
    // A store written before `sessions_state` existed has rows nobody can
    // address by rowid: clear them once, and this sync writes every row.
    if state.is_empty() {
        transaction.execute("DELETE FROM sessions_fts", [])?;
    }
    {
        let mut delete_row = transaction.prepare("DELETE FROM sessions_fts WHERE rowid = ?1")?;
        let mut delete_state = transaction.prepare("DELETE FROM sessions_state WHERE session_id = ?1")?;
        for (id, rowid) in &gone {
            delete_row.execute(params![rowid])?;
            delete_state.execute(params![id])?;
            stats.removed += 1;
        }
        let mut insert = transaction.prepare(
            "INSERT INTO sessions_fts(session_id, label, title, first_prompt, body, project, workspace) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        let mut record = transaction.prepare(
            "INSERT OR REPLACE INTO sessions_state(session_id, fts_rowid, fingerprint) VALUES (?1, ?2, ?3)",
        )?;
        for sync in &changed {
            let row = sync.row;
            if let Some((rowid, _)) = state.get(&row.session_id) {
                delete_row.execute(params![rowid])?;
            }
            let mut parts: Vec<&str> =
                sync.body_ids.iter().filter_map(|id| fetched.get(id).map(String::as_str)).collect();
            if !row.body.is_empty() {
                parts.push(&row.body);
            }
            let body = parts.join("\n");
            insert.execute(params![
                row.session_id,
                row.label,
                row.title,
                row.first_prompt,
                body,
                row.project,
                row.workspace
            ])?;
            record.execute(params![row.session_id, transaction.last_insert_rowid(), sync.fingerprint])?;
            stats.written += 1;
        }
    }
    transaction.commit()?;
    Ok(stats)
}

/// The words a person would type to mean "this project", for the indexed
/// `project` column.
///
/// The workspace folder's own name, which is what an unadopted root has to
/// go on, plus the adopted project's name when it has been renamed — a
/// person who renamed `acme-web` to `Storefront` will type either one, and
/// both should find it.
///
/// Deliberately not the whole workspace path: its leading segments are the
/// same for every project on the machine, so indexing them would make
/// "Projects" or a home-directory name match everything.
pub fn project_terms(workspace: Option<&str>, adopted_name: Option<&str>) -> String {
    let folder = workspace.map(crate::sidebar::workspace_folder).unwrap_or_default();
    let name = adopted_name.map(str::trim).filter(|n| !n.is_empty()).unwrap_or_default();
    if name.is_empty() || name.eq_ignore_ascii_case(&folder) {
        return folder;
    }
    if folder.is_empty() {
        return name.to_owned();
    }
    format!("{name} {folder}")
}

/// Record created files, ignoring ones already recorded.
///
/// The same turn recorded twice (a replay, a reconnect) must not duplicate
/// the row. The key lives in `files_seen`, whose primary key does the
/// deduplication in the insert itself: a row that was already there costs one
/// index probe and writes nothing, instead of the full-text `SELECT` per
/// record this used to run (finding `support-4`).
pub fn record_files(connection: &Connection, records: &[FileRecord]) -> Result<(), rusqlite::Error> {
    let mut claim = connection
        .prepare("INSERT OR IGNORE INTO files_seen(path, session_id) VALUES (?1, ?2)")?;
    let mut insert = connection
        .prepare("INSERT INTO files_fts(path, session_id, kind, project, workspace) VALUES (?1, ?2, ?3, ?4, ?5)")?;
    for record in records {
        // `execute` returns the rows it changed: 0 means the key was already
        // claimed, which is exactly "already recorded".
        if claim.execute(params![record.path, record.session_id])? == 0 {
            continue;
        }
        insert.execute(params![record.path, record.session_id, record.kind, record.project, record.workspace])?;
    }
    Ok(())
}
/// Sessions matching `query`, best first (bm25), at most `limit`.
pub fn query_sessions(connection: &Connection, query: &str, limit: usize) -> Vec<SessionHit> {
    let Some(matched) = fts_query(query) else { return Vec::new() };
    let mut statement = match connection.prepare(
        "SELECT session_id, label, body, workspace FROM sessions_fts WHERE sessions_fts MATCH ?1 ORDER BY bm25(sessions_fts) LIMIT ?2",
    ) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let rows = statement.query_map(params![matched, limit as i64], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
        ))
    });
    let Ok(rows) = rows else { return Vec::new() };
    rows.filter_map(Result::ok)
        .map(|(session_id, label, body, workspace)| {
            // The indexed body is Muse's raw `search_text`: unit-separated
            // header segments (ids, status, paths) ahead of the transcript.
            // The row shows words, so the snippet is cut from the cleaned
            // text, never from the raw envelope.
            let snippet = snippet_for(&clean_search_text(&body), query);
            SessionHit { session_id, label, snippet, workspace }
        })
        .collect()
}

/// Created files matching `query`, newest records first.
///
/// An empty query lists the most recently recorded files: the palette's empty
/// state shows recent work rather than nothing.
pub fn query_files(connection: &Connection, query: &str, limit: usize) -> Vec<FileHit> {
    if query.trim().is_empty() {
        let mut statement = match connection.prepare(
            "SELECT path, session_id, workspace FROM files_fts ORDER BY rowid DESC LIMIT ?1",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let rows = statement.query_map(params![limit as i64], |row| {
            Ok(FileHit { path: row.get(0)?, session_id: row.get(1)?, workspace: row.get(2)? })
        });
        let Ok(rows) = rows else { return Vec::new() };
        return rows.filter_map(Result::ok).collect();
    }
    let Some(matched) = fts_query(query) else { return Vec::new() };
    let mut statement = match connection.prepare(
        "SELECT path, session_id, workspace FROM files_fts WHERE files_fts MATCH ?1 ORDER BY bm25(files_fts) LIMIT ?2",
    ) {
        Ok(statement) => statement,
        Err(_) => return Vec::new(),
    };
    let rows = statement.query_map(params![matched, limit as i64], |row| {
        Ok(FileHit { path: row.get(0)?, session_id: row.get(1)?, workspace: row.get(2)? })
    });
    let Ok(rows) = rows else { return Vec::new() };
    rows.filter_map(Result::ok).collect()
}

/// Turn a raw query into an FTS5 `MATCH` string, or `None` when it has no
/// searchable token.
///
/// Tokens are alphanumeric runs, each a **prefix** term, joined with `AND`.
/// Anything else — quotes, colons, parens — is dropped rather than
/// interpreted, so typing `foo(` never errors the query.
///
/// Both of those are load-bearing for a palette you type into:
///
/// * The trailing `*` is what makes a half-typed word match. Without it each
///   token is a phrase that only matches a whole indexed token, so `gol`
///   never reached `gold-annular` and `har` never reached `harness` — the
///   palette looked broken for every query short of a complete word.
/// * `AND` is what makes a second word narrow the result. Under `OR` each
///   word you added widened it, so typing more made the answer worse:
///   `gold ann` returned 12 rows where `gold-annular` was the only one meant.
fn fts_query(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        // The token cannot contain a quote — it is an alphanumeric run — so
        // quoting it is enough to keep FTS5 from reading it as syntax.
        .map(|token| format!("\"{token}\"*"))
        .collect();
    if tokens.is_empty() {
        return None;
    }
    Some(tokens.join(" AND "))
}

/// Largest char boundary in `text` at or below `bound`.
fn floor_boundary(text: &str, bound: usize) -> usize {
    let mut bound = bound.min(text.len());
    while !text.is_char_boundary(bound) {
        bound -= 1;
    }
    bound
}

/// Smallest char boundary in `text` at or above `bound`.
fn ceil_boundary(text: &str, bound: usize) -> usize {
    let mut bound = bound.min(text.len());
    while !text.is_char_boundary(bound) {
        bound += 1;
    }
    bound
}

/// Muse's raw `search_text` with its index envelope stripped: the
/// `\x1f`-separated header segments (session id, short id, `valid`, the
/// workspace path, `meta`, the model id) are metadata about the session, not
/// its words. What remains is the title, the first prompt and the transcript
/// text, joined and whitespace-collapsed for display. Matching still runs on
/// the raw body in FTS; only the shown snippet is cleaned.
pub fn clean_search_text(body: &str) -> String {
    body.split(['\u{1f}', '\u{1e}'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .filter(|segment| !is_index_metadata(segment))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether a `\x1f`-separated `search_text` segment is envelope rather than
/// words: a session uuid, a short hex id, the `valid`/`meta` markers, a bare
/// absolute path (the workspace the session ran in), or a model id. A title
/// or a first prompt never matches any of these shapes, so they survive.
fn is_index_metadata(segment: &str) -> bool {
    if segment.eq_ignore_ascii_case("valid") || segment.eq_ignore_ascii_case("meta") {
        return true;
    }
    // A session uuid (`8-4-4-4-12` hex) or a short hex id (`01a07c66`).
    // The long form is exactly 36 chars with four hyphens; the short form
    // is 8–12 bare hex digits. Plain numbers and short hex words in prose
    // are shorter than that, so they survive.
    if segment.len() == 36
        && segment.chars().filter(|c| *c == '-').count() == 4
        && segment.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    {
        return true;
    }
    if (8..=12).contains(&segment.len()) && segment.chars().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    // The workspace path: absolute, and one token (transcript prose with a
    // path in it keeps its spaces, so it keeps the segment).
    if segment.starts_with('/') && !segment.contains(char::is_whitespace) {
        return true;
    }
    // A model id (`muse-spark-1.3-contributor`): one token naming a model.
    if !segment.contains(char::is_whitespace)
        && (segment.starts_with("muse-") || segment.ends_with("-contributor"))
    {
        return true;
    }
    false
}

/// One line of `body` around the query's first match, for the palette row.
///
/// `body` is already [`clean_search_text`] output: plain words, no envelope.
/// Case-insensitive; whitespace collapses; the window holds ~90 chars around
/// the first match. No match is the body's own first line: the row still says
/// something honest.
pub fn snippet_for(body: &str, query: &str) -> String {
    let flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.is_empty() {
        return String::new();
    }
    const RADIUS: usize = 45;
    const CAP: usize = 92;
    // Searched on the lowercase copy, windowed on the original: byte indices
    // can disagree past ASCII, so both ends go through a char-boundary floor
    // and ceiling rather than slicing raw.
    let lower = flat.to_lowercase();
    let first = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .filter_map(|token| lower.find(&token.to_lowercase()))
        .min();
    let mut snippet = match first {
        Some(at) => {
            let start = floor_boundary(&flat, at.saturating_sub(RADIUS));
            let end = ceil_boundary(&flat, at + RADIUS);
            let mut text = flat[start..end].to_owned();
            if start > 0 {
                text = format!("\u{2026}{}", text.trim_start());
            }
            if end < flat.len() {
                text = format!("{}{}", text.trim_end(), "\u{2026}");
            }
            text
        }
        None => flat,
    };
    if snippet.chars().count() > CAP {
        snippet = snippet.chars().take(CAP - 1).collect::<String>() + "\u{2026}";
    }
    snippet
}

/// Whether a tool call created a file, and which verb family it was.
///
/// Only writes and edits: a path the agent read, searched or ran is not a
/// file we created. Unknown (MCP) tools are never guessed at — recording a
/// wrong path would be worse than missing one.
pub fn created_target(kind: &ToolKind, target: &str) -> Option<&'static str> {
    match kind {
        ToolKind::Write => Some("write"),
        ToolKind::Edit => Some("edit"),
        ToolKind::Read | ToolKind::Search | ToolKind::Shell | ToolKind::Web | ToolKind::Browser => None,
        ToolKind::SubAgent => None,
        ToolKind::Mcp { .. } => None,
    }
    .filter(|_| !target.trim().is_empty())
}

/// Make `target` workspace-relative, or `None` when it escapes.
///
/// Absolute targets under `root` are relativized; bare relative targets are
/// taken as-is; `..` that climbs above the root (and absolute paths outside
/// it) are rejected — the recorder must not index files outside the session's
/// workspace.
pub fn relativize(root: &str, target: &str) -> Option<String> {
    let target = target.trim();
    if target.is_empty() {
        return None;
    }
    let root = root.trim_end_matches('/');
    if target.starts_with('/') {
        let stripped = target.strip_prefix(&format!("{root}/"))?;
        if stripped.is_empty() || stripped.starts_with("..") || stripped.contains("../") {
            return None;
        }
        Some(stripped.to_owned())
    } else {
        if target == ".." || target.starts_with("../") || target.contains("/../") {
            return None;
        }
        Some(target.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        open_at(std::path::Path::new(":memory:")).expect("in-memory search db")
    }

    #[test]
    fn fts5_is_available_at_open() {
        // The smoke query in `open_at` is the assertion: no FTS5, no db.
        let connection = memory();
        let version: String =
            connection.query_row("SELECT sqlite_version()", [], |row| row.get(0)).expect("version");
        assert!(!version.is_empty());
    }

    fn session_row(session_id: &str, body: &str, workspace: Option<&str>) -> SessionRow {
        SessionRow {
            session_id: session_id.into(),
            label: format!("session {session_id}"),
            title: format!("session {session_id}"),
            first_prompt: String::new(),
            body: body.into(),
            project: project_terms(workspace, None),
            workspace: workspace.map(str::to_owned),
        }
    }

    fn file_record(path: &str, session_id: &str, workspace: &str) -> FileRecord {
        FileRecord {
            path: path.into(),
            session_id: session_id.into(),
            kind: "write".into(),
            project: project_terms(Some(workspace), None),
            workspace: workspace.into(),
        }
    }

    /// The reported bug: typing a project's name found its sessions only
    /// when the path happened to appear in transcript text, so it worked in
    /// some sessions and not others. The name is indexed now.
    #[test]
    fn a_project_name_finds_its_sessions_whatever_the_transcript_says() {
        let mut connection = memory();
        rebuild_sessions(
            &mut connection,
            &[
                session_row("s1", "nothing in here names the folder", Some("/work/acme-web")),
                session_row("s2", "nothing in here either", Some("/work/notes")),
            ],
        )
        .expect("rebuild");

        let hits = query_sessions(&connection, "acme-web", LIMIT);
        let ids: Vec<&str> = hits.iter().map(|h| h.session_id.as_str()).collect();
        assert_eq!(ids, ["s1"], "the folder name finds its own session and no other");

        // A path segment every project shares must not match everything —
        // which is why the column carries the folder name, not the path.
        assert!(query_sessions(&connection, "work", LIMIT).is_empty());
    }

    /// A renamed project is findable by the name on screen as well as by the
    /// folder it still lives in.
    #[test]
    fn a_renamed_project_answers_to_both_names() {
        let mut connection = memory();
        let mut row = session_row("s1", "unrelated transcript", Some("/work/acme-web"));
        row.project = project_terms(Some("/work/acme-web"), Some("Storefront"));
        rebuild_sessions(&mut connection, &[row]).expect("rebuild");

        assert_eq!(query_sessions(&connection, "Storefront", LIMIT).len(), 1, "the name on screen");
        assert_eq!(query_sessions(&connection, "acme-web", LIMIT).len(), 1, "and the folder");
    }

    /// The terms themselves: no duplication when a project was never
    /// renamed, and nothing but the name when there is no workspace.
    #[test]
    fn project_terms_carry_the_folder_and_any_different_name() {
        assert_eq!(project_terms(Some("/work/acme-web"), None), "acme-web");
        assert_eq!(project_terms(Some("/work/acme-web"), Some("Storefront")), "Storefront acme-web");
        // Same name as the folder, in any case: said once.
        assert_eq!(project_terms(Some("/work/acme-web"), Some("acme-web")), "acme-web");
        assert_eq!(project_terms(Some("/work/acme-web"), Some("ACME-WEB")), "acme-web");
        assert_eq!(project_terms(Some("/work/acme-web"), Some("   ")), "acme-web");
        assert_eq!(project_terms(None, Some("Storefront")), "Storefront");
        assert_eq!(project_terms(None, None), "");
    }

    #[test]
    fn sessions_round_trip_through_match() {
        let mut connection = memory();
        rebuild_sessions(
            &mut connection,
            &[SessionRow {
                session_id: "s1".into(),
                label: "Fix the parser".into(),
                title: "Fix the parser".into(),
                first_prompt: "why does this panic".into(),
                body: "the parser panics on nested generics in parser.rs".into(),
                project: String::new(),
                workspace: Some("/work/a".into()),
            }],
        )
        .expect("rebuild");
        let hits = query_sessions(&connection, "generics", LIMIT);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].session_id, "s1");
        assert!(hits[0].snippet.contains("generics"));
        assert_eq!(hits[0].workspace.as_deref(), Some("/work/a"));
        assert!(query_sessions(&connection, "zebra", LIMIT).is_empty());
    }

    #[test]
    fn workspaces_round_trip_and_scope_the_query() {
        let mut connection = memory();
        rebuild_sessions(
            &mut connection,
            &[
                session_row("s1", "the parser panics on nested generics", Some("/work/a")),
                session_row("s2", "the parser panics on nested generics", Some("/work/b")),
            ],
        )
        .expect("rebuild");
        record_files(&connection, &[file_record("src/main.rs", "s1", "/work/a")]).expect("record");
        // Unscoped: everything matches.
        assert_eq!(query_sessions(&connection, "generics", LIMIT).len(), 2);
        // Scoped to B: A's session is not returned.
        let scoped: Vec<SessionHit> = query_sessions(&connection, "generics", LIMIT)
            .into_iter()
            .filter(|hit| matches_scope(hit.workspace.as_deref(), Some("/work/b")))
            .collect();
        assert_eq!(scoped.len(), 1);
        assert_eq!(scoped[0].session_id, "s2");
        // Files carry their workspace too.
        let files = query_files(&connection, "main", LIMIT);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].workspace.as_deref(), Some("/work/a"));
        assert!(matches_scope(files[0].workspace.as_deref(), Some("/work/a")));
        assert!(!matches_scope(files[0].workspace.as_deref(), Some("/work/b")));
        assert!(matches_scope(files[0].workspace.as_deref(), None));
        assert!(!matches_scope(None, Some("/work/b")));
    }

    #[test]
    fn an_old_database_migrates_to_the_workspace_schema() {
        let dir = std::env::temp_dir().join(format!("baaz-search-migrate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("search.db");
        // What the previous build left behind: the old shapes, no `meta`.
        let old = Connection::open(&path).expect("old db");
        old.execute_batch(
            "CREATE VIRTUAL TABLE sessions_fts USING fts5(session_id UNINDEXED, label, title, first_prompt, body);
             CREATE VIRTUAL TABLE files_fts USING fts5(path, session_id UNINDEXED, kind);
             INSERT INTO sessions_fts(session_id, label, title, first_prompt, body) VALUES ('s1', 'old', 'old', '', 'generics everywhere');",
        )
        .expect("old rows");
        drop(old);
        // Opening migrates: the old rows are gone, the new columns exist.
        let mut migrated = open_at(&path).expect("migrated db");
        assert!(query_sessions(&migrated, "generics", LIMIT).is_empty());
        rebuild_sessions(&mut migrated, &[session_row("s1", "generics everywhere", Some("/work/a"))])
            .expect("rebuild");
        let hits = query_sessions(&migrated, "generics", LIMIT);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].workspace.as_deref(), Some("/work/a"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The regression: the palette listed sessions on an
    /// empty query and found nothing the moment anything was typed, because
    /// every token was a whole-token phrase. `gol` has to reach
    /// `gold-annular` the way a palette you type into is expected to.
    #[test]
    fn a_half_typed_word_matches_by_prefix() {
        let mut connection = memory();
        rebuild_sessions(
            &mut connection,
            &[
                SessionRow {
                    session_id: "s1".into(),
                    label: "gold-annular".into(),
                    title: "gold-annular".into(),
                    first_prompt: String::new(),
                    body: "rename the app".into(),
                    project: String::new(),
                    workspace: Some("/work/harness".into()),
                },
                SessionRow {
                    session_id: "s2".into(),
                    label: "teal-meridian".into(),
                    title: "teal-meridian".into(),
                    first_prompt: String::new(),
                    body: "queued message".into(),
                    project: String::new(),
                    workspace: Some("/work/harness".into()),
                },
            ],
        )
        .expect("rebuild");
        let labels = |q: &str| {
            let mut found: Vec<String> =
                query_sessions(&connection, q, LIMIT).into_iter().map(|hit| hit.label).collect();
            found.sort();
            found
        };
        // Every prefix of a label finds it, one character at a time.
        for prefix in ["g", "go", "gol", "gold", "annu"] {
            assert_eq!(labels(prefix), vec!["gold-annular"], "prefix {prefix:?}");
        }
        // The whole token still works, as it always did.
        assert_eq!(labels("gold"), vec!["gold-annular"]);
        // A word from the body, half typed.
        assert_eq!(labels("queu"), vec!["teal-meridian"]);
    }

    /// A second word must narrow the answer. Joined with `OR` it widened it,
    /// so typing more of what you wanted returned more of what you did not.
    #[test]
    fn a_second_word_narrows_rather_than_widens() {
        let mut connection = memory();
        rebuild_sessions(
            &mut connection,
            &[
                SessionRow {
                    session_id: "s1".into(),
                    label: "gold-annular".into(),
                    title: "gold-annular".into(),
                    first_prompt: String::new(),
                    body: String::new(),
                    project: String::new(),
                    workspace: None,
                },
                SessionRow {
                    session_id: "s2".into(),
                    label: "gold-perigee".into(),
                    title: "gold-perigee".into(),
                    first_prompt: String::new(),
                    body: String::new(),
                    project: String::new(),
                    workspace: None,
                },
                SessionRow {
                    session_id: "s3".into(),
                    label: "silver-annulus".into(),
                    title: "silver-annulus".into(),
                    first_prompt: String::new(),
                    body: String::new(),
                    project: String::new(),
                    workspace: None,
                },
            ],
        )
        .expect("rebuild");
        let count = |q: &str| query_sessions(&connection, q, LIMIT).len();
        assert_eq!(count("gold"), 2, "one word matches both golds");
        assert_eq!(count("gold ann"), 1, "the second word narrows to one");
        let hit = query_sessions(&connection, "gold ann", LIMIT);
        assert_eq!(hit.first().map(|h| h.label.as_str()), Some("gold-annular"));
    }

    #[test]
    fn punctuation_never_errors_the_query() {
        let connection = memory();
        assert!(query_sessions(&connection, "(((:::", LIMIT).is_empty());
        assert!(query_sessions(&connection, "", LIMIT).is_empty());
        assert!(query_files(&connection, "\"", LIMIT).is_empty());
    }

    #[test]
    fn files_record_once_and_rank_by_recency_when_empty() {
        let connection = memory();
        let records =
            |path: &str| FileRecord { path: path.into(), session_id: "s1".into(), kind: "write".into(), project: String::new(), workspace: "/work/a".into() };
        record_files(&connection, &[records("src/main.rs"), records("src/main.rs")]).expect("record");
        record_files(&connection, &[records("docs/notes.md")]).expect("record");
        // Recorded twice, stored once.
        assert_eq!(query_files(&connection, "main", LIMIT).len(), 1);
        // Empty query is newest first.
        let recent = query_files(&connection, "", LIMIT);
        assert_eq!(recent.first().map(|hit| hit.path.as_str()), Some("docs/notes.md"));
    }

    #[test]
    fn the_same_path_in_another_session_is_its_own_record() {
        let connection = memory();
        let record = |session: &str| FileRecord {
            path: "src/main.rs".into(),
            session_id: session.into(),
            kind: "write".into(),
            project: String::new(),
            workspace: "/work/a".into(),
        };
        record_files(&connection, &[record("s1"), record("s2"), record("s1")]).expect("record");
        // The key is `(path, session_id)`, so two sessions that wrote the
        // same file are two rows and the repeat within a session is none.
        assert_eq!(query_files(&connection, "main", LIMIT).len(), 2);
    }

    #[test]
    fn an_index_written_before_the_dedupe_table_is_not_recorded_twice() {
        let connection = memory();
        // What an older build left behind: rows in `files_fts` and nothing in
        // `files_seen`, which `open_at` seeds from on the next open.
        connection.execute("DELETE FROM files_seen", []).expect("clear");
        connection
            .execute(
                "INSERT INTO files_fts(path, session_id, kind) VALUES ('src/main.rs', 's1', 'write')",
                [],
            )
            .expect("legacy row");
        connection
            .execute_batch(
                "INSERT OR IGNORE INTO files_seen(path, session_id) SELECT path, session_id FROM files_fts;",
            )
            .expect("seed");
        record_files(
            &connection,
            &[FileRecord {
                path: "src/main.rs".into(),
                session_id: "s1".into(),
                kind: "write".into(),
                project: String::new(),
                workspace: "/work/a".into(),
            }],
        )
        .expect("record");
        assert_eq!(query_files(&connection, "main", LIMIT).len(), 1);
    }

    #[test]
    fn the_snippet_centres_on_the_match() {
        let body = "alpha ".repeat(40) + "needle-here " + &"omega ".repeat(40);
        let snippet = snippet_for(&body, "needle-here");
        assert!(snippet.contains("needle-here"));
        assert!(snippet.len() < body.len());
    }

    #[test]
    fn the_cleaned_body_drops_the_index_envelope() {
        let raw = "01a081e5-3361-7952-94ce-456eda0dd590\x1f01a081e5\x1fhello from the probe\x1fvalid\x1fhello from the probe\x1f/private/tmp/ws\x1fmeta\x1fmuse-spark-1.3-contributor\x1fthe parser panics on nested generics";
        let clean = clean_search_text(raw);
        assert!(!clean.contains("01a081e5"));
        assert!(!clean.contains("valid"));
        assert!(!clean.contains("/private/tmp/ws"));
        assert!(!clean.contains("muse-spark"));
        assert!(!clean.contains('\u{1f}'));
        assert!(clean.contains("hello from the probe"));
        assert!(clean.contains("nested generics"));
    }

    #[test]
    fn numbers_and_short_hex_words_survive_cleaning() {
        assert_eq!(clean_search_text("42\x1fdeadbee\x1fstatus ok"), "42 deadbee status ok");
    }

    #[test]
    fn the_snippet_window_holds_about_ninety_chars() {
        let body = "alpha ".repeat(40) + "needle-here " + &"omega ".repeat(40);
        let snippet = snippet_for(&clean_search_text(&body), "needle-here");
        assert!(snippet.contains("needle-here"));
        assert!(snippet.chars().count() <= 94);
    }

    #[test]
    fn the_snippet_without_a_match_is_the_first_line() {
        assert_eq!(snippet_for("hello\nworld", "zebra"), "hello world");
        assert_eq!(snippet_for("", "x"), "");
    }

    #[test]
    fn only_write_and_edit_verbs_are_created_files() {
        let target = "src/main.rs";
        assert_eq!(created_target(&ToolKind::Write, target), Some("write"));
        assert_eq!(created_target(&ToolKind::Edit, target), Some("edit"));
        for kind in [
            ToolKind::Read,
            ToolKind::Search,
            ToolKind::Shell,
            ToolKind::Web,
            ToolKind::Browser,
            ToolKind::SubAgent,
            ToolKind::Mcp { server: "muse".into(), tool: "bash".into() },
        ] {
            assert_eq!(created_target(&kind, target), None);
        }
        assert_eq!(created_target(&ToolKind::Write, "   "), None);
    }

    #[test]
    fn escapes_above_the_workspace_are_not_recorded() {
        assert_eq!(relativize("/tmp/ws", "src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(relativize("/tmp/ws", "/tmp/ws/src/main.rs").as_deref(), Some("src/main.rs"));
        assert_eq!(relativize("/tmp/ws", "../evil.rs"), None);
        assert_eq!(relativize("/tmp/ws", "a/../../evil.rs"), None);
        assert_eq!(relativize("/tmp/ws", "/etc/passwd"), None);
        assert_eq!(relativize("/tmp/ws", ""), None);
    }

    #[test]
    fn sync_writes_only_what_changed_and_fetches_only_those_bodies() {
        use std::cell::RefCell;
        use std::collections::HashMap;
        let mut connection = memory();
        let rows = [session_row("a", "", None), session_row("b", "", None), session_row("c", "", None)];
        let ids: Vec<Vec<String>> = rows.iter().map(|row| vec![row.session_id.clone()]).collect();
        let asked: RefCell<Vec<String>> = RefCell::new(Vec::new());
        let bodies = |want: &[String]| -> HashMap<String, String> {
            asked.borrow_mut().extend(want.iter().cloned());
            want.iter().map(|id| (id.clone(), format!("transcript words for {id} kestrel{id}"))).collect()
        };
        let sync = |fingerprints: [i64; 3]| -> Vec<SyncRow<'_>> {
            rows.iter()
                .zip(&ids)
                .zip(fingerprints)
                .map(|((row, body_ids), fingerprint)| SyncRow { row, body_ids, fingerprint })
                .collect()
        };
        let first = sync_sessions(&mut connection, &sync([1, 2, 3]), &bodies).expect("first sync");
        assert_eq!(first, SyncStats { written: 3, removed: 0, unchanged: 0 });
        assert_eq!(query_sessions(&connection, "kestrelb", LIMIT).len(), 1, "a fetched body is searchable");
        asked.borrow_mut().clear();
        let again = sync_sessions(&mut connection, &sync([1, 2, 3]), &bodies).expect("same sync");
        assert_eq!(again, SyncStats { written: 0, removed: 0, unchanged: 3 }, "nothing changed, nothing written");
        assert!(asked.borrow().is_empty(), "no body is read when nothing changed");
        let one = sync_sessions(&mut connection, &sync([1, 20, 3]), &bodies).expect("one changed");
        assert_eq!(one, SyncStats { written: 1, removed: 0, unchanged: 2 });
        assert_eq!(*asked.borrow(), vec!["b".to_owned()], "only the changed row's body is read");
        assert_eq!(query_sessions(&connection, "kestrelb", LIMIT).len(), 1, "a rewritten row is not duplicated");
        let two: Vec<SyncRow<'_>> = sync([1, 20, 3]).into_iter().take(2).collect();
        let gone = sync_sessions(&mut connection, &two, &bodies).expect("one removed");
        assert_eq!(gone, SyncStats { written: 0, removed: 1, unchanged: 2 });
        assert!(query_sessions(&connection, "kestrelc", LIMIT).is_empty(), "a removed session is gone");
    }

    #[test]
    fn a_store_from_before_the_state_table_is_rewritten_once() {
        let mut connection = memory();
        // What an older build left: FTS rows and no `sessions_state`.
        connection
            .execute(
                "INSERT INTO sessions_fts(session_id, label, title, first_prompt, body, project, workspace) VALUES ('old', 'old', '', '', 'stale words', '', NULL)",
                [],
            )
            .expect("old row");
        let row = session_row("new", "fresh words", None);
        let stats = sync_sessions(&mut connection, &[SyncRow { row: &row, body_ids: &[], fingerprint: 7 }], &|_| {
            std::collections::HashMap::new()
        })
        .expect("sync");
        assert_eq!(stats.written, 1);
        assert!(query_sessions(&connection, "stale", LIMIT).is_empty(), "unaddressable old rows are cleared");
        assert_eq!(query_sessions(&connection, "fresh", LIMIT).len(), 1);
    }

    /// Timing on real data, not a gate: `BAAZ_PERF_INDEX` (a copy of Muse's
    /// `session-index.db`) and `BAAZ_PERF_SEARCH` (a copy of `search.db`,
    /// modified in place). Run with `--ignored --nocapture`.
    #[test]
    #[ignore]
    fn perf_sync_against_real_copies() {
        let (Some(index_path), Some(search_path)) =
            (std::env::var_os("BAAZ_PERF_INDEX"), std::env::var_os("BAAZ_PERF_SEARCH"))
        else {
            eprintln!("set BAAZ_PERF_INDEX and BAAZ_PERF_SEARCH");
            return;
        };
        let index_path = std::path::PathBuf::from(index_path);
        let at = std::time::Instant::now();
        let index = crate::index::read_at(&index_path);
        eprintln!("PERF index::read_at rows={} in={:?}", index.len(), at.elapsed());
        let rows: Vec<SessionRow> = index
            .iter()
            .map(|(id, entry)| SessionRow {
                session_id: id.clone(),
                label: entry.label().unwrap_or("").to_owned(),
                title: entry.title.clone(),
                first_prompt: entry.first_user_prompt.clone().unwrap_or_default(),
                body: String::new(),
                project: String::new(),
                workspace: entry.workspace_root.clone(),
            })
            .collect();
        let ids: Vec<Vec<String>> = rows.iter().map(|row| vec![row.session_id.clone()]).collect();
        let fingerprints: Vec<i64> = rows
            .iter()
            .map(|row| index.get(&row.session_id).and_then(|e| e.updated_at_us).unwrap_or(1).max(1))
            .collect();
        let bodies = |want: &[String]| crate::index::read_bodies(&index_path, want);
        let mut connection = open_at(std::path::Path::new(&search_path)).expect("open search copy");
        for (label, bump) in [("first sync (one-time migration)", None), ("unchanged", None), ("one session changed", Some(0usize))] {
            let mut prints = fingerprints.clone();
            if let Some(i) = bump {
                prints[i] += 1;
            }
            let synced: Vec<SyncRow<'_>> = rows
                .iter()
                .zip(&ids)
                .zip(&prints)
                .map(|((row, body_ids), fingerprint)| SyncRow { row, body_ids, fingerprint: *fingerprint })
                .collect();
            let at = std::time::Instant::now();
            let stats = sync_sessions(&mut connection, &synced, &bodies).expect("sync");
            eprintln!("PERF sync {label}: {stats:?} in={:?}", at.elapsed());
        }
        let at = std::time::Instant::now();
        let hits = query_sessions(&connection, "sidebar", LIMIT);
        eprintln!("PERF query 'sidebar' hits={} in={:?}", hits.len(), at.elapsed());
    }
}
