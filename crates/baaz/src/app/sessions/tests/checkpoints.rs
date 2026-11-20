//! Turn checkpoints through the window (docs/82 6.3): a live turn's start
//! and end write `refs/baaz/<uuid>/pre/<turn>` and `…/<turn>` in the
//! session's folder; the transcript's chip opens Changes on that turn's own
//! diff; a folder without git takes none and shows no scope row. The
//! scripted stand-in cannot edit files, so the test writes them between the
//! turn's start and end, as an agent would. Git runs in temp repos only.

use super::*;
use crate::right_scopes::ChangesScope;

pub(super) fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A folder beside the state dir (inside the temp dir, where tests may
/// take checkpoints), a git repository with one commit when `repo`.
pub(super) fn folder(state: &std::path::Path, repo: bool) -> std::path::PathBuf {
    let root = state.with_file_name(format!("{}-folder", state.file_name().expect("named").to_string_lossy()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("folder");
    let root = root.canonicalize().expect("canonical");
    std::fs::write(root.join("a.txt"), "one\ntwo\n").expect("a.txt");
    if repo {
        git(&root, &["init", "-q", "-b", "main"]);
        git(&root, &["config", "user.email", "t@example.com"]);
        git(&root, &["config", "user.name", "T"]);
        git(&root, &["add", "-A"]);
        git(&root, &["commit", "-q", "-m", "first"]);
    }
    root
}

/// The window over the scripted stand-in, `root` the current project, one
/// new session open in it. Returns the harness, the context and the
/// session's id.
fn session_in<'a>(
    cx: &'a mut gpui::TestAppContext,
    root: &std::path::Path,
) -> (gpui::Entity<Harness>, &'a mut gpui::VisualTestContext, String) {
    session_with(cx, root, scripted_factory())
}

/// [`session_in`] over `factory`.
pub(super) fn session_with<'a>(
    cx: &'a mut gpui::TestAppContext,
    root: &std::path::Path,
    factory: crate::providers::Registry,
) -> (gpui::Entity<Harness>, &'a mut gpui::VisualTestContext, String) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    cx.update(crate::app::bind_keys);
    let state = crate::store::support_dir();
    let mut args = lane_args(&state);
    args.login = crate::LoginSample::SignedIn;
    let (baaz, vc) =
        cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            adopt_for_muse(harness, root, cx);
            harness.provider_model.update(cx, |model, _| model.point_new_provider(ProviderId::Codex));
        })
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let id = vc.update(|_, cx| baaz.read(cx).active.clone().expect("a session is open").read(cx).rt(cx).session_id.clone());
    // The session's row, which names its refs, is written.
    crate::sessions::flush();
    (baaz, vc, id)
}

/// Run one turn by hand: it starts, `edit` runs (the agent's work), it
/// finishes; then every checkpoint job has landed.
pub(super) fn turn(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, id: &str, turn_id: &str, edit: impl FnOnce()) {
    let runtime = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone());
    vc.update(|_, cx| {
        runtime.update(cx, |rt, cx| {
            rt.test_provider_event(provider::ProviderEvent::TurnRunning { session_id: id.to_owned(), turn_id: turn_id.to_owned() }, cx)
        })
    });
    vc.run_until_parked();
    edit();
    vc.update(|_, cx| {
        runtime.update(cx, |rt, cx| {
            let assistant = aui::protocol::Turn::Assistant {
                id: turn_id.to_owned(),
                blocks: Vec::new(),
                meta: aui::protocol::TurnMeta::default(),
                timestamp: None,
            };
            rt.test_provider_event(
                provider::ProviderEvent::Deltas {
                    session_id: Some(id.to_owned()),
                    deltas: vec![
                        aui::protocol::Delta::TurnStarted { turn: assistant },
                        aui::protocol::Delta::TurnFinished { turn_id: turn_id.to_owned(), meta: aui::protocol::TurnMeta::default() },
                    ],
                },
                cx,
            )
        })
    });
    vc.run_until_parked();
    let idle = vc.update(|_, cx| runtime.read(cx).checkpoints_idle());
    assert!(idle, "every checkpoint job landed");
}

pub(super) fn session_uuid(id: &str) -> String {
    let id = id.to_owned();
    crate::db::call(crate::db::path_in(&crate::store::support_dir()), move |db| crate::db::sessions::read_row(&db.conn, "codex", &id).expect("read"))
        .flatten()
        .expect("the session has a row")
}

/// A turn's end writes its two refs; the chip on it opens Changes in its
/// own scope (Turn N) with its file; the second turn's chip shows a
/// different diff of the same file; the scope row offers This session and
/// Last turn, and the commit box stands only in All changes.
#[gpui::test]
fn a_turn_end_checkpoints_and_its_chip_opens_that_turns_own_diff(cx: &mut gpui::TestAppContext) {
    let state = lane_state("checkpoints-turns");
    let root = folder(&state.2, true);
    let (baaz, vc, id) = session_in(cx, &root);
    let file = root.join("a.txt");
    turn(&baaz, vc, &id, "a-1", || std::fs::write(&file, "one\ntwo\nthree\n").expect("turn 1 edits"));
    let uuid = session_uuid(&id);
    let pre = git(&root, &["rev-parse", &format!("refs/baaz/{uuid}/pre/a-1")]);
    let post = git(&root, &["rev-parse", &format!("refs/baaz/{uuid}/a-1")]);
    assert_ne!(pre, post, "the turn changed the tree");
    assert_eq!(git(&root, &["status", "--porcelain"]), "M a.txt", "the person's tree is as the agent left it");
    // The person edits between the turns; then turn 2 edits the same file.
    std::fs::write(root.join("mine.txt"), "mine\n").expect("the person's file");
    turn(&baaz, vc, &id, "a-2", || std::fs::write(&file, "ONE\ntwo\nthree\n").expect("turn 2 edits"));

    let pane = vc.update(|_, cx| baaz.read(cx).right_pane.clone());
    let shown = |vc: &mut gpui::VisualTestContext| {
        vc.update(|_, cx| {
            let pane = pane.read(cx);
            let scopes = &pane.cache.changes.scopes;
            let files: Vec<(String, u32, u32)> =
                scopes.scoped.as_ref().map(|list| list.files.iter().map(|f| (f.path.clone(), f.added, f.removed)).collect()).unwrap_or_default();
            (scopes.scope.clone(), files, pane.cache.changes_expanded_for(&root))
        })
    };
    let diff_lines = |vc: &mut gpui::VisualTestContext| {
        vc.update(|_, cx| {
            let diff = pane.read(cx).cache.file_diffs.get("a.txt").cloned().flatten().expect("a.txt's diff was read");
            diff.hunks.iter().flat_map(|hunk| hunk.lines.iter()).filter(|line| line.kind != aui::protocol::DiffKind::Context).map(|line| line.text.clone()).collect::<Vec<_>>()
        })
    };

    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_turn_changes("a-1", vec![file.to_string_lossy().into_owned()], cx)));
    vc.run_until_parked();
    let (scope, files, open) = shown(vc);
    assert_eq!(scope, ChangesScope::Turn("a-1".into()), "the chip routes to the turn's own scope");
    assert_eq!(files, vec![("a.txt".to_owned(), 1, 0)], "turn 1's diff: one line added, nothing of the person's");
    assert!(open.contains("a.txt"), "the turn's file opens as before");
    let one = diff_lines(vc);
    assert_eq!(one, vec!["three".to_owned()]);

    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_turn_changes("a-2", vec![file.to_string_lossy().into_owned()], cx)));
    vc.run_until_parked();
    let (scope, files, _) = shown(vc);
    assert_eq!(scope, ChangesScope::Turn("a-2".into()));
    assert_eq!(files, vec![("a.txt".to_owned(), 1, 1)], "mine.txt is the person's, not turn 2's");
    let two = diff_lines(vc);
    assert_eq!(two, vec!["one".to_owned(), "ONE".to_owned()]);
    assert_ne!(one, two, "two turns editing one file show two different diffs");

    // The row: All changes · This session · Last turn · Turn 2.
    let row = vc.update(|_, cx| pane.read(cx).cache.changes.scopes.row(Some(&id)).map(|(labels, _, active)| (labels, active)));
    let (labels, active) = row.expect("the scope row shows");
    assert_eq!(labels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), vec!["All changes", "This session", "Last turn", "Turn 2"]);
    assert_eq!(active, 3);
    // This session: from the first turn's start to now — the person's file too.
    let picked = vc.update(|_, cx| pane.update(cx, |pane, cx| pane.set_scope(&root, ChangesScope::Session, cx)));
    picked.expect("This session answers");
    vc.run_until_parked();
    let (_, files, _) = shown(vc);
    assert_eq!(files.iter().map(|(path, ..)| path.as_str()).collect::<Vec<_>>(), vec!["a.txt", "mine.txt"]);
    cleanup(&root, state);
}

/// A folder that is not a git repository takes no checkpoint, raises
/// nothing, and the Changes screen offers no scope row; the chip keeps
/// today's behaviour.
#[gpui::test]
fn a_session_outside_git_takes_no_checkpoint_and_shows_no_scope_row(cx: &mut gpui::TestAppContext) {
    let state = lane_state("checkpoints-plain");
    let root = folder(&state.2, false);
    let (baaz, vc, id) = session_in(cx, &root);
    let file = root.join("a.txt");
    turn(&baaz, vc, &id, "a-1", || std::fs::write(&file, "changed\n").expect("edit"));
    assert!(!root.join(".git").exists(), "nothing made a repository");
    let banner = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).rt(cx).banner_text().map(str::to_owned));
    assert_eq!(banner, None, "no error reaches the session");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_turn_changes("a-1", vec![file.to_string_lossy().into_owned()], cx)));
    vc.run_until_parked();
    let pane = vc.update(|_, cx| baaz.read(cx).right_pane.clone());
    vc.update(|_, cx| {
        let scopes = &pane.read(cx).cache.changes.scopes;
        assert_eq!(scopes.scope, ChangesScope::All, "the chip stays in All changes");
        assert!(scopes.row(Some(&id)).is_none(), "no scope row without checkpoints");
    });
    let refused = vc.update(|_, cx| pane.update(cx, |pane, cx| pane.set_scope(&root, ChangesScope::Last, cx)));
    assert!(refused.is_err(), "no scope to pick");
    cleanup(&root, state);
}

pub(super) fn cleanup(root: &std::path::Path, state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf)) {
    crate::db::flush();
    crate::db::close(crate::db::path_in(&state.2));
    let _ = std::fs::remove_dir_all(root);
    lane_restore(state);
}
