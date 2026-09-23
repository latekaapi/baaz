//! What "no" looks like. A provider that cannot do something says so with a
//! typed reason — it must never return `Ok` and quietly do nothing.

use std::fmt;

/// Why a [`crate::ProviderAdapter`] command failed.
///
/// The `Unsupported` variant is the whole point of this type: "not supported"
/// cannot be spelled as success, because `send` returns
/// `Result<Ack, ProviderError>` and success is the `Ok` side.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderError {
    /// This provider cannot do that. Names the capability in the same
    /// kebab-case words [`crate::Command::capability`] uses (e.g.
    /// `"fork-session"`) and gives the human reason.
    Unsupported {
        /// The capability the command needed.
        capability: String,
        /// Why this provider cannot do it.
        reason: String,
    },
    /// The provider was reachable but refused the command: bad input, a lost
    /// race (stale requirement token, retracted turn), a policy denial.
    /// Carries the human reason; the per-provider detail stays behind the
    /// seam.
    Rejected {
        /// Why the command was refused.
        reason: String,
    },
    /// No provider on the other end: the child is gone, the socket is down,
    /// the request timed out. Retry may help; a different command will not.
    Unavailable {
        /// What went wrong.
        reason: String,
    },
}

impl ProviderError {
    /// The typed refusal: this provider cannot `capability`, and here is why.
    pub fn unsupported(capability: &str, reason: impl Into<String>) -> Self {
        Self::Unsupported { capability: capability.to_owned(), reason: reason.into() }
    }

    /// The capability name when this is a refusal, else `None`.
    pub fn capability(&self) -> Option<&str> {
        match self {
            Self::Unsupported { capability, .. } => Some(capability),
            _ => None,
        }
    }

    /// Whether this is the typed refusal rather than a real failure.
    pub fn is_unsupported(&self) -> bool {
        matches!(self, Self::Unsupported { .. })
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { capability, reason } => {
                write!(f, "unsupported capability `{capability}`: {reason}")
            }
            Self::Rejected { reason } => write!(f, "provider refused the command: {reason}"),
            Self::Unavailable { reason } => write!(f, "provider unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ProviderError {}
