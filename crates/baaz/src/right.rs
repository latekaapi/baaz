//! The right pane's four bodies: real read-only data, inert labelled actions.
//!
//! [`render`] dispatches on [`RightKind`](crate::layout::RightKind) to one
//! builder per pane. It never touches a subprocess or the filesystem: it draws
//! whatever [`RightCache`] holds, and an empty cache draws the pane's loading
//! state. The reads — an expansion-aware directory walk, a handful of
//! read-only `git` invocations each with a timeout — happen in
//! [`read_snapshot`], which the [`Harness`](crate::app::Harness) refresh path
//! runs on a background task and applies with an update + notify. The file
//! tree's Refresh, Toggle and Select are wired (re-read, expand, preview);
//! a previewed text file's bytes arrive on a second background task through
//! [`complete_file_preview`]. Every other action button is inert by owner
//! decision and says so through [`inert`].

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::rc::Rc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use aui::data::button;
use aui::transcript::code_block;
use aui::workbench::{
    diff_review, doc_pane, file_card, file_tree, git_changes, pr_form, ArtifactKind, DiffReviewAction,
    DiffScope, DiffView, DocBlock, DocPage, FileNode, FileTreeAction, GitAction, PrAction,
    PrDescription, ReviewFile,
};
use aui_icons::FileType;
use aui_protocol::{ChangeKind, Diff, DiffKind, DiffLine, FileChange, Hunk};
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, ElementId, Entity, ScrollHandle, SharedString,
    WeakEntity, Window,
};
use gpui_kit::base::{h_flex, v_flex};

use crate::app::Harness;
use crate::layout::RightKind;

/// How many file-tree rows the Files pane will hold. A directory with 10,000
/// entries must not be read into the pane; the footer names the cap.
pub(crate) const FILE_WALK_CAP: usize = 300;
/// How many files of a working-tree diff reach the Diff pane.
pub(crate) const DIFF_FILES_CAP: usize = 16;
/// How many body lines of one file reach the Diff pane.
pub(crate) const DIFF_LINES_PER_FILE_CAP: usize = 500;
/// One read-only git invocation may take this long before it counts as hung.
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
/// Directory names never walked into, so a naive walk stays small.
const SKIP_DIRS: [&str; 3] = [".git", "target", "node_modules"];
/// A preview reads at most this much: anything larger gets the summary card
/// with Open / Reveal instead of its bytes.
pub(crate) const PREVIEW_MAX_BYTES: u64 = 1024 * 1024;
/// How many markdown blocks the `.md` preview builds before stopping: a
/// 1 MB note must not turn into ten thousand elements.
const PREVIEW_MD_BLOCKS_CAP: usize = 300;

/// Render-path I/O probe: incremented by [`run_git`] and by every real
/// `read_dir` in the file walk, and by nothing else. [`render`] must never
/// move it; the purity test renders every kind twice and asserts it stays
/// put. A directory served from [`DirListCache`] moves [`cache_hit_count`]
/// instead, so the two apart prove a refresh skipped the filesystem.
#[cfg(test)]
static RENDER_IO_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Directory listings served from [`DirListCache`] without a `read_dir`.
#[cfg(test)]
static DIR_CACHE_HITS: AtomicUsize = AtomicUsize::new(0);

/// Directory entries collected from `read_dir` before sorting, bounded by
/// the walk cap: the test hook that proves a huge directory is read bounded.
#[cfg(test)]
static WALK_COLLECTED: AtomicUsize = AtomicUsize::new(0);

/// How many blocking reads [`run_git`] and the file walk have done since
/// process start (or the last [`reset_io_count`]).
#[cfg(test)]
pub(crate) fn io_count() -> usize {
    RENDER_IO_COUNT.load(Ordering::Relaxed)
}

/// How many directory listings came from [`DirListCache`] since process
/// start (or the last [`reset_io_count`]).
#[cfg(test)]
pub(crate) fn cache_hit_count() -> usize {
    DIR_CACHE_HITS.load(Ordering::Relaxed)
}

/// How many directory entries the walk collected since process start (or
/// the last [`reset_io_count`]).
#[cfg(test)]
pub(crate) fn walk_collected_count() -> usize {
    WALK_COLLECTED.load(Ordering::Relaxed)
}

/// Zero [`io_count`], [`cache_hit_count`] and [`walk_collected_count`]; the
/// purity test drives [`render`] between the reset and the assert.
#[cfg(test)]
pub(crate) fn reset_io_count() {
    RENDER_IO_COUNT.store(0, Ordering::Relaxed);
    DIR_CACHE_HITS.store(0, Ordering::Relaxed);
    WALK_COLLECTED.store(0, Ordering::Relaxed);
}

/// Count one blocking read toward [`io_count`] in test builds; nothing in
/// production, where the counter does not exist.
fn note_io() {
    #[cfg(test)]
    RENDER_IO_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Count one cache-served directory toward [`cache_hit_count`] in test
/// builds; nothing in production.
fn note_cache_hit() {
    #[cfg(test)]
    DIR_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

/// Count one collected directory entry toward [`walk_collected_count`] in
/// test builds; nothing in production.
fn note_collected() {
    #[cfg(test)]
    WALK_COLLECTED.fetch_add(1, Ordering::Relaxed);
}

/// One cached read: what was read, for which root, and when.
#[derive(Clone, Debug)]
pub(crate) struct Stamped<T> {
    /// The project root the value was read for.
    pub root: PathBuf,
    /// When the read landed.
    pub at: Instant,
    /// The read itself.
    pub value: T,
}

/// Everything the right pane shows, read off the render path: the last
/// [`GitStatus`] (`None` means fetched and not a repo, as opposed to never
/// fetched), the last parsed working-tree diff, and the last file-tree listing
/// with its cap flag — each stamped with the root it was read for and the
/// instant it was read. [`render`] only reads this; the
/// [`Harness`](crate::app::Harness) refresh path writes it from a background
/// task.
/// What the Files pane previews, and how.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum PreviewContent {
    /// Selected, and the background read has not landed yet.
    Loading,
    /// A non-markdown text file: its bytes in a `code_block`.
    Text {
        /// The `code_block` language label, from the extension.
        language: String,
        /// The file's UTF-8 bytes.
        code: String,
    },
    /// A `.md` file: its bytes in a `doc_pane`.
    Markdown {
        /// The file's UTF-8 bytes.
        text: String,
    },
    /// Anything without previewable bytes: binary, too large, unreadable.
    Card {
        /// The card's meta line, e.g. why there are no bytes.
        meta: String,
    },
}

/// One open file preview: which file, and what the pane shows for it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FilePreview {
    /// The tree id: the root-relative path, e.g. `uiprobe.json`.
    pub path: String,
    /// The last segment, shown in the preview header.
    pub name: String,
    /// The language label for text previews, from the extension.
    pub language: String,
    /// The file's bytes, when small enough.
    pub size: u64,
    /// The file's mtime when the preview was (re)loaded: `None` when the
    /// file was already gone. The refresh path compares it (with [`Self::size`])
    /// against a fresh stat so an open preview follows edits on disk.
    pub mtime: Option<std::time::SystemTime>,
    /// What the pane draws.
    pub content: PreviewContent,
}

/// One directory's cached listing: the sorted entries the walk replays while
/// the directory's mtime stands still, so a refresh re-reads only what
/// changed. Entries are already cut to the walk cap (see [`FILE_WALK_CAP`]).
#[derive(Clone, Debug)]
struct CachedDir {
    /// The directory's mtime when it was read; `None` when it was unreadable.
    mtime: Option<std::time::SystemTime>,
    /// The cap the entries were cut to: a walk with another cap re-reads.
    cap: usize,
    /// `(name, is_dir)` pairs in name order, at most `cap` long.
    entries: Vec<(String, bool)>,
    /// Whether the directory held more than `cap` entries when read.
    dir_truncated: bool,
}

/// The file walk's per-directory listing cache: one stamped entry per
/// absolute directory path. It lives in [`RightCache`] so it survives
/// between refreshes, and it is dropped whenever the root changes.
#[derive(Clone, Debug, Default)]
pub(crate) struct DirListCache {
    /// The root the entries were read for; a walk for another root clears.
    root: Option<PathBuf>,
    /// Absolute directory path to its stamped listing.
    dirs: HashMap<PathBuf, CachedDir>,
}

impl DirListCache {
    /// Ready this cache for a walk of `root`, dropping everything read for
    /// another root.
    fn for_root(&mut self, root: &Path) {
        if self.root.as_ref() != Some(&root.to_path_buf()) {
            *self = DirListCache { root: Some(root.to_path_buf()), dirs: HashMap::new() };
        }
    }
}

#[derive(Default)]
pub(crate) struct RightCache {
    /// The last git status read, by root.
    pub git: Option<Stamped<Option<GitStatus>>>,
    /// The last working-tree diff parsed, by root.
    pub diffs: Option<Stamped<ParsedDiffs>>,
    /// The last file-tree listing and whether the cap cut it, by root.
    pub files: Option<Stamped<(Vec<FileNode>, bool)>>,
    /// Directory ids standing open, per project root: the session's
    /// expansion state. Refresh re-reads around it — [`apply_snapshot`]
    /// leaves it alone — so a re-read never collapses the tree.
    pub expanded: HashMap<PathBuf, HashSet<String>>,
    /// The last selected file id per root: the tree's selected marker,
    /// kept when the preview closes.
    pub selected: HashMap<PathBuf, String>,
    /// The open preview per root, if the pane is previewing a file.
    pub previews: HashMap<PathBuf, FilePreview>,
    /// The walk's per-directory listing cache: stamped per directory mtime,
    /// surviving between refreshes, dropped when the root changes.
    dir_lists: DirListCache,
    /// The pane's scroll handles, per root: the tree and the preview share
    /// one handle, so Escape/back returns to the tree where it was.
    files_scroll: RefCell<HashMap<PathBuf, ScrollHandle>>,
}

impl RightCache {
    /// The cached git status for `root`: `None` when never fetched for it,
    /// `Some(None)` when fetched and not a repo.
    pub(crate) fn git_for(&self, root: &Path) -> Option<Option<GitStatus>> {
        self.git.as_ref().filter(|slot| slot.root == root).map(|slot| slot.value.clone())
    }

    /// The cached parsed diff for `root`, or `None` when never fetched for it.
    pub(crate) fn diffs_for(&self, root: &Path) -> Option<ParsedDiffs> {
        self.diffs.as_ref().filter(|slot| slot.root == root).map(|slot| slot.value.clone())
    }

    /// The cached file listing and cap flag for `root`, or `None` when never
    /// fetched for it.
    pub(crate) fn files_for(&self, root: &Path) -> Option<(Vec<FileNode>, bool)> {
        self.files.as_ref().filter(|slot| slot.root == root).map(|slot| slot.value.clone())
    }

    /// The directory ids standing open for `root`: empty when nothing was
    /// ever toggled there.
    pub(crate) fn expanded_for(&self, root: &Path) -> HashSet<String> {
        self.expanded.get(root).cloned().unwrap_or_default()
    }

    /// The open preview for `root`, if the pane is previewing a file.
    pub(crate) fn preview_for(&self, root: &Path) -> Option<FilePreview> {
        self.previews.get(root).cloned()
    }

    /// The last selected file id for `root`, if any.
    pub(crate) fn selected_for(&self, root: &Path) -> Option<String> {
        self.selected.get(root).cloned()
    }

    /// The scroll handle the Files pane shares between its tree and its
    /// preview for `root`, created on first use.
    pub(crate) fn files_scroll_for(&self, root: &Path) -> ScrollHandle {
        self.files_scroll.borrow_mut().entry(root.to_path_buf()).or_default().clone()
    }

    /// When the slot backing `kind` last landed for `root`, if it ever did.
    /// The refresh path paces the steady-state interval off this.
    pub(crate) fn fetched_at(&self, kind: RightKind, root: &Path) -> Option<Instant> {
        match kind {
            RightKind::Browser => None,
            RightKind::Git | RightKind::Diff => {
                self.git.as_ref().filter(|slot| slot.root == root).map(|slot| slot.at)
            }
            RightKind::Files => self.files.as_ref().filter(|slot| slot.root == root).map(|slot| slot.at),
        }
    }

    /// Land one [`read_snapshot`]: every slot takes the snapshot's root and
    /// the landing instant. A snapshot for another root drops the directory
    /// listing cache: its stamped entries belong to the old tree.
    pub(crate) fn apply_snapshot(&mut self, snapshot: RightSnapshot, at: Instant) {
        if self.files.as_ref().is_some_and(|slot| slot.root != snapshot.root) {
            self.dir_lists = DirListCache::default();
        }
        let root = snapshot.root.clone();
        self.git = Some(Stamped { root: root.clone(), at, value: snapshot.git });
        self.diffs = Some(Stamped { root: root.clone(), at, value: snapshot.diffs });
        self.files = Some(Stamped { root, at, value: (snapshot.files, snapshot.files_truncated) });
    }

    /// The directory listing cache, readied for `root`: everything read for
    /// another root is dropped. The refresh path walks through this, so an
    /// unchanged directory is not re-read.
    pub(crate) fn dir_cache_for_root(&mut self, root: &Path) -> &mut DirListCache {
        self.dir_lists.for_root(root);
        &mut self.dir_lists
    }

    /// Move the directory listing cache out for a background refresh: the
    /// background task owns it while the pane stays drawable.
    pub(crate) fn take_dir_cache(&mut self) -> DirListCache {
        std::mem::take(&mut self.dir_lists)
    }

    /// Land a background refresh's directory listing cache. A snapshot for
    /// another root landing after this still drops it in [`apply_snapshot`].
    pub(crate) fn restore_dir_cache(&mut self, cache: DirListCache) {
        self.dir_lists = cache;
    }
}

/// One blocking re-read of everything the pane shows, run on a background
/// thread and applied to [`RightCache`] with an update + notify. Never called
/// from [`render`].
#[derive(Debug)]
pub(crate) struct RightSnapshot {
    /// The root everything was read for.
    pub root: PathBuf,
    /// The status read, or `None` when the root is not a git checkout.
    pub git: Option<GitStatus>,
    /// The parsed working-tree diff (empty when there is nothing to review).
    pub diffs: ParsedDiffs,
    /// The file-tree listing.
    pub files: Vec<FileNode>,
    /// Whether [`FILE_WALK_CAP`] cut the listing short.
    pub files_truncated: bool,
}

/// Re-read everything for `root`: up to six read-only git subprocesses plus a
/// capped one-level walk. Blocking — the
/// [`Harness`](crate::app::Harness) refresh path runs it on the background
/// executor (inline under `BAAZ_DETERMINISTIC`, where the git reads are
/// fixtures and the walk is the only real I/O).
#[cfg(test)]
pub(crate) fn read_snapshot(root: &Path) -> RightSnapshot {
    read_snapshot_for(root, &HashSet::new())
}

/// [`read_snapshot`] with the session's expansion state: open directories
/// walk one level deeper each, so a toggle survives the re-read that draws
/// it. Blocking, like [`read_snapshot`]. Test builds only: production
/// refreshes go through [`read_snapshot_for_cached`].
#[cfg(test)]
pub(crate) fn read_snapshot_for(root: &Path, expanded: &HashSet<String>) -> RightSnapshot {
    let mut cache = DirListCache::default();
    read_snapshot_for_cached(root, expanded, &mut cache)
}

/// [`read_snapshot_for`] through the [`RightCache`] directory listing cache:
/// directories whose mtime stands still are replayed, never re-read. The
/// refresh path calls this off the render path and lands the listing (and
/// the cache, owned by the caller) with [`RightCache::apply_snapshot`].
/// Blocking, like [`read_snapshot`].
pub(crate) fn read_snapshot_for_cached(
    root: &Path,
    expanded: &HashSet<String>,
    dir_cache: &mut DirListCache,
) -> RightSnapshot {
    dir_cache.for_root(root);
    if crate::clock::deterministic() {
        let (files, files_truncated) = walk_root_expanded_cached(root, FILE_WALK_CAP, expanded, dir_cache);
        return RightSnapshot {
            root: root.to_path_buf(),
            git: Some(fixture_git_status()),
            diffs: ParsedDiffs { diffs: vec![fixture_diff()], truncated: false },
            files,
            files_truncated,
        };
    }
    let git = read_git_status(root);
    let diffs = match &git {
        Some(status) if !status.files.is_empty() => {
            let raw = run_git(root, &["diff", "--no-color", "--no-ext-diff", "--unified=3"]).unwrap_or_default();
            parse_unified_diff(&raw)
        }
        _ => ParsedDiffs { diffs: Vec::new(), truncated: false },
    };
    let (files, files_truncated) = walk_root_expanded_cached(root, FILE_WALK_CAP, expanded, dir_cache);
    RightSnapshot { root: root.to_path_buf(), git, diffs, files, files_truncated }
}

/// Render the right pane for `kind` from the already-read [`RightCache`].
///
/// This function never reads the `Harness` entity itself — `render` runs while
/// `Harness` is borrowed for update, so `cx.entity().read(cx)` would panic —
/// and never touches a subprocess or the filesystem either. The project and
/// the cache arrive as arguments (the project from the in-memory current
/// project, which the old path re-resolved from the projects store on disk
/// every frame); toasts travel through a weak handle the action closures
/// upgrade at click time, when no borrow is held. An empty cache for the
/// current kind draws the pane's loading state — never a blocking fill.
pub(crate) fn render(
    kind: RightKind,
    cache: &RightCache,
    project: Option<(PathBuf, String)>,
    browser: Option<&Entity<aui_webview::WebviewState>>,
    cx: &mut Context<Harness>,
) -> AnyElement {
    let notify = toast_sink(cx.weak_entity());
    let harness = cx.weak_entity();
    match kind {
        RightKind::Browser => match browser {
            Some(state) => browser_pane(state, cx),
            None => loading_state(
                "right-browser-loading",
                "Browser",
                "Opening the page",
                "The webview is being attached.",
            ),
        },
        RightKind::Diff => match project {
            Some((root, _)) => match cache.git_for(&root) {
                None => loading_state(
                    "right-diff-loading",
                    "Diff review",
                    "Loading the working tree",
                    "The pane re-reads the repository in the background and fills in on its own.",
                ),
                Some(None) => empty_state(
                    "right-diff-empty",
                    "Diff review",
                    "Not a git repository",
                    "This project's folder is not a git checkout, so there is nothing to review.",
                ),
                Some(Some(status)) => {
                    if status.files.is_empty() {
                        empty_state(
                            "right-diff-clean",
                            "Diff review",
                            "No unstaged changes",
                            "The working tree is clean — there is nothing to review.",
                        )
                    } else {
                        match cache.diffs_for(&root) {
                            None => loading_state(
                                "right-diff-loading",
                                "Diff review",
                                "Loading the working tree",
                                "The pane re-reads the repository in the background and fills in on its own.",
                            ),
                            Some(parsed) => diff_pane_from(&status, &parsed, &notify),
                        }
                    }
                }
            },
            None => empty_state(
                "right-diff-empty",
                "Diff review",
                "No project is open",
                "Open or adopt a project and its unstaged changes will be reviewed here.",
            ),
        },
        RightKind::Git => match project {
            Some((root, _)) => match cache.git_for(&root) {
                None => loading_state(
                    "right-git-loading",
                    "Changes",
                    "Loading changes",
                    "The pane re-reads the repository in the background and fills in on its own.",
                ),
                Some(None) => empty_state(
                    "right-git-empty",
                    "Changes",
                    "Not a git repository",
                    "This project's folder is not a git checkout, so there are no changes to show.",
                ),
                Some(Some(status)) => git_pane_from(&status, &notify),
            },
            None => empty_state(
                "right-git-empty",
                "Changes",
                "No project is open",
                "Open or adopt a project and its uncommitted changes will be listed here.",
            ),
        },
        RightKind::Files => match project {
            Some((root, name)) => match cache.files_for(&root) {
                None => loading_state(
                    "right-files-loading",
                    "Files",
                    "Loading files",
                    "The pane re-reads the project in the background and fills in on its own.",
                ),
                Some((nodes, truncated)) => {
                    let selected = cache.selected_for(&root);
                    let nodes = mark_selected(nodes, selected.as_deref());
                    files_pane_from(&nodes, truncated, &name, &root, cache, &notify, harness)
                }
            },
            None => empty_state(
                "right-files-empty",
                "Files",
                "No project is open",
                "Open or adopt a project to browse its files here.",
            ),
        },
    }
}

/// The file tree's Refresh action, the one wired action in the pane: ask the
/// [`Harness`](crate::app::Harness) for a real re-read off the render path.
/// Silent when the window is already gone.
pub(crate) fn refresh_files(harness: WeakEntity<Harness>, cx: &mut App) {
    let _ = harness.update(cx, |this, cx| this.refresh_right_now(cx));
}

impl Harness {
    /// Flip the directory `id` open or closed for `root` and re-read, so
    /// the toggle draws its children (or hides them) on the next frame.
    pub(crate) fn toggle_files_dir(&mut self, root: &Path, id: &str, cx: &mut Context<Self>) {
        toggle_expanded(&mut self.right_cache, root, id);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
        cx.notify();
    }

    /// Start previewing `id` for `root` from a click or a step verb: the
    /// selection and the loading (or card) state land at once, and a text
    /// file's bytes follow on a background task. Never blocks the UI
    /// thread on file content — only on one metadata call.
    pub(crate) fn begin_file_preview_for(&mut self, root: &Path, id: &str, cx: &mut Context<Self>) {
        if !begin_file_preview(&mut self.right_cache, root, id) {
            self.save_right_for_active(cx);
            cx.notify();
            return;
        }
        self.save_right_for_active(cx);
        cx.notify();
        let root = root.to_path_buf();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let path = root.join(&id);
            let bytes = cx.background_executor().spawn(async move { std::fs::read(path).ok() }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                complete_file_preview(&mut this.right_cache, &root, &id, bytes);
                cx.notify();
            });
        })
        .detach();
    }

    /// Close the open preview for `root`, returning to the tree with the
    /// selected marker kept. What the preview's back control and Escape
    /// call. Returns whether one was open.
    pub(crate) fn close_file_preview_for(&mut self, root: &Path, cx: &mut Context<Self>) -> bool {
        let closed = close_file_preview(&mut self.right_cache, root);
        if closed {
            self.save_right_for_active(cx);
            cx.notify();
        }
        closed
    }

    /// Open the right pane on the Browser kind because the agent navigated
    /// `session_id` there (Z7b): like a person's click would — the pane
    /// stands open on Browser and the session's saved state says so — but
    /// without the toggle (`show_right` would close a pane already showing
    /// Browser) and without arming URL focus (the person's keyboard stays
    /// where it was). Works for a session that is not active: the saved
    /// state is written onto `session_id` directly.
    pub(crate) fn show_browser_for_agent(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // Only the session on screen changes what is on screen (review): an
        // agent in a background session must not flip the person's visible
        // pane — its session's saved state below is all that changes, and
        // it shows when the person switches there.
        let active = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
        let foreground = agent_open_flips_visible_pane(active.as_deref(), session_id);
        if foreground {
            self.right_snap = false;
            self.layout.right_kind = Some(RightKind::Browser);
            self.layout.right_open = true;
            crate::layout::write(&self.layout);
        }
        let mut right =
            self.overrides.get(session_id).and_then(|meta| meta.right.clone()).unwrap_or_default();
        right.open = true;
        right.kind = RightKind::Browser;
        self.set_override(session_id, |meta| meta.right = Some(right), cx);
        self.refresh_right_now(cx);
        cx.notify();
    }
}

/// Whether an agent's `browser_open` for `session_id` changes what is on
/// screen: only when that session is the one showing. A background
/// session's agent changes only its own saved pane state.
pub(crate) fn agent_open_flips_visible_pane(active: Option<&str>, session_id: &str) -> bool {
    active == Some(session_id)
}

/// Select `id` for `root` from the tree's action closure, which only holds
/// `&mut App`: upgrade the weak [`Harness`] and run the preview through it.
/// Silent when the window is already gone.
pub(crate) fn select_file_preview(harness: WeakEntity<Harness>, root: PathBuf, id: String, cx: &mut App) {
    let _ = harness.update(cx, |this, cx| this.begin_file_preview_for(&root, &id, cx));
}

/// Open `path` in its default app: a file in its editor or viewer, a
/// folder in Finder. Only an existing path opens — never executed, never
/// created — through the OS dispatch.
fn open_path(path: &Path, cx: &mut App) {
    if std::fs::metadata(path).is_ok() {
        cx.open_with_system(path);
    }
}

/// Select `path` in Finder: the card's "Reveal in Finder" control. A
/// missing path does nothing.
fn reveal_in_finder(path: &Path) {
    if std::fs::metadata(path).is_ok() {
        let _ = Command::new("open").arg("-R").arg(path).spawn();
    }
}

/// The transient placeholder while the background re-read is still out: the
/// same shape as [`empty_state`], under its own ids so it can never collide
/// with a baselined settled state.
fn loading_state(id: &'static str, pane: &'static str, heading: &'static str, detail: &'static str) -> AnyElement {
    empty_state(id, pane, heading, detail)
}

/// A labelled placeholder: never a blank pane, always a role and a label.
fn empty_state(id: &'static str, pane: &'static str, heading: &'static str, detail: &'static str) -> AnyElement {
    v_flex()
        .id(id)
        .role(gpui::Role::Group)
        .aria_label(pane)
        .size_full()
        .p(px(16.0))
        .gap(px(8.0))
        .child(div().child(heading))
        .child(
            div()
                .id((ElementId::from(id), "detail"))
                .role(gpui::Role::Label)
                .aria_label(format!("{pane} detail"))
                .child(detail),
        )
        .into_any_element()
}

// ── the inert-action path ────────────────────────────────────────────────

/// The exact stderr line [`inert`] emits, split out so tests can assert it
/// without capturing stderr.
fn inert_text(pane: &'static str, action: &str) -> String {
    format!("right pane: {pane} action {action} is not wired yet")
}

/// Record of every [`inert`] call, test builds only: the app's stderr log
/// cannot be asserted from a test, so the message is also pushed here.
#[cfg(test)]
fn inert_log_store() -> &'static std::sync::Mutex<Vec<String>> {
    static LOG: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    LOG.get_or_init(|| std::sync::Mutex::new(Vec::new()))
}

/// What [`inert`] recorded so far, oldest first.
#[cfg(test)]
pub(crate) fn inert_log() -> Vec<String> {
    inert_log_store().lock().map(|log| log.clone()).unwrap_or_default()
}

/// Forget what [`inert`] recorded.
#[cfg(test)]
pub(crate) fn clear_inert_log() {
    if let Ok(mut log) = inert_log_store().lock() {
        log.clear();
    }
}

/// How an inert action raises its toast: the title and body, plus the app to
/// raise it in. A closure over [`toast_sink`] in production, a recording stub
/// in tests.
type ToastSink = Rc<dyn Fn(String, String, &mut App)>;

/// Build the production [`ToastSink`]: upgrade the weak `Harness` at click
/// time — when no update borrow is held — and toast through its overlays.
/// Nothing happens when the window is already gone, apart from the log line.
fn toast_sink(weak: WeakEntity<Harness>) -> ToastSink {
    Rc::new(move |title, body, cx| {
        if let Some(harness) = weak.upgrade() {
            harness.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| overlays.toast(title, body));
            });
        }
    })
}

/// The one shared inert handler: log the attempt on stderr and raise the
/// app's existing toast naming the action. Returns the log line so tests can
/// see it. Never shells out, never mutates: Commit and Push land here too.
fn inert(pane: &'static str, action: &str, notify: &ToastSink, cx: &mut App) -> String {
    let line = inert_text(pane, action);
    crate::baaz_log!("{line}");
    #[cfg(test)]
    if let Ok(mut log) = inert_log_store().lock() {
        log.push(line.clone());
    }
    notify(
        "Not wired yet".to_string(),
        format!("{action} in the {pane} pane is not wired yet, so nothing happened."),
        cx,
    );
    line
}

/// Exhaustive action names: no `_` arm, so the compiler tells the next
/// person when the library adds a variant.
fn files_action_name(action: &FileTreeAction) -> String {
    match action {
        FileTreeAction::Select(id) => format!("Select {id}"),
        FileTreeAction::Toggle(id) => format!("Toggle {id}"),
        FileTreeAction::Search => "Search".to_string(),
        FileTreeAction::Refresh => "Refresh".to_string(),
    }
}

fn git_action_name(action: &GitAction) -> String {
    match action {
        GitAction::Toggle(path) => format!("Toggle {path}"),
        GitAction::Regenerate => "Regenerate".to_string(),
        GitAction::Amend => "Amend".to_string(),
        GitAction::Commit => "Commit".to_string(),
        GitAction::Push => "Push".to_string(),
    }
}

fn pr_action_name(action: &PrAction) -> String {
    match action {
        PrAction::PickBase => "PickBase".to_string(),
        PrAction::Cancel => "Cancel".to_string(),
        PrAction::Create => "Create".to_string(),
        PrAction::Linear => "Linear".to_string(),
    }
}

fn diff_action_name(action: &DiffReviewAction) -> String {
    match action {
        DiffReviewAction::Scope(scope) => format!("Scope {}", scope.label()),
        DiffReviewAction::View(view) => format!("View {}", view.label()),
        DiffReviewAction::Search => "Search".to_string(),
        DiffReviewAction::CollapseAll => "CollapseAll".to_string(),
        DiffReviewAction::SelectFile(path) => format!("SelectFile {path}"),
        DiffReviewAction::OpenInEditor => "OpenInEditor".to_string(),
        DiffReviewAction::Stage => "Stage".to_string(),
        DiffReviewAction::AddNote(line) => format!("AddNote {line}"),
        DiffReviewAction::EditNote(index) => format!("EditNote {index}"),
        DiffReviewAction::DeleteNote(index) => format!("DeleteNote {index}"),
        DiffReviewAction::SaveNote(index) => format!("SaveNote {index}"),
        DiffReviewAction::CancelNote(index) => format!("CancelNote {index}"),
        DiffReviewAction::Clear => "Clear".to_string(),
        DiffReviewAction::Send => "Send".to_string(),
    }
}

/// One handler per component, each a thin call to [`inert`] — except the
/// Files pane's, which wires `Refresh`, `Toggle` and `Select` to the real
/// views and leaves only `Search` inert. Factored as named functions (rather
/// than inline closures) so tests can invoke the exact handler the pane
/// wires up.
fn files_handler(
    notify: ToastSink,
    harness: WeakEntity<Harness>,
    root: PathBuf,
) -> impl Fn(&FileTreeAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| match action {
        FileTreeAction::Refresh => refresh_files(harness.clone(), cx),
        FileTreeAction::Toggle(id) => {
            let id = id.to_string();
            let root = root.clone();
            let _ = harness.update(cx, |this, cx| this.toggle_files_dir(&root, &id, cx));
        }
        FileTreeAction::Select(id) => {
            select_file_preview(harness.clone(), root.clone(), id.to_string(), cx);
        }
        FileTreeAction::Search => {
            inert("Files", &files_action_name(action), &notify, cx);
        }
    }
}

fn git_handler(notify: ToastSink) -> impl Fn(GitAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Changes", &git_action_name(&action), &notify, cx);
    }
}

fn pr_handler(notify: ToastSink) -> impl Fn(PrAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Pull request", &pr_action_name(&action), &notify, cx);
    }
}

fn diff_handler(notify: ToastSink) -> impl Fn(DiffReviewAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| {
        inert("Diff review", &diff_action_name(&action), &notify, cx);
    }
}

// ── read-only git ────────────────────────────────────────────────────────

/// Run one read-only git command in `root`, giving up after [`GIT_TIMEOUT`].
/// `None` covers everything that can go wrong: no git binary, not a repo,
/// a hung subprocess. The pane degrades to an empty state instead.
fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    note_io();
    let root = root.to_path_buf();
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let output = Command::new("git")
            .arg("-C")
            .arg(&root)
            .args(&args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();
        let _ = tx.send(output);
    });
    match rx.recv_timeout(GIT_TIMEOUT) {
        Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout).ok(),
        _ => None,
    }
}

/// Strip one layer of C-style quoting from a git pathname.
fn unquote(path: &str) -> &str {
    let path = path.trim();
    if path.len() >= 2 && path.starts_with('"') && path.ends_with('"') {
        &path[1..path.len() - 1]
    } else {
        path
    }
}

/// One porcelain v1 line into `(path, staged, kind)`. `staged` is whether the
/// index column is non-space. Renames report the new path. `None` for lines
/// with nothing to show (ignored files, blank lines, malformed rows).
fn parse_porcelain_line(line: &str) -> Option<(String, bool, ChangeKind)> {
    let bytes = line.as_bytes();
    if bytes.len() < 4 || bytes[2] != b' ' {
        return None;
    }
    let index = bytes[0] as char;
    let worktree = bytes[1] as char;
    if index == '!' {
        return None;
    }
    let raw = line[3..].split(" -> ").last().unwrap_or(&line[3..]);
    let path = unquote(raw);
    if path.is_empty() {
        return None;
    }
    let staged = index != ' ' && index != '?';
    let kind = if index == 'A' || worktree == 'A' || index == '?' || worktree == '?' {
        ChangeKind::Added
    } else if index == 'D' || worktree == 'D' {
        ChangeKind::Deleted
    } else {
        ChangeKind::Modified
    };
    Some((path.to_string(), staged, kind))
}

/// One `--numstat` output into path → (added, removed). Binary files report
/// `- -` and count as no line changes.
fn parse_numstat(text: &str) -> HashMap<String, (u32, u32)> {
    let mut counts = HashMap::new();
    for line in text.lines() {
        let mut fields = line.split('\t');
        let (Some(added), Some(removed), Some(path)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let path = path.split(" => ").last().unwrap_or(path);
        let path = unquote(path).to_string();
        if path.is_empty() {
            continue;
        }
        let pair = match (added.parse::<u32>(), removed.parse::<u32>()) {
            (Ok(added), Ok(removed)) => (added, removed),
            _ => (0, 0),
        };
        counts.insert(path, pair);
    }
    counts
}

/// Parse `rev-list --count --left-right upstream...HEAD`: the left count is
/// behind, the right count is ahead. `None` when there is no upstream, which
/// the caller falls back to `(0, 0)`.
fn parse_ahead_behind(text: &str) -> Option<(u32, u32)> {
    let mut fields = text.split_whitespace();
    let behind: u32 = fields.next()?.parse().ok()?;
    let ahead: u32 = fields.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// Everything the Git pane reads, or `None` when the project is not a git
/// repo at all.
#[derive(Clone, Debug)]
pub(crate) struct GitStatus {
    files: Vec<(FileChange, bool)>,
    branch: String,
    ahead: u32,
    behind: u32,
    added: u32,
    removed: u32,
}

/// A fixed working tree for deterministic captures.
///
/// The git and diff panes read the real repository, which is exactly what
/// makes them useful and exactly what makes them impossible to baseline: the
/// screenshot changes with every commit, so the entries would report findings
/// for ever and teach everyone to ignore them. Under `BAAZ_DETERMINISTIC`
/// they read this instead, so the baseline pins how the pane RENDERS rather
/// than what the repository happens to contain today.
fn fixture_git_status() -> GitStatus {
    let files = vec![
        (FileChange { path: "crates/baaz/src/right.rs".into(), change: ChangeKind::Modified, added: 148, removed: 12 }, false),
        (FileChange { path: "crates/baaz/src/layout.rs".into(), change: ChangeKind::Modified, added: 31, removed: 4 }, true),
        (FileChange { path: "docs/02-app.md".into(), change: ChangeKind::Modified, added: 22, removed: 9 }, false),
        (FileChange { path: "crates/baaz/src/panes/mod.rs".into(), change: ChangeKind::Added, added: 64, removed: 0 }, false),
        (FileChange { path: "scripts/old_probe.py".into(), change: ChangeKind::Deleted, added: 0, removed: 37 }, false),
    ];
    let added = files.iter().map(|(change, _)| change.added).sum();
    let removed = files.iter().map(|(change, _)| change.removed).sum();
    GitStatus { files, branch: "right-pane".into(), ahead: 2, behind: 1, added, removed }
}

/// The diff the fixture's first file shows. Small on purpose: a capture wants
/// a legible hunk, not a realistic one.
fn fixture_diff() -> Diff {
    let line = |kind, old_no, new_no, text: &str| DiffLine { kind, old_no, new_no, text: text.to_string() };
    Diff {
        path: "crates/baaz/src/right.rs".into(),
        hunks: vec![Hunk {
            header: "@@ -41,7 +41,9 @@ fn render(kind: RightKind)".into(),
            lines: vec![
                line(DiffKind::Context, Some(41), Some(41), "    let notify = toast_sink(cx.weak_entity());"),
                line(DiffKind::Del, Some(42), None, "    match kind {"),
                line(DiffKind::Add, None, Some(42), "    match kind {"),
                line(DiffKind::Add, None, Some(43), "        RightKind::Browser => browser_pane(&notify),"),
                line(DiffKind::Context, Some(43), Some(44), "        RightKind::Files => files_pane(root, &notify),"),
            ],
        }],
        added: 2,
        removed: 1,
    }
}

fn read_git_status(root: &Path) -> Option<GitStatus> {
    if crate::clock::deterministic() {
        return Some(fixture_git_status());
    }
    let porcelain = run_git(root, &["status", "--porcelain=v1", "--untracked-files=normal"])?;
    let branch = run_git(root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let (ahead, behind) = run_git(root, &["rev-list", "--count", "--left-right", "@{upstream}...HEAD"])
        .and_then(|out| parse_ahead_behind(&out))
        .unwrap_or((0, 0));
    let unstaged = run_git(root, &["diff", "--numstat"]).map(|out| parse_numstat(&out)).unwrap_or_default();
    let staged = run_git(root, &["diff", "--cached", "--numstat"])
        .map(|out| parse_numstat(&out))
        .unwrap_or_default();
    let mut files = Vec::new();
    let mut added = 0_u32;
    let mut removed = 0_u32;
    for line in porcelain.lines() {
        let Some((path, is_staged, kind)) = parse_porcelain_line(line) else {
            continue;
        };
        let counts = if is_staged {
            staged.get(&path).or_else(|| unstaged.get(&path))
        } else {
            unstaged.get(&path)
        };
        let (file_added, file_removed) = counts.copied().unwrap_or((0, 0));
        added += file_added;
        removed += file_removed;
        files.push((
            FileChange { path, change: kind, added: file_added, removed: file_removed },
            is_staged,
        ));
    }
    files.sort_by(|a, b| a.0.path.cmp(&b.0.path));
    Some(GitStatus { files, branch, ahead, behind, added, removed })
}

// ── the file walk ────────────────────────────────────────────────────────

/// The row icon for a file name, by extension.
fn file_type_for(name: &str) -> FileType {
    let lower = name.to_ascii_lowercase();
    if lower.contains(".test.") || lower.contains(".spec.") {
        FileType::Test
    } else if lower.ends_with(".tsx") {
        FileType::Tsx
    } else if lower.ends_with(".ts") {
        FileType::Ts
    } else if lower.ends_with(".json") || lower.ends_with(".lock") {
        if lower.ends_with(".lock") { FileType::Lock } else { FileType::Json }
    } else if lower.ends_with(".md") {
        FileType::Md
    } else if lower.ends_with(".css") {
        FileType::Css
    } else if lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".svg")
        || lower.ends_with(".webp")
        || lower.ends_with(".ico")
    {
        FileType::Image
    } else {
        FileType::File
    }
}

/// One level of `root`, sorted by name, skipping [`SKIP_DIRS`]. Returns the
/// nodes and whether the [`FILE_WALK_CAP`] cut the listing short.
#[cfg(test)]
fn walk_root_capped(root: &Path, cap: usize) -> (Vec<FileNode>, bool) {
    walk_root_expanded(root, cap, &HashSet::new())
}

/// [`walk_root_capped`] with the session's expansion state: every id in
/// `expanded` walks one level deeper, recursively, so an open directory
/// shows its children under it. Ids are root-relative paths (`sub`,
/// `sub/nested`); names stay the last segment. Directories absent from
/// `expanded` arrive closed. Returns the nodes and whether `cap` cut the
/// listing short. Test builds only: production walks go through
/// [`walk_root_expanded_cached`].
#[cfg(test)]
fn walk_root_expanded(root: &Path, cap: usize, expanded: &HashSet<String>) -> (Vec<FileNode>, bool) {
    let mut cache = DirListCache::default();
    walk_root_expanded_cached(root, cap, expanded, &mut cache)
}

/// [`walk_root_expanded`] through `dir_cache`: a directory whose mtime is
/// unchanged since its last read replays its cached entries instead of a
/// `read_dir` (counted as a cache hit, not a read). A changed directory is
/// re-read, bounded to `cap + 1` entries so a huge folder never fills a Vec
/// that is sorted and truncated afterwards.
pub(crate) fn walk_root_expanded_cached(
    root: &Path,
    cap: usize,
    expanded: &HashSet<String>,
    dir_cache: &mut DirListCache,
) -> (Vec<FileNode>, bool) {
    dir_cache.for_root(root);
    let mut nodes = Vec::new();
    let mut truncated = false;
    walk_level_cached(root, root, 0, cap, expanded, dir_cache, &mut nodes, &mut truncated);
    (nodes, truncated)
}

/// One level of `dir` appended to `nodes`: sorted by name, skipping
/// [`SKIP_DIRS`] by name at every level — whatever the entry is on disk,
/// so a worktree's file-shaped `.git` never lists. Stops appending once
/// `nodes` reaches `cap` and reports it in `truncated`. A directory over
/// the cap contributes its first `cap` entries in the sorted order of the
/// bounded read, and the pane's truncation note still applies.
fn walk_level_cached(
    root: &Path,
    dir: &Path,
    depth: usize,
    cap: usize,
    expanded: &HashSet<String>,
    dir_cache: &mut DirListCache,
    nodes: &mut Vec<FileNode>,
    truncated: &mut bool,
) {
    if nodes.len() >= cap {
        *truncated = true;
        return;
    }
    // The stamp is one cheap `stat`: only a changed directory pays for a
    // `read_dir` below. The stat itself never moves the I/O counter, so a
    // fully cached refresh reads nothing countable.
    let mtime = std::fs::metadata(dir).and_then(|meta| meta.modified()).ok();
    if let Some(hit) = dir_cache.dirs.get(dir) {
        if hit.mtime == mtime && hit.cap == cap {
            note_cache_hit();
            let entries = hit.entries.clone();
            let dir_truncated = hit.dir_truncated;
            append_cached_entries(root, dir, depth, cap, expanded, dir_cache, &entries, dir_truncated, nodes, truncated);
            return;
        }
    }
    note_io();
    // Bounded: at most `cap + 1` entries are ever collected — the `+ 1` is
    // the probe that tells a full directory from a truncated one. A
    // directory under the cap reads in full, so its order is exactly what
    // the old sort-and-truncate showed.
    let mut entries: Vec<(String, bool)> = Vec::new();
    let mut dir_truncated = false;
    match std::fs::read_dir(dir) {
        Ok(read) => {
            for entry in read.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                if entries.len() <= cap {
                    let is_dir = entry.file_type().as_ref().is_ok_and(|kind| kind.is_dir());
                    entries.push((name, is_dir));
                    note_collected();
                } else {
                    dir_truncated = true;
                    break;
                }
            }
        }
        Err(_) => return,
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries.len() > cap {
        entries.truncate(cap);
        dir_truncated = true;
    }
    dir_cache.dirs.insert(
        dir.to_path_buf(),
        CachedDir { mtime, cap, entries: entries.clone(), dir_truncated },
    );
    append_cached_entries(root, dir, depth, cap, expanded, dir_cache, &entries, dir_truncated, nodes, truncated);
}

/// Append one directory's `(name, is_dir)` entries to `nodes`, recursing
/// into open directories. Shared by fresh reads and cache replays so both
/// draw the same rows.
#[allow(clippy::too_many_arguments)]
fn append_cached_entries(
    root: &Path,
    dir: &Path,
    depth: usize,
    cap: usize,
    expanded: &HashSet<String>,
    dir_cache: &mut DirListCache,
    entries: &[(String, bool)],
    dir_truncated: bool,
    nodes: &mut Vec<FileNode>,
    truncated: &mut bool,
) {
    if dir_truncated {
        *truncated = true;
    }
    for (name, is_dir) in entries {
        if nodes.len() >= cap {
            *truncated = true;
            return;
        }
        let id = dir
            .strip_prefix(root)
            .ok()
            .map(|relative| relative.join(name))
            .and_then(|relative| relative.to_str().map(str::to_string))
            .unwrap_or_else(|| name.clone());
        if *is_dir {
            let open = expanded.contains(&id);
            nodes.push(FileNode::dir(id.clone(), name.clone(), open).depth(depth));
            if open {
                walk_level_cached(root, &dir.join(name), depth + 1, cap, expanded, dir_cache, nodes, truncated);
                if nodes.len() >= cap {
                    *truncated = true;
                    return;
                }
            }
        } else {
            let kind = file_type_for(name);
            nodes.push(FileNode::file(id, name.clone(), kind).depth(depth));
        }
    }
}

/// Mark the node carrying `selected` (a root-relative id) selected: what
/// the tree draws after the preview closes.
fn mark_selected(nodes: Vec<FileNode>, selected: Option<&str>) -> Vec<FileNode> {
    let Some(selected) = selected else {
        return nodes;
    };
    nodes
        .into_iter()
        .map(|node| {
            let marked = node.id.as_ref() == selected;
            node.selected(marked)
        })
        .collect()
}

// ── the file preview ───────────────────────────────────────────────────

/// The `code_block` language label for a file name, by extension.
pub(crate) fn preview_language(name: &str) -> String {
    let lower = name.to_ascii_lowercase();
    let extension = lower.rsplit('.').next().unwrap_or("");
    // A leading-dot name (`.gitignore`) has no extension: `rsplit` returns
    // the whole name, which must not read as a language.
    let extension = if lower.len() > extension.len() + 1 { extension } else { "" };
    match extension {
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "tsx" => "tsx",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "sh" | "bash" | "zsh" => "bash",
        "css" => "css",
        "html" | "htm" => "html",
        "xml" | "svg" => "xml",
        "c" | "h" => "c",
        "cpp" | "hpp" | "cc" => "cpp",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "kt" => "kotlin",
        "swift" => "swift",
        "sql" => "sql",
        "lua" => "lua",
        "lock" => "lock",
        "" => "text",
        other => other,
    }
    .to_string()
}

/// Whether `name` previews as a document rather than as code.
fn is_markdown(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".md") || lower.ends_with(".markdown")
}

/// Whether `name` previews as an image card rather than as code.
fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".png")
        || lower.ends_with(".jpg")
        || lower.ends_with(".jpeg")
        || lower.ends_with(".gif")
        || lower.ends_with(".webp")
        || lower.ends_with(".ico")
}

/// Flip the directory `id` open or closed for `root`. Returns the new
/// state: `true` when the directory now stands open.
pub(crate) fn toggle_expanded(cache: &mut RightCache, root: &Path, id: &str) -> bool {
    let open = cache.expanded.entry(root.to_path_buf()).or_default();
    if open.remove(id) {
        false
    } else {
        open.insert(id.to_string());
        true
    }
}

/// Forget the open preview for `root`, keeping the selected marker so the
/// tree still shows which file was previewed. Returns whether one was open.
pub(crate) fn close_file_preview(cache: &mut RightCache, root: &Path) -> bool {
    cache.previews.remove(root).is_some()
}

/// Start previewing `id` for `root`: the tree's selected marker moves at
/// once, and a text file under [`PREVIEW_MAX_BYTES`] opens in `Loading`
/// for the background read to complete. A directory toggles instead and
/// clears any preview; a missing, oversized or unreadable path opens the
/// card straight away. Returns whether the caller must read the file off
/// the render path and land it with [`complete_file_preview`].
pub(crate) fn begin_file_preview(cache: &mut RightCache, root: &Path, id: &str) -> bool {
    let path = root.join(id);
    let name = id.rsplit('/').next().unwrap_or(id).to_string();
    let meta = std::fs::metadata(&path);
    let Ok(meta) = meta else {
        cache.selected.insert(root.to_path_buf(), id.to_string());
        cache.previews.insert(
            root.to_path_buf(),
            FilePreview {
                path: id.to_string(),
                name,
                language: preview_language(id),
                size: 0,
                mtime: None,
                content: PreviewContent::Card { meta: "The file is gone — it may have been moved or deleted.".into() },
            },
        );
        return false;
    };
    if meta.is_dir() {
        toggle_expanded(cache, root, id);
        cache.previews.remove(root);
        return false;
    }
    cache.selected.insert(root.to_path_buf(), id.to_string());
    let size = meta.len();
    let mtime = meta.modified().ok();
    if size > PREVIEW_MAX_BYTES {
        cache.previews.insert(
            root.to_path_buf(),
            FilePreview {
                path: id.to_string(),
                name,
                language: preview_language(id),
                size,
                mtime,
                content: PreviewContent::Card {
                    meta: format!("{} — too large to preview", friendly_size(size)),
                },
            },
        );
        return false;
    }
    cache.previews.insert(
        root.to_path_buf(),
        FilePreview {
            path: id.to_string(),
            name,
            language: preview_language(id),
            size,
            mtime,
            content: PreviewContent::Loading,
        },
    );
    true
}

/// Land a background read started by [`begin_file_preview`]: `None` is a
/// failed read, non-UTF-8 bytes are binary — both get the card. Stale
/// landings (the selection moved on, or the preview closed) change
/// nothing. Returns whether a preview was updated.
pub(crate) fn complete_file_preview(
    cache: &mut RightCache,
    root: &Path,
    id: &str,
    bytes: Option<Vec<u8>>,
) -> bool {
    let Some(current) = cache.previews.get(root) else {
        return false;
    };
    if current.path != id || current.content != PreviewContent::Loading {
        return false;
    }
    let name = current.name.clone();
    let language = current.language.clone();
    let content = match bytes {
        None => PreviewContent::Card { meta: "The file could not be read.".into() },
        Some(bytes) if bytes.len() as u64 > PREVIEW_MAX_BYTES => PreviewContent::Card {
            meta: format!("{} — too large to preview", friendly_size(bytes.len() as u64)),
        },
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) if is_markdown(&name) => PreviewContent::Markdown { text },
            Ok(code) => PreviewContent::Text { language: language.clone(), code },
            Err(_) => PreviewContent::Card { meta: format!("{} — a binary file", friendly_size(current.size)) },
        },
    };
    cache.previews.insert(
        root.to_path_buf(),
        FilePreview {
            path: id.to_string(),
            name,
            language,
            size: current.size,
            mtime: current.mtime,
            content,
        },
    );
    true
}

/// What the background refresh learned about the open preview: re-read it.
/// `bytes` is `None` when the file is gone, is a directory now, is too
/// large, or could not be read — [`apply_preview_reload`] lets
/// [`begin_file_preview`] decide the card in those cases.
#[derive(Clone, Debug)]
pub(crate) struct PreviewReload {
    /// The tree id to re-read, matching the preview's path.
    pub id: String,
    /// The file's fresh bytes, when it is small enough to preview.
    pub bytes: Option<Vec<u8>>,
}

/// Decide off the render path whether the open preview changed on disk:
/// `Some` when its mtime or size moved (or it vanished), `None` when it is
/// unchanged — or still landing its first read, which the refresh must not
/// disturb. Blocking on one stat plus at most one capped read: the refresh
/// path runs it on its background task.
pub(crate) fn preview_reload_for(root: &Path, preview: &FilePreview) -> Option<PreviewReload> {
    if preview.content == PreviewContent::Loading {
        return None;
    }
    let path = root.join(&preview.path);
    let meta = match std::fs::metadata(&path) {
        Ok(meta) => meta,
        Err(_) => {
            // Gone now: reload only while the preview still shows bytes from
            // when it existed (`mtime` set) — never loop a gone card.
            return if preview.mtime.is_some() {
                Some(PreviewReload { id: preview.path.clone(), bytes: None })
            } else {
                None
            };
        }
    };
    if meta.is_dir() {
        return if preview.mtime.is_some() {
            Some(PreviewReload { id: preview.path.clone(), bytes: None })
        } else {
            None
        };
    }
    let size = meta.len();
    let mtime = meta.modified().ok();
    if mtime == preview.mtime && size == preview.size {
        return None;
    }
    let bytes = if size > PREVIEW_MAX_BYTES { None } else { std::fs::read(&path).ok() };
    Some(PreviewReload { id: preview.path.clone(), bytes })
}

/// The preview's shown text, if it shows any: the code for text files, the
/// source for markdown — `None` for loading and card previews. The refresh
/// tests read through this instead of destructuring [`PreviewContent`].
#[cfg(test)]
pub(crate) fn preview_text_shown(preview: &FilePreview) -> Option<&str> {
    match &preview.content {
        PreviewContent::Text { code, .. } => Some(code),
        PreviewContent::Markdown { text } => Some(text),
        _ => None,
    }
}

/// Land a [`preview_reload_for`] decision: re-run the preview load for its
/// id through [`begin_file_preview`]/[`complete_file_preview`], so a
/// changed file shows its new bytes and a deleted one shows the existing
/// not-found card instead of stale text. The stale-landing guard still
/// holds: a preview that moved on (or closed) is left alone. Returns whether
/// a preview was updated.
pub(crate) fn apply_preview_reload(cache: &mut RightCache, root: &Path, reload: PreviewReload) -> bool {
    let Some(current) = cache.preview_for(root) else {
        return false;
    };
    if current.path != reload.id || current.content == PreviewContent::Loading {
        return false;
    }
    if begin_file_preview(cache, root, &reload.id) {
        complete_file_preview(cache, root, &reload.id, reload.bytes);
    }
    // `begin` returning false already landed the gone/too-large card itself.
    true
}

/// `1536` reads as `1.5 KB`: the card's meta line for sizes.
fn friendly_size(size: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{size} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// The `.md` bytes as document blocks: `#`-led lines become headings,
/// blank-line-separated runs become paragraphs, capped at
/// [`PREVIEW_MD_BLOCKS_CAP`] so a huge note stays a pane, not a freeze.
fn markdown_blocks(text: &str) -> Vec<DocBlock> {
    let mut blocks = Vec::new();
    let mut paragraph = String::new();
    let push_paragraph = |paragraph: &mut String, blocks: &mut Vec<DocBlock>| {
        let trimmed = paragraph.trim();
        if !trimmed.is_empty() && blocks.len() < PREVIEW_MD_BLOCKS_CAP {
            blocks.push(DocBlock::text(trimmed.to_string()));
        }
        paragraph.clear();
    };
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            push_paragraph(&mut paragraph, &mut blocks);
            continue;
        }
        let hashes = trimmed.bytes().take_while(|byte| *byte == b'#').count();
        if hashes > 0 && hashes <= 3 && trimmed[hashes..].starts_with(' ') {
            push_paragraph(&mut paragraph, &mut blocks);
            if blocks.len() < PREVIEW_MD_BLOCKS_CAP {
                blocks.push(DocBlock::heading(hashes as u8, trimmed[hashes + 1..].trim().to_string()));
            }
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push('\n');
        }
        paragraph.push_str(trimmed);
    }
    push_paragraph(&mut paragraph, &mut blocks);
    if blocks.is_empty() {
        blocks.push(DocBlock::text("(empty note)"));
    }
    blocks
}

// ── the unified-diff parser ──────────────────────────────────────────────

/// What [`parse_unified_diff`] kept, and whether the caps cut anything.
#[derive(Clone, Debug)]
pub(crate) struct ParsedDiffs {
    diffs: Vec<Diff>,
    truncated: bool,
}

/// Parse `@@ -old[,len] +new[,len] @@` into its two start lines.
fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let mut fields = header.split_whitespace();
    if fields.next()? != "@@" {
        return None;
    }
    let old = fields.next()?.strip_prefix('-')?;
    let new = fields.next()?.strip_prefix('+')?;
    let old_start: u32 = old.split(',').next()?.parse().ok()?;
    let new_start: u32 = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Strip the `a/` or `b/` prefix git puts on diff paths.
fn strip_diff_prefix(path: &str) -> &str {
    path.strip_prefix("b/").or_else(|| path.strip_prefix("a/")).unwrap_or(path)
}

/// Parse a working-tree `git diff` into per-file [`Diff`]s, capped at
/// [`DIFF_FILES_CAP`] files and [`DIFF_LINES_PER_FILE_CAP`] body lines each.
/// Defensive throughout: binary diffs, renames, new empty files and missing
/// trailing newlines all parse without panicking.
fn parse_unified_diff(text: &str) -> ParsedDiffs {
    let mut diffs: Vec<Diff> = Vec::new();
    let mut truncated = false;
    let mut path: Option<String> = None;
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut added = 0_u32;
    let mut removed = 0_u32;
    let mut kept_lines = 0_usize;
    let mut old_no = 1_u32;
    let mut new_no = 1_u32;

    // The per-file M/A/D kind is re-derived from porcelain, not from the
    // diff: `new file mode` / `deleted file mode` lines carry no rows, and
    // the `--- /dev/null` / `+++ /dev/null` markers below only disambiguate
    // paths, so neither is tracked here.
    let finish_file = |path: &mut Option<String>,
                           hunks: &mut Vec<Hunk>,
                           added: &mut u32,
                           removed: &mut u32,
                           kept_lines: &mut usize,
                           diffs: &mut Vec<Diff>| {
        if let Some(path) = path.take() {
            diffs.push(Diff { path, hunks: std::mem::take(hunks), added: *added, removed: *removed });
        }
        *added = 0;
        *removed = 0;
        *kept_lines = 0;
    };

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("diff --git ") {
            finish_file(&mut path, &mut hunks, &mut added, &mut removed, &mut kept_lines, &mut diffs);
            if diffs.len() >= DIFF_FILES_CAP {
                truncated = true;
                break;
            }
            let mut sides = rest.split(' ');
            let old_side = sides.next().unwrap_or("");
            let new_side = sides.next().unwrap_or("");
            let next = if new_side == "/dev/null" { old_side } else { new_side };
            let next = strip_diff_prefix(next);
            if !next.is_empty() {
                path = Some(next.to_string());
            }
        } else if line.starts_with("new file mode") || line.starts_with("deleted file mode") {
            // Kind only; the rows below carry the content.
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            path = Some(strip_diff_prefix(rest.trim()).to_string());
        } else if line.starts_with("Binary files ") {
            // Nothing follows for this file: zero hunks, zero counts.
        } else if line.starts_with("--- ") {
            // `/dev/null` here means a new file; the path came from `diff --git`.
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            let side = rest.trim();
            if side != "/dev/null" && path.is_none() {
                path = Some(strip_diff_prefix(side).to_string());
            }
        } else if line.starts_with("@@") {
            if path.is_none() {
                continue;
            }
            let (old_start, new_start) = parse_hunk_header(line).unwrap_or((1, 1));
            old_no = old_start;
            new_no = new_start;
            hunks.push(Hunk { header: line.to_string(), lines: Vec::new() });
        } else if line.starts_with('\\') {
            // `\ No newline at end of file`: a marker, not a row.
        } else if path.is_some() && !hunks.is_empty() {
            let (kind, text) = if let Some(rest) = line.strip_prefix('+') {
                (DiffKind::Add, rest)
            } else if let Some(rest) = line.strip_prefix('-') {
                (DiffKind::Del, rest)
            } else if let Some(rest) = line.strip_prefix(' ') {
                (DiffKind::Context, rest)
            } else if line.is_empty() {
                // A context row whose trailing space did not survive.
                (DiffKind::Context, line)
            } else {
                // `index …`, `similarity …`, mode lines: not rows.
                continue;
            };
            if kept_lines >= DIFF_LINES_PER_FILE_CAP {
                truncated = true;
                continue;
            }
            kept_lines += 1;
            let (old, new) = match kind {
                DiffKind::Context => {
                    let pair = (Some(old_no), Some(new_no));
                    old_no += 1;
                    new_no += 1;
                    pair
                }
                DiffKind::Add => {
                    added += 1;
                    let pair = (None, Some(new_no));
                    new_no += 1;
                    pair
                }
                DiffKind::Del => {
                    removed += 1;
                    let pair = (Some(old_no), None);
                    old_no += 1;
                    pair
                }
            };
            if let Some(hunk) = hunks.last_mut() {
                hunk.lines.push(DiffLine { kind, old_no: old, new_no: new, text: text.to_string() });
            }
        }
    }
    finish_file(&mut path, &mut hunks, &mut added, &mut removed, &mut kept_lines, &mut diffs);
    ParsedDiffs { diffs, truncated }
}

// ── the four panes ───────────────────────────────────────────────────

/// The file tree, real: one level of the project root plus a level per open
/// directory, already read by the refresh path. Header names the project's
/// directory; the footer counts the nodes and names the cap. `Refresh`
/// re-reads through [`refresh_files`], `Toggle` flips expansion,
/// `Select` previews the file — only `Search` stays inert through
/// [`files_handler`]. While a file is selected the pane previews it instead
/// of the tree, with a header carrying the file name, a back control and
/// an Open control.
fn files_pane_from(
    nodes: &[FileNode],
    truncated: bool,
    name: &str,
    root: &Path,
    cache: &RightCache,
    notify: &ToastSink,
    harness: WeakEntity<Harness>,
) -> AnyElement {
    if let Some(preview) = cache.preview_for(root) {
        return file_preview_pane(&preview, root, cache, harness);
    }
    let count = nodes.len();
    let footer = if truncated {
        format!("{count} shown — capped at the first {FILE_WALK_CAP} entries")
    } else if count == 1 {
        "1 file or folder".to_string()
    } else {
        format!("{count} files and folders")
    };
    let handler = files_handler(notify.clone(), harness, root.to_path_buf());
    // Flush: these are panes and stacked panels, not cards floating on a
    // surface. Their own rounded border inside a column that already has
    // edges is what the owner saw as "extra borders and rounded corners".
    let tree = file_tree("right-files", nodes.to_vec())
        .flush()
        .header(name)
        .footer(footer)
        .on_action(move |action, window, cx| handler(action, window, cx));
    let scroll = cache.files_scroll_for(root);
    v_flex()
        .id("right-files-pane")
        .role(gpui::Role::Group)
        .aria_label("Files")
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .child(tree)
        .into_any_element()
}

/// The preview half of the Files pane: a header naming the file with a
/// back control and an Open control, then the file itself — a `doc_pane`
/// for `.md`, a `code_block` for other text, a `file_card` with Open and
/// Reveal in Finder for binary or oversized files. Every control carries
/// its role and label; no action here toasts.
fn file_preview_pane(
    preview: &FilePreview,
    root: &Path,
    cache: &RightCache,
    harness: WeakEntity<Harness>,
) -> AnyElement {
    let path = root.join(&preview.path);
    let back_harness = harness.clone();
    let back_root = root.to_path_buf();
    let open_target = path.clone();
    let back = button("right-file-back", "Back")
        .accessibility_label("Back to files")
        .on_click(move |_, _, cx| {
            let _ = back_harness.update(cx, |this, cx| {
                this.close_file_preview_for(&back_root, cx);
            });
        });
    let open = button("right-file-open", "Open")
        .accessibility_label(format!("Open {} in its default app", preview.name))
        .on_click(move |_, _, cx| open_path(&open_target, cx));
    let header = h_flex()
        .id("right-file-header")
        .role(gpui::Role::Group)
        .aria_label(format!("Previewing {}", preview.name))
        .w_full()
        .flex_none()
        .items_center()
        .gap(px(8.0))
        .p(px(8.0))
        .child(back)
        .child(
            div()
                .id("right-file-name")
                .role(gpui::Role::Label)
                .aria_label(format!("Previewing {}", preview.name))
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .child(preview.name.clone()),
        )
        .child(open);
    let body: AnyElement = match &preview.content {
        PreviewContent::Loading => div()
            .id("right-file-loading")
            .role(gpui::Role::Label)
            .aria_label(format!("Loading {}", preview.name))
            .p(px(16.0))
            .child(format!("Loading {}…", preview.name))
            .into_any_element(),
        PreviewContent::Text { language, code } => code_block(
            "right-file-code",
            preview.path.clone(),
            code.clone(),
        )
        .language(language.clone())
        .into_any_element(),
        PreviewContent::Markdown { text } => {
            let page = DocPage::new(
                preview.name.clone(),
                preview.path.clone(),
                markdown_blocks(text),
            );
            doc_pane("right-file-doc", page).into_any_element()
        }
        PreviewContent::Card { meta } => {
            let kind = if is_markdown(&preview.name) {
                ArtifactKind::Note
            } else if is_image(&preview.name) {
                ArtifactKind::Image
            } else if preview.language != "text" {
                ArtifactKind::Code
            } else {
                ArtifactKind::Other
            };
            let open_target = path.clone();
            let reveal_path = path.clone();
            file_card("right-file-card", preview.name.clone())
                .kind(kind)
                .meta(meta.clone())
                .action("open", "Open")
                .action("reveal", "Reveal in Finder")
                .on_action(move |action, _, cx| {
                    if action.as_ref() == "open" {
                        open_path(&open_target, cx);
                    } else if action.as_ref() == "reveal" {
                        reveal_in_finder(&reveal_path);
                    }
                })
                .into_any_element()
        }
    };
    let scroll = cache.files_scroll_for(root);
    v_flex()
        .id("right-files-pane")
        .role(gpui::Role::Group)
        .aria_label(format!("File preview: {}", preview.name))
        .size_full()
        .overflow_y_scroll()
        .track_scroll(&scroll)
        .child(header)
        .child(body)
        .into_any_element()
}

/// The browser pane (Z7a): the library's `webview_pane` for the active
/// session's [`WebviewState`](aui_webview::WebviewState) — nav row, page and
/// annotations panel — over role Group labelled "Browser". The nav controls
/// come labelled from the library. Intents forward to the harness:
/// screenshots and annotations land in the active session's composer draft,
/// Console toasts.
fn browser_pane(state: &Entity<aui_webview::WebviewState>, cx: &mut Context<Harness>) -> AnyElement {
    let harness = cx.weak_entity();
    let pane = aui_webview::webview_pane("right-browser", state).on_intent(
        move |intent, window, cx| {
            if let Some(harness) = harness.upgrade() {
                harness.update(cx, |harness, cx| {
                    harness.handle_browser_intent(intent, window, cx);
                });
            }
        },
    );
    v_flex()
        .id("right-browser-pane")
        .role(gpui::Role::Group)
        .aria_label("Browser")
        .size_full()
        .child(pane)
        .into_any_element()
}

/// Real status, inert verbs: the changes panel plus the PR form. The form
/// has no head parameter, so the real branch rides in the description; the
/// checks stay empty rather than fabricating green rows. Commit and Push
/// reach [`inert`] and never shell out. The status arrives already read.
fn git_pane_from(status: &GitStatus, notify: &ToastSink) -> AnyElement {
    let changes = git_changes("right-git", status.files.clone(), "", status.ahead, status.behind)
        .flush()
        .branch(status.branch.clone())
        .on_action(git_handler(notify.clone()));
    let form = pr_form(
        "right-pr",
        "main",
        "",
        vec![PrDescription::Text(SharedString::from(format!("Head branch: {}", status.branch)))],
        Vec::new(),
    )
    .flush()
    .on_action(pr_handler(notify.clone()));
    v_flex()
        .id("right-git-pane")
        .role(gpui::Role::Group)
        .aria_label("Changes")
        .size_full()
        .child(changes)
        .child(form)
        .into_any_element()
}

/// The working tree, read-only: unstaged diff parsed with caps, the first
/// file selected, real numstat totals in the summary. Status and parsed diff
/// arrive already read; a clean tree never reaches here (the caller draws the
/// clean empty state instead).
fn diff_pane_from(status: &GitStatus, parsed: &ParsedDiffs, notify: &ToastSink) -> AnyElement {
    let review_files: Vec<ReviewFile> = status.files.iter().take(DIFF_FILES_CAP).enumerate()
        .map(|(index, (change, _))| ReviewFile { change: change.clone(), notes: 0, selected: index == 0 })
        .collect();
    let first_path = review_files.first().map(|file| file.change.path.clone()).unwrap_or_default();
    let shown = parsed
        .diffs
        .iter()
        .find(|diff| diff.path == first_path)
        .or_else(|| parsed.diffs.first())
        .cloned()
        .unwrap_or(Diff { path: first_path, hunks: Vec::new(), added: 0, removed: 0 });
    let lead = if parsed.truncated {
        format!(
            "Unstaged changes — showing the first {DIFF_FILES_CAP} files, \
             {DIFF_LINES_PER_FILE_CAP} lines each"
        )
    } else {
        "Unstaged changes".to_string()
    };
    let review = diff_review("right-diff", review_files, shown, Vec::new(), DiffScope::Unstaged, DiffView::Unified)
        .flush()
        .fill()
        .summary(lead, status.added, status.removed)
        .on_action(diff_handler(notify.clone()));
    v_flex()
        .id("right-diff-pane")
        .role(gpui::Role::Group)
        .aria_label("Diff review")
        .size_full()
        .child(review)
        .into_any_element()
}

#[cfg(test)]
mod tests {

    #[test]
    fn only_the_foreground_agent_flips_the_visible_pane() {
        // Review finding (Z7b): a background session's agent opening its
        // browser must not flip the person's visible pane.
        assert!(super::agent_open_flips_visible_pane(Some("a"), "a"));
        assert!(!super::agent_open_flips_visible_pane(Some("a"), "b"));
        assert!(!super::agent_open_flips_visible_pane(None, "b"));
    }

    use super::*;
    use gpui::TestAppContext;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("baaz-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // ── the diff parser's four defensive shapes ──

    #[test]
    fn a_binary_file_diff_parses_without_rows() {
        let text = "diff --git a/img.png b/img.png\nnew file mode 100644\nindex 0000000..e69de29\nBinary files /dev/null and b/img.png differ\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "img.png");
        assert!(parsed.diffs[0].hunks.is_empty());
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (0, 0));
    }

    #[test]
    fn a_rename_parses_under_its_new_path() {
        let text = "diff --git a/old.rs b/new.rs\nsimilarity index 90%\nrename from old.rs\nrename to new.rs\nindex 1234567..89abcde 100644\n--- a/old.rs\n+++ b/new.rs\n@@ -1,2 +1,2 @@\n ctx\n-old\n+new\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "new.rs");
        assert_eq!(parsed.diffs[0].hunks.len(), 1);
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (1, 1));
        let lines = &parsed.diffs[0].hunks[0].lines;
        assert_eq!(lines.len(), 3);
        assert_eq!((lines[0].kind, lines[0].old_no, lines[0].new_no), (DiffKind::Context, Some(1), Some(1)));
        assert_eq!((lines[1].kind, lines[1].old_no, lines[1].new_no), (DiffKind::Del, Some(2), None));
        assert_eq!(lines[1].text, "old");
        assert_eq!((lines[2].kind, lines[2].old_no, lines[2].new_no), (DiffKind::Add, None, Some(2)));
        assert_eq!(lines[2].text, "new");
    }

    #[test]
    fn a_new_empty_file_parses_without_hunks() {
        let text =
            "diff --git a/empty.txt b/empty.txt\nnew file mode 100644\nindex 0000000..e69de29\n";
        let parsed = parse_unified_diff(text);
        assert!(!parsed.truncated);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].path, "empty.txt");
        assert!(parsed.diffs[0].hunks.is_empty());
    }

    #[test]
    fn a_missing_trailing_newline_leaves_no_marker_row() {
        let text = "diff --git a/a.txt b/a.txt\nindex 1234567..89abcde 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n";
        let parsed = parse_unified_diff(text);
        assert_eq!(parsed.diffs.len(), 1);
        let lines = &parsed.diffs[0].hunks[0].lines;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "old");
        assert_eq!(lines[1].text, "new");
        assert_eq!((parsed.diffs[0].added, parsed.diffs[0].removed), (1, 1));
    }

    #[test]
    fn the_diff_caps_files_and_lines_per_file() {
        let mut text = String::new();
        for index in 0..(DIFF_FILES_CAP + 4) {
            text.push_str(&format!("diff --git a/f{index}.txt b/f{index}.txt\n--- a/f{index}.txt\n+++ b/f{index}.txt\n@@ -1 +1 @@\n-x\n+y\n"));
        }
        let parsed = parse_unified_diff(&text);
        assert_eq!(parsed.diffs.len(), DIFF_FILES_CAP);
        assert!(parsed.truncated);

        let mut one = String::from("diff --git a/big.txt b/big.txt\n--- a/big.txt\n+++ b/big.txt\n@@ -1 +1 @@\n");
        for _ in 0..(DIFF_LINES_PER_FILE_CAP + 50) {
            one.push_str("+y\n");
        }
        let parsed = parse_unified_diff(&one);
        assert_eq!(parsed.diffs.len(), 1);
        assert_eq!(parsed.diffs[0].hunks[0].lines.len(), DIFF_LINES_PER_FILE_CAP);
        assert!(parsed.truncated);
    }

    // ── porcelain, numstat and the fallbacks ──

    #[test]
    fn porcelain_letters_map_to_kinds_and_staged_flags() {
        let (path, staged, kind) = parse_porcelain_line("M  src/a.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("src/a.rs", true, ChangeKind::Modified));
        let (path, staged, kind) = parse_porcelain_line(" M src/b.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("src/b.rs", false, ChangeKind::Modified));
        let (path, staged, kind) = parse_porcelain_line("A  new.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("new.rs", true, ChangeKind::Added));
        let (path, staged, kind) = parse_porcelain_line("?? untracked.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("untracked.rs", false, ChangeKind::Added));
        let (path, staged, kind) = parse_porcelain_line(" D gone.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("gone.rs", false, ChangeKind::Deleted));
        let (path, staged, kind) = parse_porcelain_line("R  old.rs -> new.rs").unwrap();
        assert_eq!((path.as_str(), staged, kind), ("new.rs", true, ChangeKind::Modified));
        assert!(parse_porcelain_line("").is_none());
        assert!(parse_porcelain_line("!! ignored.rs").is_none());
    }

    #[test]
    fn numstat_counts_binary_files_as_zero() {
        let counts = parse_numstat("10\t2\ta.rs\n-\t-\timg.png\n");
        assert_eq!(counts.get("a.rs"), Some(&(10, 2)));
        assert_eq!(counts.get("img.png"), Some(&(0, 0)));
    }

    #[test]
    fn the_upstream_fallback_is_zero_and_zero() {
        assert_eq!(parse_ahead_behind("3\t7\n"), Some((7, 3)));
        assert_eq!(parse_ahead_behind(""), None);
        assert_eq!(parse_ahead_behind("not a rev-list output"), None);
        // The pane's own fallback when the upstream read fails.
        let (ahead, behind) = parse_ahead_behind("").unwrap_or((0, 0));
        assert_eq!((ahead, behind), (0, 0));
    }

    #[test]
    fn a_non_repo_degrades_to_no_status() {
        let root = temp_root("non-repo");
        assert!(run_git(&root, &["status", "--porcelain"]).is_none());
        assert!(read_git_status(&root).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    // ── the file walk ──

    #[test]
    fn the_walk_lists_one_level_and_skips_vendored_dirs() {
        let root = temp_root("walk");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("inner.txt"), "inner").unwrap();
        for skipped in SKIP_DIRS {
            std::fs::create_dir_all(root.join(skipped)).unwrap();
            std::fs::write(root.join(skipped).join("x"), "x").unwrap();
        }
        let (nodes, truncated) = walk_root_capped(&root, FILE_WALK_CAP);
        assert!(!truncated);
        let names: Vec<String> = nodes.iter().map(|node| node.name.to_string()).collect();
        assert_eq!(names, vec!["a.txt".to_string(), "sub".to_string()]);
        assert!(nodes.iter().all(|node| node.depth == 0));
        assert!(nodes[1].is_dir());
        assert!(!nodes[0].is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A git worktree's `.git` is a FILE, not a directory. The skip must be
    /// by name alone: the directory-shaped test above passes either way, so
    /// only this one fails if the `is_dir &&` guard comes back.
    #[test]
    fn the_walk_skips_a_file_shaped_dot_git() {
        let root = temp_root("walk-worktree");
        std::fs::write(root.join("a.txt"), "a").unwrap();
        std::fs::write(root.join(".git"), "gitdir: /somewhere/else\n").unwrap();
        let (nodes, _) = walk_root_capped(&root, FILE_WALK_CAP);
        let names: Vec<String> = nodes.iter().map(|node| node.name.to_string()).collect();
        assert_eq!(names, vec!["a.txt".to_string()], "a file-shaped .git must be skipped too");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_walk_caps_huge_directories() {
        let root = temp_root("walk-cap");
        for index in 0..5 {
            std::fs::write(root.join(format!("f{index}.txt")), "x").unwrap();
        }
        let (nodes, truncated) = walk_root_capped(&root, 3);
        assert_eq!(nodes.len(), 3);
        assert!(truncated);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// H1: a huge directory is read bounded — never sorted-and-truncated
    /// from an unbounded Vec. 2000 entries collect at most `cap + 1` (the
    /// `+ 1` probe) while the listing still truncates at the cap.
    #[test]
    fn a_huge_directory_is_read_bounded() {
        let root = temp_root("walk-bounded");
        for index in 0..2000 {
            std::fs::write(root.join(format!("f{index:04}.txt")), "x").unwrap();
        }
        let mut cache = RightCache::default();
        reset_io_count();
        let (nodes, truncated) =
            walk_root_expanded_cached(&root, FILE_WALK_CAP, &HashSet::new(), cache.dir_cache_for_root(&root));
        assert!(truncated, "2000 entries over a 300 cap must truncate");
        assert_eq!(nodes.len(), FILE_WALK_CAP);
        assert!(
            walk_collected_count() <= FILE_WALK_CAP + 1,
            "the read must stay bounded, collected {}",
            walk_collected_count()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// H1: two refreshes of a root with an expanded 2000-file directory read
    /// that directory once, then not at all — until a new file moves its
    /// mtime, which re-reads just that directory.
    #[test]
    fn unchanged_expanded_directories_are_not_reread() {
        let root = temp_root("walk-dir-cache");
        std::fs::create_dir_all(root.join("big")).unwrap();
        for index in 0..2000 {
            std::fs::write(root.join("big").join(format!("f{index:04}.txt")), "x").unwrap();
        }
        let mut expanded = HashSet::new();
        expanded.insert("big".to_string());
        let mut cache = RightCache::default();
        reset_io_count();
        let (nodes, truncated) =
            walk_root_expanded_cached(&root, FILE_WALK_CAP, &expanded, cache.dir_cache_for_root(&root));
        assert!(truncated);
        assert_eq!(nodes.len(), FILE_WALK_CAP);
        let first_reads = io_count();
        assert!(first_reads > 0, "the first refresh must read");
        let first_hits = cache_hit_count();
        // Nothing changed: the second refresh replays both directories.
        let (again, _) =
            walk_root_expanded_cached(&root, FILE_WALK_CAP, &expanded, cache.dir_cache_for_root(&root));
        assert_eq!(again.len(), nodes.len());
        assert_eq!(
            io_count(),
            first_reads,
            "an unchanged tree must not re-read any directory (would re-read on the old wholesale walk)"
        );
        assert!(cache_hit_count() > first_hits, "both directories must replay from the cache");
        // A new file moves only `big`'s mtime: only `big` is re-read.
        std::fs::write(root.join("big").join("f2000.txt"), "x").unwrap();
        let (later, truncated) =
            walk_root_expanded_cached(&root, FILE_WALK_CAP, &expanded, cache.dir_cache_for_root(&root));
        assert!(truncated);
        assert_eq!(later.len(), FILE_WALK_CAP);
        assert!(io_count() > first_reads, "a changed directory must be re-read");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// H1: the preview follows the file. A rewrite (here a longer one, so
    /// the size stamp moves even on a coarse-mtime filesystem) reloads on
    /// the next refresh; an unchanged file reloads nothing; a deleted file
    /// lands the existing not-found card instead of stale text.
    #[test]
    fn a_previewed_file_rewritten_on_disk_reloads_on_refresh() {
        let root = temp_root("preview-follows");
        std::fs::write(root.join("doc.txt"), "version one\n").unwrap();
        let mut cache = RightCache::default();
        assert!(begin_file_preview(&mut cache, &root, "doc.txt"));
        let bytes = std::fs::read(root.join("doc.txt")).ok();
        assert!(complete_file_preview(&mut cache, &root, "doc.txt", bytes));
        let preview = cache.preview_for(&root).expect("a preview must be open");
        assert!(
            matches!(&preview.content, PreviewContent::Text { code, .. } if code.contains("version one")),
            "unexpected content: {:?}",
            preview.content
        );
        // Unchanged: no reload.
        assert!(preview_reload_for(&root, &preview).is_none(), "an unchanged file must not reload");
        // Rewritten: the next refresh reloads and lands the new bytes.
        std::fs::write(root.join("doc.txt"), "version two, rewritten at length\n").unwrap();
        let preview = cache.preview_for(&root).expect("the preview stays open");
        let reload = preview_reload_for(&root, &preview).expect("a rewritten file must reload");
        assert_eq!(reload.id, "doc.txt");
        assert!(apply_preview_reload(&mut cache, &root, reload));
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert!(
            matches!(&preview.content, PreviewContent::Text { code, .. } if code.contains("version two")),
            "the preview must show the new bytes, got: {:?}",
            preview.content
        );
        // Settled again: no further reload.
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert!(preview_reload_for(&root, &preview).is_none());
        // Deleted: the existing not-found card, not the stale text.
        std::fs::remove_file(root.join("doc.txt")).unwrap();
        let preview = cache.preview_for(&root).expect("the preview stays open");
        let reload = preview_reload_for(&root, &preview).expect("a deleted file must reload");
        assert!(apply_preview_reload(&mut cache, &root, reload));
        let preview = cache.preview_for(&root).expect("a preview must be open");
        assert!(
            matches!(&preview.content, PreviewContent::Card { meta } if meta.contains("gone")),
            "a deleted file must show the not-found card, got: {:?}",
            preview.content
        );
        // The gone card never loops: nothing more to reload.
        let preview = cache.preview_for(&root).expect("a preview must be open");
        assert!(preview_reload_for(&root, &preview).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_icons_follow_extensions() {
        assert_eq!(file_type_for("a.ts"), FileType::Ts);
        assert_eq!(file_type_for("a.tsx"), FileType::Tsx);
        assert_eq!(file_type_for("a.json"), FileType::Json);
        assert_eq!(file_type_for("a.md"), FileType::Md);
        assert_eq!(file_type_for("Cargo.lock"), FileType::Lock);
        assert_eq!(file_type_for("a.test.ts"), FileType::Test);
        assert_eq!(file_type_for("a.png"), FileType::Image);
        assert_eq!(file_type_for("main.rs"), FileType::File);
    }

    // ── the file preview ──

    #[test]
    fn preview_languages_come_from_the_extension() {
        assert_eq!(preview_language("main.rs"), "rust");
        assert_eq!(preview_language("probe.py"), "python");
        assert_eq!(preview_language("app.tsx"), "tsx");
        assert_eq!(preview_language("data.json"), "json");
        assert_eq!(preview_language("note.md"), "markdown");
        assert_eq!(preview_language("run.sh"), "bash");
        assert_eq!(preview_language("Makefile"), "text");
        assert_eq!(preview_language(".gitignore"), "text");
    }

    #[test]
    fn selecting_a_text_file_holds_its_path_and_language() {
        let root = temp_root("preview-text");
        std::fs::write(root.join("hello.rs"), "fn main() {}\n").unwrap();
        let mut cache = RightCache::default();
        // The click lands the selection and the loading state at once and
        // asks for the background read.
        assert!(begin_file_preview(&mut cache, &root, "hello.rs"));
        let preview = cache.preview_for(&root).expect("a preview must be open");
        assert_eq!(preview.path, "hello.rs");
        assert_eq!(preview.name, "hello.rs");
        assert_eq!(preview.language, "rust");
        assert_eq!(preview.content, PreviewContent::Loading);
        assert_eq!(cache.selected_for(&root).as_deref(), Some("hello.rs"));
        // The background read lands the bytes as text with the language.
        let bytes = std::fs::read(root.join("hello.rs")).ok();
        assert!(complete_file_preview(&mut cache, &root, "hello.rs", bytes));
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert!(
            matches!(&preview.content, PreviewContent::Text { language, code }
                if language == "rust" && code.contains("fn main")),
            "unexpected content: {:?}",
            preview.content
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn selecting_a_markdown_file_holds_markdown() {
        let root = temp_root("preview-md");
        std::fs::write(root.join("note.md"), "# Title\n\nbody\n").unwrap();
        let mut cache = RightCache::default();
        assert!(begin_file_preview(&mut cache, &root, "note.md"));
        let bytes = std::fs::read(root.join("note.md")).ok();
        assert!(complete_file_preview(&mut cache, &root, "note.md", bytes));
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert_eq!(preview.language, "markdown");
        assert!(
            matches!(&preview.content, PreviewContent::Markdown { text } if text.contains("body")),
            "unexpected content: {:?}",
            preview.content
        );
        let blocks = markdown_blocks(match &preview.content {
            PreviewContent::Markdown { text } => text,
            other => panic!("expected markdown, got {other:?}"),
        });
        assert!(matches!(blocks[0], DocBlock::Heading { level: 1, .. }), "first block: {:?}", blocks[0]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_toggles_open_and_closed() {
        let root = temp_root("preview-toggle");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub").join("inner.txt"), "inner\n").unwrap();
        let mut cache = RightCache::default();
        // Closed: one level only.
        let (nodes, _) = walk_root_expanded(&root, FILE_WALK_CAP, &cache.expanded_for(&root));
        assert!(nodes.iter().all(|node| node.depth == 0));
        assert!(!nodes.iter().any(|node| node.id.as_ref() == "sub/inner.txt"));
        // Open: the child row arrives under its directory.
        assert!(toggle_expanded(&mut cache, &root, "sub"));
        let (nodes, _) = walk_root_expanded(&root, FILE_WALK_CAP, &cache.expanded_for(&root));
        assert!(nodes.iter().any(|node| node.id.as_ref() == "sub/inner.txt" && node.depth == 1));
        assert!(nodes.iter().find(|node| node.id.as_ref() == "sub").is_some_and(|node| node.open == Some(true)));
        // Closed again: back to one level.
        assert!(!toggle_expanded(&mut cache, &root, "sub"));
        let (nodes, _) = walk_root_expanded(&root, FILE_WALK_CAP, &cache.expanded_for(&root));
        assert!(!nodes.iter().any(|node| node.id.as_ref() == "sub/inner.txt"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_large_or_binary_file_gets_the_card() {
        let root = temp_root("preview-card");
        let mut cache = RightCache::default();
        // Oversized: the card opens straight away, no background read.
        let big = root.join("big.bin");
        std::fs::write(&big, vec![b'x'; (PREVIEW_MAX_BYTES + 1) as usize]).unwrap();
        assert!(!begin_file_preview(&mut cache, &root, "big.bin"));
        let preview = cache.preview_for(&root).expect("a preview must be open");
        assert!(
            matches!(&preview.content, PreviewContent::Card { meta } if meta.contains("too large")),
            "unexpected content: {:?}",
            preview.content
        );
        // Binary: small enough to read, but not UTF-8 — the landing cards it.
        std::fs::write(root.join("blob"), [0x89, 0x50, 0x4e, 0x47, 0xff, 0xfe]).unwrap();
        assert!(begin_file_preview(&mut cache, &root, "blob"));
        let bytes = std::fs::read(root.join("blob")).ok();
        assert!(complete_file_preview(&mut cache, &root, "blob", bytes));
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert!(
            matches!(&preview.content, PreviewContent::Card { meta } if meta.contains("binary")),
            "unexpected content: {:?}",
            preview.content
        );
        // A failed read cards it too.
        assert!(begin_file_preview(&mut cache, &root, "blob"));
        assert!(complete_file_preview(&mut cache, &root, "blob", None));
        assert!(matches!(
            cache.preview_for(&root).map(|preview| preview.content),
            Some(PreviewContent::Card { .. })
        ));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn closing_a_preview_keeps_the_selected_marker() {
        let root = temp_root("preview-close");
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        let mut cache = RightCache::default();
        assert!(begin_file_preview(&mut cache, &root, "a.txt"));
        assert!(close_file_preview(&mut cache, &root));
        assert!(cache.preview_for(&root).is_none(), "the preview must be gone");
        assert!(!close_file_preview(&mut cache, &root), "closing twice reports nothing open");
        // The tree still marks the row the preview showed.
        let (nodes, _) = walk_root_capped(&root, FILE_WALK_CAP);
        let nodes = mark_selected(nodes, cache.selected_for(&root).as_deref());
        assert!(nodes.iter().any(|node| node.id.as_ref() == "a.txt" && node.selected));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_preview_landing_changes_nothing() {
        let root = temp_root("preview-stale");
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let mut cache = RightCache::default();
        assert!(begin_file_preview(&mut cache, &root, "a.txt"));
        // The selection moved on before the first read landed.
        assert!(begin_file_preview(&mut cache, &root, "b.txt"));
        assert!(!complete_file_preview(&mut cache, &root, "a.txt", Some(b"a\n".to_vec())));
        let preview = cache.preview_for(&root).expect("the preview stays open");
        assert_eq!(preview.path, "b.txt");
        assert_eq!(preview.content, PreviewContent::Loading);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A bootable [`crate::Args`] pointed at a hermetic state dir, mirroring
    /// the app tests' own helper for the one handler test that needs a real
    /// [`Harness`](crate::app::Harness).
    fn harness_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
        }
    }

    /// Point `BAAZ_STATE_DIR` at a fresh temp dir, restoring whatever was
    /// there before. The caller restores with [`restore_harness_dir`].
    fn hermetic_harness_dir(
        name: &str,
    ) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    /// Undo [`hermetic_harness_dir`].
    #[allow(clippy::needless_pass_by_value)]
    fn restore_harness_dir(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf),
    ) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    // ── the inert handlers, invoked directly ──

    /// A [`ToastSink`] that records titles and bodies instead of toasting.
    /// The inert-action tests share one process-global log
    /// ([`inert_log_store`]), and each clears it and then asserts an exact
    /// length. Run concurrently they see each other's entries: filtered to
    /// just those tests they failed every time, while the full suite passed
    /// on lucky scheduling — which is how they shipped and how they later
    /// failed a lane's gate for a change that had nothing to do with them.
    ///
    /// Taking this lock for the whole of each test is what makes the exact
    /// length assertions mean anything.
    fn inert_guard() -> std::sync::MutexGuard<'static, ()> {
        static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
        SERIAL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn test_sink(toasts: Rc<std::cell::RefCell<Vec<(String, String)>>>) -> ToastSink {
        Rc::new(move |title, body, _| toasts.borrow_mut().push((title, body)))
    }

    /// Select and Toggle no longer toast: they preview and expand through
    /// the exact handler the pane wires up, on a hermetic [`Harness`].
    /// Only Search stays inert.
    #[gpui::test]
    fn files_select_and_toggle_act_instead_of_toasting(cx: &mut TestAppContext) {
        let _serial = inert_guard();
        let state = hermetic_harness_dir("files-actions");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            use gpui::AppContext as _;
            cx.new(|cx| {
                Harness::new(
                    harness_args(&state.2),
                    crate::shot::CaptureToken::default(),
                    window,
                    cx,
                )
            })
        });
        std::fs::write(state.2.join("note.md"), "# Note\n\nhello\n").unwrap();
        std::fs::create_dir_all(state.2.join("sub")).unwrap();
        std::fs::write(state.2.join("sub").join("inner.txt"), "inner\n").unwrap();
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.show_right(crate::layout::RightKind::Files, cx))
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project().map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let handle = files_handler(test_sink(toasts.clone()), baaz.downgrade(), root.clone());
            handle(&FileTreeAction::Select(SharedString::from("note.md")), window, cx);
            handle(&FileTreeAction::Toggle(SharedString::from("sub")), window, cx);
            handle(&FileTreeAction::Search, window, cx);
            handle(&FileTreeAction::Refresh, window, cx);
        });
        vc.run_until_parked();
        // Select opened the markdown preview (bytes land off the click),
        // Toggle expanded the directory — neither toasted.
        let preview = vc.update(|_, cx| baaz.read(cx).right_cache.preview_for(&root));
        let Some(preview) = preview else {
            panic!("selecting note.md must open a preview");
        };
        assert_eq!(preview.path, "note.md");
        assert!(
            matches!(&preview.content, PreviewContent::Markdown { text } if text.contains("hello")),
            "unexpected preview content: {:?}",
            preview.content
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).right_cache.expanded_for(&root).contains("sub")),
            "toggling sub must expand it"
        );
        // The re-read the toggle requested draws the child row.
        let nodes = vc
            .update(|_, cx| baaz.read(cx).right_cache.files_for(&root))
            .expect("the refresh lands the listing")
            .0;
        assert!(
            nodes.iter().any(|node| node.id.as_ref() == "sub/inner.txt" && node.depth == 1),
            "an open directory shows its children: {:?}",
            nodes.iter().map(|node| node.id.to_string()).collect::<Vec<_>>()
        );
        // Only Search reached the toast.
        let log = inert_log();
        assert_eq!(log.len(), 1, "unexpected inert actions: {log:?}");
        assert!(log[0].contains("Files") && log[0].contains("Search"), "unexpected: {}", log[0]);
        let toasts = toasts.borrow();
        assert_eq!(toasts.len(), 1);
        assert!(toasts[0].1.contains("Search"));
        restore_harness_dir(state);
    }

    /// Z7a: the Browser pane's nav actions belong to the webview now, not to
    /// [`inert`] — Back/Forward/Reload/Screenshot/Console/annotate reach the
    /// session's [`WebviewState`](aui_webview::WebviewState) through the
    /// library's `webview_pane`. What this pins is the data contract the pane
    /// renders: navigating the state moves the URL the nav row shows, and the
    /// scripted backend (what tests and captures run on) is never native.
    #[gpui::test]
    fn browser_pane_state_drives_the_nav_row(cx: &mut TestAppContext) {
        let vc = cx.add_empty_window();
        vc.update(|_, cx| {
            let state = cx.new(|cx| {
                aui_webview::WebviewState::new(Box::new(aui_webview::FakeWebBackend::new()), cx)
            });
            assert!(!state.read(cx).is_native(), "the scripted page is gpui, never a native overlay");
            state.update(cx, |state, _| state.navigate("https://example.com"));
            assert_eq!(state.read(cx).url().as_ref(), "https://example.com");
        });
    }

    #[gpui::test]
    fn git_and_pr_actions_all_reach_inert(cx: &mut TestAppContext) {
        let _serial = inert_guard();
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let git = git_handler(test_sink(toasts.clone()));
            git(GitAction::Toggle(SharedString::from("a.rs")), window, cx);
            git(GitAction::Regenerate, window, cx);
            git(GitAction::Amend, window, cx);
            git(GitAction::Commit, window, cx);
            git(GitAction::Push, window, cx);
            let pr = pr_handler(test_sink(toasts.clone()));
            pr(PrAction::PickBase, window, cx);
            pr(PrAction::Cancel, window, cx);
            pr(PrAction::Create, window, cx);
            pr(PrAction::Linear, window, cx);
        });
        let log = inert_log();
        assert_eq!(log.len(), 9);
        assert!(log.iter().any(|line| line.contains("Changes") && line.contains("Commit")));
        assert!(log.iter().any(|line| line.contains("Changes") && line.contains("Push")));
        assert!(log.iter().any(|line| line.contains("Pull request") && line.contains("Create")));
        assert_eq!(toasts.borrow().len(), 9);
    }

    #[gpui::test]
    fn diff_actions_all_reach_inert(cx: &mut TestAppContext) {
        let _serial = inert_guard();
        let vc = cx.add_empty_window();
        clear_inert_log();
        let toasts = Rc::new(std::cell::RefCell::new(Vec::new()));
        vc.update(|window, cx| {
            let handle = diff_handler(test_sink(toasts.clone()));
            for action in [
                DiffReviewAction::Scope(DiffScope::Unstaged),
                DiffReviewAction::View(DiffView::Unified),
                DiffReviewAction::Search,
                DiffReviewAction::CollapseAll,
                DiffReviewAction::SelectFile(SharedString::from("a.rs")),
                DiffReviewAction::OpenInEditor,
                DiffReviewAction::Stage,
                DiffReviewAction::AddNote(3),
                DiffReviewAction::EditNote(0),
                DiffReviewAction::DeleteNote(0),
                DiffReviewAction::SaveNote(0),
                DiffReviewAction::CancelNote(0),
                DiffReviewAction::Clear,
                DiffReviewAction::Send,
            ] {
                handle(action, window, cx);
            }
        });
        let log = inert_log();
        assert_eq!(log.len(), 14);
        for line in &log {
            assert!(line.contains("Diff review"), "unexpected: {line}");
        }
        assert!(log.iter().any(|line| line.contains("Stage")));
        assert!(log.iter().any(|line| line.contains("Send")));
        assert_eq!(toasts.borrow().len(), 14);
    }
}

