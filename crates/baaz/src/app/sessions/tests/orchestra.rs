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

/// docs/85 14.3: a writer works in a worktree of its own (a branch off the
/// project, under Baaz's state dir), and a reviewer that follows it reads
/// that worktree in Read-only, whatever the lead's mode.
#[gpui::test]
fn a_writer_gets_its_own_worktree_and_its_reviewer_reads_it_read_only(cx: &mut gpui::TestAppContext) {
    let state = lane_state("orchestra-roles");
    let root = folder(&state.2, true);
    let (factory, _) = recording_factory_numbered(true);
    let (baaz, vc, lead) = session_with(cx, &root, factory);
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "write it", "provider": "codex"})));
    let writer_task = out["result"]["task"].as_str().unwrap().to_owned();
    vc.run_until_parked();
    let workspace_of = |vc: &mut gpui::VisualTestContext, task: &str| -> (String, aui_protocol::PermissionMode) {
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let worker = h.registry.orchestra.workers.iter().find(|(_, t)| t.as_str() == task).map(|(w, _)| w.clone()).expect("a worker");
            let rt = h.registry.find_runtime(h.active.as_ref(), &worker, cx).expect("live");
            let rt = rt.read(cx);
            (rt.workspace_path().to_owned(), rt.mode())
        })
    };
    let (folder_w, _) = workspace_of(vc, &writer_task);
    assert_ne!(folder_w, root.to_string_lossy(), "a writer never works in the project root");
    assert!(crate::app::Harness::in_worktree(&folder_w), "{folder_w} is one of Baaz's worktrees");
    let branch = super::checkpoints::git(std::path::Path::new(&folder_w), &["rev-parse", "--abbrev-ref", "HEAD"]);
    assert!(branch.contains("task-"), "its own branch: {branch}");
    // Mark the writer passed so its reviewer may follow it.
    let id = writer_task.clone();
    crate::db::call(crate::db::path(), move |db| {
        let mut t = crate::db::tasks::load(&db.conn, &id).unwrap().unwrap();
        t.status = baaz_core::tasks::Status::Passed;
        crate::db::tasks::save(&mut db.conn, &t).unwrap();
    });
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "review it", "provider": "codex", "role": "reviewer", "after": [writer_task]})));
    let reviewer_task = out["result"]["task"].as_str().unwrap().to_owned();
    vc.run_until_parked();
    let (folder_r, mode_r) = workspace_of(vc, &reviewer_task);
    assert_eq!(folder_r, folder_w, "the reviewer reads the writer's worktree");
    assert_eq!(mode_r, aui_protocol::PermissionMode::DenyUnmatched, "and only reads");
    // A provider with no Read-only mode cannot review.
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "review", "provider": "acp", "role": "reviewer"})));
    assert!(error_text(&out).starts_with("[provider_unavailable]"), "{out}");
    cleanup(&root, state);
}

fn worker_of(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, task: &str) -> String {
    vc.update(|_, cx| baaz.read(cx).registry.orchestra.workers.iter().find(|(_, t)| t.as_str() == task).map(|(w, _)| w.clone()).expect("a worker"))
}

fn status_of(task: &str) -> baaz_core::tasks::Task {
    let id = task.to_owned();
    crate::db::call(crate::db::path(), move |db| crate::db::tasks::load(&db.conn, &id).unwrap().unwrap()).unwrap()
}

/// Wait for the background verdict and its follow-ups to land.
fn settle(vc: &mut gpui::VisualTestContext) {
    for _ in 0..40 {
        vc.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    vc.run_until_parked();
}

fn commit_in(folder: &str, file: &str, text: &str) {
    let dir = std::path::Path::new(folder);
    std::fs::write(dir.join(file), text).unwrap();
    super::checkpoints::git(dir, &["add", "-A"]);
    super::checkpoints::git(dir, &["-c", "user.name=w", "-c", "user.email=w@example.invalid", "commit", "-qm", file]);
}

/// docs/85 14.4–14.6 on the scripted provider: a false "done" is caught
/// and gets one fix-up; a report cannot amend the criteria; a three-step
/// pipeline (write → review → import) runs its next steps without the lead
/// and wakes the lead exactly once, at the end, per criterion; an
/// escalation pauses its task and the person's answer resumes it.
#[gpui::test]
fn the_verdict_catches_a_false_done_and_a_pipeline_wakes_the_lead_once(cx: &mut gpui::TestAppContext) {
    let state = lane_state("orchestra-verdict");
    let root = folder(&state.2, true);
    // The project's own check: always passes once there is a commit.
    std::fs::create_dir_all(root.join(".baaz")).unwrap();
    std::fs::write(root.join(".baaz/project.toml"), "check = \"true\"\n").unwrap();
    super::checkpoints::git(&root, &["add", "-A"]);
    super::checkpoints::git(&root, &["commit", "-qm", "check"]);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, lead) = session_with(cx, &root, factory);
    let wakes = |sent: &super::undo::Sent| sent.lock().unwrap().iter().filter(|s| s.0.starts_with("From Baaz (not the user): your delegated tasks settled")).count();

    let delegate = |vc: &mut gpui::VisualTestContext, body: serde_json::Value| -> String {
        let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", body));
        assert_eq!(out["ok"], true, "{out}");
        out["result"]["task"].as_str().unwrap().to_owned()
    };
    let write = delegate(vc, serde_json::json!({"task": "Write 15 questions", "provider": "claude-code", "criteria": ["15 questions written", "the tests pass"]}));
    let review = delegate(vc, serde_json::json!({"task": "Review them", "provider": "codex", "role": "reviewer", "after": [write], "criteria": ["each question reviewed as correct"]}));
    let import = delegate(vc, serde_json::json!({"task": "Import the approved ones", "provider": "codex", "after": [review], "criteria": ["the import check passes"]}));
    settle(vc);
    assert_eq!(status_of(&review).status, baaz_core::tasks::Status::Waiting);
    let writer = worker_of(&baaz, vc, &write);

    // A report that amends the criteria is refused.
    let out = reply(&pane_call(&baaz, vc, &writer, &root, "report", serde_json::json!({"result": "done", "criteria": [{"criterion": "it mostly works", "met": true}]})));
    assert!(error_text(&out).starts_with("[criteria_frozen]"), "{out}");
    // A false done (nothing committed): caught, one fix-up turn.
    let out = reply(&pane_call(&baaz, vc, &writer, &root, "report", serde_json::json!({"result": "done"})));
    assert_eq!(out["ok"], true, "{out}");
    settle(vc);
    let t = status_of(&write);
    assert_eq!((t.status, t.fixups), (baaz_core::tasks::Status::Running, 1), "{:?}", t.verdict);
    assert!(sent.lock().unwrap().iter().any(|s| s.0.starts_with("Baaz checked your work and it is not done yet") && s.0.contains("there is no commit")));
    assert_eq!(wakes(&sent), 0, "the lead is not woken by a fix-up");
    // Committed: it passes, and the review starts by itself.
    commit_in(t.worktree.as_deref().expect("a worktree"), "questions.md", "1. What?\n");
    let out = reply(&pane_call(&baaz, vc, &writer, &root, "report", serde_json::json!({"result": "15 questions in questions.md", "criteria": [{"criterion": "1", "met": true, "evidence": "questions.md"}]})));
    assert_eq!(out["ok"], true, "{out}");
    settle(vc);
    let t = status_of(&write);
    assert_eq!(t.status, baaz_core::tasks::Status::Passed, "{:?}", t.verdict);
    assert_eq!(t.verdict.as_ref().unwrap()["criteria"][0]["state"], "not proven", "a count is not something the check proves");
    assert_eq!(t.verdict.as_ref().unwrap()["criteria"][1]["state"], "met");
    let r = status_of(&review);
    assert_eq!(r.status, baaz_core::tasks::Status::Running, "released without the lead");
    assert!(r.inputs.iter().any(|i| i.contains("15 questions in questions.md")), "{:?}", r.inputs);
    assert_eq!(wakes(&sent), 0);

    // An escalation pauses the review; the person's answer resumes it.
    let reviewer = worker_of(&baaz, vc, &review);
    let out = reply(&pane_call(&baaz, vc, &reviewer, &root, "escalate", serde_json::json!({"question": "Keep question 7?", "options": ["Keep", "Drop"]})));
    assert_eq!(out["ok"], true, "{out}");
    settle(vc);
    assert_eq!(status_of(&review).status, baaz_core::tasks::Status::NeedsYou);
    let lead_rt = vc.update(|_, cx| baaz.read(cx).registry.find_runtime(baaz.read(cx).active.as_ref(), &lead, cx).unwrap());
    let card = vc.update(|_, cx| lead_rt.read(cx).escalation_of_any()).expect("the lead's transcript shows the question");
    vc.update(|_, cx| lead_rt.update(cx, |rt, cx| rt.answer_escalation(&card, aui_protocol::Answer { selected: vec![1], other: None }, cx)));
    settle(vc);
    assert_eq!(status_of(&review).status, baaz_core::tasks::Status::Running);
    assert!(sent.lock().unwrap().iter().any(|s| s.0 == "The person answered your question: Drop"));
    let worker_rt = vc.update(|_, cx| baaz.read(cx).registry.find_runtime(baaz.read(cx).active.as_ref(), &reviewer, cx).unwrap());
    assert!(vc.update(|_, cx| worker_rt.read(cx).escalation_of_any()).is_none(), "the worker's card settled too");

    // The review reports; the import runs and passes; one wake.
    let out = reply(&pane_call(&baaz, vc, &reviewer, &root, "report", serde_json::json!({"result": "14 approved", "criteria": [{"criterion": "1", "met": true}]})));
    assert_eq!(out["ok"], true, "{out}");
    settle(vc);
    assert_eq!(status_of(&review).status, baaz_core::tasks::Status::Passed);
    let importer = worker_of(&baaz, vc, &import);
    let i = status_of(&import);
    commit_in(i.worktree.as_deref().expect("its own worktree"), "import.log", "14 imported\n");
    let out = reply(&pane_call(&baaz, vc, &importer, &root, "report", serde_json::json!({"result": "14 imported"})));
    assert_eq!(out["ok"], true, "{out}");
    settle(vc);
    assert_eq!(status_of(&import).status, baaz_core::tasks::Status::Passed);
    assert_eq!(wakes(&sent), 1, "the lead wakes once, when every task has settled");
    let wake = sent.lock().unwrap().iter().find(|s| s.0.starts_with("From Baaz")).unwrap().0.clone();
    assert!(wake.contains("Write 15 questions") && wake.contains("15 questions written — not proven") && wake.contains("the tests pass — met"), "{wake}");
    // A stale worker of a retried task is refused by generation.
    let id = write.clone();
    crate::db::call(crate::db::path(), move |db| {
        let mut t = crate::db::tasks::load(&db.conn, &id).unwrap().unwrap();
        t.status = baaz_core::tasks::Status::Failed;
        t.retry(9).unwrap();
        crate::db::tasks::save(&mut db.conn, &t).unwrap();
    });
    let out = reply(&pane_call(&baaz, vc, &writer, &root, "report", serde_json::json!({"result": "late"})));
    assert!(error_text(&out).starts_with("[stale_generation]"), "{out}");
    cleanup(&root, state);
}

/// docs/85 14.1: a restart mid-task resumes. The worker's session is gone
/// from the window (as after a quit); the reconcile on open resumes it from
/// the provider's own record in the background and tells it to continue;
/// a pending start replays; the person's view never moves.
#[gpui::test]
fn a_restart_mid_task_resumes_its_worker(cx: &mut gpui::TestAppContext) {
    let state = lane_state("orchestra-restart");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, lead) = session_with(cx, &root, factory);
    let out = reply(&pane_call(&baaz, vc, &lead, &root, "delegate", serde_json::json!({"task": "long job", "provider": "codex"})));
    let task = out["result"]["task"].as_str().unwrap().to_owned();
    vc.run_until_parked();
    let worker = worker_of(&baaz, vc, &task);
    // The app "quits": the worker's live session goes; the task row stays running.
    vc.update(|_, cx| baaz.update(cx, |h, _| {
        h.registry.forget_where(|id| id == worker);
        h.registry.orchestra.workers.clear();
    }));
    assert_eq!(status_of(&task).status, baaz_core::tasks::Status::Running);
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.reconcile_tasks_on_open(cx)));
    settle(vc);
    let seen = sent.lock().unwrap().clone();
    assert!(seen.iter().any(|s| s.0 == format!("resumed:{worker}")), "resumed from the provider's record: {seen:#?}");
    assert!(seen.iter().any(|s| s.0 == crate::orchestra::CONTINUE_TEXT), "told to continue, not the task again");
    assert_eq!(worker_of(&baaz, vc, &task), worker, "the same session, live again");
    let active = vc.update(|_, cx| baaz.read(cx).active.clone().unwrap().read(cx).rt(cx).session_id.clone());
    assert_eq!(active, lead);
    cleanup(&root, state);
}
