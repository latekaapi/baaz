//! Worktrees in temp repos: git only, a local bare repository as `origin`
//! (a file path, never the network), each test its own state dir and
//! `baaz.db`.

use super::*;
use std::process::{Command, Stdio};

/// A temp directory removed on drop, its database closed first.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("baaz-worktrees-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        // Canonical, so paths git prints (`/private/var/...`) compare equal.
        Scratch(dir.canonicalize().expect("canonical temp dir"))
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        crate::db::close(crate::db::path_in(&self.state()));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn identity(dir: &Path) {
    run(dir, &["config", "user.email", "t@example.com"]);
    run(dir, &["config", "user.name", "T"]);
}

/// A project repo with one commit and no remote.
fn project(scratch: &Scratch) -> PathBuf {
    let root = scratch.path().join("project");
    std::fs::create_dir_all(&root).expect("root");
    run(&root, &["init", "-q", "-b", "main"]);
    identity(&root);
    std::fs::write(root.join("a.txt"), "one\n").expect("a.txt");
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "first"]);
    root
}

/// A project cloned from a local bare `origin`, then `origin` moved on by
/// another clone: the project's cached `origin/main` is one commit behind.
/// Returns the project root and the commit `origin/main` now names.
fn project_with_origin(scratch: &Scratch) -> (PathBuf, String) {
    let seed = project(scratch);
    let bare = scratch.path().join("origin.git");
    run(scratch.path(), &["clone", "-q", "--bare", &seed.to_string_lossy(), &bare.to_string_lossy()]);
    let root = scratch.path().join("clone");
    run(scratch.path(), &["clone", "-q", &bare.to_string_lossy(), &root.to_string_lossy()]);
    identity(&root);
    let other = scratch.path().join("other");
    run(scratch.path(), &["clone", "-q", &bare.to_string_lossy(), &other.to_string_lossy()]);
    identity(&other);
    std::fs::write(other.join("b.txt"), "two\n").expect("b.txt");
    run(&other, &["add", "-A"]);
    run(&other, &["commit", "-q", "-m", "second"]);
    run(&other, &["push", "-q", "origin", "main"]);
    let moved = run(&other, &["rev-parse", "HEAD"]);
    (root, moved)
}

fn spec<'a>(scratch_state: &'a Path, root: &'a Path, name: Option<&'a str>) -> CreateSpec<'a> {
    CreateSpec { state_dir: scratch_state, project_root: root, project_id: Some("p-1"), branch_prefix: DEFAULT_BRANCH_PREFIX, name }
}

fn rows(state: &Path) -> Vec<Worktree> {
    crate::db::call(crate::db::path_in(state), |db| crate::db::worktrees::list(&db.conn).expect("list")).expect("db opens")
}

#[test]
fn a_worktree_is_cut_from_the_fetched_remote_default_and_locked() {
    let scratch = Scratch::new("fetched");
    let (root, moved) = project_with_origin(&scratch);
    let cached = run(&root, &["rev-parse", "origin/main"]);
    assert_ne!(cached, moved, "the clone's cached ref is behind before the create");
    let state = scratch.state();
    let worktree = create(&spec(&state, &root, None)).expect("created");
    assert_eq!(worktree.base, moved, "cut from origin/main as fetched, not the cached ref");
    assert_eq!(run(&worktree.path, &["rev-parse", "HEAD"]), moved);
    assert!(worktree.path.starts_with(state.join("worktrees").join("p-1")), "{}", worktree.path.display());
    assert!(worktree.name.starts_with("s-") && worktree.name.len() == 8, "{}", worktree.name);
    assert_eq!(worktree.branch, format!("baaz/{}", worktree.name));
    assert_eq!(run(&worktree.path, &["branch", "--show-current"]), worktree.branch);
    assert!(is_worktree_folder(&state, &worktree.path));
    assert!(!is_worktree_folder(&state, &root));
    assert_eq!(lock_reason(&worktree).as_deref(), Some("baaz session pending"), "locked as soon as it exists");
    assert_eq!(rows(&state), vec![worktree.clone()], "the row holds what was made");
}

#[test]
fn with_no_remote_it_is_cut_from_head() {
    let scratch = Scratch::new("head");
    let root = project(&scratch);
    std::fs::write(root.join("a.txt"), "local\n").expect("edit");
    run(&root, &["commit", "-q", "-am", "local"]);
    let head = run(&root, &["rev-parse", "HEAD"]);
    let base = resolve_base(&root).expect("base");
    assert_eq!(base, Base { commit: head.clone(), label: "HEAD".into() });
    let worktree = create(&spec(&scratch.state(), &root, None)).expect("created");
    assert_eq!(worktree.base, head);
    assert_eq!(std::fs::read_to_string(worktree.path.join("a.txt")).expect("checked out"), "local\n");
}

#[test]
fn an_existing_branch_is_refused_not_reset() {
    let scratch = Scratch::new("branch");
    let root = project(&scratch);
    run(&root, &["branch", "baaz/s-fixed"]);
    let before = run(&root, &["rev-parse", "baaz/s-fixed"]);
    let state = scratch.state();
    let refused = create(&spec(&state, &root, Some("s-fixed")));
    assert!(refused.is_err(), "-b refuses a branch that exists");
    assert_eq!(run(&root, &["rev-parse", "baaz/s-fixed"]), before, "the branch is untouched");
    assert!(rows(&state).is_empty(), "a failed add leaves no row");
    assert!(!state.join("worktrees/p-1/s-fixed").exists(), "nor a folder");
}

#[test]
fn worktreeinclude_copies_only_the_ignored_files_it_names() {
    let scratch = Scratch::new("include");
    let root = project(&scratch);
    std::fs::write(root.join(".gitignore"), ".env\nsecret.txt\nbuild/\n").expect("ignore");
    std::fs::write(root.join(".worktreeinclude"), ".env\n*.local\n").expect("include");
    run(&root, &["add", ".gitignore", ".worktreeinclude"]);
    run(&root, &["commit", "-q", "-m", "ignore"]);
    std::fs::write(root.join(".env"), "KEY=1\n").expect(".env");
    std::fs::create_dir_all(root.join("app")).expect("app");
    std::fs::write(root.join("app/.env"), "KEY=2\n").expect("app/.env");
    std::fs::write(root.join("secret.txt"), "no\n").expect("secret");
    std::fs::create_dir_all(root.join("build")).expect("build");
    std::fs::write(root.join("build/out.bin"), "x").expect("build out");
    // Matches the include, but git does not ignore it: work, not a secret.
    std::fs::write(root.join("todo.local"), "mine\n").expect("todo");
    let worktree = create(&spec(&scratch.state(), &root, None)).expect("created");
    assert_eq!(std::fs::read_to_string(worktree.path.join(".env")).expect(".env copied"), "KEY=1\n");
    assert_eq!(std::fs::read_to_string(worktree.path.join("app/.env")).expect("nested .env copied"), "KEY=2\n");
    assert!(!worktree.path.join("secret.txt").exists(), "an ignored file the include does not name stays");
    assert!(!worktree.path.join("build").exists());
    assert!(!worktree.path.join("todo.local").exists(), "an untracked, unignored match stays");
    assert_eq!(run(&worktree.path, &["status", "--porcelain"]), "", "the copies are ignored there too");
}

#[test]
fn no_include_file_copies_nothing() {
    let scratch = Scratch::new("no-include");
    let root = project(&scratch);
    std::fs::write(root.join(".env"), "KEY=1\n").expect(".env");
    let dest = scratch.path().join("dest");
    std::fs::create_dir_all(&dest).expect("dest");
    assert_eq!(copy_included(&root, &dest), Ok(0));
}

#[test]
fn setup_runs_once_in_the_worktree_with_its_env() {
    let scratch = Scratch::new("setup");
    let root = project(&scratch);
    let out = scratch.path().join("setup-out.txt");
    std::fs::create_dir_all(root.join(".baaz")).expect(".baaz");
    std::fs::write(
        root.join(".baaz/project.toml"),
        format!(
            "# the project's declarations\nsetup = \"echo \\\"$BAAZ_WORKTREE_PATH|$BAAZ_PROJECT_ROOT|$BAAZ_PORT|$BAAZ_PORT_COUNT|$(pwd -P)\\\" >> {}; echo ran\"\nrun = 'npm start'\npreview_url = \"http://localhost:$BAAZ_PORT\"\n",
            out.display()
        ),
    )
    .expect("project.toml");
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "declare"]);
    let state = scratch.state();
    let worktree = create(&spec(&state, &root, None)).expect("created");
    let env: Vec<(OsString, OsString)> = std::env::vars_os().collect();
    assert_eq!(run_setup_once(&state, &worktree, &env, SETUP_TIMEOUT), SetupOutcome::Ran);
    assert_eq!(
        run_setup_once(&state, &worktree, &env, SETUP_TIMEOUT),
        SetupOutcome::AlreadyRan,
        "a second open never runs it again"
    );
    let lines: Vec<String> = std::fs::read_to_string(&out).expect("setup wrote").lines().map(str::to_owned).collect();
    assert_eq!(
        lines,
        vec![format!(
            "{}|{}|{}|10|{}",
            worktree.path.display(),
            root.display(),
            worktree.port,
            worktree.path.display()
        )],
        "ran once, in the worktree, with the three variables"
    );
    assert!(std::fs::read_to_string(worktree.setup_log()).expect("log").contains("ran"), "output goes to the log");
    assert!(!worktree.setup_log().starts_with(&worktree.path), "the log is beside the worktree, not in it");
}

#[test]
fn a_failing_setup_keeps_the_worktree_and_says_why() {
    let scratch = Scratch::new("setup-fail");
    let root = project(&scratch);
    std::fs::create_dir_all(root.join(".baaz")).expect(".baaz");
    std::fs::write(root.join(".baaz/project.toml"), "setup = \"echo broken >&2; exit 3\"\n").expect("toml");
    let state = scratch.state();
    let worktree = create(&spec(&state, &root, None)).expect("created");
    let outcome = run_setup_once(&state, &worktree, &[], SETUP_TIMEOUT);
    assert_eq!(outcome, SetupOutcome::Failed("exit 3".into()));
    assert!(worktree.path.join("a.txt").exists(), "the worktree stays");
    assert!(std::fs::read_to_string(worktree.setup_log()).expect("log").contains("broken"), "stderr is logged");
    // A setup that outlives its timeout is killed with its group.
    let slow = Worktree { id: "slow".into(), ..worktree.clone() };
    std::fs::write(root.join(".baaz/project.toml"), "setup = \"sleep 30\"\n").expect("toml");
    let id = slow.id.clone();
    let row = Worktree { path: scratch.path().join("elsewhere"), ..slow.clone() };
    crate::db::call(crate::db::path_in(&state), move |db| {
        crate::db::worktrees::reserve(&db.conn, &Worktree { id, ..row }).expect("row")
    });
    let started = Instant::now();
    let timed = run_setup_once(&state, &slow, &[], Duration::from_millis(300));
    assert!(matches!(timed, SetupOutcome::Failed(ref why) if why.contains("ran past")), "{timed:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn port_blocks_are_distinct_persisted_and_reused_once_freed() {
    let scratch = Scratch::new("ports");
    let root = project(&scratch);
    let state = scratch.state();
    let first = create(&spec(&state, &root, None)).expect("first");
    let second = create(&spec(&state, &root, None)).expect("second");
    assert_eq!(first.port, PORT_BASE);
    assert_eq!(second.port, PORT_BASE + PORT_BLOCK, "the next block of ten");
    assert_ne!(first.path, second.path);
    let stored: Vec<u16> = rows(&state).iter().map(|row| row.port).collect();
    assert_eq!(stored, vec![first.port, second.port], "persisted in the rows");
    assert_eq!(discard_unused(&state, &first), Ok(true));
    let third = create(&spec(&state, &root, None)).expect("third");
    assert_eq!(third.port, PORT_BASE, "a freed block is handed out again");
    assert!(first.env().contains(&("BAAZ_PORT".into(), PORT_BASE.to_string())));
}

#[test]
fn the_row_ties_to_its_session() {
    let scratch = Scratch::new("link");
    let root = project(&scratch);
    let state = scratch.state();
    let worktree = create(&spec(&state, &root, None)).expect("created");
    crate::db::call(crate::db::path_in(&state), |db| {
        crate::db::sessions::ensure_session(&db.conn, "codex", "s-1", crate::db::sessions::ORIGIN_BAAZ).expect("row")
    });
    assert!(link_session(&state, "codex", "s-1", &worktree.path), "linked");
    assert!(!link_session(&state, "codex", "s-1", &root), "the project root is no worktree");
    let (linked, column) = crate::db::call(crate::db::path_in(&state), |db| {
        let linked = crate::db::worktrees::by_session(&db.conn, "codex", "s-1").expect("read");
        let column: Option<String> = db
            .conn
            .query_row("SELECT worktree_id FROM session WHERE provider_session_id = 's-1'", [], |row| row.get(0))
            .expect("column");
        (linked, column)
    })
    .expect("db");
    let linked = linked.expect("the session's worktree");
    assert_eq!(column.as_deref(), Some(worktree.id.as_str()), "session.worktree_id");
    assert_eq!(linked.created_by_session.as_deref(), Some("s-1"));
    assert_eq!(linked.path, worktree.path);
    assert_eq!(lock_reason(&worktree).as_deref(), Some("baaz session s-1"), "relocked in the session's name");
    assert_eq!(by_path(&state, &worktree.path).map(|row| row.id), Some(worktree.id.clone()));
    assert_eq!(env_for_folder(&state, &worktree.path), worktree.env(), "a session there starts with them");
    assert_eq!(cached_env_for_folder(&worktree.path), worktree.env(), "and the UI thread reads them without the database");
    assert!(env_for_folder(&state, &root).is_empty(), "the project root has none");
    // The row goes with the worktree, and the session row lets go of it.
    assert_eq!(discard_unused(&state, &linked), Ok(true));
    assert!(cached_env_for_folder(&worktree.path).is_empty());
    let column: Option<String> = crate::db::call(crate::db::path_in(&state), |db| {
        db.conn.query_row("SELECT worktree_id FROM session WHERE provider_session_id = 's-1'", [], |row| row.get(0)).expect("column")
    })
    .expect("db");
    assert_eq!(column, None);
}

#[test]
fn an_unused_worktree_is_removed_only_when_clean() {
    let scratch = Scratch::new("discard");
    let root = project(&scratch);
    let state = scratch.state();
    let clean = create(&spec(&state, &root, None)).expect("clean");
    let dirty = create(&spec(&state, &root, None)).expect("dirty");
    std::fs::write(dirty.path.join("work.txt"), "mine\n").expect("work");
    assert_eq!(discard_unused(&state, &clean), Ok(true));
    assert!(!clean.path.exists(), "removed");
    assert!(run(&root, &["branch", "--list", &clean.branch]).is_empty(), "its untouched branch goes too");
    assert_eq!(discard_unused(&state, &dirty), Ok(false), "never --force");
    assert!(dirty.path.join("work.txt").exists(), "the work stays");
    assert!(lock_reason(&dirty).is_some(), "still locked");
    assert_eq!(rows(&state), vec![dirty]);
}

#[test]
fn project_toml_reads_its_three_strings() {
    let config = parse_project_toml(
        "# comment\nsetup = \"npm ci && cp \\\"a\\\" b\"\nrun = 'npm run dev'\npreview_url = \"\"\"\nhttp://localhost:3000\"\"\"\nother = 1\n[table]\nsetup = \"no\"\n",
    );
    assert_eq!(config.setup.as_deref(), Some("npm ci && cp \"a\" b"));
    assert_eq!(config.run.as_deref(), Some("npm run dev"));
    assert_eq!(config.preview_url.as_deref(), Some("http://localhost:3000"));
    assert_eq!(parse_project_toml("setup = \"\"\n"), ProjectConfig::default(), "an empty setup is none");
    assert_eq!(parse_project_toml("setup = \"unterminated\n"), ProjectConfig::default());
}

#[test]
fn the_branch_prefix_and_the_project_default_read_back() {
    use crate::layout::{normalize_branch_prefix, Layout};
    assert_eq!(Layout::default().worktree_branch_prefix(), "baaz/");
    assert_eq!(normalize_branch_prefix(" my team/ "), Some("my-team/".into()));
    assert_eq!(normalize_branch_prefix("/..~^:"), None, "nothing usable: the default stands");
    let layout = Layout { worktree_branch_prefix: Some("lk/".into()), ..Layout::default() };
    assert_eq!(layout.worktree_branch_prefix(), "lk/");
    let on: crate::projects::ProjectDefaults = serde_json::from_str(r#"{"worktree":true}"#).expect("reads");
    assert!(on.worktree);
    assert_eq!(serde_json::to_value(&on).expect("writes")["worktree"], true);
    let off = crate::projects::ProjectDefaults::default();
    assert!(serde_json::to_value(&off).expect("writes").get("worktree").is_none(), "off is not written");
}
