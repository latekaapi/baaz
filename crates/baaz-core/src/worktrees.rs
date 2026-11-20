//! A worktree per session (docs/82 6.1; design input docs/26b).
//!
//! A session that opts in runs in its own `git worktree`, made under
//! `<state>/worktrees/<project>/<name>` on a new branch `<prefix><name>`
//! (prefix from Settings, [`DEFAULT_BRANCH_PREFIX`] by default), cut from
//! the project's fetched default branch ([`resolve_base`]). Its row in
//! `baaz.db` ([`crate::db::worktrees`]) holds the folder, the branch, the
//! commit it was cut from and its block of ten ports; the session row's
//! `worktree_id` ties the two once the session has an id.
//!
//! * **Made with `git worktree add -b`**, never `-B`: an existing branch of
//!   that name is an error, not a reset. Then locked, so `git worktree
//!   prune` never drops it while its folder is away.
//! * **`.worktreeinclude`** (gitignore syntax; the file Claude Code and Codex
//!   read): the untracked, ignored files it matches are copied from the
//!   project root, keeping their paths ([`copy_included`]). No file, nothing
//!   copied.
//! * **`.baaz/project.toml`** declares `setup`, `run` and `preview_url`
//!   ([`ProjectConfig`]). `setup` runs once per worktree, in it, through
//!   `sh -c`, in its own session and process group (the `posix_spawn`
//!   helper, never `pre_exec`), with a timeout and its output in a log file
//!   beside the worktree ([`run_setup_once`]). A failing setup keeps the
//!   worktree.
//! * **Env**: [`Worktree::env`] — `BAAZ_WORKTREE_PATH`, `BAAZ_PROJECT_ROOT`,
//!   `BAAZ_PORT` (the first of the block) and `BAAZ_PORT_COUNT`.
//! * **An unused worktree goes** ([`discard_unused`]): `git worktree remove`
//!   without `--force`, so a worktree with changes in it stays.
//! * **Archive and unarchive** (docs/82 6.2, docs/26b §2.8): [`archive`]
//!   preserves the work to `refs/baaz/preserved/<session uuid>` through a
//!   temporary index ([`preserve`], on [`crate::checkpoints::write_tree`]),
//!   records the ref and the branch head on the row, then removes the
//!   folder (the branch stays); [`restore`] adds it back and puts the work
//!   back unstaged. [`move_back`] hands the worktree's changes to the
//!   project root instead.
//!
//! Every function here blocks on git: callers run it off the UI thread.
//! No gpui.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::git_ops::{spawn, Finished};

/// The branch prefix when Settings names none.
pub const DEFAULT_BRANCH_PREFIX: &str = "baaz/";
/// The first port Baaz hands a worktree.
pub const PORT_BASE: u16 = 41000;
/// Ports per worktree: `BAAZ_PORT` .. `BAAZ_PORT + PORT_BLOCK - 1`.
pub const PORT_BLOCK: u16 = 10;
/// How many blocks there are (41000–60999).
pub const PORT_BLOCKS: u16 = 2000;
/// How long the base's fetch may take before the cached ref is used.
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a setup script may run before its group is killed.
pub const SETUP_TIMEOUT: Duration = Duration::from_secs(600);
/// The file that names the ignored files a worktree gets copies of.
pub const INCLUDE_FILE: &str = ".worktreeinclude";
/// The project's own declarations, relative to its root.
pub const PROJECT_FILE: &str = ".baaz/project.toml";
/// The folder under the state dir that holds every worktree.
const WORKTREES_DIR: &str = "worktrees";
/// How long a git command that is not a fetch or a setup may take.
const GIT_TIMEOUT: Duration = Duration::from_secs(60);

/// One worktree Baaz made, as its row holds it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    /// The row's id (a uuid).
    pub id: String,
    /// The project it was made for, when the session had one.
    pub project_id: Option<String>,
    /// The project's root: the main working tree it was added to.
    pub project_root: PathBuf,
    /// Its folder: the session's folder.
    pub path: PathBuf,
    /// The folder's last component (`s-1a2b3c`).
    pub name: String,
    /// The branch it is on.
    pub branch: String,
    /// The commit it was cut from.
    pub base: String,
    /// The first of its [`PORT_BLOCK`] ports.
    pub port: u16,
    /// The bare id of the session it was made for, once known.
    pub created_by_session: Option<String>,
    /// Set while it is archived: its folder is gone and this is what
    /// [`restore`] brings back (docs/82 6.2).
    pub snapshot: Option<Snapshot>,
}

/// What an archived worktree left behind: the commit its branch was on and
/// the hidden ref holding its uncommitted and untracked work ([`preserve`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// `refs/baaz/preserved/<key>`, a commit whose tree is the worktree's
    /// files as they were (ignored files aside) and whose parent is
    /// [`Self::head`]. `None` when the folder was already gone, so there
    /// was nothing to keep but the branch.
    pub reference: Option<String>,
    /// The commit the worktree's branch was on.
    pub head: String,
}

impl Worktree {
    /// What the worktree's setup script (and anything else started for its
    /// session) is given on top of the inherited environment.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("BAAZ_WORKTREE_PATH".to_owned(), self.path.to_string_lossy().into_owned()),
            ("BAAZ_PROJECT_ROOT".to_owned(), self.project_root.to_string_lossy().into_owned()),
            ("BAAZ_PORT".to_owned(), self.port.to_string()),
            ("BAAZ_PORT_COUNT".to_owned(), PORT_BLOCK.to_string()),
        ]
    }

    /// Where its setup script's output goes: beside the folder, never in it.
    pub fn setup_log(&self) -> PathBuf {
        self.path.with_file_name(format!("{}.setup.log", self.name))
    }
}

/// `<state>/worktrees`: every worktree Baaz made lives under it.
pub fn worktrees_root(state_dir: &Path) -> PathBuf {
    state_dir.join(WORKTREES_DIR)
}

/// Whether `folder` is one of Baaz's worktrees (lies under
/// [`worktrees_root`]). Pure: reads no disk.
pub fn is_worktree_folder(state_dir: &Path, folder: &Path) -> bool {
    folder.starts_with(worktrees_root(state_dir)) && folder != worktrees_root(state_dir)
}

/// A short lowercase ascii slug of `text`: what a project folder's name
/// becomes when the project has no id.
fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "project".to_owned() } else { out.chars().take(40).collect() }
}

/// The folder a project's worktrees go in, under [`worktrees_root`].
fn project_folder(project_id: Option<&str>, root: &Path) -> String {
    match project_id.filter(|id| !id.trim().is_empty()) {
        Some(id) => slug(id),
        None => slug(&root.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()),
    }
}

/// A fresh worktree name: `s-` and six hex digits.
fn fresh_name() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("s-{}", &hex[..6])
}

// ------------------------------------------------------------ git helpers

fn git(root: &Path, args: &[&str], timeout: Duration) -> Option<Finished> {
    spawn("git", root, args, None, true).and_then(|job| job.finish(Instant::now() + timeout))
}

fn git_ok(root: &Path, args: &[&str]) -> Result<String, String> {
    match git(root, args, GIT_TIMEOUT) {
        Some(done) if done.ok => Ok(done.text.trim().to_owned()),
        Some(done) => Err(first_line(&done.text, &format!("git {} failed", args.first().copied().unwrap_or("")))),
        None => Err(format!("git {} did not finish", args.first().copied().unwrap_or(""))),
    }
}

fn first_line(text: &str, fallback: &str) -> String {
    text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or(fallback).to_owned()
}

/// What a worktree is cut from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Base {
    /// The commit.
    pub commit: String,
    /// What it was read from: `origin/<default>` or `HEAD`.
    pub label: String,
}

/// The remote's default branch, when the project tracks one: `origin/HEAD`,
/// else an `origin/main` or `origin/master` that exists.
fn remote_default(root: &Path) -> Option<String> {
    if let Ok(head) = git_ok(root, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]) {
        if let Some(branch) = head.strip_prefix("origin/").filter(|branch| !branch.is_empty()) {
            return Some(branch.to_owned());
        }
    }
    ["main", "master"]
        .into_iter()
        .find(|branch| git_ok(root, &["rev-parse", "--verify", "--quiet", &format!("refs/remotes/origin/{branch}")]).is_ok())
        .map(str::to_owned)
}

/// The commit a new worktree is cut from: the remote's default branch,
/// fetched first (that branch only, within [`FETCH_TIMEOUT`]; a fetch that
/// fails or times out leaves the cached ref, and says so in the log), else
/// the project's `HEAD` when it tracks no remote.
pub fn resolve_base(root: &Path) -> Result<Base, String> {
    if let Some(branch) = remote_default(root) {
        match git(root, &["fetch", "--no-tags", "--quiet", "origin", &branch], FETCH_TIMEOUT) {
            Some(done) if done.ok => {}
            Some(done) => crate::baaz_log!(
                "worktree: fetching origin/{branch} failed ({}); cutting from the cached ref",
                first_line(&done.text, "no reason given")
            ),
            None => crate::baaz_log!(
                "worktree: fetching origin/{branch} took over {}s; cutting from the cached ref",
                FETCH_TIMEOUT.as_secs()
            ),
        }
        let label = format!("origin/{branch}");
        let commit = git_ok(root, &["rev-parse", "--verify", &format!("refs/remotes/{label}^{{commit}}")])?;
        return Ok(Base { commit, label });
    }
    let commit = git_ok(root, &["rev-parse", "--verify", "HEAD^{commit}"])
        .map_err(|_| "The project has no commit to start a worktree from.".to_owned())?;
    Ok(Base { commit, label: "HEAD".to_owned() })
}

// ---------------------------------------------------------------- create

/// What [`create`] makes.
pub struct CreateSpec<'a> {
    /// Baaz's state dir: the worktree goes under it, the row into its
    /// `baaz.db`.
    pub state_dir: &'a Path,
    /// The project's root (a git repository).
    pub project_root: &'a Path,
    /// The project's id, when it is adopted.
    pub project_id: Option<&'a str>,
    /// The branch prefix (Settings).
    pub branch_prefix: &'a str,
    /// The worktree's name, else a fresh `s-xxxxxx`.
    pub name: Option<&'a str>,
}

/// Make a worktree: resolve its base, reserve its row (and port block),
/// `git worktree add -b`, lock it, copy the `.worktreeinclude` files.
/// Blocks; off the UI thread. A failure leaves no row and no folder.
pub fn create(spec: &CreateSpec) -> Result<Worktree, String> {
    let root = spec.project_root;
    if git_ok(root, &["rev-parse", "--show-toplevel"]).is_err() {
        return Err(format!("{} is not a git repository.", root.display()));
    }
    let base = resolve_base(root)?;
    let parent = worktrees_root(spec.state_dir).join(project_folder(spec.project_id, root));
    let db_path = crate::db::path_in(spec.state_dir);
    let mut worktree = None;
    for _ in 0..8 {
        let name = spec.name.map(str::to_owned).unwrap_or_else(fresh_name);
        let path = parent.join(&name);
        if path.exists() {
            if spec.name.is_some() {
                return Err(format!("{} already exists.", path.display()));
            }
            continue;
        }
        let candidate = Worktree {
            id: uuid::Uuid::new_v4().to_string(),
            project_id: spec.project_id.map(str::to_owned),
            project_root: root.to_path_buf(),
            branch: format!("{}{name}", spec.branch_prefix),
            path,
            name,
            base: base.commit.clone(),
            port: 0,
            created_by_session: None,
            snapshot: None,
        };
        let row = candidate.clone();
        let reserved = crate::db::call(db_path.clone(), move |db| crate::db::worktrees::reserve(&db.conn, &row))
            .ok_or("Baaz's database is not available.")?;
        match reserved {
            Ok(Some(port)) => {
                worktree = Some(Worktree { port, ..candidate });
                break;
            }
            Ok(None) => return Err("Every worktree port block is taken.".to_owned()),
            // The path is already a row's (a folder deleted by hand): try
            // another name.
            Err(error) if spec.name.is_none() => crate::baaz_log!("worktree: {error}; trying another name"),
            Err(error) => return Err(error.to_string()),
        }
    }
    let worktree = worktree.ok_or("No free worktree name.")?;
    if let Err(error) = add_locked(&worktree) {
        let id = worktree.id.clone();
        crate::db::run(db_path, move |db| {
            let _ = crate::db::worktrees::delete(&db.conn, &id);
        });
        return Err(error);
    }
    match copy_included(root, &worktree.path) {
        Ok(0) => {}
        Ok(copied) => crate::baaz_log!("worktree: copied {copied} file(s) named by {INCLUDE_FILE}"),
        Err(error) => crate::baaz_log!("worktree: {INCLUDE_FILE} copy stopped ({error})"),
    }
    remember_env(&worktree);
    crate::baaz_log!(
        "worktree: made {} on {} from {} ({}), ports {}–{}",
        worktree.path.display(),
        worktree.branch,
        base.label,
        &base.commit[..base.commit.len().min(12)],
        worktree.port,
        worktree.port + PORT_BLOCK - 1
    );
    Ok(worktree)
}

/// `git worktree add -b <branch> <path> <base>`, then the lock.
fn add_locked(worktree: &Worktree) -> Result<(), String> {
    if let Some(parent) = worktree.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let path = worktree.path.to_string_lossy();
    git_ok(&worktree.project_root, &["worktree", "add", "--quiet", "-b", &worktree.branch, &path, &worktree.base])?;
    lock(worktree, "pending")
}

/// Lock the worktree for `owner` (a session id, or `pending` before one).
fn lock(worktree: &Worktree, owner: &str) -> Result<(), String> {
    let path = worktree.path.to_string_lossy();
    let reason = format!("baaz session {owner}");
    git_ok(&worktree.project_root, &["worktree", "lock", "--reason", &reason, &path]).map(drop)
}

fn unlock(worktree: &Worktree) {
    let path = worktree.path.to_string_lossy();
    let _ = git_ok(&worktree.project_root, &["worktree", "unlock", &path]);
}

/// Re-lock the worktree in the name of the session that now runs in it.
pub fn claim(worktree: &Worktree, session_id: &str) -> Result<(), String> {
    unlock(worktree);
    lock(worktree, session_id)
}

/// The reason git holds for the worktree's lock, when it is locked.
pub fn lock_reason(worktree: &Worktree) -> Option<String> {
    let list = git_ok(&worktree.project_root, &["worktree", "list", "--porcelain"]).ok()?;
    let target = worktree.path.canonicalize().unwrap_or_else(|_| worktree.path.clone());
    let mut current = None;
    for line in list.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            let path = PathBuf::from(path);
            current = Some(path.canonicalize().unwrap_or(path));
        } else if let Some(reason) = line.strip_prefix("locked") {
            if current.as_deref() == Some(target.as_path()) {
                return Some(reason.trim().to_owned());
            }
        }
    }
    None
}

/// Copy the untracked, ignored files under `root` that `.worktreeinclude`
/// matches into `dest` at the same relative paths, skipping any already
/// there. Only the patterns' matches are listed (git walks; nothing here
/// globs a tree). Returns how many were copied.
pub fn copy_included(root: &Path, dest: &Path) -> Result<usize, String> {
    let include = root.join(INCLUDE_FILE);
    if !include.is_file() {
        return Ok(0);
    }
    let from = format!("--exclude-from={}", include.to_string_lossy());
    let listed = git(root, &["ls-files", "-z", "--others", "--ignored", &from], GIT_TIMEOUT)
        .filter(|done| done.ok)
        .ok_or("git ls-files failed")?;
    let matched: Vec<&str> = listed.text.split('\0').filter(|path| !path.is_empty()).collect();
    if matched.is_empty() {
        return Ok(0);
    }
    // Of those, the ones the project itself ignores: a matching file git
    // would show as untracked is the person's work, not a local secret.
    let stdin = matched.join("\0");
    let ignored = spawn("git", root, &["check-ignore", "-z", "--stdin"], Some(&stdin), false)
        .and_then(|job| job.finish(Instant::now() + GIT_TIMEOUT))
        .map(|done| done.text)
        .unwrap_or_default();
    let mut copied = 0;
    for rel in ignored.split('\0').filter(|path| !path.is_empty()) {
        let source = root.join(rel);
        let target = dest.join(rel);
        if !source.is_file() || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        // `fs::copy` is `clonefile` on APFS: no bytes move.
        std::fs::copy(&source, &target).map_err(|error| format!("{rel}: {error}"))?;
        copied += 1;
    }
    Ok(copied)
}

// ----------------------------------------------------------- project.toml

/// What a project declares in `.baaz/project.toml`. Stage 6 runs `setup`;
/// `run` and `preview_url` are read for stage 7.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectConfig {
    /// Runs once in each new worktree.
    pub setup: Option<String>,
    /// Starts the project (stage 7).
    pub run: Option<String>,
    /// Where the running project answers (stage 7).
    pub preview_url: Option<String>,
}

/// The project's declarations: the worktree's own `.baaz/project.toml`
/// (the committed one), else the project root's. Missing, unreadable or
/// unparsable reads as nothing declared.
pub fn read_project_config(folder: &Path, project_root: &Path) -> ProjectConfig {
    [folder, project_root]
        .iter()
        .find_map(|dir| std::fs::read_to_string(dir.join(PROJECT_FILE)).ok())
        .map(|text| parse_project_toml(&text))
        .unwrap_or_default()
}

/// The three top-level string keys of a `project.toml`. A small reader of
/// the TOML that file needs — `key = "basic"`, `key = 'literal'` and
/// `key = """multi-line"""` strings and `#` comments — not a TOML parser:
/// tables end the top level, and anything else is skipped.
pub fn parse_project_toml(text: &str) -> ProjectConfig {
    let mut config = ProjectConfig::default();
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim();
        if line.starts_with('[') {
            break;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let key = key.trim();
        let value = value.trim();
        let parsed = if let Some(rest) = value.strip_prefix("\"\"\"") {
            // A multi-line basic string: to the closing `"""`, the newline
            // right after the opener trimmed, as TOML does.
            let mut body = String::new();
            let mut rest = rest.to_owned();
            loop {
                if let Some(end) = rest.find("\"\"\"") {
                    body.push_str(&rest[..end]);
                    break Some(unescape(&body));
                }
                body.push_str(&rest);
                body.push('\n');
                match lines.next() {
                    Some(next) => rest = next.to_owned(),
                    None => break None,
                }
            }
            .map(|body| body.strip_prefix('\n').map(str::to_owned).unwrap_or(body))
        } else if let Some(rest) = value.strip_prefix('"') {
            closing(rest, '"').map(unescape)
        } else if let Some(rest) = value.strip_prefix('\'') {
            rest.find('\'').map(|end| rest[..end].to_owned())
        } else {
            None
        };
        let Some(parsed) = parsed else { continue };
        match key {
            "setup" => config.setup = Some(parsed),
            "run" => config.run = Some(parsed),
            "preview_url" => config.preview_url = Some(parsed),
            _ => {}
        }
    }
    config.setup = config.setup.filter(|text| !text.trim().is_empty());
    config.run = config.run.filter(|text| !text.trim().is_empty());
    config.preview_url = config.preview_url.filter(|text| !text.trim().is_empty());
    config
}

/// A basic string's body up to its unescaped closing quote.
fn closing(rest: &str, quote: char) -> Option<&str> {
    let mut escaped = false;
    for (at, c) in rest.char_indices() {
        match c {
            '\\' if !escaped => escaped = true,
            c if c == quote && !escaped => return Some(&rest[..at]),
            _ => escaped = false,
        }
    }
    None
}

fn unescape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

// ----------------------------------------------------------------- setup

/// How a worktree's setup went.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SetupOutcome {
    /// The project declares no `setup`.
    NotDeclared,
    /// It ran before, for this worktree: never twice.
    AlreadyRan,
    /// It ran and exited 0.
    Ran,
    /// It ran and failed (or timed out, or would not start): why, for the
    /// log and the session's notice. The worktree stays.
    Failed(String),
}

/// Run the project's `setup` in `worktree`, once: the row's stamp says
/// whether it already ran, and is set before the script starts (a setup
/// that crashed Baaz is not run again on the next open). `base_env` is
/// what the script inherits before [`Worktree::env`]; `timeout` bounds it,
/// after which its whole group is killed. Blocks; off the UI thread.
pub fn run_setup_once(
    state_dir: &Path,
    worktree: &Worktree,
    base_env: &[(OsString, OsString)],
    timeout: Duration,
) -> SetupOutcome {
    let config = read_project_config(&worktree.path, &worktree.project_root);
    let Some(setup) = config.setup else { return SetupOutcome::NotDeclared };
    let db_path = crate::db::path_in(state_dir);
    let id = worktree.id.clone();
    let ran = crate::db::call(db_path.clone(), move |db| crate::db::worktrees::setup_ran(&db.conn, &id));
    match ran {
        Some(Ok(false)) => {}
        Some(Ok(true)) => return SetupOutcome::AlreadyRan,
        Some(Err(error)) => return SetupOutcome::Failed(format!("the worktree's row did not read ({error})")),
        None => return SetupOutcome::Failed("Baaz's database is not available".to_owned()),
    }
    let id = worktree.id.clone();
    let _ = crate::db::call(db_path, move |db| crate::db::worktrees::mark_setup(&db.conn, &id));
    let log = worktree.setup_log();
    let outcome = run_script(&setup, worktree, base_env, &log, timeout);
    match &outcome {
        SetupOutcome::Ran => crate::baaz_log!("worktree: setup ran in {}", worktree.path.display()),
        SetupOutcome::Failed(why) => {
            crate::baaz_log!("worktree: setup failed in {} ({why}); see {}", worktree.path.display(), log.display());
        }
        _ => {}
    }
    outcome
}

/// `sh -c <script>` in the worktree, in a new session (its own process
/// group), output appended to `log`.
fn run_script(script: &str, worktree: &Worktree, base_env: &[(OsString, OsString)], log: &Path, timeout: Duration) -> SetupOutcome {
    use provider::session_spawn::{spawn_in_new_session, FileAction};
    let ours = worktree.env();
    let mut env: Vec<(OsString, OsString)> =
        base_env.iter().filter(|(key, _)| !ours.iter().any(|(name, _)| key == name.as_str())).cloned().collect();
    env.extend(ours.into_iter().map(|(key, value)| (OsString::from(key), OsString::from(value))));
    let spawned = spawn_in_new_session(
        std::ffi::OsStr::new("/bin/sh"),
        &[OsString::from("-c"), OsString::from(script)],
        &env,
        &[
            FileAction::Open { fd: 0, path: OsString::from("/dev/null"), flags: libc::O_RDONLY },
            FileAction::Open {
                fd: 1,
                path: log.as_os_str().to_owned(),
                flags: libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
            },
            FileAction::Dup2 { from: 1, to: 2 },
            FileAction::Chdir(worktree.path.as_os_str().to_owned()),
        ],
    );
    let pid = match spawned {
        Ok(pid) => pid,
        Err(error) => return SetupOutcome::Failed(format!("sh did not start ({error})")),
    };
    let deadline = Instant::now() + timeout;
    loop {
        let mut status = 0;
        // SAFETY: waits on our own child, without blocking.
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if reaped == pid {
            return if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
                SetupOutcome::Ran
            } else if libc::WIFEXITED(status) {
                SetupOutcome::Failed(format!("exit {}", libc::WEXITSTATUS(status)))
            } else {
                SetupOutcome::Failed("it was killed".to_owned())
            };
        }
        if reaped < 0 {
            return SetupOutcome::Failed("it could not be waited on".to_owned());
        }
        if Instant::now() >= deadline {
            // SAFETY: the script leads its own group (`POSIX_SPAWN_SETSID`):
            // this kills it and everything it started, then reaps it.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::waitpid(pid, &mut status, 0);
            }
            return SetupOutcome::Failed(format!("it ran past {}s", timeout.as_secs()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

// ------------------------------------------------------------- lookups

/// The worktree whose folder is `folder`, from `state_dir`'s database.
pub fn by_path(state_dir: &Path, folder: &Path) -> Option<Worktree> {
    let folder = folder.to_path_buf();
    let found = crate::db::read(crate::db::path_in(state_dir), move |db| crate::db::worktrees::by_path(&db.conn, &folder).ok().flatten())
        .flatten();
    if let Some(worktree) = &found {
        remember_env(worktree);
    }
    found
}

/// The variables a session running in `folder` is started with: the
/// worktree's [`Worktree::env`] when `folder` is one of Baaz's live
/// worktrees with a row, else none. Reads the database only for a folder under
/// [`worktrees_root`]; what it finds is remembered for
/// [`cached_env_for_folder`].
pub fn env_for_folder(state_dir: &Path, folder: &Path) -> Vec<(String, String)> {
    if !is_worktree_folder(state_dir, folder) {
        return Vec::new();
    }
    by_path(state_dir, folder).filter(|worktree| worktree.snapshot.is_none()).map(|worktree| worktree.env()).unwrap_or_default()
}

/// What this run knows of each live worktree, by folder: filled by
/// [`create`], [`restore`], [`remember_all`] and every [`by_path`] that
/// finds a live row, emptied by [`discard_unused`], [`archive`] and
/// [`move_back`]. Lets the UI thread start a terminal with a worktree's
/// variables, and name its branch on a sidebar row, without touching the
/// database.
type Cache = std::sync::Mutex<std::collections::HashMap<PathBuf, Worktree>>;

fn cache() -> &'static Cache {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cached<T>(work: impl FnOnce(&mut std::collections::HashMap<PathBuf, Worktree>) -> T) -> T {
    work(&mut cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner))
}

/// The spellings a folder is looked up by: as named, and as the disk
/// resolves it (a sidebar row's folder is canonical; `/var` is
/// `/private/var` on a Mac).
fn spellings(folder: &Path) -> Vec<PathBuf> {
    let mut keys = vec![folder.to_path_buf()];
    if let Ok(real) = folder.canonicalize() {
        if real != folder {
            keys.push(real);
        }
    }
    keys
}

fn remember_env(worktree: &Worktree) {
    if worktree.snapshot.is_none() {
        let keys = spellings(&worktree.path);
        cached(|map| {
            for key in keys {
                map.insert(key, worktree.clone());
            }
        });
    }
}

fn forget(folder: &Path) {
    cached(|map| map.retain(|key, worktree| key != folder && worktree.path != folder));
}

/// [`env_for_folder`] from what this run already knows, without the
/// database: empty for a folder no lookup or create has named yet.
pub fn cached_env_for_folder(folder: &Path) -> Vec<(String, String)> {
    cached(|map| map.get(folder).map(Worktree::env)).unwrap_or_default()
}

/// The branch of the live worktree in `folder`, from what this run
/// already knows (no database): `None` for any other folder.
pub fn cached_branch_for_folder(folder: &Path) -> Option<String> {
    cached(|map| map.get(folder).map(|worktree| worktree.branch.clone()))
}

/// Read every live worktree row into the cache once ([`cached_branch_for_folder`],
/// [`cached_env_for_folder`]): what a launch does before the sidebar names
/// branches. Returns how many it knows. Blocks; off the UI thread.
pub fn remember_all(state_dir: &Path) -> usize {
    let rows = crate::db::read(crate::db::path_in(state_dir), |db| crate::db::worktrees::list(&db.conn).ok())
        .flatten()
        .unwrap_or_default();
    let live: Vec<Worktree> = rows.into_iter().filter(|worktree| worktree.snapshot.is_none()).collect();
    let count = live.len();
    for worktree in &live {
        remember_env(worktree);
    }
    count
}

/// Tie the session `(provider, id)` to the worktree in `folder`, and lock
/// the worktree in that session's name. Queued on the database writer
/// after the session's own record, so the row it updates exists; the
/// relock runs on the caller's thread. `false` when `folder` is no
/// worktree of Baaz's.
pub fn link_session(state_dir: &Path, provider: &str, session_id: &str, folder: &Path) -> bool {
    let (provider_key, id, path) = (provider.to_owned(), session_id.to_owned(), folder.to_path_buf());
    let linked = crate::db::call(crate::db::path_in(state_dir), move |db| {
        crate::db::worktrees::link_session(&db.conn, &provider_key, &id, &path)
    });
    match linked {
        Some(Ok(true)) => {}
        Some(Ok(false)) | None => return false,
        Some(Err(error)) => {
            crate::baaz_log!("worktree: linking session {session_id} failed ({error})");
            return false;
        }
    }
    if let Some(worktree) = by_path(state_dir, folder) {
        if let Err(error) = claim(&worktree, session_id) {
            crate::baaz_log!("worktree: relocking {} failed ({error})", folder.display());
        }
    }
    true
}

// --------------------------------------------------------------- discard

/// Remove a worktree nobody used: an unsent draft's, discarded. Only when
/// clean — `git worktree remove` without `--force` refuses a worktree with
/// changes or untracked files, and so does this; such a worktree stays,
/// locked. Its branch goes too when it still points at the base (nothing
/// was committed on it), and its row. `Ok(false)`: kept, because it has
/// changes.
pub fn discard_unused(state_dir: &Path, worktree: &Worktree) -> Result<bool, String> {
    let db_path = crate::db::path_in(state_dir);
    if worktree.path.exists() {
        let status = git_ok(&worktree.path, &["status", "--porcelain"])?;
        if !status.is_empty() {
            crate::baaz_log!("worktree: {} has changes; kept", worktree.path.display());
            return Ok(false);
        }
        unlock(worktree);
        let path = worktree.path.to_string_lossy();
        if let Err(error) = git_ok(&worktree.project_root, &["worktree", "remove", &path]) {
            let _ = lock(worktree, worktree.created_by_session.as_deref().unwrap_or("pending"));
            return Err(error);
        }
    } else {
        let _ = git_ok(&worktree.project_root, &["worktree", "prune"]);
    }
    let tip = git_ok(&worktree.project_root, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", worktree.branch)]);
    if tip.as_deref() == Ok(worktree.base.as_str()) {
        let _ = git_ok(&worktree.project_root, &["branch", "-D", &worktree.branch]);
    }
    let _ = std::fs::remove_file(worktree.setup_log());
    forget(&worktree.path);
    let id = worktree.id.clone();
    crate::db::call(db_path, move |db| crate::db::worktrees::delete(&db.conn, &id));
    crate::baaz_log!("worktree: removed unused {}", worktree.path.display());
    Ok(true)
}

// ------------------------------------------------------------- lifecycle

/// The prefix of every preserved ref: `refs/baaz/preserved/<key>`, the key
/// the session row's uuid.
pub const PRESERVED_PREFIX: &str = "refs/baaz/preserved/";

/// Write the worktree's uncommitted and untracked work (ignored files
/// aside) to `refs/baaz/preserved/<key>` (docs/26b §2.8): the folder's
/// whole state as a tree through a temporary index
/// ([`crate::checkpoints::write_tree`], the mechanism the turn checkpoints
/// share), a commit of it on the branch head written as Baaz, `update-ref`.
/// Never touches the worktree's files, its index or the person's stash. A
/// clean worktree preserves too: its commit's tree is the head's. Blocks.
pub fn preserve(worktree: &Worktree, key: &str) -> Result<Snapshot, String> {
    let dir = &worktree.path;
    let head = git_ok(dir, &["rev-parse", "--verify", "HEAD^{commit}"])?;
    let tree = crate::checkpoints::write_tree(dir)?;
    let message = format!("baaz: preserved {}", worktree.path.display());
    let commit = crate::checkpoints::commit_tree(dir, &tree, Some(&head), &message)?;
    let reference = format!("{PRESERVED_PREFIX}{key}");
    git_ok(dir, &["update-ref", &reference, &commit])?;
    crate::baaz_log!("worktree: preserved {} as {reference} ({})", worktree.path.display(), &commit[..commit.len().min(12)]);
    Ok(Snapshot { reference: Some(reference), head })
}

/// Unlock and remove the worktree's folder: `git worktree remove`, and
/// `--force` only when that refuses (changes or untracked files), which the
/// caller has already preserved and recorded. A folder already gone is
/// pruned from git's list. On failure the lock is put back.
fn remove_folder(worktree: &Worktree) -> Result<(), String> {
    if !worktree.path.exists() {
        let _ = git_ok(&worktree.project_root, &["worktree", "prune"]);
        return Ok(());
    }
    unlock(worktree);
    let path = worktree.path.to_string_lossy();
    if git_ok(&worktree.project_root, &["worktree", "remove", &path]).is_ok() {
        return Ok(());
    }
    if let Err(error) = git_ok(&worktree.project_root, &["worktree", "remove", "--force", &path]) {
        let _ = lock(worktree, worktree.created_by_session.as_deref().unwrap_or("pending"));
        return Err(error);
    }
    Ok(())
}

/// The snapshot of a worktree about to go: [`preserve`] when its folder is
/// there, else just the branch's tip (nothing else is left to keep).
fn snapshot_of(worktree: &Worktree, key: &str) -> Result<Snapshot, String> {
    if worktree.path.exists() {
        return preserve(worktree, key);
    }
    let head = git_ok(&worktree.project_root, &["rev-parse", "--verify", &format!("refs/heads/{}^{{commit}}", worktree.branch)])
        .unwrap_or_else(|_| worktree.base.clone());
    Ok(Snapshot { reference: None, head })
}

fn record_archived(state_dir: &Path, worktree: &Worktree, snapshot: &Snapshot) -> Result<(), String> {
    let (id, row) = (worktree.id.clone(), snapshot.clone());
    match crate::db::call(crate::db::path_in(state_dir), move |db| crate::db::worktrees::mark_archived(&db.conn, &id, &row)) {
        Some(Ok(())) => Ok(()),
        Some(Err(error)) => Err(format!("the worktree's row did not take the snapshot ({error})")),
        None => Err("Baaz's database is not available.".to_owned()),
    }
}

/// Archive a worktree (docs/82 6.2): [`preserve`] its work under `key`,
/// record the snapshot on its row (committed first), then unlock and
/// remove the folder. Its branch, port block and row stay, so [`restore`]
/// brings it back. Already archived: `Ok` with the row as it is. Blocks.
pub fn archive(state_dir: &Path, worktree: &Worktree, key: &str) -> Result<Worktree, String> {
    if worktree.snapshot.is_some() {
        return Ok(worktree.clone());
    }
    let snapshot = snapshot_of(worktree, key)?;
    record_archived(state_dir, worktree, &snapshot)?;
    forget(&worktree.path);
    remove_folder(worktree)?;
    crate::baaz_log!("worktree: archived {} (branch {} kept)", worktree.path.display(), worktree.branch);
    Ok(Worktree { snapshot: Some(snapshot), ..worktree.clone() })
}

/// What [`restore`] made, and anything the person should be told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Restored {
    /// The live worktree, at its folder (a fresh one when the old was taken).
    pub worktree: Worktree,
    /// Why the work was not put back as it was, when it was not: the
    /// branch moved on and the snapshot did not apply over it. The work
    /// stays at its preserved ref.
    pub note: Option<String>,
}

/// Unarchive a worktree (docs/82 6.2): `git worktree add` its branch at its
/// old folder (a fresh name beside it when something is there now; a
/// branch deleted meanwhile is made again at the snapshot's head), put the
/// preserved work back — tracked changes unstaged, everything new
/// untracked — copy the `.worktreeinclude` files again and lock it. The
/// setup script is not run again: it ran once for this worktree, and what
/// it made outside git (ignored files) went with the folder. Blocks.
pub fn restore(state_dir: &Path, worktree: &Worktree) -> Result<Restored, String> {
    let Some(snapshot) = worktree.snapshot.clone() else {
        return Err(format!("{} is not archived.", worktree.path.display()));
    };
    let root = &worktree.project_root;
    let _ = git_ok(root, &["worktree", "prune"]);
    let mut live = Worktree { snapshot: None, ..worktree.clone() };
    if live.path.exists() {
        let parent = live.path.parent().map(Path::to_path_buf).unwrap_or_default();
        let fresh = (0..8).map(|_| parent.join(fresh_name())).find(|path| !path.exists()).ok_or("No free worktree name.")?;
        crate::baaz_log!("worktree: {} is taken; restoring at {}", live.path.display(), fresh.display());
        live.name = fresh.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        live.path = fresh;
    }
    if let Some(parent) = live.path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    let path = live.path.to_string_lossy().into_owned();
    let branch_ref = format!("refs/heads/{}", live.branch);
    if git_ok(root, &["rev-parse", "--verify", "--quiet", &branch_ref]).is_ok() {
        git_ok(root, &["worktree", "add", "--quiet", &path, &live.branch])?;
    } else {
        crate::baaz_log!("worktree: branch {} was deleted; making it again at {}", live.branch, snapshot.head);
        git_ok(root, &["worktree", "add", "--quiet", "-b", &live.branch, &path, &snapshot.head])?;
    }
    let note = match snapshot.reference.as_deref() {
        Some(reference) => put_back(&live, reference, &snapshot.head),
        None => None,
    };
    if let Err(error) = lock(&live, live.created_by_session.as_deref().unwrap_or("pending")) {
        crate::baaz_log!("worktree: relocking {} failed ({error})", live.path.display());
    }
    match copy_included(root, &live.path) {
        Ok(0) => {}
        Ok(copied) => crate::baaz_log!("worktree: copied {copied} file(s) named by {INCLUDE_FILE} again"),
        Err(error) => crate::baaz_log!("worktree: {INCLUDE_FILE} copy stopped ({error})"),
    }
    let (id, at) = (live.id.clone(), live.path.clone());
    match crate::db::call(crate::db::path_in(state_dir), move |db| crate::db::worktrees::mark_restored(&db.conn, &id, &at)) {
        Some(Ok(())) => {}
        Some(Err(error)) => crate::baaz_log!("worktree: the row of {} did not take the restore ({error})", live.path.display()),
        None => crate::baaz_log!("worktree: the database was away for the restore of {}", live.path.display()),
    }
    remember_env(&live);
    crate::baaz_log!("worktree: restored {} on {} (setup not run again)", live.path.display(), live.branch);
    Ok(Restored { worktree: live, note })
}

/// Put the preserved tree at `reference` back into the fresh worktree
/// `live`, unstaged. When its branch is still at `head`, a two-tree
/// `read-tree -m -u` moves the files to the preserved tree exactly
/// (deletions and renames included) and `reset` puts the index back on the
/// branch, so edits read as unstaged and new files as untracked. When the
/// branch moved on, the snapshot's own changes are applied over it as a
/// patch; one that does not apply leaves the branch's files and says so.
fn put_back(live: &Worktree, reference: &str, head: &str) -> Option<String> {
    let dir = &live.path;
    let tip = git_ok(dir, &["rev-parse", "--verify", "HEAD^{commit}"]).unwrap_or_default();
    let commit = match git_ok(dir, &["rev-parse", "--verify", "--quiet", &format!("{reference}^{{commit}}")]) {
        Ok(commit) => commit,
        Err(_) => return Some(format!("The preserved changes ({reference}) are gone; the worktree is back on its branch.")),
    };
    if tip == head {
        let moved = git_ok(dir, &["read-tree", "-m", "-u", "HEAD", &commit]).and_then(|_| git_ok(dir, &["reset", "-q"]));
        return moved.err().map(|error| format!("The preserved changes did not come back ({error}); they are kept at {reference}."));
    }
    match apply_between(dir, &commit_parent(dir, &commit).unwrap_or_else(|| head.to_owned()), &commit, dir, false) {
        Ok(()) => None,
        Err(error) => {
            crate::baaz_log!("worktree: the snapshot of {} did not apply over the moved branch ({error})", dir.display());
            Some(format!("Its branch moved on and the preserved changes did not apply over it; they are kept at {reference}."))
        }
    }
}

fn commit_parent(dir: &Path, commit: &str) -> Option<String> {
    git_ok(dir, &["rev-parse", "--verify", "--quiet", &format!("{commit}^1")]).ok()
}

/// Apply the changes `from..to` (both commits, read in `objects_in`) to the
/// working tree at `into`, as uncommitted changes: written to a patch file
/// first (`diff --binary`, so bytes survive), `git apply --check` before
/// anything is written, then `git apply`. `check_only` stops after the
/// check. Run at the top of `into`'s repository, so every path applies.
fn apply_between(objects_in: &Path, from: &str, to: &str, into: &Path, check_only: bool) -> Result<(), String> {
    let patch = std::env::temp_dir().join(format!("baaz-worktree-{}.patch", uuid::Uuid::new_v4().simple()));
    let out = format!("--output={}", patch.to_string_lossy());
    let result = (|| {
        git_ok(objects_in, &["diff", "--binary", "--no-color", "--no-ext-diff", &out, from, to])?;
        if std::fs::metadata(&patch).map(|meta| meta.len() == 0).unwrap_or(true) {
            return Ok(());
        }
        let top = PathBuf::from(git_ok(into, &["rev-parse", "--show-toplevel"])?);
        let file = patch.to_string_lossy();
        git_ok(&top, &["apply", "--check", &file])?;
        if check_only {
            return Ok(());
        }
        git_ok(&top, &["apply", &file]).map(drop)
    })();
    let _ = std::fs::remove_file(&patch);
    result
}

/// "Move back to the project folder" (docs/82 6.2): hand the worktree's
/// changes — its branch's commits since [`Worktree::base`] and its
/// uncommitted work — to the project root as uncommitted changes, then
/// archive the worktree for good (preserved, folder removed, branch kept)
/// and drop its row, so the session runs in the root from here.
///
/// The work is preserved under `key` first (that never touches the
/// worktree), then the patch `base..snapshot` is checked against the root:
/// one that would not apply cleanly is refused with nothing written to
/// either folder. Blocks.
pub fn move_back(state_dir: &Path, worktree: &Worktree, key: &str) -> Result<(), String> {
    if worktree.snapshot.is_some() || !worktree.path.exists() {
        return Err("The worktree's folder is not there to move back from.".to_owned());
    }
    let snapshot = preserve(worktree, key)?;
    let reference = snapshot.reference.clone().unwrap_or_default();
    let commit = git_ok(&worktree.project_root, &["rev-parse", "--verify", &format!("{reference}^{{commit}}")])?;
    let root = &worktree.project_root;
    if let Err(error) = apply_between(root, &worktree.base, &commit, root, true) {
        crate::baaz_log!("worktree: moving {} back refused ({error})", worktree.path.display());
        return Err(format!(
            "The changes don't apply cleanly to the project folder ({error}). Nothing was changed; the session stays in its worktree."
        ));
    }
    apply_between(root, &worktree.base, &commit, root, false)
        .map_err(|error| format!("The changes did not apply to the project folder ({error})."))?;
    record_archived(state_dir, worktree, &snapshot)?;
    forget(&worktree.path);
    remove_folder(worktree)?;
    let id = worktree.id.clone();
    crate::db::call(crate::db::path_in(state_dir), move |db| crate::db::worktrees::delete(&db.conn, &id));
    crate::baaz_log!(
        "worktree: moved {} back into {} (branch {} and {reference} kept)",
        worktree.path.display(),
        root.display(),
        worktree.branch
    );
    Ok(())
}

/// Run `work` while no other archive, restore or move back runs in this
/// process: an Undo pressed while its archive is still removing the folder
/// waits for it, then reads the archived row, rather than racing it.
pub fn one_at_a_time<T>(work: impl FnOnce() -> T) -> T {
    static LIFECYCLE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = LIFECYCLE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    work()
}

/// The worktree the session `(provider, id)` runs in (live or archived),
/// with the key its preserved ref is named by (the session row's uuid; the
/// bare id when there is no row). `folder` is the session's folder, read
/// when the row is not tied yet. Blocks.
pub fn for_session(state_dir: &Path, provider: &str, session_id: &str, folder: Option<&Path>) -> Option<(Worktree, String)> {
    let (provider, id, folder) = (provider.to_owned(), session_id.to_owned(), folder.map(Path::to_path_buf));
    crate::db::call(crate::db::path_in(state_dir), move |db| {
        let by_session = crate::db::worktrees::by_session(&db.conn, &provider, &id).ok().flatten();
        let worktree = by_session.or_else(|| {
            folder.as_deref().and_then(|folder| crate::db::worktrees::by_path(&db.conn, folder).ok().flatten())
        })?;
        let key = crate::db::worktrees::session_key(&db.conn, &provider, &id).ok().flatten().unwrap_or(id);
        Some((worktree, key))
    })
    .flatten()
}

#[cfg(test)]
mod tests;
