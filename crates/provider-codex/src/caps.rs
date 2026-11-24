//! The declared set, grounded in `fixtures/codex/`.
//!
//! Anything with a fixture proving the round trip is `Native`. What the
//! adapter refuses in `dispatch` (fork, compact, out-of-turn shell, question
//! answers, images) is `Unavailable` with the adapter's reason, so the app
//! never offers the control (docs/81 F D1). What has no command and no
//! evidence (client-tool registration, sub-agent turns) stays `Unverified`,
//! which the gate also refuses; promote a cell only with a fixture.
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
        // thread/resume rejoins it with its turns[] history, executed
        // live 2026-09-26 (`fixtures/codex/resume.jsonl`).
        (Capability::SessionLifecycle, CapabilityState::Native),
        // `thread/fork` exists; no fixture forks, and the adapter refuses it.
        (
            Capability::ForkSession,
            CapabilityState::Unavailable {
                reason: "thread/fork was never executed against a live server; not forked blind"
                    .into(),
            },
        ),
        // Native: `thread/compact/start` executed live (24 + 25 Nov 2026,
        // docs/85-evidence/10.4): a turn of its own with one
        // `contextCompaction` item.
        (Capability::CompactSession, CapabilityState::Native),
        // thread/start and turn/start both carry an explicit model.
        (Capability::SessionConfig, CapabilityState::Native),
        // No out-of-turn shell surface was captured; the shell runs inside
        // turns.
        (
            Capability::SessionShell,
            CapabilityState::Unavailable {
                reason: "no out-of-turn shell surface was captured; the shell runs inside turns"
                    .into(),
            },
        ),
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
        (
            Capability::Questions,
            CapabilityState::Unavailable {
                reason: "question answer shapes were never captured; not answered blind"
                    .into(),
            },
        ),
        // Following the live stream works ("Retry now" revives the child
        // through FollowSession); paging rides thread/read, never captured,
        // and the app never pages a Codex session (`pages_history`).
        (
            Capability::Transcript,
            CapabilityState::Emulated {
                reason: "follows the live session; the history comes back whole on resume and \
                         is never paged"
                    .into(),
            },
        ),
        // account/read plus rateLimits, pushed unprompted after every turn.
        (Capability::Account, CapabilityState::Native),
        // `item/tool/call` exists; the registration path is unidentified and
        // no probe ever made Codex call a Baaz-provided tool.
        (Capability::ClientTools, CapabilityState::Unverified),
        // `reasoning` items fold into thinking blocks.
        (Capability::ReasoningTraces, CapabilityState::Native),
        // No sub-agent turn shape was captured.
        (Capability::SubagentTurns, CapabilityState::Unverified),
        // turn/start takes `localImage` paths (fixture `image.jsonl`) but the
        // seam carries bytes; the adapter refuses images until it stages them.
        (
            Capability::ImageInput,
            CapabilityState::Unavailable {
                reason: "turn/start takes a local image path but the seam carries bytes; images are not staged to a file yet"
                    .into(),
            },
        ),
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
        assert!(matches!(set.state(C::ForkSession), S::Unavailable { .. }));
        assert_eq!(set.state(C::CompactSession), &S::Native);
        assert_eq!(set.state(C::SessionConfig), &S::Native);
        assert!(matches!(set.state(C::SessionShell), S::Unavailable { .. }));
        assert_eq!(set.state(C::SubmitTurn), &S::Native);
        assert_eq!(set.state(C::SteerTurn), &S::Native);
        assert_eq!(set.state(C::TurnControl), &S::Native);
        assert_eq!(set.state(C::ModelCatalog), &S::Native);
        assert_eq!(set.state(C::Approvals), &S::Native);
        // NOT Native: the shape was read, never executed. Do not upgrade
        // without a fixture.
        assert!(matches!(set.state(C::Questions), S::Unavailable { .. }));
        assert!(matches!(set.state(C::Transcript), S::Emulated { .. }));
        assert!(matches!(set.state(C::ImageInput), S::Unavailable { .. }));
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
