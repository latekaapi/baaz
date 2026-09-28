//! Finding `muse` when the app was launched from the Dock.
//!
//! A GUI launch gets launchd's minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`),
//! while `muse` lives in a home install such as `~/.local/bin/muse`. Spawning
//! a bare `"muse"` then fails with "No such file or directory (os error 2)".
//! This module pushes the pattern `baaz` already uses for its other providers
//! down into `muse-client`: an env override, then `PATH`, then fixed fallback
//! dirs — plus a repaired `PATH` for the child so the `muse` wrapper and the
//! tools muse runs (git, python, brew-installed CLIs) work from a Dock launch.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use crate::error::MuseError;

/// The binary this crate spawns.
pub const MUSE_BINARY: &str = "muse";

/// The environment variable that names the `muse` binary directly.
///
/// A file names the binary; a directory names the binary inside it. Either
/// way it must exist to count: a set-but-missing override is a miss, not a
/// fallthrough to `PATH`, so an explicit but wrong path never silently
/// resolves elsewhere.
pub const MUSE_BIN_ENV: &str = "MUSE_BIN";

/// The fixed fallback install dirs, in search order.
fn muse_fallback_dirs_for(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]
}

/// The fixed fallback install dirs, rooted at the real `HOME`.
pub fn muse_fallback_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    muse_fallback_dirs_for(&home)
}

/// Where the `muse` binary comes from, in order: the [`MUSE_BIN_ENV`]
/// override, else a `PATH` lookup, else the [`muse_fallback_dirs`]
/// fallbacks. The fallbacks matter because the app also launches from the
/// Dock with a minimal `PATH` that names almost nothing.
///
/// `None` when nothing on the search path exists.
pub fn resolve_muse_program() -> Option<PathBuf> {
    let path_dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    let env = std::env::var_os(MUSE_BIN_ENV).map(PathBuf::from);
    resolve_muse_program_with(env.as_deref(), &path_dirs, &muse_fallback_dirs())
}

/// The search behind [`resolve_muse_program`], pure so tests can drive it
/// with a temp dir instead of the real `PATH` and `HOME`: pass an empty
/// `PATH` list and the Dock case is covered; pass empty fallbacks and
/// nothing outside the given dirs can answer.
pub fn resolve_muse_program_with(
    env_override: Option<&Path>,
    path_dirs: &[PathBuf],
    fallbacks: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(candidate) = env_override.filter(|p| !p.as_os_str().is_empty()) {
        // An explicit override names the binary directly; a directory names
        // the binary inside it. Either way it must exist to count.
        let direct = candidate.to_path_buf();
        if is_program_file(&direct) {
            return Some(direct);
        }
        let nested = candidate.join(MUSE_BINARY);
        if is_program_file(&nested) {
            return Some(nested);
        }
        return None;
    }
    if let Some(found) = path_dirs.iter().map(|dir| dir.join(MUSE_BINARY)).find(|p| is_program_file(p))
    {
        return Some(found);
    }
    fallbacks.iter().map(|dir| dir.join(MUSE_BINARY)).find(|p| is_program_file(p))
}

/// An existing file counts as a program. The executable bit is deliberately
/// not checked: the unit tests seed plain files, and a missing bit surfaces
/// as the spawn's own error rather than as "not installed".
fn is_program_file(path: &Path) -> bool {
    path.is_file()
}

/// What a spawn failure says when nothing was found: where it looked, and
/// the override that names the binary directly.
pub fn muse_not_found_message() -> String {
    format!(
        "{MUSE_BINARY} was not found on PATH or in ~/.local/bin, /opt/homebrew/bin, /usr/local/bin (set {MUSE_BIN_ENV} to its path)"
    )
}

/// The `PATH` value for the `muse` child: the current `PATH` first, then the
/// resolved program's directory, then the given fallback dirs — deduplicated,
/// first occurrence wins.
///
/// Pure: the caller passes the already-known dirs (typically the existing
/// fallback dirs only — there is no point putting a missing dir on `PATH`).
/// A program with no directory component (a bare `"muse"`) contributes no
/// dir of its own.
pub fn child_path_value(
    current_dirs: &[PathBuf],
    program: &Path,
    fallback_dirs: &[PathBuf],
) -> OsString {
    let mut out: Vec<PathBuf> = Vec::with_capacity(
        current_dirs.len().saturating_add(fallback_dirs.len()).saturating_add(1),
    );
    let mut push = |dir: &Path| {
        let dir = dir.to_path_buf();
        if !out.contains(&dir) {
            out.push(dir);
        }
    };
    for dir in current_dirs {
        push(dir);
    }
    if let Some(parent) = program.parent().filter(|p| !p.as_os_str().is_empty()) {
        push(parent);
    }
    for dir in fallback_dirs {
        push(dir);
    }
    std::env::join_paths(&out).unwrap_or_else(|_| {
        let mut joined = OsString::new();
        for (i, dir) in out.iter().enumerate() {
            if i > 0 {
                joined.push(OsStr::new(PATH_SEP));
            }
            joined.push(dir);
        }
        joined
    })
}

#[cfg(not(windows))]
const PATH_SEP: &str = ":";
#[cfg(windows)]
const PATH_SEP: &str = ";";

/// Whether the child needs a repaired `PATH`: the current `PATH` lacks the
/// resolved program's directory or one of the existing fallback dirs.
///
/// Missing fallback dirs (not installed on this machine) never count: there
/// is no point putting a dir that does not exist on `PATH`.
pub(crate) fn child_path_needs_repair(
    program: &Path,
    path_dirs: &[PathBuf],
    fallback_dirs: &[PathBuf],
) -> bool {
    let program_dir_missing = program
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .is_some_and(|parent| !path_dirs.iter().any(|dir| dir.as_path() == parent));
    program_dir_missing
        || fallback_dirs
            .iter()
            .filter(|dir| dir.is_dir())
            .any(|dir| !path_dirs.contains(dir))
}

/// The program [`MuseClient::spawn`](crate::MuseClient::spawn) actually
/// executes: an explicit path must exist as a file, while a bare file name
/// is looked up on `PATH` and then the fallbacks, exactly like the OS would
/// plus the Dock-launch fallbacks.
///
/// `Err` (an [`MuseError::Io`] `NotFound` carrying
/// [`muse_not_found_message`]) when nothing is found, so the app can say
/// where it looked instead of surfacing a bare "No such file or directory".
pub(crate) fn resolve_spawn_program(
    program: &Path,
    path_dirs: &[PathBuf],
    fallback_dirs: &[PathBuf],
) -> Result<PathBuf, MuseError> {
    let not_found = || {
        MuseError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            muse_not_found_message(),
        ))
    };
    if let Some(name) = bare_file_name(program) {
        let hit = path_dirs
            .iter()
            .map(|dir| dir.join(name))
            .chain(fallback_dirs.iter().map(|dir| dir.join(name)))
            .find(|p| is_program_file(p));
        return hit.ok_or_else(not_found);
    }
    if is_program_file(program) {
        Ok(program.to_path_buf())
    } else {
        Err(not_found())
    }
}

/// The file name when `program` names no directory (a bare `"muse"` or
/// `"true"`); `None` for anything with a directory component, absolute or
/// relative.
fn bare_file_name(program: &Path) -> Option<&OsStr> {
    match program.parent() {
        Some(parent) if parent.as_os_str().is_empty() => program.file_name(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_root(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "muse-client-resolve-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ))
    }

    fn seeded_binary(dir: &Path, name: &str) -> PathBuf {
        std::fs::create_dir_all(dir).expect("temp dir");
        let path = dir.join(name);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").expect("seed binary");
        path
    }

    #[test]
    fn the_env_override_names_the_binary_directly() {
        let root = test_root("env-file");
        let file = seeded_binary(&root, MUSE_BINARY);
        let (paths, fallbacks) = (Vec::new(), Vec::new());
        assert_eq!(resolve_muse_program_with(Some(&file), &paths, &fallbacks), Some(file));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_env_override_names_the_directory_holding_the_binary() {
        let root = test_root("env-dir");
        let expected = seeded_binary(&root, MUSE_BINARY);
        let (paths, fallbacks) = (Vec::new(), Vec::new());
        assert_eq!(resolve_muse_program_with(Some(&root), &paths, &fallbacks), Some(expected));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_set_but_missing_override_is_a_miss_not_a_fallthrough() {
        let path_dir = test_root("miss-path");
        seeded_binary(&path_dir, MUSE_BINARY);
        let missing = path_dir.join("no-such-dir");
        let fallbacks: Vec<PathBuf> = Vec::new();
        // `PATH` holds `muse`, but the explicit (wrong) override wins and
        // misses: it must not silently resolve elsewhere.
        assert_eq!(
            resolve_muse_program_with(
                Some(&missing),
                std::slice::from_ref(&path_dir),
                &fallbacks
            ),
            None
        );
        let _ = std::fs::remove_dir_all(&path_dir);
    }

    #[test]
    fn path_lookup_finds_the_binary() {
        let dir = test_root("path-hit");
        let expected = seeded_binary(&dir, MUSE_BINARY);
        let fallbacks: Vec<PathBuf> = Vec::new();
        assert_eq!(
            resolve_muse_program_with(None, std::slice::from_ref(&dir), &fallbacks),
            Some(expected)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dock_fallbacks_cover_an_empty_path() {
        let home = test_root("dock-home");
        let local_bin = home.join(".local/bin");
        let expected = seeded_binary(&local_bin, MUSE_BINARY);
        // No `PATH` entries at all, as from the Dock: the fallbacks still
        // find it.
        assert_eq!(
            resolve_muse_program_with(None, &[], &muse_fallback_dirs_for(&home)),
            Some(expected)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn nothing_found_is_none() {
        let empty: Vec<PathBuf> = Vec::new();
        assert_eq!(resolve_muse_program_with(None, &[], &[]), None);
        assert_eq!(
            resolve_muse_program_with(Some(Path::new("/no/such/place")), &[], &[]),
            None
        );
    }

    #[test]
    fn child_path_keeps_current_path_first_then_program_dir_then_fallbacks() {
        let current = vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")];
        let program = PathBuf::from("/Users/a/.local/bin/muse");
        let fallbacks =
            vec![PathBuf::from("/Users/a/.local/bin"), PathBuf::from("/opt/homebrew/bin")];
        let value = child_path_value(&current, &program, &fallbacks);
        assert_eq!(
            value,
            OsString::from("/usr/bin:/bin:/Users/a/.local/bin:/opt/homebrew/bin")
        );
    }

    #[test]
    fn child_path_deduplicates() {
        let current = vec![PathBuf::from("/usr/bin"), PathBuf::from("/opt/homebrew/bin")];
        let program = PathBuf::from("/usr/bin/muse");
        let fallbacks = vec![PathBuf::from("/opt/homebrew/bin"), PathBuf::from("/usr/local/bin")];
        let value = child_path_value(&current, &program, &fallbacks);
        assert_eq!(value, OsString::from("/usr/bin:/opt/homebrew/bin:/usr/local/bin"));
    }

    #[test]
    fn child_path_bare_program_contributes_no_dir() {
        let current = vec![PathBuf::from("/usr/bin")];
        let value = child_path_value(&current, Path::new("muse"), &[PathBuf::from("/opt/homebrew/bin")]);
        assert_eq!(value, OsString::from("/usr/bin:/opt/homebrew/bin"));
    }

    #[test]
    fn repair_triggers_only_on_something_missing() {
        let root = test_root("repair");
        let home_bin = root.join("homebin");
        std::fs::create_dir_all(&home_bin).expect("temp dir");
        let program = home_bin.join(MUSE_BINARY);
        // Program dir on `PATH`, existing fallback covered: no repair.
        let current = vec![home_bin.clone(), PathBuf::from("/usr/bin")];
        assert!(!child_path_needs_repair(&program, &current, &[home_bin.clone()]));
        // Program dir missing from `PATH`: repair.
        assert!(child_path_needs_repair(&program, &[PathBuf::from("/usr/bin")], &[]));
        // An existing fallback missing from `PATH`: repair.
        assert!(child_path_needs_repair(
            &PathBuf::from("/usr/bin/muse"),
            &[PathBuf::from("/usr/bin")],
            &[home_bin]
        ));
        // A fallback that does not exist never counts.
        assert!(!child_path_needs_repair(
            &PathBuf::from("/usr/bin/muse"),
            &[PathBuf::from("/usr/bin")],
            &[root.join("not-installed")]
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn spawn_resolution_finds_a_bare_name_on_path() {
        let dir = test_root("spawn-path");
        let expected = seeded_binary(&dir, MUSE_BINARY);
        let found =
            resolve_spawn_program(Path::new(MUSE_BINARY), std::slice::from_ref(&dir), &[])
                .expect("found on PATH");
        assert_eq!(found, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_resolution_accepts_an_existing_explicit_path() {
        let dir = test_root("spawn-abs");
        let expected = seeded_binary(&dir, MUSE_BINARY);
        let found = resolve_spawn_program(&expected, &[], &[]).expect("explicit path");
        assert_eq!(found, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn spawn_resolution_errors_where_it_looked() {
        let empty: Vec<PathBuf> = Vec::new();
        let err = resolve_spawn_program(Path::new(MUSE_BINARY), &[], &[]).expect_err("nothing found");
        let text = err.to_string();
        assert!(
            text.contains("muse was not found on PATH or in ~/.local/bin, /opt/homebrew/bin, /usr/local/bin (set MUSE_BIN to its path)"),
            "unexpected message: {text}"
        );
        let missing = PathBuf::from("/no/such/muse-binary");
        let err = resolve_spawn_program(&missing, &[], &[]).expect_err("explicit path missing");
        assert!(err.to_string().contains("set MUSE_BIN to its path"));
    }
}
