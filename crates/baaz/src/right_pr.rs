//! The pull request in the session (docs/82 7.3): what the Changes pane's
//! `gh` read says about the branch's PR, past the card.
//!
//! - **The card** ([`pr_card_element`]) stays the Changes foot. Its
//!   trailing actions: Details (the checks and comments) and View, or —
//!   once the PR merged — Archive this session and View.
//! - **Details** is a pushed view over Changes, like Create PR
//!   ([`pr_detail_view`]): its back row, the checks (failing first: name,
//!   state, link) and the review comments (author, `file:line` for a line
//!   comment, the first lines of the body), the card still at the foot.
//!   A pushed view, not a disclosure on the card: the card sits in the
//!   pinned foot, and a list that grows there would cover the changes it
//!   sits under (docs/83 §1: one surface, no stacked panels).
//! - **Send to agent** on a failing check or a comment puts a quote into
//!   the open session's composer through the pane's one quote event
//!   ([`RightPaneEvent::Quote`]); it sends nothing by itself.
//! - **The rows**: each read lands in the session list's [`PrBook`], keyed
//!   by (folder, branch); a row on that folder and branch shows its PR
//!   beside its branch (`crate::sidebar`). Only what Changes already read
//!   — no row reads `gh` for itself — so a row learns its PR the first
//!   time Changes shows its folder, and keeps it for the run.
//! - **Settling**: a merged read offers Archive this session; with
//!   Settings › Git & worktrees' "Archive a session when its pull request
//!   merges" on, the read archives it itself, once per PR, never under a
//!   running turn (it waits for the next read).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use aui::data::spinner;
use aui::workbench::{changes_caption, commit_foot, pr_card, pushed_row, text_action};
use aui::util::{h_flex, v_flex};
use aui::util::ScrollAxisLock as _;
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, AnyElement, Context, FontWeight, SharedString, Window};

use crate::app::Harness;
use crate::layout::LayoutGlobal as _;
use crate::git_ops::{CheckState, CommentKind, PrCheck, PrComment, PrState, PullRequest, ReviewVerdict, RowPr};
use crate::right_pane::{RightPane, RightPaneEvent};
use crate::session::{QuoteLocation, QuoteSide};

/// Lines of a comment's body the Details view shows.
const COMMENT_LINES: usize = 3;
/// Room under the Details list so its last row scrolls clear of the card.
const DETAIL_FOOT_CLEARANCE: f32 = 120.0;

// ─────────────────────────────────────────────────────────────── the book

/// What each pull request read said, by (folder, branch): the session
/// list's, read when its rows are built. Folders are compared as written
/// with no trailing `/` — rows carry canonical workspaces, and the host
/// canonicalizes the folder it reports ([`Harness::pull_request_read`]).
#[derive(Default)]
pub(crate) struct PrBook {
    by_key: HashMap<(String, String), RowPr>,
    /// (folder, branch, PR number) already archived on merge: once per PR.
    settled: HashSet<(String, String, u64)>,
}

fn folder_key(folder: &str) -> String {
    let t = folder.trim_end_matches('/');
    if t.is_empty() { folder.to_owned() } else { t.to_owned() }
}

impl PrBook {
    /// Record (or, `None`, forget) the PR of `branch` in `folder`.
    /// Whether that changed anything.
    pub(crate) fn set(&mut self, folder: &str, branch: &str, pr: Option<RowPr>) -> bool {
        let key = (folder_key(folder), branch.to_owned());
        match pr {
            Some(pr) => self.by_key.insert(key, pr) != Some(pr),
            None => self.by_key.remove(&key).is_some(),
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_key.is_empty()
    }

    /// The PR of a row in `folder` on `branch`.
    pub(crate) fn lookup(&self, folder: Option<&str>, branch: Option<&str>) -> Option<RowPr> {
        let (folder, branch) = (folder?, branch?);
        self.by_key.get(&(folder_key(folder), branch.to_owned())).copied()
    }

    /// Whether this merged PR was already archived for.
    pub(crate) fn settled(&self, folder: &str, branch: &str, number: u64) -> bool {
        self.settled.contains(&(folder_key(folder), branch.to_owned(), number))
    }

    pub(crate) fn settle(&mut self, folder: &str, branch: &str, number: u64) {
        self.settled.insert((folder_key(folder), branch.to_owned(), number));
    }
}

// ─────────────────────────────────────────────────────────────── quotes

/// What Send to agent puts in the composer: the pane's quote event's
/// payload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrQuote {
    pub text: String,
    pub lang: String,
    pub lines: usize,
    pub location: Option<QuoteLocation>,
}

impl PrQuote {
    fn new(text: String, location: Option<QuoteLocation>) -> Self {
        let lines = text.lines().count().max(1);
        Self { text, lang: "text".into(), lines, location }
    }

    fn event(self) -> RightPaneEvent {
        RightPaneEvent::Quote { text: self.text, lang: self.lang, lines: self.lines, location: self.location }
    }
}

/// A check's state in words.
fn check_word(state: CheckState) -> &'static str {
    match state {
        CheckState::Failing => "failed",
        CheckState::Running => "running",
        CheckState::Passed => "passed",
        CheckState::Skipped => "skipped",
    }
}

/// A failing check as a quote: its name and conclusion, its link, and the
/// summary gh gave, if any.
pub(crate) fn check_quote(check: &PrCheck) -> PrQuote {
    let mut text = format!("Check `{}` {} ({})", check.name, check_word(check.state), check.conclusion);
    if !check.link.is_empty() {
        text.push_str(&format!("\n{}", check.link));
    }
    if let Some(summary) = &check.summary {
        text.push_str(&format!("\n{summary}"));
    }
    PrQuote::new(text, None)
}

/// Who said it, and how: `ada requested changes`, `ada commented`.
fn said(comment: &PrComment) -> String {
    let verb = match comment.kind {
        CommentKind::Review(ReviewVerdict::ChangesRequested) => "requested changes",
        CommentKind::Review(ReviewVerdict::Approved) => "approved",
        _ => "commented",
    };
    format!("{} {verb}", comment.author)
}

/// A comment as a quote: the author and the body; a line comment carries
/// its file and line as the quote's location (docs/82 7.1's
/// [`QuoteLocation`]).
pub(crate) fn comment_quote(comment: &PrComment) -> PrQuote {
    let location = match &comment.kind {
        CommentKind::Line { path, line, old_side } => Some(QuoteLocation { path: path.clone(), first: *line, last: *line, side: QuoteSide::Review { old: *old_side } }),
        _ => None,
    };
    PrQuote::new(format!("{}:\n{}", said(comment), comment.body), location)
}

// ─────────────────────────────────────────────────────────────── the pane

impl RightPane {
    /// Details opens (or closes) over Changes.
    pub(crate) fn set_pr_details(&mut self, root: &Path, open: bool, cx: &mut Context<Self>) {
        self.update_changes(root, |ui| ui.pr_details = open, cx);
    }

    /// Send to agent on a failing check.
    pub(crate) fn send_check_to_agent(&mut self, check: &PrCheck, cx: &mut Context<Self>) {
        cx.emit(check_quote(check).event());
    }

    /// Send to agent on a comment.
    pub(crate) fn send_comment_to_agent(&mut self, comment: &PrComment, cx: &mut Context<Self>) {
        cx.emit(comment_quote(comment).event());
    }

    #[cfg(feature = "probe")]
    /// `pr-fixture:<name>`'s pane half: a recorded `gh` answer as the PR of
    /// the capture's fixture tree — the tree clean, on the PR's branch,
    /// level with its upstream, so the foot is the card. Only in a
    /// capture; an unknown name is an `Err`.
    pub(crate) fn step_pr_fixture(&mut self, root: &Path, name: &str, details: bool, cx: &mut Context<Self>) -> Result<PullRequest, String> {
        if !crate::runtime::captures() {
            return Err("only a capture's fixture tree takes a recorded PR".into());
        }
        let pr = crate::git_ops::pr_fixture(name).ok_or_else(|| format!("no recorded PR `{name}` (passing, failing, merged, closed, draft)"))?;
        let held = pr.clone();
        self.update_changes(
            root,
            |ui| {
                ui.fixture_pr = Some(held.clone());
                ui.fixture_branch = Some(held.branch.clone());
                ui.pr = Some(Some(held));
                ui.pr_read_at = Some(std::time::Instant::now());
                ui.pr_details = details;
            },
            cx,
        );
        self.refresh_now(cx);
        Ok(pr)
    }
}

/// The card's View: the PR (or a check's page) in Browse.
fn view_action(id: impl Into<SharedString>, url: String, label: &str, cx: &mut Context<RightPane>) -> AnyElement {
    let id: SharedString = id.into();
    text_action(id, "View", Some(IconName::External), scale::H_SM, cx)
        .aria_label(label.to_owned())
        .on_click(cx.listener(move |_, _, _, cx| cx.emit(RightPaneEvent::ViewPullRequest(url.clone()))))
        .into_any_element()
}

/// The card at the Changes foot (and under Details): Details and View, or,
/// once merged, Archive this session (with a session open: rule 13) and
/// View. Under Details (`in_details`) the Details action is not repeated.
pub(crate) fn pr_card_element(pr: &PullRequest, root: &Path, in_details: bool, can_act: bool, cx: &mut Context<RightPane>) -> AnyElement {
    let mut actions = h_flex().items_center();
    if pr.state == PrState::Merged && can_act {
        actions = actions.child(
            text_action("right-pr-archive", "Archive this session", None, scale::H_SM, cx)
                .aria_label("Archive this session")
                .on_click(cx.listener(|_, _, _, cx| cx.emit(RightPaneEvent::ArchiveSession))),
        );
    } else if !in_details {
        let r = root.to_path_buf();
        actions = actions.child(
            text_action("right-pr-details", "Details", None, scale::H_SM, cx)
                .aria_label("Show the checks and review comments")
                .on_click(cx.listener(move |this, _, _, cx| this.set_pr_details(&r, true, cx))),
        );
    }
    actions = actions.child(view_action("right-pr-view", pr.url.clone(), "View the pull request", cx));
    pr_card("right-pr", pr.title.clone(), pr.number, pr_state(pr.state), pr.checks, actions, cx).into_any_element()
}

/// The PR card's state for a pull request's.
pub(crate) fn pr_state(state: PrState) -> aui::workbench::PrState {
    match state {
        PrState::Open => aui::workbench::PrState::Open,
        PrState::Draft => aui::workbench::PrState::Draft,
        PrState::Merged => aui::workbench::PrState::Merged,
        PrState::Closed => aui::workbench::PrState::Closed,
    }
}

/// Details: its back row and its body — the checks, then the comments,
/// the card at the foot.
pub(crate) fn pr_detail_view(pr: &PullRequest, root: &Path, can_act: bool, _window: &mut Window, cx: &mut Context<RightPane>) -> (Option<AnyElement>, AnyElement) {
    let back = root.to_path_buf();
    let row = pushed_row("right-pr-detail-row", format!("Pull request #{}", pr.number))
        .back_label("Back to changes")
        .on_back(cx.listener(move |this, _, _, cx| this.set_pr_details(&back, false, cx)))
        .into_any_element();
    let p = cx.aui().colors;
    let mut body = v_flex()
        .id("right-pr-detail")
        .size_full()
        .overflow_y_scroll()
        .scroll_axis_locked()
        .pt(px(scale::SP_2))
        .px(px(aui::interact::LIST_INSET))
        .pb(px(DETAIL_FOOT_CLEARANCE))
        .child(changes_caption("right-pr-checks-cap", "Checks", Some(pr.check_list.len()), true, cx));
    if pr.check_list.is_empty() {
        body = body.child(quiet_line("right-pr-no-checks", "This pull request has no checks.", cx));
    }
    for (i, check) in pr.check_list.iter().enumerate() {
        body = body.child(check_row(i, check, can_act, cx));
    }
    body = body.child(changes_caption("right-pr-comments-cap", "Comments", Some(pr.comments.len()), false, cx));
    if pr.comments.is_empty() {
        body = body.child(quiet_line("right-pr-no-comments", "No review comments.", cx));
    }
    for (i, comment) in pr.comments.iter().enumerate() {
        body = body.child(comment_block(i, comment, can_act, cx));
    }
    let card = pr_card_element(pr, root, true, can_act, cx);
    let screen = div().relative().size_full().bg(p.surface_1).child(body).child(commit_foot(card, cx));
    (Some(row), screen.into_any_element())
}

fn quiet_line(id: &'static str, text: &'static str, cx: &mut Context<RightPane>) -> AnyElement {
    let p = cx.aui().colors;
    div().id(id).h(px(scale::H_MD)).px(px(scale::SP_4)).flex().items_center().text_role(TextRole::Body).text_color(p.fg_muted).child(text).into_any_element()
}

/// One check: its state glyph, name and state; Send to agent when it
/// fails; its page.
fn check_row(i: usize, check: &PrCheck, can_act: bool, cx: &mut Context<RightPane>) -> AnyElement {
    let p = cx.aui().colors;
    let glyph: AnyElement = match check.state {
        CheckState::Failing => icon(IconName::X).size(px(scale::ICON_SM)).color(p.danger).into_any_element(),
        CheckState::Running => spinner(SharedString::from(format!("right-pr-check-spin-{i}"))).size(px(scale::ICON_SM)).muted(cx).into_any_element(),
        CheckState::Passed => icon(IconName::Check).size(px(scale::ICON_SM)).color(p.success).into_any_element(),
        CheckState::Skipped => icon(IconName::Slash).size(px(scale::ICON_SM)).color(p.ink_4).into_any_element(),
    };
    let mut row = h_flex()
        .id(SharedString::from(format!("right-pr-check-{i}")))
        .role(gpui::Role::Row)
        .aria_label(format!("{}: {}", check.name, check_word(check.state)))
        .h(aui_tokens::Metrics::active().control)
        .px(px(scale::SP_3))
        .gap(px(scale::SP_3))
        .items_center()
        .text_role(TextRole::Body)
        .child(div().flex_none().child(glyph))
        .child(div().min_w(px(0.0)).flex_1().truncate().text_color(p.fg).child(check.name.clone()))
        .child(div().flex_none().text_color(if check.state == CheckState::Failing { p.danger } else { p.fg_muted }).child(check_word(check.state)));
    if check.state == CheckState::Failing && can_act {
        let held = check.clone();
        row = row.child(
            text_action(SharedString::from(format!("right-pr-check-send-{i}")), "Send to agent", None, scale::H_SM, cx)
                .aria_label(format!("Send {} to the agent", check.name))
                .on_click(cx.listener(move |this, _, _, cx| this.send_check_to_agent(&held, cx))),
        );
    }
    if !check.link.is_empty() {
        let link = check.link.clone();
        row = row.child(
            text_action(SharedString::from(format!("right-pr-check-open-{i}")), "", Some(IconName::External), scale::H_SM, cx)
                .aria_label(format!("Open the {} check", check.name))
                .on_click(cx.listener(move |_, _, _, cx| cx.emit(RightPaneEvent::ViewPullRequest(link.clone())))),
        );
    }
    row.into_any_element()
}

/// One comment: who and how (with `file:line` for a line comment), its
/// first lines, Send to agent.
fn comment_block(i: usize, comment: &PrComment, can_act: bool, cx: &mut Context<RightPane>) -> AnyElement {
    let p = cx.aui().colors;
    let changes = matches!(comment.kind, CommentKind::Review(ReviewVerdict::ChangesRequested));
    let mut head = h_flex()
        .gap(px(scale::SP_2))
        .items_center()
        .min_w(px(0.0))
        .text_role(TextRole::Body)
        .child(div().flex_none().font_weight(FontWeight::SEMIBOLD).text_color(p.fg).child(comment.author.clone()));
    head = match &comment.kind {
        CommentKind::Line { path, line, old_side } => head.child(
            div()
                .min_w(px(0.0))
                .truncate()
                .font_family(scale::FONT_MONO)
                .role_size(TextRole::Caption)
                .text_color(p.fg_muted)
                .child(format!("{path}:{line}{}", if *old_side { " (removed)" } else { "" })),
        ),
        CommentKind::Review(ReviewVerdict::ChangesRequested) => head.child(div().flex_none().text_color(p.danger).child("requested changes")),
        CommentKind::Review(ReviewVerdict::Approved) => head.child(div().flex_none().text_color(p.fg_muted).child("approved")),
        _ => head.child(div().flex_none().text_color(p.fg_muted).child("commented")),
    };
    head = head.child(div().flex_1());
    if can_act {
        let held = comment.clone();
        let label = match &comment.kind {
            CommentKind::Line { path, line, .. } => format!("Send {}'s comment on {path}:{line} to the agent", comment.author),
            _ => format!("Send {}'s comment to the agent", comment.author),
        };
        head = head.child(
            text_action(SharedString::from(format!("right-pr-comment-send-{i}")), "Send to agent", None, scale::H_SM, cx)
                .aria_label(label)
                .on_click(cx.listener(move |this, _, _, cx| this.send_comment_to_agent(&held, cx))),
        );
    }
    let lines: Vec<String> = comment.body.lines().take(COMMENT_LINES).map(str::to_owned).collect();
    let cut = comment.body.lines().count() > COMMENT_LINES;
    v_flex()
        .id(SharedString::from(format!("right-pr-comment-{i}")))
        .role(gpui::Role::Group)
        .aria_label(format!("Comment by {}", comment.author))
        .px(px(scale::SP_3))
        .py(px(scale::SP_2))
        .gap(px(scale::SP_1))
        .child(head)
        .child(
            v_flex()
                .text_role(TextRole::Body)
                .text_color(if changes { p.fg } else { p.fg_muted })
                .children(lines.into_iter().map(|l| div().min_w(px(0.0)).truncate().child(l)))
                .when(cut, |d| d.child(div().text_color(p.ink_4).child("…"))),
        )
        .into_any_element()
}

// ─────────────────────────────────────────────────────────────── the host

impl Harness {
    /// A PR read landed for `root` on `branch` (not the default branch):
    /// the session list's book takes it for the rows and, when it merged
    /// and Settings › Git & worktrees says so, the session settles.
    pub(crate) fn pull_request_read(&mut self, root: &Path, branch: &str, pr: Option<PullRequest>, window: &mut Window, cx: &mut Context<Self>) {
        let folder = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf()).to_string_lossy().into_owned();
        let row = pr.as_ref().map(PullRequest::row);
        self.session_list.update(cx, |list, cx| list.set_pull_request(&folder, branch, row, cx));
        if let Some(pr) = pr.filter(|pr| pr.state == PrState::Merged) {
            self.archive_on_merge(root, &folder, branch, pr.number, window, cx);
        }
    }

    /// The sessions a merged PR settles: the rows on `folder` and
    /// `branch`, and the open session when the pane read for its folder.
    fn merge_targets(&self, root: &Path, folder: &str, branch: &str, cx: &gpui::App) -> Vec<String> {
        let list = self.session_list.read(cx);
        let same = |w: &str| folder_key(w) == folder_key(folder);
        let mut ids: Vec<String> = list
            .sessions
            .iter()
            .filter(|e| !e.archived && e.workspace.as_deref().is_some_and(same) && crate::sidebar::shown_branch(e).as_deref() == Some(branch))
            .map(|e| e.id.clone())
            .collect();
        let pane_root = self.right_project(cx).map(|(r, _)| r);
        if pane_root.as_deref() == Some(root) {
            if let Some(id) = self.active_id(cx).filter(|id| !list.overrides.get(id).is_some_and(|m| m.archived)) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        ids
    }

    /// Archive the merged PR's sessions when the setting is on — once per
    /// PR, never under a running turn: a session mid-turn is left for the
    /// next read, and the PR is settled only when none is left.
    fn archive_on_merge(&mut self, root: &Path, folder: &str, branch: &str, number: u64, window: &mut Window, cx: &mut Context<Self>) {
        if !crate::layout::Layout::get(cx).archive_on_merge || crate::runtime::captures() {
            return;
        }
        if self.session_list.read(cx).pull_requests.settled(folder, branch, number) {
            return;
        }
        let mut waiting = false;
        for id in self.merge_targets(root, folder, branch, cx) {
            let running = self.session_list.read(cx).sessions.iter().any(|e| e.id == id && e.running)
                || self.registry.find_runtime(self.active.as_ref(), &id, cx).is_some_and(|rt| rt.read(cx).is_sending());
            if running {
                crate::baaz_log!("pr: #{number} merged; {id} is mid-turn, archived at the next read");
                waiting = true;
                continue;
            }
            self.archive_session(id, Some(window), cx);
        }
        if !waiting {
            self.session_list.update(cx, |list, _| list.pull_requests.settle(folder, branch, number));
        }
    }

    /// The merged card's Archive this session: the open session, through
    /// the archive path (a worktree session's work is preserved there).
    pub(crate) fn archive_open_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active_id(cx) {
            self.archive_session(id, Some(window), cx);
        }
    }

    #[cfg(feature = "probe")]
    /// `pr-fixture:<name>` (and `pr-details:<name>`, Details open): a
    /// recorded `gh` answer as the PR of the pane's root, on Changes, and
    /// in the rows' book for that root and the PR's branch. No root, an
    /// unknown name, or not a capture is an `Err`. Never runs `gh`.
    pub(crate) fn step_pr_fixture(&mut self, rest: &str, details: bool, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        let name = rest.trim();
        let pr = self.with_right_pane(cx, |pane, cx| pane.step_pr_fixture(&root, name, details, cx))?;
        let folder = std::fs::canonicalize(&root).unwrap_or_else(|_| root.clone()).to_string_lossy().into_owned();
        let row = pr.row();
        self.session_list.update(cx, |list, cx| list.set_pull_request(&folder, &pr.branch, Some(row), cx));
        Ok(())
    }
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git_ops::pr_fixture;

    /// Send to agent on the failing check: its name, conclusion and link,
    /// no location; on a line comment: the comment, located at its line.
    #[test]
    fn send_to_agent_quotes_a_failing_check_and_a_line_comment() {
        let pr = pr_fixture("failing").expect("recorded");
        let failing = pr.check_list.iter().find(|c| c.state == CheckState::Failing).expect("one fails");
        let q = check_quote(failing);
        assert_eq!(q.text, "Check `test` failed (FAILURE)\nhttps://github.com/acme/acme-web/actions/runs/101/job/202");
        assert_eq!((q.lang.as_str(), q.lines, q.location.clone()), ("text", 2, None));
        let draft = pr_fixture("draft").expect("recorded");
        let q = check_quote(&draft.check_list[0]);
        assert_eq!(q.text, "Check `ci/lint` failed (ERROR)\nhttps://ci.example.com/acme-web/15/lint\n2 lint errors", "the summary gh gave rides along");

        let line = pr.comments.iter().find(|c| matches!(c.kind, CommentKind::Line { old_side: false, .. })).expect("a line comment");
        let q = comment_quote(line);
        assert_eq!(q.text, "ada commented:\n`v / 100` drops the half cent here: round first.");
        let at = q.location.clone().expect("located");
        assert_eq!(at, QuoteLocation { path: "src/lib.rs".into(), first: 7, last: 7, side: QuoteSide::Review { old: false } });
        assert_eq!(at.label(), "src/lib.rs:7");
        assert_eq!(at.prompt_line(), "A review comment on src/lib.rs, line 7:");
        let old = pr.comments.iter().find(|c| matches!(c.kind, CommentKind::Line { old_side: true, .. })).expect("an outdated one");
        assert_eq!(comment_quote(old).location.expect("located").label(), "src/lib.rs:2 (removed)");

        let review = &pr.comments[0];
        let q = comment_quote(review);
        assert!(q.text.starts_with("ada requested changes:\nThe total still truncates"));
        assert_eq!(q.location, None, "a general comment has no location");
    }

    #[test]
    fn the_book_keys_by_folder_and_branch() {
        let mut book = PrBook::default();
        let pr = pr_fixture("failing").expect("recorded").row();
        assert!(book.set("/r/acme/", "fix-checkout", Some(pr)));
        assert!(!book.set("/r/acme", "fix-checkout", Some(pr)), "the same read changes nothing");
        assert_eq!(book.lookup(Some("/r/acme"), Some("fix-checkout")), Some(pr));
        assert_eq!(book.lookup(Some("/r/acme"), Some("main")), None);
        assert_eq!(book.lookup(None, Some("fix-checkout")), None);
        assert!(book.set("/r/acme", "fix-checkout", None));
        assert!(book.is_empty());
    }
}
