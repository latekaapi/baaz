//! The right pane's three bodies: real read-only data, inert labelled actions.
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
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use aui::data::button;
use aui::transcript::code_block;
use aui::workbench::{
    doc_pane, file_card, file_tree, ArtifactKind, DocBlock, DocPage, FileNode,
    FileTreeAction, GitBadge,
};
use aui_icons::{FileType, IconName};
use aui_protocol::{ChangeKind, Diff, DiffKind, DiffLine, FileChange, Hunk};
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, ElementId, Entity, ScrollHandle, SharedString,
    WeakEntity, Window,
};
use gpui_kit::base::{h_flex, v_flex};

use crate::app::Harness;
use crate::browser::StripTab;
use crate::layout::RightKind;

/// How many file-tree rows the Files pane will hold. A directory with 10,000
/// entries must not be read into the pane; the footer names the cap.
pub(crate) const FILE_WALK_CAP: usize = 300;
/// D6: the whole tree's safety cap (each folder shows at most
/// [`FILE_WALK_CAP`] entries and says so; docs/43 §2.4).
pub(crate) const FILE_TREE_TOTAL_CAP: usize = 5000;
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
pub(crate) fn note_io() {
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
/// Record how many entries one directory read holds at once (the most seen
/// since the last reset): the bound the walk keeps however big the folder.
fn note_collected(held: usize) {
    #[cfg(test)]
    WALK_COLLECTED.fetch_max(held, Ordering::Relaxed);
    #[cfg(not(test))]
    let _ = held;
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
    /// 1-based lines to band and scroll to (B9), end exclusive — from a
    /// link's `#L…`/`:line` suffix. Text previews band it through the
    /// code block; everything else ignores it.
    pub highlight: Option<std::ops::Range<u32>>,
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
    /// How many entries it held (counted past the cap, never stored).
    total: usize,
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
            *self = DirListCache {
                root: Some(root.to_path_buf()),
                dirs: HashMap::new(),
            };
        }
    }
}

#[derive(Default)]
pub(crate) struct RightCache {
    /// The last git status read, by root (`Some(None)`: not a repo).
    pub git: Option<Stamped<Option<crate::git_ops::GitTree>>>,
    /// D6: each opened row's diff, read when the row opens and dropped
    /// whenever the tree changes (docs/51 P1 #8: never the whole tree).
    /// `None` while the read is out.
    pub file_diffs: HashMap<String, Option<std::sync::Arc<Diff>>>,
    /// D6: unchanged runs a diff row expanded: (path, hunk index) → lines.
    pub gap_lines: HashMap<(String, usize), std::sync::Arc<Vec<String>>>,
    /// D6: the Changes screen's commit box, PR and form state.
    pub changes: crate::right_changes::ChangesUi,
    /// D6: the Page screen's "Search or enter an address" field.
    pub page_address: Option<gpui::Entity<gpui_kit::base::input::InputState>>,
    /// The Page row's Recent pages menu is open (owner 2026-10-18).
    pub page_menu: bool,
    /// D6: Files' filter and keyboard state.
    pub files_ui: crate::right_files::FilesUi,
    /// D6: an image file previewed at 100 % instead of fitted.
    pub files_image_actual: bool,
    /// D6: a view pushed on a screen (by strip index): full output, image.
    pub pushed: HashMap<usize, crate::right_views::Pushed>,
    /// When each pushed view went on (its 160 ms slide-in).
    pub pushed_at: HashMap<usize, Instant>,
    /// Per screen: whether the last frame showed a pushed view, and when the
    /// screen last came back from one (the 120 ms reverse, docs/43 §3).
    pub popped: std::cell::RefCell<HashMap<usize, (bool, Option<Instant>)>>,
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
    /// The last "reveal in Files" request per root (B9): the row to scroll
    /// into view plus its request token. The tree scrolls to `(id, token)`
    /// once per pair, so repeating the same reveal with a new token
    /// scrolls again after the reader scrolled away. Never persists.
    pub reveal: HashMap<PathBuf, (String, u64)>,
    /// The next reveal token [`Self::reveal`] hands out. Never persists.
    reveal_seq: u64,
    /// B8b: Changes-pane files whose inline diff stands expanded, per
    /// project root. Transient view state, never persisted: a re-read keeps
    /// the expansion, a restart does not.
    pub changes_expanded: HashMap<PathBuf, HashSet<String>>,
}

impl RightCache {
    /// The cached git status for `root`: `None` when never fetched for it,
    /// `Some(None)` when fetched and not a repo.
    pub(crate) fn git_for(&self, root: &Path) -> Option<Option<crate::git_ops::GitTree>> {
        self.git
            .as_ref()
            .filter(|slot| slot.root == root)
            .map(|slot| slot.value.clone())
    }


    /// The cached file listing and cap flag for `root`, or `None` when never
    /// fetched for it.
    pub(crate) fn files_for(&self, root: &Path) -> Option<(Vec<FileNode>, bool)> {
        self.files
            .as_ref()
            .filter(|slot| slot.root == root)
            .map(|slot| slot.value.clone())
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
        self.files_scroll
            .borrow_mut()
            .entry(root.to_path_buf())
            .or_default()
            .clone()
    }

    /// Note a "reveal in Files" request for `rel` under `root` (B9): the
    /// tree scrolls to the row on its next frames, with a fresh token so a
    /// repeated reveal re-scrolls even to the settled row.
    pub(crate) fn note_reveal(&mut self, root: &Path, rel: &str) {
        self.reveal_seq = self.reveal_seq.wrapping_add(1);
        self.reveal
            .insert(root.to_path_buf(), (rel.to_string(), self.reveal_seq));
    }

    /// B8b: the Changes-pane files standing expanded for `root`: empty when
    /// nothing was ever toggled there.
    pub(crate) fn changes_expanded_for(&self, root: &Path) -> HashSet<String> {
        self.changes_expanded.get(root).cloned().unwrap_or_default()
    }

    /// B8b: flip one file's inline-diff expansion, returning whether it
    /// stands open now.
    pub(crate) fn flip_changes_file(&mut self, root: &Path, path: &str) -> bool {
        let open = self.changes_expanded.entry(root.to_path_buf()).or_default();
        if open.remove(path) {
            false
        } else {
            open.insert(path.to_string());
            true
        }
    }

    /// When the slot backing `kind` last landed for `root`, if it ever did.
    /// The refresh path paces the steady-state interval off this.
    pub(crate) fn fetched_at(&self, kind: RightKind, root: &Path) -> Option<Instant> {
        match kind {
            RightKind::Browser => None,
            RightKind::Changes | RightKind::Git | RightKind::Diff => self
                .git
                .as_ref()
                .filter(|slot| slot.root == root)
                .map(|slot| slot.at),
            RightKind::Files => self
                .files
                .as_ref()
                .filter(|slot| slot.root == root)
                .map(|slot| slot.at),
        }
    }

    /// Land one [`read_snapshot`]: every slot takes the snapshot's root and
    /// the landing instant. A snapshot for another root drops the directory
    /// listing cache: its stamped entries belong to the old tree.
    pub(crate) fn apply_snapshot(&mut self, snapshot: RightSnapshot, at: Instant) {
        if self
            .files
            .as_ref()
            .is_some_and(|slot| slot.root != snapshot.root)
        {
            self.dir_lists = DirListCache::default();
        }
        let root = snapshot.root.clone();
        // A changed tree (or another root) makes every read diff stale: the
        // open rows read theirs again.
        let same = self.git.as_ref().is_some_and(|slot| slot.root == root && slot.value == snapshot.git);
        if !same {
            self.file_diffs.clear();
            self.gap_lines.clear();
        }
        self.git = Some(Stamped {
            root: root.clone(),
            at,
            value: snapshot.git,
        });
        self.files = Some(Stamped {
            root,
            at,
            value: (snapshot.files, snapshot.files_truncated),
        });
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
    /// The working tree, or `None` when the root is not a git checkout.
    pub git: Option<crate::git_ops::GitTree>,
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
        let (files, files_truncated) =
            walk_root_expanded_cached(root, FILE_WALK_CAP, expanded, dir_cache);
        return RightSnapshot {
            root: root.to_path_buf(),
            git: Some(fixture_git_status()),
            files,
            files_truncated,
        };
    }
    let git = crate::git_ops::read_tree(root);
    let (files, files_truncated) =
        walk_root_expanded_cached(root, FILE_WALK_CAP, expanded, dir_cache);
    RightSnapshot {
        root: root.to_path_buf(),
        git,
        files,
        files_truncated,
    }
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
/// What the Browser arm draws when there is no live webview (B7): the
/// loading placeholder only while the pane stands open; the closed marker
/// once it stands closed — never the loading placeholder on close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BrowserPlaceholder {
    /// The live page.
    Page,
    /// The pane is open and the webview is still attaching.
    Loading,
    /// The pane is closed: no page, no placeholder flash.
    Closed,
}

/// Pick the [`BrowserPlaceholder`] for `pane_open` and whether the frame
/// has a live webview. Pure so tests pin the close rule directly.
pub(crate) fn browser_placeholder_for(pane_open: bool, has_webview: bool) -> BrowserPlaceholder {
    if has_webview {
        BrowserPlaceholder::Page
    } else if pane_open {
        BrowserPlaceholder::Loading
    } else {
        BrowserPlaceholder::Closed
    }
}

/// What [`render`] needs for the Browser kind: the live page when one
/// exists, the shown owner's strip rows, and the window tab hover runs on.
pub(crate) struct BrowserPane<'a> {
    /// The shown tab's live page; `None` draws the loading/closed states.
    pub state: Option<&'a Entity<aui_webview::WebviewState>>,
    /// The shown owner's tabs; empty draws no strip.
    pub tabs: &'a [StripTab],
    /// The window, for tab hover tracking.
    pub window: &'a mut Window,
    /// Whether the strip offers the "Browse" button (B2a-H4): true when
    /// the shown owner is a session, whose active tab carries into Browse
    /// full-width. Person tabs already live in Browse, so they get none.
    pub carry: bool,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render(
    kind: RightKind,
    cache: &RightCache,
    project: Option<(PathBuf, String)>,
    session_edits: &[String],
    browser: BrowserPane<'_>,
    pane_open: bool,
    width: f32,
    agent: &str,
    cx: &mut Context<Harness>,
) -> AnyElement {
    let harness = cx.weak_entity();
    match kind {
        RightKind::Browser => match browser_placeholder_for(pane_open, browser.state.is_some()) {
            BrowserPlaceholder::Page => browser_pane(
                browser.state.expect("has_webview means Some"),
                browser.tabs,
                browser.carry,
                browser.window,
                cx,
            ),
            BrowserPlaceholder::Loading => loading_state(
                "right-browser-loading",
                "Browser",
                "Opening the page",
                "The webview is being attached.",
            ),
            BrowserPlaceholder::Closed => closed_browser_state(),
        },
        RightKind::Changes | RightKind::Diff | RightKind::Git => match project {
            Some((root, _)) => crate::right_changes::changes_screen(cache, &root, cache.git_for(&root), session_edits, width, agent, browser.window, cx).1,
            None => empty_state("right-changes-empty", "Changes", "No project is open", "Open or adopt a project and its uncommitted changes will be listed here."),
        },
        RightKind::Files => {
            match project {
                Some((root, name)) => match cache.files_for(&root) {
                    None => loading_state(
                        "right-files-loading",
                        "Files",
                        "Loading files",
                        "The pane re-reads the project in the background and fills in on its own.",
                    ),
                    Some((nodes, truncated)) => {
                        let selected = cache.selected_for(&root);
                        let nodes = decorate_nodes(mark_selected(nodes, selected.as_deref()), cache.git_for(&root).flatten().as_ref());
                        files_pane_from(&nodes, truncated, &name, &root, cache, harness)
                    }
                },
                None => empty_state(
                    "right-files-empty",
                    "Files",
                    "No project is open",
                    "Open or adopt a project to browse its files here.",
                ),
            }
        }
    }
}

/// The screen's place on the strip (D6): Changes 0, Files 1, Page 2.
pub(crate) fn screen_index(kind: RightKind) -> usize {
    match kind {
        RightKind::Changes | RightKind::Diff | RightKind::Git => 0,
        RightKind::Files => 1,
        RightKind::Browser => 2,
    }
}

/// The glyph a screen wears in its row and toggle (docs/43 §2.2).
pub(crate) fn screen_glyph(kind: RightKind) -> IconName {
    match kind {
        RightKind::Changes | RightKind::Diff | RightKind::Git => IconName::Git,
        RightKind::Files => IconName::Folder,
        RightKind::Browser => IconName::Globe,
    }
}

/// One screen on the strip (D6, docs/43 §2.2): its 32 px row — glyph,
/// name, its own actions, ✕ — over its body, with the 24 px fade under the
/// row. The pane is `complementary` "Right pane"; each screen a group named
/// for itself.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_screen(
    kind: RightKind,
    cache: &RightCache,
    project: Option<(PathBuf, String)>,
    session_edits: &[String],
    browser: BrowserPane<'_>,
    pane_open: bool,
    width: f32,
    agent: &str,
    files_focus: &gpui::FocusHandle,
    cx: &mut Context<Harness>,
) -> AnyElement {
    let slug = kind.slug();
    let screen = screen_index(kind);
    let close = cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| this.toggle_right(cx));
    let pane_row = || aui::workbench::pane_row(SharedString::from(format!("right-row-{slug}")), screen_glyph(kind), kind.label());
    if cache.pushed.contains_key(&screen) && cache.pushed_at.get(&screen).is_some_and(|at| at.elapsed().as_secs_f32() < 0.16) {
        browser.window.request_animation_frame();
    }
    // Back is the reverse, 120 ms: the screen under it comes in 8 px from the
    // left as it fades in. Any pop path counts — the transition is read
    // here, from what this screen showed last frame.
    let pushed_now = cache.pushed.contains_key(&screen)
        || (matches!(kind, RightKind::Files) && project.as_ref().is_some_and(|(root, _)| cache.preview_for(root).is_some()))
        || (matches!(kind, RightKind::Changes | RightKind::Diff | RightKind::Git)
            && cache.changes.form.is_some());
    let back_since = {
        let mut popped = cache.popped.borrow_mut();
        let entry = popped.entry(screen).or_insert((pushed_now, None));
        if entry.0 && !pushed_now {
            entry.1 = Some(Instant::now());
        }
        entry.0 = pushed_now;
        entry.1.map(|at| at.elapsed().as_secs_f32() / 0.12).filter(|t| *t < 1.0 && !cx.reduce_motion())
    };
    if back_since.is_some() {
        browser.window.request_animation_frame();
    }
    let (row, body, pushed): (AnyElement, AnyElement, bool) = if let Some(view) = cache.pushed.get(&screen) {
        let (row, body) = crate::right_views::pushed_screen(screen, view, width, browser.window, cx);
        (row, body, true)
    } else {
        match (kind, project) {
            (RightKind::Changes | RightKind::Diff | RightKind::Git, Some((root, _))) => {
                let (row, body) = crate::right_changes::changes_screen(cache, &root, cache.git_for(&root), session_edits, width, agent, browser.window, cx);
                let pushed = row.is_some();
                (row.unwrap_or_else(|| pane_row().on_close(close).into_any_element()), body, pushed)
            }
            (RightKind::Browser, _) => {
                let row = crate::right_page::page_row(pane_row(), cache.page_menu, browser.state, cx).on_close(close);
                // Over a native page the menu sits in its own window: a press
                // anywhere else in this one closes it (live 2026-10-19).
                let overlay_menu = cache.page_menu
                    && browser.state.is_some_and(|st| st.read(cx).is_native() && !crate::right_page::page_covered(st.read(cx), cache));
                // (The in-window menu closes itself on an outside press; this
                // one would also fire on a press inside it, before its click.)
                let row = if overlay_menu {
                    div()
                        .on_mouse_down_out(cx.listener(|this: &mut Harness, _: &gpui::MouseDownEvent, _, cx| this.set_page_menu(false, cx)))
                        .child(row)
                        .into_any_element()
                } else {
                    row.into_any_element()
                };
                let menu = crate::right_page::page_menu_in_window(cache.page_menu, browser.state, cache, width, cx);
                let body = crate::right_page::page_body(browser, pane_open, agent, cache.page_address.clone(), cx);
                let body = match menu {
                    Some(menu) => div().relative().size_full().child(body).child(menu).into_any_element(),
                    None => body,
                };
                (row, body, false)
            }
            (RightKind::Files, project) => {
                let (row, body) = crate::right_files::files_screen(cache, project.clone(), width, files_focus, browser.window, cx);
                let pushed = project.as_ref().is_some_and(|(root, _)| cache.preview_for(root).is_some());
                (row, body, pushed)
            }
            (RightKind::Changes | RightKind::Diff | RightKind::Git, None) => (
                pane_row().on_close(close).into_any_element(),
                aui::workbench::state_block("right-changes-noproject", IconName::Git, "No project").line("Open or add a project to see its changes.").into_any_element(),
                false,
            ),
        }
    };
    let chrome_under_row = opens_on_chrome(kind, pushed, cache.files_ui.filter.is_some());
    // A pushed view slides 8 px in from the right and fades in, 160 ms
    // ease-out (docs/43 §3).
    let body = if pushed {
        let since = cache.pushed_at.get(&screen).map(|at| at.elapsed().as_secs_f32() / 0.16).unwrap_or(1.0).min(1.0);
        let eased = 1.0 - (1.0 - since).powi(3);
        div().size_full().relative().left(px(8.0 * (1.0 - eased))).opacity(eased).child(body).into_any_element()
    } else if let Some(t) = back_since {
        let eased = 1.0 - (1.0 - t).powi(3);
        div().size_full().relative().left(px(-8.0 * (1.0 - eased))).opacity(eased).child(body).into_any_element()
    } else {
        body
    };
    v_flex()
        .id(SharedString::from(format!("right-screen-{slug}")))
        .relative()
        .size_full()
        .role(gpui::Role::Complementary)
        .aria_label(format!("Right pane: {}", kind.label()))
        .key_context("RightPane")
        .on_action(cx.listener(move |this: &mut Harness, _: &crate::app::RightBack, _, cx| {
            this.pop_right_view(screen, cx);
        }))
        .child(row)
        .child(div().flex_1().min_h(px(0.0)).child(body))
        // The fade is for content scrolling under the row; a screen whose
        // body opens on its own opaque chrome (the Page's nav row, the
        // Files filter) has none there. Drawn over that chrome, it veiled
        // the address field (owner 2026-10-21: "the address bar is getting
        // hidden under the Page header's shadow"; the locked boards draw
        // the nav row above the fade).
        .when(!chrome_under_row, |screen| screen.child(aui::workbench::pane_row_fade(cx)))
        .into_any_element()
}

/// Whether a screen's body opens on opaque chrome of its own right under
/// the pane row — the Page's nav row, the open Files filter — which the
/// row's fade must not veil. A pushed view opens on content.
fn opens_on_chrome(kind: RightKind, pushed: bool, filter_open: bool) -> bool {
    !pushed
        && match kind {
            RightKind::Browser => true,
            RightKind::Files => filter_open,
            _ => false,
        }
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
    /// selection and the loading state land at once; the file's metadata
    /// and bytes follow on one background task (gone, folder, too large or
    /// the text). Never touches the filesystem on the UI thread — a session
    /// open that restores a preview used to stat here.
    pub(crate) fn begin_file_preview_for(&mut self, root: &Path, id: &str, cx: &mut Context<Self>) {
        self.begin_file_preview_for_line(root, id, None, cx);
    }

    /// [`Self::begin_file_preview_for`] with a line band (B9): a link's
    /// `#L…`/`:line` suffix opens banded and scrolled there.
    pub(crate) fn begin_file_preview_for_line(
        &mut self,
        root: &Path,
        id: &str,
        highlight: Option<std::ops::Range<u32>>,
        cx: &mut Context<Self>,
    ) {
        mark_file_preview_loading(&mut self.right_cache, root, id, highlight);
        self.save_right_for_active(cx);
        cx.notify();
        let root = root.to_path_buf();
        let id = id.to_string();
        cx.spawn(async move |this, cx| {
            let path = root.join(&id);
            let probe = cx.background_executor().spawn(async move { probe_file_preview(&path) }).await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                if land_file_preview(&mut this.right_cache, &root, &id, probe) {
                    this.save_right_for_active(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Reveal an absolute link path in the Files pane (B9): the landing
    /// behind [`SessionEvent::RevealPath`](crate::session::SessionEvent),
    /// after the lifecycle's background task settled existence. `is_dir`
    /// is `None` when nothing exists there (a quiet toast, like the old
    /// link path); a folder selects and scrolls with no preview, a file
    /// previews banded on `highlight`. A path outside the project falls
    /// back to the system opener with a toast — the tree cannot show it.
    /// Never touches the filesystem itself.
    pub(crate) fn reveal_link_path(
        &mut self,
        root: &Path,
        abs: &Path,
        is_dir: Option<bool>,
        highlight: Option<std::ops::Range<u32>>,
        cx: &mut Context<Self>,
    ) {
        let name = abs
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(is_dir) = is_dir else {
            self.overlays.update(cx, |overlays, _| {
                overlays.toast("Link", format!("No such file: {name}"));
            });
            cx.notify();
            return;
        };
        let Ok(rel) = abs.strip_prefix(root) else {
            cx.open_with_system(abs);
            self.overlays.update(cx, |overlays, _| {
                overlays.toast("Link", "Outside the project — opened in its default app.");
            });
            cx.notify();
            return;
        };
        let rel = rel.to_string_lossy().into_owned();
        if rel.is_empty() {
            self.select_right(RightKind::Files, cx);
            return;
        }
        if is_dir {
            select_dir(&mut self.right_cache, root, &rel);
            self.right_cache.note_reveal(root, &rel);
            self.select_right(RightKind::Files, cx);
            return;
        }
        if let Some(parent) = rel.rsplit_once('/').map(|(parent, _)| parent) {
            expand_to(&mut self.right_cache, root, parent);
        }
        self.right_cache.note_reveal(root, &rel);
        self.select_right(RightKind::Files, cx);
        self.begin_file_preview_for_line(root, &rel, highlight, cx);
    }

    /// Close the open preview for `root`, returning to the tree with the
    /// selected marker kept. What the preview's back control and Escape
    /// call. Returns whether one was open.
    pub(crate) fn close_file_preview_for(&mut self, root: &Path, cx: &mut Context<Self>) -> bool {
        // The keyboard goes on from the file that was open (live 2026-10-19:
        // ↓ after Back started at the top).
        if let Some(path) = self.right_cache.preview_for(root).map(|p| p.path.clone()) {
            self.right_cache.files_ui.cursor = Some(path);
        }
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
    /// through the non-toggling [`Harness::select_right`] (`show_right`
    /// would close a pane already showing Browser) and without arming URL
    /// focus (the person's keyboard stays where it was). Works for a
    /// session that is not active: the saved state is written onto
    /// `session_id` directly.
    pub(crate) fn show_browser_for_agent(&mut self, session_id: &str, cx: &mut Context<Self>) {
        // Only the session on screen changes what is on screen (review): an
        // agent in a background session must not flip the person's visible
        // pane — its session's saved state below is all that changes, and
        // it shows when the person switches there.
        let active = self
            .active
            .as_ref()
            .map(|view| view.read(cx).session_id.clone());
        let foreground = agent_open_flips_visible_pane(active.as_deref(), session_id);
        if foreground {
            self.select_right(RightKind::Browser, cx);
            // A person's own open earns URL-field focus; an agent's never
            // does — undo the arm `select_right` owes the person.
            self.browser_url_focus_armed = false;
        }
        let mut right = self
            .overrides
            .get(session_id)
            .and_then(|meta| meta.right.clone())
            .unwrap_or_default();
        right.open = true;
        right.kind = RightKind::Browser;
        // B7: agent navigation state is not searchable either — the cheap
        // debounced write, never the session-list settle.
        self.store_right_state_cheap(session_id, right, cx);
        // B7fix2: the foreground open builds the webview on a deferred task
        // with the window — never as a render side effect.
        self.defer_browser_prewarm(cx);
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
pub(crate) fn select_file_preview(
    harness: WeakEntity<Harness>,
    root: PathBuf,
    id: String,
    cx: &mut App,
) {
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
fn loading_state(
    id: &'static str,
    pane: &'static str,
    heading: &'static str,
    detail: &'static str,
) -> AnyElement {
    empty_state(id, pane, heading, detail)
}

/// The closed Browser pane (B7): the last page snapshot stays in the
/// native view while the column slides out, so gpui draws this still
/// marker underneath — never the "Opening the page" loading placeholder.
/// Carries the same role and label as every pane state.
fn closed_browser_state() -> AnyElement {
    empty_state(
        "right-browser-closed",
        "Browser",
        "Browser",
        "The browser pane is closed.",
    )
}

/// A labelled placeholder: never a blank pane, always a role and a label.
fn empty_state(
    id: &'static str,
    pane: &'static str,
    heading: &'static str,
    detail: &'static str,
) -> AnyElement {
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

/// B8c: whether a working-tree path is one of the session's edited paths: an
/// exact match, or the edit target made relative to `root` matching exactly
/// (fold targets may be absolute while `git status` paths are root-relative).
/// Whole paths only: a root-level `a.rs` never claims an edit to
/// `/work/src/a.rs` — the old `ends_with("/<status path>")` did.
fn edit_covers(status_path: &str, edit: &str, root: &Path) -> bool {
    if status_path == edit {
        return true;
    }
    let relative = Path::new(edit)
        .strip_prefix(root)
        .ok()
        .map(|relative| relative.to_string_lossy().into_owned());
    relative
        .as_deref()
        .is_some_and(|relative| !relative.is_empty() && status_path == relative)
}

/// B8b: working-tree files, one list per Changes section.
pub(crate) type ChangeFiles = Vec<FileChange>;

/// B8b: split the working tree into this session's files first, everything
/// else below. Session files follow the session's edit order; the rest keep
/// the status order. A status file matches at most one edit, so a repeated
/// edit target cannot duplicate a row. Matching is exact (see
/// [`edit_covers`]): absolute edit targets are relativized against `root`,
/// the project root the status paths are relative to.
pub(crate) fn partition_session_files(
    files: &[FileChange],
    session_edits: &[String],
    root: &Path,
) -> (ChangeFiles, ChangeFiles) {
    let mut taken = vec![false; files.len()];
    let mut session = Vec::new();
    for edit in session_edits {
        let edit = edit.trim();
        if edit.is_empty() {
            continue;
        }
        let mut found = None;
        for (index, change) in files.iter().enumerate() {
            if !taken[index] && edit_covers(&change.path, edit, root) {
                found = Some(index);
                break;
            }
        }
        if let Some(index) = found {
            taken[index] = true;
            session.push(files[index].clone());
        }
    }
    let mut other = Vec::new();
    for (index, file) in files.iter().enumerate() {
        if !taken[index] {
            other.push(file.clone());
        }
    }
    (session, other)
}

/// One handler per component, each a thin call to [`inert`] — except the
/// Files pane's, which wires `Refresh`, `Toggle` and `Select` to the real
/// views and leaves only `Search` inert. Factored as named functions (rather
/// than inline closures) so tests can invoke the exact handler the pane
/// wires up.
fn files_handler(
    harness: WeakEntity<Harness>,
    root: PathBuf,
) -> impl Fn(&FileTreeAction, &mut Window, &mut App) + 'static {
    move |action, _window, cx| match action {
        FileTreeAction::Toggle(id) => {
            let id = id.to_string();
            let root = root.clone();
            let _ = harness.update(cx, |this, cx| this.toggle_files_dir(&root, &id, cx));
        }
        FileTreeAction::Select(id) => {
            select_file_preview(harness.clone(), root.clone(), id.to_string(), cx);
        }
    }
}

// ── read-only git ────────────────────────────────────────────────────────


/// A fixed working tree for deterministic captures.
///
/// The git and diff panes read the real repository, which is exactly what
/// makes them useful and exactly what makes them impossible to baseline: the
/// screenshot changes with every commit, so the entries would report findings
/// for ever and teach everyone to ignore them. Under `BAAZ_DETERMINISTIC`
/// they read this instead, so the baseline pins how the pane RENDERS rather
/// than what the repository happens to contain today.
fn fixture_git_status() -> crate::git_ops::GitTree {
    let file = |path: &str, change: ChangeKind, added: u32, removed: u32| FileChange { path: path.into(), change, added, removed };
    crate::git_ops::GitTree {
        files: vec![
            file("crates/baaz/src/layout.rs", ChangeKind::Modified, 31, 4),
            file("crates/baaz/src/panes/mod.rs", ChangeKind::Added, 64, 0),
            file("crates/baaz/src/right.rs", ChangeKind::Modified, 148, 12),
            file("docs/02-app.md", ChangeKind::Modified, 22, 9),
            file("scripts/old_probe.py", ChangeKind::Deleted, 0, 37),
        ],
        branch: "right-pane".into(),
        upstream: Some("origin/right-pane".into()),
        ahead: 2,
        behind: 1,
        ignored: vec![".cargo/".into(), ".claude/".into(), ".dev.env".into(), ".relay/".into(), "target/".into()],
        untracked: Vec::new(),
    }
}

/// The diff the fixture's `right.rs` shows (canvas v49 `D6Changes`): two
/// hunks with an unchanged run between them. Small on purpose: a capture
/// wants a legible diff, not a realistic one.
pub(crate) fn fixture_diff_for(path: &str) -> Diff {
    let line = |kind, old_no, new_no, text: &str| DiffLine { kind, old_no, new_no, text: text.to_string() };
    use DiffKind::{Add, Context, Del};
    Diff {
        path: path.into(),
        hunks: vec![
            Hunk {
                header: "@@ -2820,10 +2820,12 @@ fn changes_heading".into(),
                lines: vec![
                    line(Context, Some(2820), Some(2820), "fn changes_heading(id: &'static str, label: SharedString) -> AnyElement {"),
                    line(Context, Some(2821), Some(2821), "    div()"),
                    line(Context, Some(2822), Some(2822), "        .id(id)"),
                    line(Del, Some(2823), None, "        .px(px(16.0))"),
                    line(Del, Some(2824), None, "        .pt(px(12.0))"),
                    line(Add, None, Some(2823), "        .h(px(32.0))"),
                    line(Add, None, Some(2824), "        .px(px(8.0))"),
                    line(Add, None, Some(2825), "        .text_color(cx.theme().muted)"),
                    line(Add, None, Some(2826), "        .text_size(rems(TEXT_CAPTION))"),
                    line(Context, Some(2825), Some(2827), "        .child(label)"),
                ],
            },
            Hunk {
                header: "@@ -2853,7 +2852,9 @@ fn session_file_row".into(),
                lines: vec![
                    line(Context, Some(2853), Some(2852), "fn session_file_row("),
                    line(Context, Some(2854), Some(2853), "    change: &FileChange,"),
                    line(Del, Some(2855), None, "    open: bool,"),
                    line(Add, None, Some(2854), "    open: bool, hovered: bool,"),
                    line(Add, None, Some(2855), "    theme: &Theme,"),
                ],
            },
        ],
        added: 148,
        removed: 12,
    }
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
        if lower.ends_with(".lock") {
            FileType::Lock
        } else {
            FileType::Json
        }
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
fn walk_root_expanded(
    root: &Path,
    cap: usize,
    expanded: &HashSet<String>,
) -> (Vec<FileNode>, bool) {
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
    walk_level_cached(
        root,
        root,
        0,
        cap,
        expanded,
        dir_cache,
        &mut nodes,
        &mut truncated,
    );
    (nodes, truncated)
}

/// One level of `dir` appended to `nodes`: sorted by name, skipping
/// [`SKIP_DIRS`] by name at every level — whatever the entry is on disk,
/// so a worktree's file-shaped `.git` never lists. Stops appending once
/// `nodes` reaches `cap` and reports it in `truncated`. A directory over
/// the cap contributes its first `cap` entries in the sorted order of the
/// bounded read, and the pane's truncation note still applies.
#[allow(clippy::too_many_arguments)] // one recursion's state, threaded by hand
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
    if nodes.len() >= FILE_TREE_TOTAL_CAP {
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
            let total = hit.total;
            append_cached_entries(
                root,
                dir,
                depth,
                cap,
                expanded,
                dir_cache,
                &entries,
                (dir_truncated, total),
                nodes,
                truncated,
            );
            return;
        }
    }
    note_io();
    // Bounded: a max-heap holds at most `cap + 1` entries, so a huge folder
    // never builds an unbounded Vec, and what stays is the `cap` smallest
    // names — the folder's real first `cap`, not the first the disk handed
    // back (live 2026-10-19: f117 was followed by f120).
    let mut heap: std::collections::BinaryHeap<(String, bool)> = std::collections::BinaryHeap::new();
    let mut total = 0usize;
    match std::fs::read_dir(dir) {
        Ok(read) => {
            for entry in read.filter_map(Result::ok) {
                let name = entry.file_name().to_string_lossy().into_owned();
                if SKIP_DIRS.contains(&name.as_str()) {
                    continue;
                }
                total += 1;
                if heap.len() >= cap && heap.peek().is_some_and(|top| top.0 <= name) {
                    // Past the cap and after every kept name: counted only.
                    continue;
                }
                let is_dir = entry.file_type().as_ref().is_ok_and(|kind| kind.is_dir());
                heap.push((name, is_dir));
                if heap.len() > cap {
                    heap.pop();
                }
                note_collected(heap.len() + 1);
            }
        }
        Err(_) => return,
    }
    let entries: Vec<(String, bool)> = heap.into_sorted_vec();
    let dir_truncated = total > entries.len();
    dir_cache.dirs.insert(
        dir.to_path_buf(),
        CachedDir {
            mtime,
            cap,
            entries: entries.clone(),
            dir_truncated,
            total,
        },
    );
    append_cached_entries(
        root,
        dir,
        depth,
        cap,
        expanded,
        dir_cache,
        &entries,
        (dir_truncated, total),
        nodes,
        truncated,
    );
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
    (dir_truncated, dir_total): (bool, usize),
    nodes: &mut Vec<FileNode>,
    truncated: &mut bool,
) {
    if dir_truncated {
        *truncated = true;
    }
    for (name, is_dir) in entries {
        if nodes.len() >= FILE_TREE_TOTAL_CAP {
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
                // `cap` is per folder: a capped child ends with its own line
                // and the walk goes on to its siblings (live 2026-10-19, the
                // folders after a 350-file one vanished). The tree's total
                // is bounded by FILE_TREE_TOTAL_CAP at the loop's top.
                walk_level_cached(
                    root,
                    &dir.join(name),
                    depth + 1,
                    cap,
                    expanded,
                    dir_cache,
                    nodes,
                    truncated,
                );
            }
        } else {
            let kind = file_type_for(name);
            nodes.push(FileNode::file(id, name.clone(), kind).depth(depth));
        }
    }
    // A capped folder ends with one faint line (docs/43 §2.4).
    if dir_truncated && nodes.len() < FILE_TREE_TOTAL_CAP {
        let dir_id = dir.strip_prefix(root).ok().map(|r| r.to_string_lossy().into_owned()).unwrap_or_default();
        nodes.push(FileNode::note(
            format!("{dir_id}/:capped"),
            format!("Showing the first {cap} of {} in this folder. Filter to find the rest.", aui::workbench::group_thousands(dir_total)),
            depth,
        ));
    }
}

/// D6 (docs/43 §2.4): the tree's git letters and faint ignored paths, from
/// the working tree. A folder carries the letter of what changed under it
/// (one kind → its letter, a mix → M).
pub(crate) fn decorate_nodes(nodes: Vec<FileNode>, tree: Option<&crate::git_ops::GitTree>) -> Vec<FileNode> {
    let Some(tree) = tree else { return nodes };
    let letter = |f: &FileChange| match f.change {
        ChangeKind::Added if tree.is_untracked(&f.path) => GitBadge::U,
        ChangeKind::Added => GitBadge::A,
        ChangeKind::Deleted => GitBadge::D,
        _ => GitBadge::M,
    };
    nodes
        .into_iter()
        .map(|node| {
            let id = node.id.to_string();
            let ignored = tree.is_ignored(&id);
            let badge = if node.is_dir() {
                let prefix = format!("{id}/");
                let mut kinds = tree.files.iter().filter(|f| f.path.starts_with(&prefix)).map(letter);
                kinds.next().map(|first| if kinds.all(|k| k == first) { first } else { GitBadge::M })
            } else {
                tree.change(&id).map(letter)
            };
            let node = node.ignored(ignored);
            match badge {
                Some(b) => node.badge(b),
                None => node,
            }
        })
        .collect()
}

/// Mark the node carrying `selected` (a root-relative id) selected: what
/// the tree draws after the preview closes.
pub(crate) fn mark_selected_pub(nodes: Vec<FileNode>, selected: Option<&str>) -> Vec<FileNode> {
    mark_selected(nodes, selected)
}

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
    let extension = if lower.len() > extension.len() + 1 {
        extension
    } else {
        ""
    };
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
/// Whether a path names an image the image view shows.
pub(crate) fn is_image_path(path: &str) -> bool {
    is_image(path)
}

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

/// Stand every ancestor of the root-relative `rel` — and `rel` itself —
/// open for `root` (B9): what a folder-link reveal needs, where
/// [`toggle_expanded`] only flips one directory. Inserting (never
/// removing) keeps whatever the person had open.
pub(crate) fn expand_to(cache: &mut RightCache, root: &Path, rel: &str) {
    let open = cache.expanded.entry(root.to_path_buf()).or_default();
    let mut prefix = String::new();
    for part in rel.split('/').filter(|part| !part.is_empty()) {
        if prefix.is_empty() {
            prefix = part.to_string();
        } else {
            prefix.push('/');
            prefix.push_str(part);
        }
        open.insert(prefix.clone());
    }
}

/// Select the directory `rel` for `root` (B9): its ancestors stand open,
/// the tree's selected marker moves onto it, and any open file preview
/// closes — selecting a directory never previews it.
pub(crate) fn select_dir(cache: &mut RightCache, root: &Path, rel: &str) {
    expand_to(cache, root, rel);
    cache.selected.insert(root.to_path_buf(), rel.to_string());
    cache.previews.remove(root);
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
#[cfg(test)]
pub(crate) fn begin_file_preview(cache: &mut RightCache, root: &Path, id: &str) -> bool {
    begin_file_preview_highlight(cache, root, id, None)
}

/// [`begin_file_preview`] with a line band (B9): a link's `#L…`/`:line`
/// suffix rides [`FilePreview::highlight`] from the loading state through
/// the landed bytes, so the preview opens banded and scrolled there.
pub(crate) fn begin_file_preview_highlight(
    cache: &mut RightCache,
    root: &Path,
    id: &str,
    highlight: Option<std::ops::Range<u32>>,
) -> bool {
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
                highlight,
                size: 0,
                mtime: None,
                content: PreviewContent::Card {
                    meta: "The file is gone — it may have been moved or deleted.".into(),
                },
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
                highlight,
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
            highlight,
            size,
            mtime,
            content: PreviewContent::Loading,
        },
    );
    true
}

/// What a background probe of a preview path found (see
/// [`probe_file_preview`]).
#[derive(Debug)]
pub(crate) enum PreviewProbe {
    /// Nothing there.
    Gone,
    /// A folder.
    Dir,
    /// A file past [`PREVIEW_MAX_BYTES`].
    TooLarge(u64, Option<std::time::SystemTime>),
    /// A file, its size, mtime and bytes (`None` = the read failed).
    File(u64, Option<std::time::SystemTime>, Option<Vec<u8>>),
}

/// The filesystem half of a preview, run off the UI thread: metadata, then
/// the bytes when the file is small enough.
pub(crate) fn probe_file_preview(path: &Path) -> PreviewProbe {
    let Ok(meta) = std::fs::metadata(path) else {
        return PreviewProbe::Gone;
    };
    if meta.is_dir() {
        return PreviewProbe::Dir;
    }
    let size = meta.len();
    let mtime = meta.modified().ok();
    if size > PREVIEW_MAX_BYTES {
        return PreviewProbe::TooLarge(size, mtime);
    }
    PreviewProbe::File(size, mtime, std::fs::read(path).ok())
}

/// Select `id` and show it loading, touching no file: the UI-thread half of
/// a preview open.
pub(crate) fn mark_file_preview_loading(cache: &mut RightCache, root: &Path, id: &str, highlight: Option<std::ops::Range<u32>>) {
    let name = id.rsplit('/').next().unwrap_or(id).to_string();
    cache.selected.insert(root.to_path_buf(), id.to_string());
    cache.previews.insert(
        root.to_path_buf(),
        FilePreview { path: id.to_string(), name, language: preview_language(id), highlight, size: 0, mtime: None, content: PreviewContent::Loading },
    );
}

/// Land a [`probe_file_preview`] for the preview [`mark_file_preview_loading`]
/// started: the gone or too-large card, a folder's expand (the preview
/// closes), or the bytes. A stale landing (the selection moved on) changes
/// nothing. Returns whether the pane changed.
pub(crate) fn land_file_preview(cache: &mut RightCache, root: &Path, id: &str, probe: PreviewProbe) -> bool {
    let Some(current) = cache.previews.get_mut(root) else {
        return false;
    };
    if current.path != id || current.content != PreviewContent::Loading {
        return false;
    }
    match probe {
        PreviewProbe::Gone => {
            current.content = PreviewContent::Card { meta: "The file is gone — it may have been moved or deleted.".into() };
            true
        }
        PreviewProbe::Dir => {
            cache.previews.remove(root);
            toggle_expanded(cache, root, id);
            true
        }
        PreviewProbe::TooLarge(size, mtime) => {
            current.size = size;
            current.mtime = mtime;
            current.content = PreviewContent::Card { meta: format!("{} — too large to preview", friendly_size(size)) };
            true
        }
        PreviewProbe::File(size, mtime, bytes) => {
            current.size = size;
            current.mtime = mtime;
            complete_file_preview(cache, root, id, bytes)
        }
    }
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
    let highlight = current.highlight.clone();
    let content = match bytes {
        None => PreviewContent::Card {
            meta: "The file could not be read.".into(),
        },
        Some(bytes) if bytes.len() as u64 > PREVIEW_MAX_BYTES => PreviewContent::Card {
            meta: format!(
                "{} — too large to preview",
                friendly_size(bytes.len() as u64)
            ),
        },
        Some(bytes) => match String::from_utf8(bytes) {
            Ok(text) if is_markdown(&name) => PreviewContent::Markdown { text },
            Ok(code) => PreviewContent::Text {
                language: language.clone(),
                code,
            },
            Err(_) => PreviewContent::Card {
                meta: format!("{} — a binary file", friendly_size(current.size)),
            },
        },
    };
    cache.previews.insert(
        root.to_path_buf(),
        FilePreview {
            path: id.to_string(),
            name,
            language,
            highlight,
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
                Some(PreviewReload {
                    id: preview.path.clone(),
                    bytes: None,
                })
            } else {
                None
            };
        }
    };
    if meta.is_dir() {
        return if preview.mtime.is_some() {
            Some(PreviewReload {
                id: preview.path.clone(),
                bytes: None,
            })
        } else {
            None
        };
    }
    let size = meta.len();
    let mtime = meta.modified().ok();
    if mtime == preview.mtime && size == preview.size {
        return None;
    }
    let bytes = if size > PREVIEW_MAX_BYTES {
        None
    } else {
        std::fs::read(&path).ok()
    };
    Some(PreviewReload {
        id: preview.path.clone(),
        bytes,
    })
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
pub(crate) fn apply_preview_reload(
    cache: &mut RightCache,
    root: &Path,
    reload: PreviewReload,
) -> bool {
    let Some(current) = cache.preview_for(root) else {
        return false;
    };
    if current.path != reload.id || current.content == PreviewContent::Loading {
        return false;
    }
    // A re-read keeps the link's band: the reload carries bytes, not the
    // highlight, so it rides back in from the preview being replaced.
    let highlight = current.highlight.clone();
    if begin_file_preview_highlight(cache, root, &reload.id, highlight) {
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
                blocks.push(DocBlock::heading(
                    hashes as u8,
                    trimmed[hashes + 1..].trim().to_string(),
                ));
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
    let handler = files_handler(harness, root.to_path_buf());
    // Flush: these are panes and stacked panels, not cards floating on a
    // surface. Their own rounded border inside a column that already has
    // edges is what the owner saw as "extra borders and rounded corners".
    // A B9 reveal scrolls its row into view through the library's
    // `scroll_to` (unknown ids are ignored there).
    let _ = (name, footer);
    let mut tree = file_tree("right-files", nodes.to_vec()).flush();
    if let Some((reveal, token)) = cache.reveal.get(root) {
        tree = tree.scroll_to(reveal.clone()).scroll_token(*token);
    }
    let tree = tree.on_action(move |action, window, cx| handler(action, window, cx));
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
        PreviewContent::Text { language, code } => {
            let mut block = code_block("right-file-code", preview.path.clone(), code.clone())
                .language(language.clone())
                // The preview is the pane's own scroller: a long file
                // virtualises against the pane's height (L6 fill mode).
                .fill(true);
            // A B9 link band: highlight the linked lines and start the
            // list there (the library starts at the highlight when no
            // explicit scroll line rides along, so the start doubles).
            if let Some(range) = preview.highlight.clone() {
                block = block
                    .highlight_lines(range.clone())
                    .scroll_to_line(range.start);
            }
            block.into_any_element()
        }
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
fn browser_pane(
    state: &Entity<aui_webview::WebviewState>,
    tabs: &[StripTab],
    carry: bool,
    window: &mut Window,
    cx: &mut Context<Harness>,
) -> AnyElement {
    let harness = cx.weak_entity();
    let strip_harness = harness.clone();
    let pane =
        aui_webview::webview_pane("right-browser", state).on_intent(move |intent, window, cx| {
            if let Some(harness) = harness.upgrade() {
                harness.update(cx, |harness, cx| {
                    harness.handle_browser_intent(intent, window, cx);
                });
            }
        });
    let column = v_flex()
        .id("right-browser-pane")
        .role(gpui::Role::Group)
        .aria_label("Browser")
        .size_full();
    // D6: the session has one page — no tab strip in the pane.
    let _ = (tabs, carry, &strip_harness, window);
    column.child(pane).into_any_element()
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

    #[test]
    fn closing_the_browser_never_draws_the_loading_placeholder() {
        // B7: without a live webview the open pane shows the loading
        // placeholder while attaching — but a closed pane never does.
        // The live page (or its snapshot) owns the close; the placeholder
        // would flash over it while sliding out.
        use super::{browser_placeholder_for, BrowserPlaceholder};
        assert_eq!(
            browser_placeholder_for(true, true),
            BrowserPlaceholder::Page,
            "an open pane with a webview shows the page"
        );
        assert_eq!(
            browser_placeholder_for(false, true),
            BrowserPlaceholder::Page,
            "a closing pane keeps its live webview — the snapshot slides out, not a placeholder"
        );
        assert_eq!(
            browser_placeholder_for(true, false),
            BrowserPlaceholder::Loading,
            "an open pane still attaching shows the loading placeholder"
        );
        assert_eq!(
            browser_placeholder_for(false, false),
            BrowserPlaceholder::Closed,
            "a closed pane with no webview draws the closed marker, never the loading placeholder"
        );
    }

    use super::*;
    use gpui::TestAppContext;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("baaz-right-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    // ── B8b: this session first, the rest below ──

    fn status_file(path: &str, added: u32, removed: u32) -> FileChange {
        FileChange { path: path.to_owned(), change: ChangeKind::Modified, added, removed }
    }

    #[test]
    fn session_files_come_first_in_edit_order_and_others_stay_below() {
        // B8b: a session with two Edit blocks lists those two files first
        // (in edit order) and every other working-tree file below.
        let root = std::path::Path::new("/work/tree");
        let files = vec![
            status_file("docs/guide.md", 2, 1),
            status_file("src/b.rs", 8, 3),
            status_file("src/a.rs", 5, 0),
        ];
        let edits = vec!["src/b.rs".to_owned(), "src/a.rs".to_owned()];
        let (session, other) = partition_session_files(&files, &edits, root);
        let session_paths: Vec<&str> = session
            .iter()
            .map(|change| change.path.as_str())
            .collect();
        let other_paths: Vec<&str> = other
            .iter()
            .map(|change| change.path.as_str())
            .collect();
        assert_eq!(session_paths, vec!["src/b.rs", "src/a.rs"]);
        assert_eq!(other_paths, vec!["docs/guide.md"]);
    }

    #[test]
    fn absolute_edit_targets_still_match_relative_status_paths() {
        let root = std::path::Path::new("/work/tree");
        let files = vec![status_file("src/a.rs", 1, 1)];
        let edits = vec!["/work/tree/src/a.rs".to_owned()];
        let (session, other) = partition_session_files(&files, &edits, root);
        assert_eq!(session.len(), 1);
        assert!(other.is_empty());
        // An edit the tree does not contain matches nothing, and a repeat
        // never duplicates a row.
        let edits = vec![
            "nope.rs".to_owned(),
            "/work/tree/src/a.rs".to_owned(),
            "src/a.rs".to_owned(),
        ];
        let (session, other) = partition_session_files(&files, &edits, root);
        assert_eq!(session.len(), 1);
        assert!(other.is_empty());
    }

    #[test]
    fn a_suffix_only_edit_never_matches() {
        // B8c: the old `ends_with("/<status path>")` let a root-level `a.rs`
        // claim an edit to `/work/src/a.rs`. Matching is exact now: the
        // edit relativized against the root must equal the whole status path.
        let root = std::path::Path::new("/work");
        let files = vec![status_file("a.rs", 1, 1)];
        let edits = vec!["/work/src/a.rs".to_owned()];
        let (session, other) = partition_session_files(&files, &edits, root);
        assert!(session.is_empty());
        assert_eq!(other.len(), 1);
        // An edit outside the root matches nothing either.
        let edits = vec!["/elsewhere/a.rs".to_owned()];
        let (session, other) = partition_session_files(&files, &edits, root);
        assert!(session.is_empty());
        assert_eq!(other.len(), 1);
    }

    #[test]
    fn an_empty_session_leaves_the_whole_tree_for_other_changes() {
        let root = std::path::Path::new("/work/tree");
        let files = vec![status_file("src/a.rs", 1, 1)];
        let (session, other) = partition_session_files(&files, &[], root);
        assert!(session.is_empty());
        assert_eq!(other.len(), 1);
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
        assert_eq!(
            names,
            vec!["a.txt".to_string()],
            "a file-shaped .git must be skipped too"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_walk_caps_huge_directories() {
        let root = temp_root("walk-cap");
        for index in 0..5 {
            std::fs::write(root.join(format!("f{index}.txt")), "x").unwrap();
        }
        let (nodes, truncated) = walk_root_capped(&root, 3);
        // D6: the folder shows its first three, then its faint line.
        assert_eq!(nodes.iter().filter(|n| !n.note).count(), 3);
        assert!(nodes.last().is_some_and(|n| n.note && n.name.contains("first 3 of 5")), "{:?}", nodes.last());
        assert!(truncated);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Live 2026-10-19: the folder cap is per folder — the siblings after a
    /// capped, open folder still list.
    #[test]
    fn folders_after_a_capped_folder_still_list() {
        let root = temp_root("walk-capped-sibling");
        std::fs::create_dir_all(root.join("big")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        for index in 0..(FILE_WALK_CAP + 50) {
            std::fs::write(root.join("big").join(format!("f{index}.txt")), "x").unwrap();
        }
        std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let mut cache = RightCache::default();
        let expanded: HashSet<String> = ["big".to_string(), "src".to_string()].into();
        let (nodes, _) = walk_root_expanded_cached(&root, FILE_WALK_CAP, &expanded, cache.dir_cache_for_root(&root));
        assert!(nodes.iter().any(|n| n.note), "the capped folder ends with its line");
        assert!(nodes.iter().any(|n| n.id.as_ref() == "src"), "src follows the capped folder");
        assert!(nodes.iter().any(|n| n.id.as_ref() == "src/main.rs"), "and lists its file");
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
        let (nodes, truncated) = walk_root_expanded_cached(
            &root,
            FILE_WALK_CAP,
            &HashSet::new(),
            cache.dir_cache_for_root(&root),
        );
        assert!(truncated, "2000 entries over a 300 cap must truncate");
        assert_eq!(nodes.iter().filter(|n| !n.note).count(), FILE_WALK_CAP);
        assert!(nodes.last().is_some_and(|n| n.note && n.name.contains("of 2 000")));
        assert!(
            walk_collected_count() <= FILE_WALK_CAP + 1,
            "the read must stay bounded, held {}",
            walk_collected_count()
        );
        let names: Vec<&str> = nodes.iter().filter(|n| !n.note).map(|n| n.name.as_ref()).collect();
        assert_eq!(names.first(), Some(&"f0000.txt"));
        assert_eq!(names.last(), Some(&"f0299.txt"), "the folder's real first 300, alphabetically");
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
        let (nodes, truncated) = walk_root_expanded_cached(
            &root,
            FILE_WALK_CAP,
            &expanded,
            cache.dir_cache_for_root(&root),
        );
        assert!(truncated);
        // `big` itself plus its first 300 (each folder caps on its own now).
        assert_eq!(nodes.iter().filter(|n| !n.note).count(), FILE_WALK_CAP + 1);
        let first_reads = io_count();
        assert!(first_reads > 0, "the first refresh must read");
        let first_hits = cache_hit_count();
        // Nothing changed: the second refresh replays both directories.
        let (again, _) = walk_root_expanded_cached(
            &root,
            FILE_WALK_CAP,
            &expanded,
            cache.dir_cache_for_root(&root),
        );
        assert_eq!(again.len(), nodes.len());
        assert_eq!(
            io_count(),
            first_reads,
            "an unchanged tree must not re-read any directory (would re-read on the old wholesale walk)"
        );
        assert!(
            cache_hit_count() > first_hits,
            "both directories must replay from the cache"
        );
        // A new file moves only `big`'s mtime: only `big` is re-read.
        std::fs::write(root.join("big").join("f2000.txt"), "x").unwrap();
        let (later, truncated) = walk_root_expanded_cached(
            &root,
            FILE_WALK_CAP,
            &expanded,
            cache.dir_cache_for_root(&root),
        );
        assert!(truncated);
        assert_eq!(later.iter().filter(|n| !n.note).count(), FILE_WALK_CAP + 1);
        assert!(
            io_count() > first_reads,
            "a changed directory must be re-read"
        );
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
        assert!(
            preview_reload_for(&root, &preview).is_none(),
            "an unchanged file must not reload"
        );
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
        assert!(
            matches!(blocks[0], DocBlock::Heading { level: 1, .. }),
            "first block: {:?}",
            blocks[0]
        );
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
        assert!(nodes
            .iter()
            .any(|node| node.id.as_ref() == "sub/inner.txt" && node.depth == 1));
        assert!(nodes
            .iter()
            .find(|node| node.id.as_ref() == "sub")
            .is_some_and(|node| node.open == Some(true)));
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
        assert!(
            cache.preview_for(&root).is_none(),
            "the preview must be gone"
        );
        assert!(
            !close_file_preview(&mut cache, &root),
            "closing twice reports nothing open"
        );
        // The tree still marks the row the preview showed.
        let (nodes, _) = walk_root_capped(&root, FILE_WALK_CAP);
        let nodes = mark_selected(nodes, cache.selected_for(&root).as_deref());
        assert!(nodes
            .iter()
            .any(|node| node.id.as_ref() == "a.txt" && node.selected));
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
        assert!(!complete_file_preview(
            &mut cache,
            &root,
            "a.txt",
            Some(b"a\n".to_vec())
        ));
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
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    /// Point `BAAZ_STATE_DIR` at a fresh temp dir, restoring whatever was
    /// there before. The caller restores with [`restore_harness_dir`].
    fn hermetic_harness_dir(
        name: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        Option<std::ffi::OsString>,
        std::path::PathBuf,
    ) {
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
        state: (
            std::sync::MutexGuard<'static, ()>,
            Option<std::ffi::OsString>,
            std::path::PathBuf,
        ),
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

    /// Select and Toggle no longer toast: they preview and expand through
    /// the exact handler the pane wires up, on a hermetic [`Harness`].
    /// Only Search stays inert.
    #[gpui::test]
    fn files_select_and_toggle_act_instead_of_toasting(cx: &mut TestAppContext) {
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
            baaz.update(cx, |harness, cx| {
                harness.show_right(crate::layout::RightKind::Files, cx)
            })
        });
        vc.run_until_parked();
        let root = vc
            .update(|_, cx| baaz.read(cx).right_project().map(|(root, _)| root))
            .expect("the hermetic workspace is adopted at boot");
        vc.update(|window, cx| {
            let handle = files_handler(baaz.downgrade(), root.clone());
            handle(
                &FileTreeAction::Select(SharedString::from("note.md")),
                window,
                cx,
            );
            handle(
                &FileTreeAction::Toggle(SharedString::from("sub")),
                window,
                cx,
            );
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
            vc.update(|_, cx| baaz
                .read(cx)
                .right_cache
                .expanded_for(&root)
                .contains("sub")),
            "toggling sub must expand it"
        );
        // The re-read the toggle requested draws the child row.
        let nodes = vc
            .update(|_, cx| baaz.read(cx).right_cache.files_for(&root))
            .expect("the refresh lands the listing")
            .0;
        assert!(
            nodes
                .iter()
                .any(|node| node.id.as_ref() == "sub/inner.txt" && node.depth == 1),
            "an open directory shows its children: {:?}",
            nodes
                .iter()
                .map(|node| node.id.to_string())
                .collect::<Vec<_>>()
        );
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
            assert!(
                !state.read(cx).is_native(),
                "the scripted page is gpui, never a native overlay"
            );
            state.update(cx, |state, _| state.navigate("https://example.com"));
            assert_eq!(state.read(cx).url().as_ref(), "https://example.com");
        });
    }

    /// B9: `expand_to` stands every ancestor — and the folder itself —
    /// open, keeping whatever was already open.
    #[test]
    fn expand_to_opens_every_ancestor() {
        let root = temp_root("expand-to");
        let mut cache = RightCache::default();
        expand_to(&mut cache, &root, "crates/baaz/src");
        let open = cache.expanded_for(&root);
        assert!(open.contains("crates"), "the top ancestor opens");
        assert!(open.contains("crates/baaz"), "the middle ancestor opens");
        assert!(open.contains("crates/baaz/src"), "the folder itself opens");
        // Insert-only: a second reveal keeps the first folder's state.
        expand_to(&mut cache, &root, "docs");
        let open = cache.expanded_for(&root);
        assert!(
            open.contains("crates/baaz/src"),
            "the earlier folder stays open"
        );
        assert!(open.contains("docs"), "the new folder opens");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B9: selecting a directory moves the marker onto it and never
    /// previews — a folder reveal shows the tree, not a file.
    #[test]
    fn selecting_a_directory_sets_selection_and_no_preview() {
        let root = temp_root("select-dir");
        std::fs::create_dir_all(root.join("crates/baaz/src")).unwrap();
        let mut cache = RightCache::default();
        select_dir(&mut cache, &root, "crates/baaz/src");
        assert_eq!(
            cache.selected_for(&root).as_deref(),
            Some("crates/baaz/src")
        );
        assert!(
            cache.preview_for(&root).is_none(),
            "a folder reveal previews nothing"
        );
        let open = cache.expanded_for(&root);
        assert!(open.contains("crates") && open.contains("crates/baaz/src"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// B9: a file preview opened with a line band keeps it from the
    /// loading state through the landed bytes.
    #[test]
    fn a_file_preview_carries_its_line_band() {
        let root = temp_root("preview-line");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        let mut cache = RightCache::default();
        assert!(begin_file_preview_highlight(
            &mut cache,
            &root,
            "src/main.rs",
            Some(2..3)
        ));
        assert_eq!(
            cache
                .preview_for(&root)
                .and_then(|preview| preview.highlight),
            Some(2..3),
            "the loading state already carries the band"
        );
        assert!(complete_file_preview(
            &mut cache,
            &root,
            "src/main.rs",
            Some(b"fn main() {}\n".to_vec())
        ));
        let preview = cache.preview_for(&root).expect("a preview");
        assert_eq!(
            preview.highlight,
            Some(2..3),
            "the landed bytes keep the band"
        );
        assert_eq!(preview.language, "rust", "a .rs file previews as rust");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The probe/mark/land trio against a real directory.
    fn landed(root: &std::path::Path, id: &str) -> (RightCache, bool) {
        let mut cache = RightCache::default();
        mark_file_preview_loading(&mut cache, root, id, None);
        let probe = probe_file_preview(&root.join(id));
        let changed = land_file_preview(&mut cache, root, id, probe);
        (cache, changed)
    }

    #[test]
    fn landing_a_missing_path_shows_the_gone_card() {
        let root = temp_root("land-gone");
        let (cache, changed) = landed(&root, "nope.txt");
        assert!(changed);
        let preview = cache.preview_for(&root).expect("preview stays open");
        assert!(
            matches!(&preview.content, PreviewContent::Card { meta } if meta.starts_with("The file is gone")),
            "{:?}",
            preview.content
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn landing_a_directory_closes_the_preview_and_expands_it() {
        let root = temp_root("land-dir");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let (cache, changed) = landed(&root, "sub");
        assert!(changed);
        assert!(cache.preview_for(&root).is_none());
        assert!(cache.expanded_for(&root).contains("sub"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn landing_an_oversized_file_shows_the_too_large_card_with_its_size() {
        let root = temp_root("land-big");
        std::fs::write(root.join("big.bin"), vec![b'x'; (PREVIEW_MAX_BYTES + 1) as usize]).unwrap();
        let (cache, changed) = landed(&root, "big.bin");
        assert!(changed);
        let preview = cache.preview_for(&root).expect("preview");
        assert_eq!(preview.size, PREVIEW_MAX_BYTES + 1);
        assert!(preview.mtime.is_some());
        assert!(
            matches!(&preview.content, PreviewContent::Card { meta } if meta.contains("too large")),
            "{:?}",
            preview.content
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn landing_a_small_text_file_shows_its_text_with_size_and_mtime() {
        let root = temp_root("land-text");
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let (cache, changed) = landed(&root, "a.txt");
        assert!(changed);
        let preview = cache.preview_for(&root).expect("preview");
        assert_eq!(preview.size, 6);
        assert!(preview.mtime.is_some());
        assert!(
            matches!(&preview.content, PreviewContent::Text { code, .. } if code == "hello\n"),
            "{:?}",
            preview.content
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_stale_landing_after_the_selection_moved_changes_nothing() {
        let root = temp_root("land-stale");
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        let mut cache = RightCache::default();
        mark_file_preview_loading(&mut cache, &root, "a.txt", None);
        mark_file_preview_loading(&mut cache, &root, "b.txt", None);
        let probe = probe_file_preview(&root.join("a.txt"));
        assert!(!land_file_preview(&mut cache, &root, "a.txt", probe));
        let preview = cache.preview_for(&root).expect("preview");
        assert_eq!(preview.path, "b.txt");
        assert_eq!(preview.content, PreviewContent::Loading);
        assert_eq!(preview.size, 0);
        let _ = std::fs::remove_dir_all(&root);
    }


    /// Owner 2026-10-21: the address field sat under the pane row's fade.
    #[test]
    fn the_row_fade_never_veils_a_screens_own_chrome() {
        use super::opens_on_chrome;
        assert!(opens_on_chrome(RightKind::Browser, false, false), "the Page's nav row");
        assert!(opens_on_chrome(RightKind::Files, false, true), "the open Files filter");
        assert!(!opens_on_chrome(RightKind::Files, false, false), "the tree scrolls under the fade");
        assert!(!opens_on_chrome(RightKind::Changes, false, false));
        assert!(!opens_on_chrome(RightKind::Browser, true, false), "a pushed view opens on content");
    }
}
