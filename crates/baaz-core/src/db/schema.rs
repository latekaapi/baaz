//! The database's tables and the steps between their versions.
//!
//! `PRAGMA user_version` says which step a file has reached; each step is
//! one transaction that ends by stamping its number, so an interrupted step
//! leaves the previous version whole. A file stamped with a version this
//! build has never seen is refused (never rewritten): a newer Baaz wrote it.
//!
//! The usage ledger predates the database. It kept its own version in
//! `meta.schema_version` (1–3, `crate::usage`); a file that has one is
//! brought to the ledger's version 3 first, and step 1 copies its rows into
//! [`TURN`](V1) and leaves the old table where it was.
//!
//! What the tables hold (docs/81 B §c.2, as built):
//!
//! * `provider`: the three providers, for the foreign keys.
//! * `account`: who paid. Empty until something records an account per
//!   turn; `turn.account_id` points here.
//! * `worktree`: a session's worktree (docs/82 6.1, [`crate::worktrees`]):
//!   its folder, branch, the commit it was cut from, the project root it
//!   belongs to, its block of ten ports (unique among rows) and when its
//!   setup script ran; while it is archived (docs/82 6.2), when, and the
//!   hidden ref and branch head its snapshot is. `session.worktree_id`
//!   points here.
//! * `session`: one row per session, keyed `(provider_key,
//!   provider_session_id)`: the same id under two providers is two rows
//!   (D-10). `id` is Baaz's own uuid, what every child row points at.
//!   `provider_key` is `''` for a session Baaz has no provider record for
//!   yet (a Muse row never opened here, a draft): the first record re-keys
//!   that row in place. Every title is a column of this row
//!   (`SessionMeta::title`). `has_meta` and `has_record` say which of the
//!   window's two views of the row ([`crate::sessions::Overrides`],
//!   [`crate::provider_sessions::ProviderSessionStore`]) hold it; a row
//!   with neither (one the ledger or a sent turn made) is not listed.
//! * `turn`: one row per turn, keyed by the session's uuid and the
//!   provider's turn id. An assistant turn carries the ledger (tokens, cost:
//!   `cost_usd` is `NULL` with `cost_source = 'unknown'` when the provider
//!   never says, never a `0.0` that reads as free); a user turn carries the
//!   bubble text its submit showed (`display_text`) and the text the
//!   provider echoes (`input_text`).
//! * `attachment`: what a turn was sent with, one row per attachment,
//!   keyed by the user turn's id (`NULL` for rows imported from before
//!   turns were known); `ordinal` is the sent turn's place in the session,
//!   `position` the attachment's place in it.
//! * `prompt_history`: the composer's prompt history per workspace.
//! * `import_job` / `import_item`: the import journal (what Undo removes);
//!   an item names its session by provider and id, so it can name a row
//!   that has not landed yet.
//! * `migration_journal`: the session migration's crash-resume record.
//! * `meta`: key/value (`json_import`: when the JSON stores were imported).
//!
//! Not here: `projects.json` (projects are not keyed by session, and the
//! plan row moves only the session stores), the regenerable caches and
//! preferences, the hand-off snapshots (copies of a transcript, kept as
//! files) and the transcripts themselves (the providers' own files).

use rusqlite::{params, Connection, Transaction};

/// The schema this build writes.
pub const VERSION: i32 = 5;

/// Step 1: every table.
const V1: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE provider(
    key TEXT PRIMARY KEY,
    label TEXT NOT NULL
);
CREATE TABLE account(
    id TEXT PRIMARY KEY,
    provider_key TEXT NOT NULL REFERENCES provider(key),
    lane TEXT,
    label TEXT,
    plan TEXT,
    observed_at INTEGER
);
CREATE TABLE worktree(
    id TEXT PRIMARY KEY,
    project_id TEXT,
    path TEXT NOT NULL UNIQUE,
    branch TEXT,
    created_by_session TEXT
);
CREATE TABLE session(
    provider_key TEXT NOT NULL,
    provider_session_id TEXT NOT NULL,
    id TEXT NOT NULL UNIQUE,
    project_id TEXT,
    worktree_id TEXT REFERENCES worktree(id),
    workspace_path TEXT,
    origin TEXT NOT NULL DEFAULT 'baaz',
    name TEXT,
    handoff_title TEXT,
    generated_title TEXT,
    provider_title TEXT,
    first_prompt TEXT,
    derived_title TEXT,
    title_attempted INTEGER NOT NULL DEFAULT 0,
    pinned INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    hidden INTEGER NOT NULL DEFAULT 0,
    side_session INTEGER NOT NULL DEFAULT 0,
    browser_chat INTEGER NOT NULL DEFAULT 0,
    unread INTEGER NOT NULL DEFAULT 0,
    handoff_to TEXT,
    handoff_from TEXT,
    handoff_from_provider TEXT,
    last_summary TEXT,
    last_ask TEXT,
    last_error TEXT,
    right_state TEXT,
    transcript_locator TEXT,
    has_meta INTEGER NOT NULL DEFAULT 0,
    has_record INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER,
    updated_at INTEGER,
    turns INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(provider_key, provider_session_id)
);
CREATE INDEX session_by_bare_id ON session(provider_session_id);
CREATE TABLE turn(
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_id TEXT NOT NULL,
    role TEXT NOT NULL DEFAULT 'assistant',
    view_cursor TEXT,
    finished_at INTEGER,
    model TEXT,
    account_id TEXT REFERENCES account(id),
    duration_ms INTEGER,
    tokens_in INTEGER,
    tokens_out INTEGER,
    reasoning INTEGER,
    cache_read INTEGER,
    cache_write INTEGER,
    cached_tokens INTEGER,
    cost_usd REAL,
    cost_source TEXT NOT NULL DEFAULT 'unknown',
    provider TEXT NOT NULL DEFAULT '',
    input_text TEXT,
    display_text TEXT,
    PRIMARY KEY(session_id, turn_id)
);
CREATE TABLE attachment(
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL REFERENCES session(id) ON DELETE CASCADE,
    turn_id TEXT,
    ordinal INTEGER NOT NULL,
    position INTEGER NOT NULL,
    sent_text TEXT NOT NULL,
    kind TEXT NOT NULL,
    file_rel_path TEXT,
    body TEXT NOT NULL
);
CREATE INDEX attachment_by_session ON attachment(session_id, ordinal, position);
CREATE TABLE prompt_history(
    workspace_path TEXT NOT NULL,
    ordinal INTEGER NOT NULL,
    text TEXT NOT NULL,
    PRIMARY KEY(workspace_path, ordinal)
);
CREATE TABLE import_job(
    id TEXT PRIMARY KEY,
    started_at INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT 'homes'
);
CREATE TABLE import_item(
    job_id TEXT NOT NULL REFERENCES import_job(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    provider_key TEXT NOT NULL,
    provider_session_id TEXT NOT NULL,
    PRIMARY KEY(job_id, ordinal)
);
CREATE TABLE migration_journal(
    key TEXT PRIMARY KEY,
    state TEXT NOT NULL,
    at INTEGER NOT NULL,
    outcome TEXT
);
INSERT INTO provider(key, label) VALUES
    ('muse', 'Muse'), ('claude-code', 'Claude Code'), ('codex', 'Codex');
";

/// Step 2 (docs/82 6.1): what a worktree row needs beyond its path and
/// branch. `port` is the first of the worktree's block of ten; two live
/// rows never share one.
const V2: &str = "
ALTER TABLE worktree ADD COLUMN project_root TEXT;
ALTER TABLE worktree ADD COLUMN base TEXT;
ALTER TABLE worktree ADD COLUMN port INTEGER;
ALTER TABLE worktree ADD COLUMN created_at INTEGER;
ALTER TABLE worktree ADD COLUMN setup_at INTEGER;
CREATE UNIQUE INDEX worktree_by_port ON worktree(port);
";

/// Step 3 (docs/82 6.2): an archived worktree's snapshot. `archived_at`
/// is set while its folder is gone; `preserved_ref` names the hidden ref
/// holding its uncommitted work and `preserved_head` the commit its branch
/// was on.
const V3: &str = "
ALTER TABLE worktree ADD COLUMN archived_at INTEGER;
ALTER TABLE worktree ADD COLUMN preserved_ref TEXT;
ALTER TABLE worktree ADD COLUMN preserved_head TEXT;
";

/// Version 4 (docs/85 10.8): the ledger's reads by time — the usage page,
/// "this week" — get their indexes. The legacy `usage_turns` copy goes too,
/// but only when every row of it is already in `turn` ([`drop_copied_ledger`]):
/// a ledger this build could not read was never copied and stays.
const V4: &str = "
CREATE INDEX IF NOT EXISTS turn_finished ON turn(finished_at);
CREATE INDEX IF NOT EXISTS turn_provider_finished ON turn(provider, finished_at);
";

/// Version 5 (docs/85 11.2): Muse's listing and index facts move into the
/// `session` table ([`super::listing`]), so baaz.db is the one list source.
/// Added only where missing, so the step is safe to run again.
const V5: &[(&str, &str)] = &[
    ("listing", "TEXT"),
    ("listing_order", "INTEGER"),
    ("index_facts", "TEXT"),
    ("created_by", "TEXT"),
];

/// Drop the legacy `usage_turns` when nothing in it is missing from `turn`.
/// A table of an unknown shape fails the check and is kept.
fn drop_copied_ledger(tx: &Transaction) -> rusqlite::Result<()> {
    let exists = tx.query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'usage_turns'", [], |_| Ok(())).is_ok();
    if !exists {
        return Ok(());
    }
    let missing = tx.query_row(
        "SELECT count(*) FROM usage_turns u WHERE NOT EXISTS (
             SELECT 1 FROM turn t JOIN session s ON s.id = t.session_id
             WHERE s.provider_session_id = u.session_id AND t.turn_id = u.turn_id)",
        [],
        |row| row.get::<_, i64>(0),
    );
    if missing == Ok(0) {
        tx.execute_batch("DROP TABLE usage_turns;")?;
    }
    Ok(())
}

/// Bring `conn` to [`VERSION`]: the ledger's own versions first, then every
/// step this file has not run, each in its own transaction.
pub fn migrate(conn: &mut Connection) -> rusqlite::Result<()> {
    super::check_writer("migrate");
    let version: i32 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > VERSION {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "baaz.db schema version {version} is newer than this build ({VERSION})"
        )));
    }
    if version < 1 {
        // The ledger's own steps run first so its rows copy in their final
        // shape. A ledger this build cannot read is left alone and not
        // copied; the rest of the database works without it.
        let ledger = crate::usage::upgrade_legacy_ledger(conn);
        if let Err(error) = &ledger {
            crate::baaz_log!("db: the old usage ledger is not readable ({error}); it is left as it is and not copied");
        }
        let tx = conn.transaction()?;
        tx.execute_batch(V1)?;
        if matches!(ledger, Ok(true)) {
            copy_ledger(&tx)?;
        }
        tx.pragma_update(None, "user_version", 1)?;
        tx.commit()?;
    }
    if version < 2 {
        let tx = conn.transaction()?;
        tx.execute_batch(V2)?;
        tx.pragma_update(None, "user_version", 2)?;
        tx.commit()?;
    }
    if version < 3 {
        let tx = conn.transaction()?;
        tx.execute_batch(V3)?;
        tx.pragma_update(None, "user_version", 3)?;
        tx.commit()?;
    }
    if version < 4 {
        let tx = conn.transaction()?;
        tx.execute_batch(V4)?;
        drop_copied_ledger(&tx)?;
        tx.pragma_update(None, "user_version", 4)?;
        tx.commit()?;
    }
    if version < 5 {
        let tx = conn.transaction()?;
        let have: Vec<String> = {
            let mut columns = tx.prepare("SELECT name FROM pragma_table_info('session')")?;
            let names = columns.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            names
        };
        for (column, kind) in V5 {
            if !have.iter().any(|name| name == column) {
                tx.execute_batch(&format!("ALTER TABLE session ADD COLUMN {column} {kind};"))?;
            }
        }
        tx.pragma_update(None, "user_version", 5)?;
        tx.commit()?;
    }
    Ok(())
}

/// Copy the ledger's rows into `turn`, each under its session's row (made
/// when missing). The old table stays.
fn copy_ledger(tx: &Transaction) -> rusqlite::Result<()> {
    let mut rows = tx.prepare(
        "SELECT session_id, view_cursor, turn_id, finished_at_ms, model, duration_ms, tokens_in, tokens_out,
                reasoning_tokens, cache_read_tokens, cache_write_tokens, cached_tokens, cost_usd, provider
         FROM usage_turns ORDER BY finished_at_ms",
    )?;
    let ledger = rows
        .query_map([], |row| {
            Ok(crate::usage::UsageRow {
                session_id: row.get(0)?,
                view_cursor: row.get(1)?,
                turn_id: row.get(2)?,
                finished_at_ms: row.get(3)?,
                model: row.get(4)?,
                duration_ms: row.get(5)?,
                tokens_in: row.get(6)?,
                tokens_out: row.get(7)?,
                reasoning_tokens: row.get(8)?,
                cache_read_tokens: row.get(9)?,
                cache_write_tokens: row.get(10)?,
                cached_tokens: row.get(11)?,
                cost_usd: row.get(12)?,
                provider: row.get(13)?,
                compaction: false,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(rows);
    for row in &ledger {
        super::turns::record_ledger_row(tx, row)?;
    }
    Ok(())
}

/// The key `json_import` writes when the JSON stores are in.
pub const IMPORTED_KEY: &str = "json_import";

/// A `meta` value.
pub fn meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    use rusqlite::OptionalExtension as _;
    conn.query_row("SELECT value FROM meta WHERE key = ?1", params![key], |row| row.get(0)).optional()
}

/// Set a `meta` value.
pub fn set_meta(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute("INSERT OR REPLACE INTO meta(key, value) VALUES (?1, ?2)", params![key, value])?;
    Ok(())
}
