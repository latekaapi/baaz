//! Quote lines from the right pane (docs/82 7.1): select lines in an open
//! Changes diff or a file preview, and the selection bar's **Quote** puts
//! them in the open session's composer as a code quote carrying the path and
//! line range — the same quote the transcript's bar makes (docs/43 §0),
//! through the pane's one quote event ([`RightPaneEvent::Quote`]).
//!
//! The selection lives here, in the pane's cache: which view it is over and
//! the range of that view's selectable lines (`aui::workbench::diff_rows`
//! for a diff, the file's lines for a preview). The library draws the tint
//! and reports presses; this module turns a range into the quote's text,
//! language and location, and hangs the bar over the screen's body.
//!
//! A diff quote is a `diff` fence with the `+` / `-` / space markers,
//! numbered on the new side; when every selected line was removed it is
//! numbered on the old side and says so: `src/a.rs:40–42 (removed)`.

use std::cell::Cell;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use aui::protocol::Diff;
use aui::workbench::{diff_quote, diff_rows, selected_lines, DiffSide, LineSelectGeometry};
use gpui::{AnyElement, Bounds, Context, Pixels, Window};

use crate::right_pane::{RightPane, RightPaneEvent};
#[cfg(feature = "probe")]
use crate::layout::LayoutGlobal as _;
use crate::session::{QuoteLocation, QuoteSide};

/// Which view a line selection is over.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LineTarget {
    /// The open diff of `path` (root-relative) on the Changes screen.
    Diff { root: PathBuf, path: String },
    /// The file preview of `path` (root-relative) on the Files screen.
    Preview { root: PathBuf, path: String },
}

/// The pane's line selection and what its bar needs.
#[derive(Default)]
pub(crate) struct LineSelectUi {
    /// The held selection: its view and the range of that view's lines.
    pub held: Option<(LineTarget, Range<usize>)>,
    /// Where the selected lines were drawn (the views fill it at prepaint).
    pub geometry: LineSelectGeometry,
    /// Where the screen body holding the selection was drawn last frame.
    pub body: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// Whether a Quote has somewhere to go: a session is open (its
    /// composer). No session, no bar.
    pub can_quote: bool,
    /// The Changes screen's keyboard focus while lines are selected there
    /// (Esc clears them); the Files screen uses the pane's own.
    pub focus: Option<gpui::FocusHandle>,
}

impl LineSelectUi {
    /// The held range when it is over `target`.
    pub(crate) fn range_for(&self, target: &LineTarget) -> Option<Range<usize>> {
        self.held.as_ref().filter(|(t, _)| t == target).map(|(_, r)| r.clone())
    }
}

/// Lines ready for the composer: the quote's text, its fence language,
/// how many lines, and where they came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct LineQuote {
    pub text: String,
    pub lang: String,
    pub lines: usize,
    pub location: QuoteLocation,
}

/// A preview's `range` (0-based file lines, end exclusive) of `code` as a
/// quote: the lines as written, in the preview's highlighting language.
pub(crate) fn preview_quote(path: &str, language: &str, code: &str, range: &Range<usize>) -> Option<LineQuote> {
    let lines: Vec<String> = code.lines().map(str::to_string).collect();
    let r = aui::workbench::clamp_lines(range, lines.len())?;
    let text = selected_lines(&lines, &r)?;
    Some(LineQuote {
        text,
        lang: language.to_string(),
        lines: r.len(),
        location: QuoteLocation { path: path.to_string(), first: r.start as u32 + 1, last: r.end as u32, side: QuoteSide::File },
    })
}

/// A diff's `range` (over `aui::workbench::diff_rows`, with the opened
/// gaps `gaps`) as a `diff` quote.
pub(crate) fn diff_line_quote(path: &str, diff: &Diff, gaps: &[(usize, Arc<Vec<String>>)], range: &Range<usize>) -> Option<LineQuote> {
    let rows = diff_rows(diff, gaps);
    let q = diff_quote(&rows, range)?;
    let side = match q.side {
        DiffSide::New => QuoteSide::DiffNew,
        DiffSide::Old => QuoteSide::DiffOld,
    };
    Some(LineQuote { text: q.text, lang: "diff".into(), lines: q.lines, location: QuoteLocation { path: path.to_string(), first: q.first, last: q.last, side } })
}

/// The opened gaps of `path`'s diff, as the diff body draws them.
pub(crate) fn open_gaps(cache: &crate::right::RightCache, path: &str, diff: &Diff) -> Vec<(usize, Arc<Vec<String>>)> {
    (0..diff.hunks.len()).filter_map(|i| cache.gap_lines.get(&(path.to_string(), i)).map(|lines| (i, lines.clone()))).collect()
}

impl RightPane {
    /// A press or drag over a view's lines: hold the new range, take the
    /// keyboard (so Esc clears it).
    pub(crate) fn select_lines(&mut self, target: LineTarget, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let focus = match &target {
            LineTarget::Diff { .. } => self.cache.line_select.focus.clone().unwrap_or_else(|| self.focus.clone()),
            LineTarget::Preview { .. } => self.focus.clone(),
        };
        self.hold_lines(Some((target, range)), cx);
        window.focus(&focus, cx);
    }

    /// Whether quoted lines have somewhere to go: a session is open (its
    /// composer) and no provider switch is pending.
    pub(crate) fn can_quote(&self) -> bool {
        self.facts.session.is_some() && !self.facts.pending_neutral
    }

    /// Drop the held selection; whether there was one.
    pub(crate) fn clear_line_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.hold_lines(None, cx)
    }

    /// The one owner of the held selection: set it, redrawing when it
    /// changed; whether it changed.
    pub(crate) fn hold_lines(&mut self, held: Option<(LineTarget, Range<usize>)>, cx: &mut Context<Self>) -> bool {
        if self.cache.line_select.held == held {
            return false;
        }
        self.cache.line_select.held = held;
        cx.notify();
        true
    }

    /// The held selection as a quote, read from what the pane shows now.
    pub(crate) fn held_line_quote(&self) -> Result<LineQuote, String> {
        let (target, range) = self.cache.line_select.held.clone().ok_or("no lines are selected")?;
        match target {
            LineTarget::Preview { root, path } => {
                let preview = self.cache.preview_for(&root).filter(|p| p.path == path).ok_or("the preview of those lines is closed")?;
                let crate::right::PreviewContent::Text { language, code } = &preview.content else {
                    return Err("the preview shows no text".into());
                };
                preview_quote(&path, language, code, &range).ok_or_else(|| "the selected lines are past the file's end".into())
            }
            LineTarget::Diff { root, path } => {
                if !self.cache.changes_expanded_for(&root).contains(&path) {
                    return Err("that diff is closed".into());
                }
                let diff = self.cache.file_diffs.get(&path).cloned().flatten().ok_or("that diff is not loaded")?;
                let gaps = open_gaps(&self.cache, &path, &diff);
                diff_line_quote(&path, &diff, &gaps, &range).ok_or_else(|| "the selected lines are past the diff's end".into())
            }
        }
    }

    /// The bar's Quote: the held lines go to the open session's composer
    /// (which takes the keyboard), and the selection clears.
    pub(crate) fn quote_held_lines(&mut self, cx: &mut Context<Self>) -> Result<(), String> {
        if !self.can_quote() {
            return Err("no session is open to quote into".into());
        }
        let quote = self.held_line_quote()?;
        self.clear_line_selection(cx);
        cx.emit(RightPaneEvent::Quote { text: quote.text, lang: quote.lang, lines: quote.lines, location: Some(quote.location) });
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `select-lines:<path>:<a>-<b>` (and `…:old:<a>-<b>`): hold a line
    /// selection over the open preview of `path` (file lines `a..=b`) or,
    /// on Changes, over `path`'s open diff (the rows numbered `a..=b` on
    /// the new side, or on the old side with `old:`). Fails, holding
    /// nothing, when that view is not open or loaded or no line matches.
    pub(crate) fn step_select_lines(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (head, span) = rest.trim().rsplit_once(':').ok_or("expected <path>:<a>-<b>")?;
        let (path, old) = match head.strip_suffix(":old") {
            Some(path) => (path, true),
            None => (head, false),
        };
        let (a, b) = span.split_once('-').ok_or("expected <a>-<b>")?;
        let (a, b): (u32, u32) = (a.trim().parse().map_err(|_| format!("`{a}` is not a line"))?, b.trim().parse().map_err(|_| format!("`{b}` is not a line"))?);
        if a == 0 || b < a {
            return Err(format!("`{a}-{b}` is not a line range"));
        }
        let (root, _) = self.facts.project.clone().ok_or("no project for the right pane")?;
        let kind = crate::layout::right_kind(crate::layout::Layout::get(cx));
        let (target, range) = match kind {
            crate::layout::RightKind::Files => {
                let preview = self.cache.preview_for(&root).filter(|p| p.path == path).ok_or_else(|| format!("no preview of `{path}` is open"))?;
                let crate::right::PreviewContent::Text { code, .. } = &preview.content else {
                    return Err(format!("the preview of `{path}` shows no text (yet)"));
                };
                if old {
                    return Err("a preview has no old side".into());
                }
                let total = code.lines().count() as u32;
                if b > total {
                    return Err(format!("`{path}` has {total} lines"));
                }
                (LineTarget::Preview { root, path: path.to_string() }, (a as usize - 1)..b as usize)
            }
            crate::layout::RightKind::Changes => {
                if !self.cache.changes_expanded_for(&root).contains(path) {
                    return Err(format!("the diff of `{path}` is not open"));
                }
                let diff = self.cache.file_diffs.get(path).cloned().flatten().ok_or_else(|| format!("the diff of `{path}` is not loaded"))?;
                let rows = diff_rows(&diff, &open_gaps(&self.cache, path, &diff));
                let side = |l: &aui::protocol::DiffLine| if old { l.old_no.filter(|_| l.kind == aui::protocol::DiffKind::Del) } else { l.new_no };
                let hits: Vec<usize> = rows.iter().enumerate().filter(|(_, l)| side(l).is_some_and(|n| (a..=b).contains(&n))).map(|(i, _)| i).collect();
                let (Some(first), Some(last)) = (hits.first(), hits.last()) else {
                    return Err(format!("no line {a}-{b} in the diff of `{path}`"));
                };
                (LineTarget::Diff { root, path: path.to_string() }, *first..*last + 1)
            }
            other => return Err(format!("the pane shows {other:?}, not Changes or Files")),
        };
        self.hold_lines(Some((target, range)), cx);
        Ok(())
    }
}

/// The selection bar over a screen's body when the held selection is over
/// `target`, the screen draws that view (`shown`) and a session can take
/// the quote; the body must be `relative` and record its bounds in
/// `LineSelectUi::body`.
pub(crate) fn screen_bar(cache: &crate::right::RightCache, target: &LineTarget, shown: bool, window: &mut Window, cx: &mut Context<RightPane>) -> Option<AnyElement> {
    let ui = &cache.line_select;
    let present = shown && ui.can_quote && ui.range_for(target).is_some();
    let id = match target {
        LineTarget::Diff { .. } => "right-changes-quote-bar",
        LineTarget::Preview { .. } => "right-file-quote-bar",
    };
    let quote = cx.listener(|this: &mut RightPane, _: &gpui::ClickEvent, _, cx| {
        if let Err(reason) = this.quote_held_lines(cx) {
            crate::baaz_log!("quote lines: nothing quoted ({reason})");
        }
    });
    aui::workbench::line_quote_bar(id, present, &ui.geometry, ui.body.get(), quote, window, cx)
}

/// The target of `path`'s diff in `root`.
pub(crate) fn diff_target(root: &Path, path: &str) -> LineTarget {
    LineTarget::Diff { root: root.to_path_buf(), path: path.to_string() }
}

#[cfg(all(test, feature = "probe"))]
mod tests {
    use super::*;
    use aui::protocol::{DiffKind, DiffLine, Hunk};

    fn diff() -> Diff {
        let l = |kind, old, new, text: &str| DiffLine { kind, old_no: old, new_no: new, text: text.into() };
        Diff {
            path: "src/a.rs".into(),
            hunks: vec![Hunk {
                header: "@@ -38,6 +38,5 @@".into(),
                lines: vec![
                    l(DiffKind::Context, Some(38), Some(38), "fn a() {"),
                    l(DiffKind::Del, Some(39), None, "    one();"),
                    l(DiffKind::Del, Some(40), None, "    two();"),
                    l(DiffKind::Add, None, Some(39), "    both();"),
                    l(DiffKind::Context, Some(41), Some(40), "}"),
                ],
            }],
            added: 1,
            removed: 2,
        }
    }

    /// (a) A diff selection is a `diff` quote: markers kept, numbered on
    /// the new side.
    #[test]
    fn a_diff_selection_quotes_with_markers_and_new_side_lines() {
        let q = diff_line_quote("src/a.rs", &diff(), &[], &(0..4)).unwrap();
        assert_eq!(q.text, " fn a() {\n-    one();\n-    two();\n+    both();");
        assert_eq!((q.lang.as_str(), q.lines), ("diff", 4));
        assert_eq!(q.location, QuoteLocation { path: "src/a.rs".into(), first: 38, last: 39, side: QuoteSide::DiffNew });
        assert_eq!(q.location.label(), "src/a.rs:38–39");
    }

    /// (a) Removed lines alone are numbered on the old side, and say so.
    #[test]
    fn removed_lines_alone_quote_on_the_old_side() {
        let q = diff_line_quote("src/a.rs", &diff(), &[], &(1..3)).unwrap();
        assert_eq!(q.text, "-    one();\n-    two();");
        assert_eq!(q.location, QuoteLocation { path: "src/a.rs".into(), first: 39, last: 40, side: QuoteSide::DiffOld });
        assert_eq!(q.location.label(), "src/a.rs:39–40 (removed)");
    }

    /// (b) A preview selection quotes the lines as written, in the
    /// preview's language, 1-based.
    #[test]
    fn a_preview_selection_quotes_the_lines_as_written() {
        let code = "fn main() {\n\tlet x = 1;\n    run(x);\n}\n";
        let q = preview_quote("src/main.rs", "rust", code, &(1..3)).unwrap();
        assert_eq!(q.text, "\tlet x = 1;\n    run(x);");
        assert_eq!((q.lang.as_str(), q.lines), ("rust", 2));
        assert_eq!(q.location, QuoteLocation { path: "src/main.rs".into(), first: 2, last: 3, side: QuoteSide::File });
        assert_eq!(q.location.label(), "src/main.rs:2–3");
        // One line reads as one number; past the end quotes nothing.
        assert_eq!(preview_quote("src/main.rs", "rust", code, &(3..4)).unwrap().location.label(), "src/main.rs:4");
        assert_eq!(preview_quote("src/main.rs", "rust", code, &(9..12)), None);
    }
}
