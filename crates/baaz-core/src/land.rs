//! `task_land` (docs/85 14.8, D-Q6 idea 8): merge finished tasks' branches
//! into the lead's branch one at a time, with the check after each.
//!
//! * The project must be clean (no tracked changes) before anything moves.
//! * Each branch's worktree is saved first ([`crate::worktrees::archive`]:
//!   its work to `refs/baaz/preserved/<task id>`, the folder removed, the
//!   branch kept), so nothing a worker did is lost whatever happens next.
//! * A merge is made with `--no-commit`; the check runs on the merged tree;
//!   only a passing check commits it. A conflict or a failing check aborts
//!   that merge (`git merge --abort`: history is never rewritten) and stops,
//!   naming the conflicted files or the check's tail. Never resolved here.
//! * After a clean land the merged branch is deleted; the saved ref stays.

use std::path::Path;
use std::time::Duration;

use crate::checks::Check;

/// One branch to land.
#[derive(Clone, Debug)]
pub struct Item {
    pub task_id: String,
    pub branch: String,
    /// Its worktree, when one of Baaz's.
    pub worktree: Option<crate::worktrees::Worktree>,
}

/// What happened to one item. The run stops at the first that is not
/// [`Step::Landed`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Landed { task_id: String, branch: String, saved: Option<String> },
    Conflict { task_id: String, files: Vec<String> },
    CheckFailed { task_id: String, tail: String },
    Refused { task_id: String, why: String },
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("git").current_dir(dir).args(args).output().map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Land `items` into `root`'s checked-out branch (blocking).
pub fn land(state_dir: &Path, root: &Path, items: &[Item], check: &Check, deadline: Duration) -> Vec<Step> {
    let mut out = Vec::new();
    let dirty = git(root, &["status", "--porcelain", "--untracked-files=no"]).map(|s| !s.is_empty()).unwrap_or(true);
    if dirty {
        if let Some(first) = items.first() {
            out.push(Step::Refused { task_id: first.task_id.clone(), why: "the project has uncommitted changes; commit or stash them first".into() });
        }
        return out;
    }
    for item in items {
        // Save the work before anything moves.
        let saved = match &item.worktree {
            Some(worktree) => match crate::worktrees::archive(state_dir, worktree, &item.task_id) {
                Ok(archived) => archived.snapshot.and_then(|s| s.reference),
                Err(why) => {
                    out.push(Step::Refused { task_id: item.task_id.clone(), why: format!("its worktree could not be saved: {why}") });
                    return out;
                }
            },
            None => None,
        };
        if let Err(why) = git(root, &["merge", "--no-ff", "--no-commit", &item.branch]) {
            let files: Vec<String> = git(root, &["diff", "--name-only", "--diff-filter=U"]).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default();
            let _ = git(root, &["merge", "--abort"]);
            if files.is_empty() {
                out.push(Step::Refused { task_id: item.task_id.clone(), why: format!("git refused the merge: {why}") });
            } else {
                out.push(Step::Conflict { task_id: item.task_id.clone(), files });
            }
            return out;
        }
        let (passed, tail) = match check {
            Check::Command(command) => {
                let outcome = crate::checks::run(root, command, deadline);
                (outcome.passed, outcome.tail)
            }
            Check::Parse => {
                let changed: Vec<String> = git(root, &["diff", "--cached", "--name-only"]).map(|s| s.lines().map(str::to_owned).collect()).unwrap_or_default();
                match crate::checks::parse_files(root, &changed) {
                    Ok(()) => (true, String::new()),
                    Err(why) => (false, why),
                }
            }
        };
        if !passed {
            let _ = git(root, &["merge", "--abort"]);
            out.push(Step::CheckFailed { task_id: item.task_id.clone(), tail });
            return out;
        }
        let message = format!("Land {} (task {})", item.branch, item.task_id);
        if let Err(why) = git(root, &["-c", "user.name=Baaz", "-c", "user.email=baaz@localhost", "commit", "--no-edit", "-m", &message]) {
            let _ = git(root, &["merge", "--abort"]);
            out.push(Step::Refused { task_id: item.task_id.clone(), why: format!("the merge could not be committed: {why}") });
            return out;
        }
        let _ = git(root, &["branch", "-d", &item.branch]);
        out.push(Step::Landed { task_id: item.task_id.clone(), branch: item.branch.clone(), saved });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-land-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]).unwrap();
        git(&dir, &["config", "user.email", "t@example.invalid"]).unwrap();
        git(&dir, &["config", "user.name", "T"]).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        git(&dir, &["add", "-A"]).unwrap();
        git(&dir, &["commit", "-qm", "init"]).unwrap();
        dir
    }

    fn branch(dir: &Path, name: &str, file: &str, text: &str) {
        git(dir, &["checkout", "-q", "-b", name, "main"]).unwrap();
        std::fs::write(dir.join(file), text).unwrap();
        git(dir, &["add", "-A"]).unwrap();
        git(dir, &["commit", "-qm", name]).unwrap();
        git(dir, &["checkout", "-q", "main"]).unwrap();
    }

    fn item(id: &str, branch: &str) -> Item {
        Item { task_id: id.into(), branch: branch.into(), worktree: None }
    }

    /// docs/85 14.8 on a temp repository: two branches land one at a time
    /// and their branches go; a conflict stops with the file named and
    /// leaves main as it was; a failing check aborts its merge.
    #[test]
    fn branches_land_one_at_a_time_and_a_conflict_stops_with_the_files() {
        let dir = repo();
        let state = dir.join(".state");
        branch(&dir, "baaz/t1", "b.txt", "b\n");
        branch(&dir, "baaz/t2", "c.txt", "c\n");
        let steps = land(&state, &dir, &[item("t1", "baaz/t1"), item("t2", "baaz/t2")], &Check::Command("test -f a.txt".into()), Duration::from_secs(30));
        assert!(matches!(steps.as_slice(), [Step::Landed { .. }, Step::Landed { .. }]), "{steps:?}");
        assert!(dir.join("b.txt").exists() && dir.join("c.txt").exists());
        assert!(git(&dir, &["rev-parse", "--verify", "baaz/t1"]).is_err(), "a landed branch is deleted");

        branch(&dir, "baaz/t3", "a.txt", "three\n");
        branch(&dir, "baaz/t4", "a.txt", "four\n");
        let head = git(&dir, &["rev-parse", "HEAD"]).unwrap();
        let steps = land(&state, &dir, &[item("t3", "baaz/t3"), item("t4", "baaz/t4")], &Check::Parse, Duration::from_secs(30));
        assert!(matches!(&steps[0], Step::Landed { .. }));
        assert_eq!(steps[1], Step::Conflict { task_id: "t4".into(), files: vec!["a.txt".into()] });
        assert!(git(&dir, &["status", "--porcelain", "--untracked-files=no"]).unwrap().is_empty(), "the conflict was aborted, never resolved");
        assert_ne!(git(&dir, &["rev-parse", "HEAD"]).unwrap(), head);
        assert!(git(&dir, &["rev-parse", "--verify", "baaz/t4"]).is_ok(), "an unlanded branch stays");

        branch(&dir, "baaz/t5", "d.txt", "d\n");
        let before = git(&dir, &["rev-parse", "HEAD"]).unwrap();
        let steps = land(&state, &dir, &[item("t5", "baaz/t5")], &Check::Command("false".into()), Duration::from_secs(30));
        assert!(matches!(&steps[0], Step::CheckFailed { .. }));
        assert_eq!(git(&dir, &["rev-parse", "HEAD"]).unwrap(), before, "a failing check commits nothing");
        assert!(!dir.join("d.txt").exists());

        std::fs::write(dir.join("a.txt"), "dirty\n").unwrap();
        assert!(matches!(land(&state, &dir, &[item("t5", "baaz/t5")], &Check::Parse, Duration::from_secs(30))[0], Step::Refused { .. }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
