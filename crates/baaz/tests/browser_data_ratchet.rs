//! `browser.db` stays off the UI thread (task 4.5, audit F13): the ratchet.
//!
//! History, page words and bookmarks live in one SQLite file that only the
//! `BrowserData` worker touches (`src/browser/data.rs`, on the background
//! executor); the UI reads its in-memory front. Before task 4.5 the file was
//! opened, written and FTS-searched synchronously inside `render`. The checks
//! below keep SQLite out of every other file:
//!
//! - inside `src/browser/` (and `crates/baaz-browser-core`, where its
//!   window-free files moved in docs/82 4.11), only the worker side names
//!   SQLite: `data.rs`,
//!   `history.rs` (`BrowserDb`), `bookmarks.rs` (its tables) and
//!   `chrome_import.rs` (a read-only copy of Chrome's own History, and the
//!   import the worker runs);
//! - the functions that run SQL against `browser.db` are called from
//!   `data.rs` alone (and their own file's tests);
//! - nothing outside `src/browser/` names the store or those functions.
//!
//! At runtime every `BrowserDb` call asserts it runs inside a worker job
//! whenever the UI thread is known (`browser::data::check_off_ui`, pinned by
//! `sqlite_on_the_ui_thread_outside_the_worker_panics`). When this test
//! fails, route the new read through the front or the new write through an
//! `Op` instead.

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

/// Every code line (comments dropped) of every file under `src/`, as
/// `(path relative to src, line number, text)`. The browser's window-free
/// half (`crates/baaz-browser-core`, docs/82 4.11) reads as the
/// `browser/` files it was, and the core crate's files as `baaz-core/…`,
/// so the rules below bind the moved code as before.
fn code_lines() -> Vec<(String, usize, String)> {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut out = code_lines_under(&source_root(), "");
    out.extend(code_lines_under(&crates.join("baaz-browser-core/src"), "browser/"));
    out.extend(code_lines_under(&crates.join("baaz-core/src"), "baaz-core/"));
    out
}

/// [`code_lines`] for the files under `root`, their paths prefixed.
fn code_lines_under(root: &Path, prefix: &str) -> Vec<(String, usize, String)> {
    let mut files = Vec::new();
    rust_files(root, &mut files);
    let mut out = Vec::new();
    for path in files {
        let rel = format!("{prefix}{}", path.strip_prefix(root).unwrap_or(&path).display());
        if rel == "browser/lib.rs" {
            continue;
        }
        let text = fs::read_to_string(&path).unwrap_or_default();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if !code.trim().is_empty() {
                out.push((rel.clone(), n + 1, code.to_owned()));
            }
        }
    }
    out
}

/// The worker side: the only files under `src/browser/` that may name SQLite.
const WORKER_SIDE: [&str; 4] = ["browser/data.rs", "browser/history.rs", "browser/bookmarks.rs", "browser/chrome_import.rs"];

const SQLITE: [&str; 10] = [
    "rusqlite",
    "Connection",
    ".execute(",
    ".execute_batch(",
    ".prepare(",
    ".prepare_cached(",
    ".query_row(",
    ".query_map(",
    "BrowserDb",
    ".conn()",
];

/// The functions that run SQL against `browser.db`.
const SQL_FNS: [&str; 10] = [
    "bookmarks::add(",
    "bookmarks::add_from(",
    "bookmarks::add_folder(",
    "bookmarks::find(",
    "bookmarks::update(",
    "bookmarks::remove(",
    "bookmarks::remove_source(",
    "bookmarks::load_front(",
    "chrome_import::import(",
    "chrome_import::undo(",
];

fn hits(lines: &[(String, usize, String)], tokens: &[&str], allowed: impl Fn(&str) -> bool) -> Vec<String> {
    lines
        .iter()
        .filter(|(file, ..)| !allowed(file))
        .filter(|(_, _, code)| tokens.iter().any(|token| code.contains(token)))
        .map(|(file, n, code)| format!("{file}:{n}: {}", code.trim()))
        .collect()
}

#[test]
fn only_the_worker_side_of_the_browser_names_sqlite() {
    let lines = code_lines();
    let browser: Vec<_> = lines.into_iter().filter(|(file, ..)| file.starts_with("browser/")).collect();
    assert!(browser.len() > 1000, "the browser module was read ({} lines)", browser.len());
    let found = hits(&browser, &SQLITE, |file| WORKER_SIDE.contains(&file));
    assert!(
        found.is_empty(),
        "SQLite outside the BrowserData worker side ({}):\n  {}\nRead the front (`BrowserData`) or queue an `Op` instead.",
        WORKER_SIDE.join(", "),
        found.join("\n  ")
    );
}

#[test]
fn only_the_worker_runs_the_browser_db_functions() {
    let lines = code_lines();
    // Each file may use its own functions (its tests); everyone else goes
    // through `data.rs`.
    let found: Vec<String> = hits(&lines, &SQL_FNS, |file| file == "browser/data.rs")
        .into_iter()
        .filter(|hit| {
            let own = |module: &str| hit.starts_with(&format!("browser/{module}.rs:"));
            !(own("bookmarks") || (own("chrome_import") && hit.contains("bookmarks::")))
        })
        .collect();
    assert!(found.is_empty(), "browser.db's SQL called outside the worker:\n  {}", found.join("\n  "));
}

#[test]
fn nothing_outside_the_browser_names_the_store() {
    let lines = code_lines();
    let outside: Vec<_> = lines.into_iter().filter(|(file, ..)| !file.starts_with("browser/")).collect();
    let found = hits(&outside, &["BrowserDb", "browser::history::BrowserDb", "browser.db()"], |_| false);
    assert!(found.is_empty(), "the store named outside `src/browser/`:\n  {}", found.join("\n  "));
}

#[test]
fn the_ratchet_sees_a_planted_call() {
    let planted = vec![
        ("browser/omnibox.rs".to_owned(), 1, "let n = db.conn().query_row(\"SELECT 1\", [], |r| r.get(0));".to_owned()),
        ("browser/data.rs".to_owned(), 2, "let n = db.conn().query_row(\"SELECT 1\", [], |r| r.get(0));".to_owned()),
    ];
    let found = hits(&planted, &SQLITE, |file| WORKER_SIDE.contains(&file));
    assert_eq!(found.len(), 1, "the omnibox line is caught, the worker's is not: {found:?}");
}
