//! D6's Changes screen in Baaz (docs/43 §2.6, §4): this session's files and
//! the rest of the working tree, each row opening its diff in place; the
//! commit box at the foot whose single primary follows the branch — Commit,
//! Push, Create PR, then the PR card; Create PR as a pushed view. The git
//! itself is [`crate::git_ops`]; this module owns the screen's state, its
//! actions and its drawing. Nothing here touches the disk on the render path.

use aui::util::ScrollAxisLock as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aui::interact::{fluid_list, Axis};
use aui::protocol::{ChangeKind, Diff, FileChange};
use aui::workbench::{
    ahead_behind, base_and_head, base_picker, branch_label, change_row, changes_caption, diff_fold, commit_foot, commit_frame,
    commit_message_view, diff_body, draft_checkbox, form_field, hook_failed_note, pr_card, primary_action, pushed_row, skeleton_bar,
    state_block, text_action, unchanged_between, well_input,
};
use aui_icons::IconName;
use aui_tokens::{ActiveAui, AuiStyled};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, IntoElement, ScrollHandle, SharedString, Window};
use gpui_kit::base::input::{InputState, TextareaState};
use gpui_kit::base::ElementExt as _;
use gpui_kit::component::input::Textarea;
use gpui_kit::base::{h_flex, v_flex};

use crate::app::Harness;
use crate::git_ops::{self, CommitOutcome, GitTree, PullRequest};

/// What the commit box is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitBusy {
    Committing,
    Pushing,
    CreatingPr,
}

/// The Create PR form (a pushed view on Changes).
pub(crate) struct PrForm {
    pub base: String,
    pub bases: Vec<String>,
    pub title: Entity<InputState>,
    pub body: Entity<TextareaState>,
    pub draft: bool,
    pub base_menu: bool,
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
    /// `origin/HEAD`'s branch, read once per root.
    pub default_branch: Option<String>,
    /// Commits the remote lacks when the branch has no upstream.
    pub unpushed_without_upstream: Option<u32>,
    pub form: Option<PrForm>,
    /// The transcript's chip opened these files: they flash 9 % once.
    pub flash: Option<(Instant, Vec<String>)>,
    pub scroll: ScrollHandle,
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

/// What follows the branch when nothing is waiting to push: "up to date",
/// or how far the remote is ahead (no Pull here; docs/43 §2.6).
fn level_note(behind: u32) -> SharedString {
    if behind == 0 { "· up to date".into() } else { format!("· ↓{behind} on the remote").into() }
}

fn default_of(ui: &ChangesUi) -> &str {
    ui.default_branch.as_deref().unwrap_or("main")
}

impl Harness {
    /// The Changes state for `root`, reset when the root moved.
    pub(crate) fn changes_ui(&mut self, root: &Path) -> &mut ChangesUi {
        if self.right_cache.changes.root.as_deref() != Some(root) {
            self.right_cache.changes = ChangesUi { root: Some(root.to_path_buf()), ..ChangesUi::default() };
        }
        &mut self.right_cache.changes
    }

    /// After a snapshot lands for `root`: keep the box's facts fresh — the
    /// default branch once, the unpushed count, the PR on its poll, a draft
    /// for a new change set. Every read runs in the background.
    pub(crate) fn changes_after_snapshot(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(Some(tree)) = self.right_cache.git_for(root) else { return };
        let root_buf = root.to_path_buf();
        let ui = self.changes_ui(root);
        let need_default = ui.default_branch.is_none();
        let need_unpushed = tree.upstream.is_none() && tree.files.is_empty();
        let checks_running = matches!(&ui.pr, Some(Some(pr)) if pr.checks.is_some_and(|c| c.2));
        let poll = if checks_running { CHECKS_POLL } else { PR_POLL };
        let need_pr = tree.files.is_empty() && !ui.pr_reading && ui.pr_read_at.is_none_or(|at| at.elapsed() >= poll) && tree.upstream.is_some();
        if need_pr {
            ui.pr_reading = true;
        }
        if need_default || need_unpushed || need_pr {
            let root = root_buf.clone();
            cx.spawn(async move |this, cx| {
                let (default, unpushed, pr) = cx
                    .background_executor()
                    .spawn(async move {
                        let default = git_ops::default_branch(&root);
                        let unpushed = need_unpushed.then(|| git_ops::unpushed_without_upstream(&root, &default));
                        let pr = need_pr.then(|| git_ops::pr_for_branch(&root));
                        (default, unpushed, pr)
                    })
                    .await;
                let _ = this.update(cx, |this: &mut Self, cx| {
                    let ui = this.changes_ui(&root_buf);
                    ui.default_branch = Some(default);
                    if let Some(n) = unpushed {
                        ui.unpushed_without_upstream = Some(n);
                    }
                    if let Some(pr) = pr {
                        ui.pr = Some(pr);
                        ui.pr_read_at = Some(Instant::now());
                        ui.pr_reading = false;
                    }
                    cx.notify();
                });
            })
            .detach();
        }
        self.maybe_draft_commit(root, &tree, cx);
    }

    /// Draft the commit message once per change set on the session's own
    /// utility model (owner 2026-10-17), never over the person's words.
    fn maybe_draft_commit(&mut self, root: &Path, tree: &GitTree, cx: &mut Context<Self>) {
        if tree.files.is_empty() {
            return;
        }
        let key = change_set_key(&tree.files);
        let provider = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).map(|id| self.session_lane_provider(&id));
        let title = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).and_then(|id| self.collapsed_head_label(&self.chain_head(&id), cx));
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
        if own_words || ui.drafting || ui.drafted_for == Some(key) {
            return;
        }
        // No session's provider yet (the first snapshot can land before the
        // session opens): leave the set undrafted for the next snapshot
        // (live 2026-10-19: the set was marked and never drafted).
        let Some(provider) = provider else { return };
        ui.message.clear();
        ui.drafted_for = Some(key);
        let prompt = commit_prompt(title.as_deref(), &tree.files);
        if !crate::utility::supports(provider) {
            if provider == crate::providers::ProviderId::Muse {
                self.changes_ui(root).drafting = true;
                self.draft_on_muse(root, DraftKind::Commit, prompt, cx);
            }
            return;
        }
        let ui = self.changes_ui(root);
        ui.drafting = true;
        let root = root.to_path_buf();
        let model = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).and_then(|id| self.utility_model(&id, cx));
        cx.spawn(async move |this, cx| {
            let reply = cx.background_executor().spawn(async move { crate::utility::complete(provider, model.as_deref(), &prompt, None) }).await;
            if std::env::var("BAAZ_DRAFT_TRACE").as_deref() == Ok("1") {
                eprintln!("baaz: draft reply {:?}", reply.as_ref().map(|t| t.chars().take(120).collect::<String>()));
            }
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                ui.drafting = false;
                if let Ok(text) = reply {
                    let text = clean_draft(&text);
                    if !ui.message_edited && ui.message.trim().is_empty() && !text.is_empty() {
                        ui.message = text;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn draft_failed(&mut self, root: &Path, kind: DraftKind, cx: &mut Context<Self>) {
        match kind {
            DraftKind::Commit => self.changes_ui(root).drafting = false,
            DraftKind::PrBody => {
                if let Some(form) = self.changes_ui(root).form.as_mut() {
                    form.drafting = false;
                }
            }
        }
        cx.notify();
    }

    /// Toggle one row's diff, reading it the first time it opens.
    pub(crate) fn toggle_change_row(&mut self, root: &Path, change: &FileChange, cx: &mut Context<Self>) {
        let open = self.right_cache.flip_changes_file(root, &change.path);
        if open && !self.right_cache.file_diffs.contains_key(&change.path) {
            self.load_file_diff(root, change.clone(), cx);
        }
        cx.notify();
    }

    fn load_file_diff(&mut self, root: &Path, change: FileChange, cx: &mut Context<Self>) {
        self.right_cache.file_diffs.insert(change.path.clone(), None);
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let path = change.path.clone();
            let diff = if crate::clock::deterministic() {
                Some(crate::right::fixture_diff_for(&path))
            } else {
                let root = root.clone();
                cx.background_executor().spawn(async move { git_ops::read_file_diff(&root, &change) }).await
            };
            let _ = this.update(cx, |this: &mut Self, cx| {
                if let Some(diff) = diff {
                    this.right_cache.file_diffs.insert(path, Some(Arc::new(diff)));
                } else {
                    this.right_cache.file_diffs.remove(&path);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Diffs for rows already open when the tree re-read (their reads were
    /// dropped as stale).
    pub(crate) fn reload_open_diffs(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(Some(tree)) = self.right_cache.git_for(root) else { return };
        for path in self.right_cache.changes_expanded_for(root) {
            if !self.right_cache.file_diffs.contains_key(&path) {
                if let Some(change) = tree.change(&path).cloned() {
                    self.load_file_diff(root, change, cx);
                }
            }
        }
    }

    /// Expand the unchanged run after hunk `i` of `path` from the file on disk.
    pub(crate) fn expand_diff_gap(&mut self, root: &Path, path: String, i: usize, range: std::ops::Range<u32>, cx: &mut Context<Self>) {
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let p = path.clone();
            let lines = if crate::clock::deterministic() {
                (range.start..range.end).map(|n| format!("        // line {n}")).collect()
            } else {
                cx.background_executor().spawn(async move { git_ops::read_lines(&root, &p, range) }).await
            };
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.right_cache.gap_lines.insert((path, i), Arc::new(lines));
                cx.notify();
            });
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

    /// Commit every listed file with the message.
    pub(crate) fn commit_changes(&mut self, root: &Path, cx: &mut Context<Self>) {
        self.settle_commit_message(root, cx);
        let ui = self.changes_ui(root);
        let message = ui.message.trim().to_string();
        if message.is_empty() || ui.busy.is_some() {
            return;
        }
        ui.busy = Some(GitBusy::Committing);
        ui.hook_failure = None;
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let outcome = cx.background_executor().spawn(async move { git_ops::commit_all(&r, &message) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                ui.busy = None;
                match outcome {
                    CommitOutcome::Done => {
                        ui.message.clear();
                        ui.message_edited = false;
                        ui.drafted_for = None;
                        ui.unpushed_without_upstream = None;
                    }
                    CommitOutcome::HookFailed(lines) => ui.hook_failure = Some(lines),
                    CommitOutcome::Failed(reason) => {
                        this.overlays.update(cx, |o, _| {
                            o.toast("Commit", reason);
                        });
                    }
                }
                this.refresh_right_now(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// Push the branch (setting its upstream the first time).
    pub(crate) fn push_branch(&mut self, root: &Path, cx: &mut Context<Self>) {
        let Some(Some(tree)) = self.right_cache.git_for(root) else { return };
        let ui = self.changes_ui(root);
        if ui.busy.is_some() {
            return;
        }
        ui.busy = Some(GitBusy::Pushing);
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let result = cx.background_executor().spawn(async move { git_ops::push(&r, &tree.branch, tree.upstream.is_some()) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                ui.busy = None;
                ui.unpushed_without_upstream = None;
                ui.pr_read_at = None;
                if let Err(reason) = result {
                    this.overlays.update(cx, |o, _| {
                        o.toast("Push", reason);
                    });
                }
                this.refresh_right_now(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// "Ask … to fix it": the hook's output goes into the composer as a
    /// quote (docs/43 §2.6.3, §4), with no draft text.
    pub(crate) fn ask_to_fix_hook(&mut self, _root: &Path, lines: &[String], window: &mut Window, cx: &mut Context<Self>) {
        let text = lines.join("\n");
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.add_quote(text, None, Some(("text".into(), lines.len())), window, cx));
        }
        cx.notify();
    }

    /// Open the Create PR form: base = the default branch, the title and
    /// description drafted from the session once (owner 2026-10-17).
    pub(crate) fn open_pr_form(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let base = self.changes_ui(root).default_branch.clone().unwrap_or_else(|| "main".into());
        let label = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).and_then(|id| self.collapsed_head_label(&self.chain_head(&id), cx));
        let title = cx.new(|cx| {
            let mut s = InputState::new(window, cx);
            if let Some(label) = &label {
                s.set_value(label, window, cx);
            }
            s
        });
        let body = cx.new(|cx| aui::composer::composer_state_rows("Describe the change", 6, 16, window, cx));
        let ui = self.changes_ui(root);
        ui.form = Some(PrForm { base, bases: Vec::new(), title, body: body.clone(), draft: false, base_menu: false, drafting: false });
        cx.notify();
        let root_buf = root.to_path_buf();
        let provider = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).map(|id| self.session_lane_provider(&id));
        let files = self.right_cache.git_for(root).flatten().map(|t| t.files).unwrap_or_default();
        let base_for_summary = self.changes_ui(root).form.as_ref().map(|f| f.base.clone()).unwrap_or_else(|| "main".into());
        if let Some(form) = self.changes_ui(root).form.as_mut() {
            form.drafting = provider.is_some();
        }
        let model = self.active.as_ref().map(|view| view.read(cx).session_id.clone()).and_then(|id| self.utility_model(&id, cx));
        cx.spawn(async move |this, cx| {
            let r = root_buf.clone();
            let bases = cx.background_executor().spawn(async move { git_ops::remote_branches(&r) }).await;
            let r = root_buf.clone();
            let (commits, stat) = cx.background_executor().spawn(async move { git_ops::branch_summary(&r, &base_for_summary) }).await;
            let prompt = pr_prompt(label.as_deref(), &commits, &stat, &files);
            if provider == Some(crate::providers::ProviderId::Muse) {
                let (r, muse_prompt) = (root_buf.clone(), prompt.clone());
                let _ = this.update(cx, |this: &mut Self, cx| this.draft_on_muse(&r, DraftKind::PrBody, muse_prompt, cx));
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
        let Some(Some(tree)) = self.right_cache.git_for(root) else { return };
        let ui = self.changes_ui(root);
        let Some(form) = ui.form.as_ref() else { return };
        let title = form.title.read(cx).value().to_string();
        let body = form.body.read(cx).value().to_string();
        let (base, draft) = (form.base.clone(), form.draft);
        if title.trim().is_empty() || ui.busy.is_some() {
            return;
        }
        ui.busy = Some(GitBusy::CreatingPr);
        cx.notify();
        let root = root.to_path_buf();
        cx.spawn(async move |this, cx| {
            let r = root.clone();
            let head = tree.branch.clone();
            let result = cx.background_executor().spawn(async move { git_ops::create_pr(&r, &base, &head, &title, &body, draft) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                let ui = this.changes_ui(&root);
                ui.busy = None;
                match result {
                    Ok(pr) => {
                        ui.pr = Some(Some(pr));
                        ui.pr_read_at = Some(Instant::now());
                        ui.form = None;
                    }
                    Err(reason) => {
                        this.overlays.update(cx, |o, _| {
                            o.toast("Create PR", reason);
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The PR card's View: the PR in Browse (owner 2026-10-17).
    pub(crate) fn view_pull_request(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        if url.is_empty() {
            return;
        }
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.step_tab_new(&url, window, cx);
    }

    /// The transcript's `+N −M` chip: Changes, that turn's files opened, the
    /// list scrolled to the first, the rows flashing once (docs/43 §4).
    pub(crate) fn open_turn_changes(&mut self, files: Vec<String>, cx: &mut Context<Self>) {
        self.select_right(crate::layout::RightKind::Changes, cx);
        let Some((root, _)) = self.right_project() else { return };
        let tree = self.right_cache.git_for(&root).flatten();
        let mut matched = Vec::new();
        for f in &files {
            let rel = Path::new(f).strip_prefix(&root).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| f.clone());
            if tree.as_ref().is_some_and(|t| t.change(&rel).is_some()) {
                matched.push(rel);
            }
        }
        for rel in &matched {
            if !self.right_cache.changes_expanded_for(&root).contains(rel) {
                if let Some(change) = tree.as_ref().and_then(|t| t.change(rel)).cloned() {
                    self.toggle_change_row(&root, &change, cx);
                }
            }
        }
        let ui = self.changes_ui(&root);
        ui.flash = Some((Instant::now(), matched.clone()));
        ui.scroll.set_offset(gpui::point(px(0.0), px(0.0)));
        cx.notify();
    }
}

impl Harness {
    /// `changes-open:<path>`: open that row's diff (idempotent), refreshing
    /// first so the tree is there. Unknown paths record a step failure.
    pub(crate) fn step_changes_open(&mut self, rest: &str, cx: &mut Context<Self>) {
        let path = rest.trim();
        let Some((root, _)) = self.right_project() else {
            crate::steps::record_step_failure("changes-open:");
            return;
        };
        self.select_right(crate::layout::RightKind::Changes, cx);
        let Some(change) = self.right_cache.git_for(&root).flatten().and_then(|t| t.change(path).cloned()) else {
            crate::steps::record_step_failure(&format!("changes-open:{path}"));
            return;
        };
        if !self.right_cache.changes_expanded_for(&root).contains(path) {
            self.toggle_change_row(&root, &change, cx);
        }
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
    window: &mut Window,
    cx: &mut Context<Harness>,
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
        return pr_form_view(ui, &root_buf, &tree, width, window, cx);
    }
    let expanded = cache.changes_expanded_for(root);
    let p = cx.aui().colors;
    let (session, other) = crate::right::partition_session_files(&tree.files, session_edits, root);
    let flash = ui.flash.as_ref().filter(|(at, _)| at.elapsed() < FLASH).map(|(at, files)| (1.0 - at.elapsed().as_secs_f32() / FLASH.as_secs_f32(), files.clone()));
    if flash.is_some() {
        aui_motion::request_frame(window);
    }
    let foot = foot_for(&tree, ui);
    let scroll = ui.scroll.clone();
    let row_tops = ui.row_tops.clone();
    let diffs = cache.file_diffs.clone();
    let gaps = cache.gap_lines.clone();

    let mut list = fluid_list("right-changes-list", Axis::Y).gap(px(0.0)).label("Changes");
    let mut ix = 0usize;
    let mut sections: Vec<(&str, Vec<FileChange>)> = Vec::new();
    if !session.is_empty() || !session_edits.is_empty() {
        sections.push(("This session", session));
    }
    if !other.is_empty() {
        sections.push(("Other changes", other));
    }
    let mut first = true;
    let mut opened: Vec<(String, AnyElement)> = Vec::new();
    for (label, files) in sections {
        list = list.item_static(ix, changes_caption(SharedString::from(format!("right-changes-cap-{ix}")), label, Some(files.len()), first, cx));
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
            let row_el = |id: SharedString, cx: &mut Context<Harness>| {
                let (r1, c1, r2, a2) = (toggle_root.clone(), toggle_change.clone(), open_root.clone(), open_abs.clone());
                let mut row = change_row(id, change.clone(), open).width(px(width)).flash(flash_amount).on_toggle(cx.listener(move |this, _, _, cx| {
                    this.toggle_change_row(&r1, &c1, cx);
                }));
                if change.change != ChangeKind::Deleted {
                    row = row.on_open_file(cx.listener(move |this, _, _, cx| {
                        this.reveal_link_path(&r2, &a2, Some(false), None, cx);
                    }));
                }
                row
            };
            let row = row_el(id.clone(), cx);
            if open {
                opened.push((change.path.clone(), row_el(SharedString::from(format!("right-changes-sticky:{}", change.path)), cx).into_any_element()));
            }
            // `Some(None)`: read, but there is no text diff (a binary file).
            let unreadable = matches!(diffs.get(&change.path), Some(None));
            let diff = diffs.get(&change.path).cloned().flatten();
            let path = change.path.clone();
            let gap_root = root_buf.clone();
            let weak = cx.weak_entity();
            let gaps = gaps.clone();
            let muted = cx.aui().colors.fg_muted;
            let body = move || -> AnyElement {
                match diff {
                    Some(diff) => {
                        let d: Arc<Diff> = diff.clone();
                        let mut b = diff_body(SharedString::from(format!("right-diff:{path}")), d.clone());
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
                        .h(px(28.0))
                        .px(px(22.0))
                        .flex()
                        .items_center()
                        .text_px(13.2)
                        .text_color(muted)
                        .child("Binary file — no text diff.")
                        .into_any_element(),
                    None => div().h(px(24.0)).into_any_element(),
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
                list = list.item_toolbar(ix, wrap.child(fold));
                ix += 1;
            }
        }
    }

    let foot_el = foot_element(ui, &root_buf, &tree, foot, agent, window, cx);
    let clean = tree.files.is_empty();
    let body = if clean {
        div().size_full().pb(px(72.0)).child(state_block("right-changes-clean", IconName::Check, "No changes").line("The working tree is clean.")).into_any_element()
    } else {
        div()
            .id("right-changes-scroll")
            .size_full()
            .overflow_y_scroll()
            .scroll_axis_locked()
            .track_scroll(&scroll)
            // The sticky row is decided in render: a scroll re-renders, after
            // the wheel has moved the offset (live 2026-10-19: it never stuck).
            .on_scroll_wheel(cx.listener(|_: &mut Harness, _: &gpui::ScrollWheelEvent, _, cx| cx.notify()))
            .pt(px(4.0))
            .px(px(6.0))
            .pb(px(180.0))
            .child(list)
            .into_any_element()
    };
    // The sticky row: an open file whose row scrolled under the pane row
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
    let mut screen = div().relative().size_full().bg(p.surface_2).child(body);
    if let Some(row) = sticky {
        screen = screen.child(div().absolute().top(px(0.0)).left(px(6.0)).right(px(6.0)).bg(p.surface_2).child(row));
    }
    (None, screen.child(commit_foot(foot_el, cx)).into_any_element())
}

fn loading_body(cx: &Context<Harness>) -> gpui::Div {
    v_flex().pt(px(4.0)).px(px(6.0)).child(changes_caption("right-changes-loading-cap", "This session", None, true, cx)).children([90.0, 120.0, 70.0, 110.0, 84.0].iter().map(|w| {
        h_flex()
            .h(px(32.0))
            .gap(px(10.0))
            .pl(px(24.0))
            .pr(px(8.0))
            .items_center()
            .child(skeleton_bar(px(10.0), cx))
            .child(skeleton_bar(px(*w), cx))
            .child(div().flex_1())
            .child(skeleton_bar(px(44.0), cx))
    }))
}

/// The commit box (or the hook note over it, or the PR card).
fn foot_element(ui: &ChangesUi, root: &Path, tree: &GitTree, foot: Foot, agent: &str, window: &mut Window, cx: &mut Context<Harness>) -> AnyElement {
    let busy = ui.busy;
    let hook = ui.hook_failure.clone();
    let editor = ui.editor.clone();
    let message = ui.message.clone();
    let drafting = ui.drafting;
    let root_buf = root.to_path_buf();
    match foot {
        Foot::Commit => {
            let n = tree.files.len();
            let label = if busy == Some(GitBusy::Committing) { "Committing…".to_string() } else { format!("Commit {n} file{}", if n == 1 { "" } else { "s" }) };
            let editing = editor.is_some();
            let focused = editor.as_ref().is_some_and(|e| gpui::Focusable::focus_handle(e.read(cx), cx).is_focused(window));
            let message_el: AnyElement = match editor {
                Some(editor) => {
                    let r1 = root_buf.clone();
                    let r2 = root_buf.clone();
                    div()
                        .id("right-commit-editor")
                        .min_h(px(44.0))
                        .px(px(4.0))
                        .on_action(cx.listener(move |this, _: &crate::app::CommitMessage, _, cx| this.commit_changes(&r1, cx)))
                        .on_mouse_down_out(cx.listener(move |this, _, _, cx| this.settle_commit_message(&r2, cx)))
                        .key_context("CommitMessage")
                        .child(Textarea::new(&editor).appearance(false).bordered(false).text_size(aui_tokens::scaled(14.3)).line_height(px(22.0)))
                        .into_any_element()
                }
                None => {
                    let r = root_buf.clone();
                    let shown = if drafting && message.is_empty() { String::new() } else { message.clone() };
                    commit_message_view("right-commit-message", &shown, cx).on_click(cx.listener(move |this, _, window, cx| this.edit_commit_message(&r, window, cx))).into_any_element()
                }
            };
            let disabled = message.trim().is_empty() && !editing;
            let r = root_buf.clone();
            let commit = primary_action("right-commit", label, None, busy == Some(GitBusy::Committing), disabled || busy.is_some(), cx)
                .when(!disabled && busy.is_none(), |b| b.on_click(cx.listener(move |this, _, _, cx| this.commit_changes(&r, cx))));
            let box_el = commit_frame(focused, cx)
                .child(message_el)
                .child(h_flex().h(px(28.0)).gap(px(6.0)).items_center().child(branch_label(&tree.branch, ahead_behind(tree.ahead, tree.behind), cx)).child(div().flex_1()).child(commit));
            match hook {
                Some(lines) => {
                    let who = agent;
                    let (r1, r2) = (root_buf.clone(), root_buf.clone());
                    let l1 = lines.clone();
                    let l2 = lines.clone();
                    let actions = vec![
                        text_action("right-hook-output", "Show full output", None, 24.0, cx)
                            .on_click(cx.listener(move |this, _, _, cx| this.show_hook_output(&r1, l1.clone(), cx)))
                            .into_any_element(),
                        text_action("right-hook-ask", format!("Ask {who} to fix it"), None, 24.0, cx)
                            .on_click(cx.listener(move |this, _, window, cx| this.ask_to_fix_hook(&r2, &l2, window, cx)))
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
            commit_frame(false, cx)
                .child(
                    h_flex()
                        .h(px(28.0))
                        .items_center()
                        .child(branch_label(&tree.branch, Some(suffix.into()), cx))
                        .child(div().flex_1())
                        .child(primary_action("right-push", "Push", None, busy == Some(GitBusy::Pushing), busy.is_some(), cx).on_click(cx.listener(move |this, _, _, cx| this.push_branch(&r, cx)))),
                )
                .into_any_element()
        }
        Foot::CreatePr => {
            let r = root_buf.clone();
            commit_frame(false, cx)
                .child(
                    h_flex()
                        .h(px(28.0))
                        .items_center()
                        .child(branch_label(&tree.branch, Some(level_note(tree.behind)), cx))
                        .child(div().flex_1())
                        .child(primary_action("right-create-pr", "Create PR", Some(IconName::PullRequest), false, busy.is_some(), cx).on_click(cx.listener(move |this, _, window, cx| this.open_pr_form(&r, window, cx)))),
                )
                .into_any_element()
        }
        Foot::Level => commit_frame(false, cx)
            .child(h_flex().h(px(28.0)).items_center().child(branch_label(&tree.branch, Some(level_note(tree.behind)), cx)))
            .into_any_element(),
        Foot::Pr(pr) => {
            let url = pr.url.clone();
            let view = text_action("right-pr-view", "View", Some(IconName::External), 24.0, cx).on_click(cx.listener(move |this, _, window, cx| this.view_pull_request(url.clone(), window, cx)));
            pr_card("right-pr", pr.title.clone(), pr.number, pr.state, pr.checks, view, cx).into_any_element()
        }
    }
}

/// The Create PR pushed view: its row and its body (docs/43 §2.6).
fn pr_form_view(ui: &ChangesUi, root: &Path, tree: &GitTree, width: f32, window: &mut Window, cx: &mut Context<Harness>) -> (Option<AnyElement>, AnyElement) {
    let busy = ui.busy;
    let form = ui.form.as_ref().expect("the form stands open");
    let (base, bases, title, body, draft, menu, drafting) = (form.base.clone(), form.bases.clone(), form.title.clone(), form.body.clone(), form.draft, form.base_menu, form.drafting);
    let p = cx.aui().colors;
    let back_root = root.to_path_buf();
    let close = cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| this.toggle_right(cx));
    let row = pushed_row("right-pr-row", "Create pull request")
        .back_label("Back to changes")
        .width(px(width))
        .on_back(cx.listener(move |this, _, _, cx| {
            this.changes_ui(&back_root).form = None;
            cx.notify();
        }))
        .on_close(close)
        .into_any_element();
    let title_focused = gpui::Focusable::focus_handle(title.read(cx), cx).is_focused(window);
    let body_focused = gpui::Focusable::focus_handle(body.read(cx), cx).is_focused(window);
    let menu_root = root.to_path_buf();
    let mut picker = div().relative().child(base_picker("right-pr-base", base.clone(), cx).on_click(cx.listener(move |this, _, _, cx| {
        if let Some(f) = this.changes_ui(&menu_root).form.as_mut() {
            f.base_menu = !f.base_menu;
        }
        cx.notify();
    })));
    if menu && !bases.is_empty() {
        let mut items = v_flex().p(px(4.0)).min_w(px(160.0)).rounded(px(10.0)).bg(p.overlay).shadow(p.floating_shadow());
        for b in bases {
            let pick_root = root.to_path_buf();
            let value = b.clone();
            items = items.child(
                div()
                    .id(SharedString::from(format!("right-pr-base-{b}")))
                    .role(gpui::Role::MenuItem)
                    .aria_label(b.clone())
                    .h(px(28.0))
                    .px(px(8.0))
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .cursor_pointer()
                    .font_family(aui_tokens::scale::FONT_MONO)
                    .hover(move |s| s.bg(p.hover_overlay()))
                    .child(b)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(f) = this.changes_ui(&pick_root).form.as_mut() {
                            f.base = value.clone();
                            f.base_menu = false;
                        }
                        cx.notify();
                    })),
            );
        }
        picker = picker.child(div().absolute().top(px(36.0)).left(px(0.0)).child(items));
    }
    let draft_root = root.to_path_buf();
    let body_el = v_flex()
        .id("right-pr-form")
        .size_full()
        .overflow_y_scroll()
        .scroll_axis_locked()
        .p(px(12.0))
        .pb(px(64.0))
        .gap(px(12.0))
        .child(form_field("Into", base_and_head(picker, &tree.branch, cx), cx))
        .child(form_field("Title", well_input(gpui_kit::base::input::Input::new(&title), 32.0, title_focused, cx), cx))
        .child(form_field(
            "Description",
            well_input(
                v_flex()
                    .child(Textarea::new(&body).appearance(false).bordered(false).text_size(aui_tokens::scaled(14.3)).line_height(px(22.0)))
                    .child(div().mt(px(8.0)).text_color(p.fg_muted).child(if drafting { "Drafting from this session…" } else { "Drafted from this session · edit freely" })),
                150.0,
                body_focused,
                cx,
            ),
            cx,
        ))
        .child(draft_checkbox("right-pr-draft", draft, cx).on_click(cx.listener(move |this, _, _, cx| {
            if let Some(f) = this.changes_ui(&draft_root).form.as_mut() {
                f.draft = !f.draft;
            }
            cx.notify();
        })));
    let cancel_root = root.to_path_buf();
    let create_root = root.to_path_buf();
    let foot = h_flex()
        .justify_end()
        .gap(px(6.0))
        .px(px(4.0))
        .child(text_action("right-pr-cancel", "Cancel", None, 28.0, cx).on_click(cx.listener(move |this, _, _, cx| {
            this.changes_ui(&cancel_root).form = None;
            cx.notify();
        })))
        .child(
            primary_action("right-pr-create", "Create PR", Some(IconName::PullRequest), busy == Some(GitBusy::CreatingPr), busy.is_some(), cx)
                .on_click(cx.listener(move |this, _, _, cx| this.create_pull_request(&create_root, cx))),
        );
    let screen = div().relative().size_full().child(body_el).child(commit_foot(foot, cx));
    (Some(row), screen.into_any_element())
}

#[cfg(test)]
mod tests {
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
        ui.pr = Some(Some(PullRequest { number: 7, title: "t".into(), url: "u".into(), state: aui::workbench::PrState::Open, checks: None }));
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
    fn one_change_set_hashes_the_same_twice() {
        let a = vec![change("a"), change("b")];
        assert_eq!(change_set_key(&a), change_set_key(&a.clone()));
        assert_ne!(change_set_key(&a), change_set_key(&[change("a")]));
    }
}
