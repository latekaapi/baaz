//! The dock's block-intent logic: what every [`aui_terminal::TerminalGridIntent`]
//! the grid raises does (D44).
//!
//! The library performs none of these actions — it only reports the intent
//! with a block index into [`aui_terminal::TerminalSession::blocks`]. The
//! host reads the block back with
//! [`aui_terminal::TerminalSession::block_text`] and decides what to do.
//! This module holds the pure pieces (URL gating, the Ask draft builder);
//! [`crate::app::Harness`] owns the live wiring.

/// How much of a block's output an Ask draft may carry: head+tail (D47).
pub const ASK_CAP_BYTES: usize = 4096;

/// Whether `cx.open_url` may open this link.
///
/// Only `http`/`https` pass. This is a security boundary: a program can
/// print any OSC 8 link it likes, so `file:`, `javascript:` and
/// scheme-less strings are all refused.
pub fn openable_url(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once("://") else { return false };
    scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
}

/// Marker between a capped output's head and tail.
const TRUNCATION_MARKER: &str = "\n… [output truncated] …\n";

/// Head+tail of `text`, capped at [`ASK_CAP_BYTES`] bytes (D47).
///
/// Short text passes through untouched. Longer text keeps its first and
/// last halves around [`TRUNCATION_MARKER`], cut back to character
/// boundaries so the cap never splits a UTF-8 sequence.
pub fn cap_output(text: &str) -> String {
    if text.len() <= ASK_CAP_BYTES {
        return text.to_owned();
    }
    let half = (ASK_CAP_BYTES - TRUNCATION_MARKER.len()) / 2;
    let head_end = floor_char_boundary(text, half);
    let tail_start = ceil_char_boundary(text, text.len().saturating_sub(ASK_CAP_BYTES - TRUNCATION_MARKER.len() - head_end));
    format!("{}{TRUNCATION_MARKER}{}", &text[..head_end], &text[tail_start..])
}

/// The largest `end <= max` that is a character boundary of `text`.
fn floor_char_boundary(text: &str, max: usize) -> usize {
    let mut end = max.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The smallest `start >= min` that is a character boundary of `text`.
fn ceil_char_boundary(text: &str, min: usize) -> usize {
    let mut start = min.min(text.len());
    while start < text.len() && !text.is_char_boundary(start) {
        start += 1;
    }
    start
}

/// Quote `text` as a `>` block, one marker per line.
pub fn quote_block(text: &str) -> String {
    text.lines().map(|line| format!("> {line}")).collect::<Vec<_>>().join("\n")
}

/// The Ask draft for a block: what lands in the composer's draft, unsent.
///
/// `"About this terminal output:"`, then the block's command as a `$`
/// line, then its ANSI-free output (capped by [`cap_output`]) as a quoted
/// block. A non-empty composer is appended to rather than clobbered; an
/// empty one is replaced. This builder only builds text — the caller puts
/// it through `SessionView::set_draft` and never sends it.
pub fn build_ask_draft(command: &str, output: &str, existing: &str) -> String {
    let mut draft = format!("About this terminal output:\n$ {}\n{}", command.trim(), quote_block(&cap_output(output)));
    if !existing.trim().is_empty() {
        draft = format!("{}\n\n{draft}", existing.trim_end());
    }
    draft
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openable_url_accepts_http_and_https_only() {
        assert!(openable_url("http://example.com/x"));
        assert!(openable_url("https://example.com/x"));
        assert!(openable_url("HTTPS://example.com/x"));
        assert!(!openable_url("file:///etc/passwd"));
        assert!(!openable_url("javascript:alert(1)"));
        assert!(!openable_url("example.com/no-scheme"));
        assert!(!openable_url(""));
    }

    #[test]
    fn capped_output_keeps_head_and_tail_within_4kb() {
        let head = "H".repeat(3000);
        let tail = "T".repeat(3000);
        let capped = cap_output(&format!("{head}\n{tail}"));
        assert!(capped.len() <= ASK_CAP_BYTES, "capped to 4 KB, got {}", capped.len());
        assert!(capped.starts_with(&"H".repeat(100)), "head survives");
        assert!(capped.ends_with(&"T".repeat(100)), "tail survives");
        assert!(capped.contains("truncated"), "the join says so");
    }

    #[test]
    fn short_output_passes_through_untouched() {
        assert_eq!(cap_output("ok\n"), "ok\n");
        assert_eq!(cap_output(&"x".repeat(ASK_CAP_BYTES)), "x".repeat(ASK_CAP_BYTES));
    }

    #[test]
    fn the_cap_never_splits_a_character() {
        let text = "é".repeat(3000);
        let capped = cap_output(&text);
        assert!(capped.len() <= ASK_CAP_BYTES + "é".len());
        assert!(capped.is_char_boundary(0));
    }

    #[test]
    fn the_ask_draft_quotes_command_and_output() {
        let draft = build_ask_draft("git status -sb", "## main\n", "");
        assert!(draft.starts_with("About this terminal output:"), "the header leads");
        assert!(draft.contains("$ git status -sb"), "the command rides along");
        assert!(draft.contains("> ## main"), "the output is quoted");
    }

    #[test]
    fn the_ask_draft_appends_rather_than_clobbering() {
        let draft = build_ask_draft("ls", "a\n", "why is this failing?");
        assert!(draft.starts_with("why is this failing?"), "existing text survives first");
        assert!(draft.contains("About this terminal output:"), "the block follows");
    }

    #[test]
    fn the_ask_draft_caps_a_firehose() {
        let draft = build_ask_draft("cat big.log", &"y".repeat(9000), "");
        assert!(draft.len() <= "About this terminal output:\n$ cat big.log\n".len() + ASK_CAP_BYTES + 64);
        assert!(draft.contains("truncated"), "the cap says so");
    }
}
