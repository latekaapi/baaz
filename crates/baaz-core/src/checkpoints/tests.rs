//! Checkpoints in temp repos: git only, never the network, each test its
//! own folder.

use super::*;
use std::process::{Command, Stdio};

/// A temp directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("baaz-checkpoints-{tag}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        Scratch(dir.canonicalize().expect("canonical temp dir"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
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

/// A repo with one commit (`a.txt`, `.gitignore` ignoring `*.log`), a
/// staged change and a stash, so a snapshot that touched any of them shows.
fn repo(scratch: &Scratch) -> PathBuf {
    let root = scratch.0.join("repo");
    std::fs::create_dir_all(&root).expect("root");
    run(&root, &["init", "-q", "-b", "main"]);
    run(&root, &["config", "user.email", "t@example.com"]);
    run(&root, &["config", "user.name", "T"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\n").expect("a.txt");
    std::fs::write(root.join(".gitignore"), "*.log\n").expect(".gitignore");
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "first"]);
    root
}

/// What a snapshot must leave as it was: HEAD, the index, the stash, the
/// status (the working tree as git sees it).
fn person_state(root: &Path) -> (String, String, String, String) {
    (
        run(root, &["rev-parse", "HEAD"]),
        run(root, &["ls-files", "-s"]),
        run(root, &["stash", "list"]),
        run(root, &["status", "--porcelain", "--untracked-files=all"]),
    )
}

fn names(files: &[FileChange]) -> Vec<(String, ChangeKind, u32, u32)> {
    files.iter().map(|f| (f.path.clone(), f.change, f.added, f.removed)).collect()
}

/// Drive a recorder through its jobs synchronously, as the app does one
/// job at a time off the UI thread.
fn drain(recorder: &mut Recorder, first: Option<Job>, key: &str) {
    let mut job = first;
    while let Some(next) = job {
        let done = run_job(next, || Some(key.to_owned()));
        job = recorder.job_done(done);
    }
}

#[test]
fn a_snapshot_leaves_the_index_head_stash_and_worktree_untouched() {
    let scratch = Scratch::new("untouched");
    let root = repo(&scratch);
    std::fs::write(root.join("a.txt"), "one\nTWO\nthree\n").expect("edit");
    run(&root, &["stash", "-q"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").expect("edit");
    std::fs::write(root.join("staged.txt"), "staged\n").expect("staged");
    run(&root, &["add", "staged.txt"]);
    std::fs::write(root.join("new.txt"), "new\n").expect("untracked");
    let before = person_state(&root);
    let index_bytes = std::fs::read(root.join(".git/index")).expect("index");

    let checkpoint = snapshot(&root, None, "test").expect("snapshot").expect("a git folder takes checkpoints");

    assert_eq!(std::fs::read(root.join(".git/index")).expect("index"), index_bytes, "the index file is byte-identical");
    assert_eq!(person_state(&root), before, "HEAD, index, stash and status are as they were");
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).expect("a"), "one\ntwo\nthree\nfour\n");
    assert_eq!(run(&root, &["rev-parse", &format!("{}^", checkpoint.commit)]), before.0, "its parent is HEAD");
    let leftovers: Vec<_> = std::fs::read_dir(root.join(".git"))
        .expect(".git")
        .flatten()
        .filter(|entry| entry.file_name().to_string_lossy().starts_with("baaz-snapshot-"))
        .collect();
    assert!(leftovers.is_empty(), "the temporary index is removed");
}

#[test]
fn a_snapshot_holds_untracked_files_and_not_ignored_ones() {
    let scratch = Scratch::new("untracked");
    let root = repo(&scratch);
    std::fs::write(root.join("new.txt"), "new\n").expect("untracked");
    std::fs::write(root.join("build.log"), "noise\n").expect("ignored");
    let checkpoint = snapshot(&root, None, "test").expect("snapshot").expect("on");
    let listed = run(&root, &["ls-tree", "-r", "--name-only", &checkpoint.commit]);
    let listed: Vec<&str> = listed.lines().collect();
    assert!(listed.contains(&"new.txt"), "the untracked file is in it: {listed:?}");
    assert!(!listed.contains(&"build.log"), "the ignored file is not: {listed:?}");
}

#[test]
fn an_unchanged_tree_reuses_the_last_commit() {
    let scratch = Scratch::new("reuse");
    let root = repo(&scratch);
    std::fs::write(root.join("a.txt"), "changed\n").expect("edit");
    let first = snapshot(&root, None, "first").expect("snapshot").expect("on");
    let again = snapshot(&root, Some(&first), "again").expect("snapshot").expect("on");
    assert_eq!(again, first, "same tree, same commit: no new object");
    std::fs::write(root.join("a.txt"), "changed again\n").expect("edit");
    let moved = snapshot(&root, Some(&first), "moved").expect("snapshot").expect("on");
    assert_ne!(moved.commit, first.commit, "a changed tree is a new commit");
}

#[test]
fn a_turn_gets_a_pre_and_a_post_ref() {
    let scratch = Scratch::new("refs");
    let root = repo(&scratch);
    let mut recorder = Recorder::new(Some(root.clone()));
    let job = recorder.turn_started("turn-1");
    drain(&mut recorder, job, "uuid-1");
    std::fs::write(root.join("a.txt"), "edited by the agent\n").expect("edit");
    let job = recorder.turn_ended("turn-1");
    drain(&mut recorder, job, "uuid-1");
    assert!(recorder.idle());
    assert_eq!(recorder.key(), Some("uuid-1"));
    let refs = session_refs(&root, "uuid-1").expect("refs read");
    let turn = refs.get("turn-1").expect("the turn has checkpoints");
    let (pre, post) = (turn.pre.clone().expect("pre"), turn.post.clone().expect("post"));
    assert_eq!(run(&root, &["rev-parse", "refs/baaz/uuid-1/pre/turn-1"]), pre);
    assert_eq!(run(&root, &["rev-parse", "refs/baaz/uuid-1/turn-1"]), post);
    let files = diff_files(&root, &pre, &post).expect("diff");
    assert_eq!(names(&files), vec![("a.txt".to_owned(), ChangeKind::Modified, 1, 3)]);
    assert!(run(&root, &["branch", "--list"]).lines().all(|line| !line.contains("baaz")), "no branch shows them");
}

#[test]
fn two_turns_editing_one_file_show_two_different_diffs() {
    let scratch = Scratch::new("two-turns");
    let root = repo(&scratch);
    let mut recorder = Recorder::new(Some(root.clone()));
    let key = "uuid-2";
    let job = recorder.turn_started("t1");
    drain(&mut recorder, job, key);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\n").expect("turn 1 edit");
    let job = recorder.turn_ended("t1");
    drain(&mut recorder, job, key);
    let job = recorder.turn_started("t2");
    drain(&mut recorder, job, key);
    std::fs::write(root.join("a.txt"), "ONE\ntwo\nthree\nfour\n").expect("turn 2 edit");
    let job = recorder.turn_ended("t2");
    drain(&mut recorder, job, key);

    let refs = session_refs(&root, key).expect("refs");
    let diff_of = |turn: &str| {
        let refs = refs.get(turn).expect("checkpoints");
        let (pre, post) = (refs.pre.as_deref().expect("pre"), refs.post.as_deref().expect("post"));
        diff_file(&root, pre, post, "a.txt").expect("a diff")
    };
    let (one, two) = (diff_of("t1"), diff_of("t2"));
    let lines = |diff: &Diff| -> Vec<(aui_protocol::DiffKind, String)> {
        diff.hunks.iter().flat_map(|hunk| hunk.lines.iter()).filter(|line| line.kind != aui_protocol::DiffKind::Context).map(|line| (line.kind, line.text.clone())).collect()
    };
    assert_eq!(lines(&one), vec![(aui_protocol::DiffKind::Add, "four".to_owned())], "turn 1 added a line");
    assert_eq!(
        lines(&two),
        vec![(aui_protocol::DiffKind::Del, "one".to_owned()), (aui_protocol::DiffKind::Add, "ONE".to_owned())],
        "turn 2 changed the first line"
    );
    assert_ne!(lines(&one), lines(&two));
    // Turn 2 started on turn 1's tree: its pre is turn 1's post.
    assert_eq!(refs["t2"].pre, refs["t1"].post, "an unchanged tree reuses the commit");
}

#[test]
fn edits_made_between_turns_are_not_in_the_next_turns_diff() {
    let scratch = Scratch::new("between");
    let root = repo(&scratch);
    let mut recorder = Recorder::new(Some(root.clone()));
    let key = "uuid-3";
    let job = recorder.turn_started("t1");
    drain(&mut recorder, job, key);
    std::fs::write(root.join("agent1.txt"), "agent\n").expect("turn 1");
    let job = recorder.turn_ended("t1");
    drain(&mut recorder, job, key);
    // The person, between the turns.
    std::fs::write(root.join("mine.txt"), "mine\n").expect("person");
    let job = recorder.turn_started("t2");
    drain(&mut recorder, job, key);
    std::fs::write(root.join("agent2.txt"), "agent\n").expect("turn 2");
    let job = recorder.turn_ended("t2");
    drain(&mut recorder, job, key);

    let refs = session_refs(&root, key).expect("refs");
    let t2 = &refs["t2"];
    let files = diff_files(&root, t2.pre.as_deref().expect("pre"), t2.post.as_deref().expect("post")).expect("diff");
    assert_eq!(names(&files), vec![("agent2.txt".to_owned(), ChangeKind::Added, 1, 0)], "the person's file is not the agent's");
    assert_ne!(t2.pre, refs["t1"].post, "the person's edit made a new pre");
    // The whole session, from the first pre to the working tree, has all three.
    let now = write_tree(&root).expect("tree");
    let session = diff_files(&root, refs["t1"].pre.as_deref().expect("pre"), &now).expect("diff");
    let paths: Vec<&str> = session.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, vec!["agent1.txt", "agent2.txt", "mine.txt"]);
}

#[test]
fn a_session_forgotten_takes_its_refs_and_nothing_else() {
    let scratch = Scratch::new("cleanup");
    let root = repo(&scratch);
    for key in ["gone", "kept"] {
        let mut recorder = Recorder::new(Some(root.clone()));
        let job = recorder.turn_started("t1");
        drain(&mut recorder, job, key);
        let job = recorder.turn_ended("t1");
        drain(&mut recorder, job, key);
    }
    let head = run(&root, &["rev-parse", "HEAD"]);
    run(&root, &["update-ref", "refs/baaz/preserved/gone", &head]);
    assert_eq!(delete_session_refs(&root, "gone").expect("deleted"), 2, "its pre and post");
    assert!(session_refs(&root, "gone").expect("read").is_empty());
    assert_eq!(session_refs(&root, "kept").expect("read").len(), 1, "another session's stay");
    assert_eq!(run(&root, &["rev-parse", "refs/baaz/preserved/gone"]), head, "a worktree's preserved ref is not a checkpoint");
    assert_eq!(delete_session_refs(&root, "gone").expect("again"), 0, "a second delete finds nothing");
}

#[test]
fn a_folder_that_is_not_git_takes_no_checkpoint_and_raises_no_error() {
    let scratch = Scratch::new("plain");
    let plain = scratch.0.join("plain");
    std::fs::create_dir_all(&plain).expect("plain");
    std::fs::write(plain.join("x.txt"), "x\n").expect("x");
    assert!(!supported(&plain));
    assert_eq!(snapshot(&plain, None, "test"), Ok(None));
    let mut recorder = Recorder::new(Some(plain.clone()));
    let job = recorder.turn_started("t1");
    drain(&mut recorder, job, "k");
    assert!(!recorder.on(), "found unsupported once, asked no more");
    assert!(recorder.turn_ended("t1").is_none());
    assert!(recorder.turn_started("t2").is_none());
    assert!(!plain.join(".git").exists(), "nothing was made");
    assert!(!supported(&scratch.0.join("missing")), "a missing folder takes none");
}

#[test]
fn a_bare_or_sparse_repository_takes_no_checkpoint() {
    let scratch = Scratch::new("refused");
    let bare = scratch.0.join("bare.git");
    std::fs::create_dir_all(&bare).expect("bare");
    run(&bare, &["init", "-q", "--bare"]);
    assert!(!supported(&bare));
    let root = repo(&scratch);
    run(&root, &["config", "core.sparseCheckout", "true"]);
    assert_eq!(snapshot(&root, None, "test"), Ok(None));
}

#[test]
fn refs_wait_for_the_session_uuid() {
    let scratch = Scratch::new("waiting");
    let root = repo(&scratch);
    let mut recorder = Recorder::new(Some(root.clone()));
    let job = recorder.turn_started("t1").expect("a job");
    let done = run_job(job, || None);
    assert_eq!(done.waiting.len(), 1, "no uuid yet: the pre waits");
    assert!(recorder.job_done(done).is_none());
    std::fs::write(root.join("a.txt"), "edit\n").expect("edit");
    let job = recorder.turn_ended("t1").expect("a job");
    assert_eq!(job.waiting.len(), 1, "the waiting ref rides the next job");
    let done = run_job(job, || Some("late".to_owned()));
    assert_eq!(done.written.len(), 2, "both refs land once the uuid is known");
    let refs = session_refs(&root, "late").expect("refs");
    assert!(refs["t1"].pre.is_some() && refs["t1"].post.is_some());
}

#[test]
fn a_turn_id_becomes_one_safe_ref_component() {
    assert_eq!(component("msg_01ABC-x"), "msg_01ABC-x");
    for odd in ["a/b", "a..b", ".lock", "pre", "", "a b:c~^?*[\\"] {
        let c = component(odd);
        assert!(!c.is_empty() && c != PRE && c.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_'), "{odd:?} → {c:?}");
    }
    assert_ne!(component("a/b"), component("a:b"), "two ids never meet");
    assert_eq!(pre_ref("k", "t"), "refs/baaz/k/pre/t");
    assert_eq!(turn_ref("k", "t"), "refs/baaz/k/t");
}
