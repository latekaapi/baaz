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

// ------------------------------------------------- lifecycle (docs/82 6.2)

/// A project with files to delete and rename, an ignored `.env` that
/// `.worktreeinclude` names, and a worktree of it.
fn lifecycle_project(scratch: &Scratch) -> (PathBuf, Worktree) {
    let root = project(scratch);
    std::fs::write(root.join("b.txt"), "bee\n").expect("b.txt");
    std::fs::write(root.join("c.txt"), "sea\n").expect("c.txt");
    std::fs::write(root.join(".gitignore"), ".env\n").expect("ignore");
    std::fs::write(root.join(".worktreeinclude"), ".env\n").expect("include");
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "more"]);
    std::fs::write(root.join(".env"), "SECRET=1\n").expect(".env");
    let worktree = create(&spec(&scratch.state(), &root, None)).expect("created");
    assert!(worktree.path.join(".env").is_file(), "the include copied it at create");
    (root, worktree)
}

/// Every kind of change a session leaves: a commit on its branch, a
/// tracked edit (unstaged), a staged edit, a deleted file, a renamed file
/// (`git mv`, staged), a staged new file and an untracked one.
fn dirty(worktree: &Worktree) {
    let dir = &worktree.path;
    std::fs::write(dir.join("committed.txt"), "on the branch\n").expect("committed");
    run(dir, &["add", "committed.txt"]);
    run(dir, &["-c", "user.email=t@example.com", "-c", "user.name=T", "commit", "-q", "-m", "work"]);
    std::fs::write(dir.join("a.txt"), "one\nedited\n").expect("edit");
    std::fs::write(dir.join("b.txt"), "bee\nstaged edit\n").expect("staged edit");
    run(dir, &["add", "b.txt"]);
    std::fs::remove_file(dir.join("c.txt")).expect("delete");
    run(dir, &["mv", "a.txt", "a-renamed.txt"]);
    std::fs::write(dir.join("staged-new.txt"), "staged\n").expect("staged new");
    run(dir, &["add", "staged-new.txt"]);
    std::fs::write(dir.join("untracked.txt"), "loose\n").expect("untracked");
}

fn status(dir: &Path) -> Vec<String> {
    // Not `run`: its trim would eat the first line's leading space.
    let out = Command::new("git").current_dir(dir).args(["status", "--porcelain", "--untracked-files=all"]).output().expect("git runs");
    let mut lines: Vec<String> =
        String::from_utf8_lossy(&out.stdout).lines().filter(|line| !line.is_empty()).map(str::to_owned).collect();
    lines.sort();
    lines
}

fn row(state: &Path, id: &str) -> Worktree {
    rows(state).into_iter().find(|row| row.id == id).expect("the row")
}

fn listed(root: &Path, folder: &Path) -> bool {
    run(root, &["worktree", "list", "--porcelain"]).lines().any(|line| line == format!("worktree {}", folder.display()))
}

/// THE ROUND TRIP. Archive preserves the dirty tree to its hidden ref
/// (recorded on the row before anything goes), removes the folder and its
/// lock, keeps the branch, and touches neither the project's index nor its
/// stash; unarchive brings the folder back on the branch at the same head
/// with every file as it was — edits unstaged, new files untracked, the
/// deletion and the rename as files — the `.env` copied again, re-locked.
#[test]
fn archive_then_unarchive_round_trips_a_dirty_tree() {
    let scratch = Scratch::new("round-trip");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    // As the window holds it once its session landed (`link_session`).
    let worktree = Worktree { created_by_session: Some("sess-1".to_owned()), ..worktree };
    claim(&worktree, "sess-1").expect("claimed");
    dirty(&worktree);
    let head = run(&worktree.path, &["rev-parse", "HEAD"]);
    let before: Vec<(String, String)> = ["a-renamed.txt", "b.txt", "committed.txt", "staged-new.txt", "untracked.txt"]
        .iter()
        .map(|name| (name.to_string(), std::fs::read_to_string(worktree.path.join(name)).expect(name)))
        .collect();
    let root_index = std::fs::read(root.join(".git/index")).expect("the project's index");

    let archived = archive(&state, &worktree, "uuid-1").expect("archived");
    let snapshot = archived.snapshot.clone().expect("a snapshot");
    assert_eq!(snapshot.reference.as_deref(), Some("refs/baaz/preserved/uuid-1"));
    assert_eq!(snapshot.head, head, "the branch head is recorded");
    assert_eq!(row(&state, &worktree.id).snapshot, Some(snapshot.clone()), "the row holds the ref and the head");
    assert!(!worktree.path.exists(), "the folder is gone");
    assert!(!listed(&root, &worktree.path), "git no longer lists it (and so holds no lock on it)");
    assert_eq!(run(&root, &["rev-parse", &format!("refs/heads/{}", worktree.branch)]), head, "the branch is kept");
    let preserved = run(&root, &["rev-parse", "refs/baaz/preserved/uuid-1^1"]);
    assert_eq!(preserved, head, "the preserved commit sits on the branch head");
    assert_eq!(run(&root, &["stash", "list"]), "", "the stash is untouched");
    assert_eq!(std::fs::read(root.join(".git/index")).expect("index"), root_index, "the project's index is untouched");
    assert_eq!(status(&root), Vec::<String>::new(), "the project root is clean");
    assert_eq!(cached_branch_for_folder(&worktree.path), None, "the cache forgot it");
    assert_eq!(archive(&state, &archived, "uuid-1").expect("again"), archived, "archiving twice changes nothing");

    let restored = restore(&state, &archived).expect("restored");
    assert_eq!(restored.note, None);
    let back = restored.worktree;
    assert_eq!(back.path, worktree.path, "at its old folder");
    assert_eq!(back.snapshot, None);
    let live = row(&state, &worktree.id);
    assert_eq!((live.snapshot, live.path), (None, back.path.clone()), "the row is live again");
    assert_eq!(run(&back.path, &["branch", "--show-current"]), worktree.branch);
    assert_eq!(run(&back.path, &["rev-parse", "HEAD"]), head, "at the same head");
    assert_eq!(
        status(&back.path),
        vec![" D a.txt", " D c.txt", " M b.txt", "?? a-renamed.txt", "?? staged-new.txt", "?? untracked.txt"],
        "edits unstaged, new files untracked, the deletion and the rename as files"
    );
    for (name, text) in &before {
        assert_eq!(&std::fs::read_to_string(back.path.join(name)).expect(name), text, "{name} as it was");
    }
    assert!(!back.path.join("c.txt").exists(), "the deleted file stays deleted");
    assert_eq!(std::fs::read_to_string(back.path.join(".env")).expect(".env"), "SECRET=1\n", "the include's file is copied again");
    assert_eq!(lock_reason(&back).as_deref(), Some("baaz session sess-1"), "locked again in its session's name");
    assert_eq!(cached_branch_for_folder(&back.path), Some(worktree.branch.clone()));
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&back.path).output();
}

/// Something now sits at the old folder: the worktree comes back beside
/// it under a fresh name, and the row follows.
#[test]
fn unarchive_into_a_taken_folder_takes_a_fresh_name() {
    let scratch = Scratch::new("taken");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    std::fs::write(worktree.path.join("untracked.txt"), "loose\n").expect("untracked");
    let archived = archive(&state, &worktree, "uuid-2").expect("archived");
    std::fs::create_dir_all(&worktree.path).expect("someone else's folder");
    let back = restore(&state, &archived).expect("restored").worktree;
    assert_ne!(back.path, worktree.path);
    assert_eq!(back.path.parent(), worktree.path.parent(), "beside the old one");
    assert_eq!(row(&state, &worktree.id).path, back.path, "the row names the new folder");
    assert_eq!(status(&back.path), vec!["?? untracked.txt"]);
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&back.path).output();
}

/// The branch was deleted while archived: it is made again at the
/// snapshot's head, and the work still comes back.
#[test]
fn unarchive_after_the_branch_was_deleted_recreates_it() {
    let scratch = Scratch::new("branch-gone");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    dirty(&worktree);
    let head = run(&worktree.path, &["rev-parse", "HEAD"]);
    let archived = archive(&state, &worktree, "uuid-3").expect("archived");
    run(&root, &["branch", "-D", &worktree.branch]);
    let back = restore(&state, &archived).expect("restored").worktree;
    assert_eq!(run(&back.path, &["branch", "--show-current"]), worktree.branch, "the old branch name");
    assert_eq!(run(&back.path, &["rev-parse", "HEAD"]), head, "at the preserved head");
    assert!(status(&back.path).contains(&"?? untracked.txt".to_owned()));
    assert_eq!(std::fs::read_to_string(back.path.join("b.txt")).expect("b"), "bee\nstaged edit\n");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&back.path).output();
}

/// A folder deleted by hand before the archive: nothing to preserve, so
/// the row records the branch's tip alone, and git's list is pruned.
#[test]
fn archiving_a_missing_folder_records_the_branch_alone() {
    let scratch = Scratch::new("missing");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    let head = run(&worktree.path, &["rev-parse", "HEAD"]);
    unlock(&worktree);
    std::fs::remove_dir_all(&worktree.path).expect("deleted by hand");
    let archived = archive(&state, &worktree, "uuid-4").expect("archived");
    assert_eq!(archived.snapshot, Some(Snapshot { reference: None, head: head.clone() }));
    assert!(!listed(&root, &worktree.path), "pruned");
    let back = restore(&state, &archived).expect("restored").worktree;
    assert_eq!(run(&back.path, &["rev-parse", "HEAD"]), head);
    assert_eq!(status(&back.path), Vec::<String>::new());
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&back.path).output();
}

/// MOVE BACK. The worktree's branch commits and its uncommitted work land
/// in a clean project root as uncommitted changes — the root's changes are
/// the worktree's — and the worktree goes (preserved, branch kept, row
/// gone).
#[test]
fn move_back_hands_the_changes_to_a_clean_root() {
    let scratch = Scratch::new("move-back");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    dirty(&worktree);
    let files = ["a-renamed.txt", "b.txt", "committed.txt", "staged-new.txt", "untracked.txt"];
    let expected: Vec<(String, String)> =
        files.iter().map(|name| (name.to_string(), std::fs::read_to_string(worktree.path.join(name)).expect(name))).collect();
    move_back(&state, &worktree, "uuid-5").expect("moved back");
    assert_eq!(
        status(&root),
        vec![" D a.txt", " D c.txt", " M b.txt", "?? a-renamed.txt", "?? committed.txt", "?? staged-new.txt", "?? untracked.txt"],
        "the worktree's changes, uncommitted, in the root"
    );
    for (name, text) in &expected {
        assert_eq!(&std::fs::read_to_string(root.join(name)).expect(name), text, "{name}");
    }
    assert_eq!(run(&root, &["branch", "--show-current"]), "main", "the root stays on its branch");
    assert!(!worktree.path.exists(), "the worktree is removed");
    assert!(run(&root, &["rev-parse", "--verify", &format!("refs/heads/{}", worktree.branch)]).len() == 40, "its branch is kept");
    assert!(run(&root, &["rev-parse", "--verify", "refs/baaz/preserved/uuid-5"]).len() == 40, "its work is preserved");
    assert!(rows(&state).iter().all(|row| row.id != worktree.id), "its row is gone");
}

/// A root whose own changes collide with the worktree's: refused, and
/// neither folder changes.
#[test]
fn move_back_refuses_when_the_root_conflicts_and_changes_nothing() {
    let scratch = Scratch::new("move-back-conflict");
    let state = scratch.state();
    let (root, worktree) = lifecycle_project(&scratch);
    std::fs::write(worktree.path.join("b.txt"), "bee\nworktree\n").expect("worktree edit");
    std::fs::write(worktree.path.join("untracked.txt"), "loose\n").expect("untracked");
    std::fs::write(root.join("b.txt"), "root's own\n").expect("root edit");
    let (root_before, tree_before) = (status(&root), status(&worktree.path));
    let error = move_back(&state, &worktree, "uuid-6").expect_err("refused");
    assert!(error.contains("don't apply cleanly"), "{error}");
    assert_eq!(status(&root), root_before, "the root is untouched");
    assert_eq!(std::fs::read_to_string(root.join("b.txt")).expect("b"), "root's own\n");
    assert!(!root.join("untracked.txt").exists(), "nothing half-applied");
    assert_eq!(status(&worktree.path), tree_before, "the worktree is untouched");
    assert_eq!(row(&state, &worktree.id).snapshot, None, "and still live");
    assert!(lock_reason(&worktree).is_some(), "and still locked");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&worktree.path).output();
}

/// A launch reads every live row into the cache; an archived one is not.
#[test]
fn remember_all_caches_live_rows_only() {
    let scratch = Scratch::new("remember");
    let state = scratch.state();
    let (root, live) = lifecycle_project(&scratch);
    let gone = create(&spec(&state, &root, None)).expect("second");
    archive(&state, &gone, "uuid-7").expect("archived");
    forget(&live.path);
    assert_eq!(remember_all(&state), 1);
    assert_eq!(cached_branch_for_folder(&live.path), Some(live.branch.clone()));
    assert_eq!(cached_branch_for_folder(&gone.path), None);
    assert!(env_for_folder(&state, &gone.path).is_empty(), "an archived worktree gives no variables");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force", "--force"]).arg(&live.path).output();
}

/// The new-session row's check (docs/82 6.6): a repository, a folder inside
/// one and a worktree (whose `.git` is a file) are in one; a plain folder
/// is not.
#[test]
fn in_git_repository_sees_repos_subfolders_and_worktrees() {
    let scratch = Scratch::new("in-repo");
    let plain = scratch.path().join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    assert!(!in_git_repository(&plain), "a folder outside any repository");
    let root = project(&scratch);
    assert!(in_git_repository(&root));
    let sub = root.join("src/deep");
    std::fs::create_dir_all(&sub).expect("sub");
    assert!(in_git_repository(&sub), "a folder inside the repository");
    let worktree = create(&spec(&scratch.state(), &root, None)).expect("created");
    assert!(worktree.path.join(".git").is_file());
    assert!(in_git_repository(&worktree.path), "a worktree");
}

/// docs/82 7.4: a plain session's Run block comes from the top of the
/// range down, past blocks other Runs hold and blocks something listens
/// on, and its env carries the port.
#[test]
fn a_plain_run_takes_the_highest_free_block_and_its_env_names_it() {
    let top = PORT_BASE + (PORT_BLOCKS - 1) * PORT_BLOCK;
    assert_eq!(run_port_block(&[], |_| true), Some(top));
    assert_eq!(run_port_block(&[top], |_| true), Some(top - PORT_BLOCK), "a block another Run holds is skipped");
    assert_eq!(run_port_block(&[], |port| port != top), Some(top - PORT_BLOCK), "a listening block is skipped");
    assert_eq!(run_port_block(&[], |_| false), None);
    let env = plain_env(Path::new("/acme"), 60990);
    assert!(env.contains(&("BAAZ_PORT".to_owned(), "60990".to_owned())), "{env:?}");
    assert!(env.contains(&("BAAZ_PORT_COUNT".to_owned(), PORT_BLOCK.to_string())), "{env:?}");
    assert!(env.contains(&("BAAZ_PROJECT_ROOT".to_owned(), "/acme".to_owned())), "{env:?}");
    assert!(!env.iter().any(|(key, _)| key == "BAAZ_WORKTREE_PATH"), "no worktree: {env:?}");
}

/// `port_free` is a bind: a port a listener holds is not free.
#[test]
fn a_held_port_is_not_free() {
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("skipped: this sandbox forbids listening ({error})");
            return;
        }
    };
    let port = listener.local_addr().expect("bound").port();
    assert!(!port_free(port));
    drop(listener);
}
