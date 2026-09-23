//! The declared set, exactly as doc §6 declares it.
//!
//! Anything without evidence is `Unverified` — honest ignorance, still
//! attempted — except `Questions`, which is `Unavailable` with its reason:
//! Claude Code asks in prose and there is no question id to answer.
//!
//! Two cells carry doc §8's caveat in their comments: `ForkSession` and
//! `SubagentTurns` were read off `--help` and frame fields, never executed
//! live. They stay `Native` per the §6 table (the brief requires the table
//! exactly); the report says plainly they never ran.

use provider::{version_at_least, Capability, CapabilitySet, CapabilityState};

/// The oldest CLI this seam supports: 2.1.0. `--include-partial-messages`,
/// `--strict-mcp-config`, `--permission-prompts` and `--session-id` are all
/// present at the probed 2.1.276 and none was probed below it.
///
/// Compared with [`version_at_least`](provider::version_at_least), never an
/// exact pin: pinning a CLI version by string equality once made half the
/// features fail closed after a routine upgrade. `claude --version` prints
/// `2.1.276 (Claude Code)`; the shared parser already tolerates that
/// whitespace trailer.
pub const CLAUDE_VERSION_FLOOR: &str = "2.1.0";

/// Whether `version` meets the [`CLAUDE_VERSION_FLOOR`] floor. Newer is
/// supported; older is not; unparseable fails closed.
pub fn claude_version_supported(version: &str) -> bool {
    version_at_least(version, CLAUDE_VERSION_FLOOR)
}

/// The capability set of doc §6, cell for cell.
pub fn capabilities() -> CapabilitySet {
    CapabilitySet::new([
        (Capability::SessionLifecycle, CapabilityState::Native),
        // Native per §6 (--fork-session on --help); never executed — see §8.
        (Capability::ForkSession, CapabilityState::Native),
        (
            Capability::CompactSession,
            CapabilityState::Emulated {
                reason: "--autocompact <auto|tokens> sets a window; there is no 'compact now' \
                         command over --print. Compaction happens when the window fills, not \
                         when asked"
                    .into(),
            },
        ),
        (Capability::SessionConfig, CapabilityState::Native),
        (Capability::SessionShell, CapabilityState::Native),
        (Capability::SubmitTurn, CapabilityState::Native),
        (Capability::SteerTurn, CapabilityState::Unverified),
        (Capability::TurnControl, CapabilityState::Unverified),
        (
            Capability::ModelCatalog,
            CapabilityState::Emulated {
                reason: "--model takes aliases and ids, but no fixture enumerates them; Baaz \
                         supplies the list"
                    .into(),
            },
        ),
        (Capability::Approvals, CapabilityState::Native),
        (
            Capability::Questions,
            CapabilityState::Unavailable {
                reason: "Claude Code asks in prose; there is no question id to answer".into(),
            },
        ),
        (Capability::Transcript, CapabilityState::Native),
        (Capability::Account, CapabilityState::Native),
        (Capability::ClientTools, CapabilityState::Native),
        (Capability::ReasoningTraces, CapabilityState::Native),
        // Native per §6 (parent_tool_use_id on every streamed frame);
        // never executed — see §8.
        (Capability::SubagentTurns, CapabilityState::Native),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_doc_section_6() {
        let set = capabilities();
        use Capability as C;
        use CapabilityState as S;
        assert_eq!(set.state(C::SessionLifecycle), &S::Native);
        assert_eq!(set.state(C::ForkSession), &S::Native);
        assert!(matches!(set.state(C::CompactSession), S::Emulated { .. }));
        assert_eq!(set.state(C::SessionConfig), &S::Native);
        assert_eq!(set.state(C::SessionShell), &S::Native);
        assert_eq!(set.state(C::SubmitTurn), &S::Native);
        // NOT Native: nothing probed them. Do not upgrade without a fixture.
        assert_eq!(set.state(C::SteerTurn), &S::Unverified);
        assert_eq!(set.state(C::TurnControl), &S::Unverified);
        assert!(matches!(set.state(C::ModelCatalog), S::Emulated { .. }));
        assert_eq!(set.state(C::Approvals), &S::Native);
        assert!(matches!(
            set.state(C::Questions),
            S::Unavailable { reason }
            if reason == "Claude Code asks in prose; there is no question id to answer"
        ));
        assert_eq!(set.state(C::Transcript), &S::Native);
        assert_eq!(set.state(C::Account), &S::Native);
        assert_eq!(set.state(C::ClientTools), &S::Native);
        assert_eq!(set.state(C::ReasoningTraces), &S::Native);
        assert_eq!(set.state(C::SubagentTurns), &S::Native);
    }

    #[test]
    fn below_the_floor_is_refused_and_above_passes() {
        assert!(!claude_version_supported("2.0.9"));
        assert!(!claude_version_supported("1.9.99"));
        assert!(claude_version_supported("2.1.0"));
        assert!(claude_version_supported("2.1.276 (Claude Code)"));
        assert!(claude_version_supported("2.2.0"));
        assert!(!claude_version_supported("not-a-version"), "unparseable fails closed");
    }
}
