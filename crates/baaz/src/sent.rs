//! What the person sent with each turn (D4, docs/41 §2.2; the owner chose to
//! wire it, 2026-10-14).
//!
//! The wire's user message carries text only, so the pictures, files,
//! quotation and Browse annotation batch a turn went out with are Baaz's to
//! remember. One JSON file per session under the support dir
//! (`sent/<session>.json`) holds them in send order, with pasted and captured
//! pictures copied beside it (`sent/<session>/<n>.<ext>`) so a tile still
//! has its picture after a restart. Turns are paired with what was sent by
//! their text, in order — the provider's own echo replaces the optimistic
//! bubble, and both carry the text that was typed.

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

impl SentAnnotations {
    /// The library's batch for the turn.
    pub fn batch(&self) -> aui::transcript::AnnotationBatch {
        aui::transcript::AnnotationBatch {
            screenshot: self.screenshot.clone(),
            size: self.size,
            address: self.address.clone(),
            notes: self
                .notes
                .iter()
                .map(|n| aui::transcript::AnnotationNote { index: n.index, at: n.at, selector: n.selector.clone(), note: n.note.clone() })
                .collect(),
        }
    }
}

/// A session id as a file name: anything outside `[A-Za-z0-9_-]` becomes `_`.
fn file_stem(session_id: &str) -> String {
    session_id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect()
}

/// The folder a session's sent pictures live in.
pub fn folder(session_id: &str) -> PathBuf {
    crate::store::support_dir().join("sent").join(file_stem(session_id))
}

fn index_path(session_id: &str) -> PathBuf {
    crate::store::support_dir().join("sent").join(format!("{}.json", file_stem(session_id)))
}

/// Everything sent in a session so far, in send order. A missing or
/// unreadable file is an empty history, never an error on screen.
pub fn load(session_id: &str) -> Vec<SentItem> {
    std::fs::read(index_path(session_id)).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

/// Appends one sent turn's extras and writes the session's file.
pub fn append(session_id: &str, item: SentItem) -> std::io::Result<()> {
    let mut items = load(session_id);
    items.push(item);
    write_index(session_id, &items)
}

/// A session's id changed (a provider draft took the id its child
/// minted): `items` — what was sent under `old` — join what `new` already
/// holds, on disk and in the returned list, and `old`'s index goes.
/// Pictures stay where they were saved; their paths ride the items.
pub fn rekey(old: &str, new: &str, items: Vec<SentItem>) -> Vec<SentItem> {
    let mut merged = load(new);
    if items.is_empty() {
        return merged;
    }
    merged.extend(items);
    if let Err(error) = write_index(new, &merged) {
        crate::baaz_log!("sent: could not move the turns sent while opening: {error}");
    } else {
        let _ = std::fs::remove_file(index_path(old));
    }
    merged
}

fn write_index(session_id: &str, items: &[SentItem]) -> std::io::Result<()> {
    let path = index_path(session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec(items).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(tmp, path)
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

/// Pairs the session's user turns with what was sent, by text, in order:
/// each sent item goes to the first user turn after the previous match whose
/// text is the same (trimmed). Turns sent before this store existed, or from
/// another app, pair with nothing.
pub fn pair(turns: &[Rc<Turn>], items: &[SentItem]) -> HashMap<String, SentItem> {
    let mut out = HashMap::new();
    let mut next = 0usize;
    for turn in turns {
        let Turn::User { id, text, .. } = turn.as_ref() else { continue };
        let Some(offset) = items[next..].iter().position(|item| item.text.trim() == text.trim()) else { continue };
        let item = &items[next + offset];
        next += offset + 1;
        if !item.attachments.is_empty() || item.annotations.is_some() {
            out.insert(id.clone(), item.clone());
        }
    }
    out
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
        }
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

    /// A provider draft's sends while opening move to the id its child
    /// minted, beside what that id already held — never dropped — and the
    /// provisional index goes.
    #[test]
    fn rekey_moves_what_was_sent_while_opening() {
        let _guard = crate::store::test_env_lock();
        let (old, new) = ("sent-rekey-provisional", "sent-rekey-landed");
        append(new, sent("earlier", "e.txt")).expect("seed");
        append(old, sent("while opening", "w.txt")).expect("seed");
        let merged = rekey(old, new, load(old));
        let texts: Vec<_> = merged.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["earlier", "while opening"]);
        assert_eq!(load(new), merged, "on disk too");
        assert!(load(old).is_empty(), "the provisional index is gone");
        let _ = std::fs::remove_file(index_path(new));
    }

    #[test]
    fn a_session_id_becomes_a_safe_file_name() {
        assert_eq!(file_stem("ses/../x:1"), "ses____x_1");
    }
}
