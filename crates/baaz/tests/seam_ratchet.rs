//! The provider seam's ratchet: Baaz never talks to a provider's wire.
//!
//! Since P4 (docs/63) every provider, Muse included, reaches the app
//! through `provider` and its own adapter crate: `provider-muse` is the only
//! crate that names Muse's wire, and `crates/baaz` depends on neither
//! `muse-client` nor `muse-adapter`. The checks below keep it that way: no
//! source file mentions either crate, the manifest lists neither, and no
//! view asks whether it rides a "provider lane" — every session does, and a
//! provider's own features are registry properties
//! (`crate::providers::ProviderId`), never a fork on its name.
//!
//! Without a mechanical check a seam rots in a week, and a comment is not a
//! check. When one of these fails, the new code should be reaching the wire
//! through the seam instead.

use std::fs;
use std::path::{Path, PathBuf};

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
fn no_file_names_muse_client() {
    let hits = files_mentioning_muse_client();
    assert!(
        hits.is_empty(),
        "{} files under crates/baaz/src mention `muse_client`.\n\
         New code must reach the wire through `provider` / `provider-muse`, not directly.\nFiles:\n  {}",
        hits.len(),
        hits.join("\n  "),
    );
}

#[test]
fn the_rewired_files_stay_off_the_wire() {
    let hits = files_mentioning_muse_client();
    // `wire.rs` issues neutral `Command`s and must never mention the wire crate again.
    assert!(
        !hits.iter().any(|f| f == "wire.rs"),
        "wire.rs mentions `muse_client` again; it issues neutral commands and must not",
    );
    // `conn.rs` neither: the transitional bundle went with P4 step 6c, and
    // the connection is the hub alone.
    assert!(
        !hits.iter().any(|f| f == "conn.rs"),
        "conn.rs mentions `muse_client` again; the connection is the hub alone",
    );
}

/// Every `.rs` file under `crates/baaz/src` that contains `needle`.
fn files_mentioning(needle: &str) -> Vec<String> {
    let root = source_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut hits: Vec<String> = files
        .into_iter()
        .filter(|path| fs::read_to_string(path).is_ok_and(|text| text.contains(needle)))
        .map(|path| path.strip_prefix(&root).unwrap_or(&path).display().to_string())
        .collect();
    hits.sort();
    hits
}

#[test]
fn no_file_names_the_muse_adapter_either() {
    let hits = files_mentioning("muse_adapter");
    assert!(hits.is_empty(), "these files reach past the seam into `muse_adapter`: {hits:?}");
}

#[test]
fn the_manifest_lists_no_wire_crate() {
    let manifest = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).expect("manifest");
    for wire in ["muse-client", "muse-adapter"] {
        let listed = manifest.lines().any(|line| {
            let line = line.trim_start();
            !line.starts_with('#') && (line.starts_with(&format!("{wire}.")) || line.starts_with(&format!("{wire} ")))
        });
        assert!(!listed, "crates/baaz depends on `{wire}` again; reach Muse through `provider-muse`");
    }
}

/// The second ratchet: no view forks on "is this the provider lane" or
/// on the old legacy-pump question. Every session rides a lane; what a
/// provider offers beyond the seam is a registry property.
#[test]
fn no_view_forks_on_the_lane_it_rides() {
    for fork in ["is_provider_lane", "uses_legacy_pump", "Lane::Muse"] {
        let hits = files_mentioning(fork);
        assert!(hits.is_empty(), "`{fork}` is back in {hits:?}: name the feature in the registry instead");
    }
}
