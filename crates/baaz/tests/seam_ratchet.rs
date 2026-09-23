//! The provider seam's ratchet: `muse_client` coupling may only go down.
//!
//! `conn.rs` and `wire.rs` now reach the wire through `provider` and
//! `provider-muse` rather than talking to `muse_client` themselves. The rest
//! of the app still does: the session and transcript code passes MSP schema
//! types around, and `conn.rs` keeps a deliberately named transitional
//! bundle — the shared transport and the raw event pump — because the
//! session views cannot yet be served by `Command` and `ProviderEvent`
//! alone.
//!
//! That transitional state is fine. What is not fine is it growing back.
//! Without a mechanical check a seam rots in a week, and a comment is not a
//! check — so this test pins the count and only ever lets it fall.
//!
//! **When this test fails because the number went down, lower `CEILING`.**
//! When it fails because the number went up, do not raise it: the new code
//! should be reaching the wire through the seam instead.

use std::fs;
use std::path::{Path, PathBuf};

/// The most files under `crates/baaz/src` that may mention `muse_client`.
///
/// Measured after the connection path moved behind the seam. It may only
/// ever be reduced.
const CEILING: usize = 20;

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

fn files_mentioning_muse_client() -> Vec<String> {
    let root = source_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut hits: Vec<String> = files
        .into_iter()
        .filter(|path| {
            fs::read_to_string(path).is_ok_and(|text| text.contains("muse_client"))
        })
        .map(|path| {
            path.strip_prefix(&root).unwrap_or(&path).display().to_string()
        })
        .collect();
    hits.sort();
    hits
}

#[test]
fn muse_client_coupling_only_ever_goes_down() {
    let hits = files_mentioning_muse_client();
    assert!(
        hits.len() <= CEILING,
        "{} files under crates/baaz/src mention `muse_client`, above the ceiling of {}.\n\
         New code must reach the wire through `provider` / `provider-muse`, not directly.\n\
         Raising CEILING is not the fix.\nFiles:\n  {}",
        hits.len(),
        CEILING,
        hits.join("\n  "),
    );
}

#[test]
fn the_two_rewired_files_are_accounted_for() {
    let hits = files_mentioning_muse_client();
    // `wire.rs` issues neutral `Command`s and must never mention the wire crate again.
    assert!(
        !hits.iter().any(|f| f == "wire.rs"),
        "wire.rs mentions `muse_client` again; it issues neutral commands and must not",
    );
    // `conn.rs` still does, for the named transitional bundle the module doc
    // describes. Pinned so its removal is a deliberate act that updates this test,
    // rather than something nobody notices either way.
    assert!(
        hits.iter().any(|f| f == "conn.rs"),
        "conn.rs no longer mentions `muse_client` — the transitional bundle is gone. \
         Good: remove this assertion and lower CEILING.",
    );
}
