//! Full-text search over sessions and created files (Task E).
//!
//! Two FTS5 tables in a Baaz-owned `search.db` under the support dir:
//!
//! * `sessions_fts(session_id UNINDEXED, label, title, first_prompt, body)` —
//!   fed from [`crate::index::read`] (Muse's `search_text`, which carries
//!   transcript text) plus the `sessions.json` overrides that name the rows.
//!   It is a cache: Muse's index stays the freshness source and this db is
//!   rebuilt off the UI thread at boot and after each index refresh.
//! * `files_fts(path, session_id UNINDEXED, kind)` — "files we created", i.e.
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
const SCHEMA_VERSION: u32 = 2;

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
    /// The session's workspace, canonicalized when it exists.
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
    /// The session's workspace, canonicalized when it exists.
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
    // The smoke query: without FTS5 this fails and the caller treats search
    // as unavailable rather than as "no matches".
    connection.execute_batch(
        "CREATE VIRTUAL TABLE IF NOT EXISTS fts5_smoke USING fts5(x); DROP TABLE fts5_smoke;",
    )?;
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
             CREATE VIRTUAL TABLE sessions_fts USING fts5(session_id UNINDEXED, label, title, first_prompt, body, workspace UNINDEXED);
             CREATE VIRTUAL TABLE files_fts USING fts5(path, session_id UNINDEXED, kind, workspace UNINDEXED);
             CREATE TABLE files_seen (path TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY (path, session_id)) WITHOUT ROWID;",
        )?;
        connection.execute(
            "INSERT OR REPLACE INTO meta(key, value) VALUES ('schema_version', ?1)",
            params![SCHEMA_VERSION.to_string()],
        )?;
    } else {
        connection.execute_batch(
            "CREATE VIRTUAL TABLE IF NOT EXISTS sessions_fts USING fts5(session_id UNINDEXED, label, title, first_prompt, body, workspace UNINDEXED);
             CREATE VIRTUAL TABLE IF NOT EXISTS files_fts USING fts5(path, session_id UNINDEXED, kind, workspace UNINDEXED);",
        )?;
    }
    // Dedupe for `files_fts`, which cannot hold a unique index of its own:
    // an FTS5 table has no constraints, so `(path, session_id)` used to be
    // checked with a `SELECT` per record — a full-text scan each time
    // (finding `support-4`). This ordinary table carries the key, and
    // `INSERT OR IGNORE` on it both dedupes and says whether the row was new.
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS files_seen (path TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY (path, session_id)) WITHOUT ROWID;",
    )?;
    // An index written before this table existed still has its rows; seeding
    // from them is what keeps the first run after an upgrade from recording
    // every known file a second time.
    connection.execute_batch(
        "INSERT OR IGNORE INTO files_seen(path, session_id) SELECT path, session_id FROM files_fts;",
    )?;
    Ok(connection)
}

/// Replace the whole session index with `rows`.
///
/// Deletes-then-inserts in one transaction: a crash leaves the previous
/// index or the next one, never half of each.
pub fn rebuild_sessions(connection: &mut Connection, rows: &[SessionRow]) -> Result<(), rusqlite::Error> {
    let transaction = connection.transaction()?;
    transaction.execute("DELETE FROM sessions_fts", [])?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO sessions_fts(session_id, label, title, first_prompt, body, workspace) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for row in rows {
            insert.execute(params![
                row.session_id,
                row.label,
                row.title,
                row.first_prompt,
                row.body,
                row.workspace
            ])?;
        }
    }
    transaction.commit()
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
        .prepare("INSERT INTO files_fts(path, session_id, kind, workspace) VALUES (?1, ?2, ?3, ?4)")?;
    for record in records {
        // `execute` returns the rows it changed: 0 means the key was already
        // claimed, which is exactly "already recorded".
        if claim.execute(params![record.path, record.session_id])? == 0 {
            continue;
        }
        insert.execute(params![record.path, record.session_id, record.kind, record.workspace])?;
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
            workspace: workspace.map(str::to_owned),
        }
    }

    fn file_record(path: &str, session_id: &str, workspace: &str) -> FileRecord {
        FileRecord { path: path.into(), session_id: session_id.into(), kind: "write".into(), workspace: workspace.into() }
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
                    workspace: Some("/work/harness".into()),
                },
                SessionRow {
                    session_id: "s2".into(),
                    label: "teal-meridian".into(),
                    title: "teal-meridian".into(),
                    first_prompt: String::new(),
                    body: "queued message".into(),
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
                    workspace: None,
                },
                SessionRow {
                    session_id: "s2".into(),
                    label: "gold-perigee".into(),
                    title: "gold-perigee".into(),
                    first_prompt: String::new(),
                    body: String::new(),
                    workspace: None,
                },
                SessionRow {
                    session_id: "s3".into(),
                    label: "silver-annulus".into(),
                    title: "silver-annulus".into(),
                    first_prompt: String::new(),
                    body: String::new(),
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
            |path: &str| FileRecord { path: path.into(), session_id: "s1".into(), kind: "write".into(), workspace: "/work/a".into() };
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
}
