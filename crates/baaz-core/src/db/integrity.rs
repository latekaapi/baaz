//! Keeping baaz.db whole (docs/85 11.4).
//!
//! * **Unclean exit.** While Baaz has the database open, `baaz.db.open` sits
//!   beside it; a clean quit removes it ([`mark_clean_exit`]). Found at the
//!   next open, the last run did not quit cleanly, so the file gets a
//!   `PRAGMA quick_check` first; a file that fails it is set aside and the
//!   newest backup is restored in its place, with one toast
//!   ([`crate::store::StoreProblem::DatabaseRestored`]).
//! * **Backups.** Before a migration step runs, the file is copied to
//!   `baaz.db.pre-v<N>` (N the version it was at) — after checking the disk
//!   has room for it. Once a day, a clean open also refreshes `baaz.db.bak`
//!   (`VACUUM INTO`). The restore takes the newest of these.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

/// The marker beside an open database.
fn marker(path: &Path) -> PathBuf {
    path.with_extension("db.open")
}

/// The rolling backup.
pub fn rolling_backup(path: &Path) -> PathBuf {
    path.with_extension("db.bak")
}

/// The backup taken before a migration from `version`.
pub fn pre_migration_backup(path: &Path, version: i32) -> PathBuf {
    path.with_extension(format!("db.pre-v{version}"))
}

/// Whether the last run that opened `path` quit without closing it.
pub fn last_exit_unclean(path: &Path) -> bool {
    marker(path).exists()
}

/// Note that `path` is open (the marker a clean quit removes).
pub fn mark_open(path: &Path) {
    let _ = std::fs::write(marker(path), std::process::id().to_string());
}

/// A clean quit: the marker goes.
pub fn mark_clean_exit(path: &Path) {
    let _ = std::fs::remove_file(marker(path));
}

/// `PRAGMA quick_check` on the file at `path`: `true` when it answers ok.
pub fn quick_check(path: &Path) -> bool {
    let Ok(conn) = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return false };
    conn.query_row("PRAGMA quick_check", [], |row| row.get::<_, String>(0)).is_ok_and(|answer| answer == "ok")
}

/// The newest backup of `path` that passes its own check.
pub fn newest_good_backup(path: &Path) -> Option<PathBuf> {
    let dir = path.parent()?;
    let stem = path.file_name()?.to_string_lossy().into_owned();
    let mut backups: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|candidate| {
            candidate.file_name().map(|n| n.to_string_lossy().into_owned()).is_some_and(|name| {
                name == format!("{stem}.bak") || name.starts_with(&format!("{stem}.pre-v"))
            })
        })
        .filter_map(|candidate| std::fs::metadata(&candidate).and_then(|m| m.modified()).ok().map(|at| (at, candidate)))
        .collect();
    backups.sort_by_key(|backup| std::cmp::Reverse(backup.0));
    backups.into_iter().map(|(_, candidate)| candidate).find(|candidate| quick_check(candidate))
}

/// After an unclean exit: a file that fails its check is set aside and the
/// newest good backup takes its place. Returns what was restored, when
/// anything was.
pub fn repair_after_unclean_exit(path: &Path) -> Option<PathBuf> {
    if !path.exists() || quick_check(path) {
        return None;
    }
    let backup = newest_good_backup(path);
    let aside = path.with_extension(format!("db.damaged-{}", crate::usage::now_ms()));
    let _ = std::fs::rename(path, &aside);
    for side in ["db-wal", "db-shm"] {
        let _ = std::fs::remove_file(path.with_extension(side));
    }
    match &backup {
        Some(backup) => {
            let _ = std::fs::copy(backup, path);
            crate::store::report(crate::store::StoreProblem::DatabaseRestored {
                from: backup.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            });
        }
        None => crate::store::report(crate::store::StoreProblem::DatabaseUnavailable {
            error: format!("baaz.db was damaged and no backup was good; it was kept as {}", aside.display()),
        }),
    }
    backup
}

/// Free bytes on the volume holding `path`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let dir = if path.is_dir() { path } else { path.parent()? };
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs fills the struct it is given; `c` is a valid C string.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut stat) };
    (rc == 0).then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
}

/// Before a migration from `version`: the disk must hold a copy of the file
/// and room for the step (twice its size, plus 16 MB), and the copy is
/// taken. `Err` refuses the migration with its reason.
pub fn before_migration(path: &Path, version: i32, free: Option<u64>) -> Result<(), String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let need = size.saturating_mul(2) + 16 * 1024 * 1024;
    if let Some(free) = free {
        if free < need {
            return Err(format!("only {free} bytes free; a database update needs {need}"));
        }
    }
    std::fs::copy(path, pre_migration_backup(path, version)).map(|_| ()).map_err(|error| format!("no backup before the update ({error})"))
}

/// Refresh the rolling backup when it is missing or a day old.
pub fn refresh_rolling_backup(conn: &Connection, path: &Path) {
    let backup = rolling_backup(path);
    let fresh = std::fs::metadata(&backup)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|at| at.elapsed().ok())
        .is_some_and(|age| age < std::time::Duration::from_secs(24 * 3600));
    if fresh {
        return;
    }
    let tmp = path.with_extension("db.bak.tmp");
    let _ = std::fs::remove_file(&tmp);
    let quoted = tmp.to_string_lossy().replace('\'', "''");
    if conn.execute_batch(&format!("VACUUM INTO '{quoted}'")).is_ok() {
        let _ = std::fs::rename(&tmp, &backup);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-integrity-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp");
        dir
    }

    fn good_db(path: &Path, value: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(&format!("CREATE TABLE t(v TEXT); INSERT INTO t VALUES ('{value}');")).unwrap();
    }

    #[test]
    fn a_damaged_file_after_an_unclean_exit_comes_back_from_its_backup() {
        let dir = temp("restore");
        let path = dir.join("baaz.db");
        good_db(&path, "backed-up");
        std::fs::copy(&path, rolling_backup(&path)).unwrap();
        // Damage the live file: garbage over its pages.
        let mut bytes = std::fs::read(&path).unwrap();
        for byte in bytes.iter_mut().skip(100) {
            *byte = 0xAB;
        }
        std::fs::write(&path, bytes).unwrap();
        assert!(!quick_check(&path), "the damage is seen");
        mark_open(&path);
        assert!(last_exit_unclean(&path));
        let restored = repair_after_unclean_exit(&path).expect("restored");
        assert_eq!(restored, rolling_backup(&path));
        let conn = Connection::open(&path).unwrap();
        let value: String = conn.query_row("SELECT v FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(value, "backed-up");
        assert!(std::fs::read_dir(&dir).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains("damaged")), "the damaged file is kept aside");
        mark_clean_exit(&path);
        assert!(!last_exit_unclean(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_whole_file_is_left_alone() {
        let dir = temp("whole");
        let path = dir.join("baaz.db");
        good_db(&path, "fine");
        assert_eq!(repair_after_unclean_exit(&path), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_migration_needs_room_and_takes_a_copy() {
        let dir = temp("room");
        let path = dir.join("baaz.db");
        good_db(&path, "x");
        assert!(before_migration(&path, 4, Some(1024)).is_err(), "too little room is refused");
        before_migration(&path, 4, Some(u64::MAX)).expect("room");
        assert!(pre_migration_backup(&path, 4).exists());
        assert!(free_bytes(&dir).is_some_and(|free| free > 0));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
