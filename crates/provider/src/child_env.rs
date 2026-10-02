//! The scrubbed environment every provider child spawns with.
//!
//! A Baaz launched from inside a Claude desktop agent session inherits that
//! session's env (`CLAUDE_CODE_ENTRYPOINT=claude-desktop`,
//! `CLAUDE_CODE_SESSION_ID`, the messaging socket, …) and — because every
//! spawn used to inherit everything but `PATH` — passed it on: Baaz's Claude
//! children stamped their transcripts as the desktop's own. Every child Baaz
//! spawns (claude, codex app-server, muse serve, probes, terminal PTY
//! shells) goes through this scrub.
//!
//! What goes:
//!
//! * `CLAUDECODE`, every `CLAUDE_CODE_*`, every `CLAUDE_AGENT_SDK_*`,
//!   `CLAUDE_PID`, `CLAUDE_EFFORT` — always;
//! * `ANTHROPIC_BASE_URL` — only when Baaz's own env had
//!   `CLAUDE_CODE_ENTRYPOINT` set, i.e. Baaz itself runs under a desktop
//!   agent and the inherited value is the desktop's, not the owner's. An
//!   owner-set value on a plain launch survives.
//!
//! [`should_scrub`] is the pure predicate — unit-tested here.
//! [`scrub_command`] applies it to a [`std::process::Command`] (which cannot
//! enumerate removals by prefix, so the parent env is read at apply time).
//! [`scrub_overrides`] and [`with_scrubbed_env`] cover the two spawn
//! surfaces that cannot carry removals: the terminal PTY (whose config only
//! adds vars) and `muse serve` (whose `Command` lives in another crate).

use std::ffi::OsString;
use std::path::PathBuf;

/// Whether `name` must be removed from a provider child's environment.
///
/// `parent_had_entrypoint` is whether Baaz's own env had
/// `CLAUDE_CODE_ENTRYPOINT` set — the only condition that also removes
/// `ANTHROPIC_BASE_URL`. Pure: tests drive this directly, never the
/// process env.
pub fn should_scrub(name: &str, parent_had_entrypoint: bool) -> bool {
    if name == "CLAUDECODE" || name == "CLAUDE_PID" || name == "CLAUDE_EFFORT" {
        return true;
    }
    if name.starts_with("CLAUDE_CODE_") || name.starts_with("CLAUDE_AGENT_SDK_") {
        return true;
    }
    if name == "ANTHROPIC_BASE_URL" {
        return parent_had_entrypoint;
    }
    false
}

/// Whether Baaz's own env had `CLAUDE_CODE_ENTRYPOINT` set: Baaz itself
/// runs under a desktop agent session, so an inherited `ANTHROPIC_BASE_URL`
/// is the desktop's and goes too.
pub fn parent_had_entrypoint() -> bool {
    std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some()
}

/// Remove every scrubbed inherited var from `command`'s environment.
/// `PATH` (and every other var) is untouched.
pub fn scrub_command(command: &mut std::process::Command) {
    let entrypoint = parent_had_entrypoint();
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        if should_scrub(key, entrypoint) {
            command.env_remove(key);
        }
    }
}

/// The scrub as explicit blank values, for spawn surfaces that can only
/// set, never remove: the terminal PTY config. One `(name, "")` per
/// scrubbed var present in this process's env, sorted by name. Empty is
/// falsy for the node CLIs that read these, so a blanked var behaves as
/// unset; the Baaz homes are deliberately absent here — PTY shells keep
/// the owner's own homes, only the inherited desktop vars go.
pub fn scrub_overrides() -> Vec<(String, String)> {
    let entrypoint = parent_had_entrypoint();
    let mut out = Vec::new();
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        if should_scrub(key, entrypoint) {
            out.push((key.to_owned(), String::new()));
        }
    }
    out.sort();
    out
}

/// Baaz's own state dir: `BAAZ_STATE_DIR` when set non-empty, else
/// `~/Library/Application Support/baaz`. The Baaz-owned provider homes
/// (`claude-home`, `codex-home`) live directly under this.
pub fn state_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BAAZ_STATE_DIR").filter(|dir| !dir.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("baaz")
}

/// Create `dir` and symlink each `names` entry from `owner_root/name` to
/// `dir/name` when the source exists and the target is absent. Idempotent:
/// a second run changes nothing. Never overwrites (an existing file, dir
/// or link stays), never copies, never writes inside `owner_root`.
pub fn ensure_linked_dir(dir: &PathBuf, owner_root: &PathBuf, names: &[&str]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    for name in names {
        let src = owner_root.join(name);
        if std::fs::symlink_metadata(&src).is_err() {
            continue;
        }
        let dst = dir.join(name);
        if std::fs::symlink_metadata(&dst).is_ok() {
            continue;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&src, &dst)?;
        #[cfg(not(unix))]
        {
            let _ = (&src, &dst);
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "provider homes need symlinks",
            ));
        }
    }
    Ok(())
}

/// The guard behind [`with_scrubbed_env`]: removes the scrubbed vars on
/// creation, restores them (with their exact values) on drop — including
/// on panic, so a failed spawn never leaks a half-scrubbed process env.
struct ScrubGuard {
    removed: Vec<(String, OsString)>,
}

impl ScrubGuard {
    fn hold() -> Self {
        let entrypoint = parent_had_entrypoint();
        let mut removed = Vec::new();
        for (key, value) in std::env::vars_os() {
            let Some(key) = key.to_str() else { continue };
            if should_scrub(key, entrypoint) {
                removed.push((key.to_owned(), value));
            }
        }
        for (key, _) in &removed {
            std::env::remove_var(key);
        }
        Self { removed }
    }
}

impl Drop for ScrubGuard {
    fn drop(&mut self) {
        for (key, value) in &self.removed {
            std::env::set_var(key, value);
        }
    }
}

/// Run `f` with every scrubbed var removed from this process's env,
/// restored afterwards. Only for spawns whose [`std::process::Command`]
/// lives in a crate that cannot apply [`scrub_command`] (`muse serve`):
/// every other child scrubs its own `Command` instead, which touches no
/// shared state. Process-global by nature — concurrent spawns on other
/// threads inherit the scrubbed env too, which is the state their own
/// `Command`s would reach anyway — so keep the closure to the spawn call.
pub fn with_scrubbed_env<T>(f: impl FnOnce() -> T) -> T {
    let _guard = ScrubGuard::hold();
    f()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_inheritance_goes() {
        for name in [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_HOST_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_AGENT_SDK_VERSION",
            "CLAUDE_PID",
            "CLAUDE_EFFORT",
        ] {
            assert!(should_scrub(name, false), "{name} scrubs without an entrypoint");
            assert!(should_scrub(name, true), "{name} scrubs with an entrypoint");
        }
    }

    #[test]
    fn the_base_url_goes_only_under_a_desktop_agent() {
        assert!(
            !should_scrub("ANTHROPIC_BASE_URL", false),
            "an owner-set value on a plain launch survives"
        );
        assert!(
            should_scrub("ANTHROPIC_BASE_URL", true),
            "the desktop's value goes with the entrypoint"
        );
    }

    #[test]
    fn owner_auth_and_neighbours_survive() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_MODEL",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "HOME",
            "PATH",
            "SHELL",
        ] {
            assert!(!should_scrub(name, false), "{name} survives");
            assert!(!should_scrub(name, true), "{name} survives even under an entrypoint");
        }
    }

    #[test]
    fn names_are_exact_or_prefixed_never_substring() {
        assert!(!should_scrub("MY_CLAUDECODE", false));
        assert!(!should_scrub("CLAUDE", false));
        assert!(!should_scrub("ANTHROPIC_BASE_URL_EXTRA", true));
    }

    /// Serialize the tests that mutate the process environment: the
    /// runner executes tests on threads sharing one environment.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct SavedEnv {
        vars: Vec<(String, Option<OsString>)>,
    }

    impl SavedEnv {
        fn save(names: &[&str]) -> Self {
            Self { vars: names.iter().map(|name| ((*name).to_owned(), std::env::var_os(name))).collect() }
        }
    }

    impl Drop for SavedEnv {
        fn drop(&mut self) {
            for (name, value) in &self.vars {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn overrides_blank_what_the_parent_carries() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedEnv::save(&[
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
        ]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "claude-desktop");
        std::env::set_var("ANTHROPIC_BASE_URL", "http://desktop:2000");
        std::env::set_var("ANTHROPIC_API_KEY", "owner-key");

        let overrides = scrub_overrides();
        assert!(
            overrides.contains(&("CLAUDECODE".to_owned(), String::new())),
            "present scrubbed vars blank: {overrides:?}"
        );
        assert!(
            overrides.contains(&("CLAUDE_CODE_ENTRYPOINT".to_owned(), String::new())),
            "the entrypoint itself blanks: {overrides:?}"
        );
        assert!(
            overrides.contains(&("ANTHROPIC_BASE_URL".to_owned(), String::new())),
            "the desktop's base url blanks: {overrides:?}"
        );
        assert!(
            !overrides.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY"),
            "owner auth never appears: {overrides:?}"
        );

        std::env::remove_var("CLAUDE_CODE_ENTRYPOINT");
        let overrides = scrub_overrides();
        assert!(
            !overrides.iter().any(|(name, _)| name == "ANTHROPIC_BASE_URL"),
            "without the entrypoint the owner's base url survives: {overrides:?}"
        );
    }

    #[test]
    fn the_guard_removes_and_restores() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedEnv::save(&["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT"]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "claude-desktop");

        with_scrubbed_env(|| {
            assert_eq!(std::env::var_os("CLAUDECODE"), None, "removed inside the guard");
            assert_eq!(
                std::env::var_os("CLAUDE_CODE_ENTRYPOINT"),
                None,
                "the entrypoint goes too"
            );
        });
        assert_eq!(std::env::var_os("CLAUDECODE"), Some(OsString::from("1")), "restored after");
        assert_eq!(
            std::env::var_os("CLAUDE_CODE_ENTRYPOINT"),
            Some(OsString::from("claude-desktop")),
            "restored after"
        );
    }

    #[test]
    fn linked_dirs_create_links_only_when_absent() {
        let root = std::env::temp_dir().join(format!(
            "provider-child-env-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let owner = root.join("owner");
        let home = root.join("home");
        std::fs::create_dir_all(owner.join("skills")).expect("owner skills");
        std::fs::write(owner.join("settings.json"), "{}").expect("owner settings");
        // `plugins/` absent on the owner side: never created, never linked.

        ensure_linked_dir(&home, &owner, &["settings.json", "skills", "plugins"]).expect("ensure");
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).expect("reads through"),
            "{}",
            "the link reaches the owner's file"
        );
        assert!(std::fs::symlink_metadata(home.join("settings.json")).expect("meta").file_type().is_symlink());
        assert!(home.join("skills").is_dir(), "a linked dir reads as a dir");
        assert!(std::fs::symlink_metadata(home.join("plugins")).is_err(), "absent source links nothing");

        // Idempotent: a second run changes nothing, and a real file in the
        // way is never overwritten.
        std::fs::remove_file(home.join("settings.json")).expect("unlink");
        std::fs::write(home.join("settings.json"), "mine").expect("a real file in the way");
        ensure_linked_dir(&home, &owner, &["settings.json", "skills", "plugins"]).expect("re-ensure");
        assert_eq!(
            std::fs::read_to_string(home.join("settings.json")).expect("reads"),
            "mine",
            "a real file is never overwritten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
