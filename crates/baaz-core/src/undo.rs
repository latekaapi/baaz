//! Undo (docs/82 6.4): put files back from a session's turn checkpoints
//! ([`crate::checkpoints`]), and take that back again.
//!
//! Three actions, each checked before it changes anything and each
//! undoable:
//!
//! * [`undo_turn`] — reverse-apply one finished turn's own change
//!   (`pre/<turn>..<turn>`, binary-safe) to the folder. Only the files that
//!   turn changed are touched, so the person's edits to other files and a
//!   later turn's edits to other files stay. When the reverse does not apply
//!   cleanly (a later turn or the person changed the same lines since) it is
//!   refused, naming the files, and nothing changes.
//! * [`revert_in_turn`] — the same for one file of that turn.
//! * [`revert_to_head`] — one file back to `HEAD` (a file `HEAD` does not
//!   have is removed, and unstaged when it was staged; a staged rename's
//!   old path comes back with it).
//!
//! Before any of them changes a file, the folder is snapshotted (the
//! checkpoints' own [`crate::checkpoints::write_tree`], untracked files
//! included) and named `refs/baaz/<uuid>/undo/<n>`; the newest [`KEEP`]
//! stay, and they go with the session's other refs when it is forgotten.
//! The [`Applied`] that comes back is what [`restore`] — the toast's Undo —
//! needs: it writes exactly the touched files back from that snapshot
//! (recreating what the action deleted, removing what it created), and
//! only while each still holds what the action left; otherwise it is
//! refused the same way.
//!
//! What is restored is the working tree. A file [`revert_to_head`] also
//! unstaged comes back as an unstaged change: the snapshot holds files,
//! not the index.
//!
//! Paths are relative to the repository's top (as git reports them), and
//! every git command runs there. Every function blocks on git: callers run
//! it off the UI thread. No gpui.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::checkpoints::{self, component, PREFIX, UNDO};

/// How many safety snapshots a session keeps: the newest, by number.
pub const KEEP: usize = 8;
/// How long one git command of an action may take.
const TIMEOUT: Duration = Duration::from_secs(60);

/// What an action did: enough for [`restore`] to take it back.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Applied {
    /// Its number under `refs/baaz/<uuid>/undo/`.
    pub n: u64,
    /// The snapshot taken before it (a commit).
    pub safety: String,
    /// The files it touched, top-relative, sorted.
    pub files: Vec<String>,
    /// Each of `files` as it left it: its blob id, `None` when absent.
    pub after: Vec<Option<String>>,
}

/// Why an action changed nothing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// It does not apply cleanly: these files changed since.
    Conflicts(Vec<String>),
    /// There is nothing to put back (the turn changed no files, the file
    /// has no change).
    Nothing,
    /// These files are not in the safety snapshot (ignored by git), so
    /// nothing could bring them back afterwards.
    Unprotected(Vec<String>),
    /// Git could not be run, or failed.
    Failed(String),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Conflicts(files) => write!(f, "changed since: {}", files.join(", ")),
            Refusal::Nothing => write!(f, "nothing to put back"),
            Refusal::Unprotected(files) => write!(f, "ignored by git: {}", files.join(", ")),
            Refusal::Failed(why) => write!(f, "{why}"),
        }
    }
}

fn failed(error: String) -> Refusal {
    Refusal::Failed(error)
}

fn git(top: &Path, args: &[&str]) -> Result<String, Refusal> {
    checkpoints::git_in(top, args, &[], None, TIMEOUT).map_err(failed)
}

/// `git --literal-pathspecs <args> -- <paths>`: a path is a path, never a
/// glob.
fn git_paths(top: &Path, args: &[&str], paths: &[String]) -> Result<String, Refusal> {
    let mut all: Vec<&str> = vec!["--literal-pathspecs"];
    all.extend_from_slice(args);
    all.push("--");
    all.extend(paths.iter().map(String::as_str));
    git(top, &all)
}

/// The repository's top for `dir`.
fn top(dir: &Path) -> Result<PathBuf, Refusal> {
    git(dir, &["rev-parse", "--show-toplevel"]).map(PathBuf::from)
}

/// `refs/baaz/<key>/undo/<n>`.
pub fn undo_ref(key: &str, n: u64) -> String {
    format!("{PREFIX}{}/{UNDO}/{n}", component(key))
}

/// The session's safety snapshot numbers, ascending.
fn numbers(top: &Path, key: &str) -> Result<Vec<u64>, Refusal> {
    let prefix = format!("{PREFIX}{}/{UNDO}/", component(key));
    let raw = git(top, &["for-each-ref", "--format=%(refname)", &prefix])?;
    let mut out: Vec<u64> = raw.lines().filter_map(|name| name.trim().strip_prefix(&prefix)?.parse().ok()).collect();
    out.sort_unstable();
    Ok(out)
}

/// Drop all but the newest `keep` safety snapshots of `key`.
pub fn prune(dir: &Path, key: &str, keep: usize) -> Result<usize, Refusal> {
    let top = top(dir)?;
    let numbers = numbers(&top, key)?;
    let old = numbers.len().saturating_sub(keep);
    if old == 0 {
        return Ok(0);
    }
    let script: String = numbers[..old].iter().map(|n| format!("delete {}\n", undo_ref(key, *n))).collect();
    checkpoints::git_in(&top, &["update-ref", "--stdin"], &[], Some(&script), TIMEOUT).map_err(failed)?;
    Ok(old)
}

/// Which of `paths` the commit `rev` holds.
fn held_by(top: &Path, rev: &str, paths: &[String]) -> Result<HashSet<String>, Refusal> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    let raw = git_paths(top, &["ls-tree", "-r", "-z", "--name-only", rev], paths)?;
    Ok(raw.split('\0').filter(|p| !p.is_empty()).map(str::to_owned).collect())
}

/// Whether `path` is there (a file, or a link: never followed).
fn present(top: &Path, path: &str) -> bool {
    std::fs::symlink_metadata(top.join(path)).is_ok()
}

/// Each of `paths` as it is now: its blob id, `None` when absent.
fn hashes(top: &Path, paths: &[String]) -> Result<Vec<Option<String>>, Refusal> {
    let there: Vec<&String> = paths.iter().filter(|p| present(top, p)).collect();
    let mut ids = Vec::new();
    if !there.is_empty() {
        let mut args = vec!["hash-object", "--"];
        args.extend(there.iter().map(|p| p.as_str()));
        ids = git(top, &args)?.lines().map(str::to_owned).collect();
    }
    if ids.len() != there.len() {
        return Err(failed("git hash-object answered for the wrong number of files".into()));
    }
    let mut ids = ids.into_iter();
    Ok(paths.iter().map(|p| if present(top, p) { ids.next() } else { None }).collect())
}

/// Remove `path` and then any folder it leaves empty, up to the top.
fn remove(top: &Path, path: &str) -> Result<(), Refusal> {
    let full = top.join(path);
    match std::fs::remove_file(&full) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(format!("{path}: {error}"))),
    }
    let mut parent = full.parent();
    while let Some(folder) = parent.filter(|folder| *folder != top && folder.starts_with(top)) {
        if std::fs::remove_dir(folder).is_err() {
            break;
        }
        parent = folder.parent();
    }
    Ok(())
}

/// The binary-safe patch from `from` to `to` over `paths`, written to a
/// file in the git directory (the caller removes it). Prefixes are spelt
/// out so a `diff.noprefix` setting cannot change the patch's shape.
fn patch(top: &Path, from: &str, to: &str, paths: &[String]) -> Result<PathBuf, Refusal> {
    let name = format!("baaz-undo-{}.patch", uuid::Uuid::new_v4().simple());
    let file = PathBuf::from(git(top, &["rev-parse", "--git-path", &name])?);
    let file = if file.is_absolute() { file } else { top.join(file) };
    let output = format!("--output={}", file.display());
    git_paths(
        top,
        &[
            "diff",
            "--binary",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &output,
            from,
            to,
        ],
        paths,
    )?;
    Ok(file)
}

/// Reverse-apply `from..to` over `paths` to the working tree: checked
/// first, applied only when the whole of it applies. A failed check names
/// the files whose own part does not apply.
fn reverse_apply(top: &Path, from: &str, to: &str, paths: &[String], check_only: bool) -> Result<(), Refusal> {
    let file = patch(top, from, to, paths)?;
    let path = file.to_string_lossy().into_owned();
    let check = git(top, &["apply", "-R", "--check", "--whitespace=nowarn", &path]);
    let result = match check {
        Ok(_) if check_only => Ok(()),
        Ok(_) => git(top, &["apply", "-R", "--whitespace=nowarn", &path]).map(drop),
        Err(_) => {
            let _ = std::fs::remove_file(&file);
            return Err(Refusal::Conflicts(conflicting(top, from, to, paths)));
        }
    };
    let _ = std::fs::remove_file(&file);
    result
}

/// The files of `paths` whose own reverse does not apply.
fn conflicting(top: &Path, from: &str, to: &str, paths: &[String]) -> Vec<String> {
    let mut out = Vec::new();
    for path in paths {
        let one = std::slice::from_ref(path);
        let fits = patch(top, from, to, one).is_ok_and(|file| {
            let ok = git(top, &["apply", "-R", "--check", "--whitespace=nowarn", &file.to_string_lossy()]).is_ok();
            let _ = std::fs::remove_file(&file);
            ok
        });
        if !fits {
            out.push(path.clone());
        }
    }
    // A patch that fails as a whole always has a part that fails alone;
    // if none did, name them all rather than none.
    if out.is_empty() {
        out = paths.to_vec();
    }
    out
}

/// The files `from..to` changes, sorted.
fn changed(top: &Path, from: &str, to: &str) -> Result<Vec<String>, Refusal> {
    let files = checkpoints::diff_files(top, from, to).ok_or_else(|| failed("git could not read the turn's change".into()))?;
    Ok(files.into_iter().map(|f| f.path).collect())
}

/// Snapshot the folder as `refs/baaz/<key>/undo/<n>` before `files` are
/// touched, refusing when one of them exists but is not in it (ignored),
/// then run `act` and record what it left. A failed `act` drops the ref.
fn guarded(top: &Path, key: &str, files: Vec<String>, act: impl FnOnce(&Path) -> Result<(), Refusal>) -> Result<Applied, Refusal> {
    let tree = checkpoints::write_tree(top).map_err(failed)?;
    let held = held_by(top, &tree, &files)?;
    let bare: Vec<String> = files.iter().filter(|p| present(top, p) && !held.contains(*p)).cloned().collect();
    if !bare.is_empty() {
        return Err(Refusal::Unprotected(bare));
    }
    let n = numbers(top, key)?.last().map_or(1, |last| last + 1);
    let safety = checkpoints::commit_tree(top, &tree, checkpoints::head(top).as_deref(), &format!("baaz: before undo {n}")).map_err(failed)?;
    let name = undo_ref(key, n);
    checkpoints::set_ref(top, &name, &safety).map_err(failed)?;
    if let Err(refusal) = act(top) {
        let _ = git(top, &["update-ref", "-d", &name]);
        return Err(refusal);
    }
    let after = hashes(top, &files)?;
    if let Err(error) = prune(top, key, KEEP) {
        crate::baaz_log!("undo: old safety snapshots not pruned ({error})");
    }
    Ok(Applied { n, safety, files, after })
}

/// The files the turn `pre..post` changed: what [`undo_turn`] would put
/// back (the confirm's list).
pub fn turn_files(dir: &Path, pre: &str, post: &str) -> Result<Vec<String>, Refusal> {
    changed(&top(dir)?, pre, post)
}

/// Undo one finished turn: reverse-apply `pre..post` to the folder of
/// `dir`, its files only. Refused, changing nothing, when it does not
/// apply cleanly.
pub fn undo_turn(dir: &Path, key: &str, pre: &str, post: &str) -> Result<Applied, Refusal> {
    let top = top(dir)?;
    let files = changed(&top, pre, post)?;
    if files.is_empty() {
        return Err(Refusal::Nothing);
    }
    reverse_apply(&top, pre, post, &files, true)?;
    let paths = files.clone();
    guarded(&top, key, files, |top| reverse_apply(top, pre, post, &paths, false))
}

/// Revert one file's part of the turn `pre..post`. Refused like
/// [`undo_turn`]; [`Refusal::Nothing`] when the turn did not change it.
pub fn revert_in_turn(dir: &Path, key: &str, pre: &str, post: &str, path: &str) -> Result<Applied, Refusal> {
    let top = top(dir)?;
    if !changed(&top, pre, post)?.iter().any(|p| p == path) {
        return Err(Refusal::Nothing);
    }
    let files = vec![path.to_owned()];
    reverse_apply(&top, pre, post, &files, true)?;
    let paths = files.clone();
    guarded(&top, key, files, |top| reverse_apply(top, pre, post, &paths, false))
}

/// Revert one file to `HEAD`: its content and its index entry go back to
/// `HEAD`'s; a file `HEAD` does not have is removed (and unstaged). A
/// staged rename's row is its new path: its old path goes back too, so the
/// rename is undone whole (docs/82 7.2), not left as a deletion.
/// [`Refusal::Nothing`] when the file already matches `HEAD`.
pub fn revert_to_head(dir: &Path, key: &str, path: &str) -> Result<Applied, Refusal> {
    let top = top(dir)?;
    let mut files = vec![path.to_owned()];
    let head = checkpoints::head(&top);
    let status = git_paths(&top, &["status", "--porcelain=v1", "-z", "--untracked-files=all"], &files)?;
    if status.trim_matches('\0').is_empty() {
        return Err(Refusal::Nothing);
    }
    if let Some(head) = &head {
        files.extend(rename_source(&top, head, path)?);
    }
    let in_head = match &head {
        Some(head) => held_by(&top, head, &files)?,
        None => HashSet::new(),
    };
    let staged: HashSet<String> = git_paths(&top, &["ls-files", "-z"], &files)?.split('\0').filter(|p| !p.is_empty()).map(str::to_owned).collect();
    guarded(&top, key, files.clone(), move |top| {
        let (back, gone): (Vec<String>, Vec<String>) = files.iter().cloned().partition(|p| in_head.contains(p));
        if !back.is_empty() {
            git_paths(top, &["restore", "--source=HEAD", "--staged", "--worktree"], &back)?;
        }
        let unstage: Vec<String> = gone.iter().filter(|p| staged.contains(*p)).cloned().collect();
        if !unstage.is_empty() {
            git_paths(top, &["rm", "--cached", "-f", "-q"], &unstage)?;
        }
        for path in &gone {
            remove(top, path)?;
        }
        Ok(())
    })
}

/// The old path of a staged rename whose new path is `path`, if any.
fn rename_source(top: &Path, head: &str, path: &str) -> Result<Option<String>, Refusal> {
    let raw = git(top, &["diff", "--cached", "--name-status", "-M", "-z", head])?;
    let mut fields = raw.split('\0');
    while let Some(status) = fields.next() {
        if status.starts_with('R') || status.starts_with('C') {
            let (Some(old), Some(new)) = (fields.next(), fields.next()) else { break };
            if new == path && status.starts_with('R') {
                return Ok(Some(old.to_owned()));
            }
        } else if fields.next().is_none() {
            break;
        }
    }
    Ok(None)
}

/// Take an action back: each touched file as the safety snapshot held it
/// (a file it did not hold is removed). Refused, changing nothing, when a
/// file no longer holds what the action left. Returns the files.
pub fn restore(dir: &Path, applied: &Applied) -> Result<Vec<String>, Refusal> {
    let top = top(dir)?;
    let now = hashes(&top, &applied.files)?;
    let moved: Vec<String> = applied.files.iter().zip(now.iter().zip(&applied.after)).filter(|(_, (now, after))| now != after).map(|(p, _)| p.clone()).collect();
    if !moved.is_empty() {
        return Err(Refusal::Conflicts(moved));
    }
    let held = held_by(&top, &applied.safety, &applied.files)?;
    let (back, gone): (Vec<String>, Vec<String>) = applied.files.iter().cloned().partition(|p| held.contains(p));
    if !back.is_empty() {
        let source = format!("--source={}", applied.safety);
        git_paths(&top, &["restore", &source, "--worktree"], &back)?;
    }
    for path in &gone {
        remove(&top, path)?;
    }
    Ok(applied.files.clone())
}

#[cfg(test)]
mod tests;
