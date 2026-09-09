//! The one test that talks to a real `muse serve`.
//!
//! It is `#[ignore]`d, because it spawns a child process and needs the `muse`
//! binary on `PATH`. Run it with:
//!
//! ```text
//! cargo test -p muse-client -- --ignored live_echo
//! ```
//!
//! # Every test in this file spends a real subscription turn
//!
//! It routes through the **echo** provider, which phase 1 recorded as free and
//! which is not. On a signed-in machine the session log
//! (`~/.local/share/muse/sessions/…/session.jsonl`) records `provider_id: echo`
//! on its `command_intake` record and then a metadata record naming
//! `provider_id: meta, model_id: muse-spark-1.3-contributor`; the session index
//! follows the metadata record. The turn bills reasoning tokens
//! (`fixtures/msp/transcript-echo.jsonl` carries `reasoningTokens: 94`), carries
//! a provider response id, and comes back with varied real text rather than one
//! canned line. `--provider` picks a route, not a bill.
//!
//! So these are not free tests to leave in a loop. Budget the turn before you
//! run one. The offline equivalents — `muse-adapter`'s capture fixtures and its
//! `a_live_fold_and_a_backfilled_fold_agree` parity gate — cover the same fold
//! for nothing, and they are what CI runs.

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

/// **F3 — live / backfill parity.** The same session, folded two ways, must be
/// the same session.
///
/// A transcript can arrive twice: live, as a stream of `item/started`,
/// `item/delta` and `item/completed`; or backfilled, as `view/page` events that
/// never replay a delta and hand every item over whole. If those two paths ever
/// disagree, then reopening a session changes what it says — which is the one
/// thing a transcript may never do.
///
/// # This test spends one real subscription turn per run
///
/// It is `#[ignore]`d and it must stay that way. There is **no free provider on
/// a signed-in machine.** A session started with `providerId: "echo"` records
/// `provider_id: echo` on its `command_intake` record and then a metadata
/// record naming `provider_id: meta, model_id: muse-spark-1.3-contributor`; it
/// bills reasoning tokens, carries provider response ids, and answers with
/// varied real text. `--provider echo` picks a *route*, not a free ride. So
/// this test is **not the F3 gate**.
///
/// The gate is
/// `muse-adapter/tests/fixtures.rs::a_live_fold_and_a_backfilled_fold_agree`,
/// which derives the backfill stream from the checked-in captures the way
/// `view/page` serves it — drop `item/started` and `item/delta`, keep
/// `item/completed` and every session-level notification — and costs nothing.
/// It is what actually found the ordering bug this test was written to catch.
///
/// Run this one only to re-verify that derivation against a live server, and
/// only with the turn budgeted first:
///
/// ```text
/// cargo test -p muse-client -- --ignored live_backfill_parity   # spends a turn
/// ```
///
/// Two families of difference are **normalised** rather than fixed, because
/// backfill genuinely cannot know them and pretending otherwise would be worse:
///
/// * **streaming flags.** A live text block is `streaming: true` while its
///   deltas arrive; a backfilled one was never mid-flight. Both settle to
///   `false` on the turn's terminal, so the comparison settles them.
/// * **turn metas.** `TurnMeta` is assembled from `session/tokenUsage`, which
///   `view/page` does not serve; a backfilled turn therefore has the default
///   meta and the live one has real numbers.
///
/// Everything else — turn order, turn ids, block order, block content, the
/// user/assistant split — must match exactly.
#[test]
#[ignore = "spawns a real `muse serve` child"]
fn live_backfill_parity() {
    use muse_client::schema::{SessionResumeParams, ViewPageParams};

    let workspace = std::env::temp_dir().join(format!("muse-parity-{}", new_command_id()));
    std::fs::create_dir_all(&workspace).expect("temp workspace");

    let client = MuseClient::spawn(&MuseConfig::default()).expect("muse serve starts");
    let events = client.events();
    client
        .initialize("harness", env!("CARGO_PKG_VERSION"), ClientCapabilities::default())
        .expect("initialize");

    let started = client
        .session_start(&SessionStartParams {
            command_id: new_command_id(),
            workspace_root: Some(workspace.to_string_lossy().into_owned()),
            // The cap is on `meta`; parity is a fold question and echo answers it.
            provider_id: Some("echo".into()),
            ..Default::default()
        })
        .expect("session/start on echo");
    let session_id = started.session.session_id.clone();

    let turn = client
        .turn_start(&TurnStartParams {
            command_id: new_command_id(),
            session_id: session_id.clone(),
            input: vec![TurnInputPart::text("parity, please")],
            ..Default::default()
        })
        .expect("turn/start");

    // ---- 1. fold it live
    let mut live = MuseFold::new();
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
        live.apply(event);
    }
    assert!(completed, "the echo turn never completed");

    // ---- 2. fold the same session out of the view, from the start
    client
        .session_resume(&SessionResumeParams {
            command_id: new_command_id(),
            session_id: session_id.clone(),
            // The whole point: attach without inline history, then page the view.
            exclude_items: Some(true),
            cursor: None,
            history: None,
        })
        .expect("session/resume");

    let mut backfill = MuseFold::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = client
            .view_page(&ViewPageParams {
                session_id: session_id.clone(),
                limit: 1000,
                cursor: cursor.clone(),
                direction: None,
                anchor: None,
            })
            .expect("view/page");
        let empty = page.events.is_empty();
        for event in page.events {
            let params = serde_json::to_value(&event.params).expect("params serialize");
            backfill.apply(MuseEvent::Notification {
                method: event.method,
                cursor: params.get("viewCursor").and_then(|v| v.as_str()).map(str::to_owned),
                session_id: Some(session_id.clone()),
                params,
            });
        }
        match page.next_cursor {
            Some(next) if !empty => cursor = Some(next),
            _ => break,
        }
    }

    // ---- 3. compare, after normalising what backfill cannot know
    let normalise = |fold: &MuseFold| -> Vec<Turn> {
        let mut turns = fold.session(&session_id).expect("session exists").turns.clone();
        for turn in &mut turns {
            if let Turn::Assistant { blocks, meta, .. } = turn {
                *meta = aui_protocol::TurnMeta::default();
                for block in blocks.iter_mut() {
                    match block {
                        Block::Text { streaming, .. } => *streaming = false,
                        Block::Thinking { state, .. } => *state = aui_protocol::ThinkingState::Done,
                        _ => {}
                    }
                }
            }
        }
        turns
    };

    let (a, b) = (normalise(&live), normalise(&backfill));
    assert_eq!(
        a.len(),
        b.len(),
        "live folded {} turns, backfill folded {}:\nlive:     {a:#?}\nbackfill: {b:#?}",
        a.len(),
        b.len()
    );
    for (live_turn, backfilled) in a.iter().zip(&b) {
        assert_eq!(live_turn, backfilled, "a turn folded differently from the view than from the stream");
    }

    let _ = std::fs::remove_dir_all(&workspace);
}
