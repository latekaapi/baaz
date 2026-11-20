//! Checkpoints: a hidden ref per turn (docs/82 6.3).
//!
//! When a turn starts and when it ends, the session's folder is written to a
//! commit — its full working state, tracked and untracked (ignored files
//! aside), through a temporary index ([`write_tree`]) — and named by a ref
//! no branch list shows:
//!
//! * `refs/baaz/<session uuid>/pre/<turn>` when the turn starts, and
//! * `refs/baaz/<session uuid>/<turn>` when it ends (done, stopped or
//!   failed, as long as it ran).
//!
//! A turn's own diff is `pre/<turn>..<turn>`, so what the person edited
//! between two turns is never put on the agent, and Undo (docs/82 6.4) can
//! put a folder back to its `pre` without wiping those edits. A snapshot
//! whose tree is the last one's reuses that commit ([`snapshot`]): a turn
//! that changed nothing writes no object. Nothing here touches the person's
//! index, working tree, stash or `HEAD`; the commit's parent is the folder's
//! `HEAD` at that moment (none on an unborn branch).
//!
//! A folder that is no git work tree, a bare repository or a sparse checkout
//! takes no checkpoints ([`supported`]). Two sessions in one folder each
//! snapshot whatever is in it: nothing tries to tell their edits apart.
//!
//! [`Recorder`] is one session's bookkeeping (which turns started and
//! ended, the last snapshot, the refs waiting on the session's uuid, one
//! job at a time); [`run_job`] does a job's git work. The reads the Changes
//! screen's scopes need are [`session_refs`], [`diff_files`], [`diff_file`]
//! and [`read_lines_at`]; [`delete_session_refs`] is what forgetting a
//! session does to them (Undo's safety snapshots under
//! `refs/baaz/<uuid>/undo/<n>` go with them, [`crate::undo`]).
//!
//! Every function here blocks on git: callers run it off the UI thread.
//! No gpui.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aui_protocol::{ChangeKind, Diff, FileChange};

use crate::git_ops::{spawn, spawn_with_env};

/// Every checkpoint ref starts here (`refs/baaz/<session uuid>/…`). The
/// worktree service's preserved refs share the namespace under their own
/// fixed name ([`crate::worktrees::PRESERVED_PREFIX`]).
pub const PREFIX: &str = "refs/baaz/";
/// The folder under a session's refs that holds the pre-turn checkpoints.
pub const PRE: &str = "pre";
/// The folder under a session's refs that holds Undo's safety snapshots
/// ([`crate::undo`], docs/82 6.4).
pub const UNDO: &str = "undo";
/// How long a snapshot's git command may take (an `add -A` over a big
/// untracked tree is the slow one).
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a read for the Changes screen may take.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Who a snapshot commit is written as: a hidden ref, never a branch the
/// person pushes, so it does not borrow their identity (or need one set).
const IDENTITY: [(&str, &str); 4] = [
    ("GIT_AUTHOR_NAME", "Baaz"),
    ("GIT_AUTHOR_EMAIL", "baaz@localhost"),
    ("GIT_COMMITTER_NAME", "Baaz"),
    ("GIT_COMMITTER_EMAIL", "baaz@localhost"),
];

/// One snapshot: the commit and the tree it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checkpoint {
    pub commit: String,
    pub tree: String,
}

// ------------------------------------------------------------ git helpers

fn first_line(text: &str, fallback: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(fallback).to_owned()
}

pub(crate) fn git_in(dir: &Path, args: &[&str], env: &[(&str, &OsStr)], stdin: Option<&str>, timeout: Duration) -> Result<String, String> {
    let done = spawn_with_env("git", dir, args, stdin, true, env).and_then(|job| job.finish(Instant::now() + timeout));
    match done {
        Some(done) if done.ok => Ok(done.text.trim().to_owned()),
        Some(done) => Err(first_line(&done.text, &format!("git {} failed", args.first().copied().unwrap_or("")))),
        None => Err(format!("git {} did not finish", args.first().copied().unwrap_or(""))),
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_in(dir, args, &[], None, SNAPSHOT_TIMEOUT)
}

/// A read's raw output (stdout only, untrimmed: `-z` output ends in NUL).
fn git_read(dir: &Path, args: &[&str]) -> Option<String> {
    crate::render_io::note_io();
    let done = spawn("git", dir, args, None, false)?.finish(Instant::now() + READ_TIMEOUT)?;
    done.ok.then_some(done.text)
}

/// Whether `dir` takes checkpoints: inside a git work tree (not a bare
/// repository) that is not a sparse checkout. A missing folder does not.
pub fn supported(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    match git_ok(dir, &["rev-parse", "--is-inside-work-tree", "--is-bare-repository"]) {
        Ok(text) if text.split_whitespace().collect::<Vec<_>>() == ["true", "false"] => {}
        _ => return false,
    }
    git_ok(dir, &["config", "--bool", "core.sparseCheckout"]).map_or(true, |value| value != "true")
}

/// The folder's whole working state as a tree object: a copy of its own
/// index (so unchanged files are not hashed again) as `GIT_INDEX_FILE`,
/// `add -A`, `write-tree`, the copy removed. Untracked files are in it,
/// ignored ones are not. The folder's index, files and stash are never
/// touched.
pub fn write_tree(dir: &Path) -> Result<String, String> {
    let index = PathBuf::from(git_ok(dir, &["rev-parse", "--git-path", "index"])?);
    let index = if index.is_absolute() { index } else { dir.join(index) };
    let temp = index.with_file_name(format!("baaz-snapshot-{}.index", uuid::Uuid::new_v4().simple()));
    if index.is_file() {
        std::fs::copy(&index, &temp).map_err(|error| format!("{}: {error}", temp.display()))?;
    }
    let env = [("GIT_INDEX_FILE", temp.as_os_str())];
    // From the top, so `add -A` sees the whole tree whatever the folder.
    let tree = git_ok(dir, &["rev-parse", "--show-toplevel"]).and_then(|top| {
        let top = PathBuf::from(top);
        git_in(&top, &["add", "-A"], &env, None, SNAPSHOT_TIMEOUT).and_then(|_| git_in(&top, &["write-tree"], &env, None, SNAPSHOT_TIMEOUT))
    });
    let _ = std::fs::remove_file(&temp);
    tree
}

/// A commit of `tree` on `parent` (none: a root commit), written as Baaz.
pub fn commit_tree(dir: &Path, tree: &str, parent: Option<&str>, message: &str) -> Result<String, String> {
    let identity: Vec<(&str, &OsStr)> = IDENTITY.iter().map(|(key, value)| (*key, OsStr::new(*value))).collect();
    let mut args = vec!["commit-tree", tree];
    if let Some(parent) = parent {
        args.extend(["-p", parent]);
    }
    args.extend(["-m", message]);
    git_in(dir, &args, &identity, None, SNAPSHOT_TIMEOUT)
}

/// The folder's `HEAD` commit, `None` on an unborn branch.
pub fn head(dir: &Path) -> Option<String> {
    git_ok(dir, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]).ok().filter(|head| !head.is_empty())
}

/// Snapshot `dir`: `Ok(None)` when it takes no checkpoints ([`supported`]);
/// `reuse` comes back as it is when the tree is unchanged since it.
pub fn snapshot(dir: &Path, reuse: Option<&Checkpoint>, message: &str) -> Result<Option<Checkpoint>, String> {
    if !supported(dir) {
        return Ok(None);
    }
    let tree = write_tree(dir)?;
    if let Some(reuse) = reuse.filter(|reuse| reuse.tree == tree) {
        return Ok(Some(reuse.clone()));
    }
    let commit = commit_tree(dir, &tree, head(dir).as_deref(), message)?;
    Ok(Some(Checkpoint { commit, tree }))
}

// ------------------------------------------------------------------- refs

/// A turn id (or a session key) as one ref component: letters, digits, `-`
/// and `_` kept, anything else `-`, with a short hash of the original
/// appended when anything changed (so two ids never meet); never empty and
/// never [`PRE`] or [`UNDO`], which would collide with the pre-turn and
/// undo folders.
pub fn component(id: &str) -> String {
    let kept: String = id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    if kept == id && !id.is_empty() && id != PRE && id != UNDO {
        return kept;
    }
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    let base = if kept.is_empty() { "turn".to_owned() } else { kept.chars().take(80).collect() };
    format!("{base}-{:08x}", hasher.finish() as u32)
}

/// `refs/baaz/<key>/<turn>`: the checkpoint after the turn.
pub fn turn_ref(key: &str, turn_id: &str) -> String {
    format!("{PREFIX}{}/{}", component(key), component(turn_id))
}

/// `refs/baaz/<key>/pre/<turn>`: the checkpoint as the turn started.
pub fn pre_ref(key: &str, turn_id: &str) -> String {
    format!("{PREFIX}{}/{PRE}/{}", component(key), component(turn_id))
}

/// Point `name` at `commit`.
pub fn set_ref(dir: &Path, name: &str, commit: &str) -> Result<(), String> {
    git_ok(dir, &["update-ref", name, commit]).map(drop)
}

/// One turn's two checkpoints, as commits.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TurnRefs {
    pub pre: Option<String>,
    pub post: Option<String>,
}

/// Every checkpoint of the session `key` in the repository of `dir`, by
/// turn component ([`component`]). `None` when git could not be read.
pub fn session_refs(dir: &Path, key: &str) -> Option<HashMap<String, TurnRefs>> {
    let prefix = format!("{PREFIX}{}/", component(key));
    let raw = git_read(dir, &["for-each-ref", "--format=%(refname)%09%(objectname)", &prefix])?;
    let mut out: HashMap<String, TurnRefs> = HashMap::new();
    for line in raw.lines() {
        let Some((name, commit)) = line.split_once('\t') else { continue };
        let Some(rest) = name.strip_prefix(&prefix) else { continue };
        match rest.strip_prefix(&format!("{PRE}/")) {
            Some(turn) => out.entry(turn.to_owned()).or_default().pre = Some(commit.to_owned()),
            None if !rest.contains('/') => out.entry(rest.to_owned()).or_default().post = Some(commit.to_owned()),
            None => {}
        }
    }
    Some(out)
}

/// Delete every checkpoint ref of the session `key` (`refs/baaz/<key>/…`,
/// Undo's safety snapshots under `undo/` with them) in the repository of `dir`, in one `update-ref --stdin`. Returns how
/// many went. The worktree service's preserved refs are not under it.
pub fn delete_session_refs(dir: &Path, key: &str) -> Result<usize, String> {
    let prefix = format!("{PREFIX}{}/", component(key));
    let raw = git_ok(dir, &["for-each-ref", "--format=%(refname)", &prefix])?;
    let names: Vec<&str> = raw.lines().map(str::trim).filter(|name| !name.is_empty()).collect();
    if names.is_empty() {
        return Ok(0);
    }
    let script: String = names.iter().map(|name| format!("delete {name}\n")).collect();
    git_in(dir, &["update-ref", "--stdin"], &[], Some(&script), SNAPSHOT_TIMEOUT)?;
    Ok(names.len())
}

// ------------------------------------------------------------------ reads

/// The files that differ between `from` and `to` (commits or trees), with
/// `+N −M`, sorted by path: the shape the Changes screen draws for the
/// working tree ([`crate::git_ops::read_tree`]). Renames read as a delete
/// and an add. `None` when git could not be read.
pub fn diff_files(dir: &Path, from: &str, to: &str) -> Option<Vec<FileChange>> {
    let names = git_read(dir, &["diff", "--no-renames", "--name-status", "-z", "--no-color", "--no-ext-diff", from, to])?;
    let counts = git_read(dir, &["diff", "--no-renames", "--numstat", "-z", "--no-color", "--no-ext-diff", from, to])
        .map(|raw| crate::git_ops::parse_numstat_z(&raw))
        .unwrap_or_default();
    let mut files = Vec::new();
    let mut fields = names.split('\0');
    while let Some(status) = fields.next() {
        if status.is_empty() {
            continue;
        }
        let Some(path) = fields.next().filter(|path| !path.is_empty()) else { break };
        let change = match status.chars().next() {
            Some('A') => ChangeKind::Added,
            Some('D') => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        let (added, removed) = counts.get(path).copied().unwrap_or((0, 0));
        files.push(FileChange { path: path.to_owned(), change, added, removed });
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Some(files)
}

/// One file's diff between `from` and `to`, cut like the working tree's
/// ([`crate::git_ops::parse_one_diff`]). `None` when nothing can be read.
pub fn diff_file(dir: &Path, from: &str, to: &str, path: &str) -> Option<Diff> {
    let raw = git_read(dir, &["diff", "--no-renames", "--no-color", "--no-ext-diff", "--unified=3", from, to, "--", path])?;
    Some(crate::git_ops::parse_one_diff(path, &raw))
}

/// Lines `range` (1-based, end exclusive) of `path` as `rev` (a commit or a
/// tree) holds it: an unchanged run a checkpoint diff's row expands.
pub fn read_lines_at(dir: &Path, rev: &str, path: &str, range: std::ops::Range<u32>) -> Vec<String> {
    let Some(text) = git_read(dir, &["cat-file", "-p", &format!("{rev}:{path}")]) else { return Vec::new() };
    text.lines().skip(range.start.saturating_sub(1) as usize).take((range.end - range.start) as usize).map(str::to_owned).collect()
}

// -------------------------------------------------------------- recorder

/// Which end of a turn a job snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Moment {
    /// The turn started: `pre/<turn>`.
    Pre,
    /// The turn ended: `<turn>`.
    Post,
}

/// One snapshot to take off the UI thread ([`run_job`]): everything it
/// needs travels with it, and [`JobDone`] brings it back to the
/// [`Recorder`].
#[derive(Clone, Debug)]
pub struct Job {
    pub folder: PathBuf,
    pub turn_id: String,
    pub moment: Moment,
    /// The last snapshot this session took: reused when the tree is the same.
    pub reuse: Option<Checkpoint>,
    /// The session's uuid, once known.
    pub key: Option<String>,
    /// Refs still waiting on the uuid: (turn id, moment, commit).
    pub waiting: Vec<(String, Moment, String)>,
}

/// What a [`Job`] did.
#[derive(Clone, Debug)]
pub struct JobDone {
    pub turn_id: String,
    pub moment: Moment,
    /// The snapshot; `Ok(None)` when the folder takes no checkpoints.
    pub checkpoint: Result<Option<Checkpoint>, String>,
    /// The session's uuid, when the job knew or found it.
    pub key: Option<String>,
    /// Refs it could not write yet (no uuid).
    pub waiting: Vec<(String, Moment, String)>,
    /// Refs it wrote.
    pub written: Vec<String>,
}

/// Take a job's snapshot and name it: its own ref, and every ref still
/// waiting, once `lookup_key` (called only while the uuid is unknown)
/// answers. Blocks.
pub fn run_job(job: Job, lookup_key: impl FnOnce() -> Option<String>) -> JobDone {
    let what = match job.moment {
        Moment::Pre => "before",
        Moment::Post => "after",
    };
    let message = format!("baaz: checkpoint {what} turn {}", job.turn_id);
    let checkpoint = snapshot(&job.folder, job.reuse.as_ref(), &message);
    let mut waiting = job.waiting;
    if let Ok(Some(checkpoint)) = &checkpoint {
        waiting.push((job.turn_id.clone(), job.moment, checkpoint.commit.clone()));
    }
    let key = job.key.or_else(|| if waiting.is_empty() { None } else { lookup_key() });
    let mut written = Vec::new();
    if let Some(key) = &key {
        waiting.retain(|(turn, moment, commit)| {
            let name = match moment {
                Moment::Pre => pre_ref(key, turn),
                Moment::Post => turn_ref(key, turn),
            };
            match set_ref(&job.folder, &name, commit) {
                Ok(()) => {
                    written.push(name);
                    false
                }
                Err(error) => {
                    crate::baaz_log!("checkpoints: {name} not written ({error})");
                    false
                }
            }
        });
    }
    if let Err(error) = &checkpoint {
        crate::baaz_log!("checkpoints: {} {what} turn {} failed ({error})", job.folder.display(), job.turn_id);
    }
    JobDone { turn_id: job.turn_id, moment: job.moment, checkpoint, key, waiting, written }
}

/// One session's checkpoint bookkeeping (see the module doc): one job at a
/// time, in the order the turns asked; each turn's `pre` once and its
/// `post` once, and a `post` only for a turn whose `pre` was asked for (a
/// turn that ran here). A folder found unsupported is not asked again.
#[derive(Debug, Default)]
pub struct Recorder {
    /// The session's folder; `None`: this session takes no checkpoints.
    folder: Option<PathBuf>,
    key: Option<String>,
    last: Option<Checkpoint>,
    unsupported: bool,
    started: HashSet<String>,
    ended: HashSet<String>,
    queue: VecDeque<(String, Moment)>,
    busy: bool,
    waiting: Vec<(String, Moment, String)>,
}

impl Recorder {
    /// A recorder for a session in `folder` (`None`: off).
    pub fn new(folder: Option<PathBuf>) -> Self {
        Self { folder, ..Self::default() }
    }

    /// Whether this session takes checkpoints as far as is known.
    pub fn on(&self) -> bool {
        self.folder.is_some() && !self.unsupported
    }

    /// The folder this session checkpoints, while it takes checkpoints
    /// ([`Self::on`]).
    pub fn folder(&self) -> Option<&Path> {
        self.folder.as_deref().filter(|_| !self.unsupported)
    }

    /// The session's uuid, once a job found it.
    pub fn key(&self) -> Option<&str> {
        self.key.as_deref()
    }

    /// Nothing queued or running.
    pub fn idle(&self) -> bool {
        !self.busy && self.queue.is_empty()
    }

    /// The turn `turn_id` started: its `pre` is queued (once). Returns the
    /// job to run now, when none is running.
    pub fn turn_started(&mut self, turn_id: &str) -> Option<Job> {
        if !self.on() || !self.started.insert(turn_id.to_owned()) {
            return None;
        }
        self.queue.push_back((turn_id.to_owned(), Moment::Pre));
        self.next()
    }

    /// The turn `turn_id` ended (any outcome): its `post` is queued, once,
    /// when its start was seen.
    pub fn turn_ended(&mut self, turn_id: &str) -> Option<Job> {
        if !self.on() || !self.started.contains(turn_id) || !self.ended.insert(turn_id.to_owned()) {
            return None;
        }
        self.queue.push_back((turn_id.to_owned(), Moment::Post));
        self.next()
    }

    /// A job came back: keep what it learned, and hand out the next one.
    pub fn job_done(&mut self, done: JobDone) -> Option<Job> {
        self.busy = false;
        match done.checkpoint {
            Ok(Some(checkpoint)) => self.last = Some(checkpoint),
            Ok(None) => {
                self.unsupported = true;
                self.queue.clear();
            }
            Err(_) => {}
        }
        if done.key.is_some() {
            self.key = done.key;
        }
        self.waiting = done.waiting;
        self.next()
    }

    fn next(&mut self) -> Option<Job> {
        if self.busy || !self.on() {
            return None;
        }
        let folder = self.folder.clone()?;
        let (turn_id, moment) = self.queue.pop_front()?;
        self.busy = true;
        Some(Job {
            folder,
            turn_id,
            moment,
            reuse: self.last.clone(),
            key: self.key.clone(),
            waiting: std::mem::take(&mut self.waiting),
        })
    }
}

#[cfg(test)]
mod tests;
