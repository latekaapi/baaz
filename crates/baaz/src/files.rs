//! The `@` mention index: the workspace's files, walked once.
//!
//! On the wire a mention is **plain text inside the text part** (research doc
//! §1.5) — a structured `mention` part is reserved and rejected — so all this
//! has to produce is a relative path to paste into the draft.
//!
//! The walk respects `.gitignore` through the `ignore` crate, skips `.git`, and
//! stops at [`CAP`] entries: a mention picker that had to hold a monorepo in
//! memory would cost more than it is worth, and 5 000 paths is more than the
//! filter ever shows.

use std::path::Path;

/// How many paths the index holds.
pub const CAP: usize = 5_000;
/// How many rows the picker shows at once.
pub const VISIBLE: usize = 8;

/// One mentionable file: the path and its lowercase, lowered once.
///
/// Lowercasing at walk time (rather than per keystroke) is what keeps the `@`
/// menu out of the typing-critical path: [`filter`] lowercases only the query
/// and compares against these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// The workspace-relative path, e.g. `src/main.rs`.
    pub path: String,
    /// `path` lowercased, for case-insensitive ranking without reallocating.
    pub lower: String,
}

/// [`walk`]'s result: the entries it found, and whether it stopped at [`CAP`]
/// rather than running out of files on its own (finding `support-8`) — a
/// monorepo's mention picker silently missing files past 5 000 with no
/// signal at all was the bug; this is the signal.
#[derive(Clone, Debug)]
pub struct WalkResult {
    /// The mentionable files, sorted shortest path first.
    pub entries: Vec<FileEntry>,
    /// `true` when the walk hit [`CAP`] and stopped, meaning some files in
    /// the workspace are not in `entries` at all.
    pub truncated: bool,
}

/// Walk `root` for mentionable files, relative to it, sorted shortest first.
///
/// Blocking: the caller runs it on the background executor.
pub fn walk(root: &Path) -> WalkResult {
    walk_capped(root, CAP)
}

/// [`walk`] against an explicit cap, which is what the test uses to exercise
/// truncation without creating [`CAP`] real files.
fn walk_capped(root: &Path, cap: usize) -> WalkResult {
    let mut out = Vec::new();
    let mut truncated = false;
    for entry in ignore::WalkBuilder::new(root).hidden(true).git_ignore(true).git_global(true).build().flatten() {
        if out.len() >= cap {
            truncated = true;
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Ok(rel) = entry.path().strip_prefix(root) else { continue };
        let rel = rel.to_string_lossy();
        if rel.is_empty() || rel.starts_with(".git/") {
            continue;
        }
        let path = rel.into_owned();
        let lower = path.to_lowercase();
        out.push(FileEntry { path, lower });
    }
    // Shortest first, so `src/main.rs` beats `vendor/a/b/c/main.rs` on an equal
    // match, and stable within a length so the list never reshuffles.
    out.sort_by(|a, b| a.path.len().cmp(&b.path.len()).then_with(|| a.path.cmp(&b.path)));
    WalkResult { entries: out, truncated }
}

/// Rank `paths` against `query` by subsequence match.
///
/// An empty query is every path in walk order. A path matches when the query's
/// characters appear in it in order, case-insensitively; ties break on how tight
/// the match is, then on how short the path is, which is what makes typing
/// `mainrs` land on `src/main.rs`.
///
/// Pure and cheap per call (only the query is lowercased), but still called
/// off the UI thread: 5 000 subsequence scans per keystroke do not belong in
/// render-adjacent code.
pub fn filter<'a>(paths: &'a [FileEntry], query: &str) -> Vec<&'a FileEntry> {
    if query.is_empty() {
        return paths.iter().take(VISIBLE).collect();
    }
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    let mut scored: Vec<(usize, usize, &FileEntry)> = Vec::new();
    for entry in paths {
        if let Some(span) = subsequence_span(&entry.lower, &needle) {
            scored.push((span, entry.path.len(), entry));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.path.cmp(&b.2.path)));
    scored.into_iter().take(VISIBLE).map(|(_, _, entry)| entry).collect()
}

/// How many characters of `haystack` the first subsequence match spans, or
/// `None` when there is none.
fn subsequence_span(haystack: &str, needle: &[char]) -> Option<usize> {
    let mut chars = haystack.char_indices();
    let mut first = None;
    let mut last = 0usize;
    for want in needle {
        loop {
            let (at, got) = chars.next()?;
            if got == *want {
                first.get_or_insert(at);
                last = at;
                break;
            }
        }
    }
    Some(last + 1 - first.unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths() -> Vec<FileEntry> {
        ["src/main.rs", "src/app.rs", "vendor/deep/nested/main.rs", "README.md"]
            .into_iter()
            .map(|path| FileEntry { path: path.to_owned(), lower: path.to_lowercase() })
            .collect()
    }

    #[test]
    fn a_subsequence_matches_across_separators() {
        let paths = paths();
        let hits = filter(&paths, "mainrs");
        assert_eq!(hits.first().map(|e| e.path.as_str()), Some("src/main.rs"));
        assert_eq!(hits.len(), 2);
    }

    #[test]
    fn no_match_is_no_rows() {
        let paths = paths();
        assert!(filter(&paths, "zzz").is_empty());
    }

    #[test]
    fn an_empty_query_is_the_head_of_the_index() {
        let paths = paths();
        assert_eq!(filter(&paths, "").len(), 4);
    }

    #[test]
    fn matching_is_case_insensitive() {
        let paths = paths();
        assert_eq!(filter(&paths, "readme").first().map(|e| e.path.as_str()), Some("README.md"));
    }

    #[test]
    fn the_walk_lowercases_once() {
        let entry = FileEntry { path: "Src/Main.RS".to_owned(), lower: "src/main.rs".to_owned() };
        assert_eq!(filter(&[entry], "MAIN").len(), 1);
    }

    /// **support-8 / A-MECH-17.** The walk used to stop at `CAP` with no
    /// signal at all; it must now say so.
    #[test]
    fn a_walk_that_hits_the_cap_reports_truncated() {
        let dir = std::env::temp_dir().join(format!("baaz-files-walk-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        for i in 0..5 {
            std::fs::write(dir.join(format!("file{i}.txt")), "x").expect("temp file");
        }
        let capped = walk_capped(&dir, 3);
        assert!(capped.truncated, "hitting the cap must be reported");
        assert_eq!(capped.entries.len(), 3);

        let uncapped = walk_capped(&dir, 100);
        assert!(!uncapped.truncated, "not hitting the cap must not be reported");
        assert_eq!(uncapped.entries.len(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
