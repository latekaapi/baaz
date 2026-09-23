//! The tap must fail loudly, never silently.
//!
//! `tap_for` (`src/lib.rs`) drops an UNKNOWN server-request method silently
//! — deliberate, the card still arrives through the fold. But a KNOWN method
//! (`approval/request`, `userInput/request`) whose params fail to decode
//! used to vanish the same way: the person lost the prompt while the card
//! still appeared, so the app looked like it was waiting on nothing. That
//! case must surface as an error the caller can see.
//!
//! The puppet's `--emit-bad-tap` sends two server requests right after
//! `initialize`: an unknown `frobnicate/request` (must stay silent) and a
//! malformed `approval/request` (must surface). Both travel the real reader
//! thread, pump and fold. Like the arms test, this proves the adapter's
//! behavior, not a real server's — nothing here talks to one.

use std::path::PathBuf;
use std::time::Duration;

use muse_client::{MuseClient, MuseConfig};
use provider::{ConnectInfo, Provider, ProviderEvent};
use provider_muse::MuseAdapter;

fn puppet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py")
}

#[test]
fn malformed_approval_tap_surfaces_a_visible_error() {
    let script = puppet();
    assert!(script.exists(), "fake server script missing: {}", script.display());

    let client = MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: vec!["--emit-bad-tap".to_owned()],
    })
    .expect("fake server spawns — is python3 on PATH?");
    let mut adapter = Provider::new(MuseAdapter::new(client));
    adapter.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");

    // The unknown `frobnicate/request` must produce nothing at all — no tap,
    // no error — so the first event on the channel has to be the malformed
    // approval surfacing. The fold yields no deltas for either frame.
    let rx = adapter.events();
    let mut saw_deltas = false;
    let reason = loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(ProviderEvent::ConnectionLost { reason }) => break reason,
            Ok(ProviderEvent::Deltas { .. }) => {
                saw_deltas = true;
                continue;
            }
            Ok(other) => panic!("the bad tap must surface as an error, not {other:?}"),
            Err(_) => panic!("timed out waiting for the malformed tap to surface"),
        }
    };
    assert!(
        reason.contains("approval/request"),
        "the error must name the method whose params failed, got: {reason}"
    );
    assert!(
        !saw_deltas,
        "a tap nobody can decode must not fold transcript deltas either"
    );

    // One bad prompt must not end the session: the pump keeps running, so
    // shutting down still reports the child's exit exactly once.
    adapter.shutdown();
    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(ProviderEvent::ConnectionLost { reason }) => {
            assert!(reason.contains("exited"), "exit notice must describe the exit: {reason}");
        }
        other => panic!("expected the exit notice after shutdown, got {other:?}"),
    }
}
