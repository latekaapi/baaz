//! `harness-probe` — drive a real `muse serve` and print what the fold makes of it.
//!
//! Spawns `muse serve --trust-workspace`, initializes, starts an **echo**
//! session in a temporary workspace, lists models, sends one turn, and prints
//! every folded [`aui_protocol::Delta`] as JSON, one per line.
//!
//! It runs **durable**, not `--no-session-log`, because an ephemeral host emits
//! no view events at all — see `docs/01-transport.md` §4. Set
//! `HARNESS_PROBE_EPHEMERAL=1` to spawn with `--no-session-log` and watch that
//! happen.
//!
//! ```text
//! cargo run -p muse-adapter --bin harness-probe
//! cargo run -p muse-adapter --bin harness-probe -- "say something else"
//! ```
//!
//! The echo provider is free, which is why the probe uses it. It emits one
//! canned `agentMessage` and nothing else: no reasoning, no tool calls, no
//! approvals. That is enough to prove framing, ids, delta shape and the fold.

use std::time::{Duration, Instant};

use muse_adapter::MuseFold;
use muse_client::schema::{
    ClientCapabilities, ModelListParams, SessionStartParams, TurnInputPart, TurnStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseConfig, MuseEvent};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let prompt = std::env::args().nth(1).unwrap_or_else(|| "say hi".to_owned());

    let workspace = std::env::temp_dir().join(format!("harness-probe-{}", new_command_id()));
    std::fs::create_dir_all(&workspace)?;

    let client = MuseClient::spawn(&MuseConfig {
        // Durable: an ephemeral host accepts the turn and then says nothing.
        no_session_log: std::env::var_os("HARNESS_PROBE_EPHEMERAL").is_some(),
        ..MuseConfig::default()
    })?;
    let events = client.events();

    let (init, warning) = client.initialize(
        "harness",
        env!("CARGO_PKG_VERSION"),
        ClientCapabilities { requested_capabilities: Some(vec!["userShell".into()]), ..Default::default() },
    )?;
    eprintln!("server {} {}", init.server_info.name, init.server_info.version);
    if let Some(warning) = warning {
        eprintln!("warning: {warning:?}");
    }

    let session = client.session_start(&SessionStartParams {
        command_id: new_command_id(),
        workspace_root: Some(workspace.to_string_lossy().into_owned()),
        provider_id: Some("echo".into()),
        ..Default::default()
    })?;
    let session_id = session.session.session_id.clone();
    eprintln!("session {session_id}");

    let models = client.model_list(&ModelListParams { session_id: Some(session_id.clone()) })?;
    eprintln!("catalog {} ({} models)", models.provider_id, models.models.len());
    for model in &models.models {
        eprintln!(
            "  {} {}{}",
            model.model_id,
            model.context_limit.map(|n| format!("{n} ctx")).unwrap_or_default(),
            if model.is_default { " · default" } else { "" }
        );
    }

    let command_id = new_command_id();
    let turn = client.turn_start(&TurnStartParams {
        command_id: command_id.clone(),
        session_id: session_id.clone(),
        input: vec![TurnInputPart::text(&prompt)],
        ..Default::default()
    })?;
    eprintln!("turn {} · {:?}", turn.turn_id, turn.disposition);

    let mut fold = MuseFold::new();
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let Ok(event) = events.recv_timeout(Duration::from_millis(500)) else {
            if Instant::now() > deadline {
                eprintln!("gave up waiting for the turn to finish");
                break;
            }
            continue;
        };
        let finished = matches!(
            &event,
            MuseEvent::Notification { method, params, .. }
                if method == "turn/completed"
                    && params.get("turnId").and_then(|v| v.as_str()) == Some(turn.turn_id.as_str())
        );
        let closed = matches!(event, MuseEvent::Closed(_));
        for delta in fold.apply(event) {
            println!("{}", serde_json::to_string(&delta)?);
        }
        if finished || closed {
            break;
        }
    }

    eprintln!("---- folded session ----");
    if let Some(session) = fold.session(&session_id) {
        println!("{}", serde_json::to_string_pretty(session)?);
    }
    let _ = std::fs::remove_dir_all(&workspace);
    Ok(())
}
