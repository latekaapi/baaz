//! The hub against the puppet: one child, several sessions, each lane
//! hearing only its own.
//!
//! The puppet (`tests/fixtures/model_list_puppet.py --emit-turn`) answers a
//! `turn/start` with a whole short turn for that session — `turn/started`,
//! an echoing `agentMessage`, `turn/completed` — so turns of two sessions
//! interleave on one pipe exactly as they would on a real `muse serve`. No
//! model is ever reached.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use muse_client::{MuseClient, MuseConfig};
use provider::{Ack, Command, ConnectInfo, Provider, ProviderAdapter, ProviderEvent, SubmissionPart};
use provider_muse::{MuseHub, MuseLane};

fn puppet(extra: &[&str]) -> MuseClient {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/model_list_puppet.py");
    MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: extra.iter().map(|arg| arg.to_string()).collect(),
    })
    .expect("fake server spawns — is python3 on PATH?")
}

/// A connected hub and its first lane (whose connect shook hands).
fn hub(extra: &[&str]) -> (MuseHub, Provider) {
    let hub = MuseHub::from_client(puppet(extra));
    let mut first: MuseLane = hub.lane();
    first.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("handshake");
    (hub, Provider::new(first))
}

fn lane(hub: &MuseHub) -> Provider {
    let mut lane = hub.lane();
    lane.connect(&ConnectInfo::new("baaz", "0.1.0")).expect("a lane reports the handshake");
    Provider::new(lane)
}

fn resume(provider: &Provider, session: &str) {
    let ack = provider
        .send(Command::ResumeSession {
            request_id: format!("r-{session}"),
            session_id: session.into(),
            cursor: None,
            metadata_only: true,
            mode: None,
        })
        .expect("resume");
    assert!(matches!(ack, Ack::Session { ref session_id, .. } if session_id == session), "{ack:?}");
}

fn submit(provider: &Provider, session: &str, text: &str) {
    provider
        .send(Command::SubmitInput {
            request_id: format!("c-{session}"),
            session_id: session.into(),
            parts: vec![SubmissionPart::Text(text.into())],
            display_text: None,
            effort: None,
        })
        .expect("submit");
}

/// Every event `provider` delivers until `done` says so (or 5 s pass).
fn collect(provider: &Provider, done: impl Fn(&[ProviderEvent]) -> bool) -> Vec<ProviderEvent> {
    let rx = provider.events();
    let mut seen = Vec::new();
    let until = Instant::now() + Duration::from_secs(5);
    while !done(&seen) && Instant::now() < until {
        if let Ok(event) = rx.recv_timeout(Duration::from_millis(100)) {
            seen.push(event);
        }
    }
    seen
}

fn settled(events: &[ProviderEvent], session: &str) -> bool {
    events.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { session_id, .. } if session_id == session))
}

fn sessions_of(events: &[ProviderEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::Deltas { session_id, .. } => session_id.clone(),
            ProviderEvent::TurnRunning { session_id, .. }
            | ProviderEvent::TurnSettled { session_id, .. }
            | ProviderEvent::SessionFacts { session_id, .. }
            | ProviderEvent::HeaderChanged { session_id, .. }
            | ProviderEvent::ApprovalModeChanged { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn two_sessions_on_one_child_each_hear_only_their_own() {
    let (hub, a) = hub(&["--emit-turn"]);
    let b = lane(&hub);
    resume(&a, "sa");
    resume(&b, "sb");
    submit(&a, "sa", "first");
    submit(&b, "sb", "second");
    let heard_a = collect(&a, |seen| settled(seen, "sa"));
    let heard_b = collect(&b, |seen| settled(seen, "sb"));
    assert!(settled(&heard_a, "sa") && settled(&heard_b, "sb"), "both turns ran");
    assert!(sessions_of(&heard_a).iter().all(|id| id == "sa"), "lane a heard only sa: {heard_a:?}");
    assert!(sessions_of(&heard_b).iter().all(|id| id == "sb"), "lane b heard only sb: {heard_b:?}");
    let echoed = heard_a.iter().any(|event| matches!(event, ProviderEvent::Deltas { deltas, .. }
        if format!("{deltas:?}").contains("echo: first")));
    assert!(echoed, "the reply reached its own lane");
    // The rows hear both settles on the connection stream.
    let rows = hub.connection_events();
    let mut row_settles = Vec::new();
    while let Ok(event) = rows.recv_timeout(Duration::from_millis(200)) {
        if let ProviderEvent::TurnSettled { session_id, .. } = event {
            row_settles.push(session_id);
        }
    }
    row_settles.sort();
    assert_eq!(row_settles, ["sa", "sb"]);
}

#[test]
fn events_before_a_lane_binds_wait_for_it() {
    let (hub, sender) = hub(&["--emit-turn"]);
    // A turn on a session no lane follows yet.
    submit(&sender, "sc", "early");
    std::thread::sleep(Duration::from_millis(300));
    let late = lane(&hub);
    resume(&late, "sc");
    let heard = collect(&late, |seen| settled(seen, "sc"));
    assert!(settled(&heard, "sc"), "the waiting events were handed over on bind: {heard:?}");
}

#[test]
fn dropping_a_lane_leaves_the_child_for_the_others() {
    let (hub, a) = hub(&["--emit-turn"]);
    let b = lane(&hub);
    resume(&a, "sa");
    resume(&b, "sb");
    drop(a);
    submit(&b, "sb", "still here");
    let heard = collect(&b, |seen| settled(seen, "sb"));
    assert!(settled(&heard, "sb"), "the shared child outlived the dropped lane");
    assert!(!hub.followed().contains(&"sa".to_owned()), "the dropped lane's session is unbound");
}

#[test]
fn a_decision_without_a_stage_token_takes_the_pending_stage() {
    let records = std::env::temp_dir().join(format!("provider-muse-hub-{}.jsonl", std::process::id()));
    let _ = std::fs::remove_file(&records);
    let (_hub, lane) = hub(&["--record", records.to_str().unwrap()]);
    resume(&lane, "s-1");
    // The puppet's pending set holds approval a-1 at stage {a-1, 3}.
    let pending = lane.send(Command::ListPending { session_id: "s-1".into() }).expect("pending");
    assert!(matches!(pending, Ack::PendingWork { ref approvals, .. } if approvals.len() == 1));
    lane.send(Command::DecideApproval {
        request_id: "d-1".into(),
        session_id: "s-1".into(),
        approval: "a-1".into(),
        choice: "c-yes".into(),
        stage_token: None,
        feedback: None,
    })
    .expect("decide");
    let text = std::fs::read_to_string(&records).expect("records");
    let decide = text
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|record| record["method"] == "approval/decide")
        .expect("a decision went out");
    assert_eq!(decide["params"]["requirementId"], serde_json::json!({"approvalId": "a-1", "sourceIndex": 3}));
    let _ = std::fs::remove_file(&records);
}

#[test]
fn a_page_lands_on_the_lane_and_then_says_so() {
    let (_hub, lane) = hub(&[]);
    resume(&lane, "s-1");
    let ack = lane
        .send(Command::PageTranscript { session_id: "s-1".into(), after: None, limit: 50, backward: false })
        .expect("page");
    assert!(matches!(ack, Ack::TranscriptPage { ref deltas, next_cursor: None } if deltas.is_empty()), "{ack:?}");
    let heard = collect(&lane, |seen| seen.iter().any(|e| matches!(e, ProviderEvent::TranscriptPaged { .. })));
    assert!(
        heard.iter().any(|event| matches!(event,
            ProviderEvent::TranscriptPaged { session_id, next_cursor: None } if session_id == "s-1")),
        "the page's landing is in the stream: {heard:?}"
    );
}

#[test]
fn account_changes_reach_the_connection_stream_and_no_session_lane() {
    let (hub, lane) = hub(&[]);
    resume(&lane, "s-1");
    let rows = hub.connection_events();
    lane.send(Command::LogOut).expect("log out");
    let mut account = None;
    let until = Instant::now() + Duration::from_secs(5);
    while account.is_none() && Instant::now() < until {
        if let Ok(ProviderEvent::AccountChanged { lane, .. }) = rows.recv_timeout(Duration::from_millis(100)) {
            account = Some(lane);
        }
    }
    assert_eq!(account, Some(provider::AccountLane::LoggedOut), "the app hears the sign-out");
    let heard = collect(&lane, |_| false);
    assert!(
        !heard.iter().any(|event| matches!(event, ProviderEvent::AccountChanged { .. })),
        "a session lane never sees account events: {heard:?}"
    );
}
