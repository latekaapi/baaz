//! The core crates stay windowless (docs/81 D Q4, docs/82 4.11): the
//! ratchet.
//!
//! `crates/baaz-core` (the stores, the session list's model, providers,
//! usage, projects, search, import, git, the open/switch state machine) and
//! `crates/baaz-browser-core` (tabs, history, bookmarks, ad blocking, the
//! agent page tools and their policy) hold the part of Baaz that has no
//! window. Nothing in them may name gpui or the aui component library,
//! which is built on it — directly, through a dependency, or in code:
//!
//! * `cargo tree` of each crate, normal and build edges, every feature on,
//!   lists no gpui package and no aui crate but `aui-protocol` (plain data);
//! * neither manifest names one;
//! * no code line (comments dropped) of either crate's sources names one.
//!
//! The app (`crates/baaz`) wraps the core in views; a module that needs a
//! window type stays there, split from its model. When this fails, put the
//! window half back in the app and hand the core plain data.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The two crates the ratchet holds to no window.
const CORES: [&str; 2] = ["baaz-core", "baaz-browser-core"];

/// Crates that bring a window: gpui itself, and every aui crate but
/// `aui-protocol`.
fn is_windowed(package: &str) -> bool {
    package.contains("gpui") || (package.starts_with("aui") && package != "aui-protocol")
}

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The package names in `cargo tree -p <krate>` (normal and build edges,
/// every feature), one per line, deduplicated.
fn tree(krate: &str) -> Vec<String> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(cargo)
        .current_dir(crates_dir().join(".."))
        .args(["tree", "-p", krate, "-e", "normal,build", "--all-features", "--prefix", "none", "--format", "{p}"])
        .output()
        .unwrap_or_else(|error| panic!("cargo tree -p {krate} runs: {error}"));
    assert!(
        output.status.success(),
        "cargo tree -p {krate} failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut names: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split_whitespace().next().map(str::to_owned))
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|_| panic!("{} reads", dir.display())).flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// The code lines of `text` (line comments and doc comments dropped) that
/// name a windowed crate: `gpui`, or an aui crate other than the protocol.
fn windowed_lines(text: &str) -> Vec<(usize, String)> {
    let mut hits = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let code = line.split("//").next().unwrap_or("");
        let named = code
            .split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == "gpui" || word.starts_with("gpui_") || (word.starts_with("aui") && word != "aui_protocol" && (word == "aui" || word.starts_with("aui_"))));
        if named {
            hits.push((n + 1, line.trim().to_owned()));
        }
    }
    hits
}

#[test]
fn no_core_crate_depends_on_a_window() {
    for krate in CORES {
        let names = tree(krate);
        assert!(names.iter().any(|name| name == krate), "cargo tree listed {krate} itself: {names:?}");
        let windowed: Vec<&String> = names.iter().filter(|name| is_windowed(name)).collect();
        assert!(windowed.is_empty(), "{krate} pulls in a window through its dependencies: {windowed:?}");
    }
}

#[test]
fn no_core_manifest_names_a_window() {
    for krate in CORES {
        let manifest = std::fs::read_to_string(crates_dir().join(krate).join("Cargo.toml")).expect("manifest reads");
        let named: Vec<&str> = manifest
            .lines()
            .map(str::trim_start)
            .filter(|line| !line.starts_with('#') && !line.starts_with("description"))
            .filter(|line| {
                let name = line.split(|c: char| c == '=' || c == '.' || c.is_whitespace()).next().unwrap_or("");
                is_windowed(name)
            })
            .collect();
        assert!(named.is_empty(), "{krate}/Cargo.toml names a windowed crate: {named:?}");
    }
}

#[test]
fn no_core_source_names_a_window() {
    for krate in CORES {
        let src = crates_dir().join(krate).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        assert!(files.len() > 5, "{krate}'s sources were read ({} files)", files.len());
        let mut found = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path).expect("source reads");
            for (n, line) in windowed_lines(&text) {
                found.push(format!("{krate}/{}:{n}: {line}", path.strip_prefix(&src).unwrap_or(&path).display()));
            }
        }
        assert!(found.is_empty(), "the core names a window in code:\n  {}", found.join("\n  "));
    }
}

/// The checks above can fail: a planted line and a planted package are
/// both caught, and the protocol crate is not.
#[test]
fn the_ratchet_sees_a_planted_window() {
    assert_eq!(windowed_lines("use gpui::App;").len(), 1);
    assert_eq!(windowed_lines("fn f(cx: &mut gpui::Context<Self>) {}").len(), 1);
    assert_eq!(windowed_lines("let mark = aui_icons::Provider::Muse;").len(), 1);
    assert_eq!(windowed_lines("pub use aui::nav::Grouping;").len(), 1);
    assert!(windowed_lines("use aui_protocol::Block;").is_empty(), "the protocol is plain data");
    assert!(windowed_lines("// gpui's executor runs this").is_empty(), "a comment may name the app's toolkit");
    assert!(windowed_lines("let gpuix = 1; let auid = 2;").is_empty(), "whole words only");
    assert!(is_windowed("gpui-pre") && is_windowed("gpui-kit") && is_windowed("aui-tokens") && is_windowed("aui"));
    assert!(!is_windowed("aui-protocol") && !is_windowed("provider"));
}
