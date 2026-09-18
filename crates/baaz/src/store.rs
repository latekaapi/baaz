//! Baaz's own on-disk state, next to Muse's rather than inside it.
//!
//! Muse owns `~/.config/muse` and `~/.local/share/muse`; Baaz never
//! writes to either. What Baaz has to remember — the billing tier it
//! last probed (Phase 5 A1), the session names and hidden flags MSP has no
//! room for (A2) — lives under `~/Library/Application Support/baaz`,
//! which is the macOS answer for an application's own state.
//!
//! Two rules:
//!
//! * **Every write is atomic.** A file is written beside itself and renamed,
//!   so a crash mid-write leaves the previous state rather than half of the
//!   next one.
//! * **Every read is best-effort.** A missing file, a truncated file and a
//!   file this build cannot parse are all ordinary; each yields the default
//!   rather than an error a person has to see.

use std::path::PathBuf;

/// `~/Library/Application Support/baaz`, honouring `BAAZ_STATE_DIR` so a
/// test can point the whole store somewhere disposable.
pub fn support_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BAAZ_STATE_DIR") {
        return PathBuf::from(dir);
    }
    default_support_dir()
}

/// `~/Library/Application Support/baaz` regardless of `BAAZ_STATE_DIR`:
/// the directory the real app writes, which a scripted run with its own state
/// dir still has to recognise as Baaz's own (the tier probe's throwaway
/// workspace lives there and is never a project to adopt).
pub fn default_support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("baaz")
}

/// The pre-rename default, `~/Library/Application Support/harness`.
///
/// The name changed with the product (harness → Baaz); this path exists only
/// so [`migrate_legacy_support_dir`] can move it aside once. Nothing new is
/// ever written here.
pub fn legacy_default_support_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("harness")
}

/// What [`migrate_support_dir_at`] did: the startup log prints it, and the
/// unit test asserts on it.
#[derive(Debug, PartialEq, Eq)]
pub enum StateMigration {
    /// Nothing to do: the new directory already exists, or there is no old
    /// one to move.
    NotNeeded,
    /// The old directory was renamed onto the new path.
    Moved,
    /// A rename was impossible (cross-device, permissions), so the tree was
    /// copied and the old directory removed afterwards.
    Copied,
}

/// One-time move of the pre-rename state directory onto the new path.
///
/// On startup, when `BAAZ_STATE_DIR` is unset (an explicit state dir means
/// the default paths are untouched), the default is `baaz`, and no `baaz`
/// directory exists yet while a `harness` one does, the old directory is
/// moved — sessions, projects, the search index, everything — so existing
/// state survives the rename. Never fails startup: every outcome is logged
/// and returned, and the app continues against whatever is on disk.
pub fn migrate_legacy_support_dir() -> StateMigration {
    if std::env::var_os("BAAZ_STATE_DIR").is_some() {
        return StateMigration::NotNeeded;
    }
    let outcome = migrate_support_dir_at(&default_support_dir(), &legacy_default_support_dir());
    match &outcome {
        StateMigration::NotNeeded => {}
        StateMigration::Moved => {
            eprintln!(
                "baaz: migrated state {} -> {}",
                legacy_default_support_dir().display(),
                default_support_dir().display()
            );
        }
        StateMigration::Copied => {
            eprintln!(
                "baaz: copied state {} -> {} (rename impossible; old directory removed after a complete copy)",
                legacy_default_support_dir().display(),
                default_support_dir().display()
            );
        }
    }
    outcome
}

/// The move itself, against explicit paths so the unit test never touches
/// `HOME` or the real `Application Support`.
fn migrate_support_dir_at(new: &std::path::Path, old: &std::path::Path) -> StateMigration {
    if new.exists() || !old.exists() {
        return StateMigration::NotNeeded;
    }
    if let Some(parent) = new.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return StateMigration::NotNeeded;
        }
    }
    if std::fs::rename(old, new).is_ok() {
        return StateMigration::Moved;
    }
    if copy_dir_all(old, new).is_ok() && std::fs::remove_dir_all(old).is_ok() {
        return StateMigration::Copied;
    }
    StateMigration::NotNeeded
}

/// Recursively copy a directory tree: `rename` cannot cross devices, and the
/// two support paths share a parent, but a symlinked or oddly-mounted home
/// still gets here.
fn copy_dir_all(old: &std::path::Path, new: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(new)?;
    for entry in std::fs::read_dir(old)? {
        let entry = entry?;
        let target = new.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_dir_all(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Write `bytes` to `path` through a sibling temporary file and a rename.
///
/// The parent directory is created if it is missing. Blocking; call it off the
/// UI thread.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&temporary, bytes)?;
    match std::fs::rename(&temporary, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Parse a JSON file in the store, or the type's default.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Serializes the tests that point `BAAZ_STATE_DIR` at a temp dir: two
/// tests pointing it at two dirs at once would read each other's state.
#[cfg(test)]
pub(crate) fn test_env_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    &LOCK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_replaces_the_previous_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("baaz-store-{}", std::process::id()));
        let path = dir.join("thing.json");
        write_atomic(&path, b"{\"a\":1}").expect("first write");
        write_atomic(&path, b"{\"a\":2}").expect("second write");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":2}");
        let leftovers = std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).count();
        assert_eq!(leftovers, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unparseable_file_reads_as_the_default() {
        let dir = std::env::temp_dir().join(format!("baaz-store-bad-{}", std::process::id()));
        let path = dir.join("thing.json");
        write_atomic(&path, b"not json").expect("write");
        assert_eq!(read_json::<serde_json::Map<String, serde_json::Value>>(&path).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_legacy_state_dir_moves_to_the_new_path_once() {
        let base = std::env::temp_dir().join(format!("baaz-migrate-{}", std::process::id()));
        let old = base.join("harness");
        let new = base.join("baaz");
        let _ = std::fs::remove_dir_all(&base);
        // No old dir, no new dir: nothing to do.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        // An old dir with sessions, projects and a nested database moves whole.
        std::fs::create_dir_all(old.join("nested")).expect("seed old");
        std::fs::write(old.join("sessions.json"), b"{}").expect("seed sessions");
        std::fs::write(old.join("nested").join("search.db"), b"db").expect("seed db");
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::Moved);
        assert!(!old.exists(), "the old path is gone after the move");
        assert_eq!(std::fs::read_to_string(new.join("sessions.json")).unwrap(), "{}");
        assert_eq!(std::fs::read(new.join("nested").join("search.db")).unwrap(), b"db");
        // A second run is a no-op: the new dir already exists.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        let _ = std::fs::remove_dir_all(&base);
    }
}
