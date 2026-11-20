//! The new-session row (docs/82 6.6) through the window: the folder control
//! and the "Own worktree" toggle on an unsent draft and on nothing else;
//! a folder pick moves the draft (text carried, project changed, the old
//! lane hung up); "Choose a folder…" adopts the folder first; the toggle
//! moves the draft into a worktree and back. Git runs in temp repos only;
//! the lanes are the scripted double behind a recorder.

use super::worktree::{active, cleanup, git, recording_factory, recording_factory_counting, repo, send, window};
use super::*;
use crate::session::pickers::{FolderPick, NewRow, CHOOSE_FOLDER_ROW};

/// The open session's new-session row, as its composer draws it.
fn row(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> Option<NewRow> {
    vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).rt(cx).new_row().cloned())
}

fn runtime(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> gpui::Entity<SessionRuntime> {
    vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone())
}

/// A second repository beside the first, adopted (not current).
fn second_repo(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, state: &std::path::Path) -> (std::path::PathBuf, String) {
    let root = repo(&state.with_file_name(format!("{}-two", state.file_name().expect("named").to_string_lossy())));
    let project = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.session_list.update(cx, |list, _| list.projects.add(&root).id.clone())));
    (root, project)
}

/// A fresh draft shows the row — the project's name, the toggle off (the
/// project's default) — and a session that sent shows neither control.
#[gpui::test]
fn the_row_shows_on_an_unsent_draft_and_goes_after_a_send(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-shows");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let name = vc.update(|_, cx| baaz.read(cx).session_list.read(cx).projects.find(&project).expect("adopted").name.clone());
    assert_eq!(row(&baaz, vc), Some(NewRow { folder: name, worktree: Some(false) }), "folder ▾ and Own worktree, off");
    send(&baaz, vc, "hello");
    assert_eq!(row(&baaz, vc), None, "a session with turns shows neither");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("open");
            assert!(harness.open_folder_menu(&view, cx).is_err(), "no folder menu after a send");
        })
    });
    cleanup(&root, state);
}

/// The toggle's first state is the project's default.
#[gpui::test]
fn the_toggle_starts_at_the_projects_default(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-default");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.set_worktree_default(&project, true, cx))).expect("default on");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (_, folder) = active(&baaz, vc);
    assert!(Harness::in_worktree(&folder));
    assert_eq!(row(&baaz, vc).and_then(|row| row.worktree), Some(true), "on, as the project says");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// The toggle moves the draft into a worktree (its lane reopens there) and
/// back to the root, where the unused worktree is removed; while the
/// draft moves the row says so and a second flip is refused.
#[gpui::test]
fn the_toggle_moves_the_draft_into_a_worktree_and_back(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-toggle");
    let root = repo(&state.2);
    let (factory, seen) = recording_factory();
    let (baaz, vc, _project) = window(cx, &state.2, &root, factory);
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_toggle_worktree(rt.clone(), cx)));
    let moving = vc.update(|_, cx| rt.read(cx).is_moving());
    assert!(moving, "the old draft shows the move in flight");
    let twice = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_draft_worktree(rt.clone(), true, cx)));
    assert!(twice.is_err(), "a second flip while it moves is refused");
    vc.run_until_parked();
    let (_, worktree) = active(&baaz, vc);
    assert!(Harness::in_worktree(&worktree), "in a worktree: {worktree}");
    assert_eq!(row(&baaz, vc).and_then(|row| row.worktree), Some(true), "the toggle reads on");
    {
        let seen = seen.lock().expect("seen");
        assert_eq!(seen.get(seen.len() - 2), Some(&worktree), "its lane reopened there");
    }
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_toggle_worktree(rt, cx)));
    vc.run_until_parked();
    let (_, back) = active(&baaz, vc);
    assert_eq!(back, root.to_string_lossy(), "back in the project root");
    assert_eq!(row(&baaz, vc).and_then(|row| row.worktree), Some(false), "the toggle reads off");
    assert!(!std::path::Path::new(&worktree).exists(), "the unused worktree is removed");
    assert_eq!(git(&root, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1);
    cleanup(&root, state);
}

/// The folder picker lists the projects, the draft's own first, then
/// "Choose a folder…"; picking another project moves the draft there
/// through the replace: the typed text comes along, the session's
/// project is the pick, and the old draft's lane is gone.
#[gpui::test]
fn picking_another_project_moves_the_draft(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-move");
    let root = repo(&state.2);
    let (factory, seen, shutdowns) = recording_factory_counting();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let (other_root, other) = second_repo(&baaz, vc, &state.2);
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (old_id, _) = active(&baaz, vc);
    let hung_up = shutdowns.load(std::sync::atomic::Ordering::Relaxed);
    vc.update(|window, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        view.update(cx, |view, cx| view.set_draft("carry me over".into(), window, cx));
    });
    // The menu, as the control opens it.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("open");
            harness.open_folder_menu(&view, cx).expect("the menu opens");
        })
    });
    let ids = vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        let picker = view.read(cx).folder_picker(cx).expect("the folder picker is open");
        picker.read(cx).delegate.rows.iter().map(|row| row.id.to_string()).collect::<Vec<_>>()
    });
    assert_eq!(ids.first(), Some(&format!("p:{project}")), "the draft's own project first: {ids:?}");
    assert!(ids.contains(&format!("p:{other}")), "the other projects: {ids:?}");
    assert_eq!(ids.last().map(String::as_str), Some(CHOOSE_FOLDER_ROW), "then Choose a folder: {ids:?}");
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_folder_pick(rt.clone(), &FolderPick::Project(other.clone()), cx)));
    vc.run_until_parked();
    let (new_id, folder) = active(&baaz, vc);
    assert_ne!(new_id, old_id, "a new draft");
    assert_eq!(folder, other_root.to_string_lossy(), "it runs in the picked project");
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("open");
        assert_eq!(view.read(cx).draft_text(cx), "carry me over", "the typed text came along");
        assert_eq!(harness.current_project_id().as_deref(), Some(other.as_str()), "the picked project is current");
        assert_eq!(harness.open_state.draft_project(&new_id), Some(other.as_str()), "the new draft is the pick's");
        assert_eq!(harness.open_state.draft_project(&old_id), None, "the old project no longer names the old draft");
        assert!(harness.find_view(&old_id, cx).is_none(), "the old draft's view is gone");
        assert!(!harness.session_list.read(cx).provider_sessions.contains_key(&old_id), "and its record");
    });
    assert!(shutdowns.load(std::sync::atomic::Ordering::Relaxed) > hung_up, "the old draft's lane is hung up");
    assert_eq!(seen.lock().expect("seen").iter().step_by(2).cloned().collect::<Vec<_>>(),
        vec![root.to_string_lossy().into_owned(), other_root.to_string_lossy().into_owned()], "two opens: the root, then the pick");
    let name = vc.update(|_, cx| baaz.read(cx).session_list.read(cx).projects.find(&other).expect("adopted").name.clone());
    assert_eq!(row(&baaz, vc).map(|row| row.folder), Some(name), "the control names the pick");
    let again = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_draft_to_project(runtime_of(harness, cx), &other, cx)));
    assert!(again.is_err(), "already there: refused");
    let _ = std::fs::remove_dir_all(&other_root);
    cleanup(&root, state);
}

fn runtime_of(harness: &Harness, cx: &gpui::App) -> gpui::Entity<SessionRuntime> {
    harness.active.clone().expect("open").read(cx).runtime().clone()
}

/// "Choose a folder…" (the panel's answer, through its seam): the folder
/// becomes a project, as Add project does, and the draft moves there. A
/// folder outside git offers no worktree.
#[gpui::test]
fn choosing_a_folder_adopts_it_and_moves_the_draft(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-choose");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, _project) = window(cx, &state.2, &root, factory);
    let plain = state.2.with_file_name(format!("{}-plain", state.2.file_name().expect("named").to_string_lossy()));
    let _ = std::fs::remove_dir_all(&plain);
    std::fs::create_dir_all(&plain).expect("plain");
    let plain = plain.canonicalize().expect("canonical");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let rt = runtime(&baaz, vc);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_draft_to_folder(rt, &plain, cx))).expect("moves");
    vc.run_until_parked();
    let (_, folder) = active(&baaz, vc);
    assert_eq!(folder, plain.to_string_lossy(), "the draft runs in the chosen folder");
    let adopted = vc.update(|_, cx| baaz.read(cx).session_list.read(cx).projects.projects.iter().any(|project| project.root == plain));
    assert!(adopted, "the folder is a project now");
    let name = plain.file_name().expect("named").to_string_lossy().into_owned();
    assert_eq!(row(&baaz, vc), Some(NewRow { folder: name, worktree: None }), "no toggle outside git");
    let _ = std::fs::remove_dir_all(&plain);
    cleanup(&root, state);
}

/// A draft with no lane (Muse's local draft, the scripted chrome's
/// stand-in) shows the folder control but no toggle: it has no lane to
/// reopen in a worktree.
#[gpui::test]
fn a_local_draft_offers_no_worktree(cx: &mut gpui::TestAppContext) {
    let state = lane_state("new-row-local");
    let root = repo(&state.2);
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    cx.update(crate::app::bind_keys);
    let mut args = lane_args(&state.2);
    args.login = crate::LoginSample::SignedIn;
    let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
    vc.run_until_parked();
    vc.update(|_, cx| baaz.update(cx, |harness, cx| adopt_for_muse(harness, &root, cx)));
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (id, _) = active(&baaz, vc);
    assert!(super::super::open::is_local_draft(&id), "a local draft: {id}");
    let row = row(&baaz, vc).expect("the row shows");
    assert_eq!(row.worktree, None, "no toggle without a lane");
    let rt = runtime(&baaz, vc);
    let refused = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_draft_worktree(rt, true, cx)));
    assert!(refused.is_err());
    cleanup(&root, state);
}
