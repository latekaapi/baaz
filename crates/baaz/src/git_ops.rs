//! Git and GitHub for the right pane's Changes screen (D6, docs/43 §2.6, §4;
//! docs/51 P1 #8).
//!
//! Reads: one `status --porcelain=v2 -z --branch --ignored=matching` and one
//! `diff HEAD --numstat -z`, started together, with `--no-optional-locks` so a
//! read never takes `.git/index.lock` from under the agent. A file's diff is
//! read only when its row opens ([`read_file_diff`]), never the whole tree.
//!
//! Writes, only on the person's click: commit every listed file (no staging,
//! owner 2026-10-17), push, and `gh pr create`. Tests never reach `gh` (the
//! seam returns nothing under `cfg(test)`); git runs only in temp repos.

use std::collections::HashMap;
use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use aui::protocol::{ChangeKind, Diff, DiffKind, DiffLine, FileChange, Hunk};

/// How long a read may take before the pane gives up on it.
pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// A commit runs the project's hooks (clippy, tests): give it room.
pub(crate) const WRITE_TIMEOUT: Duration = Duration::from_secs(600);
/// The most of one command's output read.
const OUTPUT_CAP: usize = 4 * 1024 * 1024;
/// Lines a file's diff shows before it is cut.
pub(crate) const DIFF_LINE_CAP: usize = 500;
/// Untracked files counted line by line, at most this many and this big.
const UNTRACKED_COUNT_CAP: usize = 200;
const UNTRACKED_BYTES_CAP: u64 = 1024 * 1024;

/// The working tree as the Changes screen shows it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct GitTree {
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
}

impl GitTree {
    /// The change at `path`, if it changed.
    pub(crate) fn change(&self, path: &str) -> Option<&FileChange> {
        self.files.iter().find(|f| f.path == path)
    }

    /// Whether `path` (root-relative, a directory without its slash) is
    /// ignored: itself, or under an ignored directory.
    pub(crate) fn is_ignored(&self, path: &str) -> bool {
        self.ignored.iter().any(|ig| match ig.strip_suffix('/') {
            Some(dir) => path == dir || path.starts_with(&format!("{dir}/")),
            None => path == ig,
        })
    }
}

/// One command started in the background, so several run at once.
pub(crate) struct Job {
    child: std::process::Child,
    output: std::sync::mpsc::Receiver<(Vec<u8>, bool)>,
}

/// Start `git <args>` in `root` without waiting. `stdin` is written and
/// closed first when given (a commit message).
pub(crate) fn spawn(program: &str, root: &Path, args: &[&str], stdin: Option<&str>, merge_stderr: bool) -> Option<Job> {
    let mut command = Command::new(program);
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
pub(crate) struct Finished {
    pub ok: bool,
    pub text: String,
}

impl Job {
    /// Wait until `deadline`; a job still running then is killed.
    pub(crate) fn finish(mut self, deadline: Instant) -> Option<Finished> {
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
    crate::right::note_io();
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
pub(crate) fn parse_status_v2(raw: &str) -> GitTree {
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
            "?" => tree.files.push(FileChange { path: rest.to_string(), change: ChangeKind::Added, added: 0, removed: 0 }),
            "!" => tree.ignored.push(rest.to_string()),
            _ => {}
        }
    }
    tree.files.sort_by(|a, b| a.path.cmp(&b.path));
    tree
}

/// Parse `diff --numstat -z` into path → (added, removed). A rename's record
/// is `a\tb\t` followed by the old and new paths as their own fields.
pub(crate) fn parse_numstat_z(raw: &str) -> HashMap<String, (u32, u32)> {
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

/// Lines in a file, for an untracked file's `+N`. Big files count as 0.
fn count_lines(path: &Path) -> u32 {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() <= UNTRACKED_BYTES_CAP => std::fs::read(path)
            .map(|bytes| {
                let n = bytes.iter().filter(|b| **b == b'\n').count();
                (n + usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'))) as u32
            })
            .unwrap_or(0),
        _ => 0,
    }
}

/// Read the working tree for `root`: `None` when it is not a git checkout.
/// Two commands, started together.
pub(crate) fn read_tree(root: &Path) -> Option<GitTree> {
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
pub(crate) fn read_file_diff(root: &Path, change: &FileChange) -> Option<Diff> {
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
pub(crate) fn parse_one_diff(path: &str, raw: &str) -> Diff {
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
pub(crate) fn read_lines(root: &Path, path: &str, range: std::ops::Range<u32>) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(root.join(path)) else { return Vec::new() };
    text.lines().skip(range.start.saturating_sub(1) as usize).take((range.end - range.start) as usize).map(str::to_string).collect()
}

// ─────────────────────────────────────────────────────────────── writes

/// How a commit ended.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum CommitOutcome {
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

/// Commit every listed file with `message` (owner 2026-10-17: no staging —
/// `git add -A`, then `git commit -F -`). Blocking: run it off the UI thread.
pub(crate) fn commit_all(root: &Path, message: &str) -> CommitOutcome {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let Some(add) = spawn("git", root, &["add", "-A"], None, true).and_then(|j| j.finish(deadline)) else {
        return CommitOutcome::Failed("git add timed out".into());
    };
    if !add.ok {
        return CommitOutcome::Failed(first_line(&add.text));
    }
    let Some(out) = spawn("git", root, &["commit", "-F", "-"], Some(message), true).and_then(|j| j.finish(deadline)) else {
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

/// Push the branch, setting its upstream on `origin` the first time.
pub(crate) fn push(root: &Path, branch: &str, has_upstream: bool) -> Result<(), String> {
    let deadline = Instant::now() + WRITE_TIMEOUT;
    let args: Vec<&str> = if has_upstream { vec!["push"] } else { vec!["push", "-u", "origin", branch] };
    match spawn("git", root, &args, None, true).and_then(|j| j.finish(deadline)) {
        Some(f) if f.ok => Ok(()),
        Some(f) => Err(first_line(&f.text)),
        None => Err("The push timed out.".into()),
    }
}

/// The repository's default branch (`origin/HEAD`), else `main`.
pub(crate) fn default_branch(root: &Path) -> String {
    let deadline = Instant::now() + READ_TIMEOUT;
    git_read(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"])
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .and_then(|f| f.text.trim().strip_prefix("origin/").map(str::to_string))
        .unwrap_or_else(|| "main".into())
}

/// The remote's branches, for the PR form's base picker.
pub(crate) fn remote_branches(root: &Path) -> Vec<String> {
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

/// Commits on this branch the remote does not have, when no upstream is
/// set: against `origin/<default>`.
pub(crate) fn unpushed_without_upstream(root: &Path, default: &str) -> u32 {
    let deadline = Instant::now() + READ_TIMEOUT;
    let range = format!("origin/{default}..HEAD");
    git_read(root, &["rev-list", "--count", &range])
        .and_then(|j| j.finish(deadline))
        .filter(|f| f.ok)
        .and_then(|f| f.text.trim().parse().ok())
        .unwrap_or(0)
}

// ─────────────────────────────────────────────────────────────── GitHub

/// The branch's pull request, as the PR card shows it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: aui::workbench::PrState,
    /// (passed, total, still running).
    pub checks: Option<(u32, u32, bool)>,
}

/// `gh pr view --json …` into a [`PullRequest`].
pub(crate) fn parse_pr_view(raw: &str) -> Option<PullRequest> {
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let number = v.get("number")?.as_u64()?;
    let state = match (v.get("state").and_then(|s| s.as_str()), v.get("isDraft").and_then(|d| d.as_bool())) {
        (Some("MERGED"), _) => aui::workbench::PrState::Merged,
        (Some("CLOSED"), _) => aui::workbench::PrState::Closed,
        (_, Some(true)) => aui::workbench::PrState::Draft,
        _ => aui::workbench::PrState::Open,
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
pub(crate) fn pr_for_branch(root: &Path) -> Option<PullRequest> {
    if cfg!(test) || crate::clock::deterministic() {
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
pub(crate) fn create_pr(root: &Path, base: &str, head: &str, title: &str, body: &str, draft: bool) -> Result<PullRequest, String> {
    if cfg!(test) {
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
        assert_eq!(pr.state, aui::workbench::PrState::Open);
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
        assert_eq!(commit_all(root, "Change a, add b\n\nBody."), CommitOutcome::Done);
        assert!(read_tree(root).unwrap().files.is_empty(), "every listed file was committed");
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
        match commit_all(root, "x") {
            CommitOutcome::HookFailed(lines) => assert!(lines.iter().any(|l| l.contains("unused variable")), "{lines:?}"),
            other => panic!("expected a hook failure, got {other:?}"),
        }
    }

    #[test]
    fn a_folder_that_is_not_a_repo_reads_none() {
        let dir = Scratch::new("plain");
        assert!(read_tree(dir.path()).is_none());
    }
}
