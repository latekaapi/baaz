//! "Show full output" for truncated tool and user-shell cards.
//!
//! A provider can truncate a tool's visible output while it keeps the full
//! bytes behind a stored-output reference (the session's facts list which).
//! The card's fold row asks for them with one `ReadStoredOutput`; the
//! provider pages, decodes and caps them, and the card renders what came
//! back. Nothing here is optimistic: the body is replaced on the provider's
//! answer only (D4), and a replayed capture refuses the read like every
//! other command.

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

/// Split fetched bytes into lines, the way the fold splits visible output: no
/// trailing empty line for output that ends in a newline, and nothing at all
/// for empty output.
pub fn split_output_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    text.trim_end_matches('\n').split('\n').map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_output_is_no_lines() {
        assert!(split_output_lines("").is_empty());
    }

}
