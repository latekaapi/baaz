//! muse's declared set: what the wire has shown, and the floors beneath it.
//!
//! Two facts established on this machine, encoded here rather than
//! rediscovered: muse 1.3.0 grants `sessionMcp`; muse 1.2.1 never granted
//! `sessionMcp`. The command-backed entries are `Native` because every
//! [`Command`](provider::Command) arm is exercised against the recording
//! puppet (method *and* params) and the `sessionMcp` grant facts above are
//! on record; reasoning traces and sub-agent turns have no live probe in
//! this task, so they stay
//! [`Unverified`](provider::CapabilityState::Unverified) — not `Native`
//! because they probably work.
//!
//! The session shell is `Unverified` too, deliberately. The `userShell`
//! grant *is* on record at 1.0.3, 1.1.1, and 1.2.1
//! (`grantedCapabilities:["userShell"]` in `fixtures/msp/transcript-echo.jsonl`,
//! `transcript-account.jsonl`, and `transcript-1.2.1-shapes.jsonl`), but
//! every recorded `session/userShell` execution on those versions ends with
//! the item in status `failed` — the sandbox was unavailable, so the command
//! was never started. A grant without a single clean run is not the hard
//! evidence a `Native` needs, and 1.3.0 and newer were never probed for
//! `userShell` at all. `Unverified` is attempted, never refused, so nothing
//! about sending changes — only the declaration stops guessing.
//!
//! Nothing here talks to a live server. Every entry is a claim until a
//! live probe checks it.

use std::cmp::Ordering;
use provider::compare_versions;

use provider::{Capability, CapabilitySet, CapabilityState};

/// The oldest muse this seam supports: 1.2.1, the version the schema was
/// generated from and the oldest fixture on record.
///
/// A provider newer than the floor is supported: every comparison here is a
/// numeric minimum, never an exact string. The exact pin is what made half
/// the features fail closed on a routine upgrade with no test catching it,
/// because a shell stub stood in for the real CLI — so the comparison
/// parses `major.minor.patch` numerically and ignores `-suffix`/`+build`
/// trailers (a real `muse --version` reports e.g. `1.3.0-R3401.1`).
pub const MUSE_VERSION_FLOOR: &str = "1.2.1";

/// The oldest muse that grants `sessionMcp`: 1.3.0 grants it, 1.2.1 never
/// did. Client-side tools need this floor; everything else needs only
/// [`MUSE_VERSION_FLOOR`].
pub const MUSE_MCP_VERSION_FLOOR: &str = "1.3.0";

/// Whether `version` meets the [`MUSE_VERSION_FLOOR`] floor. Newer than the
/// floor is supported; older is not; unparseable fails closed (below),
/// because assuming support for a version nobody can read is how the exact
/// pin failed — loudly in the wrong direction.
pub fn muse_version_supported(version: &str) -> bool {
    compare_versions(version, MUSE_VERSION_FLOOR) != Ordering::Less
}

/// muse's declared set for one agent version. Only
/// [`Capability::ClientTools`] is version-dependent (gated on
/// [`MUSE_MCP_VERSION_FLOOR`]); every other command-backed capability is
/// `Native` on any parseable version, so an unknown or older server still
/// attempts what it can — the floor refuses nothing by itself, and in
/// particular a stub reporting `0.0.0-test` still sends. The session shell
/// is [`Unverified`](provider::CapabilityState::Unverified) on every
/// version: granted on record at 1.0.3–1.2.1 but never cleanly executed
/// there, and never probed at 1.3.0 or newer. Reasoning traces and sub-agent
/// turns are `Unverified` on every version: nobody has probed them live.
/// `Unverified` is attempted, never refused, so the shell still sends.
pub fn capabilities_for_version(version: &str) -> CapabilitySet {
    let client_tools = if compare_versions(version, MUSE_MCP_VERSION_FLOOR) != Ordering::Less {
        CapabilityState::Native
    } else {
        CapabilityState::Unavailable {
            reason: format!(
                "muse {version} never granted sessionMcp (seen on 1.2.1); \
                 client-side tools need muse {MUSE_MCP_VERSION_FLOOR} or newer"
            ),
        }
    };
    CapabilitySet::new([
        (Capability::SessionLifecycle, CapabilityState::Native),
        (Capability::ForkSession, CapabilityState::Native),
        (Capability::CompactSession, CapabilityState::Native),
        (Capability::SessionConfig, CapabilityState::Native),
        (Capability::SessionShell, CapabilityState::Unverified),
        (Capability::SubmitTurn, CapabilityState::Native),
        (Capability::SteerTurn, CapabilityState::Native),
        (Capability::TurnControl, CapabilityState::Native),
        (Capability::ModelCatalog, CapabilityState::Native),
        (Capability::Approvals, CapabilityState::Native),
        (Capability::Questions, CapabilityState::Native),
        (Capability::Transcript, CapabilityState::Native),
        (Capability::Account, CapabilityState::Native),
        (Capability::ClientTools, client_tools),
        (Capability::ReasoningTraces, CapabilityState::Unverified),
        (Capability::SubagentTurns, CapabilityState::Unverified),
    ])
}

