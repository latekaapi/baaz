//! The one test that talks to a real `muse serve`.
//!
//! It is `#[ignore]`d, because it spawns a child process and needs the `muse`
//! binary on `PATH`. Run it with:
//!
//! ```text
//! cargo test -p muse-client -- --ignored live_echo
//! ```
//!
//! It uses the **echo** provider, which is free and emits one canned
//! `agentMessage`. Nothing here may run against `meta`.

use std::time::{Duration, Instant};

use aui_protocol::{Block, Turn};
use muse_adapter::MuseFold;
use muse_client::schema::{
    ClientCapabilities, SessionStartParams, TurnInputPart, TurnStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseConfig, MuseEvent};

#[test]
#[ignore = "spawns a real `muse serve` child"]
fn live_echo() {
    let workspace = std::env::temp_dir().join(format!("muse-client-live-{}", new_command_id()));
    std::fs::create_dir_all(&workspace).expect("temp workspace");

    // Durable, not `--no-session-log`: an ephemeral host emits no view events,
    // so there would be nothing for the fold to see. See docs/01-transport.md §4.
    let client = MuseClient::spawn(&MuseConfig::default())
    .expect("muse serve starts — is `muse` on PATH?");
    let events = client.events();

    let (init, warning) = client
        .initialize(
            "harness",
            env!("CARGO_PKG_VERSION"),
            ClientCapabilities {
                requested_capabilities: Some(vec!["userShell".into()]),
                ..Default::default()
            },
        )
        .expect("initialize");
    assert_eq!(init.server_info.name, "muse");
    // A fingerprint mismatch is a warning condition, never a failure.
    if let Some(warning) = &warning {
        eprintln!("schema warning: {warning:?}");
    }

    let started = client
        .session_start(&SessionStartParams {
            command_id: new_command_id(),
            workspace_root: Some(workspace.to_string_lossy().into_owned()),
            provider_id: Some("echo".into()),
            ..Default::default()
        })
        .expect("session/start on the free echo provider");
    let session_id = started.session.session_id.clone();

    let turn = client
        .turn_start(&TurnStartParams {
            command_id: new_command_id(),
            session_id: session_id.clone(),
            input: vec![TurnInputPart::text("say hi")],
            ..Default::default()
        })
        .expect("turn/start");
    // For a fresh turn the ack's `turnId` equals its `commandId`.
    assert_eq!(turn.turn_id, turn.command_id);

    let mut fold = MuseFold::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut completed = false;
    while Instant::now() < deadline && !completed {
        let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else { continue };
        if let MuseEvent::Notification { method, params, .. } = &event {
            if method == "turn/completed"
                && params.get("turnId").and_then(|v| v.as_str()) == Some(turn.turn_id.as_str())
            {
                completed = true;
            }
        }
        if matches!(event, MuseEvent::Closed(_)) {
            panic!("muse serve exited before the turn finished");
        }
        fold.apply(event);
    }
    assert!(completed, "the echo turn never completed");

    let session = fold.session(&session_id).expect("the fold saw the session");
    let user = session
        .turns
        .iter()
        .find(|t| matches!(t, Turn::User { .. }))
        .expect("the person's message became a user turn");
    let Turn::User { text, .. } = user else { unreachable!() };
    assert_eq!(text, "say hi");

    let assistant = session
        .turns
        .iter()
        .find(|t| t.id() == turn.turn_id)
        .expect("the reply became an assistant turn");
    let text = assistant
        .blocks()
        .iter()
        .find_map(|block| match block {
            Block::Text { text, streaming } => {
                assert!(!streaming, "the text block should be settled once the turn completes");
                Some(text.clone())
            }
            _ => None,
        })
        .expect("the assistant turn carries a text block");
    assert!(!text.trim().is_empty(), "the echo provider replied with nothing");

    let _ = std::fs::remove_dir_all(&workspace);
}
