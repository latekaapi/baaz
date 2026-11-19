//! Copy ratchet (docs/82 5.6, standing rule 14): no provider name in app
//! copy except on that provider's own row — and there it is read from the
//! provider's descriptor (`ProviderId::label`, `account_label`, …), never
//! typed as a literal.
//!
//! The scan is deliberately simple: every string literal in the shipped
//! code of `crates/baaz/src` and `crates/baaz-core/src` (comments, test
//! files and `#[cfg(test)]` items removed), checked for a provider's name
//! as a whole word. A literal that is an argument of a log or panic call
//! (`baaz_log!`, `eprintln!`, `expect`, …) is not copy and is skipped.
//! Whatever else names a provider must be on [`ALLOW`], with its file and
//! why it is not app copy: a wire value the app parses, probe fixture data
//! standing in for a provider's own wire, a schema seed.
//!
//! **When this test fails on a new literal, name the provider from the
//! registry** (`crate::providers::ProviderId::label`, `shared_label()`) or
//! make the words generic (`crate::providers::THE_AGENT`, "this session's
//! agent"). Adding to `ALLOW` is for a literal that is not copy at all.
//! `ALLOW` may only shrink: an entry that matches nothing must go, and
//! [`CEILING`] is its most entries.
//!
//! At the time of writing (5.6): 62 literals named a provider in copy
//! positions before the sweep (54 of them copy, 8 the entries below), 8
//! after, all on `ALLOW`.

use std::fs;
use std::path::{Path, PathBuf};

/// The names rule 14 keeps out of shared copy. "Claude" covers "Claude
/// Code"; case-sensitive, so wire keys (`claude-code`, `codex`) and env
/// names (`ANTHROPIC_API_KEY`) are not words of copy.
const NAMES: [&str; 5] = ["Muse", "Claude", "Codex", "OpenAI", "Anthropic"];

/// Calls whose literal arguments are logs, panics or assertions — never
/// drawn on screen.
const NOT_COPY_CALLS: [&str; 12] = [
    "baaz_log!",
    "eprintln!",
    "println!",
    "eprint!",
    "print!",
    "panic!",
    "unreachable!",
    "assert!",
    "assert_eq!",
    "debug_assert!",
    "expect",
    "boot_mark",
];

/// Literals that name a provider and are not app copy, as `(file,
/// substring of the literal, why)`. Files are prefixed by crate. May only
/// shrink.
const ALLOW: [(&str, &str, &str); 8] = [
    ("baaz/account_usage.rs", "Muse Code ", "strips the provider's name off the plan its wire reports, so the row never shows it"),
    ("baaz/account_usage.rs", "Claude ", "strips the provider's name off the plan its auth probe reports (\"Claude Max\" -> \"Max\")"),
    ("baaz-core/tier.rs", "Muse Code ", "strips the provider's name off the plan its wire reports, for the footer"),
    ("baaz/main.rs", "Muse Code High Usage", "probe-only `--tier subscription`: stands in for the plan string Muse's wire sends (stripped before display)"),
    ("baaz/session/scripting.rs", "Muse Wonder 1.0", "probe-only model catalog: the provider's own model names, as its wire would list them"),
    ("baaz/session/scripting.rs", "Muse Spark 1.3", "probe-only model catalog: the provider's own model names, as its wire would list them"),
    ("baaz/session/scripting.rs", "Muse Spark 1.2", "probe-only model catalog: the provider's own model names, as its wire would list them"),
    ("baaz-core/db/schema.rs", "('muse', 'Muse')", "the V1 schema's seed rows for the stored provider table, written once by the migration"),
];

/// The most entries [`ALLOW`] may hold. Lower it with the list.
const CEILING: usize = 8;

fn source_roots() -> [(PathBuf, &'static str); 2] {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    [(manifest.join("src"), "baaz/"), (manifest.join("../baaz-core/src"), "baaz-core/")]
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

/// Test-only by path: a `tests.rs` module or anything under `tests/`
/// (each declared `#[cfg(test)] mod tests;` by its parent).
fn is_test_path(relative: &str) -> bool {
    relative == "tests.rs" || relative.ends_with("/tests.rs") || relative.starts_with("tests/") || relative.contains("/tests/")
}

/// One lexed token of interest.
#[derive(Debug)]
enum Piece {
    /// A string literal's contents and the byte offset it starts at.
    Literal { start: usize, text: String },
}

/// The byte just past the literal opening at `at` (`"…"`, `r#"…"#`,
/// `b"…"`), and its contents — `None` contents for a byte string, which
/// is never copy. `None` when `at` opens no literal.
fn literal_at(bytes: &[u8], at: usize) -> Option<(usize, Option<String>)> {
    let byte_string = bytes[at] == b'b';
    let (end, text) = text_literal_at(bytes, at)?;
    Some((end, (!byte_string).then_some(text)))
}

fn text_literal_at(bytes: &[u8], at: usize) -> Option<(usize, String)> {
    let ident_before = at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_');
    let mut i = at;
    if bytes[i] == b'b' && !ident_before && matches!(bytes.get(i + 1), Some(b'"') | Some(b'r')) {
        i += 1;
    } else if bytes[i] != b'"' && bytes[i] != b'r' {
        return None;
    }
    if bytes[i] == b'r' {
        if ident_before && i == at {
            return None;
        }
        let mut j = i + 1;
        let mut hashes = 0;
        while bytes.get(j) == Some(&b'#') {
            hashes += 1;
            j += 1;
        }
        if bytes.get(j) != Some(&b'"') {
            return None;
        }
        let body = j + 1;
        let mut k = body;
        while k < bytes.len() {
            if bytes[k] == b'"' && (0..hashes).all(|h| bytes.get(k + 1 + h) == Some(&b'#')) {
                return Some((k + 1 + hashes, String::from_utf8_lossy(&bytes[body..k]).into_owned()));
            }
            k += 1;
        }
        return Some((bytes.len(), String::from_utf8_lossy(&bytes[body..]).into_owned()));
    }
    if bytes[i] != b'"' {
        return None;
    }
    let body = i + 1;
    let mut k = body;
    while k < bytes.len() && bytes[k] != b'"' {
        if bytes[k] == b'\\' {
            k += 1;
        }
        k += 1;
    }
    let end = k.min(bytes.len());
    Some(((k + 1).min(bytes.len()), String::from_utf8_lossy(&bytes[body..end]).into_owned()))
}

/// The end of the comment at `at`, if one opens there.
fn comment_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'/' {
        return None;
    }
    match bytes.get(at + 1) {
        Some(b'/') => Some(bytes[at..].iter().position(|&b| b == b'\n').map_or(bytes.len(), |n| at + n)),
        Some(b'*') => {
            let mut depth = 1;
            let mut i = at + 2;
            while i + 1 < bytes.len() && depth > 0 {
                if bytes[i] == b'/' && bytes[i + 1] == b'*' {
                    depth += 1;
                    i += 2;
                } else if bytes[i] == b'*' && bytes[i + 1] == b'/' {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
            Some(i.min(bytes.len()))
        }
        _ => None,
    }
}

/// The end of the char literal at `at` (`'x'`, `'\n'`, `'"'`), if one
/// opens there; a lifetime (`'a`) is not one.
fn char_end(bytes: &[u8], at: usize) -> Option<usize> {
    if bytes[at] != b'\'' {
        return None;
    }
    if bytes.get(at + 1) == Some(&b'\\') {
        let close = bytes[at + 2..].iter().position(|&b| b == b'\'')?;
        return Some(at + 2 + close + 1);
    }
    if bytes.get(at + 2) == Some(&b'\'') {
        return Some(at + 3);
    }
    // One multi-byte char: its closing quote within 4 bytes. An ASCII
    // letter after the quote with no close is a lifetime (`'a`).
    if bytes.get(at + 1).is_some_and(|&b| b >= 0x80) {
        return (3..=5).find(|&n| bytes.get(at + n) == Some(&b'\'')).map(|n| at + n + 1);
    }
    None
}

/// `code` with every literal's and comment's bytes replaced by spaces
/// (newlines kept), plus the literals: what the structure scans read.
fn lex(code: &str) -> (Vec<u8>, Vec<Piece>) {
    let bytes = code.as_bytes();
    let mut blank = bytes.to_vec();
    let mut pieces = Vec::new();
    let mut i = 0;
    let wipe = |blank: &mut Vec<u8>, from: usize, to: usize| {
        for b in &mut blank[from..to] {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    while i < bytes.len() {
        if let Some(end) = comment_end(bytes, i) {
            wipe(&mut blank, i, end);
            i = end;
        } else if let Some((end, text)) = literal_at(bytes, i) {
            if let Some(text) = text {
                pieces.push(Piece::Literal { start: i, text });
            }
            // Keep the quote so the call scan still sees an argument.
            wipe(&mut blank, i + 1, end);
            i = end;
        } else if let Some(end) = char_end(bytes, i) {
            wipe(&mut blank, i, end);
            i = end;
        } else {
            i += 1;
        }
    }
    (blank, pieces)
}

/// The index of the `}` closing the `{` at `open` in blanked code.
fn closing_brace(blank: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &b) in blank.iter().enumerate().skip(open) {
        match b {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// The byte ranges of every `#[cfg(test)]` item: the attribute through
/// the item's closing brace (or `;`).
fn test_items(blank: &[u8]) -> Vec<(usize, usize)> {
    let text = String::from_utf8_lossy(blank);
    let mut ranges = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("#[cfg(test)]").map(|n| from + n) {
        let after = at + "#[cfg(test)]".len();
        // The item ends at its body's closing brace, or at a `;`, or — for
        // a gated field, arm or argument — at the `,` / `}` after it
        // (outside parentheses). Stopping early only scans more code.
        let mut parens = 0i32;
        let mut end = blank.len() - 1;
        for (i, &b) in blank.iter().enumerate().skip(after) {
            match b {
                b'(' | b'[' => parens += 1,
                b')' | b']' => parens -= 1,
                b'{' if parens == 0 => {
                    end = closing_brace(blank, i).unwrap_or(blank.len() - 1);
                    break;
                }
                b';' | b',' | b'}' if parens == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        ranges.push((at, end));
        from = end + 1;
    }
    ranges
}

/// Whether the literal at `start` is an argument (at any depth) of a log,
/// panic or assertion call: walk out through the enclosing parentheses
/// until the statement ends, reading the name before each.
fn inside_not_copy_call(blank: &[u8], start: usize) -> bool {
    let mut depth = 0i32;
    let mut i = start;
    while i > 0 {
        i -= 1;
        match blank[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' if depth > 0 => depth -= 1,
            b'(' | b'[' => {
                let head = &blank[..i];
                let name_end = head.iter().rposition(|b| !b.is_ascii_whitespace()).map_or(0, |n| n + 1);
                let name_start = head[..name_end]
                    .iter()
                    .rposition(|&b| !(b.is_ascii_alphanumeric() || b == b'_' || b == b'!' || b == b':'))
                    .map_or(0, |n| n + 1);
                let name = String::from_utf8_lossy(&head[name_start..name_end]);
                let name = name.rsplit("::").next().unwrap_or(&name);
                if NOT_COPY_CALLS.contains(&name) {
                    return true;
                }
            }
            b';' | b'{' | b'}' if depth == 0 => return false,
            _ => {}
        }
    }
    false
}

/// Whether `text` names a provider as a whole word.
fn names_a_provider(text: &str) -> bool {
    NAMES.iter().any(|name| {
        text.match_indices(name).any(|(at, _)| {
            let end = at + name.len();
            let before_ok = !text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_');
            let after_ok = !text[end..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
            before_ok && after_ok
        })
    })
}

/// Every literal in `code` that names a provider and is copy, as `(line,
/// literal)`.
fn offending_literals(code: &str) -> Vec<(usize, String)> {
    let (blank, pieces) = lex(code);
    let tests = test_items(&blank);
    pieces
        .into_iter()
        .filter_map(|Piece::Literal { start, text }| {
            if tests.iter().any(|&(from, to)| start >= from && start <= to) {
                return None;
            }
            if !names_a_provider(&text) || inside_not_copy_call(&blank, start) {
                return None;
            }
            Some((code[..start].matches('\n').count() + 1, text))
        })
        .collect()
}

/// Every offending literal in the shipped code, as `(file, line, literal)`.
fn offenders() -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (root, prefix) in source_roots() {
        let mut files = Vec::new();
        rust_files(&root, &mut files);
        files.sort();
        for path in files {
            let relative = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
            if is_test_path(&relative) {
                continue;
            }
            let Ok(code) = fs::read_to_string(&path) else { continue };
            for (line, text) in offending_literals(&code) {
                hits.push((format!("{prefix}{relative}"), line, text));
            }
        }
    }
    hits
}

fn allowed(file: &str, text: &str) -> bool {
    ALLOW.iter().any(|(listed, needle, _)| *listed == file && text.contains(needle))
}

#[test]
fn app_copy_names_no_provider_outside_its_own_row() {
    let unlisted: Vec<String> = offenders()
        .into_iter()
        .filter(|(file, _, text)| !allowed(file, text))
        .map(|(file, line, text)| format!("{file}:{line}: {text:?}"))
        .collect();
    assert!(
        unlisted.is_empty(),
        "{} string literals name a provider in app copy (rule 14). Read the name from the registry \
         (`ProviderId::label`, `providers::shared_label()`) on that provider's own row, or say \
         `providers::THE_AGENT` / \"this session's agent\":\n  {}",
        unlisted.len(),
        unlisted.join("\n  "),
    );
}

#[test]
fn the_copy_allowlist_only_shrinks() {
    assert!(ALLOW.len() <= CEILING, "ALLOW grew past its ceiling of {CEILING}: name the provider from the registry instead");
    let hits = offenders();
    let stale: Vec<String> = ALLOW
        .iter()
        .filter(|(file, needle, _)| !hits.iter().any(|(hit, _, text)| hit == file && text.contains(needle)))
        .map(|(file, needle, _)| format!("{file}: {needle:?}"))
        .collect();
    assert!(stale.is_empty(), "these ALLOW entries match nothing any more: remove them and lower CEILING:\n  {}", stale.join("\n  "));
}

/// The scan's self-test: a provider's name planted in a toast is caught,
/// in a `format!` too; comments, log lines, panics and test code are not.
#[test]
fn the_copy_scan_catches_a_planted_toast() {
    let sample = r####"
// "Muse" in a comment is prose.
/* and "Codex" in a block comment */
fn a(cx: &mut Cx) {
    overlays.toast("Muse finished", "It is still on disk.", cx);
    let detail = format!("Ask {} about it", label);
    let banner = format!("Claude Code is waiting: {reason}");
    crate::baaz_log!("draft on Muse failed: {reason}");
    let x = value.expect("Codex wrote no line");
    let quote = '"';
    let raw = r#"Codex says "hi""#;
    let lifetime: &'static str = "plain";
    assert_eq!(x, "Anthropic API", "OpenAI");
    let bytes = b"Muse";
}
#[cfg(test)]
mod tests {
    fn t() { let s = "{"; toast("Muse in a test", cx); }
}
#[cfg(test)]
fn helper() -> &'static str { "Claude in a test helper" }
fn after() { banner("OpenAI is down"); }
"####;
    let hits: Vec<String> = offending_literals(sample).into_iter().map(|(_, text)| text).collect();
    assert_eq!(
        hits,
        ["Muse finished", "Claude Code is waiting: {reason}", "Codex says \"hi\"", "OpenAI is down"],
        "the plant, the format!, the raw string and the code after the test items — nothing else"
    );
    assert!(!allowed("baaz/app/list.rs", "Muse finished"), "a plant in a real file is not allowlisted");
    assert!(names_a_provider("Muse's list") && !names_a_provider("Museum") && !names_a_provider("claude-code"));
}
