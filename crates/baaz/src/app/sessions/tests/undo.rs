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
type Sent = std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>;

/// The scripted double behind a recorder of every send.
fn recording_factory() -> (crate::providers::Registry, Sent) {
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
    let factory = crate::providers::Registry::test(move |_: ProviderId| {
        let mut adapter = Recording { inner: provider::scripted::ScriptedProvider::new(), sent: factory_sent.clone() };
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
