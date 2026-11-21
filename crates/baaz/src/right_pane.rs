//! The right pane as its own entity (task 4.3): the first
//! [`Region`](crate::region::Region).
//!
//! [`RightPane`] owns everything only the pane reads: the read cache
//! ([`RightCache`]), the divider's width and drag, the git/filesystem refresh
//! (its in-flight and again flags, the reconcile key, the landed snapshot's
//! hash, the file watch and its task), the open/close snap, the strip's
//! motion flag, the Files keyboard focus, the drafted PR body waiting for a
//! window, and the Files context menu. It renders itself (`impl Render`):
//! `Harness::render` composes the view and seats the pane's tab row
//! ([`Region::actions`]) in the shell's header, and no longer builds either.
//!
//! What only the root knows reaches the pane as [`RightFacts`]: the project
//! the pane reads for, the active session (its view, for "This session" and
//! pushed outputs; its lane provider, for drafts and the agent's name), the
//! pending-switch neutrality, the Settings page's hide, and the session's
//! live page. The root pushes them every frame before composing the pane,
//! and before every call it makes into the pane ([`Harness::with_right_pane`]),
//! so a call never acts on a stale project.
//!
//! What the pane cannot do itself goes out as a [`RightPaneEvent`]: persisting
//! the session's pane state (the sessions store is the root's), opening the
//! pane on a kind or closing it (the layout gesture with its webview prewarm
//! and save), a URL for the Page, a quote for the composer, a draft from the
//! session's provider, the PR in Browse, the views menu, a page intent. The
//! root subscribes once in `Harness::new` and maps each to its own method.
//!
//! The pane is the session's workspace (docs/82 8.1): Changes · Files · Page
//! · Terminal · Tasks on the strip, and a second pane may stack below the
//! strip on its own kind ([`crate::layout::Layout::shown_lower`]) under a
//! divider the pane drags itself ([`LowerDivider`]). The Terminal view is
//! the terminal dock's own body ([`crate::terminal::TerminalDock::pane_screen`]):
//! the pane is given the dock at construction, as it is given the host.
//!
//! The per-frame reconcile that used to run in the root's per-frame hook
//! (`sync_right_cache`) runs in the pane's own render, and a layout observer
//! drops the watch when the pane closes (a closed pane is not mounted, so it
//! does not render). The pane is composed uncached: the facts it draws from
//! are pushed during the root's render, so a cached pane would miss them.

use std::path::PathBuf;

use gpui::{
    prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable,
    SharedString, Task, Window,
};
use gpui_kit::base::input::TextareaState;

use crate::app::Harness;
use crate::layout::{self, RightKind};
use crate::overlays::Overlays;
use crate::region::Region;
use crate::resize::RightResizeDrag;
use crate::right::{self, RightCache};
use crate::session::SessionView;
use crate::layout::LayoutGlobal as _;
use aui_tokens::ActiveAui as _;

#[cfg(test)]
thread_local! {
    /// Render probe (tests): how many changed rows the pane's last render
    /// drew from its cache for the shown project, `None` before any data.
    /// Moved only by [`RightPane::render`], so a test can tell what reached
    /// the screen.
    static DRAWN_CHANGES: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

/// What the last render of the pane drew: the changed rows of the shown
/// project (see [`DRAWN_CHANGES`]).
#[cfg(test)]
pub(crate) fn drawn_changes() -> Option<usize> {
    DRAWN_CHANGES.with(|drawn| drawn.get())
}

/// What the pane needs from the root: pushed every frame and before every
/// root call into the pane (see the module doc). Never persists.
#[derive(Clone, Default)]
pub(crate) struct RightFacts {
    /// The project the pane reads for: root plus display name (a worktree
    /// session's worktree, under its project's name).
    pub project: Option<(PathBuf, String)>,
    /// The active session's view: its edited files ("This session") and its
    /// tool outputs (pushed full-output views).
    pub session: Option<Entity<SessionView>>,
    /// The active session's lane provider: the agent's name on the Page and
    /// the route a commit or PR draft takes.
    pub provider: Option<crate::providers::ProviderId>,
    /// A provider switch is pending (Q4c): the pane keeps its width and
    /// shows nothing session-specific.
    pub pending_neutral: bool,
    /// The Settings page stands open: the pane is hidden, not closed.
    pub settings_open: bool,
    /// The session's live page, when one exists (the root builds it).
    pub page: Option<Entity<aui_webview::WebviewState>>,
    /// The window's lifecycle runs: signed in, or a live launch. An offline
    /// or replayed capture that is still signed out reconciles nothing per
    /// frame (the root's frame never did), so no read and no file watch
    /// starts behind it.
    pub live: bool,
}

impl RightFacts {
    /// The agent's name the Page and the commit box speak of.
    pub(crate) fn agent(&self) -> &'static str {
        self.provider.map_or(crate::providers::THE_AGENT, |provider| provider.label())
    }
}

/// What the pane asks of the root (see the module doc).
#[derive(Clone, Debug)]
pub(crate) enum RightPaneEvent {
    /// The pane's per-session state changed (a preview, a selection, an
    /// expansion): save it onto the active session's record.
    Persist,
    /// Open the pane on a kind, never closing it (`Harness::select_right`).
    Select(RightKind),
    /// The ✕: toggle the pane (`Harness::toggle_right`).
    Toggle,
    /// Open `url` as the session's page (HTML, SVG and PDF rows; the
    /// no-page address field).
    OpenUrl(String),
    /// Reveal an absolute file in Files through the link path (a Changes
    /// row's file button).
    Reveal { root: PathBuf, abs: PathBuf },
    /// A Changes row's "Revert" (docs/82 6.4): `path` in `root`, to `HEAD`
    /// (`turn` None, All changes) or out of that turn (its id, number and
    /// ends: Last turn, Turn N). The session asks before it changes
    /// anything.
    RevertFile { root: PathBuf, path: String, turn: Option<(String, usize, String, String)> },
    /// Quote text into the active session's composer as a code quote in
    /// `lang`, `lines` long: a refused hook's output (no location), or lines
    /// selected in a diff or a preview (docs/82 7.1, with where they came
    /// from).
    Quote { text: String, lang: String, lines: usize, location: Option<crate::session::QuoteLocation> },
    /// A new change set wants a commit message drafted on the session's
    /// provider; the pane already marked it drafting.
    CommitDraftWanted { root: PathBuf, files: Vec<aui_protocol::FileChange> },
    /// Create PR: the form needs the session's title, provider and model.
    OpenPrForm(PathBuf),
    /// A draft goes through a hidden side session (Muse).
    DraftOnSide { root: PathBuf, kind: crate::right_changes::DraftKind, prompt: String },
    /// The PR card's View (or a check's page): the URL in Browse.
    ViewPullRequest(String),
    /// A PR read landed for `root` on the feature branch `branch` (docs/82
    /// 7.3): the rows' book takes it, and a merged one may settle the
    /// session.
    PullRequestRead { root: PathBuf, branch: String, pr: Option<crate::git_ops::PullRequest> },
    /// The merged card's Archive this session: the open session.
    ArchiveSession,
    /// The tab row's ＋: the views menu.
    OpenViewsMenu,
    /// A Tasks row: show the terminal on the tab its command runs in.
    OpenTerminalTab(String),
    /// The lower pane's ✕ (docs/82 8.1).
    CloseLower,
    /// What the Page's own controls ask (screenshot, annotate, console…).
    BrowserIntent(aui_webview::WebviewIntent),
}

/// The refresh bookkeeping: one read in flight at most, a second asked for
/// while it runs, the last reconciled (open, kind, root), the last landed
/// snapshot's hash, and the file watch with its task.
#[derive(Default)]
struct Refresh {
    in_flight: bool,
    again: bool,
    last_key: Option<(bool, RightKind, Option<PathBuf>)>,
    print: Option<u64>,
    watch: Option<(crate::right_watch::RightWatch, Task<()>)>,
}

/// The lower pane's divider (docs/82 8.1): the drag in flight, and the
/// bounds of the pane's body the top pane's share is measured against.
#[derive(Default)]
pub(crate) struct LowerDivider {
    /// The grab y in window pixels and the share when the drag began.
    drag: Option<(f32, f32)>,
    /// The body's bounds, from its last prepaint.
    bounds: std::rc::Rc<std::cell::Cell<Option<gpui::Bounds<gpui::Pixels>>>>,
}

/// The top pane's share after the pointer moved from `grab` to `at` over a
/// body `height` tall, starting from `start`: the divider follows the
/// pointer, held to its range ([`crate::layout::clamp_split`]).
pub(crate) fn lower_drag_share(start: f32, grab: f32, at: f32, height: f32) -> f32 {
    if height <= 0.0 {
        return crate::layout::clamp_split(start);
    }
    crate::layout::clamp_split(start + (at - grab) / height)
}

/// The right pane: Changes · Files · Page · Terminal · Tasks on one strip
/// under its tab row, and the lower pane under it when one stands open.
pub(crate) struct RightPane {
    /// The last git/diff/filesystem reads, with the roots and instants they
    /// were read for. [`right::render`] draws from this and never touches a
    /// subprocess or the filesystem itself.
    pub(crate) cache: RightCache,
    /// The divider's width and whatever drag is in flight over it. Never
    /// active while the sidebar's drag is: one capture overlay covers both.
    pub(crate) resize: RightResizeDrag,
    refresh: Refresh,
    /// A session-switch restore just moved the pane: the next frame renders
    /// it at its target with no open/close animation (Z2). Armed by the
    /// restore when it flips open/closed, consumed once by the root's render
    /// through the shell's `resizing` bypass ([`Self::take_snap`]). User
    /// toggles disarm it, so they keep animating.
    pub(crate) snap: bool,
    /// D6: the strip is between screens this frame (the Page screen's native
    /// view hides behind its still picture while it moves).
    pub(crate) strip_moving: bool,
    /// D6: the Files screen's keyboard focus (↑ ↓ ← → ⏎, ⌘F, /, Esc): the
    /// pane's focus handle.
    pub(crate) focus: FocusHandle,
    /// D6: a drafted PR description waiting for a frame with the window to
    /// land in its editor.
    pub(crate) pending_pr_body: Option<(Entity<TextareaState>, String)>,
    /// The window's floating layer, shared with the root: the pane's toasts,
    /// and its two menus (the Files context menu, the Page row's Recent
    /// pages) as entries on the overlay stack.
    pub(crate) overlays: Entity<Overlays>,
    /// The terminal's tabs and their task book (docs/82 6.5): the Tasks
    /// screen draws the open session's tasks and stops them through it.
    pub(crate) host: Entity<crate::terminal::TerminalHost>,
    /// The terminal (docs/82 8.1): the Terminal view draws the dock's own
    /// body, so the pane and the dock are one terminal.
    pub(crate) terminal: Entity<crate::terminal::TerminalDock>,
    /// The lower pane's divider and its drag.
    pub(crate) lower: LowerDivider,
    /// What the root told the pane (see [`RightFacts`]).
    pub(crate) facts: RightFacts,
    /// The layout observer: drops the watch when the pane closes.
    _layout: gpui::Subscription,
}

impl EventEmitter<RightPaneEvent> for RightPane {}

impl RightPane {
    /// How often the open pane re-reads git and the filesystem while it sits
    /// open on a kind that reads them: at most once per interval.
    const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);
    /// The fallback poll while a file watch follows the root.
    const WATCHED_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
    /// How long a burst of file events settles before one refresh.
    const WATCH_SETTLE: std::time::Duration = std::time::Duration::from_millis(150);

    /// The pane at boot: the divider at `width` (what `layout.json`
    /// restored), nothing read yet.
    pub(crate) fn new(
        width: f32,
        overlays: Entity<Overlays>,
        host: Entity<crate::terminal::TerminalHost>,
        terminal: Entity<crate::terminal::TerminalDock>,
        cx: &mut Context<Self>,
    ) -> Self {
        let layout = cx.observe_global::<layout::LayoutStore>(|this: &mut Self, cx| {
            this.drop_watch_if_idle(cx);
            cx.notify();
        });
        let mut cache = RightCache::default();
        cache.line_select.focus = Some(cx.focus_handle());
        Self {
            cache,
            resize: RightResizeDrag::restored(width),
            refresh: Refresh::default(),
            snap: false,
            strip_moving: false,
            focus: cx.focus_handle(),
            pending_pr_body: None,
            overlays,
            host,
            terminal,
            lower: LowerDivider::default(),
            facts: RightFacts::default(),
            _layout: layout,
        }
    }

    /// The divider's width now, in window pixels.
    pub(crate) fn width(&self) -> f32 {
        self.resize.width
    }

    /// Consume the restore's snap (one frame lands without animating).
    pub(crate) fn take_snap(&mut self) -> bool {
        std::mem::take(&mut self.snap)
    }

    /// A toast on the window's floating layer.
    pub(crate) fn toast(&mut self, title: &str, body: impl Into<String>, cx: &mut Context<Self>) {
        let body = body.into();
        self.overlays.update(cx, |overlays, cx| {
            overlays.toast(title, body, cx);
        });
    }

    /// The pane's per-session Files state for `root`, as the session record
    /// stores it: the previewed file, the selected row, the open folders
    /// (sorted).
    pub(crate) fn files_state(&self, root: &std::path::Path) -> (Option<String>, Option<String>, Vec<String>) {
        let preview = self.cache.preview_for(root).map(|preview| preview.path.clone());
        let selected = self.cache.selected_for(root);
        let mut expanded: Vec<String> = self.cache.expanded_for(root).into_iter().collect();
        expanded.sort();
        (preview, selected, expanded)
    }

    /// Put a session's stored Files state back onto `root` (Z2): expansion,
    /// selection and the preview (re-read off the UI thread), or nothing at
    /// all for a session with no stored state.
    pub(crate) fn restore_files(&mut self, root: &std::path::Path, stored: Option<&crate::sessions::RightState>, cx: &mut Context<Self>) {
        match stored {
            Some(state) => {
                let expanded: std::collections::HashSet<String> = state.files_expanded.iter().cloned().collect();
                if expanded.is_empty() {
                    self.cache.expanded.remove(root);
                } else {
                    self.cache.expanded.insert(root.to_path_buf(), expanded);
                }
                match &state.files_selected {
                    Some(selected) => {
                        self.cache.selected.insert(root.to_path_buf(), selected.clone());
                    }
                    None => {
                        self.cache.selected.remove(root);
                    }
                }
                match &state.files_preview {
                    Some(preview) => self.begin_file_preview_for(root, preview, cx),
                    None => {
                        right::close_file_preview(&mut self.cache, root);
                    }
                }
            }
            None => {
                self.cache.expanded.remove(root);
                self.cache.selected.remove(root);
                right::close_file_preview(&mut self.cache, root);
            }
        }
    }

    /// The active session's edited files, oldest first — the Changes pane's
    /// "This session" section — from the view's cache keyed by its turns, so
    /// a steady frame reads two values and scans nothing. Empty with no
    /// session.
    pub(crate) fn session_edits(&self, cx: &mut Context<Self>) -> Vec<String> {
        self.facts
            .session
            .as_ref()
            .map(|view| match view.read(cx).fresh_session_edits(cx) {
                Some(fresh) => fresh,
                None => view.update(cx, |view, cx| view.refresh_session_edits(cx)),
            })
            .unwrap_or_default()
    }

    // ── refresh ─────────────────────────────────────────────────────────

    /// Whether `kind` shows data read from git or the filesystem: Changes
    /// and Files (the Page draws a webview, Terminal the ptys, Tasks the
    /// terminal's book).
    fn needs_data(kind: RightKind) -> bool {
        matches!(kind, RightKind::Changes | RightKind::Files)
    }

    /// The kind whose data the open pane reads: the top one when it reads
    /// any, else the lower pane's (docs/82 8.1). `None` while the pane is
    /// closed or shows nothing read from git or the filesystem. One read
    /// lands both slots, so one kind paces it.
    fn reading_kind(cx: &App) -> Option<RightKind> {
        let layout = layout::Layout::get(cx);
        if !layout.right_open {
            return None;
        }
        let top = layout::right_kind(layout);
        if Self::needs_data(top) {
            return Some(top);
        }
        layout.shown_lower().filter(|kind| Self::needs_data(*kind))
    }

    /// Keep a file watch on `root` while the pane shows it: a change on disk
    /// refreshes the pane within [`Self::WATCH_SETTLE`].
    fn ensure_watch(&mut self, root: Option<&std::path::Path>, cx: &mut Context<Self>) {
        let Some(root) = root else {
            self.refresh.watch = None;
            return;
        };
        // Tests never start one either: its fsevents thread wakes the test
        // scheduler from outside, which the scheduler refuses as
        // nondeterministic (`right_watch` tests the watch itself).
        if crate::runtime::captures() || cfg!(test) {
            return;
        }
        if self.refresh.watch.as_ref().is_some_and(|(watch, _)| watch.root() == root) {
            return;
        }
        let Some((watch, mut events)) = crate::right_watch::RightWatch::start(root) else {
            self.refresh.watch = None;
            return;
        };
        let task = cx.spawn(async move |this, cx| {
            use futures::StreamExt as _;
            while events.next().await.is_some() {
                cx.background_executor().timer(Self::WATCH_SETTLE).await;
                while events.try_recv().is_ok() {}
                if this.update(cx, |this, cx| this.refresh_now(cx)).is_err() {
                    return;
                }
            }
        });
        self.refresh.watch = Some((watch, task));
    }

    /// Re-read git and the filesystem for the open pane, off the render path.
    /// No-op while the pane is closed, on the Page, or with no project. At
    /// most one re-read is ever in flight: an explicit request (opening, a
    /// kind switch, the file tree's Refresh) while one runs is coalesced, and
    /// the interval in [`Self::sync_refresh`] paces the steady state. Under
    /// `BAAZ_DETERMINISTIC` the read is fixtures plus a capped walk and runs
    /// inline, so captures settle on their first frame; otherwise it runs on
    /// the background executor and lands with an update + notify of the pane
    /// alone. Nothing here blocks the UI thread.
    pub(crate) fn refresh_now(&mut self, cx: &mut Context<Self>) {
        let Some(kind) = Self::reading_kind(cx) else { return };
        let Some((root, _)) = self.facts.project.clone() else { return };
        // The walk draws around this: a re-read never collapses a toggled
        // directory or drops the preview's tree.
        let expanded = self.cache.expanded_for(&root);
        if crate::runtime::captures() {
            let at = std::time::Instant::now();
            let snapshot = {
                let dir_cache = self.cache.dir_cache_for_root(&root);
                right::read_snapshot_for_cached(&root, &expanded, dir_cache)
            };
            let reload = self.cache.preview_for(&root).and_then(|preview| right::preview_reload_for(&root, &preview));
            self.cache.apply_snapshot(snapshot, at);
            if let Some(reload) = reload {
                right::apply_preview_reload(&mut self.cache, &root, reload);
            }
            self.changes_after_snapshot(&root, cx);
            self.reload_open_diffs(&root, cx);
            self.refresh.last_key = Some((true, kind, Some(root)));
            cx.notify();
            return;
        }
        if self.refresh.in_flight {
            self.refresh.again = true;
            return;
        }
        self.refresh.in_flight = true;
        // The listing cache and the open preview travel into the background
        // task: unchanged directories are replayed there, and a preview whose
        // file changed on disk is re-read there — never on the render path.
        let dir_cache = self.cache.take_dir_cache();
        let preview = self.cache.preview_for(&root);
        cx.spawn(async move |this, cx| {
            let (snapshot, dir_cache, reload, print) = cx
                .background_executor()
                .spawn(async move {
                    let mut dir_cache = dir_cache;
                    let snapshot = right::read_snapshot_for_cached(&root, &expanded, &mut dir_cache);
                    let reload = preview.as_ref().and_then(|preview| right::preview_reload_for(&root, preview));
                    // What the pane would draw, hashed here off the UI
                    // thread: an unchanged refresh lands without a redraw.
                    let print = {
                        use std::hash::{Hash, Hasher};
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        format!("{snapshot:?}").hash(&mut hasher);
                        hasher.finish()
                    };
                    (snapshot, dir_cache, reload, print)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.refresh.in_flight = false;
                this.cache.restore_dir_cache(dir_cache);
                let root = snapshot.root.clone();
                this.cache.apply_snapshot(snapshot, std::time::Instant::now());
                let reloaded = reload.is_some();
                if let Some(reload) = reload {
                    right::apply_preview_reload(&mut this.cache, &root, reload);
                }
                this.changes_after_snapshot(&root, cx);
                this.reload_open_diffs(&root, cx);
                // The 2 s refresh used to redraw the window even when git and
                // the tree were exactly as before (perf audit 2026-10-16).
                if reloaded || this.refresh.print != Some(print) {
                    this.refresh.print = Some(print);
                    cx.notify();
                }
                if std::mem::take(&mut this.refresh.again) {
                    this.refresh_now(cx);
                }
            });
        })
        .detach();
    }

    /// Reconcile the refresh state, from the pane's own render: re-read at
    /// once when the pane opened, its kind changed, or its project changed;
    /// re-read on the interval while it sits open on a data kind; never
    /// while closed or on the Page. The `right:` step verb writes the layout
    /// directly, so it is covered here rather than at a call site.
    fn sync_refresh(&mut self, cx: &mut Context<Self>) {
        if !self.facts.live {
            return;
        }
        if self.drop_watch_if_idle(cx) {
            return;
        }
        let Some(kind) = Self::reading_kind(cx) else { return };
        let root = self.facts.project.as_ref().map(|(root, _)| root.clone());
        self.ensure_watch(root.as_deref(), cx);
        if self.refresh.last_key != Some((true, kind, root.clone())) {
            self.refresh.last_key = Some((true, kind, root));
            self.refresh_now(cx);
            return;
        }
        // The steady state, paced off when the slot for this kind last
        // landed: never fetched re-reads at once, a fresh landing waits out
        // the interval, and a hung in-flight read is never doubled.
        let Some(root) = root else { return };
        // With a live watch the poll is only a fallback.
        let interval = if self.refresh.watch.is_some() { Self::WATCHED_INTERVAL } else { Self::REFRESH_INTERVAL };
        let due = self.cache.fetched_at(kind, &root).is_none_or(|at| at.elapsed() >= interval);
        if due && !self.refresh.in_flight {
            self.refresh_now(cx);
        }
    }

    /// The closed (or Page) half of the reconcile, which needs no frame:
    /// note the key and drop the watch. A closed pane is not mounted, so
    /// the layout observer runs this instead of the render. Returns whether
    /// the pane is idle.
    fn drop_watch_if_idle(&mut self, cx: &mut Context<Self>) -> bool {
        if Self::reading_kind(cx).is_some() {
            return false;
        }
        let kind = layout::right_kind(layout::Layout::get(cx));
        let open = layout::Layout::get(cx).right_open;
        let root = self.facts.project.as_ref().map(|(root, _)| root.clone());
        self.refresh.last_key = Some((open, kind, root));
        self.refresh.watch = None;
        true
    }

    /// A two-finger sideways swipe in the pane: one screen along the strip
    /// (Changes · Files · Page · Terminal · Tasks), stopping at either end.
    fn swipe_screen(&mut self, dir: i32, cx: &mut Context<Self>) {
        let at = right::screen_index(layout::right_kind(layout::Layout::get(cx))) as i32 + dir;
        if let Some(kind) = usize::try_from(at).ok().and_then(|i| RightKind::ALL.get(i).copied()) {
            cx.emit(RightPaneEvent::Select(kind));
        }
    }

    /// Whether the Create PR form's base-branch menu stands open (an entry
    /// on the overlay stack, so Escape and a click away close it
    /// newest-first).
    pub(crate) fn base_menu_open(&self, cx: &gpui::App) -> bool {
        self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::PrBaseMenu)
    }

    /// Open (this pane drawing it) or close the base-branch menu.
    pub(crate) fn set_base_menu(&mut self, open: bool, cx: &mut Context<Self>) {
        let owner = cx.entity_id();
        self.overlays.update(cx, |o, cx| {
            if open {
                o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::PrBaseMenu).owner(owner), cx);
            } else {
                o.close(crate::overlay_stack::LayerKind::PrBaseMenu, cx);
            }
        });
    }

    /// Whether the Page row's Recent pages menu stands open (an entry on the
    /// overlay stack).
    pub(crate) fn page_menu_open(&self, cx: &gpui::App) -> bool {
        self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::PageMenu)
    }

    /// Whether the Page screen draws its own state over `state`, so the
    /// native view hides (see [`crate::right_page::page_covered`]).
    pub(crate) fn page_covered(&self, state: &aui_webview::WebviewState) -> bool {
        crate::right_page::page_covered(state, &self.cache)
    }
}

impl Focusable for RightPane {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Region for RightPane {
    /// The shown screen's name ("Changes", "Files", "Page", "Terminal",
    /// "Tasks"). In Browse the
    /// Page kind shows Changes (B2a-H2b), and the title says so.
    fn title(&self, cx: &App) -> SharedString {
        let kind = layout::right_kind(layout::Layout::get(cx));
        let shown = if layout::Layout::get(cx).mode == layout::Mode::Browse && kind == RightKind::Browser { RightKind::Changes } else { kind };
        shown.label().into()
    }

    /// The pane's tab row (docs/83 §6), for the shell to seat in the right
    /// column's 44 px header in both modes: Changes · Files · Page ·
    /// Terminal, Tasks while it shows or the session has tasks, the open file's tab while
    /// Files previews one (strip mode only), `＋`, maximise and ✕.
    fn actions(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let kind = layout::right_kind(layout::Layout::get(cx));
        let strip_mode = layout::Layout::get(cx).mode != layout::Mode::Browse && !self.facts.pending_neutral;
        let open_file = if strip_mode {
            self.facts.project.as_ref().and_then(|(root, _)| {
                self.cache.preview_for(root).map(|preview| (root.clone(), SharedString::from(preview.name)))
            })
        } else {
            None
        };
        let tasks = self.tasks_tab_shown(kind, cx);
        Some(self.render_tabs(kind, open_file, tasks, cx))
    }

    /// The pane draws over its Page while the strip slides (the page shows
    /// as its still picture) and while the Page screen shows Baaz's own
    /// state there: no page yet, couldn't load, crashed, a pushed view, a
    /// pin's note.
    fn covers_page(&self, cx: &App) -> bool {
        self.strip_moving || self.facts.page.as_ref().is_some_and(|page| self.page_covered(page.read(cx)))
    }
}

impl Render for RightPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The sidebar drag's twin: a release outside the window never
        // reaches the capture overlay, so an armed divider drag settles
        // where it stands when the window goes inactive.
        if self.resize.active && !self.resize.scripted && !window.is_window_active() {
            self.resize.active = false;
            self.resize.persist(cx);
        }
        // The git and filesystem reads, reconciled off the render path: at
        // once on open, kind or project change, on the 2 s interval while
        // open on a data kind, never while closed or on the Page.
        self.sync_refresh(cx);
        // The Changes scope row shows for the open session's checkpoints
        // only (docs/82 6.3): the screen draws from the cache, so the id
        // rides there.
        self.cache.changes.scopes.shown_session = self.scope_session_id(cx);
        self.cache.changes.scopes.revert = self.revert_offered(cx);
        // docs/82 7.1: lines quote into the open session's composer; with
        // none open (or a switch pending) there is nowhere to put them.
        self.cache.line_select.can_quote = self.can_quote();
        if !self.cache.pushed.is_empty() {
            self.sync_pushed_outputs(cx);
        }
        // A drafted PR description lands in its editor here, where the
        // window is at hand (the draft came back on a background task).
        if let Some((body, text)) = self.pending_pr_body.take() {
            body.update(cx, |state, cx| state.set_value(&text, window, cx));
        }
        let kind = layout::right_kind(layout::Layout::get(cx));
        let right_open = layout::Layout::get(cx).right_open && !self.facts.settings_open;
        let browse_mode = layout::Layout::get(cx).mode == layout::Mode::Browse;
        // Q4c: the neutral body — an empty pane that keeps its width, so the
        // centre never resizes mid-switch.
        if self.facts.pending_neutral {
            return gpui_kit::base::v_flex().size_full().into_any_element();
        }
        let project = self.facts.project.clone();
        let width = self.resize.width;
        #[cfg(test)]
        DRAWN_CHANGES.with(|drawn| {
            let rows = project.as_ref().and_then(|(root, _)| self.cache.git_for(root).flatten()).map(|tree| tree.files.len());
            drawn.set(rows);
        });
        // D6 (docs/43 §2.3): in Sessions the pane is a window onto one of
        // five screens on a strip — Changes · Files · Page · Terminal · Tasks
        // (docs/82 8.1) — under the
        // pane's tab row (docs/83 §6), each with its own sub-header if any;
        // a tab or toggle slides the strip (240 ms ease-out), a two-finger
        // sideways swipe moves one screen. Browse keeps its pane's body as it
        // was (out of D6's scope): one body under the same tab row.
        if browse_mode {
            // B2a-H2b: in Browse the pane never drives a native page — one
            // tab, one page. The Page kind falls back to Changes; the header
            // keeps the stored kind, only the body falls back.
            let body_kind = if kind == RightKind::Browser { RightKind::Changes } else { kind };
            if right_open && body_kind == RightKind::Tasks {
                self.sync_tasks(cx);
            }
            // B8c: "This session" is computed only while the pane shows
            // Changes (the walk is O(all blocks), so every other kind skips it).
            let session_edits = if right_open && body_kind == RightKind::Changes { self.session_edits(cx) } else { Vec::new() };
            let body = right::render(
                body_kind,
                &self.cache,
                project,
                &session_edits,
                right::BrowserPane { state: None, window: &mut *window },
                &self.terminal,
                right_open,
                width,
                self.facts.agent(),
                self.base_menu_open(cx),
                cx,
            );
            // The one body is the pane's landmark, named like a strip
            // screen is ("Right pane: Changes").
            return gpui::div()
                .id("right-browse-body")
                .size_full()
                .role(gpui::Role::Complementary)
                .aria_label(format!("Right pane: {}", self.title(cx)))
                .child(body)
                .into_any_element();
        }
        let index = right::screen_index(kind);
        let pos = aui::workbench::strip_position("right-strip", index, window, cx);
        self.strip_moving = aui::workbench::strip_moving(pos, index);
        let visible = aui::workbench::strip_visible(pos, RightKind::ALL.len());
        // docs/82 8.1: the lower pane's kind, drawn under the strip and
        // never on it — a slide passing its slot shows the ground there, so
        // no screen is built twice in one frame.
        let lower = layout::Layout::get(cx).shown_lower();
        if visible(right::screen_index(RightKind::Tasks)) || lower == Some(RightKind::Tasks) {
            self.sync_tasks(cx);
        }
        if visible(right::screen_index(RightKind::Browser)) && self.cache.page_address.is_none() {
            self.ensure_page_address(window, cx);
        }
        let page = self.facts.page.clone();
        let page_menu = self.page_menu_open(cx);
        let base_menu = self.base_menu_open(cx);
        let mut screens: Vec<Option<AnyElement>> = Vec::with_capacity(RightKind::ALL.len());
        for (i, screen_kind) in RightKind::ALL.into_iter().enumerate() {
            if !visible(i) || lower == Some(screen_kind) {
                screens.push(None);
                continue;
            }
            let page = if screen_kind == RightKind::Browser { page.clone() } else { None };
            screens.push(Some(self.screen(screen_kind, page.as_ref(), right_open, page_menu, base_menu, window, cx)));
        }
        let swipe = cx.weak_entity();
        let strip = aui::workbench::pane_strip("right-strip", pos, px(width), screens)
            .on_swipe(move |dir, _, cx| {
                let _ = swipe.update(cx, |this, cx| this.swipe_screen(dir, cx));
            })
            .into_any_element();
        match lower {
            Some(lower) => {
                let below = self.screen(lower, None, right_open, page_menu, base_menu, window, cx);
                self.stacked(strip, lower, below, cx)
            }
            None => strip,
        }
    }
}

impl RightPane {
    /// One screen of the pane, on the strip or in the lower pane: the
    /// shared builder ([`right::render_screen`]) with this frame's facts.
    #[allow(clippy::too_many_arguments)]
    fn screen(
        &mut self,
        kind: RightKind,
        page: Option<&Entity<aui_webview::WebviewState>>,
        right_open: bool,
        page_menu: bool,
        base_menu: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let edits = if kind == RightKind::Changes { self.session_edits(cx) } else { Vec::new() };
        right::render_screen(
            kind,
            &self.cache,
            self.facts.project.clone(),
            &edits,
            right::BrowserPane { state: page, window },
            &self.terminal,
            right_open,
            self.resize.width,
            self.facts.agent(),
            &self.focus,
            page_menu,
            base_menu,
            cx,
        )
    }

    /// The strip over the lower pane (docs/82 8.1, docs/83 §6): the top
    /// pane takes its share of the height ([`layout::Layout::split`]), a 1
    /// px `line-region` divides them with a grab strip over it, and the
    /// lower pane carries a 36 px sub-header naming its view with a ✕.
    fn stacked(&mut self, strip: AnyElement, kind: RightKind, below: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let share = layout::Layout::get(cx).split();
        let close = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(RightPaneEvent::CloseLower));
        let head = aui::workbench::pushed_row("right-lower-head", kind.label())
            .action(aui::workbench::pane_action("right-lower-close", aui_icons::IconName::X, "Close lower pane").on_click(close));
        let grab = cx.listener(|this: &mut Self, event: &gpui::MouseDownEvent, _, cx| {
            let share = layout::Layout::get(cx).split();
            this.lower.drag = Some((f32::from(event.position.y), share));
            // The share is pinned where it stands as the drag arms: a layout
            // change, so the pane's layout observer redraws it with the
            // drag's listeners in place.
            layout::Layout::stage(cx, |layout| layout.right_split = Some(share));
            cx.stop_propagation();
        });
        let handle = gpui::div()
            .id("right-lower-resize")
            .role(gpui::Role::Splitter)
            .aria_label("Resize lower pane")
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .top(px(-aui::shell::RESIZE_HANDLE_W / 2.0))
            .h(px(aui::shell::RESIZE_HANDLE_W))
            .cursor(gpui::CursorStyle::ResizeUpDown)
            .on_mouse_down(gpui::MouseButton::Left, grab);
        let lower = gpui_kit::base::v_flex()
            .id("right-lower")
            .relative()
            .flex_1()
            .min_h(px(0.0))
            .w_full()
            .border_t_1()
            .border_color(p.line_region)
            .child(head)
            .child(gpui::div().flex_1().min_h(px(0.0)).child(below))
            .child(handle);
        let bounds = self.lower.bounds.clone();
        let measure = gpui::canvas(move |at, _, _| bounds.set(Some(at)), |_, _, _, _| {}).absolute().size_full();
        // Mid-drag the window's own move and release listeners carry it, so
        // the pointer may leave the 6 px strip (or the pane) and the
        // divider keeps following; the release settles it.
        let follow = self.lower.drag.is_some().then(|| {
            let (moves, ends) = (cx.weak_entity(), cx.weak_entity());
            gpui::canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let moves = moves.clone();
                    window.on_mouse_event(move |event: &gpui::MouseMoveEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            let _ = moves.update(cx, |this, cx| this.drag_lower_to(f32::from(event.position.y), cx));
                        }
                    });
                    let ends = ends.clone();
                    window.on_mouse_event(move |_: &gpui::MouseUpEvent, phase, _, cx| {
                        if phase == gpui::DispatchPhase::Bubble {
                            let _ = ends.update(cx, |this, cx| this.end_lower_drag(cx));
                        }
                    });
                },
            )
            .absolute()
            .size_full()
        });
        gpui_kit::base::v_flex()
            .relative()
            .size_full()
            .child(measure)
            .child(gpui::div().w_full().h(gpui::relative(share)).flex_none().overflow_hidden().child(strip))
            .child(lower)
            .children(follow)
            .into_any_element()
    }

    /// One move of the divider drag to window y `at`: the share follows,
    /// live (staged, not written).
    fn drag_lower_to(&mut self, at: f32, cx: &mut Context<Self>) {
        let Some((grab, start)) = self.lower.drag else { return };
        let height = self.lower.bounds.get().map_or(0.0, |bounds| f32::from(bounds.size.height));
        let share = lower_drag_share(start, grab, at, height);
        layout::Layout::stage(cx, |layout| layout.right_split = Some(share));
    }

    /// The release: the share is written, and the session's record takes it.
    fn end_lower_drag(&mut self, cx: &mut Context<Self>) {
        if self.lower.drag.take().is_none() {
            return;
        }
        // Written: the layout observer redraws the pane without the drag.
        let share = layout::Layout::get(cx).split();
        layout::Layout::update(cx, |layout| layout.right_split = Some(share));
        cx.emit(RightPaneEvent::Persist);
    }
}

// ── the root's side ─────────────────────────────────────────────────────

impl Harness {
    /// What the pane needs from the root now (see [`RightFacts`]); the page
    /// is the existing one — the render builds it where the window is.
    pub(crate) fn right_facts(&self, cx: &App) -> RightFacts {
        let session = self.active.clone();
        let provider = session.as_ref().and_then(|view| self.session_lane_provider(&view.read(cx).rt(cx).session_id, cx));
        RightFacts {
            project: self.right_project(cx),
            session,
            provider,
            pending_neutral: self.pending_right_neutral(cx),
            settings_open: self.settings_open(cx),
            page: self.existing_browser(cx),
            live: matches!(self.auth, crate::login::Auth::SignedIn(_)) || !(self.args.offline || self.args.replay.is_some()),
        }
    }

    /// Run `f` on the pane with fresh facts: every root call into the pane
    /// goes through here, so the pane never acts on a stale project or
    /// session.
    pub(crate) fn with_right_pane<R>(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut RightPane, &mut Context<RightPane>) -> R,
    ) -> R {
        let facts = self.right_facts(cx);
        self.right_pane.update(cx, |pane, cx| {
            pane.facts = facts;
            f(pane, cx)
        })
    }

    /// Re-read the pane's data now (see [`RightPane::refresh_now`]).
    pub(crate) fn refresh_right_now(&mut self, cx: &mut Context<Self>) {
        self.with_right_pane(cx, |pane, cx| pane.refresh_now(cx));
    }

    /// The divider's width now.
    pub(crate) fn right_width(&self, cx: &App) -> f32 {
        self.right_pane.read(cx).width()
    }

    /// The pane for this frame (strip mode builds the session's page here,
    /// where the window is at hand): push the facts, then compose the view.
    /// Z7a2: the Page draws the active session's live webview, created
    /// lazily here — gated on the pane standing open on the Page, so
    /// activation, boot and every other kind never create a webview as a
    /// render side effect. While closing (or while Settings hides the pane)
    /// the existing webview passes through, so the last page snapshot slides
    /// out instead of the "Opening the page" placeholder (B7).
    pub(crate) fn compose_right_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut facts = self.right_facts(cx);
        if Self::page_strip_open_for(&facts, cx) {
            facts.page = Some(self.ensure_browser_person(window, cx));
        }
        self.right_pane.update(cx, |pane, _| pane.facts = facts);
        // The Terminal view draws the dock's body: its facts go in too
        // (the centre composes the dock as well, but not in every mode).
        let dock = self.dock_facts(cx);
        self.terminal_dock.update(cx, |terminal, _| terminal.facts = dock);
        self.right_pane.clone().into_any_element()
    }

    /// Whether the pane stands open on the session's Page in strip mode (not
    /// Browse, no neutral switch pending, not hidden by Settings): where the
    /// pane draws the live webview.
    pub(crate) fn page_strip_open(&self, cx: &App) -> bool {
        Self::page_strip_open_for(&self.right_facts(cx), cx)
    }

    fn page_strip_open_for(facts: &crate::right_pane::RightFacts, cx: &App) -> bool {
        let layout = layout::Layout::get(cx);
        let strip_mode = layout.mode != layout::Mode::Browse && !facts.pending_neutral;
        let open_on_page = layout.right_open && !facts.settings_open && layout::right_kind(layout) == RightKind::Browser;
        strip_mode && open_on_page
    }

    /// One event from the pane, mapped to the root's own method.
    pub(crate) fn on_right_pane_event(
        &mut self,
        _pane: &Entity<RightPane>,
        event: &RightPaneEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            RightPaneEvent::Persist => self.save_right_for_active(cx),
            RightPaneEvent::Select(kind) => self.select_right(*kind, cx),
            RightPaneEvent::Toggle => self.toggle_right(cx),
            RightPaneEvent::OpenUrl(url) => self.open_url_in_browser(url.clone(), cx),
            RightPaneEvent::Reveal { root, abs } => self.reveal_link_path(root, abs, Some(false), None, cx),
            RightPaneEvent::RevertFile { root, path, turn } => {
                if let Err(why) = self.revert_file_from_changes(root, path, turn.clone(), cx) {
                    crate::baaz_log!("undo: revert not asked ({why})");
                }
            }
            RightPaneEvent::Quote { text, lang, lines, location } => {
                if let Some(view) = self.active.clone() {
                    // The session owns its quotes and redraws itself.
                    view.update(cx, |view, cx| view.add_quote(text.clone(), None, Some((lang.clone(), *lines)), location.clone(), window, cx));
                }
            }
            RightPaneEvent::CommitDraftWanted { root, files } => self.draft_commit_for_pane(root, files, cx),
            RightPaneEvent::OpenPrForm(root) => self.open_pr_form(root, window, cx),
            RightPaneEvent::DraftOnSide { root, kind, prompt } => self.draft_on_muse(root, *kind, prompt.clone(), cx),
            RightPaneEvent::ViewPullRequest(url) => self.view_pull_request(url.clone(), window, cx),
            RightPaneEvent::PullRequestRead { root, branch, pr } => self.pull_request_read(root, branch, pr.clone(), window, cx),
            RightPaneEvent::ArchiveSession => self.archive_open_session(window, cx),
            RightPaneEvent::OpenViewsMenu => self.open_menu(crate::overlays::MenuKind::RightViews, cx),
            RightPaneEvent::OpenTerminalTab(tab) => {
                if !self.settings_open(cx) {
                    let tab = tab.clone();
                    self.show_terminal(cx);
                    self.with_terminal_dock(cx, |dock, cx| dock.open_tab(Some(tab), window, cx));
                }
            }
            RightPaneEvent::CloseLower => {
                let _ = self.set_right_lower(None, window, cx);
            }
            RightPaneEvent::BrowserIntent(intent) => self.handle_browser_intent(intent.clone(), window, cx),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::lower_drag_share;

    /// docs/82 8.1: the divider follows the pointer and holds to 0.25–0.75.
    #[test]
    fn the_lower_divider_follows_and_clamps() {
        assert_eq!(lower_drag_share(0.5, 400.0, 480.0, 800.0), 0.6);
        assert_eq!(lower_drag_share(0.5, 400.0, 0.0, 800.0), crate::layout::SPLIT_MIN);
        assert_eq!(lower_drag_share(0.5, 400.0, 2000.0, 800.0), crate::layout::SPLIT_MAX);
        assert_eq!(lower_drag_share(0.9, 0.0, 0.0, 0.0), crate::layout::SPLIT_MAX, "no measured height: the start, held");
    }
}
