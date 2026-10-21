//! A tool call's full stored output, paged in through `item/readOutput`.
//!
//! The transcript keeps a truncated body; the full bytes stay on the host
//! behind the item's output reference. They are read in pages of
//! [`FULL_OUTPUT_PAGE`], decoded (UTF-8 or base64), and capped at
//! [`FULL_OUTPUT_CAP`] so a runaway log never floods the view.

use base64::Engine as _;
use muse_client::schema::{ItemReadOutputEncoding, ItemReadOutputParams};
use muse_client::{MuseClient, MuseError};
use provider::ProviderError;

/// One `item/readOutput` page.
pub const FULL_OUTPUT_PAGE: u64 = 512 * 1024;

/// The most a full-output read keeps.
pub const FULL_OUTPUT_CAP: u64 = 2 * 1024 * 1024;

/// Read the whole stored output, page by page: the text and whether it is
/// complete (`false` when [`FULL_OUTPUT_CAP`] cut it). Blocking.
pub fn read_whole(
    client: &MuseClient,
    session_id: &str,
    item_id: &str,
    output_ref: &str,
) -> Result<(String, bool), ProviderError> {
    read(client, session_id, item_id, output_ref).map_err(crate::translate::transport_error)
}

fn read(client: &MuseClient, session_id: &str, item_id: &str, output_ref: &str) -> Result<(String, bool), MuseError> {
    let mut bytes: Vec<u8> = Vec::new();
    let mut offset: u64 = 0;
    loop {
        let page = client.item_read_output(&ItemReadOutputParams {
            item_id: item_id.to_owned(),
            length_bytes: Some(FULL_OUTPUT_PAGE),
            offset_bytes: Some(offset),
            output_ref: output_ref.to_owned(),
            session_id: session_id.to_owned(),
        })?;
        push_page(&mut bytes, &page.content, &page.encoding)?;
        if bytes.len() as u64 >= FULL_OUTPUT_CAP {
            bytes.truncate(FULL_OUTPUT_CAP as usize);
            return Ok((String::from_utf8_lossy(&bytes).into_owned(), false));
        }
        if page.eof {
            break;
        }
        // The next page starts where this one ended; a page that does not
        // move forward ends the read rather than looping on it.
        let next = page.offset_bytes.saturating_add(page.byte_len);
        if page.byte_len == 0 || next <= offset {
            break;
        }
        offset = next;
    }
    Ok((String::from_utf8_lossy(&bytes).into_owned(), true))
}

/// Append one page's content, decoded.
pub fn push_page(bytes: &mut Vec<u8>, content: &str, encoding: &ItemReadOutputEncoding) -> Result<(), MuseError> {
    match encoding {
        ItemReadOutputEncoding::Utf8 => bytes.extend_from_slice(content.as_bytes()),
        ItemReadOutputEncoding::Base64 => {
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(content.as_bytes())
                .map_err(|error| MuseError::Protocol(format!("item/readOutput served undecodable base64: {error}")))?;
            bytes.extend_from_slice(&decoded);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_decode_by_their_encoding() {
        let mut bytes = Vec::new();
        push_page(&mut bytes, "he", &ItemReadOutputEncoding::Utf8).unwrap();
        push_page(&mut bytes, "bGxv", &ItemReadOutputEncoding::Base64).unwrap();
        assert_eq!(bytes, b"hello");
        assert!(push_page(&mut bytes, "%%%", &ItemReadOutputEncoding::Base64).is_err());
    }
}
