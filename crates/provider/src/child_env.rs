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
//! [`scrub_overrides`] covers the one spawn surface that cannot carry
//! removals: the terminal PTY (whose config only adds vars). `muse serve`
//! rides `muse-client`'s `spawn_with_env` with [`scrubbed_removals`].
//!
//! Nothing here touches the process environment: the "launched under a
//! desktop agent" fact is captured once ([`startup_had_entrypoint`]) and
//! every child carries its own `env_remove`s. A concurrent spawn therefore
//! never observes a half-scrubbed process env.

use std::path::PathBuf;
use std::sync::OnceLock;

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

/// Whether Baaz was launched under a desktop agent session, captured once
/// for the whole process: the first call reads `CLAUDE_CODE_ENTRYPOINT`
/// and every later call replays that answer. Call [`capture_startup_entrypoint`]
/// on the startup path (before any thread spawns) so the capture lands
/// early; even without that call the value is right, because nothing in
/// Baaz ever mutates these vars after launch — [`OnceLock`] only freezes
/// the answer against future writers.
///
/// An inherited `ANTHROPIC_BASE_URL` is the desktop's (and goes too) only
/// when this is true.
pub fn startup_had_entrypoint() -> bool {
    static STARTUP_ENTRYPOINT: OnceLock<bool> = OnceLock::new();
    *STARTUP_ENTRYPOINT.get_or_init(|| std::env::var_os("CLAUDE_CODE_ENTRYPOINT").is_some())
}

/// Capture the launch fact behind [`startup_had_entrypoint`] now, on the
/// startup path before any thread spawns. Idempotent: a second call (or
/// the lazy first [`startup_had_entrypoint`]) keeps the first answer.
pub fn capture_startup_entrypoint() {
    let _ = startup_had_entrypoint();
}

/// The scrubbed names among `names`, for an explicit entrypoint flag.
/// Pure: tests drive this directly with fixed name lists, never the
/// process env. Sorted, so builders produce deterministic removals.
pub fn removals_for<'a>(
    names: impl Iterator<Item = &'a str>,
    parent_had_entrypoint: bool,
) -> Vec<String> {
    let mut out: Vec<String> = names
        .filter(|name| should_scrub(name, parent_had_entrypoint))
        .map(str::to_owned)
        .collect();
    out.sort();
    out
}

/// Every scrubbed var present in this process's env, for an explicit
/// entrypoint flag. Reads the parent env (never writes it) so a
/// [`std::process::Command`] — which cannot enumerate removals by prefix
/// — can carry one `env_remove` per entry.
pub fn scrubbed_removals_for(parent_had_entrypoint: bool) -> Vec<String> {
    let mut names = Vec::new();
    for (key, _) in std::env::vars_os() {
        let Some(key) = key.to_str() else { continue };
        names.push(key.to_owned());
    }
    removals_for(names.iter().map(String::as_str), parent_had_entrypoint)
}

/// Every scrubbed var present in this process's env, with the entrypoint
/// captured at startup ([`startup_had_entrypoint`]). Hand these to a
/// `Command` as `env_remove`s, or to `muse-client`'s `spawn_with_env`.
pub fn scrubbed_removals() -> Vec<String> {
    scrubbed_removals_for(startup_had_entrypoint())
}

/// Remove every scrubbed inherited var from `command`'s environment, with
/// the entrypoint captured at startup. `PATH` (and every other var) is
/// untouched, and so is this process's env: the removals live on the
/// child's `Command`, never as a process-global mutation. Returns the
/// removed names, so tests can assert a built `Command` carries them
/// (see [`std::process::Command::get_envs`]).
pub fn scrub_command(command: &mut std::process::Command) -> Vec<String> {
    let removed = scrubbed_removals();
    for name in &removed {
        command.env_remove(name);
    }
    removed
}

/// The scrub as explicit blank values, for the one spawn surface that can
/// only set, never remove: the terminal PTY config. One `(name, "")` per
/// scrubbed var present in this process's env, sorted by name. Empty is
/// falsy for the node CLIs that read these, so a blanked var behaves as
/// unset; the Baaz homes are deliberately absent here — PTY shells keep
/// the owner's own homes, only the inherited desktop vars go.
pub fn scrub_overrides() -> Vec<(String, String)> {
    scrub_overrides_for(startup_had_entrypoint())
}

/// [`scrub_overrides`] for an explicit entrypoint flag. The PTY tests
/// drive this directly; production uses the startup-captured flag.
pub fn scrub_overrides_for(parent_had_entrypoint: bool) -> Vec<(String, String)> {
    scrubbed_removals_for(parent_had_entrypoint)
        .into_iter()
        .map(|name| (name, String::new()))
        .collect()
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

/// There is deliberately no process-env guard here anymore: an earlier
/// revision removed the scrubbed vars from the whole process around the
/// spawn and restored them after, which raced concurrent `getenv` (UB on
/// macOS) and let a concurrent spawn read a scrubbed entrypoint. Every
/// spawn surface now carries its own per-`Command` removals
/// ([`scrub_command`], [`scrubbed_removals`], [`scrub_overrides_for`]), so
/// no shared state is ever touched.

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

    /// Serialize the tests that touch the process environment: the
    /// runner executes tests on threads sharing one environment. The
    /// entrypoint flag itself is captured once per process
    /// ([`startup_had_entrypoint`]), so entrypoint-sensitive assertions
    /// drive the explicit-flag builders ([`removals_for`],
    /// [`scrubbed_removals_for`], [`scrub_overrides_for`]) and never the
    /// cached value.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct SavedEnv {
        vars: Vec<(String, Option<std::ffi::OsString>)>,
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
    fn removals_name_only_what_the_flag_names() {
        let names = [
            "CLAUDECODE",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_AGENT_SDK_VERSION",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
            "PATH",
        ];
        assert_eq!(
            removals_for(names.iter().copied(), true),
            vec![
                "ANTHROPIC_BASE_URL",
                "CLAUDECODE",
                "CLAUDE_AGENT_SDK_VERSION",
                "CLAUDE_CODE_ENTRYPOINT",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>(),
            "sorted, entrypoint on"
        );
        assert_eq!(
            removals_for(names.iter().copied(), false),
            vec!["CLAUDECODE", "CLAUDE_AGENT_SDK_VERSION", "CLAUDE_CODE_ENTRYPOINT"]
                .into_iter()
                .map(str::to_owned)
                .collect::<Vec<_>>(),
            "the owner's base url survives a plain launch"
        );
    }

    #[test]
    fn overrides_blank_what_the_parent_carries() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedEnv::save(&[
            "CLAUDECODE",
            "CLAUDE_CODE_SESSION_ID",
            "ANTHROPIC_BASE_URL",
            "ANTHROPIC_API_KEY",
        ]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_SESSION_ID", "s-desktop");
        std::env::set_var("ANTHROPIC_BASE_URL", "http://desktop:2000");
        std::env::set_var("ANTHROPIC_API_KEY", "owner-key");

        // The entrypoint flag is process-cached, so the entrypoint-gated
        // var is asserted through the explicit-flag builder below; the
        // always-scrubbed vars hold under either flag.
        for entrypoint in [false, true] {
            let overrides = scrub_overrides_for(entrypoint);
            assert!(
                overrides.contains(&("CLAUDECODE".to_owned(), String::new())),
                "present scrubbed vars blank (entrypoint={entrypoint}): {overrides:?}"
            );
            assert!(
                overrides.contains(&("CLAUDE_CODE_SESSION_ID".to_owned(), String::new())),
                "the session id blanks (entrypoint={entrypoint}): {overrides:?}"
            );
            assert!(
                !overrides.iter().any(|(name, _)| name == "ANTHROPIC_API_KEY"),
                "owner auth never appears (entrypoint={entrypoint}): {overrides:?}"
            );
        }
        assert!(
            scrub_overrides_for(true)
                .contains(&("ANTHROPIC_BASE_URL".to_owned(), String::new())),
            "the desktop's base url blanks under an entrypoint"
        );
        assert!(
            !scrub_overrides_for(false).iter().any(|(name, _)| name == "ANTHROPIC_BASE_URL"),
            "without the entrypoint the owner's base url survives"
        );
    }

    #[test]
    fn the_command_builder_removes_without_touching_the_process_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedEnv::save(&["CLAUDECODE", "ANTHROPIC_API_KEY"]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("ANTHROPIC_API_KEY", "owner-key");

        let mut command = std::process::Command::new("true");
        let removed = scrub_command(&mut command);
        assert!(removed.contains(&"CLAUDECODE".to_owned()), "the builder names it: {removed:?}");
        assert!(
            command.get_envs().any(|(name, value)| name == "CLAUDECODE" && value.is_none()),
            "the Command carries the removal"
        );
        assert!(
            !command.get_envs().any(|(name, _)| name == "ANTHROPIC_API_KEY"),
            "owner auth is neither set nor removed on the Command"
        );
        assert_eq!(
            std::env::var_os("CLAUDECODE"),
            Some(std::ffi::OsString::from("1")),
            "the process env is untouched"
        );
        assert_eq!(
            std::env::var_os("ANTHROPIC_API_KEY"),
            Some(std::ffi::OsString::from("owner-key")),
            "owner auth survives in the process env"
        );
    }

    #[test]
    fn concurrent_builds_never_mutate_the_process_env() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let _saved = SavedEnv::save(&["CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT", "ANTHROPIC_API_KEY"]);
        std::env::set_var("CLAUDECODE", "1");
        std::env::set_var("CLAUDE_CODE_ENTRYPOINT", "claude-desktop");
        std::env::set_var("ANTHROPIC_API_KEY", "owner-key");

        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..50 {
                        let mut command = std::process::Command::new("true");
                        let removed = scrub_command(&mut command);
                        assert!(
                            removed.contains(&"CLAUDECODE".to_owned()),
                            "every build names the scrubbed var"
                        );
                        assert!(!scrubbed_removals().is_empty(), "removals are built per call");
                    }
                });
            }
        });
        assert_eq!(
            std::env::var_os("CLAUDECODE"),
            Some(std::ffi::OsString::from("1")),
            "concurrent builds left the process env alone"
        );
        assert_eq!(
            std::env::var_os("CLAUDE_CODE_ENTRYPOINT"),
            Some(std::ffi::OsString::from("claude-desktop")),
            "the entrypoint was never removed mid-flight"
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
