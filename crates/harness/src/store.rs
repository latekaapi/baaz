//! The harness's own on-disk state, next to Muse's rather than inside it.
//!
//! Muse owns `~/.config/muse` and `~/.local/share/muse`; the harness never
//! writes to either. What the harness has to remember — the billing tier it
//! last probed (Phase 5 A1), the session names and hidden flags MSP has no
//! room for (A2) — lives under `~/Library/Application Support/harness`,
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

/// `~/Library/Application Support/harness`, honouring `HARNESS_STATE_DIR` so a
/// test can point the whole store somewhere disposable.
pub fn support_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HARNESS_STATE_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Library").join("Application Support").join("harness")
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rename_replaces_the_previous_contents_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("harness-store-{}", std::process::id()));
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
        let dir = std::env::temp_dir().join(format!("harness-store-bad-{}", std::process::id()));
        let path = dir.join("thing.json");
        write_atomic(&path, b"not json").expect("write");
        assert_eq!(read_json::<serde_json::Map<String, serde_json::Value>>(&path).len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
