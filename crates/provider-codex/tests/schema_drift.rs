//! The schema drift gate: the checked-in `schemas/codex/` bundle is pinned
//! to one CLI version, and the protocol may move under us.
//!
//! Two halves, because tests never reach a real CLI (CLAUDE.md rule 4;
//! docs/82 5.2 — this test used to run the installed `codex` on every
//! `cargo test`, so its result depended on what the machine had installed):
//!
//! * [`the_committed_schema_bundle_matches_its_manifest`] runs on every
//!   `cargo test`, offline: every committed schema file hashes to its
//!   `schemas/codex/MANIFEST.sha256` entry and `VERSION` names the pin. A
//!   one-byte edit to a committed schema fails here.
//! * [`the_installed_codex_regenerates_the_pinned_bundle`] is the drift gate
//!   proper, `#[ignore]`d and gated on `BAAZ_LIVE_CODEX_SCHEMA=1`, run by
//!   hand when the CLI moves:
//!
//! ```text
//! BAAZ_LIVE_CODEX_SCHEMA=1 cargo test -p provider-codex --test schema_drift -- --ignored --nocapture
//! ```
//!
//! It never spawns a live `codex app-server` session (that spends the
//! owner's money): it runs only two local metadata commands — `codex
//! --version` and `codex app-server generate-json-schema --out <tempdir>` —
//! and compares hashes. Nothing in this test writes into `schemas/codex/`.
//! Its outcomes, in order:
//!
//! 1. `codex` absent: visible skip, printing why.
//! 2. Version mismatch against `schemas/codex/VERSION`: a loud skip, not a
//!    failure. The gate is pinned to a version and a newer CLI is the
//!    owner's business.
//! 3. On a version match: regenerate into a temp dir, hash every `.json`,
//!    and compare against `schemas/codex/MANIFEST.sha256`. Any mismatch
//!    FAILS with counts and the first few paths. One entry — the v2
//!    aggregate bundle, which the CLI serializes with unstable key order —
//!    is compared by canonical content instead of bytes; everything else is
//!    byte-pinned.
//!
//! A drift is NOT a bug: it means the CLI moved, and this gate exists to
//! make that a decision rather than a surprise. Do NOT "fix" a failure by
//! regenerating the manifest — that launders drift into the new definition
//! of correct.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the checked-in bundle lives, from this crate's manifest dir.
fn schema_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schemas/codex")
}

/// sha256 of one file, via the platform hasher. Two spellings are tried
/// (`sha256sum`, then `shasum -a 256`) so the gate works wherever the owner
/// runs it; both print `<hash>  <path>`.
fn sha256_file(path: &Path) -> String {
    let attempts: &[(&str, &[&str])] =
        &[("sha256sum", &[]), ("shasum", &["-a", "256"])];
    for (program, args) in attempts {
        let mut command = Command::new(program);
        command.args(*args).arg(path);
        match command.output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                let hash = text.split_whitespace().next().unwrap_or_default();
                assert!(
                    hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()),
                    "unparseable hash output from {program} for {}: {text:?}",
                    path.display()
                );
                return hash.to_owned();
            }
            _ => continue,
        }
    }
    panic!(
        "no sha256 hasher found (tried sha256sum, shasum -a 256) to hash {}",
        path.display()
    );
}

/// Every `.json` file under `dir`, as `/`-joined paths relative to `dir`.
fn json_files_relative(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<String>) {
        let entries = std::fs::read_dir(dir)
            .unwrap_or_else(|error| panic!("cannot read dir {}: {error}", dir.display()));
        for entry in entries {
            let entry = entry.expect("dir entry reads");
            let path = entry.path();
            if path.is_dir() {
                walk(&path, root, out);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                let rel = path
                    .strip_prefix(root)
                    .expect("walk stays under root")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// `MANIFEST.sha256` as `relative path -> expected hash`.
fn read_manifest(schema_dir: &Path) -> BTreeMap<String, String> {
    let text = std::fs::read_to_string(schema_dir.join("MANIFEST.sha256"))
        .expect("schemas/codex/MANIFEST.sha256 reads");
    let mut entries = BTreeMap::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let (hash, path) = line
            .split_once(char::is_whitespace)
            .map(|(hash, rest)| (hash, rest.trim()))
            .unwrap_or_else(|| panic!("MANIFEST.sha256 line {} is malformed: {line:?}", index + 1));
        assert!(
            hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            "MANIFEST.sha256 line {} has a bad hash: {line:?}",
            index + 1
        );
        entries.insert(path.to_owned(), hash.to_owned());
    }
    assert!(!entries.is_empty(), "MANIFEST.sha256 parsed to zero entries");
    entries
}

/// Recursively key-sorted copy of a JSON value, for comparing files the
/// CLI serializes nondeterministically (see below).
fn canonical(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut out = serde_json::Map::with_capacity(map.len());
            for key in keys {
                out.insert(key.clone(), canonical(&map[key]));
            }
            serde_json::Value::Object(out)
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(canonical).collect())
        }
        _ => value.clone(),
    }
}

/// The one manifest entry the CLI serializes nondeterministically:
/// `codex_app_server_protocol.v2.schemas.json` is a 516-definition bundle
/// the CLI writes out of a hash map, so key order varies run to run
/// (proven: two consecutive generations on the same machine hash
/// differently with identical content). Byte-pinning it would fail
/// forever, so the gate compares its content canonically instead — a real
/// CLI move still changes definitions and still fails. This exception
/// covers this file only; every other entry is byte-pinned.
const ORDER_UNSTABLE_BUNDLE: &str = "codex_app_server_protocol.v2.schemas.json";

/// Which top-level `definitions` entries differ between two canonicalized
/// bundles, so a content drift names names instead of just failing.
fn differing_definitions(
    expected: &serde_json::Value,
    actual: &serde_json::Value,
) -> Vec<String> {
    let mut differing = Vec::new();
    let expected_defs = expected.get("definitions").and_then(|defs| defs.as_object());
    let actual_defs = actual.get("definitions").and_then(|defs| defs.as_object());
    match (expected_defs, actual_defs) {
        (Some(expected_defs), Some(actual_defs)) => {
            let mut names: Vec<&String> =
                expected_defs.keys().chain(actual_defs.keys()).collect();
            names.sort();
            names.dedup();
            for name in names {
                if expected_defs.get(name) != actual_defs.get(name) {
                    differing.push(name.clone());
                }
            }
            if expected != actual && differing.is_empty() {
                differing.push("<outside definitions>".to_owned());
            }
        }
        _ => differing.push("<unrecognized bundle shape>".to_owned()),
    }
    differing
}

fn explain_drift() -> &'static str {
    "A drift is NOT a bug: it means the CLI moved, and this gate exists to \
     make that a decision rather than a surprise. Do NOT regenerate \
     schemas/codex/MANIFEST.sha256 to make this pass — that launders drift \
     into the new definition of correct."
}

/// The first few paths of a diff list, so the failure names names instead
/// of just saying hashes differ.
fn first_few(paths: &[String]) -> String {
    const SHOWN: usize = 10;
    let mut shown = paths.iter().take(SHOWN).cloned().collect::<Vec<_>>().join(", ");
    if paths.len() > SHOWN {
        shown.push_str(&format!(" (+{} more)", paths.len() - SHOWN));
    }
    shown
}

/// The committed bundle agrees with its own manifest — offline, every run.
#[test]
fn the_committed_schema_bundle_matches_its_manifest() {
    let schema_dir = schema_dir();
    let pinned = std::fs::read_to_string(schema_dir.join("VERSION")).expect("schemas/codex/VERSION reads");
    assert!(!pinned.trim().is_empty(), "schemas/codex/VERSION names the pinned CLI version");
    let manifest = read_manifest(&schema_dir);
    let (checked, drift) = committed_drift(&schema_dir, &manifest);
    assert!(checked > 0, "no committed schema file is listed in MANIFEST.sha256");
    assert!(
        drift.is_empty(),
        "drift gate: {} committed schema file(s) disagree with MANIFEST.sha256 \
         ({checked} of {} manifest entries are committed here): {}. {}.",
        drift.len(),
        manifest.len(),
        first_few(&drift),
        explain_drift()
    );
}

/// The committed files listed in `manifest` that do not hash to their
/// entry, and how many were checked. Files the checkout does not carry
/// (the `v2/` payload) are covered only by the live regeneration.
fn committed_drift(schema_dir: &Path, manifest: &BTreeMap<String, String>) -> (usize, Vec<String>) {
    let mut drift = Vec::new();
    let mut checked = 0usize;
    for (rel, expected) in manifest {
        let path = schema_dir.join(rel);
        if !path.is_file() {
            continue;
        }
        checked += 1;
        let actual = sha256_file(&path);
        if &actual != expected {
            drift.push(format!("{rel} (manifest {expected}, committed {actual})"));
        }
    }
    drift.sort();
    (checked, drift)
}

/// The drift gate proper: the installed `codex` regenerates the pinned
/// bundle byte for byte. Runs the real CLI, so never under `cargo test`.
#[test]
#[ignore = "runs the installed codex; BAAZ_LIVE_CODEX_SCHEMA=1 to run"]
fn the_installed_codex_regenerates_the_pinned_bundle() {
    if std::env::var("BAAZ_LIVE_CODEX_SCHEMA").as_deref() != Ok("1") {
        println!("drift gate: SKIP — BAAZ_LIVE_CODEX_SCHEMA is not 1; the installed codex was not run");
        return;
    }
    let schema_dir = schema_dir();

    // 1. `codex` absent: skip visibly, printing why.
    let version_output = match Command::new("codex").arg("--version").output() {
        Ok(output) => output,
        Err(error) => {
            println!(
                "drift gate: SKIP — `codex` is not on PATH ({error}); \
                 cannot check the schema bundle without the CLI that owns it"
            );
            return;
        }
    };
    let raw = String::from_utf8_lossy(&version_output.stdout);
    let raw = raw.trim();
    let raw = if raw.is_empty() {
        String::from_utf8_lossy(&version_output.stderr).trim().to_owned()
    } else {
        raw.to_owned()
    };
    // `codex --version` prints `codex-cli 0.144.6` — mind the prefix.
    let cli_version =
        raw.strip_prefix("codex-cli").map(str::trim_start).unwrap_or(&raw).to_owned();
    let pinned = std::fs::read_to_string(schema_dir.join("VERSION"))
        .expect("schemas/codex/VERSION reads")
        .trim()
        .to_owned();

    // 2. Version mismatch: a loud skip, not a failure. The gate is pinned
    // to a version and a newer CLI is the owner's business.
    if cli_version != pinned {
        println!(
            "drift gate: SKIP — CLI version {cli_version:?} does not match pinned \
             {pinned:?}; the gate only runs against the pinned version"
        );
        return;
    }

    let manifest = read_manifest(&schema_dir);
    // The committed subset against the same manifest (the offline half
    // checks it too): the regeneration below is compared against it.
    let (committed_checked, committed_drift) = committed_drift(&schema_dir, &manifest);

    // 3. Regenerate into a temp dir (never into `schemas/codex/`) and hash
    // every `.json` against the manifest.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let tempdir =
        std::env::temp_dir().join(format!("codex-schema-drift-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&tempdir)
        .unwrap_or_else(|error| panic!("cannot create tempdir {}: {error}", tempdir.display()));
    // Every exit below removes the temp dir first: the test borrows the
    // directory, it never keeps it.
    let cleanup = |tempdir: &Path| {
        if let Err(error) = std::fs::remove_dir_all(tempdir) {
            println!("drift gate: warning — could not remove {}: {error}", tempdir.display());
        }
    };

    let generate = Command::new("codex")
        .arg("app-server")
        .arg("generate-json-schema")
        .arg("--out")
        .arg(&tempdir)
        .output();
    let generate = match generate {
        Ok(output) => output,
        Err(error) => {
            cleanup(&tempdir);
            panic!("drift gate: `codex app-server generate-json-schema` would not run: {error}");
        }
    };
    if !generate.status.success() {
        cleanup(&tempdir);
        panic!(
            "drift gate: schema regeneration failed: {}",
            String::from_utf8_lossy(&generate.stderr).trim()
        );
    }
    let generated = json_files_relative(&tempdir);
    let generated_hashes: BTreeMap<String, String> = generated
        .iter()
        .map(|rel| (rel.clone(), sha256_file(&tempdir.join(rel))))
        .collect();

    let mut differed = Vec::new();
    let mut missing = Vec::new();
    for (rel, expected) in &manifest {
        // The order-unstable bundle is compared by content, not by bytes
        // (see `ORDER_UNSTABLE_BUNDLE`): the committed file it is compared
        // against is itself byte-pinned by the manifest in the committed
        // check above, so no pin is dropped.
        if rel == ORDER_UNSTABLE_BUNDLE {
            match generated_hashes.get(rel) {
                Some(_) => {
                    let regenerated_text = std::fs::read_to_string(tempdir.join(rel))
                        .unwrap_or_else(|error| {
                            panic!("drift gate: regenerated {rel} does not read: {error}")
                        });
                    let committed_text = std::fs::read_to_string(schema_dir.join(rel))
                        .unwrap_or_else(|error| {
                            panic!("drift gate: committed {rel} does not read: {error}")
                        });
                    let parse = |text: &str, which: &str| {
                        serde_json::from_str::<serde_json::Value>(text).unwrap_or_else(|error| {
                            panic!("drift gate: {which} {rel} is not JSON: {error}")
                        })
                    };
                    let regenerated = canonical(&parse(&regenerated_text, "regenerated"));
                    let committed = canonical(&parse(&committed_text, "committed"));
                    if regenerated != committed {
                        let names = differing_definitions(&committed, &regenerated);
                        differed.push(format!(
                            "{rel} (content differs ignoring key order; \
                             {} definition(s): {})",
                            names.len(),
                            first_few(&names)
                        ));
                    }
                }
                None => missing.push(rel.clone()),
            }
            continue;
        }
        match generated_hashes.get(rel) {
            Some(actual) if actual != expected => {
                differed.push(format!("{rel} (manifest {expected}, regenerated {actual})"));
            }
            None => missing.push(rel.clone()),
            _ => {}
        }
    }
    let generated_set: std::collections::BTreeSet<&String> = generated_hashes.keys().collect();
    let mut extra: Vec<String> = generated_set
        .into_iter()
        .filter(|rel| !manifest.contains_key(*rel))
        .cloned()
        .collect();
    extra.sort();
    missing.sort();
    differed.sort();
    cleanup(&tempdir);

    // 4. Mismatches fail with counts and the first few paths — "hashes
    // differ" alone wastes the next hour.
    if !committed_drift.is_empty() {
        panic!(
            "drift gate: {} committed schema file(s) disagree with MANIFEST.sha256 \
             ({} of {} manifest entries are committed here): {}. {}.",
            committed_drift.len(),
            committed_checked,
            manifest.len(),
            first_few(&committed_drift),
            explain_drift()
        );
    }
    if !differed.is_empty() || !missing.is_empty() || !extra.is_empty() {
        panic!(
            "drift gate: CLI {pinned} regenerated {} file(s) with {} differing, {} missing, \
             {} extra versus MANIFEST.sha256 ({} entries). differing: {}. missing: {}. extra: {}. {}.",
            generated.len(),
            differed.len(),
            missing.len(),
            extra.len(),
            manifest.len(),
            first_few(&differed),
            first_few(&missing),
            first_few(&extra),
            explain_drift()
        );
    }
    println!(
        "drift gate: PASS — {} regenerated file(s) match MANIFEST.sha256 ({} entries), \
         {} committed file(s) agree",
        generated.len(),
        manifest.len(),
        committed_checked
    );
}
