//! The pane tools' pure half (docs/85 13.1–13.4, W4): what an agent's
//! `pane_show`, `changes_read`, `changes_revert`, `changes_comment`,
//! `changes_commit` and `pr_status` ask for, read and answer, with no
//! window. The app half (`baaz::app::pane_tools`) puts the answer on
//! screen through the right pane's own paths.
//!
//! Results are short by default (rule 19): a diff stat, one file's diff
//! only when a file is named, a PR's state and counts with the failing
//! checks and the comments' first lines.

use std::path::Path;

use aui_protocol::{ChangeKind, Diff, DiffKind, FileChange};
use serde_json::{json, Value};

/// The most diff lines one `changes_read {file}` answers before "N more".
pub const DIFF_TEXT_CAP: usize = 400;
/// The most files a stat names before "N more".
pub const STAT_CAP: usize = 60;

/// What a Changes view or diff is of: the pane's scopes, as an agent names
/// them (`all`, `session`, `last_turn`, `turn_3`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    All,
    Session,
    LastTurn,
    /// The Nth agent turn of the session, from 1.
    Turn(usize),
}

impl Scope {
    /// `all` (also the default), `session`, `last_turn` (`last`), `turn_3`
    /// (`turn 3`, `turn3`, `3`).
    pub fn parse(text: Option<&str>) -> Result<Self, String> {
        let raw = text.map(str::trim).unwrap_or("").to_ascii_lowercase().replace([' ', '-'], "_");
        match raw.as_str() {
            "" | "all" | "all_changes" => Ok(Self::All),
            "session" | "this_session" => Ok(Self::Session),
            "last" | "last_turn" => Ok(Self::LastTurn),
            other => {
                let digits = other.strip_prefix("turn_").or_else(|| other.strip_prefix("turn")).unwrap_or(other);
                match digits.parse::<usize>() {
                    Ok(n) if n > 0 => Ok(Self::Turn(n)),
                    _ => Err(format!("unknown scope `{other}` (all, session, last_turn, turn_<n>)")),
                }
            }
        }
    }

    /// The word the answer and the pane use.
    pub fn words(&self) -> String {
        match self {
            Self::All => "all changes".into(),
            Self::Session => "this session".into(),
            Self::LastTurn => "last turn".into(),
            Self::Turn(n) => format!("turn {n}"),
        }
    }
}

/// One agent turn of a session with its checkpoints (`pre`, `post`), in
/// transcript order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TurnEnds {
    pub turn_id: String,
    pub pre: Option<String>,
    pub post: Option<String>,
}

/// What a scope reads between: `None` for All (the working tree against
/// `HEAD`), else `(from, to)` where `to: None` means the folder as it is
/// now. The turn id is the Turn scope's own, for the pane.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ends {
    pub from: String,
    pub to: Option<String>,
    pub turn_id: Option<String>,
    /// The turn's place, from 1 (Turn and Last turn).
    pub number: Option<usize>,
}

/// The ends `scope` reads in a session whose turns are `turns`. `Err`
/// names why a scope cannot answer (no checkpoints, no such turn).
pub fn ends(scope: &Scope, turns: &[TurnEnds]) -> Result<Option<Ends>, String> {
    let finished = |t: &TurnEnds| t.pre.is_some() && t.post.is_some();
    let turn_ends = |n: usize, t: &TurnEnds| Ends { from: t.pre.clone().unwrap_or_default(), to: t.post.clone(), turn_id: Some(t.turn_id.clone()), number: Some(n) };
    match scope {
        Scope::All => Ok(None),
        Scope::Session => {
            let first = turns.iter().find_map(|t| t.pre.clone()).ok_or("this session has no checkpoints yet (a folder without git, or no finished turn here)")?;
            Ok(Some(Ends { from: first, to: None, turn_id: None, number: None }))
        }
        Scope::LastTurn => {
            let (i, t) = turns.iter().enumerate().rev().find(|(_, t)| finished(t)).ok_or("no finished turn with checkpoints in this session")?;
            Ok(Some(turn_ends(i + 1, t)))
        }
        Scope::Turn(n) => {
            let t = turns.get(n - 1).ok_or_else(|| format!("this session has {} agent turns", turns.len()))?;
            if !finished(t) {
                return Err(format!("turn {n} has no checkpoints"));
            }
            Ok(Some(turn_ends(*n, t)))
        }
    }
}

fn kind_word(change: ChangeKind) -> &'static str {
    match change {
        ChangeKind::Added => "added",
        ChangeKind::Deleted => "deleted",
        ChangeKind::Modified => "modified",
    }
}

/// A stat over `files`: `{scope, files: [{path, change, added, removed}],
/// total: {files, added, removed}, more?}`.
pub fn stat(scope: &Scope, files: &[FileChange]) -> Value {
    let (added, removed) = files.iter().fold((0u64, 0u64), |(a, r), f| (a + f.added as u64, r + f.removed as u64));
    let listed: Vec<Value> = files
        .iter()
        .take(STAT_CAP)
        .map(|f| json!({"path": f.path, "change": kind_word(f.change), "added": f.added, "removed": f.removed}))
        .collect();
    let mut out = json!({"scope": scope.words(), "files": listed, "total": {"files": files.len(), "added": added, "removed": removed}});
    if files.len() > STAT_CAP {
        out["more"] = json!(files.len() - STAT_CAP);
    }
    if files.is_empty() {
        out["note"] = json!("nothing changed in this scope");
    }
    out
}

/// A diff as unified text (`@@` headers, `+`/`-`/` ` rows), cut at
/// [`DIFF_TEXT_CAP`] lines with the count of the rest.
pub fn diff_text(diff: &Diff) -> (String, usize) {
    let mut out = String::new();
    let mut lines = 0usize;
    let mut cut = 0usize;
    for hunk in &diff.hunks {
        for (i, line) in std::iter::once(None).chain(hunk.lines.iter().map(Some)).enumerate() {
            if lines >= DIFF_TEXT_CAP {
                cut += usize::from(i > 0);
                continue;
            }
            match line {
                None => out.push_str(&hunk.header),
                Some(line) => {
                    out.push(match line.kind {
                        DiffKind::Add => '+',
                        DiffKind::Del => '-',
                        DiffKind::Context => ' ',
                    });
                    out.push_str(&line.text);
                }
            }
            out.push('\n');
            lines += 1;
        }
    }
    (out, cut)
}

/// One file's answer: `{scope, path, change, added, removed, diff, more?}`.
pub fn file_answer(scope: &Scope, change: &FileChange, diff: Option<&Diff>) -> Value {
    let mut out = json!({"scope": scope.words(), "path": change.path, "change": kind_word(change.change), "added": change.added, "removed": change.removed});
    match diff {
        Some(diff) => {
            let (text, cut) = diff_text(diff);
            out["diff"] = json!(text);
            if cut > 0 {
                out["more"] = json!(format!("{cut} more diff lines not shown"));
            }
        }
        None => out["diff"] = json!("(no text diff: binary or unreadable)"),
    }
    out
}

/// `changes_read` in `root` (blocking; run it off the UI thread): the
/// stat, or `file`'s diff. `ends` from [`ends`].
pub fn read(root: &Path, scope: &Scope, ends: Option<&Ends>, file: Option<&str>) -> Result<Value, String> {
    let file = file.map(|f| normalise_path(root, f)).filter(|f| !f.is_empty());
    let files = match ends {
        None => crate::git_ops::read_tree(root).ok_or("this folder is not a git checkout")?.files,
        Some(ends) => {
            let to = match &ends.to {
                Some(to) => to.clone(),
                None => crate::checkpoints::write_tree(root)?,
            };
            let files = crate::checkpoints::diff_files(root, &ends.from, &to).ok_or("git could not read that diff")?;
            if let Some(path) = &file {
                let change = files.iter().find(|f| &f.path == path).ok_or_else(|| format!("{path} did not change in {}", scope.words()))?;
                let diff = crate::checkpoints::diff_file(root, &ends.from, &to, path);
                return Ok(file_answer(scope, change, diff.as_ref()));
            }
            files
        }
    };
    if let Some(path) = &file {
        let change = files.iter().find(|f| &f.path == path).ok_or_else(|| format!("{path} did not change in {}", scope.words()))?;
        let diff = crate::git_ops::read_file_diff(root, change);
        return Ok(file_answer(scope, change, diff.as_ref()));
    }
    Ok(stat(scope, &files))
}

/// A path as the agent gave it, made project-relative: an absolute path
/// inside `root` loses the prefix; `./` goes.
pub fn normalise_path(root: &Path, path: &str) -> String {
    let path = path.trim();
    let rel = Path::new(path).strip_prefix(root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| path.to_owned());
    rel.trim_start_matches("./").to_owned()
}

/// What `pane_show` asks the pane to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Show {
    /// The Files tree, or one file revealed (and previewed at `line`).
    Files { path: Option<String>, line: Option<u32> },
    /// Changes in a scope, with one file's diff opened when named.
    Changes { scope: Scope, path: Option<String> },
    Browser,
    Terminal,
    Tasks,
    /// The PR card (Changes' foot), read again.
    Pr,
}

impl Show {
    /// From the tool's arguments. `file` and `diff` need a `path`.
    pub fn parse(root: &Path, params: &Value) -> Result<Self, String> {
        let text = |key: &str| params.get(key).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty());
        let path = text("path").map(|p| normalise_path(root, p)).filter(|p| !p.is_empty());
        let line = params.get("line").and_then(|l| l.as_u64().or_else(|| l.as_str().and_then(|s| s.parse().ok()))).map(|l| l as u32).filter(|l| *l > 0);
        let scope = Scope::parse(text("scope"))?;
        let view = text("view").unwrap_or(if path.is_some() { "file" } else { "changes" }).to_ascii_lowercase();
        Ok(match view.as_str() {
            "files" | "tree" => Self::Files { path, line },
            "file" | "preview" => Self::Files { path: Some(path.ok_or("view file needs a path")?), line },
            "changes" => Self::Changes { scope, path },
            "diff" => Self::Changes { scope, path: Some(path.ok_or("view diff needs a path")?) },
            "browser" | "page" => Self::Browser,
            "terminal" => Self::Terminal,
            "tasks" => Self::Tasks,
            "pr" | "pull_request" => Self::Pr,
            other => return Err(format!("unknown view `{other}` (files, file, changes, diff, browser, terminal, tasks, pr)")),
        })
    }

    /// One line for the answer: what the person now sees.
    pub fn words(&self) -> String {
        match self {
            Self::Files { path: None, .. } => "the Files tree".into(),
            Self::Files { path: Some(path), line: Some(line) } => format!("{path} at line {line} in the preview"),
            Self::Files { path: Some(path), line: None } => format!("{path} in the preview"),
            Self::Changes { scope, path: None } => format!("Changes ({})", scope.words()),
            Self::Changes { scope, path: Some(path) } => format!("the diff of {path} ({})", scope.words()),
            Self::Browser => "the browser".into(),
            Self::Terminal => "the terminal".into(),
            Self::Tasks => "Tasks".into(),
            Self::Pr => "the pull request card".into(),
        }
    }
}

/// `pr_status`: the PR's state, checks (failing named) and comments (first
/// line each, at most 10), or `{pr: null}` when the branch has none.
pub fn pr_summary(pr: Option<&crate::git_ops::PullRequest>) -> Value {
    use crate::git_ops::{CheckState, CommentKind, PrState, ReviewVerdict};
    let Some(pr) = pr else {
        return json!({"pr": null, "note": "no pull request for this branch (or gh is not signed in)"});
    };
    let state = match pr.state {
        PrState::Open => "open",
        PrState::Draft => "draft",
        PrState::Merged => "merged",
        PrState::Closed => "closed",
    };
    let count = |s: CheckState| pr.check_list.iter().filter(|c| c.state == s).count();
    let failing: Vec<Value> = pr.check_list.iter().filter(|c| c.state == CheckState::Failing).map(|c| json!({"name": c.name, "conclusion": c.conclusion, "link": c.link})).collect();
    let comments: Vec<Value> = pr
        .comments
        .iter()
        .take(10)
        .map(|c| {
            let first = c.body.lines().next().unwrap_or("").chars().take(200).collect::<String>();
            let mut v = json!({"author": c.author, "text": first});
            match &c.kind {
                CommentKind::Review(ReviewVerdict::ChangesRequested) => v["review"] = json!("changes requested"),
                CommentKind::Review(ReviewVerdict::Approved) => v["review"] = json!("approved"),
                CommentKind::Review(ReviewVerdict::Commented) => v["review"] = json!("commented"),
                CommentKind::Comment => {}
                CommentKind::Line { path, line, .. } => v["at"] = json!(format!("{path}:{line}")),
            }
            v
        })
        .collect();
    json!({
        "pr": {"number": pr.number, "title": pr.title, "url": pr.url, "state": state, "branch": pr.branch},
        "checks": {"passed": count(CheckState::Passed) + count(CheckState::Skipped), "failing": count(CheckState::Failing), "running": count(CheckState::Running), "failing_checks": failing},
        "comments": comments,
        "more_comments": pr.comments.len().saturating_sub(10),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").current_dir(dir).args(args).env("GIT_AUTHOR_NAME", "t").env("GIT_AUTHOR_EMAIL", "t@t").env("GIT_COMMITTER_NAME", "t").env("GIT_COMMITTER_EMAIL", "t@t").status().unwrap().success();
        assert!(ok, "git {args:?}");
    }

    /// A fixture repo: one commit, then two "turns" with checkpoints taken
    /// the way the recorder takes them (`write_tree` + `commit_tree`).
    fn fixture() -> (std::path::PathBuf, Vec<TurnEnds>) {
        let dir = std::env::temp_dir().join(format!("baaz-pane-tools-{}-{}", std::process::id(), rand_suffix()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(dir.join("b.rs"), "fn b() {}\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        let snap = |dir: &Path| crate::checkpoints::write_tree(dir).unwrap();
        let pre1 = snap(&dir);
        std::fs::write(dir.join("a.rs"), "fn a() {}\nfn a2() {}\n").unwrap();
        let post1 = snap(&dir);
        let pre2 = post1.clone();
        std::fs::write(dir.join("c.rs"), "fn c() {}\n").unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        let post2 = snap(&dir);
        let turns = vec![
            TurnEnds { turn_id: "t1".into(), pre: Some(pre1), post: Some(post1) },
            TurnEnds { turn_id: "t2".into(), pre: Some(pre2), post: Some(post2) },
        ];
        (dir, turns)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    }

    #[test]
    fn scopes_parse_as_agents_say_them() {
        assert_eq!(Scope::parse(None).unwrap(), Scope::All);
        assert_eq!(Scope::parse(Some("last_turn")).unwrap(), Scope::LastTurn);
        assert_eq!(Scope::parse(Some("last")).unwrap(), Scope::LastTurn);
        assert_eq!(Scope::parse(Some("turn_3")).unwrap(), Scope::Turn(3));
        assert_eq!(Scope::parse(Some("turn 2")).unwrap(), Scope::Turn(2));
        assert_eq!(Scope::parse(Some("This session")).unwrap(), Scope::Session);
        assert!(Scope::parse(Some("turn_0")).is_err());
        assert!(Scope::parse(Some("yesterday")).is_err());
    }

    #[test]
    fn a_stat_by_default_and_one_files_diff_on_request_in_every_scope() {
        let (dir, turns) = fixture();
        // All: the working tree against HEAD (a.rs, b.rs modified; c.rs new).
        let all = read(&dir, &Scope::All, None, None).unwrap();
        assert_eq!(all["total"]["files"], 3, "{all}");
        assert!(all.get("diff").is_none(), "a stat carries no diff text");
        // Last turn: turn 2 only (b.rs emptied, c.rs added).
        let last_ends = ends(&Scope::LastTurn, &turns).unwrap();
        let last = read(&dir, &Scope::LastTurn, last_ends.as_ref(), None).unwrap();
        let paths: Vec<&str> = last["files"].as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["b.rs", "c.rs"]);
        assert_eq!(last["scope"], "last turn");
        // Turn 1: a.rs only, and its diff on request.
        let one = ends(&Scope::Turn(1), &turns).unwrap();
        let diff = read(&dir, &Scope::Turn(1), one.as_ref(), Some("a.rs")).unwrap();
        assert!(diff["diff"].as_str().unwrap().contains("+fn a2() {}"), "{diff}");
        assert_eq!(diff["added"], 1);
        // An absolute path inside the project reads the same.
        let abs = dir.join("a.rs").to_string_lossy().into_owned();
        assert_eq!(read(&dir, &Scope::Turn(1), one.as_ref(), Some(&abs)).unwrap()["path"], "a.rs");
        // A file that did not change in the scope is a refusal that says so.
        assert!(read(&dir, &Scope::Turn(1), one.as_ref(), Some("c.rs")).unwrap_err().contains("did not change in turn 1"));
        // Session: the first checkpoint against the folder now.
        let session = read(&dir, &Scope::Session, ends(&Scope::Session, &turns).unwrap().as_ref(), None).unwrap();
        assert_eq!(session["total"]["files"], 3);
        // All with a file: the working tree's diff.
        let c = read(&dir, &Scope::All, None, Some("c.rs")).unwrap();
        assert_eq!(c["change"], "added");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scopes_without_checkpoints_refuse_in_words() {
        assert!(ends(&Scope::LastTurn, &[]).unwrap_err().contains("no finished turn"));
        assert!(ends(&Scope::Session, &[]).is_err());
        let half = [TurnEnds { turn_id: "t".into(), pre: Some("x".into()), post: None }];
        assert!(ends(&Scope::Turn(1), &half).unwrap_err().contains("no checkpoints"));
        assert!(ends(&Scope::Turn(4), &half).unwrap_err().contains("1 agent turns"));
        assert_eq!(ends(&Scope::All, &[]).unwrap(), None);
    }

    #[test]
    fn long_diffs_are_cut_with_a_count() {
        use aui_protocol::{DiffLine, Hunk};
        let lines = (0..DIFF_TEXT_CAP + 50).map(|i| DiffLine { kind: DiffKind::Add, old_no: None, new_no: Some(i as u32), text: format!("l{i}") }).collect();
        let diff = Diff { path: "x".into(), hunks: vec![Hunk { header: "@@ -0,0 +1 @@".into(), lines }], added: 0, removed: 0 };
        let (text, cut) = diff_text(&diff);
        assert_eq!(text.lines().count(), DIFF_TEXT_CAP);
        assert_eq!(cut, 51);
    }

    #[test]
    fn pane_show_reads_every_view() {
        let root = Path::new("/r");
        let show = |v: Value| Show::parse(root, &v);
        assert_eq!(show(json!({"view": "file", "path": "/r/src/a.rs", "line": 42})).unwrap(), Show::Files { path: Some("src/a.rs".into()), line: Some(42) });
        assert_eq!(show(json!({"view": "files"})).unwrap(), Show::Files { path: None, line: None });
        assert_eq!(show(json!({"view": "diff", "path": "a.rs", "scope": "last_turn"})).unwrap(), Show::Changes { scope: Scope::LastTurn, path: Some("a.rs".into()) });
        assert_eq!(show(json!({"view": "changes", "scope": "turn_2"})).unwrap(), Show::Changes { scope: Scope::Turn(2), path: None });
        assert_eq!(show(json!({"view": "terminal"})).unwrap(), Show::Terminal);
        assert_eq!(show(json!({"view": "tasks"})).unwrap(), Show::Tasks);
        assert_eq!(show(json!({"view": "browser"})).unwrap(), Show::Browser);
        assert_eq!(show(json!({"view": "pr"})).unwrap(), Show::Pr);
        assert_eq!(show(json!({"path": "a.rs", "line": "7"})).unwrap(), Show::Files { path: Some("a.rs".into()), line: Some(7) }, "a path alone previews it");
        assert!(show(json!({"view": "diff"})).is_err());
        assert!(show(json!({"view": "settings"})).is_err());
    }

    #[test]
    fn pr_status_summarises_a_fixture() {
        let failing = crate::git_ops::pr_fixture("failing").unwrap();
        let v = pr_summary(Some(&failing));
        assert!(v["checks"]["failing"].as_u64().unwrap() > 0, "{v}");
        assert_eq!(v["checks"]["failing_checks"].as_array().unwrap().len() as u64, v["checks"]["failing"].as_u64().unwrap());
        assert!(v["comments"].as_array().unwrap().iter().any(|c| c.get("at").is_some()), "line comments carry path:line: {v}");
        assert_eq!(pr_summary(None)["pr"], Value::Null);
        let merged = pr_summary(Some(&crate::git_ops::pr_fixture("merged").unwrap()));
        assert_eq!(merged["pr"]["state"], "merged");
    }
}
