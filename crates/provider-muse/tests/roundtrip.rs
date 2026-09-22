//! The muse adapter against the puppet: [`MuseAdapter`] wraps a client
//! spawned on `tests/fixtures/model_list_puppet.py` — the same fake-server
//! seam `muse-client`'s own tests use — and round-trips `ListModels` to the
//! `model/list` call. No real `muse` process, no prompt, no spent turn.

use std::path::PathBuf;
use std::time::Duration;

use muse_client::{MuseClient, MuseConfig};
use provider::{Ack, Command, ConnectInfo, ProviderAdapter};
use provider_muse::MuseAdapter;

fn puppet() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py")
}

#[test]
fn muse_adapter_round_trips_list_models_to_the_model_list_call() {
    let script = puppet();
    assert!(script.exists(), "fake server script missing: {}", script.display());

    let client = MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: Vec::new(),
    })
    .expect("fake server spawns — is python3 on PATH?");
    let mut adapter = MuseAdapter::new(client);
    assert_eq!(adapter.id(), aui_protocol::Provider::Muse);

    let handshake = adapter.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");
    assert_eq!(handshake.provider, aui_protocol::Provider::Muse);
    assert_eq!(handshake.agent_name, "fake-muse");

    // The puppet answers only `model/list`: this ack proves the command
    // reached the right `muse-client` call with its payload intact.
    match adapter.send(Command::ListModels { session: None }).expect("list-models") {
        Ack::ModelCatalog { models, provider } => {
            assert_eq!(provider, "fake");
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "fake-pro");
            assert_eq!(models[0].label, "Fake Pro");
            assert!(!models[0].active);
        }
        other => panic!("list-models must ack a catalog, not {other:?}"),
    }

    adapter.shutdown();
    // The pump forwards the puppet's exit as one connection notice, then
    // stops. Anything else here would mean the adapter invents traffic.
    let mut notices = 0;
    let rx = adapter.events();
    while let Ok(event) = rx.recv_timeout(Duration::from_secs(5)) {
        match event {
            provider::ProviderEvent::ConnectionLost { .. } => notices += 1,
            other => panic!("unexpected adapter traffic after shutdown: {other:?}"),
        }
    }
    assert_eq!(notices, 1, "the adapter must report the disconnect exactly once");
}
