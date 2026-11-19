//! Golden transcripts for the Codex fold (docs/82 5.3): every fixture under
//! `fixtures/codex/`, folded the way the adapter folds it, must produce the
//! session recorded in `tests/golden/` byte for byte. See
//! `provider/tests/support/golden.rs` for what a golden holds.

#[path = "../../provider/tests/support/golden.rs"]
mod golden;

use std::collections::HashMap;
use std::path::Path;

use aui_protocol::Provider;
use golden::{Mask, Recorder};
use provider::ProviderEvent;
use provider_codex::fold::{step_line, CodexFold};
use serde_json::Value;

const SESSION: &str = "codex";

/// Fold one fixture as the adapter does: server frames through
/// `step_line`, a `thread/resume` answer through `apply_resume_turns`, and
/// the person's approval answers (client responses) through
/// `record_decision`. After every line the child is also lost on a copy.
fn record(path: &Path) -> Recorder {
    let mut fold = CodexFold::new();
    fold.set_model("golden-model");
    fold.set_workspace("/golden/workspace");
    let mut recorder = Recorder::new(Provider::Codex, Mask::Replay);
    // Client request id -> method; server request id -> (method, params).
    let mut client_requests: HashMap<String, String> = HashMap::new();
    let mut server_requests: HashMap<String, (String, Value)> = HashMap::new();
    let text = std::fs::read_to_string(path).expect("fixture reads");
    for (number, line) in text.lines().enumerate() {
        let envelope: Value = serde_json::from_str(line).expect("fixture line parses");
        let frame = &envelope["frame"];
        let id = frame.get("id").map(Value::to_string);
        let method = frame.get("method").and_then(Value::as_str);
        match envelope["_dir"].as_str() {
            Some("client->server") => match (id, method) {
                (Some(id), Some(method)) => {
                    client_requests.insert(id, method.to_owned());
                }
                (Some(id), None) => {
                    if let (Some((method, params)), Some(decision)) = (
                        server_requests.get(&id),
                        frame.pointer("/result/decision").and_then(Value::as_str),
                    ) {
                        let approval = if method == "mcpServer/elicitation/request" {
                            format!("{}{id}", provider_codex::child::MCP_ELICITATION_ID_PREFIX)
                        } else {
                            params.get("itemId").and_then(Value::as_str).unwrap_or_default().to_owned()
                        };
                        fold.record_decision(&approval, decision);
                    }
                }
                _ => {}
            },
            _ => {
                if let (Some(id), None) = (&id, method) {
                    let resumed = client_requests.get(id).is_some_and(|m| m == "thread/resume");
                    if let (true, Some(thread)) = (resumed, frame.pointer("/result/thread")) {
                        let thread_id = thread.get("id").and_then(Value::as_str).unwrap_or_default();
                        let turns = thread.get("turns").and_then(Value::as_array).cloned().unwrap_or_default();
                        recorder.apply(SESSION, fold.apply_resume_turns(thread_id, &turns));
                    }
                } else {
                    if let (Some(id), Some(method)) = (&id, method) {
                        server_requests.insert(id.clone(), (method.to_owned(), frame["params"].clone()));
                    }
                    let mut deltas = Vec::new();
                    step_line(&mut fold, &frame.to_string(), &mut |event| {
                        if let ProviderEvent::Deltas { deltas: more, .. } = event {
                            deltas.extend(more);
                        }
                    });
                    recorder.apply(SESSION, deltas);
                }
            }
        }
        recorder.step(number + 1);
        let mut lost = fold.clone();
        recorder.abandon(number + 1, vec![(SESSION.to_owned(), lost.abandon_live_turn("golden: the agent process exited"))]);
    }
    recorder
}

#[test]
fn every_codex_fixture_folds_to_its_golden_session() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();
    for fixture in golden::fixtures(&root.join("../../fixtures/codex")) {
        let golden = golden::golden_path(&root.join("tests/golden"), &fixture);
        if let Some(failure) = record(&fixture).check(&golden) {
            failures.push(failure);
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
