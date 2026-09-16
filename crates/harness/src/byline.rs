//! The sidebar's two-line byline (part 3).
//!
//! Line 1 is the owner's last request, line 2 is what came back. Computed
//! **free by default**: the last user message and the first meaningful line
//! of the latest assistant reply, collapsed, stripped of code fences and
//! markdown noise, and truncated to the row's own width ([`excerpt_line`]).
//! The free excerpt refreshes after every completed turn, so it costs no
//! model call.
//!
//! Only when the switch is on AND the free excerpt is poor (empty,
//! code-only, or past twice the row's width — see [`is_poor_excerpt`]) does
//! the app spend ONE cheap model call to rewrite the two lines, through the
//! same side-session mechanism as auto-titles: debounced to an idle session,
//! at most one start every [`REWRITE_DEBOUNCE_SECS`], skipping turns that
//! changed little ([`should_rewrite`]).

use std::time::Instant;

/// At most one rewrite start per session per this many seconds. Thirty
/// seconds is several turns at a comfortable reading pace: a session that
/// keeps producing poor excerpts still only spends one cheap call per half
/// minute, and a session that settles spends nothing more.
pub const REWRITE_DEBOUNCE_SECS: u64 = 30;

/// Past this many combined characters the free excerpt is poor: twice the
/// row's own 80-character cap, i.e. past what the row could show even if
/// both halves used the whole line. Below it the free cut fits; past it the
/// truncation is lossy enough to merit one rewrite.
pub const POOR_LENGTH_CHARS: usize = 160;

/// One side of the byline, free: the first meaningful line outside fenced
/// code regions — blanks and markdown markers skipped, inline noise
/// stripped — collapsed and cut to the row's own width. `None` is "nothing
/// to show", which is what a code-only reply excerpts to — the rewrite
/// predicate treats it as poor.
pub fn excerpt_line(text: &str) -> Option<String> {
    let mut fenced = false;
    for line in text.lines() {
        // Fenced regions are code, not prose: the ticks toggle, and
        // everything they guard is skipped for the lines around it.
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let stripped = strip_markdown(line);
        if stripped.is_empty() {
            continue;
        }
        let flat: String = stripped.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.is_empty() {
            continue;
        }
        return Some(crate::sidebar::one_line(&flat));
    }
    None
}

/// Whether the free excerpt is worth one rewrite: either half empty,
/// either half still fence-marked (the excerpt found no prose), either half
/// code-only, or the pair past what the row could ever show.
pub fn is_poor_excerpt(ask: Option<&str>, result: Option<&str>) -> bool {
    let (Some(ask), Some(result)) = (ask, result) else { return true };
    if ask.trim().is_empty() || result.trim().is_empty() {
        return true;
    }
    if ask.trim_start().starts_with("```") || result.trim_start().starts_with("```") {
        return true;
    }
    if is_code_only(ask) || is_code_only(result) {
        return true;
    }
    ask.chars().count() + result.chars().count() > POOR_LENGTH_CHARS
}

/// Whether a rewrite may start now: the switch on, the session idle (a
/// running turn never triggers one), the excerpt poor, the pair changed
/// since what is stored, and the last start outside the debounce window.
pub fn should_rewrite(
    auto_on: bool,
    running: bool,
    poor: bool,
    stored: (Option<&str>, Option<&str>),
    current: (Option<&str>, Option<&str>),
    last_start: Option<Instant>,
    now: Instant,
) -> bool {
    if !auto_on || running || !poor {
        return false;
    }
    if stored == current {
        return false;
    }
    match last_start {
        Some(started) => now.duration_since(started).as_secs() >= REWRITE_DEBOUNCE_SECS,
        None => true,
    }
}

/// The rewrite prompt: the two poor lines, quoted, asking for two short
/// replacements and nothing else. Bounded like the title prompt, for the
/// same cost reason.
pub fn rewrite_prompt(ask: &str, result: &str) -> String {
    let ask: String = ask.chars().take(crate::titles::TITLE_PROMPT_CHARS).collect();
    let result: String = result.chars().take(crate::titles::TITLE_PROMPT_CHARS).collect();
    format!(
        "Rewrite these two sidebar lines about a chat session — first what the user last asked, then what the assistant replied — as two short lines, each under 12 words:\n\nask: {ask}\nresult: {result}\n\nReply with exactly two lines, the rewritten ask then the rewritten result, no quotes, no numbering, no explanation."
    )
}

/// A rewrite reply as the two lines: the first two non-empty lines,
/// cleaned the free way. `None` is "nothing usable" — retry once per the
/// attempt budget, then keep the free excerpt, silently.
pub fn parse_rewrite(reply: &str) -> Option<(String, String)> {
    let mut fenced = false;
    let mut lines = reply.lines().filter_map(|line| {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
            return None;
        }
        if fenced {
            return None;
        }
        let stripped = strip_markdown(line);
        (!stripped.is_empty()).then(|| stripped.split_whitespace().collect::<Vec<_>>().join(" "))
    });
    let ask = lines.next().filter(|line| !is_code_only(line))?;
    let result = lines.next().filter(|line| !is_code_only(line))?;
    Some((crate::sidebar::one_line(&ask), crate::sidebar::one_line(&result)))
}

/// No alphabetic run of two or more: symbols, not prose — so short real
/// replies ("OK", "Hi", "Yes") pass while `=> {}`, `x = 1` and `…` do not.
/// Unicode-aware, so non-English words count as letters.
fn is_code_only(text: &str) -> bool {
    const CODE_ONLY_WORD: usize = 2;
    let mut run = 0;
    for c in text.chars() {
        if c.is_alphabetic() {
            run += 1;
            if run >= CODE_ONLY_WORD {
                return false;
            }
        } else {
            run = 0;
        }
    }
    true
}

/// One line without its markdown markers: headings, quotes, list bullets
/// (including `- [x]` task items and `1. ` ordered items), bold/italic/code
/// spans. Inline content is kept — only the markers go. A leading number is
/// a marker only with a `.` or `)` behind it, so a date like `2026-09-16`
/// survives.
fn strip_markdown(line: &str) -> String {
    let mut text = line.trim().to_owned();
    for _ in 0..4 {
        let before = text.len();
        let trimmed = text.trim();
        let checkboxed = trimmed
            .strip_prefix("[x]")
            .or_else(|| trimmed.strip_prefix("[X]"))
            .or_else(|| trimmed.strip_prefix("[ ]"))
            .map(str::trim_start)
            .unwrap_or(trimmed);
        let bulleted = checkboxed.trim_start_matches(['#', '>', '-', '*', '+', '`']).trim_start();
        text = strip_list_number(bulleted).to_owned();
        if text.len() == before {
            break;
        }
    }
    text.replace("**", "").replace("__", "").replace('`', "").trim().to_owned()
}

/// `1. ` and `1) ` lose their number; `2026-09-16` keeps it.
fn strip_list_number(text: &str) -> &str {
    let digits = text.len() - text.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    if digits == 0 {
        return text;
    }
    let rest = &text[digits..];
    if rest.starts_with(['.', ')']) {
        rest[1..].trim_start()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_prose_passes_through_collapsed() {
        assert_eq!(excerpt_line("tighten  validation\nsecond"), Some("tighten validation".into()));
    }

    #[test]
    fn fences_and_blank_lines_are_skipped_for_the_first_meaningful_line() {
        assert_eq!(
            excerpt_line("```rust\nlet x = 1;\n```\npatched the validator"),
            Some("patched the validator".into())
        );
        assert_eq!(excerpt_line("\n\n   \npatched it"), Some("patched it".into()));
        assert_eq!(excerpt_line("```\n```"), None);
        assert_eq!(excerpt_line("```rust\nlet x = 1;\n```"), None);
    }

    #[test]
    fn markdown_markers_are_stripped_from_both_sides() {
        assert_eq!(excerpt_line("## Tighten validation"), Some("Tighten validation".into()));
        assert_eq!(excerpt_line("> quoted reply"), Some("quoted reply".into()));
        assert_eq!(excerpt_line("- [x] fixed the **flaky** test"), Some("fixed the flaky test".into()));
        assert_eq!(excerpt_line("1. `cargo test` passes"), Some("cargo test passes".into()));
        assert_eq!(excerpt_line("2026-09-16 release notes"), Some("2026-09-16 release notes".into()));
    }

    #[test]
    fn very_long_lines_are_cut_the_rows_own_way() {
        let excerpt = excerpt_line(&"word ".repeat(100)).expect("a long line still excerpts");
        assert_eq!(excerpt, crate::sidebar::one_line(&"word ".repeat(100)));
    }

    #[test]
    fn non_english_prose_is_prose() {
        assert_eq!(excerpt_line("  日本語のテストを修正  "), Some("日本語のテストを修正".into()));
        assert_eq!(excerpt_line("إصلاح  الاختبار"), Some("إصلاح الاختبار".into()));
        assert!(!is_code_only("日本語のテストを修正"));
        assert!(!is_code_only("إصلاح الاختبار"));
        assert!(is_code_only("=> {}"));
    }

    #[test]
    fn empty_either_half_is_poor() {
        assert!(is_poor_excerpt(None, Some("patched it")));
        assert!(is_poor_excerpt(Some("fix it"), None));
        assert!(is_poor_excerpt(Some("  "), Some("patched it")));
        assert!(!is_poor_excerpt(Some("fix it"), Some("patched it")));
    }

    #[test]
    fn code_only_and_overlong_pairs_are_poor() {
        assert!(is_poor_excerpt(Some("fix it"), Some("```rust")));
        assert!(is_poor_excerpt(Some("=> {}"), Some("patched it")));
        assert!(is_poor_excerpt(Some("x = 1"), Some("patched it")));
        // …while short real replies stay prose, not code.
        assert!(!is_poor_excerpt(Some("say hi"), Some("Hi.")));
        assert!(!is_poor_excerpt(Some("go on"), Some("OK")));
        let long = "word ".repeat(40);
        assert!(is_poor_excerpt(Some(&long), Some("patched it")));
        assert!(!is_poor_excerpt(Some(&"word ".repeat(20)), Some("patched it")));
    }

    #[test]
    fn the_rewrite_runs_only_when_idle_poor_changed_and_cooled_down() {
        let now = Instant::now();
        let old = now - std::time::Duration::from_secs(REWRITE_DEBOUNCE_SECS + 1);
        let recent = now - std::time::Duration::from_secs(5);
        let stored = (Some("fix it"), Some("patched it"));
        let current = (Some("fix it harder"), Some("patched it twice"));
        assert!(should_rewrite(true, false, true, stored, current, None, now));
        assert!(should_rewrite(true, false, true, stored, current, Some(old), now));
        assert!(!should_rewrite(false, false, true, stored, current, None, now));
        assert!(!should_rewrite(true, true, true, stored, current, None, now));
        assert!(!should_rewrite(true, false, false, stored, current, None, now));
        assert!(!should_rewrite(true, false, true, stored, stored, None, now));
        assert!(!should_rewrite(true, false, true, stored, current, Some(recent), now));
    }

    #[test]
    fn a_rewrite_reply_parses_to_two_clean_lines() {
        assert_eq!(
            parse_rewrite("Tighten validation\nPatched the validator"),
            Some(("Tighten validation".into(), "Patched the validator".into()))
        );
        assert_eq!(parse_rewrite("only one line"), None);
        assert_eq!(parse_rewrite("```\n```"), None);
        assert_eq!(parse_rewrite(""), None);
    }

    #[test]
    fn the_rewrite_prompt_quotes_both_lines_and_asks_for_two() {
        let prompt = rewrite_prompt("fix it", "patched it");
        assert!(prompt.contains("fix it"));
        assert!(prompt.contains("patched it"));
        assert!(prompt.contains("exactly two lines"));
    }
}
