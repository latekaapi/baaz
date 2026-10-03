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
//! when absent — never copied, never overwritten — and nothing is ever
//! written inside the owner's `~/.claude`.

use std::path::{Path, PathBuf};

/// The Baaz-owned Claude home's name under the state dir.
pub const HOME_DIR_NAME: &str = "claude-home";

/// Owner entries symlinked into the Baaz home when they exist.
pub const LINKED_ENTRIES: &[&str] =
    &["settings.json", "skills", "plugins", "CLAUDE.md", "agents", "commands"];

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
    Ok(home)
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
static LEGACY_RESUME: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(std::collections::HashSet::new()));

/// Pin `session_id` to resume from the owner's home: the host's consent
/// gate calls this for the one child it admitted, before the spawn.
pub fn pin_legacy_resume(session_id: &str) {
    if let Ok(mut pinned) = LEGACY_RESUME.lock() {
        pinned.insert(session_id.to_owned());
    }
}

/// Whether `session_id` resumes from the owner's home right now.
pub fn is_legacy_resume(session_id: &str) -> bool {
    LEGACY_RESUME.lock().is_ok_and(|pinned| pinned.contains(session_id))
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
        for name in ["settings.json", "skills", "plugins"] {
            let dst = home.join(name);
            assert!(
                std::fs::symlink_metadata(&dst).expect("linked").file_type().is_symlink(),
                "{name} is a link, not a copy"
            );
        }
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
        pin_legacy_resume(&id);
        clear_legacy_resumes();
        assert!(!is_legacy_resume(&id), "consent clears every pin");
    }
}
