//! muse's declared set: what the wire has shown, and the floors beneath it.
//!
//! Two facts established on this machine, encoded here rather than
//! rediscovered: muse 1.3.0 grants `sessionMcp` and `userShell`; muse 1.2.1
//! never granted `sessionMcp`. The command-backed entries are `Native`
//! because every [`Command`](provider::Command) arm is exercised against
//! the recording puppet (method *and* params) and the grants above are on
//! record in the live fixtures; reasoning traces and sub-agent turns have
//! no live probe in this task, so they stay
//! [`Unverified`](provider::CapabilityState::Unverified) — not `Native`
//! because they probably work.
//!
//! Nothing here talks to a live server. Every entry is a claim until a
//! live probe checks it.

use std::cmp::Ordering;

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
/// [`MUSE_MCP_VERSION_FLOOR`]); every command-backed capability is `Native`
/// on any parseable version, so an unknown or older server still attempts
/// what it can — the floor refuses nothing by itself, and in particular a
/// stub reporting `0.0.0-test` still sends. Reasoning traces and sub-agent
/// turns are [`Unverified`](provider::CapabilityState::Unverified) on every
/// version: nobody has probed them live.
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
        (Capability::SessionShell, CapabilityState::Native),
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

/// Numeric `major.minor.patch` comparison of `version` against `floor`.
/// A leading `v` is ignored, anything from the first `-`/`+` on is a
/// trailer and ignored, and missing components are zero (`"1.3"` is
/// `1.3.0`). Anything unparseable compares below everything: fail closed.
fn compare_versions(version: &str, floor: &str) -> Ordering {
    match (parse_version(version), parse_version(floor)) {
        (Some(version), Some(floor)) => version.cmp(&floor),
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
    }
}

/// Parse `major.minor.patch` numerically. Returns `None` when the leading
/// component is not a number at all.
fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.strip_prefix('v').unwrap_or(version);
    let version = version.split(['-', '+']).next().unwrap_or(version);
    let mut parts = version.split('.');
    let mut next = || match parts.next() {
        None | Some("") => Some(0),
        Some(part) => part.parse::<u64>().ok(),
    };
    Some((next()?, next()?, next()?))
}
