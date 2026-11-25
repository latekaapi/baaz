//! Orchestration's lead tools over the scripted provider (docs/85 14.2):
//! a delegate opens its worker in the background and sends it the task, the
//! status reads it back, a stop stops it, and every refusal carries its
//! typed code. Git runs in temp repos only; no real CLI is reached.

use super::checkpoints::{cleanup, folder, session_with};
use super::undo::{pane_call, recording_factory_numbered, reply};
use super::*;

fn error_text(out: &serde_json::Value) -> String {
    assert_eq!(out["ok"], false, "refused: {out}");
    out["error"]["error"].as_str().unwrap_or_default().to_owned()
}

#[gpui::test]
fn a_delegate_starts_its_worker_in_the_background_and_the_limits_refuse_by_code(cx: &mut gpui::TestAppContext) {
    let state = lane_state("orchestra");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, lead) = session_with(cx, &root, factory);

    let out = reply(&pane_call(&baaz, vc, &lead, &root, "agents_available", serde_json::json!({})));
    assert!(out["result"]["agents"].as_array().unwrap().iter().any(|a| a["provider"] == "codex"), "{out}");
    assert_eq!(out["result"]["limits"]["writers"], 4);

    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({
        "task": "Write hello.txt saying hi", "provider": "codex", "role": "writer", "criteria": ["the tests pass", "the greeting reads well to you"]
    })));
    assert_eq!(out["ok"], true, "{out}");
    let task = out["result"]["task"].as_str().unwrap().to_owned();
    vc.run_until_parked();
    // The person still looks at the lead; the worker is parked live.
    let (active, workers) = vc.update(|_, cx| {
        let h = baaz.read(cx);
        (h.active.clone().unwrap().read(cx).rt(cx).session_id.clone(), h.registry.orchestra.workers.clone())
    });
    assert_eq!(active, lead, "a delegate never moves the person");
    let (worker, of) = workers.iter().next().map(|(w, t)| (w.clone(), t.clone())).expect("a worker was started");
    assert_eq!(of, task);
    assert_ne!(worker, lead, "the worker is a session of its own");
    let prompt = sent.lock().unwrap().iter().map(|s| s.0.clone()).find(|t| t.starts_with("Write hello.txt")).expect("the worker got its task");
    assert!(prompt.contains("1. the tests pass") && prompt.contains("call the `report` tool"), "{prompt}");

    let out = reply(&pane_call(&baaz, vc, &lead, &root, "task_status", serde_json::json!({})));
    let row = &out["result"]["tasks"][0];
    assert_eq!((row["status"].as_str(), row["session"].as_str()), (Some("running"), Some(worker.as_str())), "{out}");

    // Typed refusals.
    let code = |out: serde_json::Value| error_text(&out);
    assert!(code(reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "x", "provider": "bogus"})))).starts_with("[bad_argument]"));
    assert!(code(reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "x", "provider": "codex", "after": ["task-nope"]})))).starts_with("[not_found]"));
    assert!(code(reply(&pane_call(&baaz, vc, "someone-else", &root, "task_stop", serde_json::json!({"task": task})))).starts_with("[not_yours]"));
    for n in 0..3 {
        let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": format!("writer {n}"), "provider": "codex"})));
        assert_eq!(out["ok"], true, "writer {n}: {out}");
        vc.run_until_parked();
    }
    assert!(code(reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "a fifth", "provider": "codex"})))).starts_with("[too_many_writers]"));
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "read it", "provider": "codex", "role": "reviewer"})));
    assert_eq!(out["ok"], true, "a reviewer is not a writer: {out}");
    vc.run_until_parked();

    // A worker delegating sits at depth 2; its worker cannot go deeper.
    let out = reply(&pane_call(&baaz, vc, &worker, &root, "delegate", serde_json::json!({"task": "help", "provider": "codex", "role": "helper"})));
    assert_eq!(out["ok"], true, "{out}");
    vc.run_until_parked();
    let workers = vc.update(|_, cx| baaz.read(cx).registry.orchestra.workers.len());
    assert_eq!(workers, 6, "the first writer, three more, a reviewer and the worker's helper");
    let out = reply(&pane_call(&baaz, vc, &worker, &root, "task_status", serde_json::json!({})));
    let helper = out["result"]["tasks"][0]["session"].as_str().unwrap().to_owned();
    let out = reply(&pane_call(&baaz, vc, &helper, &root, "delegate", serde_json::json!({"task": "deeper", "provider": "codex", "role": "helper"})));
    assert!(error_text(&out).starts_with("[too_deep]"), "{out}");

    // task_send and session_send reach the worker as an agent's words.
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "task_send", serde_json::json!({"task": task, "text": "Also add a newline."})));
    assert_eq!(out["ok"], true, "{out}");
    vc.run_until_parked();
    let seen = sent.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.0.starts_with("From session") && s.0.contains("(an agent, not the user)") && s.0.ends_with("Also add a newline.")), "{out} {seen:#?}");
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "session_send", serde_json::json!({"session": "s-nowhere", "text": "hi"})));
    assert!(error_text(&out).starts_with("[not_found]"));
    // session_watch wraps an open session in a task of the caller's.
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "session_watch", serde_json::json!({"sessions": [helper]})));
    assert_eq!(out["result"]["watching"].as_array().unwrap().len(), 1, "{out}");
    // task_wait answers within its seconds.
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "task_wait", serde_json::json!({"task": task, "seconds": 1})));
    assert_eq!(out["result"]["status"], "running");

    let out = reply(&pane_call(&baaz, vc, &lead, &root, "task_stop", serde_json::json!({"task": task})));
    assert_eq!(out["result"]["status"], "stopped", "{out}");
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "task_status", serde_json::json!({"task": task})));
    assert_eq!(out["result"]["tasks"][0]["status"], "stopped");
    cleanup(&root, state);
}
