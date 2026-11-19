//! The card a failed turn leaves in its transcript (docs/82 5.5, docs/81 F
//! D5): one shape for every fold, so a Claude Code or Codex turn that failed
//! — a rate limit included — reads the way a Muse one always has: an error
//! card with Retry, not only a "Failed" row in the sidebar.

use aui_protocol::Block;

/// Why a turn failed, as far as the card's title is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureKind {
    /// The account's usage or rate limit stopped it: Retry after a while.
    RateLimited,
    /// The service was overloaded or unreachable: Retry may work at once.
    Unavailable,
    /// The conversation no longer fits the model's context window.
    ContextFull,
    /// Anything else the provider reported.
    Other,
}

impl FailureKind {
    /// The card's title.
    pub fn title(self) -> &'static str {
        match self {
            FailureKind::RateLimited => "Rate limit reached",
            FailureKind::Unavailable => "Service unavailable",
            FailureKind::ContextFull => "Context window full",
            FailureKind::Other => "The turn failed",
        }
    }

    /// The kind a provider's own failure words name, for a provider that
    /// reports failures as text only (Claude Code's `result`): the HTTP
    /// status or the words the vendor's errors carry. Anything unnamed is
    /// [`FailureKind::Other`] — never a guess at a rate limit.
    pub fn from_words(message: &str) -> FailureKind {
        let lower = message.to_lowercase();
        if lower.contains("rate limit")
            || lower.contains("rate_limit")
            || lower.contains("usage limit")
            || lower.contains(" 429")
            || lower.starts_with("429")
        {
            FailureKind::RateLimited
        } else if lower.contains("overloaded") || lower.contains(" 529") || lower.contains(" 503") {
            FailureKind::Unavailable
        } else if lower.contains("prompt is too long") || lower.contains("context window") {
            FailureKind::ContextFull
        } else {
            FailureKind::Other
        }
    }
}

/// The error card for a failed turn: the kind's title, the provider's own
/// words as the detail, and Retry offered — the app shows the button only
/// on a turn it can resend.
pub fn card(kind: FailureKind, message: &str) -> Block {
    let detail = message.trim();
    Block::Error {
        title: kind.title().to_owned(),
        detail: if detail.is_empty() { "No detail was given.".to_owned() } else { detail.to_owned() },
        retryable: kind != FailureKind::ContextFull,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_limit_reads_as_one_whoever_reports_it() {
        for words in [
            "API Error: 429 {\"type\":\"error\",\"error\":{\"type\":\"rate_limit_error\"}}",
            "Claude AI usage limit reached|1790000000",
            "You've hit your usage limit.",
            "429 Too Many Requests",
        ] {
            assert_eq!(FailureKind::from_words(words), FailureKind::RateLimited, "{words}");
        }
        assert_eq!(FailureKind::from_words("API Error: 529 overloaded_error"), FailureKind::Unavailable);
        assert_eq!(FailureKind::from_words("Prompt is too long"), FailureKind::ContextFull);
        assert_eq!(FailureKind::from_words("the turn failed"), FailureKind::Other);
    }

    #[test]
    fn the_card_offers_retry_unless_retrying_cannot_help() {
        let Block::Error { title, detail, retryable } = card(FailureKind::RateLimited, " slow down ") else {
            panic!("an error card");
        };
        assert_eq!((title.as_str(), detail.as_str(), retryable), ("Rate limit reached", "slow down", true));
        assert!(matches!(card(FailureKind::ContextFull, ""), Block::Error { retryable: false, .. }));
    }
}
