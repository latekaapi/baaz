//! The declared set, grounded in `fixtures/codex/`.
//!
//! Anything with a fixture proving the round trip is `Native`. Anything read
//! off a method name but never executed (`thread/fork`, `thread/compact/start`,
//! `item/tool/requestUserInput`, out-of-turn shell, client-tool registration)
//! is `Unverified` — honest ignorance, still attempted — never quietly
//! upgraded without a fixture. Nothing here is `Unavailable`: no negative has
//! been established on this backend.
//!
//! `app-server` is flagged `[experimental]` in `codex --help`, so the floor
//! below is a version we pin, and the protocol may move under us.

use provider::{version_at_least, Capability, CapabilitySet, CapabilityState};

/// The oldest CLI this seam supports: 0.144.6, the version every frame in
/// `fixtures/codex/` was captured from. Compared with
/// [`version_at_least`](provider::version_at_least), never an exact pin.
///
/// `codex --version` prints `codex-cli 0.144.6`. The shared parser fails
/// closed on that prefix (it would read the major as `codex-cli 0` and refuse
/// a version that is in fact the floor itself), so [`codex_version_supported`]
/// strips the prefix before comparing — the stage-3 `2.1.276 (Claude Code)`
/// bug in a new costume.
pub const CODEX_VERSION_FLOOR: &str = "0.144.6";

/// Whether `version` meets the [`CODEX_VERSION_FLOOR`] floor. Newer is
/// supported; older is not; unparseable fails closed. The `codex-cli ` prefix
/// of `codex --version` is stripped first; without that strip the floor
/// version itself would fail closed.
pub fn codex_version_supported(version: &str) -> bool {
    let version = version.strip_prefix("codex-cli").map(str::trim_start).unwrap_or(version);
    version_at_least(version, CODEX_VERSION_FLOOR)
}

/// The capability set, cell for cell against the fixture evidence.
pub fn capabilities() -> CapabilitySet {
    CapabilitySet::new([
        // thread/start mints the thread; the session mapping stores it.
        (Capability::SessionLifecycle, CapabilityState::Native),
        // `thread/fork` exists; no fixture forks. Do not upgrade blind.
        (Capability::ForkSession, CapabilityState::Unverified),
        // `thread/compact/start` exists; never executed. Do not upgrade blind.
        (Capability::CompactSession, CapabilityState::Unverified),
        // thread/start and turn/start both carry an explicit model.
        (Capability::SessionConfig, CapabilityState::Native),
        // No out-of-turn shell surface was captured; the shell runs inside
        // turns. Attempted, never refused by the gate.
        (Capability::SessionShell, CapabilityState::Unverified),
        // turn/start, proven in every fixture.
        (Capability::SubmitTurn, CapabilityState::Native),
        // turn/steer answers `{"turnId":…}`, proven mid-turn.
        (Capability::SteerTurn, CapabilityState::Native),
        // turn/interrupt ends `interrupted`, proven after a steer.
        (Capability::TurnControl, CapabilityState::Native),
        // model/list answers four rows under the `data` key.
        (Capability::ModelCatalog, CapabilityState::Native),
        // item/commandExecution/requestApproval answered `accept`, the
        // command then ran. The decision travels as a per-kind typed enum,
        // never a string literal: `approved` fails closed and looks like a
        // denial. The fileChange and permissions params shapes come from
        // their schema files, not from a capture — no fixture ever
        // recorded those two requests, and no permissions answer was ever
        // executed live.
        (Capability::Approvals, CapabilityState::Native),
        // item/tool/requestUserInput exists; no probe ever settled one.
        (Capability::Questions, CapabilityState::Unverified),
        // Transcript paging rides thread/read, whose shape was never
        // captured; following the live stream works. Attempted, page refused.
        (Capability::Transcript, CapabilityState::Unverified),
        // account/read plus rateLimits, pushed unprompted after every turn.
        (Capability::Account, CapabilityState::Native),
        // `item/tool/call` exists; the registration path is unidentified and
        // no probe ever made Codex call a Baaz-provided tool.
        (Capability::ClientTools, CapabilityState::Unverified),
        // `reasoning` items fold into thinking blocks.
        (Capability::ReasoningTraces, CapabilityState::Native),
        // No sub-agent turn shape was captured.
        (Capability::SubagentTurns, CapabilityState::Unverified),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_the_fixture_evidence() {
        let set = capabilities();
        use Capability as C;
        use CapabilityState as S;
        assert_eq!(set.state(C::SessionLifecycle), &S::Native);
        assert_eq!(set.state(C::ForkSession), &S::Unverified);
        assert_eq!(set.state(C::CompactSession), &S::Unverified);
        assert_eq!(set.state(C::SessionConfig), &S::Native);
        assert_eq!(set.state(C::SessionShell), &S::Unverified);
        assert_eq!(set.state(C::SubmitTurn), &S::Native);
        assert_eq!(set.state(C::SteerTurn), &S::Native);
        assert_eq!(set.state(C::TurnControl), &S::Native);
        assert_eq!(set.state(C::ModelCatalog), &S::Native);
        assert_eq!(set.state(C::Approvals), &S::Native);
        // NOT Native: the shape was read, never executed. Do not upgrade
        // without a fixture.
        assert_eq!(set.state(C::Questions), &S::Unverified);
        assert_eq!(set.state(C::Transcript), &S::Unverified);
        assert_eq!(set.state(C::Account), &S::Native);
        assert_eq!(set.state(C::ClientTools), &S::Unverified);
        assert_eq!(set.state(C::ReasoningTraces), &S::Native);
        assert_eq!(set.state(C::SubagentTurns), &S::Unverified);
    }

    #[test]
    fn the_floor_parses_with_its_prefix_and_garbage_fails_closed() {
        // The bug this exists for: `codex-cli 0.144.6` must parse, because it
        // IS the floor. Without the strip the shared parser reads the major
        // as `codex-cli 0` and fails closed on the version we pin.
        assert!(codex_version_supported("codex-cli 0.144.6"));
        assert!(codex_version_supported("codex-cli 0.145.0"));
        assert!(codex_version_supported("0.144.6"), "bare triple still parses");
        assert!(codex_version_supported("0.200.0"));
        assert!(!codex_version_supported("codex-cli 0.143.9"), "older is refused");
        assert!(!codex_version_supported("codex-cli 0.100.0"), "older is refused");
        assert!(!codex_version_supported("not-a-version"), "unparseable fails closed");
        assert!(!codex_version_supported(""), "empty fails closed");
        assert!(
            !codex_version_supported("codex-cli later"),
            "a prefix with no triple fails closed"
        );
    }
}
