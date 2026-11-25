//! The provider seam's ratchet: Baaz never talks to a provider's wire.
//!
//! Since P4 (docs/63) every provider, Muse included, reaches the app
//! through `provider` and its own adapter crate: `provider-muse` is the only
//! crate that names Muse's wire, and `crates/baaz` reaches neither its
//! `client` (the transport, once `muse-client`) nor its `fold` (once
//! `muse-adapter`). The checks below keep it that way: no source file
//! mentions either, by its old crate name or its module path, the manifest lists neither, and no
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

/// The app's sources and the two window-free crates it is built on
/// (docs/82 4.11), with the prefix a hit's path carries: the seam binds the
/// code that moved out of the app as much as what stayed.
fn source_roots() -> [(PathBuf, &'static str); 3] {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    [
        (source_root(), ""),
        (crates.join("baaz-core/src"), "baaz-core/"),
        (crates.join("baaz-browser-core/src"), "baaz-browser-core/"),
    ]
}

/// Every `.rs` file under [`source_roots`] whose text satisfies `hit`, as
/// prefixed paths, sorted.
fn matching_files(hit: impl Fn(&str) -> bool) -> Vec<String> {
    let mut hits = Vec::new();
    for (root, prefix) in source_roots() {
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        for path in files {
            if fs::read_to_string(&path).is_ok_and(|text| hit(&text)) {
                hits.push(format!("{prefix}{}", path.strip_prefix(&root).unwrap_or(&path).display()));
            }
        }
    }
    hits.sort();
    hits
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

/// The wire's transport, under either name: the old crate, or the
/// `provider-muse` module it became (docs/85 10.9).
fn files_mentioning_muse_client() -> Vec<String> {
    matching_files(|text| text.contains("muse_client") || text.contains("provider_muse::client"))
}

#[test]
fn no_file_names_muse_client() {
    let hits = files_mentioning_muse_client();
    assert!(
        hits.is_empty(),
        "{} files under crates/baaz/src and the core crates mention `muse_client`.\n\
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
        !hits.iter().any(|f| f == "baaz-core/conn.rs"),
        "conn.rs mentions `muse_client` again; the connection is the hub alone",
    );
}

/// Every `.rs` file under `crates/baaz/src` and the core crates that
/// contains `needle`.
fn files_mentioning(needle: &str) -> Vec<String> {
    matching_files(|text| text.contains(needle))
}

#[test]
fn no_file_names_the_muse_adapter_either() {
    let mut hits = files_mentioning("muse_adapter");
    hits.extend(files_mentioning("provider_muse::fold"));
    assert!(hits.is_empty(), "these files reach past the seam into Muse's fold: {hits:?}");
}

#[test]
fn the_manifest_lists_no_wire_crate() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for krate in ["baaz", "baaz-core", "baaz-browser-core"] {
        let manifest = fs::read_to_string(crates.join(krate).join("Cargo.toml")).expect("manifest");
        for wire in ["muse-client", "muse-adapter"] {
            let listed = manifest.lines().any(|line| {
                let line = line.trim_start();
                !line.starts_with('#') && (line.starts_with(&format!("{wire}.")) || line.starts_with(&format!("{wire} ")))
            });
            assert!(!listed, "crates/{krate} depends on `{wire}` again; reach Muse through `provider-muse`");
        }
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
const OPEN_ROW_PATHS: [&str; 13] = [
    "app/sessions/list.rs",
    "app/sessions/open.rs",
    "app/sessions/provider_open.rs",
    "app/sessions/switch.rs",
    "app/sessions/events.rs",
    "app/sessions/handoff.rs",
    "app/sessions/steps.rs",
    // The open/switch state machine, in the core crate since docs/82 4.11.
    "../../baaz-core/src/lifecycle.rs",
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

// ------------------------------------------------------------------ 5.4

/// Whether a source file is test-only by its path: a `tests.rs` module or
/// anything under a `tests/` directory (each declared
/// `#[cfg(test)] mod tests;` by its parent).
fn is_test_path(relative: &str) -> bool {
    relative.ends_with("/tests.rs") || relative == "tests.rs" || relative.starts_with("tests/") || relative.contains("/tests/")
}

/// `text` without test modules and without `//` comments: the code a
/// shipped build compiles.
fn shipped_code(text: &str) -> String {
    strip_test_modules(text).lines().map(|line| line.split("//").next().unwrap_or(line)).collect::<Vec<_>>().join("\n")
}

/// Every shipped (non-test) `.rs` file under `root`, as `(relative path,
/// shipped code)`.
fn shipped_files(root: &Path) -> Vec<(String, String)> {
    let mut files = Vec::new();
    rust_files(root, &mut files);
    let mut out: Vec<(String, String)> = files
        .into_iter()
        .filter_map(|path| {
            let relative = path.strip_prefix(root).unwrap_or(&path).display().to_string();
            if is_test_path(&relative) {
                return None;
            }
            fs::read_to_string(&path).ok().map(|text| (relative, shipped_code(&text)))
        })
        .collect();
    out.sort();
    out
}

/// How often `needle` appears in `code` as a whole path segment
/// (`ProviderId::Muse` but not `ProviderId::MuseFoo`).
fn whole_count(code: &str, needle: &str) -> usize {
    code.match_indices(needle)
        .filter(|(at, _)| {
            let end = at + needle.len();
            let before_ok = *at == 0 || !code[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            let after_ok = !code[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
            before_ok && after_ok
        })
        .count()
}

/// docs/82 5.4: the app names no provider by its variant outside tests.
/// What a provider is (label, binaries, commands, modes) is its adapter's
/// declaration and what it does differently is a declared trait, read
/// through the registry (`crate::providers`); constructing a row for "the
/// provider that lists its own sessions" asks for that trait.
#[test]
fn the_app_names_muse_only_in_its_tests() {
    let hits: Vec<String> = shipped_files(&source_root())
        .into_iter()
        .filter(|(_, code)| whole_count(code, "ProviderId::Muse") > 0)
        .map(|(file, code)| format!("{file} ({})", whole_count(&code, "ProviderId::Muse")))
        .collect();
    assert!(
        hits.is_empty(),
        "`ProviderId::Muse` in shipped crates/baaz code: ask the registry for a trait instead.\n  {}",
        hits.join("\n  "),
    );
}

/// docs/85 11.5: the window-free core names the variant nowhere either.
/// The registry itself (`providers.rs`) is where the variant is defined and
/// registered, so it is not counted; everything else reaches "the provider
/// with a shared child" or "the provider that lists its own sessions"
/// through [`ProviderId::shared_child`] / [`ProviderId::native_lister`] or
/// a declared trait.
#[test]
fn the_core_names_muse_only_in_the_registry() {
    let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("../baaz-core/src");
    let hits: Vec<String> = shipped_files(&core)
        .into_iter()
        .filter(|(file, code)| file != "providers.rs" && whole_count(code, "ProviderId::Muse") > 0)
        .map(|(file, code)| format!("{file} ({})", whole_count(&code, "ProviderId::Muse")))
        .collect();
    assert!(hits.is_empty(), "`ProviderId::Muse` in shipped baaz-core code: ask the registry for a trait instead.\n  {}", hits.join("\n  "));
}

/// The adapter crates, one per wire: Muse's (`provider-muse` over
/// `muse-client`), Claude Code's, Codex's and ACP's (docs/82 5.7: no file
/// but the registry names it).
const ADAPTER_CRATES: [&str; 4] = ["provider_muse", "provider_codex", "provider_claude_code", "provider_acp"];

/// The registry: the one file that names the adapter crates'
/// registrations (descriptors and factories).
const REGISTRY: &str = "baaz-core/providers.rs";

/// What still reaches an adapter crate past the registry, file by file,
/// each with why. The list may only shrink: a file that stops naming an
/// adapter must leave it, and a file not listed may not start.
const WIRE_EXCEPTIONS: [(&str, &str); 18] = [
    ("app.rs", "the `--bench` replay drives a Muse-format capture event by event (provider_muse::replay)"),
    ("bench.rs", "the bench drives a Muse-format capture"),
    ("perf_turn.rs", "the perf budget folds a Muse-format capture"),
    ("session.rs", "the read-only replay lane (provider_muse::replay)"),
    ("session/runtime.rs", "the capture folder and Codex's per-model effort table (5.5 declares efforts)"),
    ("session/composer.rs", "Claude Code's effort levels and model-id normalising (5.5 declares efforts)"),
    ("session/commands.rs", "Claude Code's and Codex's effort tables (5.5 declares efforts)"),
    ("session/lane.rs", "Claude Code's control-refusal card kinds (a typed notice is 5.5's)"),
    ("transcript.rs", "each adapter's child-exited card kind (a typed notice is 5.5's)"),
    ("settings_providers.rs", "Codex's device-code sign-in drives its child from the app"),
    ("app/sessions/provider_open.rs", "the transcript Trash reads each CLI home"),
    ("baaz-core/import.rs", "the import scan reads each CLI's stored transcripts"),
    ("baaz-core/provider_search.rs", "search reads each CLI's stored transcripts"),
    ("baaz-core/provider_status.rs", "the install and sign-in probes (not yet a factory method)"),
    ("baaz-core/session_migration.rs", "the legacy-home migration moves each CLI's files"),
    ("baaz-core/sessions.rs", "forget finds each CLI's transcript files"),
    ("baaz-core/tier.rs", "Muse's billing tier reads its usage shapes (moves behind provider-muse)"),
    ("baaz-core/utility.rs", "the one-shot CLI completer pins each CLI's home"),
];

/// Every shipped file under the app and the core crates that names an
/// adapter crate in code, as prefixed paths.
fn files_reaching_an_adapter() -> Vec<String> {
    let mut hits = Vec::new();
    for (root, prefix) in source_roots() {
        for (file, code) in shipped_files(&root) {
            if ADAPTER_CRATES.iter().any(|krate| whole_count(&code, krate) > 0) {
                hits.push(format!("{prefix}{file}"));
            }
        }
    }
    hits
}

/// docs/82 5.4, 5.7: the seam ratchet covers every wire. No app code
/// reaches into an adapter crate — Muse's, Claude Code's, Codex's or
/// ACP's —
/// except through the registry, or where [`WIRE_EXCEPTIONS`] says why.
#[test]
fn no_app_code_reaches_an_adapter_except_through_the_registry() {
    let hits = files_reaching_an_adapter();
    let unlisted: Vec<&String> = hits
        .iter()
        .filter(|file| file.as_str() != REGISTRY && !WIRE_EXCEPTIONS.iter().any(|(listed, _)| listed == file))
        .collect();
    assert!(
        unlisted.is_empty(),
        "these files reach an adapter crate past the registry (`crate::providers`): {unlisted:?}"
    );
    let stale: Vec<&str> =
        WIRE_EXCEPTIONS.iter().map(|(file, _)| *file).filter(|file| !hits.iter().any(|hit| hit == file)).collect();
    assert!(stale.is_empty(), "these no longer reach an adapter: take them off WIRE_EXCEPTIONS: {stale:?}");
    assert!(hits.iter().any(|file| file == REGISTRY), "the registry registers the adapters");
}

#[test]
fn the_wire_ratchet_sees_code_and_not_prose_or_tests() {
    let sample = "\
use provider_codex::fold; // provider_muse in a comment is prose
/// [`provider_claude_code::home`] in a doc is prose
fn a() { let x = provider_claude_codex; }
#[cfg(test)]
mod tests {
    fn t() { provider_muse::replay::Folder::new(); }
}
";
    let code = shipped_code(sample);
    assert_eq!(whole_count(&code, "provider_codex"), 1);
    assert_eq!(whole_count(&code, "provider_muse"), 0, "comments and test modules are not shipped");
    assert_eq!(whole_count(&code, "provider_claude_code"), 0, "a longer name is another name");
    assert!(is_test_path("app/sessions/tests/support.rs") && is_test_path("db/tests.rs"));
    assert!(!is_test_path("app/sessions/open.rs"));
}
