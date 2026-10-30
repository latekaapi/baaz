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

// ------------------------------------------------------------------ P4-B

/// The open/row paths docs/70 §5 names: since P4-B a Muse session opens,
/// reopens and moves its row through the same code as Codex, so none of
/// these files compares a provider against Muse by name. What Muse does
/// differently is a registry property on [`ProviderId`]; constructing the
/// value (`ProviderId::Muse.as_str()`, a fixture row) is not a fork.
const OPEN_ROW_PATHS: [&str; 6] = [
    "app/lifecycle.rs",
    "app.rs",
    "app/titles.rs",
    "right_changes.rs",
    "session/lane.rs",
    "session/commands.rs",
];

/// `text` with every `#[cfg(test)]`-gated `mod … { … }` removed. Braces
/// are matched by a small lexer that skips string, raw-string, char and
/// comment contents, so a `"{"` in a test's message cannot unbalance it.
fn strip_test_modules(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        if text[i..].starts_with("#[cfg(test)]") {
            // Skip further attributes and whitespace to see what it gates.
            let mut j = i + "#[cfg(test)]".len();
            loop {
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                if text[j..].starts_with("#[") {
                    while j < bytes.len() && bytes[j] != b']' {
                        j += 1;
                    }
                    j += 1;
                } else {
                    break;
                }
            }
            let rest = &text[j..];
            let rest = rest.strip_prefix("pub(crate) ").or_else(|| rest.strip_prefix("pub ")).unwrap_or(rest);
            if rest.starts_with("mod ") {
                if let Some(open) = text[j..].find('{').map(|at| j + at) {
                    if let Some(end) = matching_brace(bytes, open) {
                        i = end + 1;
                        continue;
                    }
                }
            }
        }
        let ch = text[i..].chars().next().expect("char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// The index of the `}` closing the `{` at `open`, skipping literals and
/// comments.
fn matching_brace(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                    i += 1;
                }
                i += 1;
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"') | Some(b'#'))
                && (i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_')) =>
            {
                let mut j = i + 1;
                let mut hashes = 0;
                while bytes.get(j) == Some(&b'#') {
                    hashes += 1;
                    j += 1;
                }
                if bytes.get(j) == Some(&b'"') {
                    j += 1;
                    loop {
                        if j >= bytes.len() {
                            return None;
                        }
                        if bytes[j] == b'"' && (0..hashes).all(|k| bytes.get(j + 1 + k) == Some(&b'#')) {
                            i = j + hashes;
                            break;
                        }
                        j += 1;
                    }
                }
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'\'' => {
                // A char literal ('x', '\n', '{'), not a lifetime ('a).
                if bytes.get(i + 1) == Some(&b'\\') {
                    i += 2;
                    while i < bytes.len() && bytes[i] != b'\'' {
                        i += 1;
                    }
                } else if bytes.get(i + 2) == Some(&b'\'') {
                    i += 2;
                }
            }
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Every comparison against Muse by name in `text` (test modules already
/// stripped), as `line: source` strings. Any path to the variant counts
/// (`ProviderId::Muse`, `providers::ProviderId::Muse`,
/// `crate::providers::ProviderId::Muse`). A use is a comparison when the
/// variant sits beside `==` / `!=`, heads a match arm (`=>`, or `|` into
/// one), is a `contains(&…)` probe, or is inside `matches!(…)`.
fn muse_comparisons(text: &str) -> Vec<String> {
    let needle = "ProviderId::Muse";
    let mut hits = Vec::new();
    for (number, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or(line);
        let mut from = 0;
        while let Some(at) = code[from..].find(needle).map(|at| from + at) {
            let end = at + needle.len();
            from = end;
            // `ProviderId::MuseFoo` is another name.
            if code[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let before = code[..at]
                .trim_end_matches(|c: char| c.is_alphanumeric() || c == '_' || c == ':')
                .trim_end_matches(|c: char| c.is_whitespace() || c == '(' || c == '&')
                .trim_end_matches("Some")
                .trim_end_matches(|c: char| c.is_whitespace() || c == '(' || c == '&');
            let after = code[end..].trim_start_matches(|c: char| c.is_whitespace() || c == ')');
            let in_matches = code[..at].rfind("matches!(").is_some_and(|open| !code[open..at].contains(';'));
            let compared = before.ends_with("==")
                || before.ends_with("!=")
                || before.ends_with("contains")
                || after.starts_with("==")
                || after.starts_with("!=")
                || after.starts_with("=>")
                || (after.starts_with('|') && !after.starts_with("||"))
                || (before.ends_with('|') && !before.ends_with("||"))
                || in_matches;
            if compared {
                hits.push(format!("{}: {}", number + 1, line.trim()));
            }
        }
    }
    hits
}

#[test]
fn the_open_and_row_paths_never_compare_against_muse() {
    let root = source_root();
    let mut found = Vec::new();
    for file in OPEN_ROW_PATHS {
        let text = fs::read_to_string(root.join(file)).unwrap_or_else(|_| panic!("read {file}"));
        for hit in muse_comparisons(&strip_test_modules(&text)) {
            found.push(format!("{file}:{hit}"));
        }
    }
    assert!(
        found.is_empty(),
        "{} comparisons against `ProviderId::Muse` in the open/row paths (docs/70 §5): name the \
         difference as a registry property in `crate::providers` instead.\n  {}",
        found.len(),
        found.join("\n  "),
    );
}

#[test]
fn the_muse_ratchet_sees_what_it_must_and_nothing_else() {
    let sample = "\
fn a(p: ProviderId) {
    if p == ProviderId::Muse {}
    if crate::providers::ProviderId::Muse != p {}
    if x == Some(crate::providers::ProviderId::Muse) {}
    match p { ProviderId::Muse => {}, _ => {} }
    match (a, p) { (None, ProviderId::Muse) => {} _ => {} }
    let m = matches!(p, ProviderId::ClaudeCode | ProviderId::Muse);
    if due.contains(&ProviderId::Muse) {}
    let s = ProviderId::Muse.as_str();
    let f = factory(ProviderId::Muse);
    // p == ProviderId::Muse in a comment is prose
}
#[cfg(test)]
mod tests {
    fn t() { let s = \"{\"; assert!(p == ProviderId::Muse); }
}
fn after() { if p == ProviderId::Muse {} }
";
    let hits = muse_comparisons(&strip_test_modules(sample));
    assert_eq!(hits.len(), 8, "{hits:#?}");
    assert!(hits.iter().all(|hit| !hit.contains("as_str") && !hit.contains("factory(")));
    assert!(hits.last().is_some_and(|hit| hit.contains("fn after")), "code after a test module is scanned");
}
