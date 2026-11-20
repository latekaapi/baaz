//! Git and GitHub for the right pane's Changes screen (D6, docs/43 §2.6, §4;
//! docs/51 P1 #8).
//!
//! Reads: one `status --porcelain=v2 -z --branch --ignored=matching` and one
//! `diff HEAD --numstat -z`, started together, with `--no-optional-locks` so a
//! read never takes `.git/index.lock` from under the agent. A file's diff is
//! read only when its row opens ([`read_file_diff`]), never the whole tree.
//!
//! Writes, only on the person's click: commit exactly the ticked files (no
//! staging, owner 2026-10-17; ticks docs/82 7.2) or amend them into an
//! unpushed HEAD, a new branch, fetch, pull (fast-forward only), push, and
//! `gh pr create`. Tests never reach `gh` (the
//! seam returns nothing under `cfg(test)`); git runs only in temp repos.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aui_protocol::{ChangeKind, Diff, DiffKind, DiffLine, FileChange, Hunk};

/// How long a read may take before the pane gives up on it.
pub const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// A commit runs the project's hooks (clippy, tests): give it room.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(600);
/// The most of one command's output read.
const OUTPUT_CAP: usize = 4 * 1024 * 1024;
/// Lines a file's diff shows before it is cut.
pub const DIFF_LINE_CAP: usize = 500;
/// Untracked files counted line by line, at most this many and this big.
const UNTRACKED_COUNT_CAP: usize = 200;
const UNTRACKED_BYTES_CAP: u64 = 1024 * 1024;

/// The working tree as the Changes screen shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GitTree {
    /// Every changed path against HEAD (index and worktree together), with
    /// `+N −M`, sorted by path.
    pub files: Vec<FileChange>,
    /// The checked-out branch (`HEAD` when detached).
    pub branch: String,
    /// The upstream (`origin/right-pane`), when one is set.
    pub upstream: Option<String>,
    /// Commits ahead of / behind the upstream.
    pub ahead: u32,
    pub behind: u32,
    /// Ignored paths git reports (`target/`, `.cargo/`): the Files tree
    /// draws them faint. Directories end in `/`.
    pub ignored: Vec<String>,
    /// Untracked paths (`??`): in `files` as added, but the Files tree marks
    /// them U, not A (docs/43 §2.4).
    pub untracked: Vec<String>,
}

impl GitTree {
    /// Whether `path` is an untracked file.
    pub fn is_untracked(&self, path: &str) -> bool {
        self.untracked.iter().any(|u| u == path)
    }

    /// The change at `path`, if it changed.
    pub fn change(&self, path: &str) -> Option<&FileChange> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Whether `path` (root-relative, a directory without its slash) is
    /// ignored: itself, or under an ignored directory.
    pub fn is_ignored(&self, path: &str) -> bool {
        self.ignored.iter().any(|ig| match ig.strip_suffix('/') {
            Some(dir) => path == dir || path.starts_with(&format!("{dir}/")),
            None => path == ig,
        })
    }
}

/// One command started in the background, so several run at once.
pub struct Job {
    child: std::process::Child,
    output: std::sync::mpsc::Receiver<(Vec<u8>, bool)>,
}

/// Start `git <args>` in `root` without waiting. `stdin` is written and
/// closed first when given (a commit message).
pub fn spawn(program: &str, root: &Path, args: &[&str], stdin: Option<&str>, merge_stderr: bool) -> Option<Job> {
    spawn_with_env(program, root, args, stdin, merge_stderr, &[])
}

/// [`spawn`] with `env` set on the child on top of the usual variables
/// (a temporary `GIT_INDEX_FILE`, a commit identity).
pub fn spawn_with_env(
    program: &str,
    root: &Path,
    args: &[&str],
    stdin: Option<&str>,
    merge_stderr: bool,
    env: &[(&str, &std::ffi::OsStr)],
) -> Option<Job> {
    let mut command = Command::new(program);
    command.envs(env.iter().copied());
    if program == "git" {
        command.arg("--no-optional-locks");
    }
    command
        .current_dir(root)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(if merge_stderr { Stdio::piped() } else { Stdio::null() })
        // Never prompt for credentials or an editor from a background job.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .env("GH_PROMPT_DISABLED", "1");
    let mut child = command.spawn().ok()?;
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        let _ = pipe.write_all(text.as_bytes());
    }
    let mut stdout = child.stdout.take()?;
    let stderr = child.stderr.take();
    let (tx, output) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let err_thread = stderr.map(|mut err| {
            std::thread::spawn(move || {
                let mut bytes = Vec::new();
                let _ = (&mut err).take(OUTPUT_CAP as u64).read_to_end(&mut bytes);
                bytes
            })
        });
        let mut bytes = Vec::new();
        let _ = (&mut stdout).take(OUTPUT_CAP as u64 + 1).read_to_end(&mut bytes);
        let truncated = bytes.len() > OUTPUT_CAP;
        bytes.truncate(OUTPUT_CAP);
        if truncated {
            let _ = std::io::copy(&mut stdout, &mut std::io::sink());
        }
        if let Some(err) = err_thread.and_then(|t| t.join().ok()) {
            bytes.extend_from_slice(&err);
        }
        let _ = tx.send((bytes, truncated));
    });
    Some(Job { child, output })
}

/// How a finished job ended.
pub struct Finished {
    pub ok: bool,
    pub text: String,
}

impl Job {
    /// Wait until `deadline`; a job still running then is killed.
    pub fn finish(mut self, deadline: Instant) -> Option<Finished> {
        let wait = deadline.saturating_duration_since(Instant::now());
        let Ok((bytes, _)) = self.output.recv_timeout(wait) else {
            let _ = self.child.kill();
            let _ = self.child.wait();
            return None;
        };
        let ok = self.child.wait().ok()?.success();
        Some(Finished { ok, text: String::from_utf8_lossy(&bytes).into_owned() })
    }
}

fn git_read(root: &Path, args: &[&str]) -> Option<Job> {
    crate::render_io::note_io();
    spawn("git", root, args, None, false)
}

/// One porcelain v2 record's kind from its `XY` columns.
fn kind_of(xy: &str) -> ChangeKind {
    if xy.contains('A') || xy.contains('?') {
        ChangeKind::Added
    } else if xy.contains('D') {
        ChangeKind::Deleted
    } else {
        ChangeKind::Modified
    }
}

/// Parse `status --porcelain=v2 -z --branch --ignored=matching` into the
/// tree, without counts. Records are NUL-separated; a rename's original path
/// follows its record as its own field.
pub fn parse_status_v2(raw: &str) -> GitTree {
    let mut tree = GitTree { branch: "HEAD".into(), ..GitTree::default() };
    let mut fields = raw.split('\0').peekable();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(header) = record.strip_prefix("# ") {
            if let Some(head) = header.strip_prefix("branch.head ") {
                tree.branch = if head == "(detached)" { "HEAD".into() } else { head.to_string() };
            } else if let Some(up) = header.strip_prefix("branch.upstream ") {
                tree.upstream = Some(up.to_string());
            } else if let Some(ab) = header.strip_prefix("branch.ab ") {
                for part in ab.split_whitespace() {
                    if let Some(n) = part.strip_prefix('+') {
                        tree.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix('-') {
                        tree.behind = n.parse().unwrap_or(0);
                    }
                }
            }
            continue;
        }
        let mut parts = record.splitn(2, ' ');
        let tag = parts.next().unwrap_or("");
        let rest = parts.next().unwrap_or("");
        match tag {
            "1" => {
                // XY sub mH mI mW hH hI path
                let cols: Vec<&str> = rest.splitn(8, ' ').collect();
                if cols.len() == 8 {
                    tree.files.push(FileChange { path: cols[7].to_string(), change: kind_of(cols[0]), added: 0, removed: 0 });
                }
            }
            "2" => {
                // XY sub mH mI mW hH hI Xscore path, then the original path.
                let cols: Vec<&str> = rest.splitn(9, ' ').collect();
                if cols.len() == 9 {
                    tree.files.push(FileChange { path: cols[8].to_string(), change: ChangeKind::Modified, added: 0, removed: 0 });
                }
                let _ = fields.next();
            }
            "u" => {
                let cols: Vec<&str> = rest.splitn(10, ' ').collect();
                if cols.len() == 10 {
                    tree.files.push(FileChange { path: cols[9].to_string(), change: ChangeKind::Modified, added: 0, removed: 0 });
                }
            }
            "?" => {
                tree.files.push(FileChange { path: rest.to_string(), change: ChangeKind::Added, added: 0, removed: 0 });
                tree.untracked.push(rest.to_string());
            }
            "!" => tree.ignored.push(rest.to_string()),
            _ => {}
        }
    }
    tree.files.sort_by(|a, b| a.path.cmp(&b.path));
    tree
}

/// Parse `diff --numstat -z` into path → (added, removed). A rename's record
/// is `a\tb\t` followed by the old and new paths as their own fields.
pub fn parse_numstat_z(raw: &str) -> HashMap<String, (u32, u32)> {
    let mut counts = HashMap::new();
    let mut fields = raw.split('\0');
    while let Some(record) = fields.next() {
        let mut cols = record.splitn(3, '\t');
        let (Some(a), Some(r), Some(path)) = (cols.next(), cols.next(), cols.next()) else { continue };
        let path = if path.is_empty() {
            let _old = fields.next();
            fields.next().unwrap_or("").to_string()
        } else {
            path.to_string()
        };
        if path.is_empty() {
            continue;
        }
        counts.insert(path, (a.parse().unwrap_or(0), r.parse().unwrap_or(0)));
    }
    counts
}

/// Lines in a file, for an untracked file's `+N`. Big files count as 0, and
/// so do binary ones (a NUL in the first 8 KB, as git decides), whose
/// newlines mean nothing — a PNG read as +2,475 live (2026-10-19).
fn count_lines(path: &Path) -> u32 {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= UNTRACKED_BYTES_CAP => std::fs::read(path)
            .map(|bytes| {
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    return 0;
                }
                let n = bytes.iter().filter(|b| **b == b'\n').count();
                (n + usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'))) as u32
            })
            .unwrap_or(0),
        _ => 0,
    }
}

/// Read the working tree for `root`: `None` when it is not a git checkout.
/// Two commands, started together.
pub fn read_tree(root: &Path) -> Option<GitTree> {
    let deadline = Instant::now() + READ_TIMEOUT;
    let status = git_read(root, &["status", "--porcelain=v2", "-z", "--branch", "--untracked-files=all", "--ignored=matching"]);
    let numstat = git_read(root, &["diff", "HEAD", "--numstat", "-z", "--no-color", "--no-ext-diff"]);
    let status = status?.finish(deadline)?;
    if !status.ok {
        return None;
    }
    let mut tree = parse_status_v2(&status.text);
    let counts = numstat.and_then(|j| j.finish(deadline)).filter(|f| f.ok).map(|f| parse_numstat_z(&f.text)).unwrap_or_default();
    let mut untracked = 0;
    for file in &mut tree.files {
        if let Some((a, r)) = counts.get(&file.path) {
            file.added = *a;
            file.removed = *r;
        } else if file.change == ChangeKind::Added && untracked < UNTRACKED_COUNT_CAP {
            untracked += 1;
            file.added = count_lines(&root.join(&file.path));
        }
    }
    Some(tree)
}

/// One file's diff against HEAD, cut at [`DIFF_LINE_CAP`] lines. An
/// untracked file is all additions. `None` when nothing can be read.
pub fn read_file_diff(root: &Path, change: &FileChange) -> Option<Diff> {
    if change.change == ChangeKind::Added {
        // Untracked (or added): the file is the diff. `diff HEAD` would show
        // a staged add too, but an untracked one shows nothing.
        let text = std::fs::read_to_string(root.join(&change.path)).ok()?;
        let lines: Vec<&str> = text.lines().collect();
        let n = lines.len() as u32;
        let mut hunk = Hunk { header: format!("@@ -0,0 +1,{n} @@"), lines: Vec::new() };
        for (i, line) in lines.iter().take(DIFF_LINE_CAP).enumerate() {
            hunk.lines.push(DiffLine { kind: DiffKind::Add, old_no: None, new_no: Some(i as u32 + 1), text: line.to_string() });
        }
        return Some(Diff { path: change.path.clone(), hunks: vec![hunk], added: n, removed: 0 });
    }
    let deadline = Instant::now() + READ_TIMEOUT;
    let out = git_read(root, &["diff", "HEAD", "--no-color", "--no-ext-diff", "--unified=3", "--", &change.path])?.finish(deadline)?;
    if !out.ok {
        return None;
    }
    Some(parse_one_diff(&change.path, &out.text))
}

/// A single file's unified diff into hunks with line numbers, cut at
/// [`DIFF_LINE_CAP`] lines.
pub fn parse_one_diff(path: &str, raw: &str) -> Diff {
    let mut diff = Diff { path: path.to_string(), hunks: Vec::new(), added: 0, removed: 0 };
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut shown = 0usize;
    for line in raw.lines() {
        if line.starts_with("@@") {
            let old = line.split_whitespace().find(|t| t.starts_with('-')).and_then(|t| t[1..].split(',').next()?.parse().ok()).unwrap_or(0);
            let new = line.split_whitespace().find(|t| t.starts_with('+')).and_then(|t| t[1..].split(',').next()?.parse().ok()).unwrap_or(0);
            old_no = old;
            new_no = new;
            diff.hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
            continue;
        }
        let Some(hunk) = diff.hunks.last_mut() else { continue };
        let (kind, text) = match line.chars().next() {
            Some('+') => (DiffKind::Add, &line[1..]),
            Some('-') => (DiffKind::Del, &line[1..]),
            Some(' ') => (DiffKind::Context, &line[1..]),
            _ => continue,
        };
        let (o, n) = match kind {
            DiffKind::Add => {
                diff.added += 1;
                new_no += 1;
                (None, Some(new_no - 1))
            }
            DiffKind::Del => {
                diff.removed += 1;
                old_no += 1;
                (Some(old_no - 1), None)
            }
            _ => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
        };
        if shown < DIFF_LINE_CAP {
            hunk.lines.push(DiffLine { kind, old_no: o, new_no: n, text: text.to_string() });
            shown += 1;
        }
    }
    diff
}

/// Lines `range` (1-based, end exclusive) of the file as it is on disk: an
/// unchanged run a diff row expands.
pub fn read_lines(root: &Path, path: &str, range: std::ops::Range<u32>) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(path)) else { return Vec::new() };
    text.lines().skip(range.start.saturating_sub(1) as usize).take((range.end - range.start) as usize).map(str::to_string).collect()
}

// ─────────────────────────────────────────────────────────────── writes

/// How a commit ended.
#[derive(Clone, Debug, PartialEq)]
pub enum CommitOutcome {
    /// Committed.
    Done,
    /// A pre-commit / commit-msg hook refused it: its output, first lines first.
    HookFailed(Vec<String>),
    /// Git refused for another reason (its message).
    Failed(String),
}

/// Whether the repository runs a hook that can refuse a commit.
fn has_commit_hook(root: &Path) -> bool {
    let deadline = Instant::now() + READ_TIMEOUT;
    ["hooks/pre-commit", "hooks/commit-msg"].iter().any(|hook| {
        git_read(root, &["rev-parse", "--git-path", hook])
            .and_then(|j| j.finish(deadline))
            .filter(|f| f.ok)
            .map(|f| root.join(f.text.trim()))
            .is_some_and(|p| p.is_file())
    })
}

/// Commit exactly `paths` with `message` (owner 2026-10-17: no staging
/// step in the UI). `git add -A -- <paths>` takes new, changed and deleted
/// files among them, and `git commit -F - -- <paths>` commits only those —
/// never anything else in the working tree or the index (docs/81 F E3).
/// Blocking: run it off the UI thread.
pub fn commit_paths(root: &Path, message: &str, paths: &[String]) -> CommitOutcome {
    if paths.is_empty() {
        return CommitOutcome::Failed("Nothing to commit under this heading.".into());
    }
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let with_paths = |head: &[&'static str]| -> Vec<String> {
        head.iter().map(|arg| (*arg).to_owned()).chain(std::iter::once("--".to_owned())).chain(paths.iter().cloned()).collect()
    };
    let add_args = with_paths(&["add", "-A"]);
    let add_args: Vec<&str> = add_args.iter().map(String::as_str).collect();
    let Some(add) = spawn("git", root, &add_args, None, true).and_then(|j| j.finish(deadline)) else {
        return CommitOutcome::Failed("git add timed out".into());
    };
    if !add.ok {
        return CommitOutcome::Failed(first_line(&add.text));
    }
    let commit_args = with_paths(&["commit", "-F", "-"]);
    let commit_args: Vec<&str> = commit_args.iter().map(String::as_str).collect();
    let Some(out) = spawn("git", root, &commit_args, Some(message), true).and_then(|j| j.finish(deadline)) else {
        return CommitOutcome::Failed("The commit timed out.".into());
    };
    if out.ok {
        return CommitOutcome::Done;
    }
    if has_commit_hook(root) && !out.text.contains("nothing to commit") {
        return CommitOutcome::HookFailed(out.text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect());
    }
    CommitOutcome::Failed(first_line(&out.text))
}

fn first_line(text: &str) -> String {
    text.lines().find(|l| !l.trim().is_empty()).unwrap_or("git failed").trim().to_string()
}

/// Git's reason for a refusal: its first `fatal:` / `error:` line without
/// the prefix (a `hint:` block can come first), else its first line.
pub fn git_reason(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find_map(|l| l.strip_prefix("fatal: ").or_else(|| l.strip_prefix("error: ")))
        .map(str::to_string)
        .unwrap_or_else(|| first_line(text))
}

/// A refused or timed-out write's reason, from `what` and the job.
fn write_result(job: Option<Finished>, what: &str) -> Result<(), String> {
    match job {
        Some(f) if f.ok => Ok(()),
        Some(f) => Err(git_reason(&f.text)),
        None => Err(format!("{what} timed out.")),
    }
}

/// A fetch or pull may wait on the network this long.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(60);

/// New branch… (docs/82 7.2): `git switch -c <name>` from where HEAD is,
/// the working tree's changes carried over. A name git refuses
/// (`check-ref-format --branch`) or one that exists is refused in git's
/// words and nothing changes. Blocking: run it off the UI thread.
pub fn create_branch(root: &Path, name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("A branch needs a name.".into());
    }
    let deadline = Instant::now() + READ_TIMEOUT;
    write_result(spawn("git", root, &["check-ref-format", "--branch", name], None, true).and_then(|j| j.finish(deadline)), "Checking the name")?;
    let deadline = Instant::now() + WRITE_TIMEOUT;
    write_result(spawn("git", root, &["switch", "-c", name], None, true).and_then(|j| j.finish(deadline)), "Creating the branch")
}

/// Fetch: `git fetch --prune origin`, so `behind` is the remote's now.
pub fn fetch(root: &Path) -> Result<(), String> {
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    write_result(spawn("git", root, &["fetch", "--prune", "origin"], None, true).and_then(|j| j.finish(deadline)), "The fetch")
}

/// Pull, fast-forward only: `git pull --ff-only`. A branch that diverged,
/// or local changes the pull would overwrite, is refused by git with
/// nothing changed; its reason comes back.
pub fn pull_ff_only(root: &Path) -> Result<(), String> {
    let deadline = Instant::now() + NETWORK_TIMEOUT;
    write_result(spawn("git", root, &["pull", "--ff-only", "--no-rebase"], None, true).and_then(|j| j.finish(deadline)), "The pull")
}

/// Whether "Amend last commit" may be offered: HEAD exists and no remote
/// branch holds it (`branch -r --contains HEAD` is empty) — a pushed
/// commit is never rewritten, even on a branch with no upstream that was
/// cut from a pushed one.
pub fn head_amendable(root: &Path) -> bool {
    let deadline = Instant::now() + READ_TIMEOUT;
    let head = git_read(root, &["rev-parse", "--verify", "--quiet", "HEAD"]);
    let remote = git_read(root, &["branch", "-r", "--contains", "HEAD", "--format=%(refname)"]);
    let has_head = head.and_then(|j| j.finish(deadline)).is_some_and(|f| f.ok);
    let on_remote = remote.and_then(|j| j.finish(deadline)).map(|f| !f.ok || !f.text.trim().is_empty());
    has_head && on_remote == Some(false)
}

/// HEAD's whole message (what Amend starts from), `None` when unreadable.
pub fn head_message(root: &Path) -> Option<String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    git_read(root, &["log", "-1", "--format=%B", "HEAD"]).and_then(|j| j.finish(deadline)).filter(|f| f.ok).map(|f| f.text.trim_end().to_string())
}

/// Whether `origin` is a remote here (Fetch and Pull are offered only then).
pub fn has_origin(root: &Path) -> bool {
    let deadline = Instant::now() + READ_TIMEOUT;
    git_read(root, &["remote"]).and_then(|j| j.finish(deadline)).filter(|f| f.ok).is_some_and(|f| f.text.lines().any(|l| l.trim() == "origin"))
}

/// Amend the last commit with `message`, taking exactly `paths` into it —
/// the same guarantee as [`commit_paths`]: `git add -A -- <paths>`, then
/// `git commit --amend --only -F - -- <paths>`, so HEAD's own change stays,
/// the listed paths join it, and nothing else in the tree or the index
/// does. No paths rewrites the message alone. The caller offers this only
/// while [`head_amendable`]; it is checked again here. Blocking.
pub fn amend_paths(root: &Path, message: &str, paths: &[String]) -> CommitOutcome {
    if !head_amendable(root) {
        return CommitOutcome::Failed("The last commit is on the remote: it is not rewritten.".into());
    }
    let deadline = Instant::now() + WRITE_TIMEOUT;
    if !paths.is_empty() {
        let add_args: Vec<&str> = ["add", "-A", "--"].into_iter().chain(paths.iter().map(String::as_str)).collect();
        let Some(add) = spawn("git", root, &add_args, None, true).and_then(|j| j.finish(deadline)) else {
            return CommitOutcome::Failed("git add timed out".into());
        };
        if !add.ok {
            return CommitOutcome::Failed(first_line(&add.text));
        }
    }
    let args: Vec<&str> = ["commit", "--amend", "--only", "-F", "-", "--"].into_iter().chain(paths.iter().map(String::as_str)).collect();
    let Some(out) = spawn("git", root, &args, Some(message), true).and_then(|j| j.finish(deadline)) else {
        return CommitOutcome::Failed("The commit timed out.".into());
    };
    if out.ok {
        return CommitOutcome::Done;
    }
    if has_commit_hook(root) {
        return CommitOutcome::HookFailed(out.text.lines().filter(|l| !l.trim().is_empty()).map(str::to_string).collect());
    }
    CommitOutcome::Failed(git_reason(&out.text))
}

/// Push the branch, setting its upstream on `origin` the first time.
pub fn push(root: &Path, branch: &str, has_upstream: bool) -> Result<(), String> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let args: Vec<&str> = if has_upstream { vec!["push"] } else { vec!["push", "-u", "origin", branch] };
    match spawn("git", root, &args, None, true).and_then(|j| j.finish(deadline)) {
        Some(f) if f.ok => Ok(()),
        Some(f) => Err(first_line(&f.text)),
        None => Err("The push timed out.".into()),
    }
}

/// The repository's default branch (`origin/HEAD`), else `main`.
pub fn default_branch(root: &Path) -> String {
    let deadline = Instant::now() + READ_TIMEOUT;
    git_read(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .and_then(|f| f.text.trim().strip_prefix("origin/").map(str::to_string))
        .unwrap_or_else(|| "main".into())
}

/// The remote's branches, for the PR form's base picker.
pub fn remote_branches(root: &Path) -> Vec<String> {
    let deadline = Instant::now() + READ_TIMEOUT;
    git_read(root, &["branch", "-r", "--format=%(refname:short)"])
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .map(|f| {
            f.text
                .lines()
                .filter_map(|l| l.trim().strip_prefix("origin/"))
                .filter(|b| *b != "HEAD")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// What this branch adds over `base`, for the PR description draft: its
/// commits (subject, then body), newest first, at most 30, and `diff --stat`
/// against the merge base, at most 40 lines. Against `origin/<base>` when the
/// remote has it. Empty when git says nothing.
pub fn branch_summary(root: &Path, base: &str) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + READ_TIMEOUT;
    let remote = format!("origin/{base}");
    let has_remote = git_read(root, &["rev-parse", "--verify", "--quiet", &remote]).and_then(|j| j.finish(deadline)).is_some_and(|f| f.ok);
    let from = if has_remote { remote } else { base.to_string() };
    let range = format!("{from}..HEAD");
    let log = git_read(root, &["log", "--no-merges", "-n", "30", "--format=%s%n%b%x1e", &range]);
    let stat = git_read(root, &["diff", "--stat", "--no-color", &format!("{from}...HEAD")]);
    let commits = log
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .map(|f| f.text.split('\u{1e}').map(|c| c.trim().to_string()).filter(|c| !c.is_empty()).collect())
        .unwrap_or_default();
    let stat = stat
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .map(|f| f.text.lines().take(40).map(str::to_string).collect())
        .unwrap_or_default();
    (commits, stat)
}

/// Commits on this branch the remote does not have, when no upstream is
/// set: against `origin/<default>`.
pub fn unpushed_without_upstream(root: &Path, default: &str) -> u32 {
    let deadline = Instant::now() + READ_TIMEOUT;
    let range = format!("origin/{default}..HEAD");
    git_read(root, &["rev-list", "--count", &range])
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .and_then(|f| f.text.trim().parse().ok())
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────── GitHub

/// A pull request's state, as `gh` reports it (the PR card's dot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    /// Open.
    Open,
    /// Draft.
    Draft,
    /// Merged.
    Merged,
    /// Closed.
    Closed,
}

/// The branch's pull request, as the PR card shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PrState,
    /// (passed, total, still running).
    pub checks: Option<(u32, u32, bool)>,
}

/// `gh pr view --json …` into a [`PullRequest`].
pub fn parse_pr_view(raw: &str) -> Option<PullRequest> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let number = v.get("number")?.as_u64()?;
    let state = match (v.get("state").and_then(|s| s.as_str()), v.get("isDraft").and_then(|d| d.as_bool())) {
        (Some("MERGED"), _) => PrState::Merged,
        (Some("CLOSED"), _) => PrState::Closed,
        (_, Some(true)) => PrState::Draft,
        _ => PrState::Open,
    };
    let checks = v.get("statusCheckRollup").and_then(|r| r.as_array()).filter(|a| !a.is_empty()).map(|rollup| {
        let mut passed = 0;
        let mut running = false;
        for c in rollup {
            let conclusion = c.get("conclusion").and_then(|x| x.as_str()).unwrap_or("");
            let status = c.get("status").and_then(|x| x.as_str()).unwrap_or("COMPLETED");
            let state = c.get("state").and_then(|x| x.as_str()).unwrap_or("");
            if matches!(conclusion, "SUCCESS" | "NEUTRAL" | "SKIPPED") || state == "SUCCESS" {
                passed += 1;
            }
            if status != "COMPLETED" || state == "PENDING" {
                running = true;
            }
        }
        (passed, rollup.len() as u32, running)
    });
    Some(PullRequest {
        number,
        title: v.get("title").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        url: v.get("url").and_then(|t| t.as_str()).unwrap_or("").to_string(),
        state,
        checks,
    })
}

const PR_FIELDS: &str = "number,title,url,state,isDraft,statusCheckRollup";

/// The open (or last) PR for the checked-out branch, through `gh`. Never
/// runs under test.
pub fn pr_for_branch(root: &Path) -> Option<PullRequest> {
    if cfg!(any(test, feature = "test-support")) || crate::runtime::captures() {
        return None;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let out = spawn("gh", root, &["pr", "view", "--json", PR_FIELDS], None, false)?.finish(deadline)?;
    if !out.ok {
        return None;
    }
    parse_pr_view(&out.text)
}

/// `gh pr create`, then the PR it made. Never runs under test.
pub fn create_pr(root: &Path, base: &str, head: &str, title: &str, body: &str, draft: bool) -> Result<PullRequest, String> {
    if cfg!(any(test, feature = "test-support")) {
        return Err("gh is not run under test".into());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut args = vec!["pr", "create", "--base", base, "--head", head, "--title", title, "--body-file", "-"];
    if draft {
        args.push("--draft");
    }
    match spawn("gh", root, &args, Some(body), true).and_then(|j| j.finish(deadline)) {
        Some(f) if f.ok => pr_for_branch(root).ok_or_else(|| "The PR was created but could not be read back.".into()),
        Some(f) => Err(first_line(&f.text)),
        None => Err("gh pr create timed out.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_v2_reads_branch_changes_untracked_and_ignored() {
        let raw = "# branch.oid abc\0# branch.head right-pane\0# branch.upstream origin/right-pane\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb crates/baaz/src/right.rs\0\
1 D. N... 100644 000000 000000 ccc 000 scripts/old_probe.py\0\
2 R. N... 100644 100644 100644 ddd eee R100 new name.rs\0old name.rs\0\
? crates/baaz/src/panes/mod.rs\0! target/\0! .cargo/\0";
        let t = parse_status_v2(raw);
        assert_eq!(t.branch, "right-pane");
        assert_eq!(t.upstream.as_deref(), Some("origin/right-pane"));
        assert_eq!((t.ahead, t.behind), (2, 1));
        let paths: Vec<_> = t.files.iter().map(|f| (f.path.as_str(), f.change)).collect();
        assert_eq!(
            paths,
            vec![
                ("crates/baaz/src/panes/mod.rs", ChangeKind::Added),
                ("crates/baaz/src/right.rs", ChangeKind::Modified),
                ("new name.rs", ChangeKind::Modified),
                ("scripts/old_probe.py", ChangeKind::Deleted),
            ]
        );
        assert!(t.is_ignored("target"));
        assert!(t.is_ignored("target/debug/baaz"));
        assert!(t.is_ignored(".cargo"));
        assert!(!t.is_ignored(".github"));
        assert!(t.is_untracked("crates/baaz/src/panes/mod.rs") && !t.is_untracked("crates/baaz/src/right.rs"));
    }

    #[test]
    fn branch_summary_reads_the_branch_commits_and_stat() {
        let dir = std::env::temp_dir().join(format!("baaz-branch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            let out = std::process::Command::new("git").args(args).current_dir(&dir).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").output().unwrap();
            assert!(out.status.success(), "{args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "base"]);
        git(&["checkout", "-qb", "feature"]);
        std::fs::write(dir.join("b.txt"), "b\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-qm", "Add b", "-m", "Because b."]);
        let (commits, stat) = branch_summary(&dir, "main");
        assert_eq!(commits, vec!["Add b\nBecause b.".to_string()]);
        assert!(stat.iter().any(|l| l.contains("b.txt")), "{stat:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn binary_untracked_files_count_no_lines() {
        let dir = std::env::temp_dir().join(format!("baaz-count-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\ntwo\nthree").unwrap();
        std::fs::write(dir.join("b.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\n\n\n").unwrap();
        assert_eq!(count_lines(&dir.join("a.txt")), 3);
        assert_eq!(count_lines(&dir.join("b.png")), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn numstat_z_reads_plain_and_renamed_records() {
        let raw = "148\t12\tcrates/baaz/src/right.rs\u{0}-\t-\tassets/icon.png\u{0}3\t1\t\u{0}old.rs\u{0}new.rs\u{0}";
        let c = parse_numstat_z(raw);
        assert_eq!(c["crates/baaz/src/right.rs"], (148, 12));
        assert_eq!(c["assets/icon.png"], (0, 0));
        assert_eq!(c["new.rs"], (3, 1));
    }

    #[test]
    fn a_unified_diff_numbers_its_lines() {
        let raw = "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -10,3 +10,4 @@ fn x\n a\n-b\n+c\n+d\n e\n";
        let d = parse_one_diff("x", raw);
        assert_eq!(d.hunks.len(), 1);
        let nums: Vec<_> = d.hunks[0].lines.iter().map(|l| (l.kind, l.old_no, l.new_no)).collect();
        assert_eq!(
            nums,
            vec![
                (DiffKind::Context, Some(10), Some(10)),
                (DiffKind::Del, Some(11), None),
                (DiffKind::Add, None, Some(11)),
                (DiffKind::Add, None, Some(12)),
                (DiffKind::Context, Some(12), Some(13)),
            ]
        );
        assert_eq!((d.added, d.removed), (2, 1));
    }

    #[test]
    fn pr_view_json_reads_state_and_checks() {
        let raw = r#"{"number":128,"title":"Merge Diff and Git","url":"https://x/pull/128","state":"OPEN","isDraft":false,
            "statusCheckRollup":[{"status":"COMPLETED","conclusion":"SUCCESS"},{"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"NEUTRAL"},{"status":"IN_PROGRESS","conclusion":null}]}"#;
        let pr = parse_pr_view(raw).unwrap();
        assert_eq!(pr.number, 128);
        assert_eq!(pr.state, PrState::Open);
        assert_eq!(pr.checks, Some((3, 4, true)));
    }

    /// A temp directory removed on drop (no `tempfile` in this crate).
    struct Scratch(std::path::PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let p = std::env::temp_dir().join(format!("baaz-git-ops-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Scratch(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch_repo() -> Scratch {
        let dir = Scratch::new("repo");
        let run = |args: &[&str]| {
            let ok = Command::new("git").current_dir(dir.path()).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "t@example.com"]);
        run(&["config", "user.name", "T"]);
        std::fs::write(dir.path().join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.path().join(".gitignore"), "build/\n").unwrap();
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", "first"]);
        dir
    }

    #[test]
    fn a_real_tree_reads_counts_and_commits_everything() {
        let dir = scratch_repo();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
        std::fs::write(root.join("b.txt"), "new\nfile\n").unwrap();
        std::fs::create_dir(root.join("build")).unwrap();
        std::fs::write(root.join("build/out"), "x").unwrap();
        let tree = read_tree(root).expect("a repo");
        assert_eq!(tree.branch, "main");
        let a = tree.change("a.txt").unwrap();
        assert_eq!((a.change, a.added, a.removed), (ChangeKind::Modified, 2, 1));
        let b = tree.change("b.txt").unwrap();
        assert_eq!((b.change, b.added), (ChangeKind::Added, 2));
        assert!(tree.is_ignored("build/out"));
        let diff = read_file_diff(root, a).unwrap();
        assert_eq!((diff.added, diff.removed), (2, 1));
        let all: Vec<String> = tree.files.iter().map(|f| f.path.clone()).collect();
        assert_eq!(commit_paths(root, "Change a, add b\n\nBody.", &all), CommitOutcome::Done);
        assert!(read_tree(root).unwrap().files.is_empty(), "every listed file was committed");
    }

    /// docs/82 0.13: Commit takes only the files it was given — another
    /// session's edit, a staged change elsewhere and a new file outside the
    /// set all stay uncommitted; a deletion and a new file in the set go in.
    #[test]
    fn commit_takes_only_the_listed_files() {
        let dir = scratch_repo();
        let root = dir.path();
        std::fs::write(root.join("a.txt"), "mine\n").unwrap();
        std::fs::write(root.join("mine-new.txt"), "new\n").unwrap();
        std::fs::write(root.join("theirs.txt"), "other session\n").unwrap();
        std::fs::remove_file(root.join(".gitignore")).unwrap();
        // Something someone staged by hand, outside the set.
        std::fs::write(root.join("staged.txt"), "staged\n").unwrap();
        let ok = Command::new("git").current_dir(root).args(["add", "staged.txt"]).status().unwrap().success();
        assert!(ok);
        let set = vec!["a.txt".to_owned(), "mine-new.txt".to_owned(), ".gitignore".to_owned()];
        assert_eq!(commit_paths(root, "Mine only", &set), CommitOutcome::Done);
        let left: Vec<String> = read_tree(root).unwrap().files.iter().map(|f| f.path.clone()).collect();
        assert!(left.contains(&"theirs.txt".to_owned()), "{left:?}");
        assert!(left.contains(&"staged.txt".to_owned()), "{left:?}");
        assert!(!left.iter().any(|p| set.contains(p)), "the set went in: {left:?}");
        assert_eq!(commit_paths(root, "x", &[]), CommitOutcome::Failed("Nothing to commit under this heading.".into()));
    }

    #[test]
    fn a_failing_hook_reports_its_output() {
        let dir = scratch_repo();
        let root = dir.path();
        let hook = root.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho 'error: unused variable'\nexit 1\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(root.join("a.txt"), "changed\n").unwrap();
        match commit_paths(root, "x", &["a.txt".to_owned()]) {
            CommitOutcome::HookFailed(lines) => assert!(lines.iter().any(|l| l.contains("unused variable")), "{lines:?}"),
            other => panic!("expected a hook failure, got {other:?}"),
        }
    }

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "T")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "T")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A local bare `origin` with `main` pushed, a clone of it to work in
    /// (`work`, tracking `origin/main`) and a second clone (`other`) that
    /// stands for someone else pushing. Nothing touches the network.
    struct Remote {
        _dir: Scratch,
        work: std::path::PathBuf,
        other: std::path::PathBuf,
    }

    fn remote_repos() -> Remote {
        let dir = Scratch::new("remote");
        let (origin, work, other) = (dir.path().join("origin.git"), dir.path().join("work"), dir.path().join("other"));
        git_in(dir.path(), &["init", "-q", "--bare", "-b", "main", "origin.git"]);
        git_in(dir.path(), &["clone", "-q", origin.to_str().unwrap(), "work"]);
        git_in(&work, &["config", "user.email", "t@example.com"]);
        git_in(&work, &["config", "user.name", "T"]);
        git_in(&work, &["switch", "-q", "-c", "main"]);
        std::fs::write(work.join("a.txt"), "one\n").unwrap();
        git_in(&work, &["add", "-A"]);
        git_in(&work, &["commit", "-q", "-m", "first"]);
        git_in(&work, &["push", "-q", "-u", "origin", "main"]);
        git_in(dir.path(), &["clone", "-q", origin.to_str().unwrap(), "other"]);
        git_in(&other, &["config", "user.email", "o@example.com"]);
        git_in(&other, &["config", "user.name", "O"]);
        Remote { _dir: dir, work, other }
    }

    /// Someone else pushes a commit to `main` from the other clone.
    fn push_from_other(r: &Remote, file: &str) {
        std::fs::write(r.other.join(file), "theirs\n").unwrap();
        git_in(&r.other, &["add", "-A"]);
        git_in(&r.other, &["commit", "-q", "-m", "theirs"]);
        git_in(&r.other, &["push", "-q", "origin", "main"]);
    }

    #[test]
    fn a_new_branch_carries_the_changes_and_refuses_bad_or_taken_names() {
        let r = remote_repos();
        std::fs::write(r.work.join("a.txt"), "changed\n").unwrap();
        std::fs::write(r.work.join("new.txt"), "new\n").unwrap();
        let before = read_tree(&r.work).unwrap().files;
        let bad = create_branch(&r.work, "bad..name").unwrap_err();
        assert!(bad.contains("not a valid branch name"), "{bad}");
        let taken = create_branch(&r.work, "main").unwrap_err();
        assert!(taken.contains("already exists"), "{taken}");
        assert_eq!(read_tree(&r.work).unwrap().branch, "main", "a refusal changed nothing");
        create_branch(&r.work, "baaz/feature").unwrap();
        let after = read_tree(&r.work).unwrap();
        assert_eq!(after.branch, "baaz/feature");
        assert_eq!(after.upstream, None);
        assert_eq!(after.files, before, "the working tree came along");
        assert!(!head_amendable(&r.work), "HEAD is main's pushed commit: no amend on the new branch");
    }

    #[test]
    fn fetch_updates_behind_and_pull_fast_forwards() {
        let r = remote_repos();
        assert!(has_origin(&r.work));
        push_from_other(&r, "b.txt");
        assert_eq!(read_tree(&r.work).unwrap().behind, 0, "not fetched yet");
        fetch(&r.work).unwrap();
        assert_eq!(read_tree(&r.work).unwrap().behind, 1);
        // A local change elsewhere does not stop a fast-forward.
        std::fs::write(r.work.join("a.txt"), "mine\n").unwrap();
        pull_ff_only(&r.work).unwrap();
        let tree = read_tree(&r.work).unwrap();
        assert_eq!((tree.ahead, tree.behind), (0, 0));
        assert_eq!(std::fs::read_to_string(r.work.join("b.txt")).unwrap(), "theirs\n");
        assert_eq!(std::fs::read_to_string(r.work.join("a.txt")).unwrap(), "mine\n", "the local change stayed");
    }

    #[test]
    fn pull_refuses_a_diverged_branch_and_changes_nothing() {
        let r = remote_repos();
        push_from_other(&r, "b.txt");
        std::fs::write(r.work.join("c.txt"), "mine\n").unwrap();
        git_in(&r.work, &["add", "-A"]);
        git_in(&r.work, &["commit", "-q", "-m", "mine"]);
        std::fs::write(r.work.join("a.txt"), "edited\n").unwrap();
        fetch(&r.work).unwrap();
        let head = git_in(&r.work, &["rev-parse", "HEAD"]);
        let before = read_tree(&r.work).unwrap();
        assert_eq!((before.ahead, before.behind), (1, 1));
        let reason = pull_ff_only(&r.work).unwrap_err();
        assert!(!reason.is_empty() && !reason.starts_with("hint:"), "{reason}");
        assert_eq!(git_in(&r.work, &["rev-parse", "HEAD"]), head, "HEAD untouched");
        assert_eq!(read_tree(&r.work).unwrap(), before, "the working tree untouched");
        assert_eq!(std::fs::read_to_string(r.work.join("a.txt")).unwrap(), "edited\n");
    }

    #[test]
    fn pull_refuses_when_local_changes_are_in_the_way() {
        let r = remote_repos();
        // They change a.txt; the person has an uncommitted edit to it.
        std::fs::write(r.other.join("a.txt"), "theirs\n").unwrap();
        git_in(&r.other, &["commit", "-q", "-am", "theirs"]);
        git_in(&r.other, &["push", "-q", "origin", "main"]);
        fetch(&r.work).unwrap();
        std::fs::write(r.work.join("a.txt"), "mine\n").unwrap();
        let head = git_in(&r.work, &["rev-parse", "HEAD"]);
        assert!(pull_ff_only(&r.work).is_err());
        assert_eq!(git_in(&r.work, &["rev-parse", "HEAD"]), head);
        assert_eq!(std::fs::read_to_string(r.work.join("a.txt")).unwrap(), "mine\n");
    }

    #[test]
    fn amend_takes_only_the_ticked_paths_into_an_unpushed_head() {
        let r = remote_repos();
        assert!(!head_amendable(&r.work), "HEAD is pushed: never offered");
        assert!(matches!(amend_paths(&r.work, "x", &[]), CommitOutcome::Failed(_)), "and refused if asked");
        std::fs::write(r.work.join("b.txt"), "b\n").unwrap();
        git_in(&r.work, &["add", "-A"]);
        git_in(&r.work, &["commit", "-q", "-m", "Add b"]);
        assert!(head_amendable(&r.work), "ahead of the upstream");
        assert_eq!(head_message(&r.work).as_deref(), Some("Add b"));
        let parent = git_in(&r.work, &["rev-parse", "HEAD~1"]);
        std::fs::write(r.work.join("a.txt"), "ticked\n").unwrap();
        std::fs::write(r.work.join("c.txt"), "ticked new\n").unwrap();
        std::fs::write(r.work.join("d.txt"), "not ticked\n").unwrap();
        // Something staged by hand, outside the ticked set.
        std::fs::write(r.work.join("e.txt"), "staged\n").unwrap();
        git_in(&r.work, &["add", "e.txt"]);
        let ticked = vec!["a.txt".to_owned(), "c.txt".to_owned()];
        assert_eq!(amend_paths(&r.work, "Add b, change a\n\nBody.", &ticked), CommitOutcome::Done);
        assert_eq!(git_in(&r.work, &["rev-parse", "HEAD~1"]), parent, "HEAD was rewritten, not added to");
        assert_eq!(head_message(&r.work).as_deref(), Some("Add b, change a\n\nBody."));
        let in_head = git_in(&r.work, &["show", "--name-only", "--format=", "HEAD"]);
        let mut in_head: Vec<&str> = in_head.lines().collect();
        in_head.sort();
        assert_eq!(in_head, vec!["a.txt", "b.txt", "c.txt"], "HEAD's own change kept, the ticked paths joined it");
        let left: Vec<String> = read_tree(&r.work).unwrap().files.into_iter().map(|f| f.path).collect();
        assert_eq!(left, vec!["d.txt".to_owned(), "e.txt".to_owned()], "the rest stays uncommitted");
        // Pushed, it is not offered again.
        git_in(&r.work, &["push", "-q"]);
        assert!(!head_amendable(&r.work));
    }

    #[test]
    fn an_empty_repo_offers_no_amend_and_a_local_commit_does() {
        let dir = Scratch::new("empty");
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        assert!(!head_amendable(dir.path()), "no HEAD");
        assert!(!has_origin(dir.path()));
        let repo = scratch_repo();
        assert!(head_amendable(repo.path()), "a commit no remote holds");
    }

    #[test]
    fn git_reason_skips_hints() {
        assert_eq!(git_reason("hint: Diverging branches can't be fast-forwarded\nfatal: Not possible to fast-forward, aborting.\n"), "Not possible to fast-forward, aborting.");
        assert_eq!(git_reason("something odd\n"), "something odd");
    }

    #[test]
    fn a_folder_that_is_not_a_repo_reads_none() {
        let dir = Scratch::new("plain");
        assert!(read_tree(dir.path()).is_none());
    }
}
