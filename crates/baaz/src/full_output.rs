//! "Show full output" for truncated tool and user-shell cards.
//!
//! A view can truncate a tool's visible output while the server keeps the full
//! bytes under an `outputRef` (muse 1.1.1 `item/readOutput`,
//! `docs/10-msp-1.1.1-diff.md`). The card's fold row fetches those bytes — one
//! page at a time on a background task — and the card renders what came back.
//! Nothing here is optimistic: the body is replaced on the server's result
//! only (D4), and a replayed capture refuses the fetch like every other
//! command, through `wire_client`.

use base64::Engine as _;
use muse_client::schema::{ItemReadOutputEncoding, ItemReadOutputParams};
use muse_client::{MuseClient, MuseError};

/// How many stored bytes one `item/readOutput` page asks for. Well under the
/// wire's 6 MiB default/max, so every page is served whole.
const FULL_OUTPUT_PAGE: u64 = 512 * 1024;

/// How many stored bytes a full output may hold. Past this the card says so
/// rather than growing without bound.
pub const FULL_OUTPUT_CAP: u64 = 2 * 1024 * 1024;

/// The line a capped full output ends with. It names the cap the task states.
pub const CAPPED_MARKER: &str = "\u{2026}truncated at 2 MiB";

/// What a "Show full output" fetch is doing, per tool block id.
pub enum Fetch {
    /// Pages are still arriving on the background task.
    Fetching,
    /// The server's bytes, as lines, with whether the cap cut them short.
    Ready {
        lines: Vec<String>,
        capped: bool,
    },
}

/// The fetched bytes of one stored output, with whether the cap cut them.
pub struct Fetched {
    pub lines: Vec<String>,
    pub capped: bool,
}

/// Page `item/readOutput` from `offset` until `eof` or the cap, and return the
/// stored bytes as lines. `utf8` pages concatenate as text; `base64` pages
/// (binary media) decode first, so the card never shows encoded bytes.
pub fn fetch_full_output(
    client: &MuseClient,
    session_id: &str,
    item_id: &str,
    output_ref: &str,
) -> Result<Fetched, MuseError> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut offset: u64 = 0;
    loop {
        let params = ItemReadOutputParams {
            item_id: item_id.to_owned(),
            length_bytes: Some(FULL_OUTPUT_PAGE),
            offset_bytes: Some(offset),
            output_ref: output_ref.to_owned(),
            session_id: session_id.to_owned(),
        };
        let page = client.item_read_output(&params)?;
        push_page(&mut bytes, &page.content, &page.encoding)?;
        if bytes.len() as u64 >= FULL_OUTPUT_CAP {
            bytes.truncate(FULL_OUTPUT_CAP as usize);
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return Ok(Fetched { lines: split_output_lines(&text), capped: true });
        }
        if page.eof {
            break;
        }
        // The next page starts where this one stopped serving; a page that
        // served nothing advances nothing, so stopping is the only honest
        // move — looping would ask for the same range forever.
        let next = page.offset_bytes.saturating_add(page.byte_len);
        if page.byte_len == 0 || next <= offset {
            break;
        }
        offset = next;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Fetched { lines: split_output_lines(&text), capped: false })
}

/// Append one page's content to the accumulated stored bytes.
fn push_page(
    bytes: &mut Vec<u8>,
    content: &str,
    encoding: &ItemReadOutputEncoding,
) -> Result<(), MuseError> {
    match encoding {
        ItemReadOutputEncoding::Utf8 => bytes.extend_from_slice(content.as_bytes()),
        ItemReadOutputEncoding::Base64 => {
            let decoded =
                base64::engine::general_purpose::STANDARD.decode(content.as_bytes()).map_err(
                    |error| MuseError::Protocol(format!("item/readOutput served undecodable base64: {error}")),
                )?;
            bytes.extend_from_slice(&decoded);
        }
    }
    Ok(())
}

/// Split fetched bytes into lines, the way the fold splits visible output: no
/// trailing empty line for output that ends in a newline, and nothing at all
/// for empty output.
fn split_output_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.trim_end_matches('\n').split('\n').map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_pages_concatenate() {
        let mut bytes = Vec::new();
        push_page(&mut bytes, "hel", &ItemReadOutputEncoding::Utf8).unwrap();
        push_page(&mut bytes, "lo\n", &ItemReadOutputEncoding::Utf8).unwrap();
        assert_eq!(split_output_lines(&String::from_utf8_lossy(&bytes)), vec!["hello"]);
    }

    #[test]
    fn base64_pages_decode() {
        let mut bytes = Vec::new();
        push_page(&mut bytes, "aGVsbG8K", &ItemReadOutputEncoding::Base64).unwrap();
        assert_eq!(split_output_lines(&String::from_utf8_lossy(&bytes)), vec!["hello"]);
    }

    #[test]
    fn bad_base64_is_a_protocol_error() {
        let mut bytes = Vec::new();
        let error = push_page(&mut bytes, "!!!", &ItemReadOutputEncoding::Base64).unwrap_err();
        assert!(matches!(error, MuseError::Protocol(_)));
    }

    #[test]
    fn empty_output_is_no_lines() {
        assert!(split_output_lines("").is_empty());
    }

    #[test]
    fn cap_truncates_and_reports() {
        let mut bytes = vec![b'x'; FULL_OUTPUT_CAP as usize + 3];
        let capped = bytes.len() as u64 >= FULL_OUTPUT_CAP;
        bytes.truncate(FULL_OUTPUT_CAP as usize);
        assert!(capped);
        assert_eq!(bytes.len() as u64, FULL_OUTPUT_CAP);
    }
}
