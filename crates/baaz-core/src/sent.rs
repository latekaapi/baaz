//! What the person sent with each turn (D4, docs/41 §2.2; the owner chose to
//! wire it, 2026-10-14).
//!
//! The wire's user message carries text only, so the pictures, files,
//! quotation and Browse annotation batch a turn went out with are Baaz's to
//! remember: one `attachment` row each in `baaz.db` ([`crate::db::attachments`]),
//! with pasted and captured pictures copied into the session's folder
//! (`sent/<session>/<uuid>.<ext>`) so a tile still has its picture after a
//! restart. Each sent turn is keyed by its user turn (D-3): the optimistic
//! bubble's id when it is sent, the provider's own turn id once its echo
//! replaces the bubble ([`bind_turn`]). Turns recorded before turn ids were
//! kept pair with their turn by text, in order.

use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;

use aui_protocol::{Attachment, Turn};
use serde::{Deserialize, Serialize};

/// One sent turn's extras.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SentItem {
    /// The text the turn went out with, which pairs it with its turn.
    pub text: String,
    /// Pictures, files and the quotation, as the turn draws them.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
    /// A Browse annotation batch, when the turn was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub annotations: Option<SentAnnotations>,
    /// The user turn this was sent with: the optimistic bubble's id until
    /// the provider's echo replaces it, then the provider's turn id. `None`
    /// for turns recorded before ids were kept, which pair by text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

/// A Browse annotation batch as sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SentAnnotations {
    /// The page's screenshot, copied into the session's folder.
    #[serde(default)]
    pub screenshot: Option<String>,
    /// The screenshot's size in the pins' coordinates.
    pub size: (f32, f32),
    /// `host/path` of the page.
    pub address: String,
    /// The notes, in pin order.
    pub notes: Vec<SentNote>,
}

/// One annotation as sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SentNote {
    /// The pin's number.
    pub index: usize,
    /// Where the pin sits on the screenshot.
    pub at: (f32, f32),
    /// The element's selector.
    pub selector: String,
    /// What the person wrote.
    pub note: String,
}

/// A session id as a file name: anything outside `[A-Za-z0-9_-]` becomes `_`.
pub(crate) fn file_stem(session_id: &str) -> String {
    session_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The folder a session's sent pictures live in.
pub fn folder(session_id: &str) -> PathBuf {
    crate::store::support_dir().join("sent").join(file_stem(session_id))
}

/// Everything sent in `provider`'s session `session_id` so far, in send
/// order. A database that cannot answer is an empty history, never an error
/// on screen. Blocks on the database's writer.
pub fn load(provider: &str, session_id: &str) -> Vec<SentItem> {
    let (provider, session_id) = (provider.to_owned(), session_id.to_owned());
    crate::db::read(crate::db::path(), move |db| crate::db::attachments::load(&db.conn, &provider, &session_id))
        .and_then(|loaded| loaded.map_err(|error| crate::baaz_log!("db: sent turns do not read ({error})")).ok())
        .unwrap_or_default()
}

/// Record one sent turn's extras, off the calling thread.
pub fn append(provider: &str, session_id: &str, item: SentItem) {
    let (provider, session_id) = (provider.to_owned(), session_id.to_owned());
    let state_dir = crate::store::support_dir();
    crate::db::run(crate::db::path(), move |db| {
        if let Err(error) = crate::db::attachments::append(&mut db.conn, &provider, &session_id, &item, &state_dir) {
            crate::baaz_log!("sent: could not record the turn's attachments: {error}");
        }
    });
}

/// The turn sent as the optimistic bubble `from` is the provider's turn `to`
/// now: its extras follow the id, off the calling thread.
pub fn bind_turn(provider: &str, session_id: &str, from: &str, to: &str) {
    let (provider, session_id, from, to) = (provider.to_owned(), session_id.to_owned(), from.to_owned(), to.to_owned());
    crate::db::run(crate::db::path(), move |db| {
        if let Err(error) = crate::db::attachments::bind_turn(&db.conn, &provider, &session_id, &from, &to) {
            crate::baaz_log!("sent: could not key the turn's attachments to its turn: {error}");
        }
    });
}

/// A session's id changed (a provider draft took the id its child
/// minted): what was sent under `old` joins what `new` already holds, in
/// the database and in the returned list. Pictures stay where they were
/// saved; their paths ride the items. Blocks on the database's writer.
pub fn rekey(provider: &str, old: &str, new: &str, items: Vec<SentItem>) -> Vec<SentItem> {
    if items.is_empty() {
        return load(provider, new);
    }
    let (provider_key, old, new_id) = (provider.to_owned(), old.to_owned(), new.to_owned());
    crate::db::call(crate::db::path(), move |db| {
        if let Err(error) = crate::db::attachments::rekey(&mut db.conn, &provider_key, &old, &new_id) {
            crate::baaz_log!("sent: could not move the turns sent while opening: {error}");
        }
    });
    load(provider, new)
}

/// The files a forgotten session leaves under `state_dir`'s `sent/`: its
/// pictures' folder and the index it had before the database (D-8).
pub fn files_of(state_dir: &std::path::Path, session_id: &str) -> Vec<PathBuf> {
    let stem = file_stem(session_id);
    [state_dir.join("sent").join(&stem), state_dir.join("sent").join(format!("{stem}.json"))]
        .into_iter()
        .filter(|path| path.exists())
        .collect()
}

/// Copies a picture's bytes into the session's folder and returns its path.
pub fn save_picture(session_id: &str, extension: &str, bytes: &[u8]) -> Option<String> {
    let dir = folder(session_id);
    std::fs::create_dir_all(&dir).ok()?;
    let name = format!("{}.{extension}", uuid::Uuid::new_v4().simple());
    let path = dir.join(name);
    std::fs::write(&path, bytes).ok()?;
    Some(path.to_string_lossy().into_owned())
}

/// The extension for an image media type.
pub fn extension_for(media_type: &str) -> &'static str {
    match media_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        _ => "png",
    }
}

/// Pairs the session's user turns with what was sent: a sent turn keyed by
/// a turn's id goes to that turn; the rest pair by text, in order — each
/// goes to the first user turn after the previous match whose text is the
/// same (trimmed). Turns sent before this store existed, or from another
/// app, pair with nothing.
#[cfg(any(test, feature = "test-support"))]
pub fn pair(turns: &[Rc<Turn>], items: &[SentItem]) -> HashMap<String, SentItem> {
    let mut out = HashMap::new();
    pair_from(turns, items, 0, &mut Vec::new(), &mut out);
    out
}

/// Whether `item` waits to be paired by text: it has no turn id, or only the
/// optimistic bubble's (a send that never saw its echo before a restart).
fn pairs_by_text(item: &SentItem) -> bool {
    item.turn_id.as_deref().is_none_or(|id| id.starts_with(OPTIMISTIC_PREFIX))
}

/// The id prefix of the optimistic bubble a send folds before its echo.
pub const OPTIMISTIC_PREFIX: &str = "optimistic-";

/// [`pair`] resumed at turn `from` (speed S4, docs/51 §10): `cursors[i]` is
/// the text-pairing cursor before turn `i`, kept from the last call, so a
/// streaming refresh pairs only the turns it rebuilt instead of the
/// transcript. The caller has already removed `out`'s entries for turns
/// `from..`; `cursors` is cut to `from + 1` here (to `[0]` when it is
/// shorter, i.e. `from` is 0 or nothing was kept) and extended over
/// `turns[from..]`.
pub fn pair_from(
    turns: &[Rc<Turn>],
    items: &[SentItem],
    from: usize,
    cursors: &mut Vec<usize>,
    out: &mut HashMap<String, SentItem>,
) {
    let from = if cursors.len() > from { from } else { 0 };
    if from == 0 {
        cursors.clear();
        cursors.push(0);
    } else {
        cursors.truncate(from + 1);
    }
    let by_text: Vec<usize> = (0..items.len()).filter(|&ix| pairs_by_text(&items[ix])).collect();
    let mut next = cursors[from];
    for turn in &turns[from.min(turns.len())..] {
        if let Turn::User { id, text, .. } = turn.as_ref() {
            let keyed = items.iter().position(|item| item.turn_id.as_deref() == Some(id.as_str()));
            let found = match keyed {
                // A keyed item that would also pair by text (the optimistic
                // bubble's) moves the text cursor past itself.
                Some(ix) => {
                    if let Some(at) = by_text.iter().position(|&candidate| candidate == ix) {
                        next = next.max(at + 1);
                    }
                    Some(ix)
                }
                None => by_text[next.min(by_text.len())..]
                    .iter()
                    .position(|&ix| items[ix].text.trim() == text.trim())
                    .map(|offset| {
                        let ix = by_text[next + offset];
                        next += offset + 1;
                        ix
                    }),
            };
            if let Some(item) = found.map(|ix| &items[ix]) {
                if !item.attachments.is_empty() || item.annotations.is_some() {
                    out.insert(id.clone(), item.clone());
                }
            }
        }
        cursors.push(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::{AttachmentKind, UploadState};

    fn user(id: &str, text: &str) -> Rc<Turn> {
        Rc::new(Turn::User { id: id.into(), text: text.into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None })
    }

    fn sent(text: &str, name: &str) -> SentItem {
        SentItem {
            text: text.into(),
            attachments: vec![Attachment { name: name.into(), kind: AttachmentKind::File, size_bytes: Some(1), meta: None, state: UploadState::Ready, path: None, quote_of: None }],
            annotations: None,
            turn_id: None,
        }
    }

    fn keyed(text: &str, name: &str, turn: &str) -> SentItem {
        SentItem { turn_id: Some(turn.into()), ..sent(text, name) }
    }

    #[test]
    fn turns_pair_with_what_was_sent_by_text_in_order() {
        let turns = vec![user("u1", "hello"), user("u2", "again"), user("u3", "hello")];
        let items = vec![sent("hello", "a.txt"), sent("hello", "b.txt")];
        let paired = pair(&turns, &items);
        assert_eq!(paired["u1"].attachments[0].name, "a.txt");
        assert_eq!(paired["u3"].attachments[0].name, "b.txt");
        assert!(!paired.contains_key("u2"));
    }

    /// D-3: two turns with the same text keep their own attachments,
    /// whatever order they were recorded in and whatever was sent between
    /// them without any: each is keyed by its turn.
    #[test]
    fn two_turns_with_the_same_text_keep_separate_attachments() {
        let turns = vec![user("u1", "same"), user("u2", "same"), user("u3", "same")];
        // Recorded out of order, and the middle turn sent nothing: text
        // pairing would give u1 the second picture and u2 the first.
        let items = vec![keyed("same", "third.png", "u3"), keyed("same", "first.png", "u1")];
        let paired = pair(&turns, &items);
        assert_eq!(paired["u1"].attachments[0].name, "first.png");
        assert_eq!(paired["u3"].attachments[0].name, "third.png");
        assert!(!paired.contains_key("u2"), "the turn that sent nothing shows nothing");
    }

    /// The optimistic bubble shows its attachments by its own id; a send
    /// that never saw its echo before a restart falls back to its text.
    #[test]
    fn an_unbound_optimistic_send_pairs_by_text_after_a_restart() {
        let turns = vec![user("optimistic-r1", "look")];
        let items = vec![keyed("look", "shot.png", "optimistic-r1")];
        assert_eq!(pair(&turns, &items)["optimistic-r1"].attachments[0].name, "shot.png");
        let restarted = vec![user("u-real", "look")];
        assert_eq!(pair(&restarted, &items)["u-real"].attachments[0].name, "shot.png");
    }

    /// Resuming at any turn pairs exactly what a whole pass pairs.
    #[test]
    fn pairing_resumed_from_any_turn_matches_the_whole_pass() {
        let turns = vec![user("u1", "hello"), user("u2", "again"), user("u3", "hello"), user("u4", "again")];
        let items = vec![sent("hello", "a.txt"), keyed("again", "b.txt", "u2"), sent("hello", "c.txt")];
        let whole = pair(&turns, &items);
        for from in 0..=turns.len() {
            let mut cursors = Vec::new();
            let mut out = HashMap::new();
            pair_from(&turns, &items, 0, &mut cursors, &mut out);
            for turn in &turns[from..] {
                out.remove(turn.id());
            }
            pair_from(&turns, &items, from, &mut cursors, &mut out);
            assert_eq!(out, whole, "resumed at {from}");
            assert_eq!(cursors.len(), turns.len() + 1);
        }
    }

    /// A provider draft's sends while opening move to the id its child
    /// minted, beside what that id already held — never dropped; and a
    /// sent turn follows its bubble's id to the provider's turn id.
    #[test]
    fn rekey_moves_what_was_sent_while_opening_and_binding_keys_it_by_turn() {
        let _guard = crate::store::test_env_lock();
        let (old, new) = ("sent-rekey-provisional", "sent-rekey-landed");
        append("codex", new, sent("earlier", "e.txt"));
        append("codex", old, keyed("while opening", "w.txt", "optimistic-r1"));
        let merged = rekey("codex", old, new, load("codex", old));
        let texts: Vec<_> = merged.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["earlier", "while opening"]);
        assert_eq!(load("codex", new), merged, "in the database too");
        assert!(load("codex", old).is_empty(), "nothing is left under the provisional id");
        bind_turn("codex", new, "optimistic-r1", "turn-7");
        let bound = load("codex", new);
        assert_eq!(bound[1].turn_id.as_deref(), Some("turn-7"));
    }

    #[test]
    fn a_session_id_becomes_a_safe_file_name() {
        assert_eq!(file_stem("ses/../x:1"), "ses____x_1");
    }
}
