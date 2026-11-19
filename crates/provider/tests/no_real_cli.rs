//! The ratchet behind "tests never reach a real CLI" (CLAUDE.md standing
//! rule 4; docs/82 5.2, docs/81 B "tests" item 3): no test that `cargo test`
//! runs may name a real `codex`, `claude` or `muse` as the program it
//! starts, or resolve the installed one.
//!
//! Why a source scan and not a `PATH` without those binaries: the adapters
//! do not trust `PATH`. They resolve their program through the login shell
//! (`provider::env_path`) and well-known install directories, so a test run
//! under a scrubbed `PATH` would still find `~/.local/bin/claude` — the guard
//! would pass while the leak stayed open. What a test spawns is decided by
//! the program it hands the adapter or `Command`, and that is in the source:
//! a bare CLI name (resolved through `PATH` and the login shell), an
//! install directory, or a resolver of the installed binary. A stand-in is
//! always a path the test built itself (a temp dir, a fixture script), never
//! a literal naming the real one.
//!
//! Scanned: every file under `crates/*/tests/`, and every `#[cfg(test)]`
//! module in `crates/*/src/`. A test marked `#[ignore]` is exempt — `cargo
//! test` never runs it — and so is a test file whose every test is: the live
//! matrices (`live_modes.rs`, gated on `BAAZ_LIVE_MODES=1` too) and the
//! Codex schema drift gate's live half run real CLIs on purpose, by hand.
//! A path a test only computes with (install-path display, `PATH` math) is
//! data, not a program, and is not flagged.

use std::path::{Path, PathBuf};

/// The real CLIs a test must never start.
const CLIS: [&str; 3] = ["codex", "claude", "muse"];

/// Where the real CLIs are installed: a program literal under one of these
/// names the real binary.
const INSTALL_DIRS: [&str; 5] = ["/opt/homebrew/bin/", "/usr/local/bin/", "/.local/bin/", "/.npm-global/", "/.bun/bin/"];

/// Calls that start (or resolve, then start) the program their first
/// argument names.
const SPAWNERS: [&str; 11] = [
    "Command::new(",
    "ClaudeCodeAdapter::new(",
    "CodexAdapter::new(",
    "AcpAdapter::new(",
    "MuseHub::connect(",
    "MuseClient::spawn(",
    "MuseClient::connect(",
    "spawn_command(",
    "probe_app_server(",
    "find_program(",
    "find_program_uncached(",
];

/// Resolvers of the installed binary from the real environment.
const RESOLVERS: [&str; 1] = ["resolve_muse_program("];

fn workspace() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// `text` with every `//` comment blanked (string literals kept), so prose
/// that names a CLI never trips the scan.
fn without_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let bytes = text.as_bytes();
    let mut i = 0;
    let mut in_string = false;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            out.push(c as char);
            if c == b'\\' && i + 1 < bytes.len() {
                out.push(bytes[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_string = true;
            out.push('"');
            i += 1;
            continue;
        }
        if c == b'/' && bytes.get(i + 1) == Some(&b'/') {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

/// The end (exclusive) of the brace block opening at or after `from`.
fn block_end(text: &str, from: usize) -> Option<usize> {
    let open = from + text[from..].find('{')?;
    let mut depth = 0usize;
    for (offset, c) in text[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// `text` less every `#[ignore …]` test's body: `cargo test` never runs it.
fn without_ignored_tests(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find("#[ignore") {
        out.push_str(&rest[..at]);
        let after = &rest[at..];
        match after.find("fn ").and_then(|fn_at| block_end(after, fn_at)) {
            Some(end) => rest = &after[end..],
            None => {
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// The `#[cfg(test)]` modules of a source file.
fn test_modules(text: &str) -> String {
    let mut out = String::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("#[cfg(test)]") {
        let start = from + at;
        let after = &text[start..];
        let gated_mod = after["#[cfg(test)]".len()..].trim_start().starts_with("mod ");
        match block_end(text, start).filter(|_| gated_mod) {
            Some(end) => {
                out.push_str(&text[start..end]);
                out.push('\n');
                from = end;
            }
            None => from = start + "#[cfg(test)]".len(),
        }
    }
    out
}

/// Whether a string literal names a real CLI: the bare name (resolved
/// through `PATH` and the login shell) or a path in an install directory.
fn names_a_real_cli(literal: &str) -> bool {
    let base = literal.rsplit('/').next().unwrap_or(literal);
    if !CLIS.contains(&base) {
        return false;
    }
    literal == base || INSTALL_DIRS.iter().any(|dir| literal.contains(dir)) || literal.starts_with('~')
}

/// What `code` (test code, comments stripped) does that reaches a real CLI,
/// as `(line, what)`.
fn reaches_a_real_cli(code: &str) -> Vec<(usize, String)> {
    let line_of = |at: usize| code[..at].matches('\n').count() + 1;
    let mut found = Vec::new();
    for spawner in SPAWNERS {
        let mut from = 0;
        while let Some(at) = code[from..].find(spawner) {
            let start = from + at;
            from = start + spawner.len();
            let args = code[from..].trim_start();
            let Some(literal) = args.strip_prefix('"').and_then(|rest| rest.split('"').next()) else { continue };
            if names_a_real_cli(literal) {
                found.push((line_of(start), format!("{spawner}\"{literal}\"…)")));
            }
        }
    }
    for resolver in RESOLVERS {
        let mut from = 0;
        while let Some(at) = code[from..].find(resolver) {
            let start = from + at;
            from = start + resolver.len();
            found.push((line_of(start), resolver.trim_end_matches('(').to_owned()));
        }
    }
    found
}

/// A test file whose every test is `#[ignore]`d: `cargo test` runs none of
/// its code, helpers included (the live matrices).
fn only_ignored_tests(text: &str) -> bool {
    let tests = text.matches("#[test]").count();
    tests > 0 && text.matches("#[ignore").count() >= tests
}

/// Every place the workspace's test code reaches a real CLI.
fn offences() -> Vec<String> {
    let crates = workspace().join("crates");
    let mut offences = Vec::new();
    let Ok(members) = std::fs::read_dir(&crates) else { panic!("{} reads", crates.display()) };
    for member in members.flatten() {
        let root = member.path();
        let mut tests = Vec::new();
        rust_files(&root.join("tests"), &mut tests);
        let mut sources = Vec::new();
        rust_files(&root.join("src"), &mut sources);
        let scanned = tests.into_iter().map(|path| (path, true)).chain(sources.into_iter().map(|path| (path, false)));
        for (path, whole) in scanned {
            // This file names the CLIs as what it looks for.
            if path.ends_with("provider/tests/no_real_cli.rs") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            let text = without_comments(&text);
            if whole && only_ignored_tests(&text) {
                continue;
            }
            let code = if whole { text } else { test_modules(&text) };
            let code = without_ignored_tests(&code);
            for (line, what) in reaches_a_real_cli(&code) {
                let shown = path.strip_prefix(workspace()).unwrap_or(&path).display().to_string();
                offences.push(format!("{shown} (test code line {line}): {what}"));
            }
        }
    }
    offences.sort();
    offences
}

#[test]
fn no_test_cargo_runs_reaches_a_real_cli() {
    let offences = offences();
    assert!(
        offences.is_empty(),
        "test code names a real CLI as the program it starts (use a stand-in under a temp dir or a fixture \
         script, or mark a live test #[ignore] and gate it on BAAZ_LIVE_MODES=1):\n  {}",
        offences.join("\n  ")
    );
}

/// The scan bites: planted test code that starts each real CLI is caught,
/// and a stand-in, a comment, an ignored live test and a provider name used
/// as data are not.
#[test]
fn the_scan_catches_planted_spawns_and_spares_stand_ins() {
    let planted = r#"
        fn t() {
            let a = std::process::Command::new("codex").arg("--version").output();
            let b = ClaudeCodeAdapter::new("claude");
            let c = MuseHub::connect("/opt/homebrew/bin/muse", &info());
            let d = provider::env_path::find_program("claude");
            let e = muse_client::program::resolve_muse_program();
        }
    "#;
    let found = reaches_a_real_cli(&without_ignored_tests(&without_comments(planted)));
    let whats: Vec<&str> = found.iter().map(|(_, what)| what.as_str()).collect();
    for wanted in ["Command::new(\"codex\"", "ClaudeCodeAdapter::new(\"claude\"", "MuseHub::connect(\"/opt/homebrew/bin/muse\"", "find_program(\"claude\"", "resolve_muse_program"] {
        assert!(whats.iter().any(|what| what.starts_with(wanted)), "missed {wanted}: {whats:?}");
    }
    let spared = r#"
        // Command::new("codex") would spend money: never.
        fn t(dir: &Path) {
            let stand_in = dir.join("codex");
            let a = CodexAdapter::new(&stand_in.to_string_lossy());
            let b = ClaudeCodeAdapter::new("/nonexistent/baaz-test/claude");
            assert_eq!(provider_key, "codex");
            let c = ProviderId::parse("muse");
        }
        #[test]
        #[ignore = "spends real turns"]
        fn live() {
            let program = std::process::Command::new("claude");
        }
    "#;
    let found = reaches_a_real_cli(&without_ignored_tests(&without_comments(spared)));
    assert!(found.is_empty(), "a stand-in, a comment or an ignored live test was flagged: {found:?}");
    let module = "fn shipped() { Command::new(\"codex\"); }\n#[cfg(test)]\nmod tests {\n    fn t() { Command::new(\"claude\"); }\n}\n";
    let found = reaches_a_real_cli(&test_modules(module));
    assert_eq!(found.len(), 1, "only the test module is scanned in src: {found:?}");
    let live = "fn helper() { resolve_muse_program(); }\n#[test]\n#[ignore = \"live\"]\nfn live() { helper(); }\n";
    assert!(only_ignored_tests(live), "a file of ignored live tests is exempt, helpers and all");
    let mixed = "fn helper() { resolve_muse_program(); }\n#[test]\nfn runs() {}\n#[test]\n#[ignore]\nfn live() {}\n";
    assert!(!only_ignored_tests(mixed), "one test cargo runs scans the file");
}
