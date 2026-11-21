//! The JSON stores `baaz.db` replaced have one reader: its first-launch
//! importer (docs/82 4.12): the ratchet.
//!
//! `session-meta.json`, `session-cache.json`, `sessions.json`,
//! `provider-sessions.json`, `history.json`, `import-journal.json`,
//! `session-migration-journal.json` and the `sent/<session>.json` indexes
//! are read once, by `crates/baaz-core/src/db/json_import.rs`, and kept on
//! disk untouched as the backup. Nothing else may name them in code (the
//! importer's own tests write the fixtures it reads); the sent folder's
//! module reads no file; and the app names no SQLite outside the browser's
//! worker side and the probe's index bench, so `baaz.db` is reached only
//! through its writer thread.
//!
//! When this fails, read and write the session's row through `crate::db`
//! instead of the old file.
//!
//! The adapters keep no session store of their own either (docs/82 5.8):
//! `acp-workspaces.json` (5.7's session id → folder file) is retired with no
//! importer — the folder rides `Command::ResumeSession` from the session's
//! row — and every JSON file an adapter's code names is listed in
//! [`ADAPTER_JSON`] with why it is not a store Baaz keeps by session.

use std::path::{Path, PathBuf};

/// The retired stores, by file name.
const RETIRED: [&str; 7] = [
    "session-meta.json",
    "session-cache.json",
    "sessions.json",
    "provider-sessions.json",
    "history.json",
    "import-journal.json",
    "session-migration-journal.json",
];

/// Stores retired with nothing to import: named nowhere at all.
const RETIRED_UNIMPORTED: [&str; 1] = ["acp-workspaces.json"];

/// The adapter crates, by directory under `crates/`.
const ADAPTER_CRATES: [&str; 6] =
    ["provider", "provider-runtime", "provider-muse", "provider-claude-code", "provider-codex", "provider-acp"];

/// Every JSON file an adapter's non-test code names: `(file, the name as
/// written, why it is not a store Baaz keeps by session)`. A new one is a
/// new store until it is listed here with its reason — and one keyed by
/// session id belongs in `baaz.db` instead.
const ADAPTER_JSON: [(&str, &str, &str); 11] = [
    (
        "provider-claude-code/src/terminal.rs",
        "{session_id}.json",
        "the CLI's own --mcp-config input, rewritten at every spawn and never read back by Baaz",
    ),
    ("provider-claude-code/src/home.rs", "settings.json", "the CLI's own settings, linked from the owner's ~/.claude"),
    (
        "provider-claude-code/src/terminal.rs",
        ".claude.json",
        "the CLI's own config, read (never written) for Settings › MCP's list (docs/82 8.8)",
    ),
    (
        "provider-claude-code/src/terminal.rs",
        "settings.json",
        "the CLI's own settings (linked from ~/.claude), read for Settings › MCP's plugin list",
    ),
    ("provider-claude-code/src/terminal.rs", "mcp-claude.json", "a recorded fixture the MCP list reads in tests and captures"),
    ("provider-claude-code/src/terminal.rs", "mcp-settings.json", "a recorded fixture the MCP list reads in tests and captures"),
    (
        "provider-claude-code/src/terminal.rs",
        "{session_id}-own.json",
        "the CLI's own --mcp-config input carrying the person's servers (docs/82 8.9), rewritten at every opted-in spawn and never read back",
    ),
    ("provider-claude-code/src/lib.rs", "-own.json", "recognises that --mcp-config file in a launch's argv, so a relaunch carries it once"),
    (
        "provider-claude-code/src/registry.rs",
        ".claude.json",
        "the person's ~/.claude.json, read (never written) for their own MCP servers (docs/82 8.9)",
    ),
    ("provider-codex/src/home.rs", "auth.json", "the CLI's own credentials, linked from the owner's ~/.codex"),
    ("provider-runtime/src/ledger.rs", "children.json", "the live-children ledger (docs/82 5.1), keyed by pid for crash cleanup"),
];

/// The importer and its tests, by path under `crates/`.
const IMPORTER: [&str; 2] = ["baaz-core/src/db/json_import.rs", "baaz-core/src/db/tests.rs"];

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "target") {
                continue;
            }
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `text`'s code lines, comments dropped: `(line number, code)`.
fn code(text: &str) -> Vec<(usize, String)> {
    text.lines()
        .enumerate()
        .filter_map(|(ix, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                return None;
            }
            let code = match line.find(" // ") {
                Some(at) => &line[..at],
                None => line,
            };
            Some((ix + 1, code.to_owned()))
        })
        .collect()
}

/// Every code line of every crate's sources and tests, as
/// `(path under crates/, line number, code)`.
fn workspace_code() -> Vec<(String, usize, String)> {
    let root = crates_dir();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    let mut out = Vec::new();
    for path in files {
        let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in code(&text) {
            out.push((rel.clone(), n, line));
        }
    }
    out
}

/// Whether `code` names the file `name` itself: not as the tail of a longer
/// name (`stored-history.jsonl`, `provider-sessions.json` for
/// `sessions.json`).
fn names(code: &str, name: &str) -> bool {
    let part = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    code.match_indices(name).any(|(at, _)| {
        let before = code[..at].chars().next_back();
        let after = code[at + name.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

/// The lines that name a retired store outside the importer and this file.
fn retired_hits(lines: &[(String, usize, String)]) -> Vec<String> {
    lines
        .iter()
        .filter(|(file, ..)| !IMPORTER.contains(&file.as_str()) && !file.ends_with("baaz/tests/db_ratchet.rs"))
        .filter(|(_, _, code)| RETIRED.iter().any(|name| names(code, name)))
        .map(|(file, n, code)| format!("{file}:{n}: {}", code.trim()))
        .collect()
}

#[test]
fn no_code_but_the_importer_names_a_retired_store() {
    let lines = workspace_code();
    assert!(lines.len() > 100_000, "the workspace was read ({} lines)", lines.len());
    let found = retired_hits(&lines);
    assert!(
        found.is_empty(),
        "a retired JSON store named outside the importer ({}):\n  {}",
        IMPORTER[0],
        found.join("\n  ")
    );
    // The importer itself names every one.
    let importer: Vec<_> = lines.iter().filter(|(file, ..)| file == IMPORTER[0]).collect();
    for name in RETIRED {
        assert!(importer.iter().any(|(_, _, code)| names(code, name)), "the importer reads {name}");
    }
}

/// The sent folder keeps only pictures now: its module reads no file (the
/// per-session indexes are the importer's to read, and only the forget path
/// names one, to remove it).
#[test]
fn the_sent_folders_module_reads_no_file() {
    let text = std::fs::read_to_string(crates_dir().join("baaz-core/src/sent.rs")).expect("sent.rs");
    let readers: Vec<_> = code(&text)
        .into_iter()
        .filter(|(_, line)| line.contains("fs::read") || line.contains("File::open") || line.contains("read_json"))
        .collect();
    assert!(readers.is_empty(), "sent.rs reads a file: {readers:?}");
}

/// `baaz.db` is reached through its writer only: the app names no SQLite
/// outside the browser's worker side (its own ratchet) and the probe's
/// search-index bench.
#[test]
fn the_app_names_no_sqlite_outside_the_browser_worker() {
    let allowed = |file: &str| {
        file.starts_with("baaz/src/browser/") || file == "baaz/src/perf_budget.rs" || file.starts_with("baaz/src/app/sessions/tests/")
    };
    let found: Vec<String> = workspace_code()
        .into_iter()
        .filter(|(file, ..)| file.starts_with("baaz/src/") && !allowed(file))
        .filter(|(_, _, code)| code.contains("rusqlite") || code.contains("Connection::open"))
        .map(|(file, n, code)| format!("{file}:{n}: {}", code.trim()))
        .collect();
    assert!(found.is_empty(), "SQLite named in the app:\n  {}", found.join("\n  "));
}

/// The scanner sees a planted reader, and only outside the importer.
#[test]
fn a_planted_reader_is_caught() {
    let planted = r#"    let text = std::fs::read_to_string(dir.join("provider-sessions.json"));
    // a comment naming history.json is not code
"#;
    let lines: Vec<(String, usize, String)> =
        code(planted).into_iter().map(|(n, line)| ("baaz/src/planted.rs".to_owned(), n, line)).collect();
    assert_eq!(retired_hits(&lines).len(), 1, "the planted read is caught, the comment is not");
    assert!(!names("fixture(\"stored-history.jsonl\")", "history.json"), "a longer name is not the store");
    let importer: Vec<(String, usize, String)> =
        lines.iter().map(|(_, n, line)| (IMPORTER[0].to_owned(), *n, line.clone())).collect();
    assert!(retired_hits(&importer).is_empty(), "the importer may read it");
}

/// `text` without its trailing test module: from a `#[cfg(test)]` that
/// opens a `mod` to the end (the adapters keep their tests last).
fn without_tests(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let cut = lines
        .windows(2)
        .position(|pair| pair[0].trim() == "#[cfg(test)]" && pair[1].trim_start().starts_with("mod "))
        .unwrap_or(lines.len());
    lines[..cut].join("\n")
}

/// The `"….json"` string literals in `code`.
fn json_literals(code: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = code;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        let literal = &after[..end];
        if literal.ends_with(".json") {
            out.push(literal.to_owned());
        }
        rest = &after[end + 1..];
    }
    out
}

/// The JSON names in adapter code `(file, line, code)` not in [`ADAPTER_JSON`].
fn unlisted_adapter_json(lines: &[(String, usize, String)]) -> Vec<String> {
    lines
        .iter()
        .flat_map(|(file, n, code)| {
            json_literals(code).into_iter().filter_map(move |literal| {
                let listed = ADAPTER_JSON.iter().any(|(at, name, _)| at == file && literal.ends_with(name));
                (!listed).then(|| format!("{file}:{n}: \"{literal}\" in {}", code.trim()))
            })
        })
        .collect()
}

/// Every non-test code line of the adapter crates' `src/`.
fn adapter_code() -> Vec<(String, usize, String)> {
    let root = crates_dir();
    let mut out = Vec::new();
    for krate in ADAPTER_CRATES {
        let mut files = Vec::new();
        rust_files(&root.join(krate).join("src"), &mut files);
        for path in files {
            let rel = path.strip_prefix(&root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            for (n, line) in code(&without_tests(&text)) {
                out.push((rel.clone(), n, line));
            }
        }
    }
    out
}

/// docs/82 5.8: no adapter keeps a JSON store of its own (5.7's
/// `acp-workspaces.json` kept session id → folder outside `baaz.db`, and a
/// forgotten session's entry stayed behind). The retired name appears
/// nowhere, and every JSON file an adapter names is a listed non-store.
#[test]
fn no_adapter_keeps_a_json_store_of_its_own() {
    let everywhere = workspace_code();
    let named: Vec<String> = everywhere
        .iter()
        .filter(|(file, ..)| !file.ends_with("baaz/tests/db_ratchet.rs"))
        .filter(|(_, _, code)| RETIRED_UNIMPORTED.iter().any(|name| names(code, name)))
        .map(|(file, n, code)| format!("{file}:{n}: {}", code.trim()))
        .collect();
    assert!(named.is_empty(), "a retired adapter store is named:\n  {}", named.join("\n  "));
    let lines = adapter_code();
    assert!(lines.len() > 10_000, "the adapters were read ({} lines)", lines.len());
    let found = unlisted_adapter_json(&lines);
    assert!(
        found.is_empty(),
        "an adapter names a JSON file not in ADAPTER_JSON — a store keyed by session id belongs in baaz.db:\n  {}",
        found.join("\n  ")
    );
    // Every listed entry is still there (a stale allowance is a hole).
    for (file, name, _) in ADAPTER_JSON {
        assert!(
            lines.iter().any(|(at, _, code)| at == file && json_literals(code).iter().any(|literal| literal.ends_with(name))),
            "{file} no longer names {name}: drop it from ADAPTER_JSON"
        );
    }
}

/// The adapter scan sees a planted session store, and not its test module.
#[test]
fn a_planted_adapter_store_is_caught() {
    let planted = r#"fn remember(dir: &Path) {
    let _ = std::fs::write(dir.join("acp-sessions.json"), "{}");
}

#[cfg(test)]
mod tests {
    fn fixture() { let _ = std::fs::read_to_string("seed.json"); }
}
"#;
    let lines: Vec<(String, usize, String)> =
        code(&without_tests(planted)).into_iter().map(|(n, line)| ("provider-acp/src/lib.rs".to_owned(), n, line)).collect();
    let found = unlisted_adapter_json(&lines);
    assert_eq!(found.len(), 1, "the planted store is caught, the test fixture is not: {found:?}");
    assert!(found[0].contains("acp-sessions.json"));
    let listed: Vec<(String, usize, String)> =
        vec![("provider-runtime/src/ledger.rs".to_owned(), 1, r#"pub const FILE_NAME: &str = "children.json";"#.to_owned())];
    assert!(unlisted_adapter_json(&listed).is_empty(), "a listed file passes");
}
