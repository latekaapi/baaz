//! The `attachment` table: what each turn was sent with, keyed by the
//! session's row and the user turn's id (D-3).
//!
//! A sent turn is one [`SentItem`]: its attachments in order, then its
//! Browse annotation batch when it had one. Each is a row; `ordinal` is the
//! sent turn's place in the session, `position` the row's place in it.

use rusqlite::{params, Connection};

use crate::sent::{SentAnnotations, SentItem};

/// The `kind` of a row that holds a Browse annotation batch.
const ANNOTATIONS: &str = "annotations";

fn kind_name(kind: &aui_protocol::AttachmentKind) -> String {
    serde_json::to_value(kind).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "file".into())
}

/// `path` relative to the state dir when it lies under it (a picture copied
/// into `sent/`), else as stored.
fn relative(path: Option<&str>, state_dir: &std::path::Path) -> Option<String> {
    let path = path?;
    Some(
        std::path::Path::new(path)
            .strip_prefix(state_dir)
            .map(|rel| rel.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_owned()),
    )
}

/// Write one sent turn's rows at `ordinal` under the row `uuid`.
pub(crate) fn insert_item(
    conn: &Connection,
    uuid: &str,
    ordinal: i64,
    item: &SentItem,
    state_dir: &std::path::Path,
) -> rusqlite::Result<()> {
    let mut position = 0i64;
    for attachment in &item.attachments {
        let body = serde_json::to_string(attachment).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        super::exec(conn, 
            "INSERT INTO attachment(id, session_id, turn_id, ordinal, position, sent_text, kind, file_rel_path, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid,
                item.turn_id,
                ordinal,
                position,
                item.text,
                kind_name(&attachment.kind),
                relative(attachment.path.as_deref(), state_dir),
                body,
            ],
        )?;
        position += 1;
    }
    if let Some(annotations) = &item.annotations {
        let body = serde_json::to_string(annotations).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        super::exec(conn, 
            "INSERT INTO attachment(id, session_id, turn_id, ordinal, position, sent_text, kind, file_rel_path, body)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                uuid::Uuid::new_v4().to_string(),
                uuid,
                item.turn_id,
                ordinal,
                position,
                item.text,
                ANNOTATIONS,
                relative(annotations.screenshot.as_deref(), state_dir),
                body,
            ],
        )?;
    }
    Ok(())
}

/// The next free `ordinal` under the row `uuid`.
fn next_ordinal(conn: &Connection, uuid: &str) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COALESCE(MAX(ordinal) + 1, 0) FROM attachment WHERE session_id = ?1",
        params![uuid],
        |row| row.get(0),
    )
}

/// Append one sent turn to `(provider, session_id)`, in one transaction.
pub fn append(
    conn: &mut Connection,
    provider: &str,
    session_id: &str,
    item: &SentItem,
    state_dir: &std::path::Path,
) -> rusqlite::Result<()> {
    super::check_writer("record sent turn");
    let tx = conn.transaction()?;
    let uuid = super::sessions::ensure_session(&tx, provider, session_id, super::sessions::ORIGIN_LISTED)?;
    let ordinal = next_ordinal(&tx, &uuid)?;
    insert_item(&tx, &uuid, ordinal, item, state_dir)?;
    tx.commit()
}

/// Everything sent in `(provider, session_id)`, in send order.
pub fn load(conn: &Connection, provider: &str, session_id: &str) -> rusqlite::Result<Vec<SentItem>> {
    super::check_writer("read sent turns");
    let Some(uuid) = super::sessions::read_row(conn, provider, session_id)? else { return Ok(Vec::new()) };
    let mut statement = conn.prepare_cached(
        "SELECT ordinal, turn_id, sent_text, kind, body FROM attachment WHERE session_id = ?1
         ORDER BY ordinal, position",
    )?;
    let mut rows = statement.query(params![uuid])?;
    let mut items: Vec<(i64, SentItem)> = Vec::new();
    while let Some(row) = rows.next()? {
        let ordinal: i64 = row.get(0)?;
        let turn_id: Option<String> = row.get(1)?;
        let text: String = row.get(2)?;
        let kind: String = row.get(3)?;
        let body: String = row.get(4)?;
        if !matches!(items.last(), Some((last, _)) if *last == ordinal) {
            items.push((ordinal, SentItem { text, attachments: Vec::new(), annotations: None, turn_id }));
        }
        let Some((_, item)) = items.last_mut() else { continue };
        if kind == ANNOTATIONS {
            item.annotations = serde_json::from_str::<SentAnnotations>(&body).ok();
        } else if let Ok(attachment) = serde_json::from_str(&body) {
            item.attachments.push(attachment);
        }
    }
    Ok(items.into_iter().map(|(_, item)| item).collect())
}

/// The sent turn recorded under the user turn id `from` (the optimistic
/// bubble's) now belongs to the provider's turn `to`.
pub fn bind_turn(conn: &Connection, provider: &str, session_id: &str, from: &str, to: &str) -> rusqlite::Result<usize> {
    super::check_writer("bind sent turn");
    let Some(uuid) = super::sessions::read_row(conn, provider, session_id)? else { return Ok(0) };
    super::exec(conn, 
        "UPDATE attachment SET turn_id = ?1 WHERE session_id = ?2 AND turn_id = ?3",
        params![to, uuid, from],
    )
}

/// A session's id changed (a provider draft took the id its child
/// minted): what was sent under `old` joins what `new` holds, after it.
pub fn rekey(conn: &mut Connection, provider: &str, old: &str, new: &str) -> rusqlite::Result<()> {
    super::check_writer("rekey sent turns");
    let tx = conn.transaction()?;
    if let Some(from) = super::sessions::read_row(&tx, provider, old)? {
        let into = super::sessions::ensure_session(&tx, provider, new, super::sessions::ORIGIN_BAAZ)?;
        if from != into {
            let next = next_ordinal(&tx, &into)?;
            super::exec(&tx, 
                "UPDATE attachment SET session_id = ?1, ordinal = ordinal + ?2 WHERE session_id = ?3",
                params![into, next, from],
            )?;
        }
    }
    tx.commit()
}
