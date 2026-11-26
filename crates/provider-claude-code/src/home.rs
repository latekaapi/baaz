//! Baaz's own Claude home: `CLAUDE_CONFIG_DIR` for every claude child.
//!
//! Baaz used to write into the owner's `~/.claude` (`projects/`,
//! `sessions/`), which is exactly the store Claude for Mac scans — and a
//! Baaz launched from a desktop agent session stamped its transcripts as
//! the desktop's own. Children now run with
//! `CLAUDE_CONFIG_DIR=<state dir>/claude-home` plus an empty
//! `CLAUDE_SECURESTORAGE_CONFIG_DIR` (empty, not unset: only the empty form
//! keeps the shared `Claude Code-credentials` Keychain item instead of a
//! suffixed one that does not exist).
//!
//! The home holds symlinks to the owner's `settings.json`, `skills/` and
//! `plugins/` (plus `CLAUDE.md`, `agents/` and `commands/` when they
//! exist): hooks, permissions, skills and plugins keep applying, while
//! `projects/` and `sessions/` stay private to Baaz. Links are created only
//! when absent — never copied, never overwritten.
//!
//! One folder is shared on purpose (docs/85 10.1): Claude's per-project
//! auto memory. Every Baaz-homed child carries `--settings
//! {"autoMemoryDirectory": …}` naming `~/.claude/projects/<slug>/memory` —
//! the folder the terminal's `claude` reads and writes for the same repo
//! ([`memory_dir`]; a worktree or subfolder uses its main repo's slug,
//! [`project_root`]). So the agent may write notes there; Baaz itself
//! writes nothing inside the owner's `~/.claude`, and transcripts stay in
//! the private home. Under redirected state (`BAAZ_STATE_DIR`, which the
//! check build always sets) the folder is `<state>/claude-memory/<slug>/`
//! instead, so a scratch run never touches the owner's files (rule 25).

use std::path::{Path, PathBuf};

/// The Baaz-owned Claude home's name under the state dir.
pub const HOME_DIR_NAME: &str = "claude-home";

/// Owner entries symlinked into the Baaz home when they exist.
pub const LINKED_ENTRIES: &[&str] =
    &["settings.json", "plugins", "CLAUDE.md", "agents", "commands"];

/// The skill folders whose skills the home's `skills/` links one by one
/// (docs/85 16.2), first wins on a name both hold: the owner's own Claude
/// skills, then the shared `~/.agents/skills` every provider reads.
pub fn skill_sources(owner_home: &Path) -> Vec<PathBuf> {
    vec![owner_config_dir(owner_home).join("skills"), owner_home.join(".agents").join("skills")]
}

/// The home's `skills/`: a real folder of one link per skill (docs/85
/// 16.2), so `~/.agents/skills` joins the owner's own without anything
/// being written into either. A whole-folder link Baaz made before is
/// replaced (the link only — never what it points at); a link whose skill
/// went away is removed; a real file or folder someone put there is left.
pub fn ensure_skills_dir(home: &Path, sources: &[PathBuf]) -> std::io::Result<()> {
    let dir = home.join("skills");
    if std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink()) {
        std::fs::remove_file(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    let mut wanted: Vec<(String, PathBuf)> = Vec::new();
    for source in sources {
        let Ok(entries) = std::fs::read_dir(source) else { continue };
        let mut names: Vec<(String, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|e| e.path().join("SKILL.md").is_file())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        names.sort();
        for (name, path) in names {
            if !wanted.iter().any(|(n, _)| *n == name) {
                wanted.push((name, path));
            }
        }
    }
    sync_skill_links(&dir, &wanted)
}

/// Make `dir` hold exactly one link per `wanted` skill: stale links go,
/// missing ones are made, a real file or folder someone put there stays.
fn sync_skill_links(dir: &Path, wanted: &[(String, PathBuf)]) -> std::io::Result<()> {
    // Stale links go; real entries stay.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let is_link = entry.file_type().is_ok_and(|t| t.is_symlink());
            let target = std::fs::read_link(entry.path()).ok();
            let keep = wanted.iter().any(|(n, p)| *n == name && target.as_deref() == Some(p.as_path()));
            if is_link && !keep {
                std::fs::remove_file(entry.path())?;
            }
        }
    }
    for (name, path) in wanted {
        let link = dir.join(name);
        if std::fs::symlink_metadata(&link).is_err() {
            std::os::unix::fs::symlink(path, &link)?;
        }
    }
    Ok(())
}

/// The folder Baaz hands Claude Code as `--add-dir` so it sees a repo's
/// `.agents/skills` (docs/85 16.2): claude 2.1.276 reads only
/// `.claude/skills` (probed live 26 Nov 2026), and lists the skills in an
/// added folder's `.claude/skills` under their own names (a
/// `--plugin-dir` would prefix them). The folder is
/// `<state_dir>/claude-repo-skills/<slug>` with one link per skill in
/// `.claude/skills/`; nothing is written into the repo (rule 21). A name
/// the repo's own `.claude/skills` or any of `taken` (the home's skill
/// folders) already holds is left to them. `None` when the repo has no
/// `.agents/skills` skill to add.
pub fn repo_skills_dir(state_dir: &Path, cwd: &Path, taken: &[PathBuf]) -> std::io::Result<Option<PathBuf>> {
    let root = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(cwd);
    let Ok(entries) = std::fs::read_dir(root.join(".agents").join("skills")) else { return Ok(None) };
    let held = |name: &str| {
        std::iter::once(root.join(".claude").join("skills")).chain(taken.iter().cloned()).any(|folder| folder.join(name).join("SKILL.md").is_file())
    };
    let mut wanted: Vec<(String, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.path().join("SKILL.md").is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .filter(|(name, _)| !held(name))
        .collect();
    if wanted.is_empty() {
        return Ok(None);
    }
    wanted.sort();
    let folder = state_dir.join("claude-repo-skills").join(project_slug(root));
    let dir = folder.join(".claude").join("skills");
    std::fs::create_dir_all(&dir)?;
    sync_skill_links(&dir, &wanted)?;
    Ok(Some(folder))
}

/// `<state_dir>/claude-home`.
pub fn home_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(HOME_DIR_NAME)
}

/// The default Baaz-owned home for this run: under
/// [`provider::child_env::state_dir`], so `BAAZ_STATE_DIR` redirects it.
/// Env-only: creates nothing.
pub fn default_home() -> PathBuf {
    home_dir(&provider::child_env::state_dir())
}

/// The owner's Claude root: `~/.claude` under `owner_home`.
pub fn owner_config_dir(owner_home: &Path) -> PathBuf {
    owner_home.join(".claude")
}

/// Ensure the Baaz home exists with its owner symlinks: creates
/// `<state_dir>/claude-home` and links every [`LINKED_ENTRIES`] entry
/// whose source exists in the owner's `~/.claude` and whose target is
/// absent. Idempotent; never overwrites a real file, never copies, never
/// writes inside the owner's home. Returns the home path.
pub fn ensure_home(owner_home: &Path, state_dir: &Path) -> std::io::Result<PathBuf> {
    let home = home_dir(state_dir);
    provider::child_env::ensure_linked_dir(&home, &owner_config_dir(owner_home), LINKED_ENTRIES)?;
    ensure_skills_dir(&home, &skill_sources(owner_home))?;
    Ok(home)
}

/// The CLI's project-folder name for `path`: every character outside
/// `[A-Za-z0-9]` becomes `-` (`/Users/me/My App.x` →
/// `-Users-me-My-App-x`). Checked read-only against the owner's
/// `~/.claude/projects` on 25 Nov 2026 (claude 2.1.276): every folder
/// there is a path spelled this way — `.claude/worktrees` reads
/// `--claude-worktrees`, `Application Support` reads `Application-Support`.
pub fn project_slug(path: &Path) -> String {
    path.to_string_lossy().chars().map(|char| if char.is_ascii_alphanumeric() { char } else { '-' }).collect()
}

/// The folder whose slug names a session's memory: the main repo's root
/// for anything inside a git repo — a subfolder walks up to it, and a
/// linked worktree (`.git` is a file pointing at
/// `<main>/.git/worktrees/<name>`) resolves through its `commondir` to the
/// main repo — else `cwd` itself. The CLI does the same: auto memory is
/// one folder per repo, shared by its worktrees and subfolders (none of
/// the owner's worktree project folders has a `memory/`). Reads only.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut at = Some(cwd);
    while let Some(dir) = at {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() {
            return dir.to_path_buf();
        }
        if dot_git.is_file() {
            return worktree_main_root(&dot_git).unwrap_or_else(|| dir.to_path_buf());
        }
        at = dir.parent();
    }
    cwd.to_path_buf()
}

/// `<main>` for a worktree's `.git` file: `gitdir: <main>/.git/worktrees/x`,
/// whose `commondir` names `<main>/.git` (relative to the gitdir).
fn worktree_main_root(dot_git_file: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(dot_git_file).ok()?;
    let gitdir = text.lines().find_map(|line| line.strip_prefix("gitdir:"))?.trim();
    let gitdir = dot_git_file.parent()?.join(gitdir);
    let common = std::fs::read_to_string(gitdir.join("commondir")).ok()?;
    let common = gitdir.join(common.trim());
    let common = std::fs::canonicalize(&common).unwrap_or(common);
    common.parent().map(Path::to_path_buf)
}

/// The repo's AGENTS.md text when the folder `cwd` is in has one and no
/// CLAUDE.md (at the repo root or in `cwd`): what a CLI that does not read
/// AGENTS.md would otherwise miss (docs/85 10.2). Reads only.
pub fn agents_md_only(cwd: &Path) -> Option<String> {
    let root = project_root(cwd);
    let has_claude_md = [root.as_path(), cwd].iter().any(|dir| dir.join("CLAUDE.md").is_file());
    if has_claude_md {
        return None;
    }
    std::fs::read_to_string(root.join("AGENTS.md")).ok().filter(|text| !text.trim().is_empty())
}

/// Name of the redirected memory root under a redirected state dir.
pub const REDIRECTED_MEMORY_DIR: &str = "claude-memory";

/// The auto-memory folder for a session in `cwd` (docs/85 10.1, rule 25):
/// `<owner_config>/projects/<slug>/memory` for the owner's own run, or
/// `<state>/claude-memory/<slug>` when `redirected_state` (the
/// `BAAZ_STATE_DIR` override) is set — then nothing points into the owner's
/// `~/.claude`.
pub fn memory_dir(cwd: &Path, owner_config: &Path, redirected_state: Option<&Path>) -> PathBuf {
    let slug = project_slug(&project_root(cwd));
    match redirected_state {
        Some(state) => state.join(REDIRECTED_MEMORY_DIR).join(slug),
        None => owner_config.join("projects").join(slug).join("memory"),
    }
}

/// One owner-side file the sessions migration (B4M) may move: its
/// absolute source plus its path relative to the Claude config dir. The
/// target keeps the same relative path under the Baaz home, so a moved
/// transcript resumes where the CLI expects it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSource {
    /// Absolute owner-side path (`~/.claude/projects/<slug>/…`).
    pub source: PathBuf,
    /// Path relative to the config dir (`projects/<slug>/…`).
    pub rel: PathBuf,
    /// Whether the source is a directory (the `<id>/` companion).
    pub is_dir: bool,
}

/// Whether `session_id` is safe to match against file names: registry ids
/// are Baaz-minted UUIDs, and anything carrying a separator is refused
/// rather than walked — a hostile or corrupt id must never escape the
/// projects tree.
fn id_is_safe(session_id: &str) -> bool {
    !session_id.is_empty()
        && !session_id.contains('/')
        && !session_id.contains('\\')
        && session_id != "."
        && session_id != ".."
}

/// The owner-side Claude files for one registered session id: the
/// `projects/<slug>/<id>.jsonl` transcript (the `<slug>` is whatever slug
/// directory holds it — the registry never records it) plus the `<id>/`
/// companion directory when present. Unregistered files are never listed:
/// only this id is matched. Pure reads; creates nothing.
pub fn session_sources(owner_home: &Path, session_id: &str) -> Vec<SessionSource> {
    if !id_is_safe(session_id) {
        return Vec::new();
    }
    let projects = owner_config_dir(owner_home).join("projects");
    let Ok(slugs) = std::fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for slug in slugs.flatten() {
        if !slug.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let slug_name = slug.file_name();
        let transcript = slug.path().join(format!("{session_id}.jsonl"));
        if std::fs::symlink_metadata(&transcript).is_ok_and(|meta| meta.file_type().is_file()) {
            out.push(SessionSource {
                source: transcript,
                rel: PathBuf::from("projects")
                    .join(&slug_name)
                    .join(format!("{session_id}.jsonl")),
                is_dir: false,
            });
        }
        let companion = slug.path().join(session_id);
        if std::fs::symlink_metadata(&companion).is_ok_and(|meta| meta.file_type().is_dir()) {
            out.push(SessionSource {
                source: companion,
                rel: PathBuf::from("projects").join(&slug_name).join(session_id),
                is_dir: true,
            });
        }
    }
    out
}

/// The Baaz-home target for a [`SessionSource`] relative path: the same
/// relative path under `<state_dir>/claude-home`.
pub fn baaz_target(state_dir: &Path, rel: &Path) -> PathBuf {
    home_dir(state_dir).join(rel)
}

/// The child environment additions for a claude child rooted at `home`:
/// `CLAUDE_CONFIG_DIR` naming the Baaz home plus an empty
/// `CLAUDE_SECURESTORAGE_CONFIG_DIR` so the child keeps the owner's
/// existing Keychain login. Applied on top of the
/// [`provider::child_env`] scrub — never instead of it.
pub fn child_env(home: &Path) -> Vec<(String, String)> {
    vec![
        ("CLAUDE_CONFIG_DIR".to_owned(), home.to_string_lossy().into_owned()),
        ("CLAUDE_SECURESTORAGE_CONFIG_DIR".to_owned(), String::new()),
    ]
}

// ---------------------------------------------------------- legacy resume

/// Sessions pinned to resume from the owner's home instead of the Baaz
/// home: one entry per pre-consent resume the host admitted (see
/// [`legacy_home_for`]). Level-triggered, not one-shot — an effort-swap
/// `--resume` relaunch of the same session must see the same home — so
/// the host clears each entry when the consented lazy move lands it (and
/// all of them when consent itself lands).
///
/// Each pin may carry the owner home it resumes from (P7 import:
/// [`pin_legacy_resume_at`]); a bare pin means the real `HOME`.
#[allow(clippy::type_complexity)]
static LEGACY_RESUME: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, Option<PathBuf>>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashMap::new()));

/// Pin `session_id` to resume from the owner's home: the host's consent
/// gate calls this for the one child it admitted, before the spawn.
pub fn pin_legacy_resume(session_id: &str) {
    if let Ok(mut pinned) = LEGACY_RESUME.lock() {
        pinned.insert(session_id.to_owned(), None);
    }
}

/// Pin `session_id` to resume from `owner_home` (an imported session,
/// docs/72 §1): the same pin as [`pin_legacy_resume`], naming the home
/// explicitly — the real `HOME` in production, a fixture owner home in a
/// probe. The adapter resumes from the real `HOME`'s files unchanged and
/// points the child at any other home's config dir.
pub fn pin_legacy_resume_at(session_id: &str, owner_home: &Path) {
    if let Ok(mut pinned) = LEGACY_RESUME.lock() {
        pinned.insert(session_id.to_owned(), Some(owner_home.to_path_buf()));
    }
}

/// The owner home `session_id`'s pin names, when it named one other than
/// the real `HOME` (see [`pin_legacy_resume_at`]); `None` for a bare pin,
/// for a pin at the real `HOME`, and for an unpinned session.
pub fn legacy_owner_home(session_id: &str) -> Option<PathBuf> {
    let home = LEGACY_RESUME.lock().ok()?.get(session_id).cloned().flatten()?;
    let real = std::env::var_os("HOME").map(PathBuf::from);
    (real.as_deref() != Some(home.as_path())).then_some(home)
}

/// Whether `session_id` resumes from the owner's home right now.
pub fn is_legacy_resume(session_id: &str) -> bool {
    LEGACY_RESUME.lock().is_ok_and(|pinned| pinned.contains_key(session_id))
}

/// Drop one legacy pin: what the host calls after the consented lazy
/// move lands the session's files under the Baaz home.
pub fn clear_legacy_resume(session_id: &str) {
    if let Ok(mut pinned) = LEGACY_RESUME.lock() {
        pinned.remove(session_id);
    }
}

/// Drop every legacy pin: what the host calls when consent lands, so no
/// later resume keeps reading the owner's home after the move was
/// admitted.
pub fn clear_legacy_resumes() {
    if let Ok(mut pinned) = LEGACY_RESUME.lock() {
        pinned.clear();
    }
}

/// Whether `session_id` should resume from the owner's home: its files
/// still live only there (owner-side sources exist, no Baaz-side target
/// does — a session the move already half-carried resumes where its
/// transcript is, under the Baaz home) and `consented` is false. Pure
/// over explicit dirs; the host passes its own consent read, so this
/// stays testable with temp dirs.
pub fn legacy_home_for(
    owner_home: &Path,
    state_dir: &Path,
    session_id: &str,
    consented: bool,
) -> Option<PathBuf> {
    if consented {
        return None;
    }
    let sources = session_sources(owner_home, session_id);
    if sources.is_empty() {
        return None;
    }
    if sources.iter().any(|entry| baaz_target(state_dir, &entry.rel).exists()) {
        return None;
    }
    Some(owner_config_dir(owner_home))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "provider-cc-home-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    #[test]
    fn the_home_lives_under_the_given_state_dir() {
        let state = PathBuf::from("/tmp/baaz-state");
        assert_eq!(home_dir(&state), PathBuf::from("/tmp/baaz-state/claude-home"));
    }

    /// docs/85 16.2 (probed 26 Nov 2026): a repo's `.agents/skills` reaches
    /// claude as links in a state-dir folder handed over with `--add-dir`;
    /// a session in a subfolder finds the repo root; a name the repo's
    /// `.claude/skills` or the home's folders hold is left to them; a skill
    /// that went away loses its link; nothing is written into the repo; a
    /// repo without `.agents/skills` gets no folder.
    #[test]
    fn a_repos_agents_skills_become_links_in_an_added_folder() {
        let root = temp_root("repo-skills");
        let (state, repo, personal) = (root.join("state"), root.join("repo"), root.join("owner/.claude/skills"));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::create_dir_all(repo.join("src/deep")).unwrap();
        for (dir, name) in [(repo.join(".agents/skills"), "deploy"), (repo.join(".agents/skills"), "lint"), (repo.join(".agents/skills"), "review"), (repo.join(".claude/skills"), "lint"), (personal.clone(), "review")] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("SKILL.md"), "---\nname: x\n---\n").unwrap();
        }
        let before: Vec<_> = walk(&repo);
        let folder = repo_skills_dir(&state, &repo.join("src/deep"), &[personal.clone()]).unwrap().expect("a folder");
        assert!(folder.starts_with(state.join("claude-repo-skills")));
        let links = folder.join(".claude/skills");
        let names = |dir: &Path| { let mut n: Vec<String> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect(); n.sort(); n };
        assert_eq!(names(&links), vec!["deploy"], "lint is the repo's .claude skill, review the person's");
        assert_eq!(std::fs::read_link(links.join("deploy")).unwrap(), repo.join(".agents/skills/deploy"));
        assert_eq!(walk(&repo), before, "nothing written into the repo");

        std::fs::create_dir_all(repo.join(".agents/skills/ship")).unwrap();
        std::fs::write(repo.join(".agents/skills/ship/SKILL.md"), "x").unwrap();
        std::fs::remove_dir_all(repo.join(".agents/skills/deploy")).unwrap();
        repo_skills_dir(&state, &repo, &[personal.clone()]).unwrap().expect("a folder");
        assert_eq!(names(&links), vec!["ship"], "a gone skill loses its link, a new one gains one");

        let bare = root.join("bare");
        std::fs::create_dir_all(bare.join(".git")).unwrap();
        assert_eq!(repo_skills_dir(&state, &bare, &[]).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn walk(dir: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().filter_map(Result::ok) {
            out.push(entry.path());
            if entry.file_type().unwrap().is_dir() {
                out.extend(walk(&entry.path()));
            }
        }
        out.sort();
        out
    }

    /// docs/85 16.2: a home made before had `skills` as one link to the
    /// owner's folder; it becomes a real folder of per-skill links, the
    /// owner's skills untouched, the owner's own name winning over a
    /// shared skill of the same name.
    #[test]
    fn an_old_skills_link_becomes_a_folder_of_skill_links() {
        let root = temp_root("skills-migrate");
        let owner = root.join("owner-home");
        let home = root.join("claude-home");
        for (dir, name) in [(owner.join(".claude/skills"), "review"), (owner.join(".agents/skills"), "review"), (owner.join(".agents/skills"), "shared")] {
            std::fs::create_dir_all(dir.join(name)).unwrap();
            std::fs::write(dir.join(name).join("SKILL.md"), "x").unwrap();
        }
        std::fs::create_dir_all(&home).unwrap();
        std::os::unix::fs::symlink(owner.join(".claude/skills"), home.join("skills")).unwrap();
        ensure_skills_dir(&home, &skill_sources(&owner)).unwrap();
        assert!(!std::fs::symlink_metadata(home.join("skills")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_link(home.join("skills/review")).unwrap(), owner.join(".claude/skills/review"), "the owner's own wins");
        assert_eq!(std::fs::read_link(home.join("skills/shared")).unwrap(), owner.join(".agents/skills/shared"));
        assert!(owner.join(".claude/skills/review/SKILL.md").is_file(), "nothing of the owner's moved");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn links_reach_the_owner_files_and_skip_what_is_absent() {
        let root = temp_root("links");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(owner.join(".claude").join("skills")).expect("owner skills");
        std::fs::create_dir_all(owner.join(".claude").join("plugins")).expect("owner plugins");
        std::fs::write(owner.join(".claude").join("settings.json"), "{\"model\":\"sonnet\"}")
            .expect("owner settings");
        // `CLAUDE.md`, `agents/` and `commands/` absent: linked nothing.

        let home = ensure_home(&owner, &state).expect("ensure");
        assert_eq!(home, state.join("claude-home"));
        for name in ["settings.json", "plugins"] {
            let dst = home.join(name);
            assert!(
                std::fs::symlink_metadata(&dst).expect("linked").file_type().is_symlink(),
                "{name} is a link, not a copy"
            );
        }
        // docs/85 16.2: skills/ is a real folder of per-skill links.
        assert!(!std::fs::symlink_metadata(home.join("skills")).expect("made").file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).expect("reads through"),
            "{\"model\":\"sonnet\"}"
        );
        for name in ["CLAUDE.md", "agents", "commands"] {
            assert!(
                std::fs::symlink_metadata(home.join(name)).is_err(),
                "absent source links nothing: {name}"
            );
        }
        // The owner's tree gained nothing.
        assert!(
            std::fs::symlink_metadata(owner.join(".claude").join("projects")).is_err(),
            "nothing is written inside the owner's home"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn link_creation_is_idempotent_and_never_overwrites_a_real_file() {
        let root = temp_root("idempotent");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(owner.join(".claude")).expect("owner claude dir");
        std::fs::write(owner.join(".claude").join("settings.json"), "{}").expect("owner settings");

        ensure_home(&owner, &state).expect("first ensure");
        ensure_home(&owner, &state).expect("second ensure is a no-op");
        let home = state.join("claude-home");
        assert!(std::fs::symlink_metadata(home.join("settings.json")).expect("meta").file_type().is_symlink());

        // A real file in the way survives re-ensures.
        std::fs::remove_file(home.join("settings.json")).expect("unlink");
        std::fs::write(home.join("settings.json"), "mine").expect("a real file in the way");
        ensure_home(&owner, &state).expect("re-ensure");
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).expect("reads"),
            "mine",
            "a real file is never overwritten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_claude_child_env_names_the_home_and_blanks_secure_storage() {
        let home = PathBuf::from("/tmp/baaz-state/claude-home");
        let env = child_env(&home);
        assert!(
            env.contains(&(
                "CLAUDE_CONFIG_DIR".to_owned(),
                "/tmp/baaz-state/claude-home".to_owned()
            )),
            "the child writes to the Baaz home: {env:?}"
        );
        assert!(
            env.contains(&("CLAUDE_SECURESTORAGE_CONFIG_DIR".to_owned(), String::new())),
            "empty, not unset — the shared Keychain item: {env:?}"
        );
    }

    #[test]
    fn session_sources_lists_only_the_registered_id() {
        let root = temp_root("sources");
        let owner = root.join("owner-home");
        let slug = owner.join(".claude").join("projects").join("-work");
        std::fs::create_dir_all(&slug).expect("slug dir");
        std::fs::write(slug.join("sess-1.jsonl"), "{}\n").expect("registered transcript");
        std::fs::create_dir_all(slug.join("sess-1")).expect("companion dir");
        std::fs::write(slug.join("other.jsonl"), "{}\n").expect("unregistered transcript");

        let mut found = session_sources(&owner, "sess-1");
        found.sort_by(|a, b| a.rel.cmp(&b.rel));
        assert_eq!(found.len(), 2, "transcript plus companion: {found:?}");
        assert!(found.iter().any(|entry| entry.rel == *"projects/-work/sess-1.jsonl" && !entry.is_dir));
        assert!(found.iter().any(|entry| entry.rel == *"projects/-work/sess-1" && entry.is_dir));
        for entry in &found {
            assert_eq!(baaz_target(&root.join("state"), &entry.rel), root.join("state").join("claude-home").join(&entry.rel));
        }
        assert!(session_sources(&owner, "other-absent").is_empty(), "absent ids list nothing");
        // The unregistered file is never attributed to the registered id.
        assert!(!found.iter().any(|entry| entry.rel.to_string_lossy().contains("other")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn session_sources_refuses_ids_that_could_escape_the_tree() {
        let root = temp_root("traversal");
        let owner = root.join("owner-home");
        std::fs::create_dir_all(owner.join(".claude").join("projects")).expect("projects dir");
        for hostile in ["../x", "a/b", "..", "", "a\\b"] {
            assert!(session_sources(&owner, hostile).is_empty(), "refused: {hostile:?}");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_claude_child_env_carries_none_of_the_scrubbed_vars() {
        // The full child env is the scrubbed parent plus the two home
        // vars: compose both halves here and assert the whole.
        let parent = [
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "claude-desktop"),
            ("CLAUDE_CODE_SESSION_ID", "s-1"),
            ("CLAUDE_AGENT_SDK_VERSION", "1"),
            ("CLAUDE_PID", "1"),
            ("CLAUDE_EFFORT", "high"),
            ("ANTHROPIC_BASE_URL", "http://desktop:2000"),
            ("ANTHROPIC_API_KEY", "owner-key"),
            ("PATH", "/usr/bin:/bin"),
        ];
        let entrypoint = true;
        let mut full: Vec<(String, String)> = parent
            .iter()
            .filter(|(name, _)| !provider::child_env::should_scrub(name, entrypoint))
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        full.extend(child_env(Path::new("/tmp/baaz-state/claude-home")));
        for (name, _) in &full {
            assert!(
                !provider::child_env::should_scrub(name, entrypoint),
                "no scrubbed var survives: {name}"
            );
        }
        let names: Vec<&str> = full.iter().map(|(name, _)| name.as_str()).collect();
        assert!(names.contains(&"CLAUDE_CONFIG_DIR"));
        assert!(names.contains(&"CLAUDE_SECURESTORAGE_CONFIG_DIR"));
        assert!(names.contains(&"ANTHROPIC_API_KEY"), "owner auth survives");
        assert!(names.contains(&"PATH"), "PATH survives");
    }

    #[test]
    fn legacy_resume_points_at_the_owner_home_until_consent_or_a_move() {
        let root = temp_root("legacy");
        let owner = root.join("owner-home");
        let state = root.join("state");
        let slug = owner.join(".claude").join("projects").join("-work");
        std::fs::create_dir_all(&slug).expect("slug dir");
        std::fs::write(slug.join("sess-legacy.jsonl"), "{}\n").expect("owner transcript");

        // Unmoved and unconsented: the owner's home.
        assert_eq!(
            legacy_home_for(&owner, &state, "sess-legacy", false),
            Some(owner.join(".claude")),
            "an unmoved session resumes where its transcript is"
        );
        // Consent admitted: no legacy home — the lazy move owns it now.
        assert_eq!(
            legacy_home_for(&owner, &state, "sess-legacy", true),
            None,
            "after consent the Baaz home answers"
        );
        // Unknown ids never route legacy.
        assert_eq!(legacy_home_for(&owner, &state, "sess-absent", false), None);
        // A session the move already half-carried resumes under the Baaz
        // home, where its transcript is — never a split read.
        let target =
            state.join("claude-home").join("projects").join("-work").join("sess-legacy.jsonl");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("target parent");
        std::fs::write(&target, "{}\n").expect("moved transcript");
        assert_eq!(
            legacy_home_for(&owner, &state, "sess-legacy", false),
            None,
            "a moved transcript resumes under the Baaz home"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn legacy_pins_are_per_session_and_clearable() {
        let id = format!(
            "sess-pin-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        );
        assert!(!is_legacy_resume(&id));
        pin_legacy_resume(&id);
        assert!(is_legacy_resume(&id), "the pinned child resumes legacy");
        assert!(!is_legacy_resume("some-other-session"), "pins never leak across sessions");
        clear_legacy_resume(&id);
        assert!(!is_legacy_resume(&id), "a landed move clears its pin");
        // An imported session's pin may name its owner home (docs/72): a
        // fixture home is reported, a bare pin names none.
        let fixture = std::env::temp_dir().join("baaz-import-fixture-owner");
        pin_legacy_resume_at(&id, &fixture);
        assert!(is_legacy_resume(&id), "a pin at a home is a legacy resume");
        assert_eq!(legacy_owner_home(&id), Some(fixture.clone()));
        pin_legacy_resume(&id);
        assert_eq!(legacy_owner_home(&id), None, "a bare pin names the real HOME");
        if let Some(real) = std::env::var_os("HOME").map(PathBuf::from) {
            pin_legacy_resume_at(&id, &real);
            assert_eq!(legacy_owner_home(&id), None, "a pin at the real HOME changes nothing");
        }
        pin_legacy_resume(&id);
        clear_legacy_resumes();
        assert!(!is_legacy_resume(&id), "consent clears every pin");
    }

    /// The CLI's spelling, from folder names in the owner's
    /// `~/.claude/projects` (listed read-only, 25 Nov 2026).
    #[test]
    fn the_slug_matches_the_clis_folder_names() {
        for (path, folder) in [
            ("/Users/latekaapi/Projects/harness", "-Users-latekaapi-Projects-harness"),
            ("/Users/latekaapi/Projects/lakshya-v2", "-Users-latekaapi-Projects-lakshya-v2"),
            (
                "/Users/latekaapi/Projects/dharisanam/.claude/worktrees/agitated-mirzakhani-535f89",
                "-Users-latekaapi-Projects-dharisanam--claude-worktrees-agitated-mirzakhani-535f89",
            ),
            (
                "/Users/latekaapi/Library/Application Support/Claude/scratch-workspaces/x",
                "-Users-latekaapi-Library-Application-Support-Claude-scratch-workspaces-x",
            ),
        ] {
            assert_eq!(project_slug(Path::new(path)), folder);
        }
    }

    /// This repo's slug is a folder the CLI made, when the machine running
    /// the test has one (a read-only listing; skipped elsewhere).
    #[test]
    fn this_repos_slug_is_a_real_cli_folder_when_present() {
        let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return };
        let projects = home.join(".claude").join("projects");
        let Ok(listing) = std::fs::read_dir(&projects) else { return };
        let names: Vec<String> = listing.flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        let repo = project_root(Path::new(env!("CARGO_MANIFEST_DIR")));
        let slug = project_slug(&repo);
        if names.iter().any(|name| name.starts_with(&project_slug(&home))) && repo.ends_with("harness") {
            assert!(names.contains(&slug), "{slug} is one of the CLI's folders");
        }
    }

    #[test]
    fn root_subfolder_worktree_and_plain_folder_map_to_one_slug_each() {
        let root = temp_root("slug");
        let main = root.join("repo");
        std::fs::create_dir_all(main.join(".git").join("worktrees").join("wt1")).expect("main .git");
        std::fs::create_dir_all(main.join("src").join("deep")).expect("subfolder");
        std::fs::write(main.join(".git").join("worktrees").join("wt1").join("commondir"), "../..\n").expect("commondir");
        let worktree = root.join("worktrees").join("wt1");
        std::fs::create_dir_all(worktree.join("sub")).expect("worktree");
        std::fs::write(worktree.join(".git"), format!("gitdir: {}\n", main.join(".git/worktrees/wt1").display())).expect(".git file");
        let plain = root.join("plain").join("inner");
        std::fs::create_dir_all(&plain).expect("plain");
        let main = std::fs::canonicalize(&main).expect("canonical main");

        let owner = Path::new("/Users/o/.claude");
        let expect = owner.join("projects").join(project_slug(&main)).join("memory");
        assert_eq!(memory_dir(&main, owner, None), expect, "repo root");
        assert_eq!(memory_dir(&main.join("src/deep"), owner, None), expect, "subfolder");
        assert_eq!(memory_dir(&worktree, owner, None), expect, "a worktree maps to its main repo");
        assert_eq!(memory_dir(&worktree.join("sub"), owner, None), expect, "a worktree's subfolder too");
        assert_eq!(
            memory_dir(&plain, owner, None),
            owner.join("projects").join(project_slug(&plain)).join("memory"),
            "a folder outside git is its own"
        );
        // Redirected state (rule 25): inside the state dir, never ~/.claude.
        let state = Path::new("/tmp/baaz-check-state");
        let redirected = memory_dir(&worktree, owner, Some(state));
        assert_eq!(redirected, state.join("claude-memory").join(project_slug(&main)));
        assert!(!redirected.starts_with(owner));
        let _ = std::fs::remove_dir_all(&root);
    }
}
