//! muse's declared set: complete, version-floored, and honest about what
//! was never probed.
//!
//! The `sessionMcp` facts are encoded, not rediscovered: the 1.3.0 grant
//! comes from the brief, the 1.2.1 never-granted observation from the live
//! fixtures. Reasoning traces, sub-agent turns, and the session shell stay
//! `Unverified` — not `Native` because they probably work. The shell's grant
//! *is* on record at 1.0.3–1.2.1, but every recorded `session/userShell`
//! execution there ends with the item in status `failed` (the sandbox was
//! unavailable, so the command never started), and 1.3.0+ was never probed
//! for `userShell` at all — a grant without a clean run is not `Native`
//! evidence. Like the arms tests, these prove the adapter's declaration,
//! not a real server's behavior: nothing here talks to one.

use std::path::PathBuf;

use muse_client::{MuseClient, MuseConfig};
use provider::{Capability, CapabilityState, Command, ConnectInfo, Provider};
use provider_muse::{
    capabilities_for_version, muse_version_supported, MUSE_MCP_VERSION_FLOOR, MUSE_VERSION_FLOOR,
};

fn puppet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py")
}

#[test]
fn every_capability_has_a_state_in_muse_set() {
    // A new `Capability` added later changes `COUNT`, which breaks the
    // declaration in `capabilities_for_version` at compile time; this test
    // is the runtime half — it fails until the new entry is consciously
    // placed in one of the arms below.
    let set = capabilities_for_version("1.3.0");
    for capability in Capability::all() {
        let state = set.state(capability);
        match capability {
            Capability::ClientTools => assert_eq!(
                *state,
                CapabilityState::Native,
                "1.3.0 grants sessionMcp: client tools are native"
            ),
            Capability::ReasoningTraces | Capability::SubagentTurns => assert_eq!(
                *state,
                CapabilityState::Unverified,
                "{capability:?} was never probed live: must stay Unverified, not Native"
            ),
            Capability::SessionShell => assert_eq!(
                *state,
                CapabilityState::Unverified,
                "userShell was granted at 1.0.3-1.2.1 but never cleanly executed there, \
                 and never probed at 1.3.0+: a guess stated as Native is worse than Unverified"
            ),
            _ => assert_eq!(
                *state,
                CapabilityState::Native,
                "{capability:?} is exercised against the recording puppet"
            ),
        }
    }
}

#[test]
fn client_tools_need_the_mcp_floor() {
    // 1.2.1 never granted sessionMcp: below the floor is `Unavailable`
    // with a reason, never a silent gap and never fake success.
    match capabilities_for_version("1.2.1").state(Capability::ClientTools) {
        CapabilityState::Unavailable { reason } => {
            assert!(!reason.is_empty(), "an Unavailable without a reason is useless");
            assert!(reason.contains("sessionMcp"), "the reason must name the missing grant");
        }
        other => panic!("client tools below the MCP floor must be Unavailable, got {other:?}"),
    }
    // At the floor and above: granted.
    for version in [MUSE_MCP_VERSION_FLOOR, "1.4.0", "2.0.0"] {
        assert_eq!(
            *capabilities_for_version(version).state(Capability::ClientTools),
            CapabilityState::Native,
            "{version} grants sessionMcp"
        );
    }
}

#[test]
fn version_floor_passes_at_and_above_and_only_below_fails() {
    assert_eq!(MUSE_VERSION_FLOOR, "1.2.1", "the floor is written down here");
    for version in ["1.2.1", "1.3.0", "1.4.0", "2.0.0", "1.3.0-R3401.1", "v1.3.0"] {
        assert!(muse_version_supported(version), "{version} must pass the floor");
    }
    for version in ["1.2.0", "1.1.1", "1.0.3", "0.9.9", "0.0.0-test", "garbage", ""] {
        assert!(!muse_version_supported(version), "{version} must fail the floor");
    }
}

#[test]
fn shell_is_unverified_until_a_clean_grant_is_on_record() {
    // No version has grant-log *plus* clean-execution evidence for
    // `userShell`, so no version declares it `Native`. `Unverified` is
    // attempted, never refused — the declaration stops guessing without
    // changing what still sends.
    for version in ["1.0.3", "1.1.1", "1.2.1", "1.3.0", "1.4.0"] {
        assert_eq!(
            *capabilities_for_version(version).state(Capability::SessionShell),
            CapabilityState::Unverified,
            "{version} has no clean userShell run on record"
        );
    }
    assert!(
        CapabilityState::Unverified.allows_attempt(),
        "Unverified is attempted, never refused: the shell still sends"
    );
}

#[test]
fn adapter_set_follows_the_negotiated_version_without_bricking_old_servers() {
    let script = puppet();
    assert!(script.exists(), "fake server script missing: {}", script.display());

    let client = MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: Vec::new(),
    })
    .expect("fake server spawns — is python3 on PATH?");
    let mut adapter = Provider::new(provider_muse::MuseAdapter::new(client));
    adapter.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");

    // The puppet reports `0.0.0-test`: client tools are `Unavailable` there,
    // while the command-backed set stays native — a version the floor does
    // not know still attempts what it can. Exact pins bricked this; floors
    // do not.
    let set = adapter.capabilities();
    assert!(
        matches!(
            set.state(Capability::ClientTools),
            CapabilityState::Unavailable { .. }
        ),
        "a 0.0.0-test server never granted sessionMcp"
    );
    assert_eq!(*set.state(Capability::SessionLifecycle), CapabilityState::Native);

    // And the enforced path still lets an unrelated command through to the
    // puppet, which answers `model/list`.
    adapter
        .send(Command::ListModels { session: None })
        .expect("a Native command on an old server must still be attempted");
}
