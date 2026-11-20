//! The `session` table, and the window's two views of it.
//!
//! The window keeps the session list in memory as two maps keyed by the
//! bare session id: Baaz's facts about each session
//! ([`crate::sessions::Overrides`]) and the provider record of each session
//! Baaz opened or imported ([`crate::provider_sessions::ProviderSessionStore`]).
//! Both are one `session` row each: [`load_front`] builds the maps from the
//! rows, and [`save_meta`] / [`save_records`] write a snapshot of one map
//! back, only the rows that changed since the last snapshot, in one
//! transaction ([`FrontState`] holds that last snapshot).
//!
//! A bare id names one row for the window ([`FrontState::owned`]). The
//! table can hold the same id under two providers (D-10); the window shows
//! the row its provider record names, else the newest, and leaves the other
//! untouched.

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension as _};

use crate::provider_sessions::{ProviderSessionRecord, ProviderSessionStore};
use crate::sessions::{Overrides, SessionMeta};

/// What the window last wrote to (or read from) one database.
#[derive(Default)]
pub(crate) struct FrontState {
    /// Read or written once: the snapshots below stand for the table.
    loaded: bool,
    /// The row each bare id stands for in the window: its uuid.
    owned: HashMap<String, String>,
    /// The last facts snapshot written.
    meta: Overrides,
    /// The last records snapshot written.
    records: ProviderSessionStore,
}

/// The `origin` of a row the window holds no record for: a Muse session
/// listed by Muse, never opened here.
pub const ORIGIN_LISTED: &str = "listed";
/// The `origin` of a session Baaz started.
pub const ORIGIN_BAAZ: &str = "baaz";

/// The row for `(provider, id)`, made when missing. `provider` `''` asks
/// for whatever row the id already has (the record's first, then the
/// newest), else makes an unattributed one. A named provider takes the
/// exact row, else adopts the id's unattributed row (re-keying it in
/// place, same uuid), else makes one.
pub fn ensure_session(conn: &Connection, provider: &str, id: &str, origin: &str) -> rusqlite::Result<String> {
    if provider.is_empty() {
        if let Some(uuid) = any_row(conn, id)? {
            return Ok(uuid);
        }
    } else {
        if let Some(uuid) = exact_row(conn, provider, id)? {
            return Ok(uuid);
        }
        if let Some(uuid) = exact_row(conn, "", id)? {
            conn.execute("UPDATE session SET provider_key = ?1 WHERE id = ?2", params![provider, uuid])?;
            return Ok(uuid);
        }
    }
    let uuid = uuid::Uuid::new_v4().to_string();
    let now = super::now_ms();
    conn.execute(
        "INSERT INTO session(provider_key, provider_session_id, id, origin, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![provider, id, uuid, origin, now],
    )?;
    Ok(uuid)
}

/// The uuid of exactly `(provider, id)`.
pub fn exact_row(conn: &Connection, provider: &str, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM session WHERE provider_key = ?1 AND provider_session_id = ?2",
        params![provider, id],
        |row| row.get(0),
    )
    .optional()
}

/// The row a reader of `(provider, id)` means: the exact row, else the id's
/// unattributed one.
pub fn read_row(conn: &Connection, provider: &str, id: &str) -> rusqlite::Result<Option<String>> {
    if let Some(uuid) = exact_row(conn, provider, id)? {
        return Ok(Some(uuid));
    }
    exact_row(conn, "", id)
}

/// Any row for the bare `id`: the one with a record first, then the one the
/// window holds facts for, then the newest.
fn any_row(conn: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT id FROM session WHERE provider_session_id = ?1
         ORDER BY has_record DESC, has_meta DESC, updated_at DESC LIMIT 1",
        params![id],
        |row| row.get(0),
    )
    .optional()
}

/// Move everything `from` holds onto `into` and delete `from`: the
/// window's facts (when `into` has none), its worktree (likewise), its
/// turns and its attachments.
fn absorb(conn: &Connection, from: &str, into: &str) -> rusqlite::Result<()> {
    if from == into {
        return Ok(());
    }
    conn.execute(
        "UPDATE session SET worktree_id = (SELECT worktree_id FROM session WHERE id = ?2)
         WHERE id = ?1 AND worktree_id IS NULL",
        params![into, from],
    )?;
    let into_has_meta: bool =
        conn.query_row("SELECT has_meta FROM session WHERE id = ?1", params![into], |row| row.get(0))?;
    if !into_has_meta {
        if let Some(meta) = read_meta(conn, from)? {
            write_meta(conn, into, Some(&meta))?;
        }
    }
    conn.execute("UPDATE OR IGNORE turn SET session_id = ?1 WHERE session_id = ?2", params![into, from])?;
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM attachment WHERE session_id = ?1",
        params![into],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE attachment SET session_id = ?1, ordinal = ordinal + ?2 WHERE session_id = ?3",
        params![into, next, from],
    )?;
    conn.execute("DELETE FROM session WHERE id = ?1", params![from])?;
    Ok(())
}

/// The row the window's `id` stands for, made or re-pointed so it serves
/// `provider` when one is given (a record's provider).
fn owned_row(
    conn: &Connection,
    owned: &mut HashMap<String, String>,
    id: &str,
    provider: Option<&str>,
    origin: &str,
) -> rusqlite::Result<String> {
    if let Some(uuid) = owned.get(id).cloned() {
        let key: Option<String> =
            conn.query_row("SELECT provider_key FROM session WHERE id = ?1", params![uuid], |row| row.get(0)).optional()?;
        match (key, provider) {
            (Some(_), None) => return Ok(uuid),
            (Some(key), Some(provider)) if key == provider => return Ok(uuid),
            (Some(key), Some(provider)) if key.is_empty() => {
                // The first record for an unattributed row: re-key it, or
                // fold it into the row that provider already has.
                match exact_row(conn, provider, id)? {
                    Some(existing) => {
                        absorb(conn, &uuid, &existing)?;
                        owned.insert(id.to_owned(), existing.clone());
                        return Ok(existing);
                    }
                    None => {
                        conn.execute("UPDATE session SET provider_key = ?1 WHERE id = ?2", params![provider, uuid])?;
                        return Ok(uuid);
                    }
                }
            }
            (Some(_), Some(provider)) => {
                // The window's id now names another provider's session (the
                // collision a bare id cannot tell apart): the window's facts
                // move to that provider's row; the old row keeps everything
                // else and leaves the list.
                let target = ensure_session(conn, provider, id, origin)?;
                if let Some(meta) = read_meta(conn, &uuid)? {
                    write_meta(conn, &target, Some(&meta))?;
                }
                conn.execute("UPDATE session SET has_meta = 0, has_record = 0 WHERE id = ?1", params![uuid])?;
                owned.insert(id.to_owned(), target.clone());
                return Ok(target);
            }
            (None, _) => {
                owned.remove(id);
            }
        }
    }
    let uuid = ensure_session(conn, provider.unwrap_or(""), id, origin)?;
    owned.insert(id.to_owned(), uuid.clone());
    Ok(uuid)
}

/// Write (or clear, with `None`) the window's facts on a row.
pub(crate) fn write_meta(conn: &Connection, uuid: &str, meta: Option<&SessionMeta>) -> rusqlite::Result<()> {
    let Some(m) = meta else {
        conn.execute(
            "UPDATE session SET has_meta = 0, name = NULL, handoff_title = NULL, generated_title = NULL,
                provider_title = NULL, first_prompt = NULL, derived_title = NULL, title_attempted = 0, pinned = 0,
                archived = 0, hidden = 0, side_session = 0, browser_chat = 0, unread = 0, handoff_to = NULL,
                handoff_from = NULL, handoff_from_provider = NULL, last_summary = NULL, last_ask = NULL,
                last_error = NULL, right_state = NULL, project_id = NULL
             WHERE id = ?1",
            params![uuid],
        )?;
        return Ok(());
    };
    let right = m.right.as_ref().and_then(|right| serde_json::to_string(right).ok());
    conn.execute(
        "UPDATE session SET has_meta = 1, name = ?2, handoff_title = ?3, generated_title = ?4, provider_title = ?5,
            first_prompt = ?6, derived_title = ?7, title_attempted = ?8, pinned = ?9, archived = ?10, hidden = ?11,
            side_session = ?12, browser_chat = ?13, unread = ?14, handoff_to = ?15, handoff_from = ?16,
            handoff_from_provider = ?17, last_summary = ?18, last_ask = ?19, last_error = ?20, right_state = ?21,
            project_id = ?22
         WHERE id = ?1",
        params![
            uuid,
            m.name,
            m.handoff_title,
            m.generated_title,
            m.provider_title,
            m.first_prompt,
            m.derived_title,
            m.title_attempted,
            m.pinned,
            m.archived,
            m.hidden,
            m.side_session,
            m.browser_chat,
            m.unread,
            m.handoff_to,
            m.handoff_from,
            m.handoff_from_provider,
            m.last_summary,
            m.last_ask,
            m.last_error,
            right,
            m.project,
        ],
    )?;
    Ok(())
}

/// Write (or clear, with `None`) the window's record of a row.
pub(crate) fn write_record(conn: &Connection, uuid: &str, record: Option<&ProviderSessionRecord>) -> rusqlite::Result<()> {
    let Some(r) = record else {
        conn.execute("UPDATE session SET has_record = 0 WHERE id = ?1", params![uuid])?;
        return Ok(());
    };
    conn.execute(
        "UPDATE session SET has_record = 1, workspace_path = ?2, created_at = ?3, updated_at = ?4, turns = ?5,
            origin = ?6
         WHERE id = ?1",
        params![
            uuid,
            r.workspace,
            r.created_ms,
            r.updated_ms,
            r.turns as i64,
            r.origin.as_deref().unwrap_or(ORIGIN_BAAZ),
        ],
    )?;
    Ok(())
}

const META_COLUMNS: &str = "name, handoff_title, generated_title, provider_title, first_prompt, derived_title,
    title_attempted, pinned, archived, hidden, side_session, browser_chat, unread, handoff_to, handoff_from,
    handoff_from_provider, last_summary, last_ask, last_error, right_state, project_id";

fn meta_from(row: &rusqlite::Row, at: usize) -> rusqlite::Result<SessionMeta> {
    let right: Option<String> = row.get(at + 19)?;
    Ok(SessionMeta {
        name: row.get(at)?,
        handoff_title: row.get(at + 1)?,
        generated_title: row.get(at + 2)?,
        provider_title: row.get(at + 3)?,
        first_prompt: row.get(at + 4)?,
        derived_title: row.get(at + 5)?,
        title_attempted: row.get(at + 6)?,
        pinned: row.get(at + 7)?,
        archived: row.get(at + 8)?,
        hidden: row.get(at + 9)?,
        side_session: row.get(at + 10)?,
        browser_chat: row.get(at + 11)?,
        unread: row.get(at + 12)?,
        handoff_to: row.get(at + 13)?,
        handoff_from: row.get(at + 14)?,
        handoff_from_provider: row.get(at + 15)?,
        last_summary: row.get(at + 16)?,
        last_ask: row.get(at + 17)?,
        last_error: row.get(at + 18)?,
        right: right.and_then(|text| serde_json::from_str(&text).ok()),
        project: row.get(at + 20)?,
    })
}

/// The window's facts on one row, when it holds any.
fn read_meta(conn: &Connection, uuid: &str) -> rusqlite::Result<Option<SessionMeta>> {
    conn.query_row(
        &format!("SELECT has_meta, {META_COLUMNS} FROM session WHERE id = ?1"),
        params![uuid],
        |row| {
            let has: bool = row.get(0)?;
            if has {
                meta_from(row, 1).map(Some)
            } else {
                Ok(None)
            }
        },
    )
    .optional()
    .map(Option::flatten)
}

/// Both of the window's maps, read from the table, and the snapshots the
/// next saves diff against.
pub(crate) fn load_front(conn: &Connection, front: &mut FrontState) -> rusqlite::Result<(Overrides, ProviderSessionStore)> {
    super::check_writer("load sessions");
    let mut statement = conn.prepare(&format!(
        "SELECT id, provider_key, provider_session_id, has_meta, has_record, workspace_path, created_at, updated_at,
                turns, origin, {META_COLUMNS}
         FROM session WHERE has_meta = 1 OR has_record = 1
         ORDER BY has_record DESC, updated_at DESC"
    ))?;
    let mut owned = HashMap::new();
    let mut meta = Overrides::new();
    let mut records = ProviderSessionStore::new();
    let mut shadowed = 0usize;
    let mut rows = statement.query([])?;
    while let Some(row) = rows.next()? {
        let uuid: String = row.get(0)?;
        let provider: String = row.get(1)?;
        let id: String = row.get(2)?;
        if owned.contains_key(&id) {
            shadowed += 1;
            continue;
        }
        let has_meta: bool = row.get(3)?;
        let has_record: bool = row.get(4)?;
        if has_meta {
            meta.insert(id.clone(), meta_from(row, 10)?);
        }
        if has_record {
            let origin: String = row.get(9)?;
            records.insert(
                id.clone(),
                ProviderSessionRecord {
                    provider,
                    session_id: id.clone(),
                    workspace: row.get(5)?,
                    created_ms: row.get::<_, Option<i64>>(6)?.unwrap_or(0),
                    updated_ms: row.get::<_, Option<i64>>(7)?.unwrap_or(0),
                    turns: row.get::<_, i64>(8)?.max(0) as u64,
                    origin: (origin != ORIGIN_BAAZ).then_some(origin),
                },
            );
        }
        owned.insert(id, uuid);
    }
    if shadowed > 0 {
        crate::baaz_log!("db: {shadowed} session rows share an id with a listed row under another provider; the list shows one");
    }
    front.loaded = true;
    front.owned = owned;
    front.meta = meta.clone();
    front.records = records.clone();
    Ok((meta, records))
}

/// Write the window's facts snapshot: the rows whose facts changed, and the
/// rows whose facts left the snapshot, in one transaction.
pub(crate) fn save_meta(conn: &mut Connection, front: &mut FrontState, snapshot: Overrides) -> rusqlite::Result<()> {
    super::check_writer("save sessions");
    if !front.loaded {
        load_front(conn, front)?;
    }
    let tx = conn.transaction()?;
    for (id, meta) in &snapshot {
        if front.meta.get(id) == Some(meta) && front.owned.contains_key(id) {
            continue;
        }
        let provider = front.records.get(id).map(|record| record.provider.clone());
        let uuid = owned_row(&tx, &mut front.owned, id, provider.as_deref(), ORIGIN_LISTED)?;
        write_meta(&tx, &uuid, Some(meta))?;
    }
    for id in front.meta.keys().filter(|id| !snapshot.contains_key(*id)) {
        if let Some(uuid) = front.owned.get(id).cloned() {
            write_meta(&tx, &uuid, None)?;
            prune(&tx, &mut front.owned, id, &uuid)?;
        }
    }
    tx.commit()?;
    front.meta = snapshot;
    Ok(())
}

/// Write the window's records snapshot, like [`save_meta`].
pub(crate) fn save_records(
    conn: &mut Connection,
    front: &mut FrontState,
    snapshot: ProviderSessionStore,
) -> rusqlite::Result<()> {
    super::check_writer("save records");
    if !front.loaded {
        load_front(conn, front)?;
    }
    let tx = conn.transaction()?;
    for (id, record) in &snapshot {
        if front.records.get(id) == Some(record) && front.owned.contains_key(id) {
            continue;
        }
        let origin = record.origin.as_deref().unwrap_or(ORIGIN_BAAZ);
        let uuid = owned_row(&tx, &mut front.owned, id, Some(&record.provider), origin)?;
        write_record(&tx, &uuid, Some(record))?;
        // A row re-pointed at another provider carries the facts over.
        if let Some(meta) = front.meta.get(id) {
            if read_meta(&tx, &uuid)?.as_ref() != Some(meta) {
                write_meta(&tx, &uuid, Some(meta))?;
            }
        }
    }
    for id in front.records.keys().filter(|id| !snapshot.contains_key(*id)) {
        if let Some(uuid) = front.owned.get(id).cloned() {
            write_record(&tx, &uuid, None)?;
            prune(&tx, &mut front.owned, id, &uuid)?;
        }
    }
    tx.commit()?;
    front.records = snapshot;
    Ok(())
}

/// A row the window no longer holds either view of leaves the list; it is
/// deleted only when nothing else hangs off it (no turn, no attachment).
fn prune(conn: &Connection, owned: &mut HashMap<String, String>, id: &str, uuid: &str) -> rusqlite::Result<()> {
    let held: bool = conn.query_row(
        "SELECT has_meta OR has_record FROM session WHERE id = ?1",
        params![uuid],
        |row| row.get(0),
    )
    .optional()?
    .unwrap_or(false);
    if held {
        return Ok(());
    }
    owned.remove(id);
    conn.execute(
        "DELETE FROM session WHERE id = ?1
           AND NOT EXISTS (SELECT 1 FROM turn WHERE session_id = ?1)
           AND NOT EXISTS (SELECT 1 FROM attachment WHERE session_id = ?1)",
        params![uuid],
    )?;
    Ok(())
}

/// Forget a session for good: its row and everything hanging off it
/// (turns, attachments), every provider's row for that bare id the window
/// stood for. Returns how many rows went.
pub(crate) fn forget(conn: &mut Connection, front: &mut FrontState, provider: &str, id: &str) -> rusqlite::Result<usize> {
    super::check_writer("forget session");
    let tx = conn.transaction()?;
    let mut gone = 0;
    for key in [provider, ""] {
        if let Some(uuid) = exact_row(&tx, key, id)? {
            gone += tx.execute("DELETE FROM session WHERE id = ?1", params![uuid])?;
        }
    }
    if let Some(uuid) = front.owned.get(id).cloned() {
        gone += tx.execute("DELETE FROM session WHERE id = ?1", params![uuid])?;
    }
    tx.commit()?;
    front.owned.remove(id);
    front.meta.remove(id);
    front.records.remove(id);
    Ok(gone)
}
