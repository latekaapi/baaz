//! Projects: the workspaces this window has adopted (design `docs/12-projects.md` §4).
//!
//! A project is an adopted root with an identity of its own: a UUID, a stable
//! colour, a name that can be changed without touching the folder, and the
//! last-used model, effort and approval mode new sessions start with. The file
//! is `~/Library/Application Support/baaz/projects.json`, camelCase,
//! written atomically through [`crate::store`], and every read is
//! best-effort: a missing or unparseable file is an empty store, which loses
//! an adoption and never a session.
//!
//! A session resolves to a project in one order only: the `project` id the
//! baaz wrote into its own `sessions.json` when that id still names a
//! project; else the project whose canonical root equals the row's
//! `workspace_root`; else nothing. Never by prefix, never by the current
//! project — a worktree session's folder differs from its project's root, and
//! a prefix match would adopt it into the wrong project.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The schema version new files are written with.
const VERSION: u32 = 1;

fn default_version() -> u32 {
    VERSION
}

/// The per-project defaults a new session starts with (decision D35): the
/// last-used model, effort and approval mode. `--approval-mode` still wins
/// over the stored mode for the session it starts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDefaults {
    /// The model id, as `session/start` spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_id: Option<String>,
    /// The reasoning effort, as MSP spells it (`"high"`, `"xhigh"` …; see
    /// [`effort_string`] and [`parse_effort`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// The approval mode new sessions start in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_mode: Option<muse_client::schema::ApprovalMode>,
}

/// One adopted workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// A UUIDv4: the identity renames and moves never touch.
    pub id: String,
    /// The canonical root, unique among projects.
    pub root: PathBuf,
    /// The folder name at adoption; renameable without touching the folder.
    #[serde(default)]
    pub name: String,
    /// The label-ramp slot, 1–8.
    #[serde(default = "default_colour")]
    pub colour: u8,
    /// Pinned projects sort before unpinned ones.
    #[serde(default)]
    pub pinned: bool,
    /// RFC3339 UTC, when the project was adopted.
    #[serde(default)]
    pub added_at: String,
    /// RFC3339 UTC, when a session of this project last opened.
    #[serde(default)]
    pub last_opened_at: String,
    /// What a new session in this project starts with.
    #[serde(default)]
    pub defaults: ProjectDefaults,
}

fn default_colour() -> u8 {
    1
}

/// How long a root-exists answer is trusted before the disk is asked again:
/// short enough that a deleted worktree leaves the sidebar promptly, long
/// enough that rendering never stats the disk per frame. The list refresh
/// rechecks every adoption off the UI thread regardless of this.
const EXISTS_TTL_SECS: u64 = 30;

/// Now as whole seconds since the epoch, for [`EXISTS_TTL_SECS`].
fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|t| t.as_secs()).unwrap_or(0)
}

/// The whole store: the schema version, the current project, and the
/// adoptions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Projects {
    /// The schema version; files that predate it still read.
    #[serde(default = "default_version")]
    pub version: u32,
    /// The current project: the open session's project, else the last used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// The adoptions, in adoption order.
    #[serde(default)]
    pub projects: Vec<Project>,
    /// Root-exists answers per project id: `(exists, checked_at_secs)`.
    /// Memory only — never serialized, so a missing root stays adopted in
    /// `projects.json` and comes back when the path does. Compared by the
    /// derive like any field; both sides start empty and every read fills
    /// its own.
    #[serde(skip)]
    existence: std::cell::RefCell<std::collections::HashMap<String, (bool, u64)>>,
}

impl Default for Projects {
    fn default() -> Self {
        Self { version: VERSION, current: None, projects: Vec::new(), existence: Default::default() }
    }
}

/// `~/Library/Application Support/baaz/projects.json`.
pub fn path() -> PathBuf {
    crate::store::support_dir().join("projects.json")
}

/// Read the store. Blocking; call it off the UI thread.
pub fn read() -> Projects {
    read_at(&path())
}

/// [`read`] against an explicit path, which is what the tests use.
pub fn read_at(path: &Path) -> Projects {
    crate::store::read_json(path)
}

/// Write the store, atomically.
///
/// Best-effort: a store that cannot be written loses an adoption, which is a
/// nuisance, and never a session, which would be a loss.
pub fn write(projects: &Projects) {
    write_at(&path(), projects);
}

/// [`write()`] against an explicit path, which is what the tests use.
pub fn write_at(path: &Path, projects: &Projects) {
    if let Ok(text) = serde_json::to_vec_pretty(projects) {
        let _ = crate::store::write_atomic(path, &text);
    }
}

/// Now, as the store spells it: RFC3339 UTC to the second.
///
/// Through [`crate::clock`], not `Utc::now()` directly: since `sorted` began
/// counting `added_at` (D3), the adoption stamp decides the sidebar's order,
/// and a capture that adopts several folders in one run would otherwise order
/// them by which second each landed in — different between two runs seconds
/// apart. Frozen under `BAAZ_DETERMINISTIC=1`, every adoption in a run
/// shares one stamp, so they tie and fall back to the name.
fn now_utc() -> String {
    crate::clock::now_local().with_timezone(&chrono::Utc).format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// `root` with symlinks resolved, or verbatim when it does not resolve —
/// which is exactly "canonicalized when the path exists, verbatim otherwise".
/// `/tmp/x` and `/private/tmp/x` are one root on macOS because the first
/// canonicalizes to the second.
pub fn canonical_path(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// [`canonical_path`] for a string root, which is what the wire hands over.
pub fn canonical_str(root: &str) -> String {
    canonical_path(Path::new(root)).to_string_lossy().into_owned()
}

/// The folder a session runs in when it belongs to no project:
/// `~/baaz-sessions`.
///
/// Muse needs a workspace root for every session, so "no project" still has
/// to mean somewhere real. One folder, made once at boot, is what lets a
/// first launch start a session before anything has been adopted — and what
/// keeps that promise without a folder picker standing between the person
/// and their first question.
///
/// Lowercase and hyphenated, with no space: Muse runs shell commands in it,
/// and a space in a workspace root is a quoting papercut in every one of
/// them. It does not share the application's own name, so "Muse runs in
/// baaz-sessions" names a folder rather than reading like the app.
///
/// A run with its own `BAAZ_STATE_DIR` — a test, a capture, a journey —
/// keeps its default workspace inside that directory instead, so a
/// disposable run never creates or writes in the real one.
pub fn default_workspace() -> PathBuf {
    if std::env::var_os("BAAZ_STATE_DIR").is_some() {
        return crate::store::support_dir().join("workspace");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("baaz-sessions")
}

/// Make [`default_workspace`] exist, and answer where it is.
///
/// Best-effort, like every other boot-time write: a folder that cannot be
/// created costs the unfiled lane, not the launch, so the caller gets `None`
/// and the rest of the window comes up as usual.
pub fn ensure_default_workspace() -> Option<PathBuf> {
    let root = default_workspace();
    match std::fs::create_dir_all(&root) {
        Ok(()) => Some(canonical_path(&root)),
        Err(error) => {
            crate::baaz_log!("default workspace: cannot create {}: {error}", root.display());
            None
        }
    }
}

/// Whether a directory is a plausible workspace to work in, as opposed to
/// somewhere a launch merely happened to start (decision D39).
///
/// `/` and `$HOME` are the two that matter. Neither is a project: both hold
/// the whole machine rather than one piece of work, and a launch that lands
/// on either learned nothing about what the person wants open. A bundled
/// `.app` opened from Finder always starts at `/`, so this is the common
/// case and not an edge one.
pub fn is_workspace_root(root: &Path) -> bool {
    if root == Path::new("/") {
        return false;
    }
    match std::env::var_os("HOME") {
        Some(home) => root != Path::new(&home),
        None => true,
    }
}

/// One batch's canonicalization answers: each distinct root is read from
/// the disk once, no matter how many rows name it.
///
/// [`crate::sidebar::SessionEntry::join`] canonicalizes its row's root and compares it
/// against every adopted root, so a cold list of N rows in P projects pays
/// N×(1+P) `canonicalize` syscalls on the UI thread. The rows of one
/// refresh share a handful of distinct roots, so the refresh hoists one of
/// these over its whole row loop instead: same answers (the function is
/// pure per input string within the pass), one read per distinct root.
#[derive(Default)]
pub struct CanonicalCache {
    /// Raw root → its canonical spelling, in insertion order.
    map: std::collections::HashMap<String, String>,
}

impl CanonicalCache {
    /// The canonical spelling of `root`, reading the disk on first sight.
    pub fn get(&mut self, root: &str) -> String {
        if let Some(hit) = self.map.get(root) {
            return hit.clone();
        }
        let canon = canonical_str(root);
        self.map.insert(root.to_owned(), canon.clone());
        canon
    }

    /// How many distinct roots have been read so far.
    pub fn len(&self) -> usize {
        self.map.len()
    }
}

impl Projects {
    /// Adopt `root`, or hand back the project that already holds it.
    ///
    /// A newcomer is named for the folder's last component, takes the
    /// least-used colour slot (ties go to the lowest), and is stamped now.
    pub fn add(&mut self, root: &Path) -> &Project {
        let canonical = canonical_path(root);
        if let Some(ix) = self.projects.iter().position(|p| canonical_path(&p.root) == canonical) {
            return &self.projects[ix];
        }
        let name = canonical
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| canonical.to_string_lossy().into_owned());
        let mut counts = [0usize; 8];
        for project in &self.projects {
            if (1..=8).contains(&project.colour) {
                counts[project.colour as usize - 1] += 1;
            }
        }
        let colour = counts.iter().enumerate().min_by_key(|(_, count)| **count).map(|(ix, _)| ix as u8 + 1).unwrap_or(1);
        let now = now_utc();
        self.projects.push(Project {
            id: uuid::Uuid::new_v4().to_string(),
            root: canonical,
            name,
            colour,
            pinned: false,
            added_at: now.clone(),
            last_opened_at: now,
            defaults: ProjectDefaults::default(),
        });
        self.projects.last().expect("just pushed")
    }

    /// The project with this id, if it is still adopted.
    pub fn find(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    /// The project holding this root, comparing canonical roots so symlinked
    /// spellings of one folder are one project.
    pub fn find_by_root(&self, root: &Path) -> Option<&Project> {
        let canonical = canonical_path(root);
        self.projects.iter().find(|p| canonical_path(&p.root) == canonical)
    }

    /// Forget the adoption. Sessions keep their rows — they resolve to
    /// Unfiled now — and a `current` pointing at it is cleared.
    /// Returns whether anything was forgotten.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.projects.len();
        self.projects.retain(|p| p.id != id);
        if self.current.as_deref() == Some(id) {
            self.current = None;
        }
        // A re-adopted id must not inherit the old answer.
        self.existence.get_mut().remove(id);
        self.projects.len() != before
    }

    /// A session of this project opened: it is the most recently opened now.
    pub fn touch(&mut self, id: &str) {
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == id) {
            project.last_opened_at = now_utc();
        }
    }

    /// Which project a session belongs to: its stored project id when that
    /// adoption still exists, else the adoption whose canonical root equals
    /// the row's `workspace_root`, else nothing. Never a prefix match: a
    /// worktree session's folder differs from its project's root, and a
    /// prefix would file it under the wrong project.
    pub fn resolve(&self, workspace_root: Option<&str>, meta_project: Option<&str>) -> Option<&Project> {
        if let Some(id) = meta_project {
            if let Some(project) = self.find(id) {
                return Some(project);
            }
        }
        let root = workspace_root.filter(|s| !s.is_empty())?;
        let canonical = canonical_path(Path::new(root));
        self.projects.iter().find(|p| canonical_path(&p.root) == canonical)
    }

    /// The sidebar order: pinned projects
    /// first, then by name case-insensitively, ties by `added_at` then id.
    /// Never recency: a project never moves because a session in it was
    /// created, opened, or got a turn. No drag reorder.
    ///
    /// `last_opened_at` and `added_at` stay on the record for
    /// [`Projects::most_recent_available`] (the boot fallback) and for a
    /// future `projectOrder` — but they decide nothing about this order.
    pub fn sorted(&self) -> Vec<&Project> {
        let mut out: Vec<&Project> = self.projects.iter().collect();
        out.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                .then_with(|| a.added_at.cmp(&b.added_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        out
    }

    /// Whether the adoption's root is on disk: a deleted worktree, an
    /// unmounted volume and a detached worktree all read false, and the
    /// adoption itself is untouched — it comes back when the path does.
    ///
    /// The answer is cached per project for [`EXISTS_TTL_SECS`], so frames
    /// never touch the disk; the list refresh rechecks every adoption off
    /// the UI thread (see [`Projects::refresh_availability`]) regardless.
    /// Purely a read: it never writes `projects.json`.
    pub fn is_available(&self, id: &str) -> bool {
        let now = now_secs();
        if let Some(&(exists, at)) = self.existence.borrow().get(id) {
            if now.saturating_sub(at) < EXISTS_TTL_SECS {
                return exists;
            }
        }
        let exists = self.find(id).is_some_and(|project| project.root.is_dir());
        self.existence.borrow_mut().insert(id.to_owned(), (exists, now));
        exists
    }

    /// Record root-exists answers computed off the UI thread (the list
    /// refresh stats every root beside the branch reads), resetting their
    /// TTL. Answers for forgotten adoptions are dropped, so the cache never
    /// outgrows the store. Memory only, like every other existence answer:
    /// it never writes `projects.json`.
    pub fn refresh_availability(&mut self, answers: &std::collections::HashMap<String, bool>) {
        let now = now_secs();
        let cache = self.existence.get_mut();
        for (id, exists) in answers {
            cache.insert(id.clone(), (*exists, now));
        }
        cache.retain(|id, _| self.projects.iter().any(|project| &project.id == id));
    }

    /// The adoption when its root is on disk; a missing root is not shown
    /// anywhere, but stays adopted.
    pub fn find_available(&self, id: &str) -> Option<&Project> {
        self.find(id).filter(|project| self.is_available(&project.id))
    }

    /// [`Projects::sorted`], minus the adoptions whose root is gone: what
    /// the sidebar, the Projects palette, the project menu and the rail
    /// list. Sessions of a skipped project resolve nowhere (see
    /// [`Projects::resolve_available`]) and fall back to Unfiled
    /// like any unadopted workspace.
    pub fn sorted_available(&self) -> Vec<&Project> {
        self.sorted().into_iter().filter(|project| self.is_available(&project.id)).collect()
    }

    /// The most recently opened adoption whose root is on disk: the boot
    /// and removal fallbacks skip a missing current rather than showing it.
    pub fn most_recent_available(&self) -> Option<&Project> {
        self.projects
            .iter()
            .filter(|project| self.is_available(&project.id))
            .max_by_key(|project| project.last_opened_at.clone())
    }

    /// The current adoption when its root is on disk, else the most
    /// recently opened one that is: a missing root is never current, but
    /// stays adopted, so it resumes being current when the path does — if
    /// nothing in between claimed it.
    pub fn effective_current(&self) -> Option<&Project> {
        self.current.as_deref().and_then(|id| self.find_available(id)).or_else(|| self.most_recent_available())
    }

    /// [`Projects::resolve`], but a missing root resolves nowhere: the row
    /// falls back to Unfiled while the adoption — and the
    /// session's stored project id — stay put, so the session regroups
    /// under its project when the path comes back.
    pub fn resolve_available(
        &self,
        workspace_root: Option<&str>,
        meta_project: Option<&str>,
    ) -> Option<&Project> {
        if let Some(id) = meta_project {
            if let Some(project) = self.find_available(id) {
                return Some(project);
            }
        }
        let root = workspace_root.filter(|s| !s.is_empty())?;
        let canonical = canonical_path(Path::new(root));
        self.projects
            .iter()
            .filter(|project| self.is_available(&project.id))
            .find(|project| canonical_path(&project.root) == canonical)
    }

    /// [`Self::resolve_available`], reading every root through `cache` so a
    /// batch of rows pays one `canonicalize` per distinct root instead of
    /// one per row per project. Answers are identical: the cache is a pure
    /// memo of [`canonical_str`] within the pass.
    pub fn resolve_available_cached(
        &self,
        workspace_root: Option<&str>,
        meta_project: Option<&str>,
        cache: &mut CanonicalCache,
    ) -> Option<&Project> {
        if let Some(id) = meta_project {
            if let Some(project) = self.find_available(id) {
                return Some(project);
            }
        }
        let root = workspace_root.filter(|s| !s.is_empty())?;
        let canonical = cache.get(root);
        self.projects
            .iter()
            .filter(|project| self.is_available(&project.id))
            .find(|project| {
                let stored = project.root.to_string_lossy();
                cache.get(stored.as_ref()) == canonical
            })
    }
}

/// The branch `root` is on: `git symbolic-ref --short HEAD`, so a detached
/// head is `None` rather than a guess.
///
/// Blocking; call it on the background executor. `None` on any failure, and
/// it never logs: a folder that is not a checkout is ordinary, not a warning.
pub fn branch_of(root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["symbolic-ref", "--short", "-q", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?;
    let branch = branch.trim();
    if branch.is_empty() {
        return None;
    }
    Some(branch.to_owned())
}

/// A view's reasoning effort as the store spells it: MSP's own camelCase
/// (`"high"`, `"xhigh"`), so the stored string parses back without a table.
pub fn effort_string(effort: Option<aui_protocol::ReasoningEffort>) -> Option<String> {
    effort
        .and_then(|effort| serde_json::to_value(effort).ok())
        .and_then(|value| value.as_str().map(str::to_owned))
}

/// [`effort_string`] in reverse: an unknown spelling is no effort rather than
/// an error the person has to see.
pub fn parse_effort(text: &str) -> Option<aui_protocol::ReasoningEffort> {
    serde_json::from_value::<aui_protocol::ReasoningEffort>(serde_json::Value::String(text.to_owned())).ok()
}

/// The shared shape behind [`start_params`] and [`start_params_for_root`]:
/// a fresh command id, `root` canonicalized (so `/tmp/x` and `/private/tmp/x`
/// are the one workspace `session/list` will later filter on), and whatever
/// model default and approval mode the caller already resolved.
fn build_start_params(
    root: &Path,
    provider: &str,
    model_id: Option<String>,
    approval_mode: Option<muse_client::schema::ApprovalMode>,
) -> muse_client::schema::SessionStartParams {
    muse_client::schema::SessionStartParams {
        command_id: muse_client::new_command_id(),
        workspace_root: Some(canonical_path(root).to_string_lossy().into_owned()),
        provider_id: Some(provider.to_owned()),
        model_id,
        approval_mode,
        ..Default::default()
    }
}

/// The `session/start` params for a new session in `project_id`: the
/// project's root and defaults, with the command line's approval mode
/// winning over the stored one. `None` when there is no such project, which
/// is the caller's cue to start nothing (the hero owns that state).
pub fn start_params(
    projects: &Projects,
    project_id: Option<&str>,
    provider: &str,
    cli_approval: Option<muse_client::schema::ApprovalMode>,
) -> Option<muse_client::schema::SessionStartParams> {
    let project = project_id.and_then(|id| projects.find(id))?;
    Some(build_start_params(
        &project.root,
        provider,
        project.defaults.model_id.clone(),
        cli_approval.or_else(|| project.defaults.approval_mode.clone()),
    ))
}

/// The `session/start` params for a session in `root` that is not being
/// adopted as a project (the "start here without remembering it" path):
/// no project to draw a model or approval-mode default from, so `model_id`
/// is `None` and the command line's approval mode is the only one in play.
pub fn start_params_for_root(
    root: &Path,
    provider: &str,
    cli_approval: Option<muse_client::schema::ApprovalMode>,
) -> muse_client::schema::SessionStartParams {
    build_start_params(root, provider, None, cli_approval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Point `BAAZ_STATE_DIR` at `dir` for the test's duration, restoring
    /// whatever was there before. The store-wide lock serializes every test
    /// that touches the variable, since two tests pointing it at two dirs at
    /// once would read each other's state.
    fn with_state_dir(dir: &Path) -> (std::sync::MutexGuard<'static, ()>, Option<OsString>) {
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", dir);
        (guard, old)
    }

    /// Undo [`with_state_dir`]: put the old value back before releasing the
    /// lock, so no test leaks its dir into another.
    fn restore_state_dir(guard: std::sync::MutexGuard<'static, ()>, old: Option<OsString>) {
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    fn state_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("baaz-projects-{name}-{}", std::process::id()))
    }

    fn project_with(root: &str, colour: u8, pinned: bool) -> Project {
        Project {
            id: format!("id-{root}"),
            root: PathBuf::from(root),
            name: root.into(),
            colour,
            pinned,
            added_at: "2026-09-13T10:00:00Z".into(),
            last_opened_at: "2026-09-13T10:00:00Z".into(),
            defaults: ProjectDefaults::default(),
        }
    }

    #[test]
    fn a_store_round_trips_through_camel_case() {
        let dir = state_dir("round-trip");
        let (guard, old) = with_state_dir(&dir);
        let mut projects = Projects::default();
        let root = dir.join("ws");
        std::fs::create_dir_all(&root).expect("temp workspace");
        let id = projects.add(&root).id.clone();
        projects.current = Some(id.clone());
        projects.find(&id).expect("added");
        write(&projects);
        // The file names its fields the design's camelCase.
        let text = std::fs::read_to_string(path()).expect("projects.json");
        assert!(text.contains("\"addedAt\""), "addedAt must be camelCase: {text}");
        assert!(text.contains("\"lastOpenedAt\""), "lastOpenedAt must be camelCase: {text}");
        assert!(!text.contains("modelId"), "an empty default writes no modelId: {text}");
        let back = read();
        assert_eq!(back, projects);
        assert_eq!(back.version, 1);
        let _ = std::fs::remove_dir_all(&dir);
        restore_state_dir(guard, old);
    }

    #[test]
    fn adding_the_same_root_twice_is_one_project() {
        let mut projects = Projects::default();
        let dir = state_dir("add-twice");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let first = projects.add(&dir).clone();
        let second = projects.add(&dir).clone();
        assert_eq!(projects.projects.len(), 1);
        assert_eq!(first.id, second.id);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlinked_spelling_is_the_same_project() {
        let base = state_dir("symlink");
        let real = base.join("real");
        std::fs::create_dir_all(&real).expect("temp dir");
        #[cfg(unix)]
        {
            let link = base.join("link");
            std::os::unix::fs::symlink(&real, &link).expect("symlink");
            let mut projects = Projects::default();
            let first = projects.add(&real).id.clone();
            let second = projects.add(&link).id.clone();
            assert_eq!(first, second);
            assert_eq!(projects.projects.len(), 1);
        }
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn tmp_and_private_tmp_are_the_same_project() {
        let dir = PathBuf::from(format!("/tmp/baaz-projects-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut projects = Projects::default();
        let first = projects.add(&dir).id.clone();
        let aliased = PathBuf::from(format!("/private/tmp/baaz-projects-tmp-{}", std::process::id()));
        let second = projects.add(&aliased).id.clone();
        assert_eq!(first, second);
        assert_eq!(projects.projects.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_newcomer_takes_the_least_used_colour() {
        let mut projects = Projects::default();
        assert_eq!(projects.add(Path::new("/x/a")).colour, 1);
        assert_eq!(projects.add(Path::new("/x/b")).colour, 2);
        // Colours 1, 1, 2 in use: the next least-used slot is 3.
        projects.projects[0].colour = 1;
        projects.projects[1].colour = 1;
        projects.projects.push(project_with("/x/c", 2, false));
        assert_eq!(projects.add(Path::new("/x/d")).colour, 3);
    }

    #[test]
    fn resolve_prefers_the_stored_id_then_the_root_and_never_a_prefix() {
        let mut projects = Projects::default();
        projects.projects.push(project_with("/work/a", 1, false));
        projects.projects[0].id = "aaa".into();
        projects.projects.push(project_with("/work/b", 2, false));
        projects.projects[1].id = "bbb".into();
        // The stored id beats the root.
        assert_eq!(projects.resolve(Some("/work/a"), Some("bbb")).map(|p| p.id.as_str()), Some("bbb"));
        // A removed id falls through to the root.
        assert_eq!(projects.resolve(Some("/work/a"), Some("gone")).map(|p| p.id.as_str()), Some("aaa"));
        // A subfolder is not the project.
        assert_eq!(projects.resolve(Some("/work/a/child"), None), None);
        // A sibling prefix is not the project either.
        assert_eq!(projects.resolve(Some("/work/abc"), None), None);
        assert_eq!(projects.resolve(None, None), None);
        assert_eq!(projects.resolve(Some(""), None), None);
    }

    #[test]
    fn removing_forgets_and_clears_a_dangling_current() {
        let mut projects = Projects::default();
        projects.projects.push(project_with("/work/a", 1, false));
        projects.projects[0].id = "aaa".into();
        projects.current = Some("aaa".into());
        assert!(projects.remove("aaa"));
        assert!(!projects.remove("aaa"));
        assert!(projects.current.is_none());
    }

    #[test]
    fn touching_bumps_last_opened_and_most_recent_follows() {
        let base = state_dir("most-recent");
        for child in ["a", "b"] {
            std::fs::create_dir_all(base.join(child)).expect("temp dir");
        }
        let mut projects = Projects::default();
        let mut a = project_with(base.join("a").to_str().expect("utf8"), 1, false);
        a.id = "aaa".into();
        a.last_opened_at = "2020-01-01T00:00:00Z".into();
        projects.projects.push(a);
        let mut b = project_with(base.join("b").to_str().expect("utf8"), 2, false);
        b.id = "bbb".into();
        b.last_opened_at = "2021-01-01T00:00:00Z".into();
        projects.projects.push(b);
        assert_eq!(projects.most_recent_available().map(|p| p.id.as_str()), Some("bbb"));
        projects.touch("aaa");
        assert_eq!(projects.most_recent_available().map(|p| p.id.as_str()), Some("aaa"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Pinned first whatever the stamps say, then name order: the one
    /// pinned-first property the recency order had that the name order keeps.
    #[test]
    fn a_pinned_project_leads_whatever_its_activity() {
        let mut projects = Projects::default();
        let mut zebra = project_with("/work/zebra", 1, true);
        zebra.id = "zebra".into();
        zebra.name = "zebra".into();
        // Ancient stamps on the pinned project: under recency it would sink.
        zebra.added_at = "2020-01-01T00:00:00Z".into();
        zebra.last_opened_at = "2020-01-01T00:00:00Z".into();
        projects.projects.push(zebra);
        let mut alpha = project_with("/work/alpha", 2, false);
        alpha.id = "alpha".into();
        alpha.name = "alpha".into();
        projects.projects.push(alpha);
        let order: Vec<&str> = projects.sorted().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["zebra", "alpha"]);
    }

    #[test]
    fn sorting_is_pinned_first_then_name_case_insensitive() {
        let mut projects = Projects::default();
        for (root, name, pinned) in
            [("/work/beta", "beta", false), ("/work/Alpha", "Alpha", false), ("/work/pinned", "pinned", true)]
        {
            let mut project = project_with(root, 1, pinned);
            project.id = name.into();
            project.name = name.into();
            projects.projects.push(project);
        }
        let order: Vec<&str> = projects.sorted().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["pinned", "Alpha", "beta"]);
    }

    /// O1: touching, opening and newer sessions never reorder the groups.
    /// A touch rewrites `last_opened_at`, but the name order never reads it.
    #[test]
    fn order_is_by_name_never_by_activity() {
        fn order(projects: &Projects) -> Vec<String> {
            projects.sorted().iter().map(|p| p.id.clone()).collect()
        }
        let mut projects = Projects::default();
        for (id, name) in [("mmm", "mmm"), ("aaa", "aaa")] {
            let mut project = project_with(&format!("/work/{id}"), 1, false);
            project.id = id.into();
            project.name = name.into();
            project.added_at = "2026-09-13T10:00:00Z".into();
            project.last_opened_at = "2026-09-13T10:00:00Z".into();
            projects.projects.push(project);
        }
        assert_eq!(order(&projects), vec!["aaa", "mmm"]);
        // Touching (what opening a session used to do) moves no group.
        projects.touch("mmm");
        assert_eq!(order(&projects), vec!["aaa", "mmm"]);
        // Neither does a newer adoption stamp on the later-named project.
        projects.projects.iter_mut().find(|p| p.id == "mmm").expect("mmm").added_at =
            "2026-09-14T10:00:00Z".into();
        assert_eq!(order(&projects), vec!["aaa", "mmm"]);
    }

    /// O1: a freshly adopted project lands at its name, not on top. The
    /// header crumb names it and scroll-into-view finds it instead.
    #[test]
    fn a_freshly_adopted_project_sorts_by_name() {
        let mut projects = Projects::default();
        let mut used = project_with("/work/used", 1, false);
        used.id = "used".into();
        used.name = "used".into();
        projects.projects.push(used);
        let mut fresh = project_with("/work/fresh", 2, false);
        fresh.id = "fresh".into();
        fresh.name = "fresh".into();
        // Adopted now; never opened, so no session activity at all.
        fresh.added_at = "2026-09-13T16:00:00Z".into();
        fresh.last_opened_at = String::new();
        projects.projects.push(fresh);

        let order: Vec<&str> = projects.sorted().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["fresh", "used"]);
    }

    /// Name ties (case-insensitively) fall back to `added_at`, then id — so
    /// even unparseable stamps order deterministically and never by activity.
    #[test]
    fn name_ties_break_by_added_at_then_id() {
        let mut projects = Projects::default();
        for (id, added_at) in [("second", "2026-09-13T11:00:00Z"), ("first", "2026-09-13T10:00:00Z")] {
            let mut project = project_with(&format!("/work/{id}"), 1, false);
            project.id = id.into();
            project.name = "Same".into();
            project.added_at = added_at.into();
            projects.projects.push(project);
        }
        let order: Vec<&str> = projects.sorted().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["first", "second"]);
    }

    #[test]
    fn a_corrupt_file_reads_as_empty() {
        let dir = state_dir("corrupt");
        let (guard, old) = with_state_dir(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        std::fs::write(path(), b"not json").expect("corrupt file");
        let projects = read();
        assert!(projects.projects.is_empty());
        assert!(projects.current.is_none());
        let _ = std::fs::remove_dir_all(&dir);
        restore_state_dir(guard, old);
    }

    #[test]
    fn a_missing_file_reads_as_an_empty_versioned_store() {
        let dir = state_dir("missing");
        let (guard, old) = with_state_dir(&dir);
        let projects = read();
        assert_eq!(projects, Projects::default());
        assert_eq!(projects.version, 1);
        restore_state_dir(guard, old);
    }

    #[test]
    fn a_branch_is_none_outside_a_checkout() {
        let dir = state_dir("no-git");
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert_eq!(branch_of(&dir), None);
        assert_eq!(branch_of(Path::new("/nonexistent-baaz-root")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn effort_spells_round_trip_through_json() {
        use aui_protocol::ReasoningEffort;
        assert_eq!(effort_string(Some(ReasoningEffort::High)).as_deref(), Some("high"));
        assert_eq!(effort_string(Some(ReasoningEffort::Xhigh)).as_deref(), Some("xhigh"));
        assert_eq!(effort_string(None), None);
        assert_eq!(parse_effort("high"), Some(ReasoningEffort::High));
        assert_eq!(parse_effort("xhigh"), Some(ReasoningEffort::Xhigh));
        assert_eq!(parse_effort("nonsense"), None);
    }

    #[test]
    fn start_params_carry_the_project_and_its_defaults() {
        use muse_client::schema::ApprovalMode;
        let mut projects = Projects::default();
        let dir = state_dir("start-params");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let id = projects.add(&dir).id.clone();
        let project = projects.projects.iter_mut().find(|p| p.id == id).expect("added");
        project.defaults.model_id = Some("muse-spark-1.3".into());
        project.defaults.effort = Some("high".into());
        project.defaults.approval_mode = Some(ApprovalMode::OnRequest);
        // No project, no params: the caller starts nothing.
        assert!(start_params(&projects, None, "meta", None).is_none());
        assert!(start_params(&projects, Some("gone"), "meta", None).is_none());
        let params = start_params(&projects, Some(&id), "meta", None).expect("params");
        assert_eq!(params.workspace_root.as_deref(), Some(canonical_str(&dir.to_string_lossy()).as_str()));
        assert_eq!(params.model_id.as_deref(), Some("muse-spark-1.3"));
        assert_eq!(params.approval_mode, Some(ApprovalMode::OnRequest));
        assert_eq!(params.provider_id.as_deref(), Some("meta"));
        // The command line wins over the stored mode.
        let params = start_params(&projects, Some(&id), "meta", Some(ApprovalMode::AllowAll)).expect("params");
        assert_eq!(params.approval_mode, Some(ApprovalMode::AllowAll));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn start_params_for_root_carries_no_project_defaults() {
        use muse_client::schema::ApprovalMode;
        let dir = state_dir("start-params-root");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let params = start_params_for_root(&dir, "meta", None);
        assert_eq!(params.workspace_root.as_deref(), Some(canonical_str(&dir.to_string_lossy()).as_str()));
        assert_eq!(params.model_id, None);
        assert_eq!(params.approval_mode, None);
        assert_eq!(params.provider_id.as_deref(), Some("meta"));
        // The command line still wins when it names a mode, exactly as it
        // does for a project's own defaults.
        let params = start_params_for_root(&dir, "meta", Some(ApprovalMode::AllowAll));
        assert_eq!(params.approval_mode, Some(ApprovalMode::AllowAll));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two adoptions on disk, one whose root is gone: the store keeps both,
    /// every listing shows one. The base name is per test: parallel tests
    /// must never share a temp dir one of them removes at the end.
    fn availability_store(name: &str) -> (PathBuf, Projects) {
        let base = state_dir(name);
        let here = base.join("here");
        std::fs::create_dir_all(&here).expect("temp dir");
        let mut projects = Projects::default();
        let mut present = project_with(here.to_str().expect("utf8"), 1, false);
        present.id = "present".into();
        present.name = "present".into();
        projects.projects.push(present);
        // Never created: the deleted worktree.
        let mut gone = project_with(base.join("gone").to_str().expect("utf8"), 2, false);
        gone.id = "gone".into();
        gone.name = "gone".into();
        gone.last_opened_at = "2026-09-14T10:00:00Z".into();
        projects.projects.push(gone);
        (base, projects)
    }

    #[test]
    fn a_missing_root_is_skipped_but_stays_adopted() {
        let (base, projects) = availability_store("availability-skipped");
        // The store still holds both: hiding is a listing rule, and a folder
        // on an unmounted volume comes back when the path does.
        assert_eq!(projects.projects.len(), 2);
        assert!(projects.find("gone").is_some());
        // Every listing shows one.
        assert!(!projects.is_available("gone"));
        assert!(projects.is_available("present"));
        assert!(projects.find_available("gone").is_none());
        let listed: Vec<&str> = projects.sorted_available().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(listed, vec!["present"]);
        // The pure order is untouched: `sorted` still names both.
        assert_eq!(projects.sorted().len(), 2);
        // Sessions of the missing root resolve nowhere — Unfiled.
        assert!(projects.resolve_available(Some("gone-root"), Some("gone")).is_none());
        assert!(projects.resolve_available(Some(projects.projects[1].root.to_str().expect("utf8")), None).is_none());
        // A present root resolves exactly as before.
        let root = projects.projects[0].root.to_str().expect("utf8").to_owned();
        assert_eq!(
            projects.resolve_available(Some(&root), None).map(|p| p.id.as_str()),
            projects.resolve(Some(&root), None).map(|p| p.id.as_str()),
        );
        assert_eq!(projects.resolve_available(Some(&root), None).map(|p| p.id.as_str()), Some("present"));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn filtering_writes_nothing_back_to_the_store() {
        let dir = state_dir("availability-quiet");
        let (guard, old) = with_state_dir(&dir);
        let (base, projects) = availability_store("availability-quiet-store");
        write(&projects);
        let before = std::fs::read(path()).expect("projects.json");
        // Every read in the listing path, twice over for the cache.
        let _ = projects.sorted_available();
        let _ = projects.sorted_available();
        let _ = projects.resolve_available(None, Some("gone"));
        let _ = projects.most_recent_available();
        let _ = projects.effective_current();
        assert_eq!(std::fs::read(path()).expect("projects.json"), before);
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&base);
        restore_state_dir(guard, old);
    }

    #[test]
    fn a_missing_current_falls_back_to_the_most_recent_present_root() {
        let (base, mut projects) = availability_store("availability-current");
        projects.current = Some("gone".into());
        assert_eq!(projects.effective_current().map(|p| p.id.as_str()), Some("present"));
        // Nothing on disk: no current at all, but the adoptions survive.
        std::fs::remove_dir_all(base.join("here")).expect("remove root");
        // A fresh store: the removed root reads missing even though nothing
        // rechecked it in between (no stale positive from the earlier read).
        let cold = Projects { projects: projects.projects.clone(), ..Projects::default() };
        assert!(cold.effective_current().is_none());
        assert_eq!(cold.projects.len(), 2);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_refresh_resets_a_stale_answer_and_prunes_the_forgotten() {
        let base = state_dir("availability-refresh");
        let root = base.join("ws");
        let mut projects = Projects::default();
        let mut project = project_with(root.to_str().expect("utf8"), 1, false);
        project.id = "ws".into();
        projects.projects.push(project);
        // Missing, and the negative answer sticks inside the TTL.
        assert!(!projects.is_available("ws"));
        std::fs::create_dir_all(&root).expect("temp dir");
        assert!(!projects.is_available("ws"), "the TTL holds the stale answer");
        // The list refresh rechecks off-thread: its answers reset the TTL.
        let mut answers = std::collections::HashMap::new();
        answers.insert("ws".to_owned(), true);
        answers.insert("forgotten".to_owned(), true);
        projects.refresh_availability(&answers);
        assert!(projects.is_available("ws"));
        // Forgetting drops the adoption and its answer: a re-added id starts cold.
        assert!(projects.remove("ws"));
        let mut project = project_with(root.to_str().expect("utf8"), 1, false);
        project.id = "ws".into();
        projects.projects.push(project);
        std::fs::remove_dir_all(&root).expect("remove root");
        assert!(!projects.is_available("ws"), "no stale positive survives forgetting");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// The unfiled lane's folder. A disposable run must never create or
    /// write the real `~/Baaz`, so a state dir of its own redirects it.
    #[test]
    fn the_default_workspace_follows_a_disposable_state_dir() {
        let dir = state_dir("default-workspace");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let (guard, old) = with_state_dir(&dir);
        let root = default_workspace();
        assert_eq!(root, dir.join("workspace"), "a run with its own state dir keeps its workspace there");
        assert!(!root.exists(), "nothing exists before it is asked for");
        let made = ensure_default_workspace().expect("the folder is creatable");
        assert!(made.is_dir(), "asking for it makes it");
        // Idempotent: a second boot finds it rather than failing on it.
        assert!(ensure_default_workspace().is_some());
        // And it is a workspace, so the `@` picker will actually walk it.
        assert!(is_workspace_root(&made));
        restore_state_dir(guard, old);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// D39's rule, which decides both what boot adopts and whether the `@`
    /// and `/` menus are primed at all. A bundle opened from Finder starts at
    /// `/`, so getting this wrong walks the whole disk on first launch.
    #[test]
    fn neither_the_filesystem_root_nor_home_is_a_workspace() {
        assert!(!is_workspace_root(Path::new("/")), "/ holds the machine, not one piece of work");
        if let Some(home) = std::env::var_os("HOME") {
            assert!(!is_workspace_root(Path::new(&home)), "$HOME is where a launch lands, not a project");
            // A directory inside $HOME is ordinary: only $HOME itself is out.
            assert!(is_workspace_root(&Path::new(&home).join("Projects").join("thing")));
        }
        assert!(is_workspace_root(Path::new("/tmp/some-checkout")));
    }
}
