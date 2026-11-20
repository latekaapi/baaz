//! Undo in temp repos: git only, never the network, each test its own
//! folder.

use super::*;
use std::collections::BTreeMap;
use std::process::{Command, Stdio};

/// A temp directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("baaz-undo-{tag}-{}-{n}", std::process::id()));
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

/// A repo with one commit: `a.txt` (five lines), `b.txt`, a binary
/// `logo.bin`, and `.gitignore` ignoring `*.log`.
fn repo(scratch: &Scratch) -> PathBuf {
    let root = scratch.0.join("repo");
    std::fs::create_dir_all(&root).expect("root");
    run(&root, &["init", "-q", "-b", "main"]);
    run(&root, &["config", "user.email", "t@example.com"]);
    run(&root, &["config", "user.name", "T"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\nfive\n").expect("a.txt");
    std::fs::write(root.join("b.txt"), "bee\n").expect("b.txt");
    std::fs::write(root.join("logo.bin"), binary(1)).expect("logo.bin");
    std::fs::write(root.join(".gitignore"), "*.log\n").expect(".gitignore");
    run(&root, &["add", "-A"]);
    run(&root, &["commit", "-q", "-m", "first"]);
    root
}

/// Bytes git treats as binary (a NUL), different per `seed`.
fn binary(seed: u8) -> Vec<u8> {
    (0..600u32).map(|i| ((i as u8).wrapping_mul(seed.wrapping_add(7))) ^ seed).chain([0, 0, seed]).collect()
}

/// Every file under `root` but `.git`, with its bytes.
fn bytes(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.file_name() == Some(std::ffi::OsStr::new(".git")) {
                continue;
            }
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).expect("under root").to_string_lossy().into_owned();
                out.insert(rel, std::fs::read(&path).expect("read"));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// One turn: snapshot, `edit`, snapshot, both refs written as the
/// recorder would. Returns `(pre, post)`.
fn turn(root: &Path, key: &str, id: &str, edit: impl FnOnce(&Path)) -> (String, String) {
    let pre = checkpoints::snapshot(root, None, "pre").expect("pre").expect("supported");
    edit(root);
    let post = checkpoints::snapshot(root, None, "post").expect("post").expect("supported");
    checkpoints::set_ref(root, &checkpoints::pre_ref(key, id), &pre.commit).expect("pre ref");
    checkpoints::set_ref(root, &checkpoints::turn_ref(key, id), &post.commit).expect("post ref");
    (pre.commit, post.commit)
}

fn read(root: &Path, path: &str) -> String {
    std::fs::read_to_string(root.join(path)).expect("read")
}

#[test]
fn undoing_the_last_turn_restores_its_files_and_keeps_the_persons_other_edit() {
    let scratch = Scratch::new("last");
    let root = repo(&scratch);
    let before = bytes(&root);
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").expect("edit");
        std::fs::write(root.join("new.txt"), "agent\n").expect("create");
    });
    // The person, after the turn, edits another file.
    std::fs::write(root.join("b.txt"), "mine\n").expect("person");
    let after_person = bytes(&root);

    let applied = undo_turn(&root, "k", &pre, &post).expect("undone");
    assert_eq!(applied.files, vec!["a.txt".to_owned(), "new.txt".to_owned()], "only the turn's files");
    assert_eq!(read(&root, "a.txt"), "one\ntwo\nthree\nfour\nfive\n", "a.txt as before the turn");
    assert!(!root.join("new.txt").exists(), "the file the turn created is gone");
    assert_eq!(read(&root, "b.txt"), "mine\n", "the person's edit stays");
    let mut expected = before.clone();
    expected.insert("b.txt".into(), b"mine\n".to_vec());
    assert_eq!(bytes(&root), expected);
    assert_eq!(run(&root, &["rev-parse", &undo_ref("k", applied.n)]), applied.safety, "the safety snapshot is named");

    // The toast's Undo: exactly the bytes before the action.
    assert_eq!(restore(&root, &applied).expect("restored"), applied.files);
    assert_eq!(bytes(&root), after_person, "every byte as it was before the undo");
}

#[test]
fn an_earlier_turn_whose_lines_a_later_turn_changed_is_refused_and_nothing_changes() {
    let scratch = Scratch::new("conflict");
    let root = repo(&scratch);
    let (pre1, post1) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").expect("t1");
        std::fs::write(root.join("b.txt"), "bee 1\n").expect("t1 b");
    });
    turn(&root, "k", "t2", |root| {
        std::fs::write(root.join("a.txt"), "one\nTWO!\nthree\nfour\nfive\n").expect("t2 same line");
    });
    let state = bytes(&root);
    let refs = run(&root, &["for-each-ref", "refs/baaz/"]);

    let refused = undo_turn(&root, "k", &pre1, &post1).expect_err("refused");
    assert_eq!(refused, Refusal::Conflicts(vec!["a.txt".into()]), "names the file whose lines moved, not b.txt");
    assert_eq!(bytes(&root), state, "nothing changed");
    assert_eq!(run(&root, &["for-each-ref", "refs/baaz/"]), refs, "no safety snapshot left behind");

    // One file of that turn the later turn left alone still reverts.
    let applied = revert_in_turn(&root, "k", &pre1, &post1, "b.txt").expect("b.txt alone applies");
    assert_eq!(read(&root, "b.txt"), "bee\n");
    assert_eq!(read(&root, "a.txt"), "one\nTWO!\nthree\nfour\nfive\n", "a.txt untouched");
    restore(&root, &applied).expect("restored");
    assert_eq!(bytes(&root), state);
}

#[test]
fn an_earlier_turn_undoes_when_a_later_turn_changed_other_lines() {
    let scratch = Scratch::new("earlier");
    let root = repo(&scratch);
    let (pre1, post1) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("a.txt"), "ONE\ntwo\nthree\nfour\nfive\n").expect("t1");
    });
    turn(&root, "k", "t2", |root| {
        std::fs::write(root.join("a.txt"), "ONE\ntwo\nthree\nfour\nFIVE\n").expect("t2 another line");
    });
    undo_turn(&root, "k", &pre1, &post1).expect("applies around the later edit");
    assert_eq!(read(&root, "a.txt"), "one\ntwo\nthree\nfour\nFIVE\n", "turn 1's line back, turn 2's kept");
}

#[test]
fn a_turn_that_created_a_file_is_undone_and_the_undo_taken_back() {
    let scratch = Scratch::new("created");
    let root = repo(&scratch);
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::create_dir_all(root.join("src/deep")).expect("dir");
        std::fs::write(root.join("src/deep/made.rs"), "fn main() {}\n").expect("create");
    });
    let before = bytes(&root);
    let applied = undo_turn(&root, "k", &pre, &post).expect("undone");
    assert!(!root.join("src/deep/made.rs").exists(), "the created file is removed");
    assert!(!root.join("src").exists(), "and the folders it alone made");
    restore(&root, &applied).expect("redone");
    assert_eq!(read(&root, "src/deep/made.rs"), "fn main() {}\n", "Undo brings it back");
    assert_eq!(bytes(&root), before);
}

#[test]
fn a_turn_that_deleted_a_file_is_undone_by_recreating_it() {
    let scratch = Scratch::new("deleted");
    let root = repo(&scratch);
    let original = read(&root, "b.txt");
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::remove_file(root.join("b.txt")).expect("delete");
    });
    let applied = undo_turn(&root, "k", &pre, &post).expect("undone");
    assert_eq!(read(&root, "b.txt"), original, "recreated as it was");
    restore(&root, &applied).expect("redone");
    assert!(!root.join("b.txt").exists(), "Undo deletes it again");
}

#[test]
fn a_binary_file_round_trips() {
    let scratch = Scratch::new("binary");
    let root = repo(&scratch);
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("logo.bin"), binary(9)).expect("edit");
        std::fs::write(root.join("new.bin"), binary(3)).expect("create");
    });
    let after_turn = bytes(&root);
    let applied = undo_turn(&root, "k", &pre, &post).expect("undone");
    assert_eq!(std::fs::read(root.join("logo.bin")).expect("read"), binary(1), "the binary is back byte for byte");
    assert!(!root.join("new.bin").exists());
    restore(&root, &applied).expect("redone");
    assert_eq!(bytes(&root), after_turn);
}

#[test]
fn revert_file_to_head_for_a_tracked_change_and_an_untracked_file() {
    let scratch = Scratch::new("head");
    let root = repo(&scratch);
    std::fs::write(root.join("a.txt"), "changed\n").expect("edit");
    run(&root, &["add", "a.txt"]);
    std::fs::write(root.join("a.txt"), "changed again\n").expect("edit 2");
    std::fs::write(root.join("fresh.txt"), "untracked\n").expect("untracked");
    let before = bytes(&root);

    let tracked = revert_to_head(&root, "k", "a.txt").expect("reverted");
    assert_eq!(read(&root, "a.txt"), "one\ntwo\nthree\nfour\nfive\n", "HEAD's bytes");
    assert_eq!(run(&root, &["status", "--porcelain", "--", "a.txt"]), "", "and nothing staged");
    let untracked = revert_to_head(&root, "k", "fresh.txt").expect("removed");
    assert!(!root.join("fresh.txt").exists(), "a file HEAD lacks is removed");
    assert_eq!(revert_to_head(&root, "k", "b.txt"), Err(Refusal::Nothing), "an unchanged file has nothing to revert");

    restore(&root, &untracked).expect("back");
    restore(&root, &tracked).expect("back");
    assert_eq!(bytes(&root), before, "both Undos give the exact bytes back");
}

#[test]
fn revert_file_to_head_unstages_a_staged_new_file() {
    let scratch = Scratch::new("staged");
    let root = repo(&scratch);
    std::fs::write(root.join("added.txt"), "staged\n").expect("new");
    run(&root, &["add", "added.txt"]);
    let applied = revert_to_head(&root, "k", "added.txt").expect("reverted");
    assert!(!root.join("added.txt").exists());
    assert_eq!(run(&root, &["status", "--porcelain"]), "", "the index lost it too");
    restore(&root, &applied).expect("back");
    assert_eq!(read(&root, "added.txt"), "staged\n", "back as an untracked file");
}

#[test]
fn revert_file_in_a_turn_takes_only_that_files_part() {
    let scratch = Scratch::new("turnfile");
    let root = repo(&scratch);
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\n").expect("a");
        std::fs::write(root.join("b.txt"), "BEE\n").expect("b");
    });
    let before = bytes(&root);
    let applied = revert_in_turn(&root, "k", &pre, &post, "a.txt").expect("reverted");
    assert_eq!(applied.files, vec!["a.txt".to_owned()]);
    assert_eq!(read(&root, "a.txt"), "one\ntwo\nthree\nfour\nfive\n");
    assert_eq!(read(&root, "b.txt"), "BEE\n", "the turn's other file stays");
    assert_eq!(revert_in_turn(&root, "k", &pre, &post, "logo.bin"), Err(Refusal::Nothing), "a file the turn did not change");
    restore(&root, &applied).expect("back");
    assert_eq!(bytes(&root), before);
}

#[test]
fn undo_of_an_undo_is_refused_once_the_file_changed_again() {
    let scratch = Scratch::new("moved");
    let root = repo(&scratch);
    let (pre, post) = turn(&root, "k", "t1", |root| {
        std::fs::write(root.join("a.txt"), "agent\n").expect("a");
    });
    let applied = undo_turn(&root, "k", &pre, &post).expect("undone");
    std::fs::write(root.join("a.txt"), "the person typed here\n").expect("person");
    assert_eq!(restore(&root, &applied), Err(Refusal::Conflicts(vec!["a.txt".into()])));
    assert_eq!(read(&root, "a.txt"), "the person typed here\n", "their words stay");
}

#[test]
fn safety_snapshots_are_capped_and_go_with_the_session() {
    let scratch = Scratch::new("cap");
    let root = repo(&scratch);
    for i in 0..(KEEP + 2) {
        std::fs::write(root.join("a.txt"), format!("edit {i}\n")).expect("edit");
        revert_to_head(&root, "k", "a.txt").expect("reverted");
    }
    let kept = run(&root, &["for-each-ref", "--format=%(refname)", "refs/baaz/k/undo/"]);
    assert_eq!(kept.lines().count(), KEEP, "the newest KEEP stay");
    let kept: Vec<&str> = kept.lines().collect();
    assert!(kept.contains(&undo_ref("k", (KEEP + 2) as u64).as_str()) && !kept.contains(&undo_ref("k", 1).as_str()), "the oldest go");
    assert!(checkpoints::session_refs(&root, "k").expect("read").is_empty(), "safety snapshots are not turns");
    assert_eq!(checkpoints::delete_session_refs(&root, "k").expect("deleted"), KEEP);
}

#[test]
fn a_turn_that_changed_nothing_has_nothing_to_undo() {
    let scratch = Scratch::new("nothing");
    let root = repo(&scratch);
    let (pre, post) = turn(&root, "k", "t1", |_| {});
    assert_eq!(undo_turn(&root, "k", &pre, &post), Err(Refusal::Nothing));
    assert_eq!(turn_files(&root, &pre, &post), Ok(Vec::new()));
}

/// docs/82 7.2: All changes offers Revert on every row kind — a deleted
/// file comes back, and a renamed file's row (its new path) puts the
/// original back under its old name, the index clean.
#[test]
fn revert_file_to_head_for_a_deleted_and_a_renamed_file() {
    let scratch = Scratch::new("kinds");
    let root = repo(&scratch);
    std::fs::remove_file(root.join("b.txt")).expect("delete");
    let deleted = revert_to_head(&root, "k", "b.txt").expect("reverted");
    assert_eq!(read(&root, "b.txt"), "bee\n", "the deleted file is back");
    run(&root, &["mv", "a.txt", "moved.txt"]);
    let renamed = revert_to_head(&root, "k", "moved.txt").expect("reverted");
    assert!(!root.join("moved.txt").exists(), "the new name is gone");
    assert_eq!(read(&root, "a.txt"), "one\ntwo\nthree\nfour\nfive\n", "the old name is back");
    assert_eq!(run(&root, &["status", "--porcelain"]), "", "nothing left staged or changed");
    restore(&root, &renamed).expect("back");
    assert!(root.join("moved.txt").exists() && !root.join("a.txt").exists(), "Undo puts the rename back");
    restore(&root, &deleted).expect("back");
    assert!(!root.join("b.txt").exists());
}
