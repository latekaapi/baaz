//! The permission exchange end to end, replayed offline.
//!
//! `fixtures/claude-code/permission.jsonl` is bidirectional: lines the host
//! sent carry a `{"_dir":"host->cli","frame":{…}}` envelope, child output is
//! bare. This test drives the child side through the same
//! [`step_line`](provider_claude_code::fold::step_line) the live pump uses,
//! checks the request surfaced — and checks the only answer on the wire is
//! the one an explicit decision minted.
//!
//! No `claude` process is ever spawned. The adapter below names a binary
//! that does not exist, so a stray live call would fail loudly instead of
//! spending the owner's money.

use aui_protocol::{Block, ToolBody, ToolStatus};
use provider::{Command, ProviderAdapter, ProviderError};
use provider_claude_code::fold::{decide_approval, step_line, ClaudeFold};
use provider_claude_code::frame::{approval_headline, decode_line};
use provider_claude_code::ClaudeCodeAdapter;

struct Exchange {
    host_sent: Vec<serde_json::Value>,
    child_out: Vec<String>,
}

fn load_exchange() -> Exchange {
    let path = format!(
        "{}/../../fixtures/claude-code/permission.jsonl",
        env!("CARGO_MANIFEST_DIR")
    );
    let text = std::fs::read_to_string(path).expect("fixture reads");
    let mut host_sent = Vec::new();
    let mut child_out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line).expect("fixture is JSON");
        if value.get("_dir").and_then(|dir| dir.as_str()) == Some("host->cli") {
            host_sent.push(value.get("frame").expect("envelope carries a frame").clone());
        } else {
            child_out.push(line.to_owned());
        }
    }
    assert_eq!(host_sent.len(), 3, "initialize, one user turn, one answer");
    Exchange { host_sent, child_out }
}

/// THE HANG TEST, exchange half. If the `can_use_tool` request went
/// unanswered the child would wait silently forever (no timeout was
/// observed in the probe). So: the replay must surface exactly one pending
/// request with a tap on the shoulder, and the host side of the fixture
/// must carry exactly its answer — produced here again from an explicit
/// decision, byte-equal.
#[test]
fn unanswered_request_fails_and_explicit_allow_answers() {
    let exchange = load_exchange();
    let mut fold = ClaudeFold::new();
    let mut events = Vec::new();
    for line in &exchange.child_out {
        step_line(&mut fold, line, &mut |event| events.push(event));
    }

    // Surfaced: one pending request, fully shaped.
    let pending = fold.pending_approvals();
    assert_eq!(pending.len(), 1, "the request must surface, not fold away");
    let request = &pending[0];
    assert_eq!(request.request_id, "b4554ab2-30c2-4271-8451-dd9d6f5e226d");
    assert_eq!(request.tool_name, "mcp__baaz__ping");
    assert_eq!(request.display_name, "Ping");
    assert_eq!(request.input, serde_json::json!({}));
    assert_eq!(request.tool_use_id, "toolu_01CPKoR3sS6ZvHfqgQtJWZU5");
    assert_eq!(request.suggestions.len(), 1);

    // …with its tap on the shoulder, naming the session and the headline.
    let taps: Vec<_> = events
        .iter()
        .filter_map(|event| match event {
            provider::ProviderEvent::ApprovalRequested {
                session_id,
                approval_id,
                headline,
            } => Some((session_id, approval_id, headline)),
            _ => None,
        })
        .collect();
    assert_eq!(taps.len(), 1, "one tap per request, never a silent wait");
    assert_eq!(taps[0].0, "fdd19e9e-e32b-4a7b-b88c-437271a08851");
    assert_eq!(taps[0].1, "b4554ab2-30c2-4271-8451-dd9d6f5e226d");
    assert_eq!(taps[0].2, &approval_headline(request));

    // Answered: the host side carries a control_response for exactly this
    // request — fail here and the turn hung.
    let answers: Vec<&serde_json::Value> = exchange
        .host_sent
        .iter()
        .filter(|frame| {
            frame.get("type").and_then(|kind| kind.as_str()) == Some("control_response")
        })
        .collect();
    assert_eq!(answers.len(), 1, "the request must be answered, or the turn hangs");
    assert_eq!(
        answers[0]
            .get("response")
            .and_then(|response| response.get("request_id"))
            .and_then(|id| id.as_str()),
        Some("b4554ab2-30c2-4271-8451-dd9d6f5e226d")
    );

    // …and the answer is what an explicit allow decision mints: the test
    // supplies the decision, nothing inside the adapter did.
    let minted: serde_json::Value =
        serde_json::from_str(&decide_approval(request, "allow", None).expect("allow decides"))
            .expect("encodes JSON");
    assert_eq!(&minted, answers[0]);

    // The exchange then completes: the allowed tool's PONG lands on its
    // card and the turn finishes with the provider's real cost.
    let pongs = events
        .iter()
        .filter_map(|event| match event {
            provider::ProviderEvent::Deltas { deltas, .. } => Some(deltas),
            _ => None,
        })
        .flatten()
        .filter(|delta| match delta {
            aui_protocol::Delta::BlockUpdated {
                block:
                    Block::ToolCall {
                        body: ToolBody::Mcp { result_json, .. },
                        status: ToolStatus::Success,
                        ..
                    },
                ..
            } => result_json.contains("PONG"),
            _ => false,
        })
        .count();
    assert_eq!(pongs, 1, "the allowed tool ran and its PONG completed the card");
    let costs: Vec<f64> = events
        .iter()
        .filter_map(|event| match event {
            provider::ProviderEvent::Deltas { deltas, .. } => Some(deltas),
            _ => None,
        })
        .flatten()
        .filter_map(|delta| match delta {
            aui_protocol::Delta::TurnFinished { meta, .. } => Some(meta.cost_usd),
            _ => None,
        })
        .collect();
    assert!(
        costs.iter().any(|cost| (cost - 0.0188967).abs() < 1e-9),
        "real total_cost_usd finishes the turn, not 0.0: {costs:?}"
    );
}

/// The adapter never auto-allows: deciding an id nobody asked for is
/// rejected, and with no child running nothing is pending and nothing is
/// delivered. The binary name does not exist, so any stray spawn would
/// fail loudly — but none happens: both answers come from fold state.
#[test]
fn adapter_rejects_unknown_approvals_and_invents_none() {
    let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
    let error = adapter
        .dispatch(Command::DecideApproval {
            request_id: "d-1".into(),
            session_id: "fdd19e9e-e32b-4a7b-b88c-437271a08851".into(),
            approval: "req-nobody-asked-for".into(),
            choice: "allow".into(),
            stage_token: None,
            feedback: None,
        })
        .expect_err("unknown approvals decide nothing");
    assert!(
        matches!(error, ProviderError::Rejected { .. }),
        "unknown id is refused, never answered: {error}"
    );
    let error = adapter
        .dispatch(Command::ListPending {
            session_id: "fdd19e9e-e32b-4a7b-b88c-437271a08851".into(),
        })
        .expect_err("no child, no pending set");
    assert!(
        matches!(error, ProviderError::Unavailable { .. }),
        "pending without a child is unavailable, never invented: {error}"
    );
}

/// The child-side `control_response` (the initialize handshake answer) is
/// host-initiated traffic: it folds to nothing and raises no tap.
#[test]
fn handshake_responses_need_no_answer() {
    let exchange = load_exchange();
    let responses: Vec<String> = exchange
        .child_out
        .iter()
        .filter(|line| {
            decode_line(line).is_ok_and(|frame| matches!(frame, provider_claude_code::frame::Frame::ControlResponse { .. }))
        })
        .cloned()
        .collect();
    assert_eq!(responses.len(), 1, "the initialize answer in the fixture");
    let mut fold = ClaudeFold::new();
    let mut events = Vec::new();
    for line in &responses {
        step_line(&mut fold, line, &mut |event| events.push(event));
    }
    assert!(events.is_empty(), "handshake answers emit nothing");
    assert!(fold.pending_approvals().is_empty());
}
