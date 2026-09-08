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

/// Walk `root` for mentionable files, relative to it, sorted shortest first.
///
/// Blocking: the caller runs it on the background executor.
pub fn walk(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for entry in ignore::WalkBuilder::new(root).hidden(true).git_ignore(true).git_global(true).build().flatten() {
        if out.len() >= CAP {
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
        out.push(rel.into_owned());
    }
    // Shortest first, so `src/main.rs` beats `vendor/a/b/c/main.rs` on an equal
    // match, and stable within a length so the list never reshuffles.
    out.sort_by(|a, b| a.len().cmp(&b.len()).then_with(|| a.cmp(b)));
    out
}

/// Rank `paths` against `query` by subsequence match.
///
/// An empty query is every path in walk order. A path matches when the query's
/// characters appear in it in order, case-insensitively; ties break on how tight
/// the match is, then on how short the path is, which is what makes typing
/// `mainrs` land on `src/main.rs`.
pub fn filter<'a>(paths: &'a [String], query: &str) -> Vec<&'a String> {
    if query.is_empty() {
        return paths.iter().take(VISIBLE).collect();
    }
    let needle: Vec<char> = query.to_lowercase().chars().collect();
    let mut scored: Vec<(usize, usize, &String)> = Vec::new();
    for path in paths {
        if let Some(span) = subsequence_span(&path.to_lowercase(), &needle) {
            scored.push((span, path.len(), path));
        }
    }
    scored.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.cmp(b.2)));
    scored.into_iter().take(VISIBLE).map(|(_, _, path)| path).collect()
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

    fn paths() -> Vec<String> {
        vec![
            "src/main.rs".to_owned(),
            "src/app.rs".to_owned(),
            "vendor/deep/nested/main.rs".to_owned(),
            "README.md".to_owned(),
        ]
    }

    #[test]
    fn a_subsequence_matches_across_separators() {
        let paths = paths();
        let hits = filter(&paths, "mainrs");
        assert_eq!(hits.first().map(|p| p.as_str()), Some("src/main.rs"));
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
        assert_eq!(filter(&paths, "readme").first().map(|p| p.as_str()), Some("README.md"));
    }
}
