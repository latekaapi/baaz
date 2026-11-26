//! Undo through the window (docs/82 6.4): "Undo this turn" and "Revert
//! file" go through the confirm, land as files put back with a toast whose
//! Undo takes them back, leave a note under an undone turn, and tell the
//! provider with the next message — on the wire text, never the bubble.
//! The scripted stand-in cannot edit files, so the test writes them between
//! a turn's start and end. Git runs in temp repos only.

use super::checkpoints::{cleanup, folder, git, session_uuid, session_with, turn};
use super::*;
use crate::overlays::DialogAction;

/// What each `SubmitInput` carried: its text parts, then its display text.
pub(super) type Sent = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// The scripted double behind a recorder of every send.
pub(super) fn recording_factory() -> (crate::providers::Registry, Sent) {
    recording_factory_numbered(false)
}

/// [`recording_factory`], but every open after the first mints its own
/// session id (`s-scripted-2`, …): a lead and its workers are distinct
/// sessions (docs/85 Stage 14).
pub(super) fn recording_factory_numbered(numbered: bool) -> (crate::providers::Registry, Sent) {
    use provider::ProviderAdapter as _;
    struct Recording {
        inner: provider::scripted::ScriptedProvider,
        sent: Sent,
    }
    impl provider::ProviderAdapter for Recording {
        fn id(&self) -> provider::ProviderId {
            self.inner.id()
        }
        fn connect(&mut self, client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            self.inner.connect(client)
        }
        fn capabilities(&self) -> provider::CapabilitySet {
            self.inner.capabilities()
        }
        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            // A resume (docs/85 14.1's restart) echoes the session.
            if let provider::Command::ResumeSession { session_id, workspace, .. } = &command {
                self.sent.lock().expect("sent").push((format!("resumed:{session_id}"), workspace.clone().unwrap_or_default()));
                return Ok(provider::Ack::Session { session_id: session_id.clone(), title: None });
            }
            let steered = match &command {
                provider::Command::SteerInput { parts, .. } => Some(parts),
                _ => None,
            };
            if let Some(parts) = steered {
                let text: String = parts.iter().filter_map(|part| match part {
                    provider::SubmissionPart::Text(text) => Some(text.clone()),
                    provider::SubmissionPart::Image { .. } => None,
                }).collect();
                self.sent.lock().expect("sent").push((text, "(steered)".into()));
            }
            // A fork (docs/85 16.3) is recorded as `fork:<through turn>`,
            // beside the folder it runs in (docs/87 18.9; empty: the
            // source's).
            if let provider::Command::ForkSession { through_turn, workspace, .. } = &command {
                self.sent.lock().expect("sent").push((format!("fork:{}", through_turn.as_deref().unwrap_or("")), workspace.clone().unwrap_or_default()));
            }
            // A native goal (docs/85 15.1) is recorded as `goal:<words>`
            // (`goal:clear` when cleared, `goal-paused:` when paused).
            if let provider::Command::SetGoal { objective, paused, .. } = &command {
                let tag = if *paused { "goal-paused" } else { "goal" };
                self.sent.lock().expect("sent").push((format!("{tag}:{}", objective.as_deref().unwrap_or("clear")), String::new()));
            }
            if let provider::Command::SubmitInput { parts, display_text, .. } = &command {
                let text: String = parts
                    .iter()
                    .filter_map(|part| match part {
                        provider::SubmissionPart::Text(text) => Some(text.clone()),
                        provider::SubmissionPart::Image { .. } => None,
                    })
                    .collect();
                self.sent.lock().expect("sent").push((text, display_text.clone().unwrap_or_default()));
            }
            self.inner.dispatch(command)
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.inner.events()
        }
        fn shutdown(&mut self) {
            self.inner.shutdown();
        }
    }
    let sent: Sent = std::sync::Arc::default();
    let factory_sent = sent.clone();
    let opened = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory = crate::providers::Registry::test(move |_: ProviderId| {
        let n = opened.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let inner = if numbered && n > 1 { provider::scripted::ScriptedProvider::with_session_id(format!("s-scripted-{n}")) } else { provider::scripted::ScriptedProvider::new() };
        let mut adapter = Recording { inner, sent: factory_sent.clone() };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, sent)
}

fn runtime(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> gpui::Entity<SessionRuntime> {
    vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone())
}

/// The dialog up now: title, body, primary label, action, target.
fn dialog(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> Option<(String, String, &'static str, DialogAction, Option<String>)> {
    vc.update(|_, cx| {
        baaz.read(cx).overlays.read(cx).dialog().map(|d| (d.title.clone(), d.detail.clone(), d.primary, d.action, d.archive_target.clone()))
    })
}

/// Press the dialog's primary button (the confirm's own path).
fn confirm(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) {
    let (_, _, _, action, target) = dialog(baaz, vc).expect("a dialog is up");
    assert_eq!(action, DialogAction::UndoChanges);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.confirm_undo_dialog(target, cx)));
    vc.run_until_parked();
}

/// The newest toast's title, body and whether it offers Undo.
fn newest_toast(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> (String, String, bool) {
    vc.update(|_, cx| {
        let toast = baaz.read(cx).overlays.read(cx).toasts.last().cloned().expect("a toast");
        (toast.title.to_string(), toast.body.to_string(), !toast.actions.is_empty())
    })
}

/// The toast's Undo.
fn press_toast_undo(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) {
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.undo_newest(cx)));
    vc.run_until_parked();
}

/// Send `text`, then fold the stand-in's reply (the user turn, its own
/// turn, the finish) as the lane would, its checkpoints landed.
fn send(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, text: &str) {
    let runtime = runtime(baaz, vc);
    vc.update(|_, cx| runtime.update(cx, |rt, cx| rt.send_or_queue(text.to_owned(), cx)));
    vc.run_until_parked();
    vc.update(|_, cx| runtime.update(cx, |rt, cx| rt.test_drain_provider(cx)));
    vc.run_until_parked();
    let settled = vc.update(|_, cx| !runtime.read(cx).working() && runtime.read(cx).checkpoints_idle());
    assert!(settled, "the reply to {text:?} finished");
}

fn read(root: &std::path::Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).expect("read")
}

/// Undo this turn through the confirm: its files go back (a created one
/// removed), the person's later file stays, the turn shows its note and no
/// longer offers Undo, the toast offers Undo; the next send's wire text
/// carries the notice after the person's words while its bubble does not, and the send after it
/// carries none. The toast's Undo then puts the turn's files back, and —
/// the preface having gone — the next send says so.
#[gpui::test]
fn undo_this_turn_through_the_confirm_restores_its_files_and_tells_the_provider(cx: &mut gpui::TestAppContext) {
    let state = lane_state("undo-turn");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory();
    let (baaz, vc, id) = session_with(cx, &root, factory);
    turn(&baaz, vc, &id, "h-1", || {
        std::fs::write(root.join("a.txt"), "one\nTWO\n").expect("edit");
        std::fs::write(root.join("new.txt"), "agent\n").expect("create");
    });
    std::fs::write(root.join("mine.txt"), "mine\n").expect("the person's file, after the turn");
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| {
        assert!(rt.read(cx).turn_undoable("h-1"), "a finished turn with both checkpoints offers Undo");
        assert_eq!(rt.read(cx).last_undoable_turn().as_deref(), Some("h-1"));
        assert!(baaz.read(cx).undo_last_offered(cx), "the session menu offers Undo last turn");
    });

    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.ask_undo_turn("h-1", cx))).expect("asked");
    vc.run_until_parked();
    let (title, detail, primary, _, _) = dialog(&baaz, vc).expect("the confirm is up");
    assert_eq!((title.as_str(), primary), ("Undo this turn?", "Undo"));
    assert!(detail.contains("a.txt and new.txt") && detail.contains("turn 1"), "names the files and the turn: {detail}");
    assert_eq!(read(&root, "a.txt"), "one\nTWO\n", "nothing changes before the confirm");

    confirm(&baaz, vc);
    assert_eq!(read(&root, "a.txt"), "one\ntwo\n", "a.txt as before the turn");
    assert!(!root.join("new.txt").exists(), "the turn's new file is gone");
    assert_eq!(read(&root, "mine.txt"), "mine\n", "the person's file stays");
    assert_eq!(newest_toast(&baaz, vc), ("Turn undone".into(), "2 files restored".into(), true));
    vc.update(|_, cx| {
        let rt = rt.read(cx);
        assert_eq!(rt.undone_notes().get("h-1").map(String::as_str), Some("Turn undone · 2 files restored"), "the note under the turn");
        assert!(!rt.turn_undoable("h-1"), "an undone turn offers no second Undo");
    });
    let uuid = session_uuid(&id);
    assert!(!git(&root, &["for-each-ref", &format!("refs/baaz/{uuid}/undo/")]).is_empty(), "the safety snapshot is named");

    send(&baaz, vc, "carry on");
    send(&baaz, vc, "and again");
    let sends = sent.lock().expect("sent").clone();
    assert_eq!(sends.len(), 2);
    assert_eq!(
        sends[0].0,
        "carry on\n\n<baaz-notice>\nNote: before this message, the person undid your changes from an earlier turn (files: a.txt and new.txt). Those files are back to how they were before that turn.\n</baaz-notice>",
        "the notice rides the wire text, after the person's words (docs/85 10.5)"
    );
    assert_eq!(sends[0].1, "carry on", "the bubble is what the person typed");
    assert_eq!(sends[1].0, "and again", "once only");
    let bubbles = vc.update(|_, cx| {
        rt.read(cx).session().map(|s| s.turns.iter().filter_map(|t| match t {
            aui::protocol::Turn::User { text, .. } => Some(text.clone()),
            _ => None,
        }).collect::<Vec<_>>()).unwrap_or_default()
    });
    assert!(bubbles.iter().all(|text| !text.contains("Note:")), "no bubble shows the preface: {bubbles:?}");

    // The toast's Undo: the turn's files are back, and the provider hears.
    // (Two sends later the toast is still the newest Undo on the stack.)
    press_toast_undo(&baaz, vc);
    assert_eq!(read(&root, "a.txt"), "one\nTWO\n");
    assert_eq!(read(&root, "new.txt"), "agent\n", "the removed file comes back");
    assert_eq!(newest_toast(&baaz, vc).0, "Undo taken back");
    vc.update(|_, cx| assert!(rt.read(cx).undone_notes().is_empty(), "the note goes"));
    send(&baaz, vc, "third");
    let last = sent.lock().expect("sent").last().cloned().expect("a send");
    assert!(last.0.contains("<baaz-notice>\nNote: before this message, the person restored changes they had undone earlier (files: a.txt and new.txt)."), "{}", last.0);
    assert!(last.0.starts_with("third\n\n<baaz-notice>"), "the person's words first: {}", last.0);
    cleanup(&root, state);
}

/// Rule 13: no Undo while a turn runs in the session, nor on a turn
/// without checkpoints (a folder outside git); asking anyway changes
/// nothing.
#[gpui::test]
fn undo_is_absent_while_a_turn_runs_and_on_a_turn_without_checkpoints(cx: &mut gpui::TestAppContext) {
    let state = lane_state("undo-absent");
    let root = folder(&state.2, true);
    let (factory, _) = recording_factory();
    let (baaz, vc, id) = session_with(cx, &root, factory);
    turn(&baaz, vc, &id, "h-1", || std::fs::write(root.join("a.txt"), "changed\n").expect("edit"));
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| assert!(rt.read(cx).turn_undoable("h-1")));
    // A second turn starts and is still running.
    vc.update(|_, cx| {
        rt.update(cx, |rt, cx| rt.test_provider_event(provider::ProviderEvent::TurnRunning { session_id: id.clone(), turn_id: "h-2".into() }, cx))
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let r = rt.read(cx);
        assert!(r.undo_offered().is_none(), "no fold row offers Undo while a turn runs");
        assert!(r.last_undoable_turn().is_none());
        assert!(!baaz.read(cx).undo_last_offered(cx), "nor the session menu");
    });
    let refused = vc.update(|_, cx| rt.update(cx, |rt, cx| rt.ask_undo_turn("h-1", cx)));
    assert!(refused.is_err(), "asking anyway is refused");
    vc.run_until_parked();
    assert!(dialog(&baaz, vc).is_none(), "no confirm");
    assert_eq!(read(&root, "a.txt"), "changed\n");
    cleanup(&root, state);
}

/// A folder that is not a git repository: a finished turn has no
/// checkpoints, so nothing offers Undo or Revert.
#[gpui::test]
fn a_turn_without_checkpoints_offers_no_undo(cx: &mut gpui::TestAppContext) {
    let state = lane_state("undo-plain");
    let root = folder(&state.2, false);
    let (factory, _) = recording_factory();
    let (baaz, vc, id) = session_with(cx, &root, factory);
    turn(&baaz, vc, &id, "h-1", || std::fs::write(root.join("a.txt"), "changed\n").expect("edit"));
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| {
        let r = rt.read(cx);
        assert!(r.undo_offered().is_none() && !r.turn_undoable("h-1") && !r.undo_ready(), "no checkpoints, no Undo");
    });
    assert!(vc.update(|_, cx| rt.update(cx, |rt, cx| rt.ask_undo_turn("h-1", cx))).is_err());
    let refused = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.revert_file_from_changes(&root, "a.txt", None, cx)));
    assert!(refused.is_err(), "no Revert either");
    assert!(dialog(&baaz, vc).is_none());
    cleanup(&root, state);
}

/// Revert file from Changes: in All changes back to HEAD through the
/// confirm, the toast's Undo restoring the exact bytes (and cancelling the
/// preface that had not gone yet); in a turn's scope only that turn's part.
#[gpui::test]
fn revert_file_through_the_confirm_and_the_toasts_undo(cx: &mut gpui::TestAppContext) {
    let state = lane_state("undo-revert");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory();
    // Twelve committed lines, so the turn's edit and the person's are
    // far enough apart for the turn's own part to come out alone.
    let lines = |edits: &[(usize, &str)]| -> String {
        (1..=12).map(|n| edits.iter().find(|(at, _)| *at == n).map_or(format!("l{n}\n"), |(_, text)| format!("{text}\n"))).collect()
    };
    std::fs::write(root.join("a.txt"), lines(&[])).expect("twelve lines");
    git(&root, &["commit", "-qam", "twelve"]);
    let (baaz, vc, id) = session_with(cx, &root, factory);
    turn(&baaz, vc, &id, "h-1", || std::fs::write(root.join("a.txt"), lines(&[(2, "TURN")])).expect("turn edits"));
    std::fs::write(root.join("fresh.txt"), "untracked\n").expect("the person's new file");

    // All changes: the untracked file is removed, and Undo brings it back.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.revert_file_from_changes(&root, "fresh.txt", None, cx))).expect("asked");
    vc.run_until_parked();
    let (title, detail, primary, _, _) = dialog(&baaz, vc).expect("the confirm is up");
    assert_eq!((title.as_str(), primary), ("Revert fresh.txt?", "Revert"));
    assert!(detail.contains("last commit"), "{detail}");
    confirm(&baaz, vc);
    assert!(!root.join("fresh.txt").exists());
    assert_eq!(newest_toast(&baaz, vc), ("Reverted fresh.txt".into(), "Back to the last commit".into(), true));
    press_toast_undo(&baaz, vc);
    assert_eq!(read(&root, "fresh.txt"), "untracked\n", "Undo restores the exact bytes");
    // Taken back before any message: the provider never hears of it.
    send(&baaz, vc, "hello");
    assert_eq!(sent.lock().expect("sent").last().map(|s| s.0.clone()).as_deref(), Some("hello"));

    // A tracked change, in the turn's own scope: only what turn 1 did.
    std::fs::write(root.join("a.txt"), lines(&[(2, "TURN"), (11, "MINE")])).expect("the person edits a line after the turn");
    let uuid = session_uuid(&id);
    let pre = git(&root, &["rev-parse", &format!("refs/baaz/{uuid}/pre/h-1")]);
    let post = git(&root, &["rev-parse", &format!("refs/baaz/{uuid}/h-1")]);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.revert_file_from_changes(&root, "a.txt", Some(("h-1".into(), 1, pre, post)), cx)))
        .expect("asked");
    vc.run_until_parked();
    let (title, detail, ..) = dialog(&baaz, vc).expect("the confirm is up");
    assert_eq!(title, "Revert a.txt?");
    assert!(detail.contains("turn 1"), "{detail}");
    confirm(&baaz, vc);
    assert_eq!(read(&root, "a.txt"), lines(&[(11, "MINE")]), "turn 1's line goes, the person's stays");
    assert_eq!(newest_toast(&baaz, vc).1, "Back to before turn 1");
    press_toast_undo(&baaz, vc);
    assert_eq!(read(&root, "a.txt"), lines(&[(2, "TURN"), (11, "MINE")]));

    // A confirm closed any other way runs nothing: a stale target is refused.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.revert_file_from_changes(&root, "a.txt", None, cx))).expect("asked");
    vc.run_until_parked();
    let (.., target) = dialog(&baaz, vc).expect("up");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.close_dialog(cx)));
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.revert_file_from_changes(&root, "a.txt", None, cx))).expect("asked again");
    vc.run_until_parked();
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.confirm_undo_dialog(target, cx)));
    vc.run_until_parked();
    assert_eq!(read(&root, "a.txt"), lines(&[(2, "TURN"), (11, "MINE")]), "the first, dismissed confirm's number runs nothing");
    cleanup(&root, state);
}

/// One pane tool call through the harness's hook, as the service hands it
/// over (docs/85 13.3); the reply's channel.
pub(super) fn pane_call(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, session: &str, root: &std::path::Path, tool: &str, params: serde_json::Value) -> std::sync::mpsc::Receiver<String> {
    let (reply, rx) = std::sync::mpsc::channel();
    let call = crate::terminal::service::PaneCall { tool: tool.into(), session: session.into(), root: root.to_path_buf(), params, id: serde_json::json!(1), reply };
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.pane_tool_call(call, cx)));
    vc.run_until_parked();
    rx
}

pub(super) fn reply(rx: &std::sync::mpsc::Receiver<String>) -> serde_json::Value {
    let text = rx.recv_timeout(std::time::Duration::from_secs(10)).expect("the tool answered");
    serde_json::from_str(&text).expect("json")
}

/// docs/85 13.3: `changes_revert` goes through the person's confirm and
/// answers what happened — reverted (and the provider is told nothing more:
/// the agent asked), declined (Cancel / Escape), or one turn's part only;
/// it may ask while the agent's turn runs. `changes_comment` lands under the
/// file in Changes.
#[gpui::test]
fn the_agents_revert_waits_for_the_confirm_and_answers_what_happened(cx: &mut gpui::TestAppContext) {
    let state = lane_state("pane-revert");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory();
    let lines = |edits: &[(usize, &str)]| -> String {
        (1..=12).map(|n| edits.iter().find(|(at, _)| *at == n).map_or(format!("l{n}\n"), |(_, text)| format!("{text}\n"))).collect()
    };
    std::fs::write(root.join("a.txt"), lines(&[])).expect("twelve lines");
    git(&root, &["commit", "-qam", "twelve"]);
    let (baaz, vc, id) = session_with(cx, &root, factory);
    turn(&baaz, vc, &id, "h-1", || std::fs::write(root.join("a.txt"), lines(&[(2, "TURN")])).expect("turn edits"));
    std::fs::write(root.join("fresh.txt"), "untracked\n").expect("an untracked file");

    // Reverted: the confirm is the person's, the answer names the file.
    let rx = pane_call(&baaz, vc, &id, &root, "changes_revert", serde_json::json!({"path": "fresh.txt"}));
    let (title, ..) = dialog(&baaz, vc).expect("the person's confirm is up");
    assert_eq!(title, "Revert fresh.txt?");
    assert!(rx.try_recv().is_err(), "no answer before the person's");
    confirm(&baaz, vc);
    let out = reply(&rx);
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["reverted"], serde_json::json!(["fresh.txt"]));
    assert!(!root.join("fresh.txt").exists());
    send(&baaz, vc, "hello");
    assert_eq!(sent.lock().expect("sent").last().map(|s| s.0.clone()).as_deref(), Some("hello"), "the agent asked: no preface tells it again");

    // Turns are numbered as the pane numbers them: the session's agent turns.
    let rt = runtime(&baaz, vc);
    let n = vc.update(|_, cx| rt.read(cx).assistant_turn_ids().iter().position(|t| t == "h-1").expect("h-1 is an agent turn") + 1);
    let turn_n = format!("turn_{n}");
    // Declined: the dismissal (Cancel, Escape and the scrim all land here).
    let rx = pane_call(&baaz, vc, &id, &root, "changes_revert", serde_json::json!({"path": "a.txt", "scope": turn_n}));
    let (title, detail, .., target) = dialog(&baaz, vc).expect("up");
    assert_eq!(title, "Revert a.txt?");
    assert!(detail.contains(&format!("turn {n}")), "that turn's part: {detail}");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| {
        harness.close_dialog(cx);
        harness.decline_undo_dialog(target.as_deref(), cx);
    }));
    let out = reply(&rx);
    assert_eq!(out["result"]["declined"], true, "{out}");
    assert_eq!(read(&root, "a.txt"), lines(&[(2, "TURN")]), "nothing changed");

    // That turn, confirmed: only its line goes.
    std::fs::write(root.join("a.txt"), lines(&[(2, "TURN"), (11, "MINE")])).expect("the person's edit");
    let rx = pane_call(&baaz, vc, &id, &root, "changes_revert", serde_json::json!({"path": root.join("a.txt").to_string_lossy(), "scope": turn_n}));
    confirm(&baaz, vc);
    assert_eq!(reply(&rx)["ok"], true);
    assert_eq!(read(&root, "a.txt"), lines(&[(11, "MINE")]));

    // Refusals say why, at once.
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_revert", serde_json::json!({"path": "a.txt", "scope": "session"})));
    assert_eq!(out["ok"], false);
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_revert", serde_json::json!({"path": "a.txt", "scope": "turn_9"})));
    assert!(out["error"]["error"].as_str().unwrap_or_default().contains("agent turns"), "{out}");

    // A comment lands under the file in Changes, by the session's agent.
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_comment", serde_json::json!({"path": "a.txt", "line": 11, "text": "Why MINE?"})));
    assert_eq!(out["result"]["commented"], "a.txt:11");
    let held = vc.update(|_, cx| baaz.read(cx).right_pane.read(cx).cache.agent_comments.get(&root).and_then(|by| by.get("a.txt")).cloned());
    let held = held.expect("kept for Changes");
    assert_eq!((held[0].line, held[0].text.as_str()), (11, "Why MINE?"));
    assert!(!held[0].author.is_empty());
    assert_eq!(reply(&pane_call(&baaz, vc, &id, &root, "changes_comment", serde_json::json!({"path": "a.txt", "text": "x"})))["ok"], false, "a line is required");
    cleanup(&root, state);
}

/// docs/85 13.4: `changes_commit` commits through the commit box's flow
/// in a temp repository; `push: true` raises Baaz's own approval (the
/// session is not in Full access, rule 29) and pushes to a local bare
/// remote only on the person's yes; a dismissed approval pushes nothing.
#[gpui::test]
fn the_agents_commit_goes_through_the_box_and_its_push_waits_for_the_person(cx: &mut gpui::TestAppContext) {
    let state = lane_state("pane-commit");
    let root = folder(&state.2, true);
    let bare = root.with_file_name(format!("{}-remote.git", root.file_name().expect("named").to_string_lossy()));
    let _ = std::fs::remove_dir_all(&bare);
    git(&root, &["init", "-q", "--bare", "-b", "main", &bare.to_string_lossy()]);
    git(&root, &["remote", "add", "origin", &bare.to_string_lossy()]);
    let (factory, _) = recording_factory();
    let (baaz, vc, id) = session_with(cx, &root, factory);

    // A commit alone: the message given, the changed file in it.
    std::fs::write(root.join("a.txt"), "one\nTWO\n").expect("edit");
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_commit", serde_json::json!({"message": "Shout two"})));
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["result"]["committed"], serde_json::json!(["a.txt"]));
    assert_eq!(git(&root, &["log", "-1", "--format=%s"]), "Shout two");
    assert!(dialog(&baaz, vc).is_none(), "no push asked, no approval");

    // A commit and push: the approval is Baaz's own; Push pushes.
    std::fs::write(root.join("b.txt"), "b\n").expect("new file");
    let rx = pane_call(&baaz, vc, &id, &root, "changes_commit", serde_json::json!({"message": "Add b", "push": true}));
    let (title, _, primary, action, target) = dialog(&baaz, vc).expect("the push approval is up");
    assert_eq!((title.as_str(), primary, action), ("Push main?", "Push", DialogAction::AgentPush));
    assert!(git(&bare, &["log", "--oneline", "--all"]).is_empty(), "nothing pushed before the yes");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| {
        harness.close_dialog(cx);
        harness.confirm_agent_push(target.as_deref(), cx);
    }));
    vc.run_until_parked();
    let out = reply(&rx);
    assert_eq!(out["result"]["push"]["pushed"], "main", "{out}");
    assert_eq!(git(&bare, &["log", "-1", "--format=%s", "main"]), "Add b");

    // Dismissed: committed, not pushed.
    std::fs::write(root.join("c.txt"), "c\n").expect("another");
    let rx = pane_call(&baaz, vc, &id, &root, "changes_commit", serde_json::json!({"message": "Add c", "push": true}));
    let (.., target) = dialog(&baaz, vc).expect("up");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| {
        harness.close_dialog(cx);
        harness.decline_agent_push(target.as_deref(), cx);
    }));
    let out = reply(&rx);
    assert_eq!(out["result"]["push"]["pushed"], false, "{out}");
    assert_eq!(git(&root, &["log", "-1", "--format=%s"]), "Add c");
    assert_eq!(git(&bare, &["log", "-1", "--format=%s", "main"]), "Add b", "the remote is where it was");

    // Nothing to commit, and no message, are refusals in words.
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_commit", serde_json::json!({"message": "Empty"})));
    assert!(out["error"]["error"].as_str().unwrap_or_default().contains("nothing to commit"), "{out}");
    std::fs::write(root.join("d.txt"), "d\n").expect("d");
    let out = reply(&pane_call(&baaz, vc, &id, &root, "changes_commit", serde_json::json!({})));
    assert!(out["error"]["error"].as_str().unwrap_or_default().contains("message"), "{out}");
    let _ = std::fs::remove_dir_all(&bare);
    cleanup(&root, state);
}
