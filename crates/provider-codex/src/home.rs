//! Baaz's own Codex home: `CODEX_HOME` for every app-server child.
//!
//! Baaz's app-server threads (`source=vscode`) are listed by Codex for Mac,
//! which enumerates its own `~/.codex` with no originator filter. Children
//! now run with `CODEX_HOME=<state dir>/codex-home`, so sessions, the state
//! db and logs all live under Baaz's own state dir.
//!
//! The home holds symlinks to the owner's `config.toml`, `auth.json`,
//! `skills/` and `plugins/`: config and login keep working, while the
//! session stores stay private. `auth.json` holds a rotating refresh
//! token, so a copy would fork the rotation and log the owner out — it is
//! a symlink, re-checked before each spawn (codex may rewrite it in place
//! by temp-file-plus-rename, quietly replacing the link with a private
//! copy). The `*.sqlite` files are never linked: two processes reaching
//! one db through a link and a real path can corrupt it (the `-wal` and
//! `-shm` sit beside the opened path).

use std::path::{Path, PathBuf};

/// The Baaz-owned Codex home's name under the state dir.
pub const HOME_DIR_NAME: &str = "codex-home";

/// Owner entries symlinked into the Baaz home when they exist. Notably no
/// `*.sqlite`: those stay private to each home (see the module docs).
pub const LINKED_ENTRIES: &[&str] = &["config.toml", "auth.json", "skills", "plugins"];

/// The auth file: the one entry whose link is re-checked before each
/// spawn, because its token rotates.
pub const AUTH_FILE_NAME: &str = "auth.json";

/// `<state_dir>/codex-home`.
pub fn home_dir(state_dir: &Path) -> PathBuf {
    state_dir.join(HOME_DIR_NAME)
}

/// The default Baaz-owned home for this run: under
/// [`provider::child_env::state_dir`], so `BAAZ_STATE_DIR` redirects it.
/// Env-only: creates nothing.
pub fn default_home() -> PathBuf {
    home_dir(&provider::child_env::state_dir())
}

/// The owner's Codex root: `~/.codex` under `owner_home`.
pub fn owner_home_dir(owner_home: &Path) -> PathBuf {
    owner_home.join(".codex")
}

/// Ensure the Baaz home exists with its owner symlinks: creates
/// `<state_dir>/codex-home` and links every [`LINKED_ENTRIES`] entry whose
/// source exists in the owner's `~/.codex` and whose target is absent.
/// Idempotent; never overwrites a real file, never copies, never writes
/// inside the owner's home. Returns the home path.
///
/// The per-spawn [`relink_auth`] check is separate: this only ever fills
/// in what is absent, so a rotated `auth.json` copy is left alone here
/// and repaired there, where the intent is explicit.
pub fn ensure_home(owner_home: &Path, state_dir: &Path) -> std::io::Result<PathBuf> {
    let home = home_dir(state_dir);
    provider::child_env::ensure_linked_dir(&home, &owner_home_dir(owner_home), LINKED_ENTRIES)?;
    Ok(home)
}

/// Re-check the `auth.json` link before a spawn: when the owner's
/// `auth.json` exists and the Baaz home's copy is absent, link it; when
/// the Baaz copy exists but is no longer a symlink, see
/// [`relink_auth_at`]. An existing symlink — even one pointing elsewhere
/// — is left alone. No owner `auth.json`, no work.
pub fn relink_auth(owner_home: &Path, state_dir: &Path) -> std::io::Result<()> {
    relink_auth_at(&owner_home_dir(owner_home).join(AUTH_FILE_NAME), &home_dir(state_dir))
}

/// Whether the Baaz home's real `auth.json` is newer than the owner's: a
/// token Codex rotated into the Baaz home after the owner's last write.
/// `false` on any metadata error — the move-aside path below preserves
/// both files, so doubt relinks rather than keeps.
fn baaz_auth_is_newer(owner_auth: &Path, dst: &Path) -> bool {
    let (Ok(owner_meta), Ok(dst_meta)) =
        (std::fs::metadata(owner_auth), std::fs::metadata(dst))
    else {
        return false;
    };
    let (Ok(owner_mtime), Ok(dst_mtime)) = (owner_meta.modified(), dst_meta.modified()) else {
        return false;
    };
    dst_mtime > owner_mtime
}

/// The aside name for a real `auth.json` the relink moves out of the way:
/// `auth.json.replaced-<unix secs>`, with a counter when the second ticks
/// collide. The token survives beside the new link, never deleted.
fn replaced_name(dst: &Path) -> PathBuf {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let base = format!("{AUTH_FILE_NAME}.replaced-{secs}");
    let mut candidate = dst.with_file_name(&base);
    let mut attempt = 1u32;
    while std::fs::symlink_metadata(&candidate).is_ok() {
        attempt += 1;
        candidate = dst.with_file_name(format!("{base}-{attempt}"));
    }
    candidate
}

/// Link `owner_auth` at `dst`, creating the home dir first. Unix-only:
/// provider homes need symlinks.
#[cfg(unix)]
fn link_owner_auth(owner_auth: &Path, home: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(home)?;
    std::os::unix::fs::symlink(owner_auth, dst)
}

/// The re-check behind [`relink_auth`], against explicit paths so tests
/// drive it with temp dirs.
///
/// Never deletes a token: when the Baaz copy exists as a real file (Codex
/// replaced the link by rename on a token rotation), a copy NEWER than the
/// owner's is left in place — Baaz keeps using it — and logged once,
/// while an older-or-equal copy is renamed aside to
/// `auth.json.replaced-<unix>` and the link is restored. Either way both
/// tokens survive on disk.
fn relink_auth_at(owner_auth: &Path, home: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(owner_auth).is_err() {
        return Ok(());
    }
    let dst = home.join(AUTH_FILE_NAME);
    match std::fs::symlink_metadata(&dst) {
        Err(_) => {
            #[cfg(unix)]
            link_owner_auth(owner_auth, home, &dst)?;
            #[cfg(not(unix))]
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "provider homes need symlinks",
            ));
        }
        Ok(meta) if meta.file_type().is_symlink() => {}
        Ok(_) => {
            if baaz_auth_is_newer(owner_auth, &dst) {
                static KEEP_LOGGED: std::sync::Once = std::sync::Once::new();
                KEEP_LOGGED.call_once(|| {
                    eprintln!(
                        "baaz: keeping newer Baaz Codex auth at {}; the owner's token stays for the next rotation",
                        dst.display()
                    );
                });
                return Ok(());
            }
            let aside = replaced_name(&dst);
            std::fs::rename(&dst, &aside)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(owner_auth, &dst)?;
            #[cfg(not(unix))]
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "provider homes need symlinks",
            ));
        }
    }
    Ok(())
}

/// The child environment addition for an app-server child rooted at
/// `home`: `CODEX_HOME` naming the Baaz home. Applied on top of the
/// [`provider::child_env`] scrub — never instead of it.
pub fn child_env(home: &Path) -> Vec<(String, String)> {
    vec![("CODEX_HOME".to_owned(), home.to_string_lossy().into_owned())]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "provider-codex-home-{name}-{}-{}",
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

    fn seed_owner(owner: &Path) {
        let codex = owner.join(".codex");
        std::fs::create_dir_all(codex.join("skills")).expect("owner skills");
        std::fs::create_dir_all(codex.join("plugins")).expect("owner plugins");
        std::fs::write(codex.join("config.toml"), "model = \"x\"\n").expect("owner config");
        std::fs::write(codex.join("auth.json"), "{\"token\":\"t\"}").expect("owner auth");
        // A sqlite db the home must never link.
        std::fs::write(codex.join("state_5.sqlite"), "db").expect("owner db");
        std::fs::write(codex.join("state_5.sqlite-wal"), "wal").expect("owner wal");
    }

    #[test]
    fn the_home_lives_under_the_given_state_dir() {
        let state = PathBuf::from("/tmp/baaz-state");
        assert_eq!(home_dir(&state), PathBuf::from("/tmp/baaz-state/codex-home"));
    }

    #[test]
    fn links_reach_the_owner_files_and_never_the_sqlite_files() {
        let root = temp_root("links");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(&owner).expect("owner home");
        seed_owner(&owner);

        let home = ensure_home(&owner, &state).expect("ensure");
        assert_eq!(home, state.join("codex-home"));
        for name in LINKED_ENTRIES {
            let dst = home.join(name);
            assert!(
                std::fs::symlink_metadata(&dst).expect("linked").file_type().is_symlink(),
                "{name} is a link, not a copy"
            );
        }
        assert_eq!(
            std::fs::read_to_string(home.join("auth.json")).expect("reads through"),
            "{\"token\":\"t\"}"
        );
        for name in ["state_5.sqlite", "state_5.sqlite-wal", "state_5.sqlite-shm", "sessions"] {
            assert!(
                std::fs::symlink_metadata(home.join(name)).is_err(),
                "never linked: {name}"
            );
        }
        // The owner's tree gained nothing.
        assert!(
            std::fs::symlink_metadata(owner.join(".codex").join("sessions")).is_err(),
            "nothing is written inside the owner's home"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn link_creation_is_idempotent_and_never_overwrites_a_real_file() {
        let root = temp_root("idempotent");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(&owner).expect("owner home");
        seed_owner(&owner);

        ensure_home(&owner, &state).expect("first ensure");
        ensure_home(&owner, &state).expect("second ensure is a no-op");
        let home = state.join("codex-home");
        assert!(std::fs::symlink_metadata(home.join("config.toml")).expect("meta").file_type().is_symlink());

        // A real file in the way survives re-ensures.
        std::fs::remove_file(home.join("config.toml")).expect("unlink");
        std::fs::write(home.join("config.toml"), "mine").expect("a real file in the way");
        ensure_home(&owner, &state).expect("re-ensure");
        assert_eq!(
            std::fs::read_to_string(home.join("config.toml")).expect("reads"),
            "mine",
            "a real file is never overwritten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_auth_recheck_links_when_absent_and_leaves_links_alone() {
        let root = temp_root("relink");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(&owner).expect("owner home");
        seed_owner(&owner);

        // Absent: linked.
        relink_auth(&owner, &state).expect("relink creates");
        let dst = state.join("codex-home").join("auth.json");
        assert!(std::fs::symlink_metadata(&dst).expect("meta").file_type().is_symlink());

        // Still a link: untouched (even the target is not second-guessed).
        relink_auth(&owner, &state).expect("relink keeps");
        assert!(std::fs::symlink_metadata(&dst).expect("meta").file_type().is_symlink());

        // No owner auth: no work, no error.
        std::fs::remove_file(owner.join(".codex").join("auth.json")).expect("owner signed out");
        std::fs::remove_file(&dst).expect("unlink");
        relink_auth(&owner, &state).expect("no owner auth is fine");
        assert!(std::fs::symlink_metadata(&dst).is_err(), "nothing planted without a source");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Rewrite `path` until its mtime is strictly after `older`'s (or the
    /// deadline passes): filesystems with one-second granularity need the
    /// tick to turn over before "newer" is observable.
    fn make_newer_than(path: &Path, older: &Path) {
        let start = std::time::Instant::now();
        let body = std::fs::read(path).expect("read to rewrite");
        loop {
            std::fs::write(path, &body).expect("rewrite to bump the mtime");
            let (Ok(newer), Ok(base)) =
                (std::fs::metadata(path).and_then(|meta| meta.modified()),
                 std::fs::metadata(older).and_then(|meta| meta.modified()))
            else {
                panic!("mtimes must be readable");
            };
            if newer > base {
                return;
            }
            assert!(start.elapsed() < std::time::Duration::from_secs(30), "the clock never advanced");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn the_auth_recheck_keeps_a_newer_real_file() {
        let root = temp_root("relink-newer");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(&owner).expect("owner home");
        seed_owner(&owner);
        let owner_auth = owner.join(".codex").join("auth.json");
        let home = state.join("codex-home");
        std::fs::create_dir_all(&home).expect("baaz home");

        // Codex rotated the token into the Baaz home after the owner's
        // last write: the newest token stays where it is, as a real file.
        let dst = home.join("auth.json");
        std::fs::write(&dst, "{\"token\":\"rotated-newest\"}").expect("codex's rotated copy");
        make_newer_than(&dst, &owner_auth);
        relink_auth(&owner, &state).expect("relink keeps");
        assert!(
            std::fs::symlink_metadata(&dst).expect("meta").file_type().is_file(),
            "a newer real file is never replaced by a link"
        );
        assert_eq!(
            std::fs::read_to_string(&dst).expect("reads"),
            "{\"token\":\"rotated-newest\"}",
            "the newest token survives"
        );
        assert_eq!(
            std::fs::read_to_string(&owner_auth).expect("reads"),
            "{\"token\":\"t\"}",
            "the owner's token is untouched too"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_auth_recheck_moves_an_older_real_file_aside_and_relinks() {
        let root = temp_root("relink-older");
        let owner = root.join("owner-home");
        let state = root.join("state");
        std::fs::create_dir_all(&owner).expect("owner home");
        seed_owner(&owner);
        let owner_auth = owner.join(".codex").join("auth.json");
        let home = state.join("codex-home");
        std::fs::create_dir_all(&home).expect("baaz home");

        // A stale private copy predates the owner's current token: it is
        // renamed aside (never deleted) and the link is restored.
        let dst = home.join("auth.json");
        std::fs::write(&dst, "{\"token\":\"stale\"}").expect("a stale private copy");
        make_newer_than(&owner_auth, &dst);
        relink_auth(&owner, &state).expect("relink repairs");
        assert!(
            std::fs::symlink_metadata(&dst).expect("meta").file_type().is_symlink(),
            "the link is restored"
        );
        assert_eq!(
            std::fs::read_to_string(&dst).expect("reads through"),
            "{\"token\":\"t\"}",
            "the link reaches the owner's auth again"
        );
        let asides: Vec<_> = std::fs::read_dir(&home)
            .expect("read home")
            .filter_map(|entry| entry.ok().map(|entry| entry.file_name()))
            .filter(|name| name.to_string_lossy().starts_with("auth.json.replaced-"))
            .collect();
        assert_eq!(asides.len(), 1, "exactly one aside file: {asides:?}");
        assert_eq!(
            std::fs::read_to_string(home.join(&asides[0])).expect("reads"),
            "{\"token\":\"stale\"}",
            "the older token survives beside the link"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_codex_child_env_names_the_home_and_nothing_scrubbed() {
        let home = PathBuf::from("/tmp/baaz-state/codex-home");
        let env = child_env(&home);
        assert_eq!(
            env,
            vec![("CODEX_HOME".to_owned(), "/tmp/baaz-state/codex-home".to_owned())],
            "one var, the Baaz home"
        );
        // Composed with the scrub, no scrubbed var survives.
        let parent = [
            ("CLAUDECODE", "1"),
            ("CLAUDE_CODE_ENTRYPOINT", "claude-desktop"),
            ("CLAUDE_AGENT_SDK_VERSION", "1"),
            ("CODEX_HOME", "/Users/a/.codex"),
            ("PATH", "/usr/bin:/bin"),
        ];
        let mut full: Vec<(String, String)> = parent
            .iter()
            .filter(|(name, _)| !provider::child_env::should_scrub(name, true))
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        full.extend(env);
        // The explicit Baaz home overrides the inherited owner one: the
        // last write wins on a real `Command`.
        let homes: Vec<&str> =
            full.iter().filter(|(name, _)| name == "CODEX_HOME").map(|(_, v)| v.as_str()).collect();
        assert_eq!(homes.last(), Some(&"/tmp/baaz-state/codex-home"));
        for (name, _) in &full {
            assert!(
                !provider::child_env::should_scrub(name, true),
                "no scrubbed var survives: {name}"
            );
        }
    }
}
