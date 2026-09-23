//! muse's declared set: complete, version-floored, and honest about what
//! was never probed.
//!
//! The two grant facts are encoded, not rediscovered: muse 1.3.0 grants
//! `sessionMcp` and `userShell`; muse 1.2.1 never granted `sessionMcp`.
//! Reasoning traces and sub-agent turns stay `Unverified` — not `Native`
//! because they probably work. Like the arms tests, these prove the
//! adapter's declaration, not a real server's behavior: nothing here talks
//! to one.

use std::path::PathBuf;

use muse_client::{MuseClient, MuseConfig};
use provider::{Capability, CapabilityState, Command, ConnectInfo, ProviderAdapter};
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
fn shell_is_native_wherever_user_shell_was_granted() {
    // `userShell` is on record as granted from 1.0.3 through 1.3.0, so the
    // session shell is native across the whole supported range — and the
    // grant below the seam floor changes nothing about the declaration.
    for version in ["1.0.3", "1.1.1", "1.2.1", "1.3.0"] {
        assert_eq!(
            *capabilities_for_version(version).state(Capability::SessionShell),
            CapabilityState::Native,
            "{version} granted userShell"
        );
    }
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
    let mut adapter = provider_muse::MuseAdapter::new(client);
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
