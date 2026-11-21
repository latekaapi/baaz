//! A worktree per session (docs/82 6.1) through the window: a new session
//! with the project's default on opens its lane in a worktree (on a branch
//! with the Settings prefix) and edits only there; `worktree:on` / `worktree:off` move an unsent draft in and
//! out, and the worktree it leaves unused goes. Git runs in temp repos only;
//! the lanes are the scripted double behind a recorder.

use super::*;

/// What each `OpenSession` the recorder saw carried, in order: its
/// folder, then its env as `KEY=value` words (two entries per open).
pub(super) type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The scripted double, one distinct session per open (`s-wt-1`, …),
/// recording each `OpenSession`'s folder.
pub(super) fn recording_factory() -> (crate::providers::Registry, Seen) {
    let (factory, seen, _) = recording_factory_counting();
    (factory, seen)
}

/// [`recording_factory`], also counting the lanes hung up.
pub(super) fn recording_factory_counting() -> (crate::providers::Registry, Seen, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use provider::ProviderAdapter as _;
    struct Recording {
        inner: provider::scripted::ScriptedProvider,
        seen: Seen,
        shutdowns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
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
            if let provider::Command::OpenSession { workspace, env, .. } = &command {
                self.seen.lock().expect("seen").push(workspace.clone().unwrap_or_default());
                let env: Vec<String> = env.iter().map(|(key, value)| format!("{key}={value}")).collect();
                self.seen.lock().expect("seen").push(env.join(" "));
            }
            if let provider::Command::ResumeSession { workspace, session_id, .. } = &command {
                self.seen.lock().expect("seen").push(format!("resume {session_id} in {}", workspace.clone().unwrap_or_default()));
            }
            self.inner.dispatch(command)
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.inner.events()
        }
        fn shutdown(&mut self) {
            self.shutdowns.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            self.inner.shutdown();
        }
    }
    let seen: Seen = std::sync::Arc::default();
    let shutdowns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory_shutdowns = shutdowns.clone();
    let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory_seen = seen.clone();
    let factory = crate::providers::Registry::test(move |_: ProviderId| {
        let n = opens.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut adapter = Recording {
            inner: provider::scripted::ScriptedProvider::with_session_id(format!("s-wt-{n}")),
            seen: factory_seen.clone(),
            shutdowns: factory_shutdowns.clone(),
        };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, seen, shutdowns)
}

pub(super) fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A project repository beside the state dir (never inside it), one commit.
pub(super) fn repo(state: &std::path::Path) -> std::path::PathBuf {
    let root = state.with_file_name(format!("{}-repo", state.file_name().expect("named").to_string_lossy()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("repo dir");
    let root = root.canonicalize().expect("canonical");
    git(&root, &["init", "-q", "-b", "main"]);
    git(&root, &["config", "user.email", "t@example.com"]);
    git(&root, &["config", "user.name", "T"]);
    std::fs::write(root.join("a.txt"), "one\n").expect("a.txt");
    git(&root, &["add", "-A"]);
    git(&root, &["commit", "-q", "-m", "first"]);
    root
}

/// The window on a Codex default over `factory`, `root` adopted as the
/// current project. Returns the harness, the context and the project id.
pub(super) fn window<'a>(
    cx: &'a mut gpui::TestAppContext,
    state: &std::path::Path,
    root: &std::path::Path,
    factory: crate::providers::Registry,
) -> (gpui::Entity<Harness>, &'a mut gpui::VisualTestContext, String) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    cx.update(crate::app::bind_keys);
    let mut args = lane_args(state);
    args.login = crate::LoginSample::SignedIn;
    let (baaz, vc) =
        cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
    vc.run_until_parked();
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            let project = adopt_for_muse(harness, root, cx);
            harness.provider_model.update(cx, |model, _| model.point_new_provider(ProviderId::Codex));
            project
        })
    });
    (baaz, vc, project)
}

/// The active view's session id and folder.
pub(super) fn active(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> (String, String) {
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("a session is open");
        let rt = view.read(cx).rt(cx);
        (rt.session_id.clone(), rt.workspace_path().to_owned())
    })
}

pub(super) fn cleanup(root: &std::path::Path, state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf)) {
    crate::db::flush();
    crate::db::close(crate::db::path_in(&state.2));
    let _ = std::fs::remove_dir_all(root);
    lane_restore(state);
}

/// The scripted session that edits only its worktree: with the project's
/// default on, "New session" makes a worktree under the state dir and the
/// lane opens there — `OpenSession` carries the worktree, the view, its
/// record and the terminal relay all name it — and a file written in the
/// session's folder lands in the worktree while the project root stays
/// clean. The session row is tied to the worktree row.
#[gpui::test]
fn a_session_with_the_worktree_default_on_runs_and_writes_only_in_its_worktree(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-default");
    let root = repo(&state.2);
    let (factory, seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.set_worktree_default(&project, true, cx)))
        .expect("the default turns on");
    // Settings › Git & worktrees › Worktrees: what the prefix field stores.
    vc.update(|_, cx| crate::settings::set_worktree_branch_prefix(" team x/ ", cx));
    vc.update(|_, cx| assert_eq!(crate::layout::Layout::get(cx).worktree_branch_prefix(), "team-x/"));
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (id, folder) = active(&baaz, vc);
    let folder = std::path::PathBuf::from(folder);
    assert!(
        folder.starts_with(baaz_core::worktrees::worktrees_root(&state.2)),
        "the session's folder is a worktree under the state dir: {}",
        folder.display()
    );
    let port = baaz_core::worktrees::by_path(&state.2, &folder).expect("its row").port;
    assert_eq!(
        *seen.lock().expect("seen"),
        vec![
            folder.to_string_lossy().into_owned(),
            format!(
                "BAAZ_WORKTREE_PATH={} BAAZ_PROJECT_ROOT={} BAAZ_PORT={port} BAAZ_PORT_COUNT=10",
                folder.display(),
                root.display()
            ),
        ],
        "the lane opened there, once, its child given the worktree's variables"
    );
    assert_eq!(
        git(&folder, &["branch", "--show-current"]),
        format!("team-x/{}", folder.file_name().expect("named").to_string_lossy()),
        "on a new branch with the Settings prefix"
    );
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let record = harness.session_list.read(cx).provider_sessions.get(&id).cloned().expect("the landing wrote a record");
        assert_eq!(record.workspace.as_deref(), Some(folder.to_string_lossy().as_ref()), "the record's folder is the worktree");
        assert_eq!(harness.session_list.read(cx).overrides.get(&id).and_then(|meta| meta.project.clone()), Some(project.clone()), "its project stays the original");
        assert_eq!(harness.terminal_service.session_root(&id), Some(folder.clone()), "its terminal runs there");
    });
    // What a terminal tab of that root starts with (`open_with_cwd`).
    assert!(
        baaz_core::worktrees::cached_env_for_folder(&folder).contains(&("BAAZ_PORT".to_owned(), port.to_string())),
        "the terminal's shell gets the worktree's variables"
    );
    // An edit through the session's folder lands in the worktree only.
    std::fs::write(folder.join("edited.txt"), "agent\n").expect("write in the session's folder");
    assert_eq!(git(&folder, &["status", "--porcelain"]), "?? edited.txt");
    assert_eq!(git(&root, &["status", "--porcelain"]), "", "the project root is untouched");
    assert!(!root.join("edited.txt").exists());
    crate::db::flush();
    let linked = crate::db::call(crate::db::path_in(&state.2), move |db| {
        crate::db::worktrees::by_session(&db.conn, "codex", &id).expect("read")
    })
    .flatten()
    .expect("the session row names its worktree");
    assert_eq!(linked.path, folder);
    assert_eq!(linked.project_root, root);
    assert_eq!(linked.project_id.as_deref(), Some(project.as_str()));
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// `worktree:on` moves an unsent draft into a new worktree through the
/// switch's replace (the lane reopens there); a provider switch keeps that
/// worktree; `worktree:off` moves it back to the project root, and the
/// worktree it left — clean — is removed with its row. Each verb fails when
/// it would do nothing.
#[gpui::test]
fn worktree_on_and_off_move_an_unsent_draft_and_the_unused_worktree_goes(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-toggle");
    let root = repo(&state.2);
    let (factory, seen) = recording_factory();
    let (baaz, vc, _project) = window(cx, &state.2, &root, factory);
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (_, folder) = active(&baaz, vc);
    assert_eq!(folder, root.to_string_lossy(), "the default is off: the project root");
    let refused = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.step_worktree("off", cx)));
    assert!(refused.is_err(), "already in the root: the verb did nothing and says so");

    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.step_worktree("on", cx))).expect("on");
    vc.run_until_parked();
    let (first_id, worktree) = active(&baaz, vc);
    assert!(Harness::in_worktree(&worktree), "the draft now runs in a worktree: {worktree}");
    {
        let seen = seen.lock().expect("seen");
        assert_eq!(seen.get(seen.len() - 2), Some(&worktree), "its lane reopened there");
        assert!(seen.last().is_some_and(|env| env.contains("BAAZ_PORT=")), "with the worktree's variables");
    }
    let again = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.step_worktree("on", cx)));
    assert!(again.is_err(), "already in a worktree");

    // A provider switch of that draft keeps its folder.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let runtime = harness.active.clone().expect("draft").read(cx).runtime().clone();
            harness.on_switch_provider(runtime, &ProviderId::ClaudeCode, cx);
        })
    });
    vc.run_until_parked();
    let (switched_id, switched) = active(&baaz, vc);
    assert_ne!(switched_id, first_id);
    assert_eq!(switched, worktree, "the switch reopened in the same worktree");
    assert!(std::path::Path::new(&worktree).exists(), "and did not remove it");

    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.step_worktree("off", cx))).expect("off");
    vc.run_until_parked();
    let (_, back) = active(&baaz, vc);
    assert_eq!(back, root.to_string_lossy(), "back in the project root");
    assert!(!std::path::Path::new(&worktree).exists(), "the clean, unused worktree is removed");
    assert_eq!(git(&root, &["worktree", "list", "--porcelain"]).matches("worktree ").count(), 1, "git lists only the main tree");
    crate::db::flush();
    let rows = crate::db::call(crate::db::path_in(&state.2), |db| crate::db::worktrees::list(&db.conn).expect("list")).expect("db");
    assert!(rows.is_empty(), "its row went with it");
    cleanup(&root, state);
}

/// Open a session in a new worktree (the project's default on) and return
/// its id and folder.
fn worktree_session(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, project: &str) -> (String, std::path::PathBuf) {
    let project = project.to_owned();
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.set_worktree_default(&project, true, cx))).expect("default on");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (id, folder) = active(baaz, vc);
    assert!(Harness::in_worktree(&folder), "a worktree session: {folder}");
    (id, std::path::PathBuf::from(folder))
}

/// Send `text` on the open session and fold what the double answered so
/// far (a `[hold <ms>]` turn stays running).
pub(super) fn send(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext, text: &str) {
    let text = text.to_owned();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.send_text(text, cx)));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.test_drain_provider(cx)));
    });
    vc.run_until_parked();
}

fn porcelain(dir: &std::path::Path) -> Vec<String> {
    let out = std::process::Command::new("git").current_dir(dir).args(["status", "--porcelain"]).output().expect("git");
    let mut lines: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect();
    lines.sort();
    lines
}

fn row_of(state: &std::path::Path, id: &str) -> baaz_core::worktrees::Worktree {
    crate::db::flush();
    let id = id.to_owned();
    crate::db::call(crate::db::path_in(state), move |db| crate::db::worktrees::by_session(&db.conn, "codex", &id).expect("read"))
        .flatten()
        .expect("the session's worktree row")
}

/// THE ROUND TRIP THROUGH THE WINDOW (docs/82 6.2). Archiving a worktree
/// session archives its worktree — work preserved to the session's hidden
/// ref, the snapshot on the row, the folder gone, the branch kept — and
/// the toast's Undo (the unarchive path) brings the folder back with the
/// work unstaged before the row returns. Settings › Archived's unarchive
/// does the same.
#[gpui::test]
fn archiving_a_worktree_session_preserves_and_removes_it_and_unarchive_restores_it(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-archive");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let (id, folder) = worktree_session(&baaz, vc, &project);
    send(&baaz, vc, "edit something");
    vc.run_until_parked();
    std::fs::write(folder.join("a.txt"), "one\nedited\n").expect("tracked edit");
    std::fs::write(folder.join("new.txt"), "new\n").expect("untracked");
    let branch = git(&folder, &["branch", "--show-current"]);
    let row = row_of(&state.2, &id);
    assert_eq!(row.path, folder);

    for round in ["undo", "settings"] {
        vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.archive_session(id.clone(), Some(window), cx)));
        vc.run_until_parked();
        let archived = row_of(&state.2, &id);
        let snapshot = archived.snapshot.clone().expect("the row holds the snapshot");
        let uuid = crate::db::call(crate::db::path_in(&state.2), {
            let id = id.clone();
            move |db| crate::db::worktrees::session_key(&db.conn, "codex", &id).expect("read")
        })
        .flatten()
        .expect("the session row's uuid");
        assert_eq!(snapshot.reference, Some(format!("refs/baaz/preserved/{uuid}")), "{round}: named by the session's uuid");
        assert!(!folder.exists(), "{round}: the folder is removed");
        assert_eq!(git(&root, &["branch", "--list", &branch]).trim_start_matches(['*', '+', ' ']), branch, "{round}: the branch is kept");
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.session_list.read(cx).overrides.get(&id).is_some_and(|meta| meta.archived), "{round}: the row is archived");
        });
        assert_eq!(baaz_core::worktrees::cached_branch_for_folder(&folder), None, "{round}: no branch is named for a gone folder");

        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| match round {
                "undo" => harness.undo_newest(cx),
                _ => harness.unarchive_session(id.clone(), cx),
            })
        });
        vc.run_until_parked();
        assert!(folder.is_dir(), "{round}: the folder is back");
        assert_eq!(git(&folder, &["branch", "--show-current"]), branch);
        assert_eq!(porcelain(&folder), vec![" M a.txt", "?? new.txt"], "{round}: the work is back, unstaged");
        assert_eq!(row_of(&state.2, &id).snapshot, None, "{round}: the row is live");
        assert_eq!(baaz_core::worktrees::lock_reason(&row_of(&state.2, &id)).as_deref(), Some(format!("baaz session {id}").as_str()), "{round}: relocked");
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(!harness.session_list.read(cx).overrides.get(&id).is_some_and(|meta| meta.archived), "{round}: the row is back");
        });
        assert_eq!(baaz_core::worktrees::cached_branch_for_folder(&folder), Some(branch.clone()));
    }
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// A worktree session with a turn running is not archived from under its
/// agent: nothing changes and a toast says why.
#[gpui::test]
fn a_busy_worktree_session_is_not_archived(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-archive-busy");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let (id, folder) = worktree_session(&baaz, vc, &project);
    send(&baaz, vc, "[hold 3000] working");
    vc.run_until_parked();
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.archive_session(id.clone(), Some(window), cx)));
    vc.run_until_parked();
    assert!(folder.is_dir(), "the folder stays");
    assert_eq!(row_of(&state.2, &id).snapshot, None);
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(!harness.session_list.read(cx).overrides.get(&id).is_some_and(|meta| meta.archived), "not archived");
        assert_eq!(harness.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone()), Some(id.clone()), "still open");
    });
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// THE SHARED-FOLDER WARNING. A new session that lands in the folder where
/// another session has a turn running shows one banner — once, however
/// often it lands — whose "Run in a worktree" moves the unsent draft into a
/// worktree of its own. A folder whose other session is idle shows none.
#[gpui::test]
fn a_session_opened_where_another_is_working_is_warned_once(cx: &mut gpui::TestAppContext) {
    let state = lane_state("shared-folder");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, _project) = window(cx, &state.2, &root, factory);
    let banner = |baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext| {
        vc.update(|_, cx| {
            let view = baaz.read(cx).active.clone().expect("open");
            let rt = view.read(cx).rt(cx);
            (rt.banner_text().map(str::to_owned), rt.banner_action_for_test(), rt.session_id.clone())
        })
    };
    // Idle: the first session's turn finished before the second opens.
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let first_id = active(&baaz, vc).0;
    send(&baaz, vc, "done already");
    vc.run_until_parked();
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (text, _, idle_id) = banner(&baaz, vc);
    assert_eq!(text, None, "nobody is working in the folder: no warning");
    // Busy: that second session works, and a third opens beside it.
    send(&baaz, vc, "[hold 3000] working");
    vc.run_until_parked();
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    vc.run_until_parked();
    let (text, action, draft_id) = banner(&baaz, vc);
    assert_ne!(draft_id, idle_id);
    assert_eq!(text.as_deref(), Some(crate::session::SHARED_FOLDER_TEXT), "warned");
    assert_eq!(action, Some(crate::session::BannerAction::SharedFolder { worktree: true }), "an unsent draft is offered a worktree");
    // Dismissed, then back to the busy one and here again: not twice.
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.dismiss_banner(cx)));
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(idle_id.clone(), window, cx)));
    vc.run_until_parked();
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(draft_id.clone(), window, cx)));
    vc.run_until_parked();
    let (text, _, again) = banner(&baaz, vc);
    assert_eq!(again, draft_id);
    assert_eq!(text, None, "shown once per session");
    // A session with turns that lands there is warned, with no worktree
    // offered (it cannot move: rule 13).
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(first_id.clone(), window, cx)));
    vc.run_until_parked();
    let (text, action, landed) = banner(&baaz, vc);
    assert_eq!(landed, first_id);
    assert_eq!(text.as_deref(), Some(crate::session::SHARED_FOLDER_TEXT));
    assert_eq!(action, Some(crate::session::BannerAction::SharedFolder { worktree: false }), "only Dismiss");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(draft_id.clone(), window, cx)));
    vc.run_until_parked();
    // "Run in a worktree" on a fresh draft's banner moves it.
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("open");
        view.update(cx, |view, cx| {
            view.runtime().clone().update(cx, |rt, cx| {
                rt.forget_shared_folder_warning();
                assert!(rt.warn_shared_folder(true, cx));
                rt.run_banner_action(cx);
            })
        });
    });
    vc.run_until_parked();
    let (_, folder) = active(&baaz, vc);
    assert!(Harness::in_worktree(&folder), "the draft now runs in a worktree: {folder}");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// A worktree session reopened after a restart (`ResumeSession`) gets its
/// worktree's variables, as its open did (docs/82 6.2, the 6.1 carry-over).
#[gpui::test]
fn a_reopened_worktree_session_resumes_with_its_variables(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("worktree-resume-env");
    let root = repo(&state.2);
    let worktree = baaz_core::worktrees::create(&baaz_core::worktrees::CreateSpec {
        state_dir: &state.2,
        project_root: &root,
        project_id: None,
        branch_prefix: baaz_core::worktrees::DEFAULT_BRANCH_PREFIX,
        name: None,
    })
    .expect("a worktree");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    let folder = worktree.path.to_string_lossy().into_owned();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            z4_seed_stored_codex(harness, cx);
            harness.session_list.update(cx, |list, _| {
                if let Some(record) = list.provider_sessions.get_mut("s-codex-old") {
                    record.workspace = Some(folder.clone());
                }
            });
            harness.merge_provider_rows(cx);
        });
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx)));
    vc.run_until_parked();
    let env = commands
        .lock()
        .expect("commands")
        .iter()
        .find_map(|command| match command {
            provider::Command::ResumeSession { env, workspace, .. } => Some((env.clone(), workspace.clone())),
            _ => None,
        })
        .expect("the reopen resumed");
    assert_eq!(env.1.as_deref(), Some(folder.as_str()));
    assert_eq!(env.0, worktree.env(), "the resume carries the worktree's variables");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&worktree.path).output();
    crate::db::flush();
    crate::db::close(crate::db::path_in(&state.2));
    let _ = std::fs::remove_dir_all(&root);
    lane_restore(state);
}

/// Rule 13: "Move back to project folder" is offered only on a provider
/// that resumes a session in a folder the app names — not on the
/// providers whose own store keeps the folder (here Codex) — and the step
/// says so rather than doing nothing.
#[gpui::test]
fn move_back_is_offered_only_where_the_provider_resumes_in_a_named_folder(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-move-back-gate");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let (_id, folder) = worktree_session(&baaz, vc, &project);
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(!harness.move_back_offered(cx), "Codex resumes where its thread says: not offered");
    });
    let refused = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_back_to_root(cx)));
    assert!(refused.is_err(), "and refused, saying so");
    assert!(folder.is_dir(), "nothing moved");
    assert!(!ProviderId::Codex.has(provider::ProviderTrait::ResumesInNamedFolder));
    assert!(!ProviderId::ClaudeCode.has(provider::ProviderTrait::ResumesInNamedFolder));
    assert!(!ProviderId::Muse.has(provider::ProviderTrait::ResumesInNamedFolder));
    assert!(ProviderId::parse_known("acp").expect("acp is registered").has(provider::ProviderTrait::ResumesInNamedFolder));
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&folder).output();
    cleanup(&root, state);
}

/// MOVE BACK THROUGH THE WINDOW, on a provider that resumes in a named
/// folder (ACP): the worktree's committed and uncommitted changes land in
/// the project root, uncommitted; the worktree goes (branch kept); the
/// record names the root and the session reopens there.
#[gpui::test]
fn move_back_hands_the_changes_to_the_root_and_reopens_there(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-move-back");
    let root = repo(&state.2);
    let (factory, seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let acp = ProviderId::parse_known("acp").expect("acp is registered");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.provider_model.update(cx, |model, _| model.point_new_provider(acp))));
    let (id, folder) = worktree_session(&baaz, vc, &project);
    vc.update(|_, cx| assert_eq!(baaz.read(cx).active.as_ref().map(|view| view.read(cx).rt(cx).provider_kind()), Some(acp)));
    send(&baaz, vc, "make a change");
    std::fs::write(folder.join("a.txt"), "one\nfrom the worktree\n").expect("edit");
    git(&folder, &["-c", "user.email=t@example.com", "-c", "user.name=T", "commit", "-qam", "worktree commit"]);
    std::fs::write(folder.join("loose.txt"), "loose\n").expect("untracked");
    let branch = git(&folder, &["branch", "--show-current"]);
    vc.update(|_, cx| assert!(baaz.read(cx).move_back_offered(cx), "offered on ACP"));
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.move_back_to_root(cx))).expect("moving back");
    vc.run_until_parked();
    assert_eq!(porcelain(&root), vec![" M a.txt", "?? loose.txt"], "the worktree's changes, uncommitted, in the root");
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).expect("a"), "one\nfrom the worktree\n");
    assert!(!folder.exists(), "the worktree is removed");
    assert!(!git(&root, &["branch", "--list", &branch]).is_empty(), "its branch is kept");
    let root_text = root.to_string_lossy().into_owned();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let record = harness.session_list.read(cx).provider_sessions.get(&id).cloned().expect("the record");
        assert_eq!(record.workspace.as_deref(), Some(root_text.as_str()), "the session's folder is the root");
    });
    assert!(
        seen.lock().expect("seen").iter().any(|line| line == &format!("resume {id} in {root_text}")),
        "the session reopened in the root: {:?}",
        seen.lock().expect("seen")
    );
    crate::db::flush();
    let rows = crate::db::call(crate::db::path_in(&state.2), |db| crate::db::worktrees::list(&db.conn).expect("list")).expect("db");
    assert!(rows.is_empty(), "the worktree's row went");
    cleanup(&root, state);
}

/// Unarchive into a taken folder: the worktree comes back beside it, and
/// the session follows — its record names the new folder and its reopen
/// starts there, never in whatever now sits at the old path.
#[gpui::test]
fn unarchive_into_a_taken_folder_moves_the_session_with_it(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-unarchive-taken");
    let root = repo(&state.2);
    let (factory, seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let (id, folder) = worktree_session(&baaz, vc, &project);
    send(&baaz, vc, "edit something");
    std::fs::write(folder.join("new.txt"), "new\n").expect("untracked");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.archive_session(id.clone(), Some(window), cx)));
    vc.run_until_parked();
    assert!(!folder.exists());
    std::fs::create_dir_all(&folder).expect("someone else's folder");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.unarchive_session(id.clone(), cx)));
    vc.run_until_parked();
    let fresh = row_of(&state.2, &id).path;
    assert_ne!(fresh, folder, "a fresh folder beside the taken one");
    assert_eq!(porcelain(&fresh), vec!["?? new.txt"], "the work came back there");
    let fresh_text = fresh.to_string_lossy().into_owned();
    vc.update(|_, cx| {
        let record = baaz.read(cx).session_list.read(cx).provider_sessions.get(&id).cloned().expect("record");
        assert_eq!(record.workspace.as_deref(), Some(fresh_text.as_str()), "the record follows the worktree");
    });
    assert!(!baaz_core::worktrees::env_for_folder(&state.2, &fresh).is_empty(), "its variables are found at the new folder");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(id.clone(), window, cx)));
    vc.run_until_parked();
    assert!(
        seen.lock().expect("seen").iter().any(|line| line == &format!("resume {id} in {fresh_text}")),
        "the reopen starts in the new folder: {:?}",
        seen.lock().expect("seen")
    );
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&fresh).output();
    let _ = std::fs::remove_dir_all(&folder);
    cleanup(&root, state);
}

/// The project menu's "New sessions in their own worktree" row (Gate 6: the
/// per-project default had no control outside the probe). Offered on a
/// project in a git repository, checked as the default stands; a click
/// flips the stored default both ways. A project outside git is not offered
/// the row, and its click refuses.
#[gpui::test]
fn the_project_menu_flips_the_worktree_default_only_in_git(cx: &mut gpui::TestAppContext) {
    let state = lane_state("worktree-menu");
    let root = repo(&state.2);
    let (factory, _seen) = recording_factory();
    let (baaz, vc, project) = window(cx, &state.2, &root, factory);
    let offered = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).worktree_default_offered(&project, cx));
    assert_eq!(offered(vc), Some(false), "a git project offers the row, unchecked by default");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.flip_worktree_default(&project, cx))).expect("flips on");
    assert_eq!(offered(vc), Some(true), "the click turns the default on");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.flip_worktree_default(&project, cx))).expect("flips off");
    assert_eq!(offered(vc), Some(false), "a second click turns it off");

    // Outside git: no row, and the click refuses.
    let plain = state.2.join("plain-folder");
    std::fs::create_dir_all(&plain).unwrap();
    let plain_id = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.adopt_root(&plain, cx)));
    vc.update(|_, cx| assert_eq!(baaz.read(cx).worktree_default_offered(&plain_id, cx), None));
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.flip_worktree_default(&plain_id, cx)))
        .expect_err("no row outside git");
}
