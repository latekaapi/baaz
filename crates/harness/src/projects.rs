//! Projects: the workspaces this window has adopted (design `docs/12-projects.md` §4).
//!
//! A project is an adopted root with an identity of its own: a UUID, a stable
//! colour, a name that can be changed without touching the folder, and the
//! last-used model, effort and approval mode new sessions start with. The file
//! is `~/Library/Application Support/harness/projects.json`, camelCase,
//! written atomically through [`crate::store`], and every read is
//! best-effort: a missing or unparseable file is an empty store, which loses
//! an adoption and never a session.
//!
//! A session resolves to a project in one order only: the `project` id the
//! harness wrote into its own `sessions.json` when that id still names a
//! project; else the project whose canonical root equals the row's
//! `workspace_root`; else nothing. Never by prefix, never by the current
//! project — a worktree session's folder differs from its project's root, and
//! a prefix match would adopt it into the wrong project.

use std::collections::HashMap;
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
}

impl Default for Projects {
    fn default() -> Self {
        Self { version: VERSION, current: None, projects: Vec::new() }
    }
}

/// `~/Library/Application Support/harness/projects.json`.
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
/// apart. Frozen under `HARNESS_DETERMINISTIC=1`, every adoption in a run
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
    /// "Other workspaces" now — and a `current` pointing at it is cleared.
    /// Returns whether anything was forgotten.
    ///
    /// Package 2's project menu calls this; package 1 only re-resolves after
    /// it.
    #[allow(dead_code)]
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.projects.len();
        self.projects.retain(|p| p.id != id);
        if self.current.as_deref() == Some(id) {
            self.current = None;
        }
        self.projects.len() != before
    }

    /// A session of this project opened: it is the most recently opened now.
    pub fn touch(&mut self, id: &str) {
        if let Some(project) = self.projects.iter_mut().find(|p| p.id == id) {
            project.last_opened_at = now_utc();
        }
    }

    /// The most recently opened adoption, for boot when no stored current
    /// survives.
    pub fn most_recent(&self) -> Option<&Project> {
        self.projects.iter().max_by_key(|p| p.last_opened_at.clone())
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

    /// The sidebar order (decision D37): pinned projects first, then by
    /// recency, then by name. No drag reorder.
    ///
    /// Recency is [`recency`]: the newest of the project's last session
    /// activity — supplied by the caller as project id to epoch millis, since
    /// activity lives in the session list, not here — its `last_opened_at`,
    /// and its `added_at`. Keying on session activity alone sank a project
    /// the moment it was adopted, because a folder with no sessions yet has
    /// no activity at all and sorted as 0, under every project that had ever
    /// been used (D3). Adoption is itself the freshest thing about it.
    pub fn sorted(&self, activity: &HashMap<String, i64>) -> Vec<&Project> {
        let mut out: Vec<&Project> = self.projects.iter().collect();
        out.sort_by(|a, b| b.pinned.cmp(&a.pinned).then_with(|| recency(b, activity).cmp(&recency(a, activity))).then_with(|| a.name.cmp(&b.name)));
        out
    }
}

/// How recently a project was touched, in epoch millis: the newest of its
/// session activity, the last time one of its sessions was opened, and its
/// adoption. A freshly adopted project has only the last of those, which is
/// exactly why it counts — see [`Projects::sorted`].
///
/// The two stored stamps are RFC3339 UTC; an unparseable or empty one
/// contributes nothing rather than poisoning the key.
fn recency(project: &Project, activity: &HashMap<String, i64>) -> i64 {
    let stamp = |s: &str| chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.timestamp_millis()).unwrap_or(i64::MIN);
    activity
        .get(&project.id)
        .copied()
        .unwrap_or(i64::MIN)
        .max(stamp(&project.last_opened_at))
        .max(stamp(&project.added_at))
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
    Some(muse_client::schema::SessionStartParams {
        command_id: muse_client::new_command_id(),
        workspace_root: Some(project.root.to_string_lossy().into_owned()),
        provider_id: Some(provider.to_owned()),
        model_id: project.defaults.model_id.clone(),
        approval_mode: cli_approval.or_else(|| project.defaults.approval_mode.clone()),
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    /// Point `HARNESS_STATE_DIR` at `dir` for the test's duration, restoring
    /// whatever was there before. The store-wide lock serializes every test
    /// that touches the variable, since two tests pointing it at two dirs at
    /// once would read each other's state.
    fn with_state_dir(dir: &Path) -> (std::sync::MutexGuard<'static, ()>, Option<OsString>) {
        let guard = crate::store::test_env_lock().lock().expect("test env lock");
        let old = std::env::var_os("HARNESS_STATE_DIR");
        std::env::set_var("HARNESS_STATE_DIR", dir);
        (guard, old)
    }

    /// Undo [`with_state_dir`]: put the old value back before releasing the
    /// lock, so no test leaks its dir into another.
    fn restore_state_dir(guard: std::sync::MutexGuard<'static, ()>, old: Option<OsString>) {
        match old {
            Some(value) => std::env::set_var("HARNESS_STATE_DIR", value),
            None => std::env::remove_var("HARNESS_STATE_DIR"),
        }
        drop(guard);
    }

    fn state_dir(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("harness-projects-{name}-{}", std::process::id()))
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
        let dir = PathBuf::from(format!("/tmp/harness-projects-tmp-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut projects = Projects::default();
        let first = projects.add(&dir).id.clone();
        let aliased = PathBuf::from(format!("/private/tmp/harness-projects-tmp-{}", std::process::id()));
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
        let mut projects = Projects::default();
        projects.projects.push(project_with("/work/a", 1, false));
        projects.projects[0].id = "aaa".into();
        projects.projects[0].last_opened_at = "2020-01-01T00:00:00Z".into();
        projects.projects.push(project_with("/work/b", 2, false));
        projects.projects[1].id = "bbb".into();
        projects.projects[1].last_opened_at = "2021-01-01T00:00:00Z".into();
        assert_eq!(projects.most_recent().map(|p| p.id.as_str()), Some("bbb"));
        projects.touch("aaa");
        assert_eq!(projects.most_recent().map(|p| p.id.as_str()), Some("aaa"));
    }

    /// Epoch millis for an RFC3339 stamp, so a test's activity map speaks the
    /// same units the store's own stamps do.
    fn millis(stamp: &str) -> i64 {
        chrono::DateTime::parse_from_rfc3339(stamp).expect("stamp").timestamp_millis()
    }

    #[test]
    fn sorting_is_pinned_first_then_newest_activity_then_name() {
        let mut projects = Projects::default();
        projects.projects.push(project_with("/work/beta", 1, false));
        projects.projects[0].id = "beta".into();
        projects.projects.push(project_with("/work/alpha", 2, false));
        projects.projects[1].id = "alpha".into();
        projects.projects.push(project_with("/work/pinned", 3, true));
        projects.projects[2].id = "pinned".into();
        // Both stamps on the helper's projects are 2026-09-13T10:00:00Z, so
        // activity has to be later than that to be the newest thing about a
        // project — as it is in the app, where both are epoch millis.
        let mut activity = HashMap::new();
        activity.insert("beta".to_owned(), millis("2026-09-13T12:00:00Z"));
        activity.insert("alpha".to_owned(), millis("2026-09-13T11:00:00Z"));
        let order: Vec<&str> = projects.sorted(&activity).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["pinned", "beta", "alpha"]);
        // No activity anywhere: every project falls back to its own stamps,
        // which tie here, so ties break by name.
        let order: Vec<&str> = projects.sorted(&HashMap::new()).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["pinned", "alpha", "beta"]);
    }

    /// D3: a project adopted a moment ago has no sessions, so it has no
    /// activity — and used to sort under every project that had ever been
    /// used, i.e. straight to the bottom of the sidebar, which is the one
    /// place the person who just adopted it will not look.
    #[test]
    fn a_freshly_adopted_project_sorts_to_the_top() {
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

        let mut activity = HashMap::new();
        activity.insert("used".to_owned(), millis("2026-09-13T15:00:00Z"));

        let order: Vec<&str> = projects.sorted(&activity).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["fresh", "used"], "adoption is the freshest thing about a project with no sessions");

        // And it still yields to a project used after it was adopted.
        activity.insert("used".to_owned(), millis("2026-09-13T17:00:00Z"));
        let order: Vec<&str> = projects.sorted(&activity).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["used", "fresh"]);
    }

    /// An empty or malformed stamp contributes nothing rather than sorting a
    /// project to one end: a store written by an older build has neither.
    #[test]
    fn an_unparseable_stamp_does_not_decide_the_order() {
        let mut projects = Projects::default();
        let mut broken = project_with("/work/broken", 1, false);
        broken.id = "broken".into();
        broken.name = "broken".into();
        broken.added_at = "not a date".into();
        broken.last_opened_at = String::new();
        projects.projects.push(broken);
        let mut good = project_with("/work/good", 2, false);
        good.id = "good".into();
        good.name = "good".into();
        projects.projects.push(good);
        let order: Vec<&str> = projects.sorted(&HashMap::new()).iter().map(|p| p.id.as_str()).collect();
        assert_eq!(order, vec!["good", "broken"], "a project with no usable stamp sorts last, not first");
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
        assert_eq!(branch_of(Path::new("/nonexistent-harness-root")), None);
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
}
