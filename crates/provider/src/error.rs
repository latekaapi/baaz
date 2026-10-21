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
    /// The provider refused, and said why in a way the app can act on: a
    /// typed hint (retry later, the session is held elsewhere, the choice
    /// went stale…), a title and how loudly to show it. Never parsed from
    /// text by the caller — the adapter decides the hint.
    Refused(Refusal),
    /// No provider on the other end: the child is gone, the socket is down,
    /// the request timed out. Retry may help; a different command will not.
    Unavailable {
        /// What went wrong.
        reason: String,
    },
}

/// What the app can do about a refusal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefusalHint {
    /// Nothing specific: show it.
    None,
    /// The provider said "not now" (overloaded, backpressured): one retry
    /// after a pause is reasonable.
    RetryLater,
    /// Another client holds the session: this view stays read-only until
    /// that one lets go.
    HeldElsewhere,
    /// The session is gone or was never there.
    SessionGone,
    /// The thing acted on (an approval, a question) is gone: the provider
    /// no longer knows it, and will never settle it.
    ItemGone,
    /// The stored history was stale and is being regenerated: the same
    /// request usually succeeds once more.
    StaleHistory,
    /// The credential is gone: offer sign-in.
    SignedOut,
    /// Someone else settled it first (an approval, a question); the
    /// settlement arrives on its own.
    AlreadySettled,
    /// The choices moved under the press (a staged approval advanced); the
    /// update is on its way, nothing to show.
    StaleChoice,
    /// That choice or answer is not acceptable.
    InvalidChoice,
    /// The turn is already over.
    TurnOver,
    /// The cut point is not one the session can branch at.
    BadBoundary,
}

/// How loudly a refusal is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    /// A banner on the session.
    Banner,
    /// A dialog over the window.
    Dialog,
}

/// A typed refusal ([`ProviderError::Refused`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// What the app can do about it.
    pub hint: RefusalHint,
    /// A one-line title, e.g. "Session already in use".
    pub title: String,
    /// The full human reason.
    pub reason: String,
    /// Banner or dialog.
    pub severity: Severity,
    /// It concerns this session only, never the connection: a resume
    /// refused for it leaves the rest of the app working.
    pub session_scoped: bool,
}

/// The prefix [`ProviderError::unknown_approval`] writes; the quoted id
/// follows it.
const UNKNOWN_APPROVAL: &str = "unknown approval ";

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

    /// The refusal a decision gets when the adapter holds no pending
    /// request under `approval`: it was already answered, expired, or
    /// belonged to a child that is gone. One shape for every adapter, so
    /// the host can recognise it with [`Self::is_unknown_approval`] and
    /// drop its own card instead of leaving it armed.
    pub fn unknown_approval(approval: &str, detail: &str) -> Self {
        Self::Rejected { reason: format!("{UNKNOWN_APPROVAL}{approval:?}: {detail}") }
    }

    /// The typed hint, when the provider gave one.
    pub fn hint(&self) -> RefusalHint {
        match self {
            Self::Refused(refusal) => refusal.hint,
            _ => RefusalHint::None,
        }
    }

    /// The refusal's title, or a generic one by shape.
    pub fn title(&self) -> String {
        match self {
            Self::Refused(refusal) => refusal.title.clone(),
            Self::Unavailable { .. } => "The agent disconnected".into(),
            Self::Rejected { .. } | Self::Unsupported { .. } => "Something went wrong".into(),
        }
    }

    /// How loudly to show it: a lost connection is a dialog, anything else
    /// a banner unless the refusal says otherwise.
    pub fn severity(&self) -> Severity {
        match self {
            Self::Refused(refusal) => refusal.severity,
            Self::Unavailable { .. } => Severity::Dialog,
            Self::Rejected { .. } | Self::Unsupported { .. } => Severity::Banner,
        }
    }

    /// Whether it concerns one session only (see [`Refusal::session_scoped`]).
    pub fn is_session_scoped(&self) -> bool {
        matches!(self, Self::Refused(refusal) if refusal.session_scoped)
    }

    /// Whether this is [`Self::unknown_approval`]'s refusal: the approval
    /// is no longer pending on the provider's side, so nothing will ever
    /// settle it there.
    pub fn is_unknown_approval(&self) -> bool {
        matches!(self, Self::Rejected { reason } if reason.strip_prefix(UNKNOWN_APPROVAL).is_some_and(|rest| rest.starts_with('"')))
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported { capability, reason } => {
                write!(f, "unsupported capability `{capability}`: {reason}")
            }
            Self::Rejected { reason } => write!(f, "provider refused the command: {reason}"),
            Self::Refused(refusal) => f.write_str(&refusal.reason),
            Self::Unavailable { reason } => write!(f, "provider unavailable: {reason}"),
        }
    }
}

impl std::error::Error for ProviderError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host drops its card on exactly this refusal: an unknown id is
    /// recognised, while an unknown *choice* for a known approval (which
    /// leaves the request pending) and every other refusal are not.
    #[test]
    fn only_the_unknown_approval_refusal_reads_as_gone() {
        let gone = ProviderError::unknown_approval("ap-1", "no pending request carries that id");
        assert!(gone.is_unknown_approval());
        assert_eq!(
            gone.to_string(),
            "provider refused the command: unknown approval \"ap-1\": no pending request carries that id"
        );
        let choice = ProviderError::Rejected {
            reason: "unknown approval choice \"maybe\" for ap-1: offer \"allow\" or \"deny\"".into(),
        };
        assert!(!choice.is_unknown_approval());
        assert!(!ProviderError::Unavailable { reason: "unknown approval \"ap-1\"".into() }.is_unknown_approval());
    }
}
