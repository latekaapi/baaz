//! D6's Changes screen in Baaz (docs/43 §2.6, §4): this session's files and
//! the rest of the working tree, each row opening its diff in place; the
//! commit box at the foot whose single primary follows the branch — Commit
//! (the ticked files, or Amend), Push, Create PR, then the PR card; Create
//! PR as a pushed view; New branch…, Fetch and Pull on the branch line
//! (docs/82 7.2). Over a
//! session's checkpoints the list can be another diff — This session, Last
//! turn, one turn ([`crate::right_scopes`], docs/82 6.3) — with no commit
//! box. The git itself is [`crate::git_ops`] and [`crate::checkpoints`];
//! this module owns the screen's state, its actions and its drawing.
//! Nothing here touches the disk on the render path.

use aui::util::ScrollAxisLock as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::data::{checkbox, CheckboxIntent, CheckboxState};
use aui::interact::{dropdown, row_list, DropdownItem, LIST_INSET};
use aui::protocol::{ChangeKind, Diff, FileChange};
use aui::workbench::{
    ahead_behind, base_and_head, base_picker, branch_label, change_row, changes_caption, diff_fold, commit_foot, commit_frame,
    commit_message_view, diff_body, draft_checkbox, form_field, hook_failed_note, primary_action, pushed_row, skeleton_bar,
    state_block, text_action, unchanged_between, well_input,
};
use aui_icons::IconName;
use aui::util::{h_flex, v_flex, ElementExt as _};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, IntoElement, Pixels, ScrollHandle, SharedString, Window};
use gpui_kit::base::input::{InputState, TextareaState};
use gpui_kit::component::input::Textarea;

use crate::app::Harness;
use crate::right_pane::{RightPane, RightPaneEvent};
use crate::git_ops::{self, CommitOutcome, GitTree, PullRequest};

/// The clean state's lift above centre, clear of the commit foot.
const CLEAN_LIFT: f32 = 72.0;
/// Room under the list so the last row scrolls clear of the commit foot.
const FOOT_CLEARANCE: f32 = 180.0;
/// The same for the Create PR form's shorter foot.
const FORM_FOOT_CLEARANCE: f32 = 64.0;
/// The loading skeleton: a kind letter, a path of these widths, a stat.
const SKELETON_KIND_W: f32 = 10.0;
const SKELETON_WIDTHS: [f32; 5] = [90.0, 120.0, 70.0, 110.0, 84.0];
const SKELETON_STAT_W: f32 = 44.0;
/// The commit and description editors' line height.
const EDITOR_LH: f32 = 22.0;
/// The base-branch menu's narrowest width.
const BASE_MENU_MIN_W: f32 = 160.0;

/// What the commit box is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitBusy {
    Committing,
    Pushing,
    CreatingPr,
    /// New branch… (`git switch -c`).
    Branching,
    Fetching,
    Pulling,
}

/// The Create PR form (a pushed view on Changes).
pub(crate) struct PrForm {
    pub base: String,
    pub bases: Vec<String>,
    pub title: Entity<InputState>,
    pub body: Entity<TextareaState>,
    pub draft: bool,
    pub drafting: bool,
}

/// The Changes screen's state, for the current project root.
#[derive(Default)]
pub(crate) struct ChangesUi {
    /// The root this state belongs to; another root resets it.
    pub root: Option<PathBuf>,
    /// The commit message as drawn at rest (the editor's text while editing).
    pub message: String,
    /// Edited by the person: a draft never lands over it.
    pub message_edited: bool,
    /// The change set the last draft was asked for (no second draft for it).
    pub drafted_for: Option<u64>,
    pub drafting: bool,
    /// The message editor while the person types in it.
    pub editor: Option<Entity<TextareaState>>,
    pub busy: Option<GitBusy>,
    /// The pre-commit hook's output after a refused commit.
    pub hook_failure: Option<Vec<String>>,
    /// The branch's PR (`Some(None)`: read, none) and when it was read.
    pub pr: Option<Option<PullRequest>>,
    pub pr_read_at: Option<Instant>,
    pub pr_reading: bool,
    /// Details stands open over Changes: the PR's checks and review
    /// comments (docs/82 7.3, [`crate::right_pr`]).
    pub pr_details: bool,
    /// `origin/HEAD`'s branch, read once per root.
    pub default_branch: Option<String>,
    /// Commits the remote lacks when the branch has no upstream.
    pub unpushed_without_upstream: Option<u32>,
    pub form: Option<PrForm>,
    /// The transcript's chip opened these files: they flash 9 % once.
    pub flash: Option<(Instant, Vec<String>)>,
    /// What the list is a diff of, and the checkpoints behind the choices
    /// (docs/82 6.3, [`crate::right_scopes`]).
    pub scopes: crate::right_scopes::ScopeState,
    pub scroll: ScrollHandle,
    /// The person's ticks over the default ([`ticked_paths`]), per path;
    /// a path that leaves the tree drops its tick (docs/82 7.2).
    pub ticks: std::collections::HashMap<String, bool>,
    /// "Amend last commit" is ticked.
    pub amend: bool,
    /// The message Amend's fill replaced, put back on untick while the
    /// person has not edited it.
    pub amend_saved: Option<String>,
    /// HEAD exists and no remote holds it: Amend may be offered (read
    /// after each snapshot with changes, [`git_ops::head_amendable`]).
    pub amendable: bool,
    /// `origin` is a remote here: Fetch and Pull are offered.
    pub has_remote: bool,
    /// New branch…'s name field, while it stands open.
    pub branch_field: Option<Entity<InputState>>,
    /// A probe step put the fixture tree on this branch (captures read no
    /// real tree).
    #[cfg(feature = "probe")]
    pub fixture_branch: Option<String>,
    /// A probe step made a recorded `gh` answer this tree's PR: the tree
    /// reads clean so the foot is the card (`pr-fixture:`).
    #[cfg(feature = "probe")]
    pub fixture_pr: Option<PullRequest>,
    /// Each open row's top inside the scroll content, for the sticky row.
    pub row_tops: std::rc::Rc<std::cell::RefCell<std::collections::HashMap<String, (f32, f32)>>>,
}

/// Which field a draft fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DraftKind {
    Commit,
    PrBody,
}

/// The flash lasts this long, easing out.
const FLASH: Duration = Duration::from_millis(900);
/// While a PR's checks run, read them again this often.
const CHECKS_POLL: Duration = Duration::from_secs(15);
/// Otherwise the PR is read at most this often.
const PR_POLL: Duration = Duration::from_secs(60);

/// A stable hash of the change set (paths and counts): one draft per set.
fn change_set_key(files: &[FileChange]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    // The set of files and how each changed — not its line counts, or an
    // agent's every save would re-draft on the utility model.
    for f in files {
        f.path.hash(&mut h);
        format!("{:?}", f.change).hash(&mut h);
    }
    h.finish()
}

/// What the foot shows for the tree (docs/43 §2.6.1–6).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Foot {
    Commit,
    Push { ahead: u32 },
    CreatePr,
    Pr(PullRequest),
    /// On the default branch, clean and pushed: the branch line only.
    Level,
}

/// The foot for `tree`: changes → Commit; commits the remote lacks → Push;
/// a PR → its card; a pushed feature branch → Create PR.
pub(crate) fn foot_for(tree: &GitTree, ui: &ChangesUi) -> Foot {
    if !tree.files.is_empty() {
        return Foot::Commit;
    }
    let ahead = if tree.upstream.is_some() { tree.ahead } else { ui.unpushed_without_upstream.unwrap_or(0) };
    if ahead > 0 || (tree.upstream.is_none() && ui.unpushed_without_upstream.is_none()) && tree.branch != default_of(ui) {
        return Foot::Push { ahead: ahead.max(1) };
    }
    if let Some(Some(pr)) = &ui.pr {
        return Foot::Pr(pr.clone());
    }
    if tree.branch == default_of(ui) || tree.branch == "HEAD" {
        Foot::Level
    } else {
        Foot::CreatePr
    }
}

/// What the commit box commits (docs/82 7.2): the ticked files, in tree
/// order. A path's tick is the person's when they set one, else the
/// default — the open session's own files when it touched any (docs/81
/// F E3), else every file.
pub(crate) fn ticked_paths(files: &[FileChange], session_edits: &[String], root: &Path, ticks: &std::collections::HashMap<String, bool>) -> Vec<String> {
    let (session, _) = crate::right::partition_session_files(files, session_edits, root);
    let session: std::collections::HashSet<&str> = session.iter().map(|f| f.path.as_str()).collect();
    let by_default = |path: &str| session.is_empty() || session.contains(path);
    files.iter().filter(|f| ticks.get(&f.path).copied().unwrap_or_else(|| by_default(&f.path))).map(|f| f.path.clone()).collect()
}

/// Ticks for paths no longer in the tree go: a path that comes back takes
/// the default again.
fn keep_ticks_in(ticks: &mut std::collections::HashMap<String, bool>, files: &[FileChange]) {
    let paths: std::collections::HashSet<&str> = files.iter().map(|f| f.path.as_str()).collect();
    ticks.retain(|path, _| paths.contains(path.as_str()));
}

/// The commit button's words: "Commit N files", or "Amend" (and their
/// busy forms).
fn commit_label(n: usize, amend: bool, busy: bool) -> String {
    match (amend, busy) {
        (true, true) => "Amending…".into(),
        (true, false) => "Amend".into(),
        (false, true) => "Committing…".into(),
        (false, false) => format!("Commit {n} file{}", if n == 1 { "" } else { "s" }),
    }
}

/// Whether the branch is the default branch, or HEAD is detached: New
/// branch… is offered (docs/82 7.2).
fn on_default(tree: &GitTree, ui: &ChangesUi) -> bool {
    tree.branch == default_of(ui) || tree.branch == "HEAD"
}

/// What follows the branch when nothing is waiting to push: "up to date",
/// or how far the remote is ahead. Where Pull is offered it carries that
/// count instead and no note is drawn (docs/82 7.2).
fn level_note(behind: u32) -> SharedString {
    if behind == 0 { "· up to date".into() } else { format!("· ↓{behind} on the remote").into() }
}

/// What a capture's Amend fills the message with (captures read no HEAD).
const FIXTURE_HEAD_MESSAGE: &str = "Draw the Changes list from the working tree\n\nOne row per file, its diff in place.";

/// Amend unticked: the message it replaced comes back while the person
/// has not edited it.
fn untick_amend(ui: &mut ChangesUi) {
    ui.amend = false;
    if let Some(saved) = ui.amend_saved.take() {
        if !ui.message_edited {
            ui.message = saved;
        }
    }
}

fn default_of(ui: &ChangesUi) -> &str {
    ui.default_branch.as_deref().unwrap_or("main")
}

impl RightPane {
    /// Back from the PR form to the changes list (Back, Cancel).
    fn close_pr_form(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.update_changes(root, |ui| ui.form = None, cx);
    }

    /// Change `root`'s Changes state and redraw once: the screen's state
    /// changes go through here.
    pub(crate) fn update_changes(&mut self, root: &Path, change: impl FnOnce(&mut ChangesUi), cx: &mut Context<Self>) {
        change(self.changes_ui(root));
        cx.notify();
    }

    /// The Changes state for `root`, reset when the root moved.
    pub(crate) fn changes_ui(&mut self, root: &Path) -> &mut ChangesUi {
        if self.cache.changes.root.as_deref() != Some(root) {
            self.cache.changes = ChangesUi { root: Some(root.to_path_buf()), ..ChangesUi::default() };
        }
        &mut self.cache.changes
    }

    /// After a snapshot lands for `root`: keep the box's facts fresh — the
    /// default branch once, the unpushed count, the PR on its poll, a draft
    /// for a new change set. Every read runs in the background.
    pub(crate) fn changes_after_snapshot(&mut self, root: &Path, cx: &mut Context<Self>) {
        #[cfg(feature = "probe")]
        self.apply_fixture_branch(root);
        let Some(Some(tree)) = self.cache.git_for(root) else { return };
        let root_buf = root.to_path_buf();
        let ui = self.changes_ui(root);
        // A path that left the tree drops its tick; a new one takes the
        // default (docs/82 7.2).
        keep_ticks_in(&mut ui.ticks, &tree.files);
        let need_default = ui.default_branch.is_none();
        // Amend is offered only for a HEAD no remote holds. A capture reads
        // no real repository: the fixture's own branch facts decide.
        let captures = crate::runtime::captures();
        let need_amendable = !tree.files.is_empty() && !captures;
        if captures {
            ui.amendable = tree.upstream.is_none() || tree.ahead > 0;
            ui.has_remote = true;
        }
        let need_unpushed = tree.upstream.is_none() && tree.files.is_empty();
        let checks_running = matches!(&ui.pr, Some(Some(pr)) if pr.checks.is_some_and(|c| c.2));
        let poll = if checks_running { CHECKS_POLL } else { PR_POLL };
        // A capture reads no PR (a probe step loads a recorded one).
        let need_pr = !captures && tree.files.is_empty() && !ui.pr_reading && ui.pr_read_at.is_none_or(|at| at.elapsed() >= poll) && tree.upstream.is_some();
        // The rows hear of a feature branch's PR (docs/82 7.3): never the
        // default branch's.
        let pr_branch = (need_pr && !on_default(&tree, ui)).then(|| tree.branch.clone());
        if need_pr {
            ui.pr_reading = true;
        }
        if need_default || need_unpushed || need_pr || need_amendable {
            let root = root_buf.clone();
            cx.spawn(async move |this, cx| {
                let (default, remote, unpushed, pr, amendable) = cx
                    .background_executor()
                    .spawn(async move {
                        let default = git_ops::default_branch(&root);
                        let remote = (need_default && !captures).then(|| git_ops::has_origin(&root));
                        let unpushed = need_unpushed.then(|| git_ops::unpushed_without_upstream(&root, &default));
                        let pr = need_pr.then(|| git_ops::pr_for_branch(&root));
                        let amendable = need_amendable.then(|| git_ops::head_amendable(&root));
                        (default, remote, unpushed, pr, amendable)
                    })
                    .await;
                let read = pr.clone().zip(pr_branch);
                let _ = this.update(cx, |this: &mut Self, cx| {
                    this.update_changes(
                        &root_buf,
                        |ui| {
                            ui.default_branch = Some(default);
                            if let Some(remote) = remote {
                                ui.has_remote = remote;
                            }
                            if let Some(n) = unpushed {
                                ui.unpushed_without_upstream = Some(n);
                            }
                            if let Some(pr) = pr {
                                ui.pr = Some(pr);
                                ui.pr_read_at = Some(Instant::now());
                                ui.pr_reading = false;
                            }
                            if let Some(amendable) = amendable {
                                ui.amendable = amendable;
                                if !amendable && ui.amend {
                                    untick_amend(ui);
                                }
                            }
                        },
                        cx,
                    );
                    if let Some((pr, branch)) = read {
                        cx.emit(RightPaneEvent::PullRequestRead { root: root_buf.clone(), branch, pr });
                    }
                });
            })
            .detach();
        }
        self.maybe_draft_commit(root, &tree, cx);
        self.scopes_after_snapshot(root, cx);
    }

    /// Draft the commit message once per change set on the session's own
    /// utility model (owner 2026-10-17), never over the person's words. The
    /// pane marks the set and asks the root
    /// ([`RightPaneEvent::CommitDraftWanted`]), which holds the session's
    /// title and model and runs the completion
    /// ([`Harness::draft_commit_for_pane`]).
    fn maybe_draft_commit(&mut self, root: &Path, tree: &GitTree, cx: &mut Context<Self>) {
        if tree.files.is_empty() {
            return;
        }
        let key = change_set_key(&tree.files);
        let provider = self.facts.provider;
        let ui = self.changes_ui(root);
        // A draft that the person never touched follows the change set: a
        // new set re-drafts over it (live 2026-10-19: "Commit 3 files" under
        // a message about two). Their own words are never replaced.
        let own_words = ui.message_edited || (!ui.message.trim().is_empty() && ui.drafted_for.is_none());
        if std::env::var("BAAZ_DRAFT_TRACE").as_deref() == Ok("1") {
            eprintln!(
                "baaz: draft check provider={provider:?} own_words={own_words} drafting={} same_set={} files={}",
                ui.drafting,
                ui.drafted_for == Some(key),
                tree.files.len()
            );
        }
        // Amend's message is HEAD's: no draft lands over it.
        if own_words || ui.amend || ui.drafting || ui.drafted_for == Some(key) {
            return;
        }
        // No session's provider yet (the first snapshot can land before the
        // session opens): leave the set undrafted for the next snapshot
        // (live 2026-10-19: the set was marked and never drafted).
        let Some(provider) = provider else { return };
        ui.message.clear();
        ui.drafted_for = Some(key);
        if !crate::utility::supports(provider) && !provider.has(provider::ProviderTrait::SideSessionCompletions) {
            return;
        }
        ui.drafting = true;
        cx.emit(RightPaneEvent::CommitDraftWanted { root: root.to_path_buf(), files: tree.files.clone() });
    }

    /// A commit or PR draft came back (or failed, `None`): the box stops
    /// drafting, and the text lands where the person has not typed.
    pub(crate) fn land_draft(&mut self, root: &Path, kind: DraftKind, text: Option<String>, cx: &mut Context<Self>) {
        let text = text.map(|text| clean_draft(&text)).filter(|text| !text.is_empty());
        match kind {
            DraftKind::Commit => {
                let ui = self.changes_ui(root);
                ui.drafting = false;
                if let Some(text) = text {
                    if !ui.message_edited && ui.message.trim().is_empty() {
                        ui.message = text;
                    }
                }
            }
            DraftKind::PrBody => {
                if let Some(form) = self.changes_ui(root).form.as_mut() {
                    form.drafting = false;
                    if let Some(text) = text {
                        let body = form.body.clone();
                        if body.read(cx).value().is_empty() {
                            self.pending_pr_body = Some((body, text));
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    /// Toggle one row's diff, reading it the first time it opens.
    pub(crate) fn toggle_change_row(&mut self, root: &Path, change: &FileChange, cx: &mut Context<Self>) {
        let open = self.cache.flip_changes_file(root, &change.path);
        if !open && self.cache.line_select.held.as_ref().is_some_and(|(t, _)| *t == crate::right_quote::diff_target(root, &change.path)) {
            self.cache.line_select.held = None;
        }
        if open && !self.cache.file_diffs.contains_key(&change.path) {
            self.load_file_diff(root, change.clone(), cx);
        }
        cx.notify();
    }

    /// Read one row's diff in the shown scope: against `HEAD` in All
    /// changes, between the scope's two ends otherwise. A diff that lands
    /// after the scope moved on is dropped.
    fn load_file_diff(&mut self, root: &Path, change: FileChange, cx: &mut Context<Self>) {
        self.cache.file_diffs.insert(change.path.clone(), None);
        let ends = self.changes_ui(root).scopes.scoped_ends();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let path = change.path.clone();
            let read_ends = ends.clone();
            let diff = if crate::runtime::captures() {
                Some(crate::right::fixture_diff_for(&path))
            } else {
                let root = root.clone();
                cx.background_executor()
                    .spawn(async move {
                        match read_ends {
                            Some((from, to)) => crate::checkpoints::diff_file(&root, &from, &to, &change.path),
                            None => git_ops::read_file_diff(&root, &change),
                        }
                    })
                    .await
            };
            let _ = this.update(cx, |this: &mut Self, cx| this.land(&root, crate::right_scopes::Landed::Diff { path, diff, ends }, cx));
        })
        .detach();
    }

    /// Diffs for rows already open when the tree re-read (their reads were
    /// dropped as stale).
    pub(crate) fn reload_open_diffs(&mut self, root: &Path, cx: &mut Context<Self>) {
        for path in self.cache.changes_expanded_for(root) {
            if !self.cache.file_diffs.contains_key(&path) {
                if let Some(change) = self.shown_change(root, &path) {
                    self.load_file_diff(root, change, cx);
                }
            }
        }
    }

    /// Expand the unchanged run after hunk `i` of `path`: from the file on
    /// disk in All changes, from the scope's newer end otherwise.
    pub(crate) fn expand_diff_gap(&mut self, root: &Path, path: String, i: usize, range: std::ops::Range<u32>, cx: &mut Context<Self>) {
        let to = self.changes_ui(root).scopes.scoped_ends().map(|(_, to)| to);
        let root = root.to_path_buf();
        let root_for_land = root.clone();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let lines = if crate::runtime::captures() {
                (range.start..range.end).map(|n| format!("        // line {n}")).collect()
            } else {
                cx.background_executor()
                    .spawn(async move {
                        match to {
                            Some(to) => crate::checkpoints::read_lines_at(&root, &to, &p, range),
                            None => git_ops::read_lines(&root, &p, range),
                        }
                    })
                    .await
            };
            let _ = this.update(cx, |this: &mut Self, cx| this.land(&root_for_land, crate::right_scopes::Landed::Gap { path, hunk: i, lines }, cx));
        })
        .detach();
    }

    /// The message editor: put the drawn message into a textarea and focus
    /// it (⌘⏎ commits).
    pub(crate) fn edit_commit_message(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.changes_ui(root).message.clone();
        let editor = cx.new(|cx| {
            let mut state = aui::composer::composer_state_rows("Describe the change", 2, 8, window, cx);
            state.set_value(&text, window, cx);
            state
        });
        let focus = gpui::Focusable::focus_handle(editor.read(cx), cx);
        window.focus(&focus, cx);
        self.changes_ui(root).editor = Some(editor);
        cx.notify();
    }

    /// Leave the editor, keeping its text as the message.
    pub(crate) fn settle_commit_message(&mut self, root: &Path, cx: &mut Context<Self>) {
        let ui = self.changes_ui(root);
        if let Some(editor) = ui.editor.take() {
            let text = editor.read(cx).value().to_string();
            let ui = self.changes_ui(root);
            if text != ui.message {
                ui.message_edited = true;
                ui.message = text;
            }
        }
        cx.notify();
    }

    /// Commit exactly the ticked files ([`ticked_paths`]) with the message
    /// — never the rest of the tree — or, with "Amend last commit" ticked,
    /// take them into HEAD ([`git_ops::amend_paths`]; no ticked file
    /// rewrites the message alone).
    pub(crate) fn commit_changes(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.settle_commit_message(root, cx);
        let session_edits = self.session_edits(cx);
        let tree = self.cache.git_for(root).flatten();
        let ui = self.changes_ui(root);
        let paths = tree.map(|tree| ticked_paths(&tree.files, &session_edits, root, &ui.ticks)).unwrap_or_default();
        let message = ui.message.trim().to_string();
        let amend = ui.amend && ui.amendable;
        if message.is_empty() || ui.busy.is_some() || (paths.is_empty() && !amend) {
            return;
        }
        if crate::runtime::captures() {
            crate::baaz_log!("changes: a capture commits nothing");
            return;
        }
        ui.hook_failure = None;
        self.set_git_busy(root, Some(GitBusy::Committing), cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move { if amend { git_ops::amend_paths(&r, &message, &paths) } else { git_ops::commit_paths(&r, &message, &paths) } })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                match outcome {
                    CommitOutcome::Done => {
                        ui.message.clear();
                        ui.message_edited = false;
                        ui.drafted_for = None;
                        ui.unpushed_without_upstream = None;
                        ui.amend = false;
                        ui.amend_saved = None;
                        ui.ticks.clear();
                    }
                    CommitOutcome::HookFailed(lines) => ui.hook_failure = Some(lines),
                    CommitOutcome::Failed(reason) => this.toast("Commit", reason, cx),
                }
                this.refresh_now(cx);
                this.set_git_busy(&root, None, cx);
            });
        })
        .detach();
    }

    /// A git operation on `root`'s Changes box starts (`Some`) or ends
    /// (`None`): the box's buttons and its spinner read it.
    fn set_git_busy(&mut self, root: &Path, busy: Option<GitBusy>, cx: &mut Context<Self>) {
        self.update_changes(root, |ui| ui.busy = busy, cx);
    }

    /// Tick or untick `paths` for the commit (a row's box, or a section's
    /// tick-all): the person's choice over the default from now on.
    pub(crate) fn set_ticks(&mut self, root: &Path, paths: Vec<String>, on: bool, cx: &mut Context<Self>) {
        self.update_changes(
            root,
            |ui| {
                for path in paths {
                    ui.ticks.insert(path, on);
                }
            },
            cx,
        );
    }

    /// "Amend last commit" ticked or not. Ticked, the message becomes
    /// HEAD's unless the person wrote their own; unticked, the message it
    /// replaced comes back if still untouched.
    pub(crate) fn set_amend(&mut self, root: &Path, on: bool, cx: &mut Context<Self>) {
        if !on {
            self.update_changes(root, untick_amend, cx);
            return;
        }
        let ui = self.changes_ui(root);
        if !ui.amendable || ui.amend {
            return;
        }
        let own_words = ui.message_edited || ui.editor.is_some();
        self.update_changes(root, |ui| ui.amend = true, cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let head = if crate::runtime::captures() {
                Some(FIXTURE_HEAD_MESSAGE.to_owned())
            } else {
                cx.background_executor().spawn(async move { git_ops::head_message(&r) }).await
            };
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.update_changes(
                    &root,
                    |ui| {
                        if let Some(head) = head.filter(|_| ui.amend && !own_words && !ui.message_edited) {
                            ui.amend_saved = Some(std::mem::replace(&mut ui.message, head));
                        }
                    },
                    cx,
                );
            });
        })
        .detach();
    }

    /// New branch…: the name field opens, prefilled with the branch prefix
    /// (Settings › Worktrees), focused; ⏎ creates, Esc cancels.
    pub(crate) fn open_new_branch(&mut self, root: &Path, prefix: String, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| {
            let mut s = InputState::new(window, cx).placeholder("Branch name");
            s.set_accessibility(gpui_kit::base::input::InputAccessibility::new(Some(gpui::Role::TextInput)).with_label(Some("New branch name".into())));
            s.set_value(&prefix, window, cx);
            s
        });
        let focus = gpui::Focusable::focus_handle(field.read(cx), cx);
        window.focus(&focus, cx);
        self.update_changes(root, |ui| ui.branch_field = Some(field), cx);
    }

    /// Esc, or Cancel: the field closes and nothing changed.
    pub(crate) fn cancel_new_branch(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.update_changes(root, |ui| ui.branch_field = None, cx);
    }

    /// ⏎ in the field: `git switch -c <name>`, the working tree carried. A
    /// name git refuses, or one that exists, is a toast in git's words, the
    /// field left open and nothing changed.
    pub(crate) fn create_new_branch(&mut self, root: &Path, cx: &mut Context<Self>) {
        let ui = self.changes_ui(root);
        let Some(field) = ui.branch_field.clone() else { return };
        if ui.busy.is_some() {
            return;
        }
        let name = field.read(cx).value().trim().to_string();
        if crate::runtime::captures() {
            crate::baaz_log!("changes: a capture creates no branch");
            return;
        }
        self.set_git_busy(root, Some(GitBusy::Branching), cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx.background_executor().spawn(async move { git_ops::create_branch(&r, &name) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                match result {
                    Ok(()) => {
                        let ui = this.changes_ui(&root);
                        ui.branch_field = None;
                        ui.pr = None;
                        ui.pr_read_at = None;
                        ui.unpushed_without_upstream = None;
                    }
                    Err(reason) => this.toast("New branch", reason, cx),
                }
                this.refresh_now(cx);
                this.set_git_busy(&root, None, cx);
            });
        })
        .detach();
    }

    /// Fetch (`git fetch --prune origin`) or Pull (`git pull --ff-only`),
    /// off the UI thread, then the tree is read again. A refusal is a toast
    /// in git's words; a pull git refuses changed nothing. Never in a
    /// capture.
    pub(crate) fn sync_remote(&mut self, root: &Path, pull: bool, cx: &mut Context<Self>) {
        if self.changes_ui(root).busy.is_some() {
            return;
        }
        if crate::runtime::captures() {
            crate::baaz_log!("changes: a capture reaches no remote");
            return;
        }
        self.set_git_busy(root, Some(if pull { GitBusy::Pulling } else { GitBusy::Fetching }), cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx.background_executor().spawn(async move { if pull { git_ops::pull_ff_only(&r) } else { git_ops::fetch(&r) } }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.changes_ui(&root).pr_read_at = None;
                if let Err(reason) = result {
                    this.toast(if pull { "Pull" } else { "Fetch" }, reason, cx);
                }
                this.refresh_now(cx);
                this.set_git_busy(&root, None, cx);
            });
        })
        .detach();
    }

    #[cfg(feature = "probe")]
    /// `changes-branch:<name>`'s pane half: the capture's fixture tree on
    /// `name`, tracking `origin/<name>` and level with it.
    fn apply_fixture_branch(&mut self, root: &Path) {
        let Some(branch) = self.changes_ui(root).fixture_branch.clone() else { return };
        let clean = self.changes_ui(root).fixture_pr.is_some();
        if let Some(Some(tree)) = self.cache.git.as_mut().filter(|slot| slot.root == root).map(|slot| &mut slot.value) {
            if clean {
                tree.files.clear();
            }
            tree.upstream = Some(format!("origin/{branch}"));
            tree.branch = branch;
            tree.ahead = 0;
            tree.behind = 0;
        }
    }

    /// Push the branch (setting its upstream the first time).
    pub(crate) fn push_branch(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(Some(tree)) = self.cache.git_for(root) else { return };
        if self.changes_ui(root).busy.is_some() {
            return;
        }
        self.set_git_busy(root, Some(GitBusy::Pushing), cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx.background_executor().spawn(async move { git_ops::push(&r, &tree.branch, tree.upstream.is_some()) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                ui.unpushed_without_upstream = None;
                ui.pr_read_at = None;
                if let Err(reason) = result {
                    this.toast("Push", reason, cx);
                }
                this.refresh_now(cx);
                this.set_git_busy(&root, None, cx);
            });
        })
        .detach();
    }

    /// "Ask … to fix it": the hook's output goes into the composer as a
    /// quote (docs/43 §2.6.3, §4), with no draft text — through the root,
    /// which holds the composer.
    pub(crate) fn ask_to_fix_hook(&mut self, lines: &[String], cx: &mut Context<Self>) {
        cx.emit(RightPaneEvent::Quote { text: lines.join("\n"), lang: "text".into(), lines: lines.len(), location: None });
    }

    /// Open the Create PR form: base = the default branch, the title and
    /// description drafted from the session once (owner 2026-10-17). The
    /// root supplies what the session knows (its title, provider and model)
    /// through [`Harness::open_pr_form`].
    pub(crate) fn open_pr_form_with(&mut self, root: &Path, draft: PrDraftFacts, window: &mut Window, cx: &mut Context<Self>) {
        let PrDraftFacts { label, provider, model } = draft;
        let base = self.changes_ui(root).default_branch.clone().unwrap_or_else(|| "main".into());
        let title = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            if let Some(label) = &label {
                s.set_value(label, window, cx);
            }
            s
        });
        let body = cx.new(|cx| aui::composer::composer_state_rows("Describe the change", 6, 16, window, cx));
        let ui = self.changes_ui(root);
        ui.form = Some(PrForm { base, bases: Vec::new(), title, body: body.clone(), draft: false, drafting: false });
        cx.notify();
        let root_buf = root.to_path_buf();
        let files = self.cache.git_for(root).flatten().map(|t| t.files).unwrap_or_default();
        let base_for_summary = self.changes_ui(root).form.as_ref().map(|f| f.base.clone()).unwrap_or_else(|| "main".into());
        if let Some(form) = self.changes_ui(root).form.as_mut() {
            form.drafting = provider.is_some();
        }
        cx.spawn(async move |this, cx| {
            let r = root_buf.clone();
            let bases = cx.background_executor().spawn(async move { git_ops::remote_branches(&r) }).await;
            let r = root_buf.clone();
            let (commits, stat) = cx.background_executor().spawn(async move { git_ops::branch_summary(&r, &base_for_summary) }).await;
            let prompt = pr_prompt(label.as_deref(), &commits, &stat, &files);
            if provider.is_some_and(|provider| provider.has(provider::ProviderTrait::SideSessionCompletions)) {
                let (root, prompt) = (root_buf.clone(), prompt.clone());
                let _ = this.update(cx, |_, cx| cx.emit(RightPaneEvent::DraftOnSide { root, kind: DraftKind::PrBody, prompt }));
            }
            let draft = match provider.filter(|p| crate::utility::supports(*p)) {
                Some(p) => cx.background_executor().spawn(async move { crate::utility::complete(p, model.as_deref(), &prompt, None).ok() }).await,
                None => None,
            };
            let _ = this.update(cx, |this: &mut Self, cx| {
                if let Some(form) = this.changes_ui(&root_buf).form.as_mut() {
                    form.bases = bases;
                    form.drafting = false;
                    if let Some(text) = draft.map(|t| clean_draft(&t)).filter(|t| !t.is_empty()) {
                        let body = form.body.clone();
                        let untouched = body.read(cx).value().is_empty();
                        if untouched {
                            this.pending_pr_body = Some((body, text));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// `gh pr create` from the form.
    pub(crate) fn create_pull_request(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(Some(tree)) = self.cache.git_for(root) else { return };
        let ui = self.changes_ui(root);
        let Some(form) = ui.form.as_ref() else { return };
        let title = form.title.read(cx).value().to_string();
        let body = form.body.read(cx).value().to_string();
        let (base, draft) = (form.base.clone(), form.draft);
        if title.trim().is_empty() || ui.busy.is_some() {
            return;
        }
        self.set_git_busy(root, Some(GitBusy::CreatingPr), cx);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let head = tree.branch.clone();
            let result = cx.background_executor().spawn(async move { git_ops::create_pr(&r, &base, &head, &title, &body, draft) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                match result {
                    Ok(pr) => {
                        ui.pr = Some(Some(pr));
                        ui.pr_read_at = Some(Instant::now());
                        ui.form = None;
                    }
                    Err(reason) => this.toast("Create PR", reason, cx),
                }
                this.set_git_busy(&root, None, cx);
            });
        })
        .detach();
    }

    /// The transcript's `+N −M` chip, the pane half: that turn's `files`
    /// (absolute or relative to `root`) that are rows of the shown list
    /// open, the list scrolled to the first, the rows flashing once (docs/43
    /// §4). The shown list is the turn's own diff when the chip found its
    /// checkpoints ([`RightPane::open_turn`]), the working tree otherwise.
    pub(crate) fn open_turn_files(&mut self, root: &Path, files: &[String], cx: &mut Context<Self>) {
        let mut matched = Vec::new();
        for f in files {
            let rel = Path::new(f).strip_prefix(root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| f.clone());
            if let Some(change) = self.shown_change(root, &rel) {
                matched.push((rel, change));
            }
        }
        for (rel, change) in &matched {
            if !self.cache.changes_expanded_for(root).contains(rel) {
                self.toggle_change_row(root, change, cx);
            }
        }
        let matched: Vec<String> = matched.into_iter().map(|(rel, _)| rel).collect();
        let ui = self.changes_ui(root);
        ui.flash = Some((Instant::now(), matched.clone()));
        ui.scroll.set_offset(gpui::Point::default());
        cx.notify();
    }

    #[cfg(feature = "probe")]
    /// `tick:`'s pane half: a changed row's tick set. No tree, or a path
    /// that is not a changed row, is an `Err`.
    pub(crate) fn step_tick(&mut self, root: &Path, path: &str, on: bool, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let tree = self.cache.git_for(root).flatten().ok_or("no git tree read for the Changes pane")?;
        tree.change(path).ok_or_else(|| format!("no changed row `{path}`"))?;
        self.set_ticks(root, vec![path.to_owned()], on, cx);
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `changes-branch:`'s pane half: only a capture's fixture tree moves.
    pub(crate) fn step_fixture_branch(&mut self, root: &Path, name: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if !crate::runtime::captures() {
            return Err("only a capture's fixture tree can change branch".into());
        }
        if name.is_empty() {
            return Err("no branch name".into());
        }
        self.changes_ui(root).fixture_branch = Some(name.to_owned());
        self.refresh_now(cx);
        match self.cache.git_for(root).flatten() {
            Some(tree) if tree.branch == name => Ok(()),
            _ => Err("the fixture tree was not read: is the pane on Changes?".into()),
        }
    }

    #[cfg(feature = "probe")]
    /// `new-branch`'s pane half: the field open (idempotent). Not on the
    /// default branch, or a foot with no branch line, is an `Err`.
    pub(crate) fn step_new_branch(&mut self, root: &Path, prefix: String, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let tree = self.cache.git_for(root).flatten().ok_or("no git tree read for the Changes pane")?;
        let ui = self.changes_ui(root);
        if !on_default(&tree, ui) {
            return Err(format!("`{}` is not the default branch: New branch… is not offered", tree.branch));
        }
        if !matches!(foot_for(&tree, ui), Foot::Commit | Foot::Push { .. } | Foot::Level) {
            return Err("the foot has no branch line".into());
        }
        if ui.branch_field.is_none() {
            self.open_new_branch(root, prefix, window, cx);
        }
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `amend:`'s pane half (a setter). No changes, or Amend not offered,
    /// is an `Err`.
    pub(crate) fn step_amend(&mut self, root: &Path, on: bool, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let tree = self.cache.git_for(root).flatten().ok_or("no git tree read for the Changes pane")?;
        if tree.files.is_empty() {
            return Err("no changes: there is no commit box".into());
        }
        if !self.changes_ui(root).amendable {
            return Err("Amend is not offered: the last commit is on a remote".into());
        }
        self.set_amend(root, on, cx);
        Ok(())
    }

    #[cfg(feature = "probe")]
    /// `changes-open`'s pane half: open the changed row `path` (idempotent).
    /// Not a changed row is an `Err`; a row already open succeeds.
    pub(crate) fn open_change_row(&mut self, root: &Path, path: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let change = self.shown_change(root, path).ok_or_else(|| format!("no changed row `{path}`"))?;
        if !self.cache.changes_expanded_for(root).contains(path) {
            self.toggle_change_row(root, &change, cx);
        }
        Ok(())
    }
}

/// What a PR form's drafts need from the session (see
/// [`RightPane::open_pr_form_with`]).
pub(crate) struct PrDraftFacts {
    /// The session's title: the form's title and the prompt's subject.
    pub label: Option<String>,
    /// The session's lane provider (`None`: no session, no draft).
    pub provider: Option<crate::providers::ProviderId>,
    /// The utility model a one-shot completion runs on.
    pub model: Option<String>,
}

/// The Changes screen's host half: what needs the session, the layout, the
/// browser or the side-session wire.
impl Harness {
    /// The PR card's View: the PR in Browse (owner 2026-10-17).
    pub(crate) fn view_pull_request(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if url.is_empty() {
            return;
        }
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if let Err(reason) = self.open_new_tab(&url, window, cx) {
            crate::baaz_log!("view_pull_request: no tab opened: {reason}");
        }
    }

    /// Create PR (the pane's [`RightPaneEvent::OpenPrForm`]): the form opens
    /// with the session's title, and drafts on its provider.
    pub(crate) fn open_pr_form(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone());
        let draft = PrDraftFacts {
            label: session.as_ref().and_then(|id| self.collapsed_head_label(&self.chain_head(id, cx), cx)),
            provider: session.as_ref().and_then(|id| self.session_lane_provider(id, cx)),
            model: session.as_ref().and_then(|id| self.utility_model(id, cx)),
        };
        self.with_right_pane(cx, |pane, cx| pane.open_pr_form_with(root, draft, window, cx));
    }

    /// A change set the pane marked for a commit draft
    /// ([`RightPaneEvent::CommitDraftWanted`]): the prompt from the session's
    /// title and the files, run on the session's utility model — or through
    /// a hidden side session for a provider that completes there — landing
    /// in the pane ([`RightPane::land_draft`]).
    pub(crate) fn draft_commit_for_pane(&mut self, root: &Path, files: &[FileChange], cx: &mut Context<Self>) {
        let session = self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone());
        let Some((session, provider)) = session.and_then(|id| self.session_lane_provider(&id, cx).map(|provider| (id, provider))) else {
            self.right_pane.update(cx, |pane, cx| pane.land_draft(root, DraftKind::Commit, None, cx));
            return;
        };
        let title = self.collapsed_head_label(&self.chain_head(&session, cx), cx);
        let prompt = commit_prompt(title.as_deref(), files);
        if !crate::utility::supports(provider) {
            self.draft_on_muse(root, DraftKind::Commit, prompt, cx);
            return;
        }
        let root = root.to_path_buf();
        let model = self.utility_model(&session, cx);
        let pane = self.right_pane.downgrade();
        cx.spawn(async move |_, cx| {
            let reply = cx.background_executor().spawn(async move { crate::utility::complete(provider, model.as_deref(), &prompt, None) }).await;
            if std::env::var("BAAZ_DRAFT_TRACE").as_deref() == Ok("1") {
                eprintln!("baaz: draft reply {:?}", reply.as_ref().map(|t| t.chars().take(120).collect::<String>()));
            }
            let _ = pane.update(cx, |pane, cx| pane.land_draft(&root, DraftKind::Commit, reply.ok(), cx));
        })
        .detach();
    }

    /// The transcript's `+N −M` chip: Changes on that turn's own diff
    /// (Turn N, its two checkpoints; docs/82 6.3), or — for a turn with no
    /// checkpoint — the working tree, that turn's files opened, the list
    /// scrolled to the first, the rows flashing once (docs/43 §4).
    pub(crate) fn open_turn_changes(&mut self, turn_id: &str, files: Vec<String>, cx: &mut Context<Self>) {
        self.select_right(crate::layout::RightKind::Changes, cx);
        let Some((root, _)) = self.right_project(cx) else { return };
        self.with_right_pane(cx, |pane, cx| pane.open_turn(&root, turn_id, files, cx));
    }

    #[cfg(feature = "probe")]
    /// `changes-scope:<all|session|last>`: Changes on that scope. A capture
    /// reads no refs, so this loads the fixture index first (two finished
    /// turns over the fixture working tree). No project, no open session,
    /// an unknown word, or a scope the index cannot answer is an `Err`.
    pub(crate) fn step_changes_scope(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let scope = crate::right_scopes::ChangesScope::parse(rest.trim()).ok_or_else(|| format!("unknown scope `{}` (all, session, last)", rest.trim()))?;
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        if self.active.is_none() {
            return Err("no open session: Changes has no scopes".into());
        }
        self.select_right(crate::layout::RightKind::Changes, cx);
        self.with_right_pane(cx, |pane, cx| pane.step_scope(&root, scope, cx))
    }

    #[cfg(feature = "probe")]
    /// `tick:<path>` / `untick:<path>`: that row's commit tick (a setter).
    pub(crate) fn step_changes_tick(&mut self, rest: &str, on: bool, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        self.with_right_pane(cx, |pane, cx| pane.step_tick(&root, rest.trim(), on, cx))
    }

    #[cfg(feature = "probe")]
    /// `changes-branch:<name>`: the capture's fixture tree on that branch.
    pub(crate) fn step_changes_branch(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        self.with_right_pane(cx, |pane, cx| pane.step_fixture_branch(&root, rest.trim(), cx))
    }

    #[cfg(feature = "probe")]
    /// `new-branch`: New branch…'s field open on the Changes foot.
    pub(crate) fn step_new_branch(&mut self, window: &mut Window, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        let prefix = <crate::layout::Layout as crate::layout::LayoutGlobal>::get(cx).worktree_branch_prefix();
        self.with_right_pane(cx, |pane, cx| pane.step_new_branch(&root, prefix, window, cx))
    }

    #[cfg(feature = "probe")]
    /// `amend:<on|off>`: "Amend last commit" set.
    pub(crate) fn step_changes_amend(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let on = match rest.trim() {
            "on" => true,
            "off" => false,
            other => return Err(format!("`{other}` is not on or off")),
        };
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        self.with_right_pane(cx, |pane, cx| pane.step_amend(&root, on, cx))
    }

    #[cfg(feature = "probe")]
    /// `changes-open:<path>`: open that row's diff (idempotent), refreshing
    /// first so the tree is there. No project for the pane, or a path that
    /// is not a changed row, is an `Err`; a row already open succeeds.
    pub(crate) fn step_changes_open(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let path = rest.trim();
        let (root, _) = self.right_project(cx).ok_or("no project for the right pane")?;
        self.select_right(crate::layout::RightKind::Changes, cx);
        self.with_right_pane(cx, |pane, cx| pane.open_change_row(&root, path, cx))
    }
}

/// The prompt behind a drafted commit message.
fn commit_prompt(title: Option<&str>, files: &[FileChange]) -> String {
    let mut p = String::from(
        "Write a git commit message for the changes below. First line: an imperative summary, at most 60 characters. \
Then a blank line and at most three short lines on why. Use only the list given; do not run commands or call \
tools. Plain text only, no quotes, no markdown, no preamble.\n\n",
    );
    if let Some(t) = title {
        p.push_str(&format!("Session: {t}\n"));
    }
    p.push_str("Files:\n");
    for f in files.iter().take(40) {
        p.push_str(&format!("- {} (+{} -{})\n", f.path, f.added, f.removed));
    }
    p
}

/// The prompt behind a drafted PR description: what the branch adds over
/// its base — its commits and `diff --stat` — plus anything still
/// uncommitted. Live 2026-10-19: built from the working tree alone, a pushed
/// branch gave the model nothing and it answered with tool calls.
fn pr_prompt(title: Option<&str>, commits: &[String], stat: &[String], files: &[FileChange]) -> String {
    let mut p = String::from(
        "Write a pull request description for the branch summarised below: one short paragraph on what changed and why, \
then up to five bullet points. Use only the information given; do not run commands or call tools. Plain markdown, no \
heading, no preamble.\n\n",
    );
    if let Some(t) = title {
        p.push_str(&format!("Session: {t}\n\n"));
    }
    if !commits.is_empty() {
        p.push_str("Commits:\n");
        for c in commits {
            p.push_str(&format!("- {}\n", c.replace('\n', "\n  ")));
        }
        p.push('\n');
    }
    if !stat.is_empty() {
        p.push_str("Changed files:\n");
        for l in stat {
            p.push_str(&format!("{l}\n"));
        }
        p.push('\n');
    }
    if !files.is_empty() {
        p.push_str("Not yet committed:\n");
        for f in files.iter().take(40) {
            p.push_str(&format!("- {}\n", f.path));
        }
    }
    p
}

/// A model's draft without fences or wrapping quotes. Empty when the reply
/// is not a draft at all — a model that answered with tool calls.
pub(crate) fn clean_draft(text: &str) -> String {
    if ["<function_calls", "<invoke", "<parameter", "</invoke"].iter().any(|m| text.contains(m)) {
        return String::new();
    }
    let t = text.trim().trim_start_matches("```").trim_end_matches("```").trim();
    let t = t.strip_prefix('"').and_then(|t| t.strip_suffix('"')).unwrap_or(t);
    t.to_string()
}

// ─────────────────────────────────────────────────────────────── drawing

/// The Changes screen's body for `root` (under its row), or the pushed
/// Create PR view's row + body when the form stands open.
#[allow(clippy::too_many_arguments)]
pub(crate) fn changes_screen(
    cache: &crate::right::RightCache,
    root: &Path,
    tree: Option<Option<GitTree>>,
    session_edits: &[String],
    width: f32,
    agent: &str,
    base_menu: bool,
    window: &mut Window,
    cx: &mut Context<RightPane>,
) -> (Option<AnyElement>, AnyElement) {
    let Some(tree) = tree else {
        return (None, loading_body(cx).into_any_element());
    };
    let Some(tree) = tree else {
        return (
            None,
            state_block("right-changes-norepo", IconName::Git, "Not a git repository").line("Changes needs git. Files still shows the project.").into_any_element(),
        );
    };
    let root_buf = root.to_path_buf();
    let ui = &cache.changes;
    if ui.form.is_some() {
        return pr_form_view(ui, &root_buf, &tree, base_menu, window, cx);
    }
    if let (true, Some(Some(pr))) = (ui.pr_details, &ui.pr) {
        return crate::right_pr::pr_detail_view(pr, &root_buf, cache.line_select.can_quote, window, cx);
    }
    let expanded = cache.changes_expanded_for(root);
    let p = cx.aui().colors;
    // The scope row (docs/82 6.3): shown only over the open session's
    // checkpoints. Outside All changes the list is the scope's own diff, in
    // one section, with no commit box.
    let scopes = &ui.scopes;
    let scope_row = scopes.row(scopes.shown_session.as_deref()).map(|(labels, choices, active)| scope_row(&root_buf, labels, choices, active, cx));
    let in_all = scopes.scope == crate::right_scopes::ChangesScope::All || scope_row.is_none();
    let scoped = if in_all { None } else { Some(scopes.scoped.as_ref().filter(|list| list.scope == scopes.scope)) };
    if let Some(None) = scoped {
        let body = v_flex().size_full().children(scope_row).child(loading_body(cx));
        return (None, body.into_any_element());
    }
    let scoped = scoped.flatten();
    let scope_label = scopes.index.as_ref().map(|index| index.label(&scopes.scope)).unwrap_or_default();
    let (session, other) = match scoped {
        Some(_) => (Vec::new(), Vec::new()),
        None => crate::right::partition_session_files(&tree.files, session_edits, root),
    };
    let flash = ui.flash.as_ref().filter(|(at, _)| at.elapsed() < FLASH).map(|(at, files)| (1.0 - at.elapsed().as_secs_f32() / FLASH.as_secs_f32(), files.clone()));
    if flash.is_some() {
        aui_motion::request_frame(window);
    }
    let foot = foot_for(&tree, ui);
    let scroll = ui.scroll.clone();
    let row_tops = ui.row_tops.clone();
    let diffs = cache.file_diffs.clone();
    let gaps = cache.gap_lines.clone();
    // docs/82 6.4: what a row's "Revert" puts back — to HEAD in All
    // changes, the shown turn's part in Last turn / Turn N, nothing in This
    // session — and only while the open session can act (rule 13).
    let revert: Option<Option<(String, usize, String, String)>> = match scoped {
        None => scopes.revert.then_some(None),
        Some(_) => scopes.revert_turn().map(Some),
    };

    // docs/82 7.2: in All changes each row and section caption carries a
    // tick for the commit; the other scopes have no commit box.
    let ticked: Option<std::collections::HashSet<String>> = scoped.is_none().then(|| ticked_paths(&tree.files, session_edits, root, &ui.ticks).into_iter().collect());

    let mut list = row_list("right-changes-list").gap(Pixels::ZERO).label("Changes");
    let mut ix = 0usize;
    let mut sections: Vec<(SharedString, Vec<FileChange>)> = Vec::new();
    if let Some(list) = scoped {
        if !list.files.is_empty() {
            sections.push((scope_label.clone(), list.files.clone()));
        }
    } else {
        if !session.is_empty() || !session_edits.is_empty() {
            sections.push(("This session".into(), session));
        }
        if !other.is_empty() {
            sections.push(("Other changes".into(), other));
        }
    }
    let mut first = true;
    let mut opened: Vec<(String, AnyElement)> = Vec::new();
    for (label, files) in sections {
        let caption = changes_caption(SharedString::from(format!("right-changes-cap-{ix}")), label.clone(), Some(files.len()), first, cx);
        // An empty section ("This session 0") has nothing to tick: its
        // caption keeps the tick column empty.
        let caption: AnyElement = match &ticked {
            Some(_) if files.is_empty() => h_flex().w_full().items_end().child(tick_slot(div())).child(div().flex_1().min_w(px(0.0)).child(caption)).into_any_element(),
            Some(ticked) => {
                let paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
                let on = paths.iter().filter(|p| ticked.contains(*p)).count();
                let state = if on == 0 { CheckboxState::Off } else if on == paths.len() { CheckboxState::On } else { CheckboxState::Mixed };
                let r = root_buf.clone();
                let tick = cx.listener(move |this: &mut RightPane, intent: &CheckboxIntent, _, cx| {
                    let CheckboxIntent::Set(next) = intent;
                    this.set_ticks(&r, paths.clone(), next.checked(), cx);
                });
                h_flex()
                    .w_full()
                    .items_end()
                    .child(tick_slot(
                        checkbox(SharedString::from(format!("right-changes-tick-all-{ix}")), state, false)
                            .accessibility_label(format!("Commit every file in {label}"))
                            .on_intent(move |intent, window, cx| tick(&intent, window, cx)),
                    ).pb(px(scale::SP_2 + scale::SP_1)))
                    .child(div().flex_1().min_w(px(0.0)).child(caption))
                    .into_any_element()
            }
            None => caption.into_any_element(),
        };
        list = list.item_static(ix, caption);
        first = false;
        ix += 1;
        for change in files {
            let open = expanded.contains(&change.path);
            let flash_amount = flash.as_ref().filter(|(_, f)| f.contains(&change.path)).map(|(a, _)| *a).unwrap_or(0.0);
            let id = SharedString::from(format!("right-changes-file:{}", change.path));
            let toggle_root = root_buf.clone();
            let toggle_change = change.clone();
            let open_root = root_buf.clone();
            let open_abs = root.join(&change.path);
            // docs/82 7.5: the row's first hunk, when its diff is loaded, is
            // where "Open in …" lands.
            let first_line = diffs.get(&change.path).cloned().flatten().and_then(|diff| change_open_line(&diff));
            let open_label = crate::open_in::open_label(cx);
            let row_el = |id: SharedString, cx: &mut Context<RightPane>| {
                let (r1, c1, r2, a2) = (toggle_root.clone(), toggle_change.clone(), open_root.clone(), open_abs.clone());
                let mut row = change_row(id, change.clone(), open).width(px(width)).flash(flash_amount).on_toggle(cx.listener(move |this, _, _, cx| {
                    this.toggle_change_row(&r1, &c1, cx);
                }));
                if change.change != ChangeKind::Deleted {
                    row = row.on_open_file(cx.listener(move |_, _, _, cx| {
                        cx.emit(RightPaneEvent::Reveal { root: r2.clone(), abs: a2.clone() });
                    }));
                    let a3 = open_abs.clone();
                    row = row.on_open_in(open_label.clone(), move |_, _, cx| {
                        crate::open_in::open_path(&a3, first_line, cx);
                    });
                }
                row
            };
            // docs/82 6.4: "Revert" beside the row (the pane's quiet text
            // action), shown while it is hovered (its box always laid out, so the counts never move
            // under the pointer).
            let with_revert = |row: aui::workbench::ChangeRow, slot: &str, cx: &mut Context<RightPane>| -> AnyElement {
                let Some(turn) = revert.clone() else { return row.into_any_element() };
                use aui::interact::PaintHover as _;
                let (root, path) = (root_buf.clone(), change.path.clone());
                let button = text_action(SharedString::from(format!("right-changes-revert-{slot}:{}", change.path)), "Revert", None, scale::H_SM, cx)
                    .aria_label(format!("Revert {}", change.path))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.stop_propagation();
                        cx.emit(RightPaneEvent::RevertFile { root: root.clone(), path: path.clone(), turn: turn.clone() });
                    }));
                h_flex()
                    .w_full()
                    .items_center()
                    .hover_group()
                    .child(div().flex_1().min_w(px(0.0)).child(row))
                    .child(div().flex_none().reveal_on_hover(false).child(button))
                    .into_any_element()
            };
            // docs/82 7.2: the row's tick for the commit, before it.
            let with_tick = |row: AnyElement, slot: &str, cx: &mut Context<RightPane>| -> AnyElement {
                let Some(ticked) = &ticked else { return row };
                let on = ticked.contains(&change.path);
                let (r, path) = (root_buf.clone(), change.path.clone());
                let tick = cx.listener(move |this: &mut RightPane, intent: &CheckboxIntent, _, cx| {
                    let CheckboxIntent::Set(next) = intent;
                    this.set_ticks(&r, vec![path.clone()], next.checked(), cx);
                });
                h_flex()
                    .w_full()
                    .items_center()
                    .child(tick_slot(
                        checkbox(SharedString::from(format!("right-changes-tick-{slot}:{}", change.path)), if on { CheckboxState::On } else { CheckboxState::Off }, false)
                            .accessibility_label(format!("Commit {}", change.path))
                            .on_intent(move |intent, window, cx| tick(&intent, window, cx)),
                    ))
                    .child(div().flex_1().min_w(px(0.0)).child(row))
                    .into_any_element()
            };
            let row = with_tick(with_revert(row_el(id.clone(), cx), "row", cx), "row", cx);
            if open {
                opened.push((change.path.clone(), with_tick(with_revert(row_el(SharedString::from(format!("right-changes-sticky:{}", change.path)), cx), "sticky", cx), "sticky", cx)));
            }
            // `Some(None)`: read, but there is no text diff (a binary file).
            let unreadable = matches!(diffs.get(&change.path), Some(None));
            let diff = diffs.get(&change.path).cloned().flatten();
            let path = change.path.clone();
            let gap_root = root_buf.clone();
            let weak = cx.weak_entity();
            let gaps = gaps.clone();
            let muted = cx.aui().colors.fg_muted;
            // docs/82 7.1: the diff's lines select, for the quote bar.
            let target = crate::right_quote::diff_target(&root_buf, &change.path);
            let held = cache.line_select.range_for(&target);
            let geometry = cache.line_select.geometry.clone();
            let body = move || -> AnyElement {
                match diff {
                    Some(diff) => {
                        let d: Arc<Diff> = diff.clone();
                        let w3 = weak.clone();
                        let mut b = diff_body(SharedString::from(format!("right-diff:{path}")), d.clone()).selected(held).selection_geometry(&geometry).on_select_lines(move |range, window, cx| {
                            let _ = w3.update(cx, |this: &mut RightPane, cx| this.select_lines(target.clone(), range, window, cx));
                        });
                        for i in 0..d.hunks.len() {
                            if let Some(lines) = gaps.get(&(path.clone(), i)) {
                                b = b.open_gap(i, lines.clone());
                            }
                        }
                        let (p2, r2, w2) = (path.clone(), gap_root.clone(), weak.clone());
                        b.on_gap(move |i, _, cx| {
                            let range = unchanged_between(&d, i);
                            let _ = w2.update(cx, |this, cx| this.expand_diff_gap(&r2, p2.clone(), i, range, cx));
                        })
                        .into_any_element()
                    }
                    // Live 2026-10-19: a binary file opened onto an empty box.
                    None if unreadable => div()
                        .h(px(scale::H_MD))
                        .px(px(scale::SP_6))
                        .flex()
                        .items_center()
                        .text_role(TextRole::Body)
                        .text_color(muted)
                        .child("Binary file — no text diff.")
                        .into_any_element(),
                    None => div().h(px(scale::H_SM)).into_any_element(),
                }
            };
            // The row and its diff are two list entries: the diff is a
            // toolbar, so the pointer over it lights nothing (owner
            // 2026-10-21: one item lit the row and every hunk). The sticky
            // test still needs the pair's top and joint height: the row
            // records its top, the diff extends the height to its foot.
            let (tops, key, at) = (row_tops.clone(), change.path.clone(), scroll.clone());
            let row_wrap = div().w_full().on_prepaint(move |b, _, _| {
                // Kept relative to the scroll content, so the sticky
                // test can use the offset as it is now.
                let content_y = f32::from(b.origin.y - at.bounds().origin.y - at.offset().y);
                if std::env::var("BAAZ_STICKY_TRACE").as_deref() == Ok("1") {
                    eprintln!("baaz: sticky prepaint {key} origin={} bounds={} offset={} content_y={content_y} h={}", f32::from(b.origin.y), f32::from(at.bounds().origin.y), f32::from(at.offset().y), f32::from(b.size.height));
                }
                tops.borrow_mut().insert(key.clone(), (content_y, f32::from(b.size.height)));
            });
            list = list.item(ix, row_wrap.child(row));
            ix += 1;
            if let Some(fold) = diff_fold(SharedString::from(format!("right-changes-fold:{}", change.path)), open, body, window, cx) {
                let (tops, key, at) = (row_tops.clone(), change.path.clone(), scroll.clone());
                let wrap = div().w_full().on_prepaint(move |b, _, _| {
                    let foot = f32::from(b.origin.y + b.size.height - at.bounds().origin.y - at.offset().y);
                    if let Some(entry) = tops.borrow_mut().get_mut(&key) {
                        entry.1 = foot - entry.0;
                    }
                });
                list = list.item_static(ix, wrap.child(fold));
                ix += 1;
            }
        }
    }

    // The commit box, Push and the PR act on the working tree: All changes
    // only (rule 13).
    let n_ticked = ticked.as_ref().map_or(0, |t| t.len());
    let foot_el = scoped.is_none().then(|| foot_element(ui, &root_buf, &tree, n_ticked, foot, agent, cache.line_select.can_quote, window, cx));
    let clean = match scoped {
        Some(list) => list.files.is_empty(),
        None => tree.files.is_empty(),
    };
    let clean_line = match &scopes.scope {
        _ if scoped.is_none() => "The working tree is clean.",
        crate::right_scopes::ChangesScope::Session => "Nothing has changed since this session's first turn.",
        _ => "The turn changed no files.",
    };
    let body = if clean {
        div().size_full().pb(px(CLEAN_LIFT)).child(state_block("right-changes-clean", IconName::Check, "No changes").line(clean_line)).into_any_element()
    } else {
        div()
            .id("right-changes-scroll")
            .size_full()
            .overflow_y_scroll()
            .scroll_axis_locked()
            .track_scroll(&scroll)
            // The sticky row is decided in render: a scroll re-renders, after
            // the wheel has moved the offset (live 2026-10-19: it never stuck).
            .on_scroll(cx.listener(|_: &mut RightPane, _: &gpui::ScrollEvent, _, cx| cx.notify()))
            .pt(px(scale::SP_2))
            .px(px(LIST_INSET))
            .pb(px(FOOT_CLEARANCE))
            .child(list)
            .into_any_element()
    };
    // The sticky row: an open file whose row scrolled under the tab row
    // while its diff still shows sticks at the top (docs/43 §2.6).
    let sticky = (!clean)
        .then(|| {
            let offset = f32::from(scroll.offset().y);
            let tops = row_tops.borrow();
            opened.into_iter().find_map(|(path, el)| {
                let (content_y, h) = *tops.get(&path)?;
                let rel = content_y + offset;
                if std::env::var("BAAZ_STICKY_TRACE").as_deref() == Ok("1") {
                    eprintln!("baaz: sticky render {path} content_y={content_y} offset={offset} rel={rel} h={h}");
                }
                (rel < 0.0 && rel + h > 64.0).then_some(el)
            })
        })
        .flatten();
    // docs/82 7.1: the quote bar over a selection in one of these diffs;
    // a press anywhere else in the screen, or Esc, clears it.
    let held_here = cache.line_select.held.as_ref().is_some_and(|(t, _)| matches!(t, crate::right_quote::LineTarget::Diff { root: r, .. } if *r == root_buf));
    let bar = match &cache.line_select.held {
        Some((target @ crate::right_quote::LineTarget::Diff { path, .. }, _)) if held_here => {
            let shown = expanded.contains(path) && matches!(diffs.get(path), Some(Some(_)));
            crate::right_quote::screen_bar(cache, &target.clone(), shown, window, cx)
        }
        _ => None,
    };
    let body_cell = cache.line_select.body.clone();
    let mut screen = div()
        .id("right-changes-screen")
        .relative()
        .size_full()
        .bg(p.surface_1)
        .when_some(cache.line_select.focus.clone(), |d, f| d.track_focus(&f))
        .on_key_down(cx.listener(|this: &mut RightPane, e: &gpui::KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" && this.clear_line_selection(cx) {
                cx.stop_propagation();
            }
        }))
        .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this: &mut RightPane, _: &gpui::MouseDownEvent, _, cx| {
            this.clear_line_selection(cx);
        }))
        .when(held_here, |d| d.on_prepaint(move |bounds, _, _| body_cell.set(Some(bounds))))
        .child(body);
    if let Some(row) = sticky {
        screen = screen.child(div().absolute().top_0().left(px(LIST_INSET)).right(px(LIST_INSET)).bg(p.surface_1).child(row));
    }
    if let Some(foot_el) = foot_el {
        screen = screen.child(commit_foot(foot_el, cx));
    }
    let screen = screen.children(bar);
    match scope_row {
        Some(row) => (None, v_flex().size_full().bg(p.surface_1).child(row).child(div().flex_1().min_h(px(0.0)).child(screen)).into_any_element()),
        None => (None, screen.into_any_element()),
    }
}

/// The scope row over the Changes list (docs/82 6.3, docs/83 §6): a quiet
/// row of text tabs — Body, muted at rest, the chosen one on the
/// `ov-selected` pill, the others hovering — each a `tab` named by its
/// label, the list named "Changes scope".
fn scope_row(root: &Path, labels: Vec<SharedString>, choices: Vec<crate::right_scopes::ChangesScope>, active: usize, cx: &mut Context<RightPane>) -> AnyElement {
    let root = root.to_path_buf();
    let pick = cx.listener(move |this: &mut RightPane, ix: &usize, _, cx| {
        if let Some(scope) = choices.get(*ix).cloned() {
            if let Err(reason) = this.set_scope(&root, scope, cx) {
                crate::baaz_log!("changes: scope not changed ({reason})");
            }
        }
    });
    h_flex()
        .id("right-changes-scope")
        .flex_none()
        .h(aui_tokens::Metrics::active().control)
        .px(px(LIST_INSET))
        .items_center()
        .child(aui::data::tabs("right-changes-scope-tabs", "Changes scope", labels, active).on_select(move |ix, window, cx| pick(&ix, window, cx)))
        .into_any_element()
}

/// The tick's column before a row or caption: the box centred in a
/// `SP_6` slot.
fn tick_slot(tick: impl IntoElement) -> gpui::Div {
    h_flex().flex_none().w(px(scale::SP_6)).justify_center().child(tick)
}

fn loading_body(cx: &Context<RightPane>) -> gpui::Div {
    v_flex().pt(px(scale::SP_2)).px(px(LIST_INSET)).child(changes_caption("right-changes-loading-cap", "This session", None, true, cx)).children(SKELETON_WIDTHS.iter().map(|w| {
        h_flex()
            .h(aui_tokens::Metrics::active().control)
            .gap(px(scale::SP_3))
            .pl(px(scale::SP_6))
            .pr(px(scale::SP_3))
            .items_center()
            .child(skeleton_bar(px(SKELETON_KIND_W), cx))
            .child(skeleton_bar(px(*w), cx))
            .child(div().flex_1())
            .child(skeleton_bar(px(SKELETON_STAT_W), cx))
    }))
}

/// The commit box (or the hook note over it, or the PR card). `ticked`:
/// how many files Commit takes. The branch line carries New branch… on the
/// default branch, Fetch and Pull where a remote exists (docs/82 7.2).
#[allow(clippy::too_many_arguments)]
fn foot_element(ui: &ChangesUi, root: &Path, tree: &GitTree, ticked: usize, foot: Foot, agent: &str, can_act: bool, window: &mut Window, cx: &mut Context<RightPane>) -> AnyElement {
    let busy = ui.busy;
    let hook = ui.hook_failure.clone();
    let editor = ui.editor.clone();
    let message = ui.message.clone();
    let drafting = ui.drafting;
    let root_buf = root.to_path_buf();
    let default = on_default(tree, ui);
    // Pull carries how far behind the branch is: the branch's own suffix
    // then leaves it out, so the line fits a narrow pane.
    let pull = pull_offered(ui, tree);
    let behind = if pull { 0 } else { tree.behind };
    let level = (!pull).then(|| level_note(tree.behind));
    match foot {
        Foot::Commit => {
            let amend = ui.amend && ui.amendable;
            let label = commit_label(ticked, amend, busy == Some(GitBusy::Committing));
            let editing = editor.is_some();
            let focused = editor.as_ref().is_some_and(|e| gpui::Focusable::focus_handle(e.read(cx), cx).is_focused(window));
            let message_el: AnyElement = match editor {
                Some(editor) => {
                    let r1 = root_buf.clone();
                    let r2 = root_buf.clone();
                    div()
                        .id("right-commit-editor")
                        .min_h(px(scale::BAR_DEFAULT))
                        .px(px(scale::SP_2))
                        .on_action(cx.listener(move |this, _: &crate::app::CommitMessage, _, cx| this.commit_changes(&r1, cx)))
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.settle_commit_message(&r2, cx)))
                        .key_context("CommitMessage")
                        .child(Textarea::new(&editor).appearance(false).bordered(false).role_size(TextRole::Body).line_height(px(EDITOR_LH)))
                        .into_any_element()
                }
                None => {
                    let r = root_buf.clone();
                    let shown = if drafting && message.is_empty() { String::new() } else { message.clone() };
                    commit_message_view("right-commit-message", &shown, cx).on_click(cx.listener(move |this, _, window, cx| this.edit_commit_message(&r, window, cx))).into_any_element()
                }
            };
            // Nothing ticked commits nothing; Amend with nothing ticked
            // rewrites the message alone.
            let disabled = (message.trim().is_empty() && !editing) || (ticked == 0 && !amend);
            let r = root_buf.clone();
            let commit = primary_action("right-commit", label, None, busy == Some(GitBusy::Committing), disabled || busy.is_some(), cx)
                .when(!disabled && busy.is_none(), |b| b.on_click(cx.listener(move |this, _, _, cx| this.commit_changes(&r, cx))));
            let amend_el = ui.amendable.then(|| amend_row(amend, &root_buf, cx));
            let line = branch_line(ui, &root_buf, tree, ahead_behind(tree.ahead, behind), default, Some(commit.into_any_element()), window, cx);
            let box_el = commit_frame(focused, cx).child(message_el).children(amend_el).child(line);
            match hook {
                Some(lines) => {
                    let who = agent;
                    let r1 = root_buf.clone();
                    let l1 = lines.clone();
                    let l2 = lines.clone();
                    let actions = vec![
                        text_action("right-hook-output", "Show full output", None, scale::H_SM, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.show_hook_output(&r1, l1.clone(), cx)))
                            .into_any_element(),
                        text_action("right-hook-ask", format!("Ask {who} to fix it"), None, scale::H_SM, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.ask_to_fix_hook(&l2, cx)))
                            .into_any_element(),
                    ];
                    v_flex().child(hook_failed_note("right-hook", "pre-commit hook failed", &lines, actions, cx)).child(box_el).into_any_element()
                }
                None => box_el.into_any_element(),
            }
        }
        Foot::Push { ahead } => {
            let r = root_buf.clone();
            let suffix = format!("· {ahead} commit{} not pushed", if ahead == 1 { "" } else { "s" });
            // On the default branch the foot leads with New branch… and
            // Push names where it goes: never a silent push to the default
            // branch (docs/82 7.2).
            let primary = if default {
                h_flex()
                    .gap(px(scale::SP_2))
                    .items_center()
                    .child(
                        text_action("right-push", format!("Push to {}", tree.branch), None, scale::H_SM, cx)
                            .when(busy.is_none(), |b| b.on_click(cx.listener(move |this, _, _, cx| this.push_branch(&r, cx)))),
                    )
                    .child(new_branch_action(&root_buf, true, busy, cx))
                    .into_any_element()
            } else {
                primary_action("right-push", "Push", None, busy == Some(GitBusy::Pushing), busy.is_some(), cx).on_click(cx.listener(move |this, _, _, cx| this.push_branch(&r, cx))).into_any_element()
            };
            commit_frame(false, cx).child(branch_line(ui, &root_buf, tree, Some(suffix.into()), false, Some(primary), window, cx)).into_any_element()
        }
        Foot::CreatePr => {
            let r = root_buf.clone();
            let create = primary_action("right-create-pr", "Create PR", Some(IconName::PullRequest), false, busy.is_some(), cx).on_click(cx.listener(move |_, _, _, cx| cx.emit(RightPaneEvent::OpenPrForm(r.clone()))));
            commit_frame(false, cx).child(branch_line(ui, &root_buf, tree, level, false, Some(create.into_any_element()), window, cx)).into_any_element()
        }
        Foot::Level => commit_frame(false, cx).child(branch_line(ui, &root_buf, tree, level, default, None, window, cx)).into_any_element(),
        Foot::Pr(pr) => crate::right_pr::pr_card_element(&pr, root, false, can_act, cx),
    }
}

/// The foot's branch line: the branch and `suffix`, then New branch… (a
/// quiet action when `new_branch`), Fetch and — behind the upstream —
/// Pull where a remote exists, then `primary` at the right. While New
/// branch…'s field stands open, the field takes the line.
#[allow(clippy::too_many_arguments)]
fn branch_line(ui: &ChangesUi, root: &Path, tree: &GitTree, suffix: Option<SharedString>, new_branch: bool, primary: Option<AnyElement>, window: &mut Window, cx: &mut Context<RightPane>) -> AnyElement {
    if let Some(field) = ui.branch_field.clone() {
        return branch_field_row(field, root, ui.busy, window, cx);
    }
    let busy = ui.busy;
    // The quiet actions carry their own padding: no gap between them, so
    // the branch and its counts keep their room in a narrow pane.
    let mut line = h_flex().h(px(scale::H_MD)).items_center().child(branch_label(&tree.branch, suffix, cx).mr(px(scale::SP_2)));
    if new_branch {
        line = line.child(new_branch_action(root, false, busy, cx));
    }
    if ui.has_remote || tree.upstream.is_some() {
        let r = root.to_path_buf();
        let fetch_label = if busy == Some(GitBusy::Fetching) { "Fetching…" } else { "Fetch" };
        line = line.child(
            text_action("right-changes-fetch", fetch_label, None, scale::H_SM, cx)
                .aria_label("Fetch from origin")
                .when(busy.is_none(), |b| b.on_click(cx.listener(move |this, _, _, cx| this.sync_remote(&r, false, cx)))),
        );
        if pull_offered(ui, tree) {
            let r = root.to_path_buf();
            let pull_label = if busy == Some(GitBusy::Pulling) { "Pulling…".to_owned() } else { format!("Pull ↓{}", tree.behind) };
            line = line.child(
                text_action("right-changes-pull", pull_label, None, scale::H_SM, cx)
                    .aria_label(format!("Pull {} commit{} (fast-forward only)", tree.behind, if tree.behind == 1 { "" } else { "s" }))
                    .when(busy.is_none(), |b| b.on_click(cx.listener(move |this, _, _, cx| this.sync_remote(&r, true, cx)))),
            );
        }
    }
    line.child(div().flex_1().min_w(px(scale::SP_2))).children(primary).into_any_element()
}

/// Whether the branch line offers Pull: a remote, an upstream, and the
/// branch behind it.
fn pull_offered(ui: &ChangesUi, tree: &GitTree) -> bool {
    (ui.has_remote || tree.upstream.is_some()) && tree.upstream.is_some() && tree.behind > 0
}

/// "New branch…": a quiet action on the branch line, or the foot's primary
/// (`primary`) where a push would land on the default branch.
fn new_branch_action(root: &Path, primary: bool, busy: Option<GitBusy>, cx: &mut Context<RightPane>) -> AnyElement {
    let r = root.to_path_buf();
    let open = cx.listener(move |this: &mut RightPane, _: &gpui::ClickEvent, window, cx| {
        let prefix = <crate::layout::Layout as crate::layout::LayoutGlobal>::get(cx).worktree_branch_prefix();
        this.open_new_branch(&r, prefix, window, cx);
    });
    if primary {
        primary_action("right-new-branch-open", "New branch…", Some(IconName::Branch), false, busy.is_some(), cx).when(busy.is_none(), |b| b.on_click(open)).into_any_element()
    } else {
        text_action("right-new-branch-open", "New branch…", None, scale::H_SM, cx).when(busy.is_none(), |b| b.on_click(open)).into_any_element()
    }
}

/// New branch…'s name field on the branch line: ⏎ creates the branch, Esc
/// (or Cancel) closes it. The keys are caught before the input sees them.
fn branch_field_row(field: Entity<InputState>, root: &Path, busy: Option<GitBusy>, window: &mut Window, cx: &mut Context<RightPane>) -> AnyElement {
    let focused = gpui::Focusable::focus_handle(field.read(cx), cx).is_focused(window);
    let (keys_root, cancel_root) = (root.to_path_buf(), root.to_path_buf());
    let keys = cx.listener(move |this: &mut RightPane, e: &gpui::KeyDownEvent, _, cx| match e.keystroke.key.as_str() {
        "enter" => {
            cx.stop_propagation();
            this.create_new_branch(&keys_root, cx);
        }
        "escape" => {
            cx.stop_propagation();
            this.cancel_new_branch(&keys_root, cx);
        }
        _ => {}
    });
    let input = gpui_kit::base::input::Input::new(&field);
    h_flex()
        .gap(px(scale::SP_2))
        .items_center()
        .child(div().id("right-new-branch").flex_1().min_w(px(0.0)).capture_key_down(keys).child(well_input(input, scale::SP_7, focused, cx)))
        .child(
            text_action("right-new-branch-cancel", if busy == Some(GitBusy::Branching) { "Creating…" } else { "Cancel" }, None, scale::H_SM, cx)
                .aria_label("Cancel the new branch")
                .on_click(cx.listener(move |this, _, _, cx| this.cancel_new_branch(&cancel_root, cx))),
        )
        .into_any_element()
}

/// "Amend last commit" in the commit box, shown only while HEAD is not on
/// a remote ([`git_ops::head_amendable`]).
fn amend_row(on: bool, root: &Path, cx: &mut Context<RightPane>) -> AnyElement {
    let r = root.to_path_buf();
    let flip = cx.listener(move |this: &mut RightPane, intent: &CheckboxIntent, _, cx| {
        let CheckboxIntent::Set(next) = intent;
        this.set_amend(&r, next.checked(), cx);
    });
    let p = cx.aui().colors;
    h_flex()
        .h(px(scale::H_SM))
        .px(px(scale::SP_2))
        .gap(px(scale::SP_3))
        .items_center()
        .text_role(TextRole::Body)
        .text_color(p.fg_muted)
        .child(
            checkbox("right-commit-amend", if on { CheckboxState::On } else { CheckboxState::Off }, false)
                .accessibility_label("Amend last commit")
                .on_intent(move |intent, window, cx| flip(&intent, window, cx)),
        )
        .child("Amend last commit")
        .into_any_element()
}

/// The Create PR pushed view: its row and its body (docs/43 §2.6). `menu`:
/// the base-branch menu stands open (an entry on the overlay stack).
fn pr_form_view(ui: &ChangesUi, root: &Path, tree: &GitTree, menu: bool, window: &mut Window, cx: &mut Context<RightPane>) -> (Option<AnyElement>, AnyElement) {
    let busy = ui.busy;
    let form = ui.form.as_ref().expect("the form stands open");
    let (base, bases, title, body, draft, drafting) = (form.base.clone(), form.bases.clone(), form.title.clone(), form.body.clone(), form.draft, form.drafting);
    let p = cx.aui().colors;
    let back_root = root.to_path_buf();
    let row = pushed_row("right-pr-row", "Create pull request")
        .back_label("Back to changes")
        .on_back(cx.listener(move |this, _, _, cx| {
            this.close_pr_form(&back_root, cx);
        }))
        .into_any_element();
    let title_focused = gpui::Focusable::focus_handle(title.read(cx), cx).is_focused(window);
    let body_focused = gpui::Focusable::focus_handle(body.read(cx), cx).is_focused(window);
    let mut picker = div().relative().child(base_picker("right-pr-base", base.clone(), cx).on_click(cx.listener(move |this, _, _, cx| {
        this.set_base_menu(!menu, cx);
    })));
    if menu && !bases.is_empty() {
        // The base branches: the library's menu, under the picker.
        let mut items = dropdown("right-pr-base-menu", "Base branch").min_width(px(BASE_MENU_MIN_W));
        for b in bases {
            let pick_root = root.to_path_buf();
            let value = b.clone();
            let weak = cx.weak_entity();
            items = items.entry(DropdownItem::new(b.clone()).current(b == base).on_select(move |_, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if let Some(f) = this.changes_ui(&pick_root).form.as_mut() {
                        f.base = value.clone();
                    }
                    this.set_base_menu(false, cx);
                });
            }));
        }
        picker = picker.child(div().absolute().top_full().left_0().mt(px(scale::SP_2)).child(items));
    }
    let draft_root = root.to_path_buf();
    let body_el = v_flex()
        .id("right-pr-form")
        .size_full()
        .overflow_y_scroll()
        .scroll_axis_locked()
        .p(px(scale::SP_4))
        .pb(px(FORM_FOOT_CLEARANCE))
        .gap(px(scale::SP_4))
        .child(form_field("Into", base_and_head(picker, &tree.branch, cx), cx))
        .child(form_field("Title", well_input(gpui_kit::base::input::Input::new(&title), 32.0, title_focused, cx), cx))
        .child(form_field(
            "Description",
            well_input(
                v_flex()
                    .child(Textarea::new(&body).appearance(false).bordered(false).role_size(TextRole::Body).line_height(px(EDITOR_LH)))
                    .child(div().mt(px(scale::SP_3)).text_color(p.fg_muted).child(if drafting { "Drafting from this session…" } else { "Drafted from this session · edit freely" })),
                150.0,
                body_focused,
                cx,
            ),
            cx,
        ))
        .child(draft_checkbox("right-pr-draft", draft, cx).on_click(cx.listener(move |this, _, _, cx| {
            this.update_changes(
                &draft_root,
                |ui| {
                    if let Some(f) = ui.form.as_mut() {
                        f.draft = !f.draft;
                    }
                },
                cx,
            );
        })));
    let cancel_root = root.to_path_buf();
    let create_root = root.to_path_buf();
    let foot = h_flex()
        .justify_end()
        .gap(px(scale::SP_2))
        .px(px(scale::SP_2))
        .child(text_action("right-pr-cancel", "Cancel", None, 28.0, cx).on_click(cx.listener(move |this, _, _, cx| {
            this.close_pr_form(&cancel_root, cx);
        })))
        .child(
            primary_action("right-pr-create", "Create PR", Some(IconName::PullRequest), busy == Some(GitBusy::CreatingPr), busy.is_some(), cx)
                .on_click(cx.listener(move |this, _, _, cx| this.create_pull_request(&create_root, cx))),
        );
    let screen = div().relative().size_full().child(body_el).child(commit_foot(foot, cx));
    (Some(row), screen.into_any_element())
}

/// The line a Changes row's "Open in …" lands on (docs/82 7.5): the new
/// side's start of the diff's first hunk; `None` opens at the top.
pub(crate) fn change_open_line(diff: &aui_protocol::Diff) -> Option<u32> {
    diff.hunks.first().and_then(|hunk| aui::workbench::hunk_new_start(&hunk.header)).filter(|n| *n > 0)
}

#[cfg(test)]
mod tests {
    /// docs/82 7.2: the default ticks are the session's own files when it
    /// touched any (docs/81 F E3), else every file; the person's ticks
    /// override per path; Commit takes exactly the ticked paths.
    #[test]
    fn ticks_default_to_the_sessions_files_and_the_persons_ticks_win() {
        let files = vec![change("src/a.rs"), change("src/b.rs"), change("README.md")];
        let root = Path::new("/repo");
        let mut ticks = std::collections::HashMap::new();
        assert_eq!(ticked_paths(&files, &["src/a.rs".to_owned()], root, &ticks), vec!["src/a.rs".to_owned()], "the session's own");
        assert_eq!(ticked_paths(&files, &[], root, &ticks).len(), 3, "no session edit: every file");
        // The person ticks another file and unticks the session's.
        ticks.insert("README.md".to_owned(), true);
        ticks.insert("src/a.rs".to_owned(), false);
        assert_eq!(ticked_paths(&files, &["src/a.rs".to_owned()], root, &ticks), vec!["README.md".to_owned()]);
        // Without a session edit the unticked one stays out, the rest default in.
        assert_eq!(ticked_paths(&files, &[], root, &ticks), vec!["src/b.rs".to_owned(), "README.md".to_owned()]);
        // A path that leaves the tree drops its tick; back, it takes the default.
        let mut kept = ticks.clone();
        keep_ticks_in(&mut kept, &files[1..]);
        assert!(!kept.contains_key("src/a.rs") && kept.contains_key("README.md"));
        assert_eq!(ticked_paths(&files, &["src/a.rs".to_owned()], root, &kept), vec!["src/a.rs".to_owned(), "README.md".to_owned()]);
        // Nothing ticked: Commit takes nothing (its button is disabled).
        for f in &files {
            ticks.insert(f.path.clone(), false);
        }
        assert!(ticked_paths(&files, &[], root, &ticks).is_empty());
    }

    #[test]
    fn the_button_names_what_it_does() {
        assert_eq!(commit_label(1, false, false), "Commit 1 file");
        assert_eq!(commit_label(4, false, false), "Commit 4 files");
        assert_eq!(commit_label(0, false, false), "Commit 0 files");
        assert_eq!(commit_label(4, true, false), "Amend");
        assert_eq!((commit_label(4, false, true).as_str(), commit_label(4, true, true).as_str()), ("Committing…", "Amending…"));
    }

    /// New branch… shows on the default branch and on a detached HEAD only.
    #[test]
    fn new_branch_is_offered_on_the_default_branch() {
        let ui = ChangesUi { default_branch: Some("main".into()), ..ChangesUi::default() };
        assert!(on_default(&tree(vec![], "main", Some("origin/main"), 0), &ui));
        assert!(on_default(&tree(vec![], "HEAD", None, 0), &ui));
        assert!(!on_default(&tree(vec![], "baaz/x", None, 0), &ui));
    }

    /// Unticking Amend gives back the message it replaced, unless the
    /// person edited it since.
    #[test]
    fn unticking_amend_puts_the_message_back_if_untouched() {
        let mut ui = ChangesUi { amend: true, message: "HEAD's".into(), amend_saved: Some("draft".into()), ..ChangesUi::default() };
        untick_amend(&mut ui);
        assert_eq!((ui.amend, ui.message.as_str(), ui.amend_saved.is_none()), (false, "draft", true));
        let mut ui = ChangesUi { amend: true, message: "mine".into(), message_edited: true, amend_saved: Some("draft".into()), ..ChangesUi::default() };
        untick_amend(&mut ui);
        assert_eq!(ui.message, "mine");
    }

    use super::*;

    fn tree(files: Vec<FileChange>, branch: &str, upstream: Option<&str>, ahead: u32) -> GitTree {
        GitTree { files, branch: branch.into(), upstream: upstream.map(Into::into), ahead, ..GitTree::default() }
    }

    fn change(path: &str) -> FileChange {
        FileChange { path: path.into(), change: ChangeKind::Modified, added: 1, removed: 0 }
    }

    #[test]
    fn the_foot_follows_the_branch() {
        let mut ui = ChangesUi { default_branch: Some("main".into()), ..ChangesUi::default() };
        assert_eq!(foot_for(&tree(vec![change("a")], "f", Some("origin/f"), 0), &ui), Foot::Commit);
        assert_eq!(foot_for(&tree(vec![], "f", Some("origin/f"), 2), &ui), Foot::Push { ahead: 2 });
        assert_eq!(foot_for(&tree(vec![], "f", Some("origin/f"), 0), &ui), Foot::CreatePr);
        assert_eq!(foot_for(&tree(vec![], "main", Some("origin/main"), 0), &ui), Foot::Level);
        ui.pr = Some(Some(git_ops::pr_fixture("passing").expect("recorded")));
        assert!(matches!(foot_for(&tree(vec![], "f", Some("origin/f"), 0), &ui), Foot::Pr(_)));
        // A branch never pushed: Push, counted against the default branch.
        let fresh = ChangesUi { default_branch: Some("main".into()), unpushed_without_upstream: Some(3), ..ChangesUi::default() };
        assert_eq!(foot_for(&tree(vec![], "f", None, 0), &fresh), Foot::Push { ahead: 3 });
    }

    #[test]
    fn a_draft_loses_its_fences_and_quotes() {
        assert_eq!(clean_draft("```\nFix the thing\n```"), "Fix the thing");
        assert_eq!(clean_draft("\"Fix it\""), "Fix it");
        assert_eq!(clean_draft("I need to look first.\n<function_calls>\n<invoke name=\"bash\">"), "", "a tool call is no draft");
    }

    #[test]
    fn a_rows_open_in_lands_on_its_first_hunk() {
        let hunk = |header: &str| aui_protocol::Hunk { header: header.into(), lines: Vec::new() };
        let diff = |hunks| aui_protocol::Diff { path: "a.rs".into(), hunks, added: 1, removed: 0 };
        assert_eq!(super::change_open_line(&diff(vec![hunk("@@ -40,5 +42,7 @@ fn x"), hunk("@@ -90 +94 @@")])), Some(42));
        assert_eq!(super::change_open_line(&diff(Vec::new())), None, "no hunk: the top");
        assert_eq!(super::change_open_line(&diff(vec![hunk("@@ -0,0 +0,0 @@")])), None);
    }

    #[test]
    fn one_change_set_hashes_the_same_twice() {
        let a = vec![change("a"), change("b")];
        assert_eq!(change_set_key(&a), change_set_key(&a.clone()));
        assert_ne!(change_set_key(&a), change_set_key(&[change("a")]));
    }
}
