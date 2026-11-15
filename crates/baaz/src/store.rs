//! Baaz's own on-disk state, next to Muse's rather than inside it.
//!
//! Muse owns `~/.config/muse` and `~/.local/share/muse`; Baaz never
//! writes to either. What Baaz has to remember — the billing tier it
//! last probed, the session names and hidden flags MSP has no
//! room for — lives under `~/Library/Application Support/baaz`,
//! which is the macOS answer for an application's own state.
//!
//! The per-turn usage ledger lives here too, as its own `baaz.db` (see
//! `crate::usage`), beside the search index rather than inside it.
//!
//! Two rules:
//!
//! * **Every write is atomic.** A file is written beside itself and renamed,
//!   so a crash mid-write leaves the previous state rather than half of the
//!   next one.
//! * **A damaged file is never wiped.** A missing file yields the default
//!   quietly. A file this build cannot parse (truncated, hand-edited, written
//!   by a newer build) is renamed aside to `<name>.corrupt-<ts>`, logged and
//!   reported ([`take_problems`] → one toast); the last good copy
//!   (`<name>.bak`, kept by [`write_authored`]) is read instead when there is
//!   one. A failed write is reported the same way and returns its error.

use std::path::PathBuf;

/// `~/Library/Application Support/baaz`, honouring `BAAZ_STATE_DIR` so a
/// test can point the whole store somewhere disposable.
pub fn support_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("BAAZ_STATE_DIR") {
        return PathBuf::from(dir);
    }
    // A test that forgot to point the store somewhere disposable must never
    // write the person's real state: `cargo test` once rewrote their
    // `projects.json`. Unit tests fall back to a per-process temp dir.
    #[cfg(test)]
    {
        return std::env::temp_dir().join(format!("baaz-test-state-{}", std::process::id()));
    }
    // The same rule for a deterministic capture (the visual probe): an entry
    // that names no state dir of its own must never write the person's real
    // store. Before this, `composer-*`, `lane-*` and `reopen-failed` captures
    // left probe prompts in the owner's history and set `provider.json`.
    #[allow(unreachable_code)]
    if crate::clock::deterministic() {
        return capture_state_dir();
    }
    #[allow(unreachable_code)]
    default_support_dir()
}

/// The throwaway store a deterministic capture with no `BAAZ_STATE_DIR`
/// uses: one fresh directory per process, so two shots of the same entry
/// start from the same (empty) state.
fn capture_state_dir() -> PathBuf {
    std::env::temp_dir().join(format!("baaz-capture-state-{}", std::process::id()))
}

/// Removes this capture's throwaway store as the capture ends. Tier V makes one
/// per shot, and 1,079 had piled up before this. Only ever the capture dir:
/// a capture with its own `BAAZ_STATE_DIR` and every real run are untouched.
pub fn remove_capture_state() {
    if crate::clock::deterministic() && std::env::var_os("BAAZ_STATE_DIR").is_none() {
        let _ = std::fs::remove_dir_all(capture_state_dir());
    }
}

/// Removes the throwaway stores of captures that died before they could
/// (`baaz-capture-state-<pid>` whose pid is gone). Off the UI thread.
pub fn sweep_dead_capture_states() {
    let _ = std::thread::Builder::new().name("capture-state-sweep".into()).spawn(|| {
        sweep_dead_capture_states_in(&std::env::temp_dir(), |pid| {
            // SAFETY: signal 0 only checks that the process exists.
            let gone = unsafe { libc::kill(pid, 0) } != 0
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            !gone
        });
    });
}

fn sweep_dead_capture_states_in(temp: &std::path::Path, alive: impl Fn(i32) -> bool) {
    let Ok(entries) = std::fs::read_dir(temp) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|name| name.strip_prefix("baaz-capture-state-")) else { continue };
        let Ok(pid) = pid.parse::<i32>() else { continue };
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) && !alive(pid) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
    /// Nothing to do: there is no old directory, or the new one already has
    /// everything the old one holds.
    NotNeeded,
    /// The old directory was renamed onto the new path.
    Moved,
    /// A rename was impossible (cross-device, permissions), so the tree was
    /// copied and the old directory removed afterwards.
    Copied,
    /// A new directory already existed - an early write beat the migration -
    /// so entries missing from it were filled in from the old one. Nothing in
    /// the new tree was overwritten, and the old directory is left in place.
    Merged,
}

/// One-time move of the pre-rename state directory onto the new path.
///
/// On startup, when `BAAZ_STATE_DIR` is unset (an explicit state dir means
/// the default paths are untouched), a `harness` directory is carried onto
/// the `baaz` path — sessions, projects, the search index, everything — so
/// existing state survives the rename. With no `baaz` directory the whole
/// tree moves; when one already exists, because an early write such as the
/// tier probe beat this call, only the entries it is missing are filled in
/// and nothing is overwritten. Never fails startup: every outcome is logged
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
        StateMigration::Merged => {
            eprintln!(
                "baaz: filled missing state from {} into {} (the new directory already existed; nothing was overwritten, the old directory was kept)",
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
    if !old.exists() {
        return StateMigration::NotNeeded;
    }
    if let Some(parent) = new.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return StateMigration::NotNeeded;
        }
    }
    // The clean case: nothing at the new path, so the whole tree moves.
    if !new.exists() {
        if std::fs::rename(old, new).is_ok() {
            return StateMigration::Moved;
        }
        if copy_dir_all(old, new).is_ok() && std::fs::remove_dir_all(old).is_ok() {
            return StateMigration::Copied;
        }
        return StateMigration::NotNeeded;
    }
    // The new directory already exists. It only takes one early write — a
    // tier probe, a crashed first launch — to create it before this runs,
    // and an all-or-nothing guard here would strand the old sessions
    // forever with no warning. Fill in whatever the new tree is missing and
    // leave everything it already has alone: the new copy is authoritative,
    // because the user has been running against it.
    if !copy_missing(old, new) {
        return StateMigration::NotNeeded;
    }
    StateMigration::Merged
}

/// Copy entries of `old` that have no counterpart in `new`, recursing into
/// directories that exist on both sides. Never overwrites. Returns whether
/// anything was copied.
fn copy_missing(old: &std::path::Path, new: &std::path::Path) -> bool {
    let Ok(entries) = std::fs::read_dir(old) else {
        return false;
    };
    let mut filled = false;
    for entry in entries.flatten() {
        let target = new.join(entry.file_name());
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if target.exists() {
                filled |= copy_missing(&entry.path(), &target);
            } else if copy_dir_all(&entry.path(), &target).is_ok() {
                filled = true;
            }
        } else if !target.exists() && std::fs::copy(entry.path(), &target).is_ok() {
            filled = true;
        }
    }
    filled
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

/// What a store met that the person should hear about (docs/81 A F8, F9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreProblem {
    /// A file could not be parsed and was renamed aside. `restored` says
    /// whether its `.bak` was read in its place.
    SetAside { name: String, aside: PathBuf, restored: bool },
    /// Entries of a map store that did not parse were dropped; the whole
    /// file was copied to `aside` first.
    EntriesDropped { name: String, count: usize, aside: PathBuf },
    /// A write failed; the file on disk is the previous one.
    WriteFailed { name: String, error: String },
}

static PROBLEMS: std::sync::Mutex<Vec<StoreProblem>> = std::sync::Mutex::new(Vec::new());
static PROBLEM_WAKERS: std::sync::Mutex<Vec<futures::channel::mpsc::UnboundedSender<()>>> =
    std::sync::Mutex::new(Vec::new());

fn report(problem: StoreProblem) {
    crate::baaz_log!("store: {problem:?}");
    PROBLEMS.lock().unwrap_or_else(|p| p.into_inner()).push(problem);
    PROBLEM_WAKERS.lock().unwrap_or_else(|p| p.into_inner()).retain(|waker| waker.unbounded_send(()).is_ok());
}

/// Every problem reported since the last call, oldest first.
pub fn take_problems() -> Vec<StoreProblem> {
    std::mem::take(&mut *PROBLEMS.lock().unwrap_or_else(|p| p.into_inner()))
}

/// A channel that receives one message per reported problem, so the window
/// raises its toast when a background write fails. Problems reported before
/// the window subscribed wait in [`take_problems`].
pub fn problem_wake() -> futures::channel::mpsc::UnboundedReceiver<()> {
    let (sender, receiver) = futures::channel::mpsc::unbounded();
    PROBLEM_WAKERS.lock().unwrap_or_else(|p| p.into_inner()).push(sender);
    receiver
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()
}

/// `<path>.bak`: the previous good copy [`write_authored`] keeps.
pub fn backup_path(path: &std::path::Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".bak");
    PathBuf::from(name)
}

/// Write `bytes` to `path` through a sibling temporary file and a rename.
///
/// The parent directory is created if it is missing. The temporary name is
/// unique per call, so two writers of one path never share it; it is removed
/// when the write fails. The file and its directory are fsynced, so a crash
/// right after leaves either the old file or the new one. Blocking; call it
/// off the UI thread.
pub fn write_atomic(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut name = path.as_os_str().to_owned();
    name.push(format!(".tmp{}-{n}", std::process::id()));
    let temporary = PathBuf::from(name);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

/// [`write_atomic`] for a store the person authored (names, pins, projects,
/// layout, tabs): the current file is kept as `<name>.bak` first, and a
/// failure is reported ([`StoreProblem::WriteFailed`]) as well as returned,
/// so the caller can leave its generation where it was.
pub fn write_authored(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if path.exists() {
        let backup = backup_path(path);
        let _ = std::fs::remove_file(&backup);
        // A hard link costs nothing and keeps the old inode once the rename
        // replaces `path`; a copy where links are refused.
        if std::fs::hard_link(path, &backup).is_err() {
            let _ = std::fs::copy(path, &backup);
        }
    }
    let result = write_atomic(path, bytes);
    if let Err(error) = &result {
        report(StoreProblem::WriteFailed { name: file_name(path), error: error.to_string() });
    }
    result
}

/// Parse a JSON file in the store, or the type's default.
///
/// A missing file is the default, quietly. A file that does not parse is
/// renamed to `<name>.corrupt-<unix secs>` — never overwritten — and
/// reported; its `.bak` is read in its place when that parses.
pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &std::path::Path) -> T {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(error) => {
            crate::baaz_log!("store: cannot read {}: {error}", path.display());
            return T::default();
        }
    };
    let error = match serde_json::from_str(&text) {
        Ok(value) => return value,
        Err(error) => error,
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".corrupt-{stamp}"));
    let aside = PathBuf::from(aside);
    crate::baaz_log!("store: {} does not parse ({error}); setting it aside", path.display());
    if std::fs::rename(path, &aside).is_err() {
        let _ = std::fs::copy(path, &aside);
    }
    let restored: Option<T> = std::fs::read_to_string(backup_path(path))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok());
    report(StoreProblem::SetAside { name: file_name(path), aside, restored: restored.is_some() });
    restored.unwrap_or_default()
}

/// A field that falls back to its default when the stored value does not
/// parse (an enum variant a newer build added, a hand-edited slug), instead
/// of failing the whole file: `#[serde(default, deserialize_with =
/// "crate::store::lenient")]`.
pub fn lenient<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: serde::de::DeserializeOwned + Default,
{
    use serde::Deserialize as _;
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(serde_json::from_value(value).unwrap_or_default())
}

/// [`read_json`] for a store that is a map of independent entries
/// (`sessions.json`, `provider-sessions.json`): one entry that does not
/// parse drops only itself, and the file is copied aside first so nothing
/// it said is lost.
pub fn read_json_map<V: serde::de::DeserializeOwned>(path: &std::path::Path) -> std::collections::HashMap<String, V> {
    let raw: serde_json::Map<String, serde_json::Value> = read_json(path);
    let mut kept = std::collections::HashMap::with_capacity(raw.len());
    let mut dropped = 0usize;
    for (key, value) in raw {
        match serde_json::from_value(value) {
            Ok(entry) => {
                kept.insert(key, entry);
            }
            Err(error) => {
                crate::baaz_log!("store: {} entry {key:?} does not parse ({error}); dropped", path.display());
                dropped += 1;
            }
        }
    }
    if dropped > 0 {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let mut aside = path.as_os_str().to_owned();
        aside.push(format!(".corrupt-{stamp}"));
        let aside = PathBuf::from(aside);
        let _ = std::fs::copy(path, &aside);
        report(StoreProblem::EntriesDropped { name: file_name(path), count: dropped, aside });
    }
    kept
}

/// Serializes the tests that point `BAAZ_STATE_DIR` at a temp dir: two
/// tests pointing it at two dirs at once would read each other's state.
///
/// Returns the guard directly, poison-tolerant on purpose: a test that
/// panics while holding the guard poisons a plain `Mutex`, and every later
/// `.lock().expect(..)` dies with `PoisonError` — a cascade, not a signal.
/// Recovering the guard keeps one test's failure from failing the suite.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

    /// docs/81 A F8: a damaged file is set aside, never overwritten, and
    /// reported; with no `.bak` the read is the default.
    #[test]
    fn an_unparseable_file_is_set_aside_and_reported() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("thing.json");
        write_atomic(&path, b"{\"a\": 1, \"trunc").expect("write");
        let _ = take_problems();
        assert_eq!(read_json::<serde_json::Map<String, serde_json::Value>>(&path).len(), 0);
        assert!(!path.exists(), "the damaged file is moved, so no later write can replace it");
        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("thing.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read(aside[0].path()).unwrap(), b"{\"a\": 1, \"trunc");
        let problems = take_problems();
        assert!(
            problems.iter().any(|p| matches!(p, StoreProblem::SetAside { name, restored: false, .. } if name == "thing.json")),
            "{problems:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The last good copy is read when the file is damaged.
    #[test]
    fn a_damaged_file_falls_back_to_its_backup() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-bak-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("kept.json");
        write_authored(&path, b"{\"a\":1}").expect("first");
        write_authored(&path, b"{\"a\":2}").expect("second");
        assert_eq!(std::fs::read(backup_path(&path)).unwrap(), b"{\"a\":1}", "one .bak: the previous write");
        std::fs::write(&path, b"").expect("truncate");
        let back: serde_json::Map<String, serde_json::Value> = read_json(&path);
        assert_eq!(back.get("a"), Some(&serde_json::json!(1)), "read from the .bak");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// docs/81 A F9: a failed write returns its error, leaves no temporary
    /// and is reported.
    #[test]
    fn a_failed_write_is_reported_and_leaves_no_temporary() {
        let _guard = test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-store-fail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // A directory where the file should be: the rename fails.
        let path = dir.join("blocked.json");
        std::fs::create_dir_all(path.join("inside")).expect("seed");
        let _ = take_problems();
        assert!(write_authored(&path, b"{}").is_err());
        let names: Vec<String> =
            std::fs::read_dir(&dir).unwrap().filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(names.iter().all(|name| !name.contains(".tmp")), "{names:?}");
        assert!(take_problems().iter().any(|p| matches!(p, StoreProblem::WriteFailed { name, .. } if name == "blocked.json")));
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

    /// The regression this guards: a tier probe once wrote
    /// `baaz/tier-probe` before any launch could migrate, and the old
    /// `new.exists()` guard then refused forever — 73 sessions stranded in
    /// `harness/sessions.json` with no warning. A partial new directory must
    /// be filled in, not treated as done.
    #[test]
    fn an_early_write_to_the_new_dir_does_not_strand_the_old_state() {
        let base = std::env::temp_dir().join(format!("baaz-merge-{}", std::process::id()));
        let old = base.join("harness");
        let new = base.join("baaz");
        let _ = std::fs::remove_dir_all(&base);
        // The old tree: the user's real state.
        std::fs::create_dir_all(old.join("nested")).expect("seed old");
        std::fs::write(old.join("sessions.json"), b"OLD-SESSIONS").expect("seed sessions");
        std::fs::write(old.join("layout.json"), b"OLD-LAYOUT").expect("seed layout");
        std::fs::write(old.join("nested").join("search.db"), b"OLD-DB").expect("seed db");
        // An early write beat the migration: the new dir exists with one file,
        // and one file that also exists on the old side with newer content.
        std::fs::create_dir_all(&new).expect("seed new");
        std::fs::write(new.join("tier-probe"), b"PROBE").expect("seed probe");
        std::fs::write(new.join("layout.json"), b"NEW-LAYOUT").expect("seed new layout");

        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::Merged);
        // What the new tree lacked is filled in, including nested trees.
        assert_eq!(std::fs::read(new.join("sessions.json")).unwrap(), b"OLD-SESSIONS");
        assert_eq!(std::fs::read(new.join("nested").join("search.db")).unwrap(), b"OLD-DB");
        // What it already had is untouched: the user has been running against it.
        assert_eq!(std::fs::read(new.join("layout.json")).unwrap(), b"NEW-LAYOUT");
        assert_eq!(std::fs::read(new.join("tier-probe")).unwrap(), b"PROBE");
        // The old directory is kept, so a merge is never a destructive step.
        assert!(old.exists(), "a merge leaves the old directory in place");
        // Running again finds nothing missing and reports so.
        assert_eq!(migrate_support_dir_at(&new, &old), StateMigration::NotNeeded);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A test that panics while holding the env lock must not fail every
    /// later env-guarded test with `PoisonError`: the lock recovers, so one
    /// failure stays one failure instead of cascading across the suite.
    #[test]
    fn a_panicking_test_does_not_poison_the_next_tests_lock() {
        let probe = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _held = super::test_env_lock();
            panic!("a failing test panics while holding the env lock");
        }));
        assert!(probe.is_err(), "the probe must really panic while holding the lock");
        // The next acquisition recovers instead of dying with PoisonError.
        let _guard = super::test_env_lock();
    }
}

#[cfg(test)]
mod isolation_tests {
    /// Without `BAAZ_STATE_DIR`, a test build's store is a temp dir, never
    /// `~/Library/Application Support/baaz`.
    #[test]
    fn an_unset_state_dir_never_resolves_to_the_real_support_dir_in_tests() {
        let _guard = super::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::remove_var("BAAZ_STATE_DIR");
        let dir = super::support_dir();
        if let Some(value) = old {
            std::env::set_var("BAAZ_STATE_DIR", value);
        }
        assert_ne!(dir, super::default_support_dir());
        assert!(dir.starts_with(std::env::temp_dir()), "{dir:?}");
    }

    #[test]
    fn only_dead_capture_states_are_swept() {
        let temp = std::env::temp_dir().join(format!("baaz-capsweep-{}", std::process::id()));
        for name in ["baaz-capture-state-111", "baaz-capture-state-222", "baaz-capture-state-x", "baaz-other-111"] {
            std::fs::create_dir_all(temp.join(name)).unwrap();
        }
        super::sweep_dead_capture_states_in(&temp, |pid| pid == 222);
        assert!(!temp.join("baaz-capture-state-111").exists(), "dead");
        assert!(temp.join("baaz-capture-state-222").exists(), "alive");
        assert!(temp.join("baaz-capture-state-x").exists(), "not a pid");
        assert!(temp.join("baaz-other-111").exists(), "not ours");
        let _ = std::fs::remove_dir_all(&temp);
    }
}
