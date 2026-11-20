//! A worktree per session (docs/82 6.1) through the window: a new session
//! with the project's default on opens its lane in a worktree (on a branch
//! with the Settings prefix) and edits only there; `worktree:on` / `worktree:off` move an unsent draft in and
//! out, and the worktree it leaves unused goes. Git runs in temp repos only;
//! the lanes are the scripted double behind a recorder.

use super::*;

/// What each `OpenSession` the recorder saw carried, in order: its
/// folder, then its env as `KEY=value` words (two entries per open).
type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The scripted double, one distinct session per open (`s-wt-1`, …),
/// recording each `OpenSession`'s folder.
fn recording_factory() -> (crate::providers::Registry, Seen) {
    use provider::ProviderAdapter as _;
    struct Recording {
        inner: provider::scripted::ScriptedProvider,
        seen: Seen,
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
            self.inner.dispatch(command)
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.inner.events()
        }
        fn shutdown(&mut self) {
            self.inner.shutdown();
        }
    }
    let seen: Seen = std::sync::Arc::default();
    let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory_seen = seen.clone();
    let factory = crate::providers::Registry::test(move |_: ProviderId| {
        let n = opens.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut adapter = Recording {
            inner: provider::scripted::ScriptedProvider::with_session_id(format!("s-wt-{n}")),
            seen: factory_seen.clone(),
        };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, seen)
}

fn git(dir: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").current_dir(dir).args(args).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A project repository beside the state dir (never inside it), one commit.
fn repo(state: &std::path::Path) -> std::path::PathBuf {
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
fn window<'a>(
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
fn active(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> (String, String) {
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("a session is open");
        let rt = view.read(cx).rt(cx);
        (rt.session_id.clone(), rt.workspace_path().to_owned())
    })
}

fn cleanup(root: &std::path::Path, state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf)) {
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
    // Settings › General › Worktrees: what the prefix field stores.
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
