//! The declared set, exactly as doc §6 declares it.
//!
//! Anything without evidence is `Unverified` — honest ignorance, and like
//! `Unavailable` it is neither attempted nor offered. `Questions` is `AskUserQuestion`: the CLI hands every call to the
//! host as a `can_use_tool` request, in every permission mode, and the answers
//! go back as the tool's input.
//!
//! Doc §8's caveat, cell by cell: `ForkSession` was read off `--help`
//! and has since been executed for real (fixture `fork.jsonl`: resuming
//! `f3815266-…` with `--fork-session` minted `c4b6fee5-…`), so it stays
//! `Native`. `SubagentTurns` was read off frame fields and never executed,
//! so it is `Unverified` — honest ignorance, not attempted.

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
        // Native: executed for real (§8 caveat retired by fork.jsonl —
        // resuming f3815266-… with --fork-session minted c4b6fee5-…).
        (Capability::ForkSession, CapabilityState::Native),
        // The adapter refuses compact-now: `--autocompact` only sets a
        // window, so offering "/compact" would always fail (docs/81 F D1).
        (
            Capability::CompactSession,
            CapabilityState::Unavailable {
                reason: "no compact-now command exists over --print; compaction happens when the \
                         --autocompact window fills, not when asked"
                    .into(),
            },
        ),
        (Capability::SessionConfig, CapabilityState::Native),
        // The adapter refuses `RunShell`: the Bash tool runs inside turns.
        (
            Capability::SessionShell,
            CapabilityState::Unavailable {
                reason: "no out-of-turn shell surface was probed over stream-json stdin; the Bash \
                         tool runs inside turns"
                    .into(),
            },
        ),
        (Capability::SubmitTurn, CapabilityState::Native),
        // A mid-turn stdin line is answered with a turn and a `result` of
        // its own (probed live 2026-10-16): the words arrive, as the next
        // turn rather than inside the running one.
        (
            Capability::SteerTurn,
            CapabilityState::Emulated {
                reason: "sent while the turn runs; Claude Code answers it as its own turn after \
                         the current one"
                    .into(),
            },
        ),
        (Capability::TurnControl, CapabilityState::Unverified),
        // Native: the adapter sends `initialize` after every spawn and
        // answers `ListModels` from the answer's `models[]` (fixture
        // `permission.jsonl`; probed live 2026-09-28). Baaz's supplied
        // alias list stays as the offline/failure fallback, never the
        // served catalog.
        (Capability::ModelCatalog, CapabilityState::Native),
        (Capability::Approvals, CapabilityState::Native),
        // `AskUserQuestion` reaches the host as a `can_use_tool` request in every
        // permission mode; its answers go back as the tool's input. Native:
        // executed live 2026-11-07 on 2.1.276 (fixture `ask-user-question.jsonl`
        // — the CLI read the pick back and the model answered with it).
        (Capability::Questions, CapabilityState::Native),
        (Capability::Transcript, CapabilityState::Native),
        (Capability::Account, CapabilityState::Native),
        (Capability::ClientTools, CapabilityState::Native),
        (Capability::ReasoningTraces, CapabilityState::Native),
        // Unverified: parent_tool_use_id on every streamed frame was read
        // off --help and frame fields, never executed — see §8.
        (Capability::SubagentTurns, CapabilityState::Unverified),
        (Capability::ImageInput, CapabilityState::Native),
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
        assert!(matches!(set.state(C::CompactSession), S::Unavailable { .. }));
        assert_eq!(set.state(C::SessionConfig), &S::Native);
        assert!(matches!(set.state(C::SessionShell), S::Unavailable { .. }));
        assert_eq!(set.state(C::SubmitTurn), &S::Native);
        // NOT Native: nothing probed them. Do not upgrade without a fixture.
        assert!(matches!(set.state(C::SteerTurn), S::Emulated { .. }));
        assert_eq!(set.state(C::TurnControl), &S::Unverified);
        assert_eq!(set.state(C::ModelCatalog), &S::Native);
        assert_eq!(set.state(C::Approvals), &S::Native);
        // NOT Native until a fixture records the CLI taking the answers.
        assert_eq!(set.state(C::Questions), &S::Native);
        assert_eq!(set.state(C::Transcript), &S::Native);
        assert_eq!(set.state(C::Account), &S::Native);
        assert_eq!(set.state(C::ClientTools), &S::Native);
        assert_eq!(set.state(C::ReasoningTraces), &S::Native);
        // NOT Native: read off frame fields, never executed. Do not
        // upgrade without a fixture.
        assert_eq!(set.state(C::SubagentTurns), &S::Unverified);
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
