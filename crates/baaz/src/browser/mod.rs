//! The browser pane's engine (Z7a, tabs B2a-H1): one webview per tab.
//!
//! [`WebviewState`](aui_webview::WebviewState) holds a
//! [`WebBackend`](aui_webview::WebBackend) and what the pane knows about it.
//! This module owns the tab registry — one live state per open tab, created
//! lazily the first time the Browser kind shows that tab. The tab model
//! itself ([`tabs`]) is pure: tabs belong to a space (a project) and to an
//! owner (the person, or one session). Switching tabs hides the old tab's
//! webview; a tab hidden long enough is discarded (its state dropped, its
//! URL kept) and rebuilt on show.
//!
//! `browser_key` lookups mean "the active tab of the active session (or the
//! person's active tab in the current project when no session is open)".
//!
//! Backend: [`WryBackend`](aui_webview::WryBackend) (a WKWebView child of the
//! gpui window) in the normal app, [`FakeWebBackend`](aui_webview::FakeWebBackend)
//! when running a `--screenshot` capture or tests — a native view never
//! appears in a gpui screenshot, and captures must be deterministic. The
//! choice rides on [`Harness::browser_fake`](crate::app::Harness), an explicit
//! flag set at boot, never on environment sniffed in render.
//!
//! # The native-overlay rule
//!
//! The native view is composited ABOVE the gpui scene, so anything gpui draws
//! over the page is invisible. The host therefore calls
//! [`set_obscured`](aui_webview::WebviewState::set_obscured) with `true`
//! while any overlay covers the pane. [`browser_visible`] is the one pure
//! predicate behind that: the native view is visible only when the pane is
//! open on the Browser kind for the active session with nothing over it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use aui::workbench::Annotation;
use aui_tokens::ActiveAui;
use aui_webview::{DownloadState, FakeWebBackend, PermissionDecision, WebBackend, WebviewIntent, WebviewState, WryBackend};
use gpui::{AppContext as _, Bounds, Context, Entity, Pixels, Point, Window};
use gpui_kit::base::input::{InputEvent, InputState};

use crate::app::Harness;
use crate::layout::RightKind;
use crate::overlays::MenuKind;

pub(crate) mod chrome;
pub(crate) mod tabs;
pub(crate) mod toolbar;

pub(crate) use tabs::{Owner, StripTab, TabId, Tabs};
pub(crate) use toolbar::{BackForward, OmniRow, OmniTarget, SuggestKind};

/// The fake backend's URL key for the no-session/home webview.
const HOME_KEY: &str = "home";

/// The pending-screenshot key for the tab `id`: the Menu Screenshot
/// capture keys to the shown tab, never the session key.
fn screenshot_tab_key(id: TabId) -> String {
    format!("browse-tab:{}", id.0)
}

/// The tab behind a [`screenshot_tab_key`], or `None` for older
/// session-keyed entries, which drain through the legacy path.
fn screenshot_tab_id(key: &str) -> Option<TabId> {
    key.strip_prefix("browse-tab:")?.parse::<u64>().ok().map(TabId)
}
/// What a webview opens on when its session never navigated anywhere.
const BLANK: &str = "about:blank";

/// Everything that decides whether one session's native view may show.
///
/// All fields are positive facts about the frame; [`browser_visible`] ANDs
/// the good ones and NANDs the covering ones. One struct so the call site in
/// `render` names every input and the truth table below covers every input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BrowserVisibility {
    /// The right pane stands open.
    pub pane_open: bool,
    /// The pane shows the Browser kind.
    pub kind_browser: bool,
    /// The frame shows Browse mode's centre page instead of the pane's:
    /// the current project's person-tab page, which needs no open pane
    /// and no Browser kind. Sessions mode always leaves this false, so
    /// its visibility reads exactly as before.
    pub centre_browse: bool,
    /// This webview's session is the active one.
    pub session_active: bool,
    /// The Settings page stands open.
    pub settings_open: bool,
    /// The ⌘K palette (any list) stands open.
    pub palette_open: bool,
    /// A menu stands open: composer/chip pickers, caret popovers, overflow,
    /// view options, project menus.
    pub menu_open: bool,
    /// The composer's view-local `+` menu stands open. Plain view state,
    /// never the shared overlay stack, so it gets its own input.
    pub plus_open: bool,
    /// The account menu stands open.
    pub account_menu_open: bool,
    /// A modal dialog stands open.
    pub dialog_open: bool,
    /// The file-drop overlay covers the window.
    pub drop_cover: bool,
    /// The terminal dock covers the browser pane: open AND its laid-out
    /// bounds intersect the pane's. The dock lives in the centre column
    /// and the pane on the right, so with today's layout they never
    /// intersect — an open dock leaves the page live. The input stays so
    /// a future full-width dock still hides the page.
    pub terminal_covering: bool,
    /// The window holds the keyboard focus (its last frame landed).
    pub window_active: bool,
    /// The Browse omnibox suggestion list stands open. It drops down over
    /// the page, so while it is open the page must be obscured (it shows
    /// its screenshot stand-in, as with the palette).
    pub omnibox_open: bool,
    /// A Browse chrome popover (downloads, extensions, ⋯) stands open. It
    /// drops over the page, so while one is open the page must be
    /// obscured, like the omnibox above.
    pub chrome_popover_open: bool,
    /// The Baaz-drawn crashed page covers the page rect. The library
    /// reports no load-error event — only `Crashed` — so the error page
    /// renders from that state alone, and the native view hides under it.
    pub page_error_open: bool,
}

/// Whether `inputs`' native view may show: the pane open on the Browser
/// kind for the active session — or, in Browse, the centre page — with
/// no overlay over it and a live window. The covering rules are the
/// same for both: the palette, menus, dialogs and settings obscure the
/// centre page exactly as they obscure the pane's.
pub(crate) fn browser_visible(inputs: BrowserVisibility) -> bool {
    (inputs.pane_open && inputs.kind_browser || inputs.centre_browse)
        && inputs.session_active
        && !inputs.settings_open
        && !inputs.palette_open
        && !inputs.menu_open
        && !inputs.plus_open
        && !inputs.account_menu_open
        && !inputs.dialog_open
        && !inputs.drop_cover
        && !inputs.terminal_covering
        && !inputs.omnibox_open
        && !inputs.chrome_popover_open
        && !inputs.page_error_open
        && inputs.window_active
}

/// The shell header's height in pixels: the columns — and the browser
/// pane — hang under it. The centre column's height is the window minus
/// this same 44 px.
const HEADER_PX: f32 = 44.0;

/// The webview's nav row (back, forward, reload, address), which aui-webview
/// draws at a 38 px minimum above the native page.
const BROWSER_NAV_PX: f32 = 38.0;

/// The window's content box in window coordinates. `Window::content_mask`
/// is only valid while painting (a debug assertion aborts a debug build when
/// it is read from a mouse handler); the viewport is valid at any time and,
/// outside paint, is exactly what the mask falls back to.
fn window_content_bounds(window: &Window) -> Bounds<Pixels> {
    Bounds {
        origin: Point::default(),
        size: window.viewport_size(),
    }
}

/// The browser pane's rectangle in window coordinates, derived from the
/// window's content box: the pane hangs off the content's right edge,
/// under the header, `right_width` wide. Pure so tests can drive it.
pub(crate) fn browser_page_bounds(content: Bounds<Pixels>, right_width: Pixels) -> Bounds<Pixels> {
    Bounds {
        origin: Point {
            x: content.origin.x + content.size.width - right_width,
            y: content.origin.y + gpui::px(HEADER_PX),
        },
        size: gpui::size(right_width, content.size.height - gpui::px(HEADER_PX)),
    }
}

/// The Browse centre page's rectangle in window coordinates (B2a-H2):
/// the content box under the header, between the sidebar (`0` when
/// collapsed) and the right pane (`0` when closed), above the terminal
/// dock (`0` when closed). The dock stacks below the centre in the same
/// column — beside it, never over it — so its height subtracts from
/// both placement and the mouse-down hand-back, and it never covers
/// the page. Pure so tests can drive it.
pub(crate) fn browser_centre_bounds(
    content: Bounds<Pixels>,
    sidebar: Pixels,
    right: Pixels,
    dock: Pixels,
) -> Bounds<Pixels> {
    Bounds {
        origin: Point {
            x: content.origin.x + sidebar,
            y: content.origin.y + gpui::px(HEADER_PX),
        },
        size: gpui::size(
            content.size.width - sidebar - right,
            content.size.height - gpui::px(HEADER_PX) - dock,
        ),
    }
}

/// The terminal dock's rectangle in window coordinates: the centre
/// column's `[centre_left, centre_right)` run at the content box's
/// bottom, `height` tall. Pure so tests can drive it.
pub(crate) fn terminal_dock_bounds(
    content: Bounds<Pixels>,
    centre_left: Pixels,
    centre_right: Pixels,
    height: Pixels,
) -> Bounds<Pixels> {
    Bounds {
        origin: Point {
            x: centre_left,
            y: content.origin.y + content.size.height - height,
        },
        size: gpui::size(centre_right - centre_left, height),
    }
}

/// Whether the terminal dock covers the browser pane: only when the dock
/// stands open AND its laid-out bounds intersect the pane's. The dock
/// lives in the centre column and the pane on the right, so with today's
/// layout the two never intersect and an open dock leaves the page live;
/// the predicate keeps the input so a future full-width dock still hides
/// the page. A missing rect (pane closed, dock unmounted) is no cover.
/// Pure so the truth table below covers it.
pub(crate) fn terminal_covers_browser(
    terminal_open: bool,
    dock: Option<Bounds<Pixels>>,
    pane: Option<Bounds<Pixels>>,
) -> bool {
    terminal_open
        && dock
            .zip(pane)
            .is_some_and(|(dock, pane)| dock.intersects(&pane))
}

/// Whether the frame shows the live browser: the right pane stands open
/// on the Browser kind. Gates both the ⌘L route and the outside-click
/// keyboard release below.
pub(crate) fn browser_pane_showing(pane_open: bool, kind_browser: bool) -> bool {
    pane_open && kind_browser
}

/// Whether `key`'s native view may show as its session's page: only the
/// active session's — and never while a provider switch is pending (Q4c).
/// The centre is then the destination's loading state, so the previous
/// session's page must neither draw nor hit-test under it.
pub(crate) fn browser_session_active(key: &str, active_key: &str, pending_switch: bool) -> bool {
    !pending_switch && key == active_key
}

/// Whether a window-coordinate mouse-down outside the browser page's rect
/// must hand the keyboard back: the page holds it and the click landed
/// outside. Pure; the capture handler resolves the rect and the webview,
/// and tests drive this against the fake backend.
pub(crate) fn should_release_keyboard_on_mouse_down(
    holds_keyboard: bool,
    position: Point<Pixels>,
    page: Option<Bounds<Pixels>>,
) -> bool {
    holds_keyboard && page.is_some_and(|page| !page.contains(&position))
}

/// A session's last-activity time as ms since the epoch, for omnibox
/// recency: `0` when the clock reads before it (never in practice).
fn recency_ms(updated: &chrono::DateTime<chrono::Local>) -> u64 {
    updated.timestamp_millis().max(0) as u64
}

/// The draft text a `SendAnnotations` intent appends: the URL plus one line
/// per pin (`<index>. <element path> — <note>`). Never auto-sent; the person
/// reviews the draft first.
pub(crate) fn annotations_draft_block(annotations: &[Annotation], url: &str) -> String {
    let mut out = format!("Browser annotations — {url}");
    for annotation in annotations {
        out.push_str(&format!(
            "\n{}. {} — {}",
            annotation.index, annotation.selector, annotation.note
        ));
    }
    out
}

/// Whether the window counts as active for the shown page: the OS window
/// is active, or the currently SHOWN page holds the keyboard. Only the
/// shown tab's page ever counts — a hidden or obscured page holding the
/// keyboard (a click into a background Chromium page makes it key while
/// the gpui window goes inactive) must never keep the window "active",
/// or its native view would show under an overlay. Pure over a snapshot
/// of who holds the keyboard, so tests drive it without a webview.
pub(crate) fn shown_window_active(
    window_active: bool,
    shown: Option<TabId>,
    holds_keyboard: &HashMap<TabId, bool>,
) -> bool {
    window_active || shown.is_some_and(|id| holds_keyboard.get(&id).copied().unwrap_or(false))
}

/// Whether a middle release on `id` closes it: the press started on this
/// same tab — never press-less, never after a press that began elsewhere.
/// Pure so tests drive the pairing without a window; the row's release
/// handler clears the recorded press either way.
pub(crate) fn middle_release_closes(pressed: Option<TabId>, released: TabId) -> bool {
    pressed == Some(released)
}

/// Whether `current` is a real navigation worth persisting: non-empty, not
/// the blank default, and different from what is already stored. The poll
/// drains many times a second; only a change writes `sessions.json`. Merely
/// showing the pane must leave the stored state untouched — `about:blank`
/// is where a fresh webview starts, not somewhere the person went.
pub(crate) fn should_remember(current: &str, stored: Option<&str>) -> bool {
    !current.is_empty() && current != BLANK && stored != Some(current)
}

/// The URL a fresh webview for a session opens on: its last URL, else blank.
pub(crate) fn initial_url(stored: Option<&str>) -> &str {
    stored.unwrap_or(BLANK)
}

/// How many page screenshots one run keeps: enough to re-read the last
/// few captures, bounded so a long session never fills the disk.
/// The Chromium page engine for a new pane page, when the bundle carries CEF
/// and `BAAZ_BROWSER_ENGINE=webkit` does not ask for WebKit. `None` means
/// "use WKWebView": CEF unavailable, or it failed to open a page (logged).
#[cfg(feature = "cef")]
fn cef_backend(window: &Window, url: &str, rgba: (u8, u8, u8, u8)) -> Option<Box<dyn WebBackend>> {
    if !aui_cef::available() || std::env::var("BAAZ_BROWSER_ENGINE").as_deref() == Ok("webkit") {
        return None;
    }
    match aui_cef::CefBackend::new_at(window, url, (0.0, 0.0), (0.0, 0.0), rgba) {
        Ok(backend) => Some(Box::new(backend)),
        Err(error) => {
            crate::baaz_log!("browser: no Chromium page ({error}); falling back to WKWebView");
            None
        }
    }
}

#[cfg(not(feature = "cef"))]
fn cef_backend(
    _window: &Window,
    _url: &str,
    _rgba: (u8, u8, u8, u8),
) -> Option<Box<dyn WebBackend>> {
    None
}

pub(crate) const MAX_SCREENSHOTS: usize = 20;

/// This run's page screenshots live here: `$BAAZ_STATE_DIR/attachments/browser/`
/// (else the system temp dir), away from the composer's own attachments.
fn screenshot_dir() -> Option<PathBuf> {
    let base = std::env::var_os("BAAZ_STATE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Some(base.join("attachments").join("browser"))
}

/// Writes PNG `bytes` into [this run's screenshot dir](screenshot_dir) and
/// answers with the path, pruning back to [`MAX_SCREENSHOTS`]. The composer
/// chip carries the bytes; this file is the durable copy. Failures are
/// silent: the draft attach is primary.
fn write_screenshot_temp(bytes: &[u8]) -> Option<PathBuf> {
    let dir = screenshot_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("browser-{}-{nanos}.png", std::process::id()));
    std::fs::write(&path, bytes).ok()?;
    prune_screenshot_dir(&dir, MAX_SCREENSHOTS);
    Some(path)
}

/// Drop every `browser-*.png` in `dir` but the newest `keep`, oldest first
/// by mtime (file name breaks ties). Pure over the directory, so tests can
/// drive it; failures are silent, like the write it follows.
pub(crate) fn prune_screenshot_dir(dir: &std::path::Path, keep: usize) {
    let mut shots: Vec<(std::time::SystemTime, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_str()?;
            if !name.starts_with("browser-") || !name.ends_with(".png") {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()?
                .modified()
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            Some((modified, entry.path()))
        })
        .collect();
    shots.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    if shots.len() > keep {
        for (_, path) in shots.drain(..shots.len() - keep) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Delete this run's page screenshots (quit): captures are per-run working
/// files, and the prune above only bounds the live run. Silent when there
/// is nothing to delete.
pub(crate) fn cleanup_screenshots() {
    if let Some(dir) = screenshot_dir() {
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// One live permission ask as the router reads it: id, origin, kinds.
type LiveAsk = (u64, String, Vec<aui_webview::PermissionKind>);

impl Harness {
    /// The registry key whose webview the pane shows: the active session's
    /// id, or the home key with no session open.
    pub(crate) fn browser_key(&self, cx: &gpui::App) -> String {
        self.active
            .as_ref()
            .map(|view| view.read(cx).session_id.clone())
            .unwrap_or_else(|| HOME_KEY.to_owned())
    }

    /// Which owner `key` names: a session id names that session's tabs,
    /// [`HOME_KEY`] (no session open) the person's tabs.
    pub(crate) fn tab_owner_for_key(&self, key: &str) -> Owner {
        if key == HOME_KEY {
            Owner::Person
        } else {
            Owner::Session(key.to_owned())
        }
    }

    /// Whose tabs the pane shows: the active session's, or the person's
    /// with no session open.
    pub(crate) fn shown_tab_owner(&self, cx: &gpui::App) -> Owner {
        self.tab_owner_for_key(&self.browser_key(cx))
    }

    /// Load the tab model on first use and migrate older restores: every
    /// session with a stored `browser_url` and no tabs yet gets one
    /// session tab with that URL. Persists when either step changed the
    /// model.
    pub(crate) fn ensure_tabs_loaded(&mut self, cx: &mut Context<Self>) {
        if self.browser.tabs_loaded {
            return;
        }
        self.browser.tabs_loaded = true;
        self.browser.tabs = tabs::load();
        if self.browser.tabs.migrate_from_overrides(&self.overrides) {
            self.schedule_tabs_write(cx);
        }
    }

    /// The space an owner's active tab lives in: the current project for
    /// the person's per-project tabs, nothing for a session (a session
    /// already belongs to one project).
    fn owner_space(&self, owner: &Owner) -> Option<String> {
        match owner {
            Owner::Person => self.current_project_id(),
            Owner::Session(_) => None,
        }
    }

    /// The owner's active tab, opening its first when it has none: a
    /// session restores its last URL, everyone starts blank. The strip
    /// shows only when the owner has at least one tab.
    fn ensure_owner_tab(&mut self, owner: &Owner, cx: &mut Context<Self>) -> TabId {
        let space = self.owner_space(owner);
        if let Some(id) = self.browser.tabs.active_of(owner, space.as_deref()) {
            return id;
        }
        let stored = match owner {
            Owner::Session(session) => self
                .overrides
                .get(session)
                .and_then(|meta| meta.right.clone())
                .and_then(|right| right.browser_url),
            Owner::Person => None,
        };
        let space = self.current_project_id();
        let id = self
            .browser
            .tabs
            .open(space, owner.clone(), initial_url(stored.as_deref()), None);
        self.schedule_tabs_write(cx);
        id
    }

    /// Point the agent `browser_*` tools at `state`: the session key's
    /// active tab's page. Called on every resolve and every tab switch, so
    /// registration always follows the active tab.
    fn register_tab_page(&mut self, key: &str, id: TabId, state: &Entity<WebviewState>) {
        self.terminal_service.register_browser(key, state.clone());
        self.browser.agent_tabs.insert(key.to_owned(), id);
    }

    /// Arm the debounced `tabs.json` write: the first change in a dirty
    /// spell arms one 750 ms window, and the window serialises the CURRENT
    /// tabs when it fires — a switch inside the window lands on disk
    /// instead of being overwritten by stale state. Changes inside the
    /// window only mark dirty (no re-arm), which is the max-delay cap: a
    /// constantly-changing title lands on disk at most 750 ms after the
    /// spell starts instead of postponing the write forever. The write
    /// itself runs off the UI thread, like the sessions write. Each armed
    /// snapshot carries a monotonic generation, and the writer skips a
    /// snapshot older than the last written — two saves finishing out of
    /// order can never let the older state win.
    pub(crate) fn schedule_tabs_write(&mut self, cx: &mut Context<Self>) {
        if self.browser.tabs_save_pending {
            return;
        }
        self.browser.tabs_save_pending = true;
        self.browser.tabs_save_epoch += 1;
        let epoch = self.browser.tabs_save_epoch;
        let generation = tabs::next_tabs_generation();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(750))
                .await;
            let snapshot = this
                .update(cx, |this, _| {
                    if this.browser.tabs_save_epoch != epoch {
                        return None;
                    }
                    this.browser.tabs_save_pending = false;
                    Some(this.browser.tabs.clone())
                })
                .unwrap_or(None);
            let Some(snapshot) = snapshot else {
                return;
            };
            cx.background_executor()
                .spawn(async move {
                    tabs::write_tabs_snapshot_generation(generation, &snapshot);
                })
                .await;
        })
        .detach();
    }

    /// Land the tabs durably, synchronously: the quit and window-close
    /// durability point, next to the layout/sessions flushes. Writes the
    /// CURRENT model under a generation newer than any armed snapshot —
    /// a save still in flight can never land after this and win — then
    /// cancels the armed debounce, whose snapshot is stale by definition.
    /// A no-op when the tabs never loaded, so a quit that never touched
    /// the browser still writes nothing; otherwise the in-memory-only
    /// `last_shown` stamps land here at the latest.
    pub(crate) fn flush_tabs_save(&mut self) {
        self.browser.tabs_save_epoch += 1;
        self.browser.tabs_save_pending = false;
        if !self.browser.tabs_loaded {
            return;
        }
        let generation = tabs::next_tabs_generation();
        tabs::write_tabs_snapshot_generation(generation, &self.browser.tabs);
    }

    /// The webview for `key`, creating it on first use. `key` names an
    /// owner (a session id, or [`HOME_KEY`] for the person), and the page
    /// is that owner's active tab's — opening the tab first when the owner
    /// has none. A new page opens on the tab's URL; only an explicit
    /// person open (`focus_fresh`) takes the keyboard into a fresh blank
    /// URL field, so ⌘L is already where the person looks. Activation,
    /// restore and `browse:` steps pass `false`: the composer keeps focus.
    fn browser_for(
        &mut self,
        key: &str,
        focus_fresh: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        self.ensure_tabs_loaded(cx);
        let owner = self.tab_owner_for_key(key);
        let id = self.ensure_owner_tab(&owner, cx);
        if let Some(state) = self.browser.states.get(&id).cloned() {
            self.register_tab_page(key, id, &state);
            return state;
        }
        let url = self
            .browser
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        let fresh_blank = initial == BLANK;
        // Explicit boot flag, never environment sniffed in render: captures
        // and tests run the scripted page, the app runs WKWebView.
        self.browser_builds += 1;
        let fake = self.browser.fake || cfg!(test);
        let state = cx.new(|cx| {
            if fake {
                let mut backend = FakeWebBackend::new();
                if backend.url().as_ref() != initial {
                    backend.navigate(initial);
                }
                WebviewState::new(Box::new(backend), cx)
            } else {
                // WebKit paints white before and beneath a page; paint the
                // pane's own surface instead, so about:blank is not a white slab.
                let bg = gpui::Rgba::from(cx.aui().colors.surface_1);
                let rgba =
                    [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(
                    &*window,
                    initial,
                    (0.0, 0.0),
                    (0.0, 0.0),
                    rgba,
                ) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!(
                            "browser: no WKWebView ({error}); falling back to the scripted page"
                        );
                        let mut backend = FakeWebBackend::new();
                        if backend.url().as_ref() != initial {
                            backend.navigate(initial);
                        }
                        WebviewState::new(Box::new(backend), cx)
                    }
                }
            }
        });
        if focus_fresh && fresh_blank {
            let focus = state.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        self.browser.states.insert(id, state.clone());
        // The agent's `browser_*` tools act on this same state through the
        // socket service, keyed by session id: always the session's active
        // tab's page.
        self.register_tab_page(key, id, &state);
        state
    }

    /// The live webview the pane shows, when one already exists — the
    /// shown owner's active tab's page — never creating. The closing frame
    /// uses this: the page's last snapshot stays on screen while the column
    /// slides out instead of the "Opening the page" placeholder (B7).
    pub(crate) fn existing_browser(&self, cx: &gpui::App) -> Option<Entity<WebviewState>> {
        if !self.browser.tabs_loaded {
            return None;
        }
        let owner = self.shown_tab_owner(cx);
        let space = self.owner_space(&owner);
        self.browser
            .tabs
            .active_of(&owner, space.as_deref())
            .and_then(|id| self.browser.states.get(&id).cloned())
    }

    /// The session's live page: its active tab's state, when built. The
    /// existing pane tests reach the session's page through this instead
    /// of the tab-keyed registry.
    #[cfg(test)]
    pub(crate) fn session_page(&self, session_id: &str) -> Option<Entity<WebviewState>> {
        let owner = Owner::Session(session_id.to_owned());
        self.browser
            .tabs
            .active_of(&owner, None)
            .and_then(|id| self.browser.states.get(&id).cloned())
    }

    /// Whether this session key has a live page: its active tab's state
    /// exists. The existing pane tests pin boot and switch on this.
    #[cfg(test)]
    pub(crate) fn session_has_page(&self, session_id: &str) -> bool {
        self.session_page(session_id).is_some()
    }

    /// The Browse centre's shown tab, when Browse shows one (B2a-H2): the
    /// carried session tab while one is parked (B2a-H4), else the current
    /// project's person-tab page. `None` in Sessions mode, before the tabs
    /// load, or with no tab yet.
    pub(crate) fn browse_centre_shown(&self) -> Option<TabId> {
        if self.mode != crate::layout::Mode::Browse || !self.browser.tabs_loaded {
            return None;
        }
        if let Some(id) = self.browser.centre_override {
            if self.browser.tabs.get(id).is_some() {
                return Some(id);
            }
        }
        // A From-sessions show (B2a-H3): a session-owned tab the sidebar
        // asked the centre to hold — ownership never moves, and any
        // person-tab select clears it back to the project's own tab.
        if let Some(id) = self.browse_session_tab {
            if self.browser.tabs.get(id).is_some_and(|tab| matches!(tab.owner, Owner::Session(_))) {
                return Some(id);
            }
        }
        let space = self.current_project_id();
        self.browser.tabs.active_of(&Owner::Person, space.as_deref())
    }

    /// The Browse centre's live page, when one already exists — the
    /// shown tab's state — never creating. The mouse-down hand-back
    /// reads this instead of the pane's page while in Browse.
    pub(crate) fn browse_centre_state(&self) -> Option<Entity<WebviewState>> {
        let id = self.browse_centre_shown()?;
        self.browser.states.get(&id).cloned()
    }

    /// The open terminal dock's height in window pixels, else zero
    /// (B2a-H2b): the one height behind the centre rect, the sync's
    /// dock bounds and the mouse-down hand-back — computed once here,
    /// reused everywhere, so the three can never disagree.
    fn open_dock_height_px(&self, window: &Window) -> Pixels {
        if !self.layout.terminal_open {
            return gpui::px(0.0);
        }
        let centre_height = f32::from(window.bounds().size.height).max(0.0) - HEADER_PX;
        let want =
            self.layout.terminal_height.unwrap_or(crate::terminal::DOCK_DEFAULT_HEIGHT);
        gpui::px(crate::terminal::clamp_dock_height(want, centre_height))
    }

    /// The Browse centre column's rect for the window's `content` box:
    /// past the sidebar when open, short of the right pane when open,
    /// above the open dock. The dock stacks below the centre in the
    /// same `v_flex` — beside it, never over it — so its height
    /// subtracts here, and the column the `webview_pane` fills is
    /// exactly this rect.
    fn browse_centre_content_rect(&self, content: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
        let sidebar = if self.sidebar_open {
            gpui::px(self.resize.width)
        } else {
            gpui::px(0.0)
        };
        let right = if self.layout.right_open {
            gpui::px(self.right_resize.width)
        } else {
            gpui::px(0.0)
        };
        browser_centre_bounds(content, sidebar, right, self.open_dock_height_px(window))
    }

    /// The Browse centre page's rect for the window's `content` box: the
    /// whole column above — the centre page draws with
    /// [`WebviewPane::without_nav`](aui_webview::WebviewPane::without_nav),
    /// so no nav row offsets it. The single source behind placement and
    /// release: `sync_browser` stores it every frame
    /// ([`BrowserRegistry::centre_page_rect`]) and the mouse-down
    /// hand-back reads the stored rect, so the slide — a translate
    /// inside the clipped sidebar, which moves no column — changes
    /// nothing either side computes.
    fn browse_centre_page_rect(&self, content: Bounds<Pixels>, window: &Window) -> Bounds<Pixels> {
        self.browse_centre_content_rect(content, window)
    }

    /// The Browse centre's page (B2a-H2b): the current project's person
    /// tab, when one is open — never creating. Tab creation lives on
    /// the events that need it (entering Browse, closing the last
    /// person tab, the New-tab affordances), never in render: with no
    /// tab the centre draws its empty state instead. The page is built
    /// here, where the centre shows it — the centre twin of
    /// [`Self::browser_for`].
    pub(crate) fn browse_centre_page(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<WebviewState>> {
        self.ensure_tabs_loaded(cx);
        let id = self.browse_centre_shown()?;
        Some(self.ensure_tab_page(HOME_KEY, id, window, cx))
    }

    /// Entering Browse ensures the project has a person tab to show
    /// (B2a-H2): with none, one blank tab opens with the URL field
    /// focused. A project that already has tabs is untouched — and in
    /// particular never steals focus.
    pub(crate) fn ensure_browse_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let space = self.current_project_id();
        if self.browser.tabs.active_of(&Owner::Person, space.as_deref()).is_none() {
            self.new_browse_sidebar_tab(window, cx);
        }
    }

    /// Open a blank person tab in the current project and select it, with
    /// the address focused: the Browse sidebar's "New tab" row and the
    /// entering-Browse ensure share this one path. Clears any carried
    /// session tab: ⌘T lands on the person's new tab, never the park.
    pub(crate) fn new_browse_sidebar_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.browser.centre_override = None;
        self.ensure_tabs_loaded(cx);
        let space = self.current_project_id();
        let after = self.browser.tabs.active_of(&Owner::Person, space.as_deref());
        let id = self.browser.tabs.open(space, Owner::Person, BLANK, after);
        self.browse_session_tab = None;
        self.schedule_tabs_write(cx);
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        // ⌘T lands on the new-tab page with the field focused (B2a-H4):
        // the Harness-owned address input, empty for a blank tab.
        self.focus_browse_address_with("", window, cx);
        cx.notify();
    }

    /// Select the Browse sidebar's tab `id` (B2a-H2): the model
    /// activates and the page builds when discarded. A closed — or a
    /// session-owned — `id` is a no-op, so a × click racing the row's
    /// own select never resurrects the tab. The selection moves first and
    /// the address drops after, so focus lands on the newly shown page
    /// (or the root) — never the old page.
    /// Selecting a person tab
    /// clears a From-sessions show in the centre.
    pub(crate) fn select_browse_sidebar_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Selecting a person tab unparks any carried session tab: the
        // centre shows the person's tab from here on.
        self.browser.centre_override = None;
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none_or(|tab| tab.owner != Owner::Person) {
            return;
        }
        // The tab being left drops its find highlight (its strip keeps the
        // query), and the shared input points at the arriving tab — both
        // on the event, never during render.
        if let Some(leaving) = self.browse_centre_shown() {
            if leaving != id {
                self.stop_find_on_tab(leaving, cx);
            }
        }
        if self.browser.tabs.activate(id) {
            self.schedule_tabs_write(cx);
        }
        self.browse_session_tab = None;
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        self.browse_drop_target = None;
        self.browse_drop_at_end = false;
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        self.sync_find_input_to_tab(id, window, cx);
        self.leave_browse_address(window, cx);
        cx.notify();
    }

    /// Whether the Browse tab shortcuts may fire: only in Browse mode.
    /// The `BaazBrowse` key context scopes them; this is the belt.
    fn browse_command_ready(&mut self, cx: &mut Context<Self>) -> Option<Option<String>> {
        if self.mode != crate::layout::Mode::Browse {
            return None;
        }
        self.ensure_tabs_loaded(cx);
        Some(self.current_project_id())
    }

    /// ⌘W in Browse: while a From-sessions tab shows, stop showing it —
    /// back to the person's active tab, closing nothing. Otherwise close
    /// the current project's active person tab, the model picking the
    /// display-order neighbour. A no-op outside Browse (where ⌘W keeps
    /// closing the window) or with no tab.
    pub(crate) fn close_browse_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        if self.browse_session_tab.take().is_some() {
            cx.notify();
            return;
        }
        let owner = Owner::Person;
        let Some(id) = self.browser.tabs.active_of(&owner, space.as_deref()) else {
            return;
        };
        self.close_tab(HOME_KEY, id, window, cx);
    }

    /// ⌘⇧T in Browse: reopen the most recently closed tab, selecting it
    /// for its owner and showing it when it is this project's person
    /// tab. A no-op outside Browse or with nothing closed.
    pub(crate) fn reopen_browse_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        let Some(id) = self.browser.tabs.reopen_last() else {
            return;
        };
        self.schedule_tabs_write(cx);
        let ours = self.browser.tabs.get(id).is_some_and(|tab| {
            tab.owner == Owner::Person && tab.space.as_deref() == space.as_deref()
        });
        if ours {
            self.browse_session_tab = None;
            self.ensure_tab_page(HOME_KEY, id, window, cx);
        }
        cx.notify();
    }

    /// ⌘1–⌘8 (a 1-based display slot) and ⌘9 (the last tab) in Browse:
    /// select that person tab. Unknown slots are a no-op, as is firing
    /// outside Browse.
    pub(crate) fn select_browse_nth(
        &mut self,
        n: usize,
        last: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        let owner = Owner::Person;
        let Some(id) = self.browser.tabs.nth_tab(&owner, space.as_deref(), n, last) else {
            return;
        };
        self.select_browse_sidebar_tab(id, window, cx);
    }

    /// ⌥⌘↑/⌥⌘↓ in Browse: move the active person tab one display slot.
    /// A no-op while a From-sessions tab shows (the moves act on the
    /// person's hidden tab otherwise), at the edge, across the pinned
    /// boundary, or outside Browse.
    pub(crate) fn move_browse_shortcut(&mut self, delta: i32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(space) = self.browse_command_ready(cx) else {
            return;
        };
        if self.browse_session_tab.is_some() {
            return;
        }
        if self.browser.tabs.move_active_by(&Owner::Person, space.as_deref(), delta) {
            self.schedule_tabs_write(cx);
        }
        let _ = window;
        cx.notify();
    }

    /// Close the person tab `id` from the Browse sidebar's ×: the
    /// model's neighbour rule picks what shows next, and closing the
    /// project's last tab opens a fresh blank one (see
    /// [`Self::close_tab`]). A closed or session-owned `id` is a no-op.
    pub(crate) fn close_person_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none_or(|tab| tab.owner != Owner::Person) {
            return;
        }
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        self.browse_drop_target = None;
        self.browse_drop_at_end = false;
        self.close_tab(HOME_KEY, id, window, cx);
    }

    /// Pin or unpin the person tab `id`: the tab menu's Pin/Unpin row
    /// and the `tab-pin:<n>` step. A closed or session-owned `id` is a
    /// no-op. Answers the new pinned state.
    pub(crate) fn toggle_pin_browse_tab(&mut self, id: TabId, cx: &mut Context<Self>) -> Option<bool> {
        self.ensure_tabs_loaded(cx);
        let pinned = !self.browser.tabs.get(id).filter(|tab| tab.owner == Owner::Person)?.pinned;
        self.browser.tabs.set_pinned(id, pinned).then_some(pinned)?;
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        self.schedule_tabs_write(cx);
        cx.notify();
        Some(pinned)
    }

    /// Duplicate the person tab `id` behind itself and show the copy:
    /// the tab menu's Duplicate row. A closed or session-owned `id` is
    /// a no-op.
    pub(crate) fn duplicate_browse_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none_or(|tab| tab.owner != Owner::Person) {
            return;
        }
        if let Some(copy) = self.browser.tabs.duplicate(id) {
            self.schedule_tabs_write(cx);
            self.browse_session_tab = None;
            self.tab_menu = None;
        self.tab_menu_bounds = None;
            self.ensure_tab_page(HOME_KEY, copy, window, cx);
        }
        cx.notify();
    }

    /// Reload the person tab `id` at its own URL and show it: the tab
    /// menu's Reload row, and what a crashed row's click does (the `!`
    /// tile's promise is reload *and* select). Activating clears a
    /// From-sessions show, like every person-tab select. When the tab has
    /// no live page the ensure rebuilds it anyway. A closed or
    /// session-owned `id` is a no-op.
    pub(crate) fn reload_browse_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(tab) = self.browser.tabs.get(id).cloned() else {
            return;
        };
        if tab.owner != Owner::Person {
            return;
        }
        if self.browser.tabs.activate(id) {
            self.schedule_tabs_write(cx);
        }
        self.browse_session_tab = None;
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        let url = tab.url.clone();
        if !url.is_empty() {
            state.update(cx, |state, _cx| state.navigate(&url));
        }
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        self.browse_drop_target = None;
        cx.notify();
    }

    /// Copy the person tab `id`'s link: the tab menu's Copy link row.
    /// A closed `id` or an empty URL copies nothing.
    pub(crate) fn copy_browse_link(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if let Some(url) = self.browser.tabs.get(id).map(|tab| tab.url.clone()) {
            if !url.is_empty() {
                cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
            }
        }
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        cx.notify();
    }

    /// Hand the person tab `id` to another project: the tab menu's Move
    /// to project row. The old project's active pointer falls back to
    /// its newest remaining tab (or clears — an emptied old project
    /// shows the empty state in Browse, never an auto-blank); the new
    /// project's selects the arrival, and its page is ensured when the
    /// arrival lands in the current project. A closed, session-owned or
    /// already-there `id` (beyond the menu close) changes nothing.
    pub(crate) fn move_browse_tab_to_project(
        &mut self,
        id: TabId,
        project: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        let Some(tab) = self.browser.tabs.get(id).cloned() else {
            self.tab_menu = None;
        self.tab_menu_bounds = None;
            cx.notify();
            return;
        };
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        if tab.owner != Owner::Person || tab.space == project {
            cx.notify();
            return;
        }
        let old_space = tab.space.clone();
        if let Some(live) = self.browser.tabs.tabs.iter_mut().find(|live| live.id == id) {
            live.space = project.clone();
        }
        self.browser.tabs.bump();
        let owner = Owner::Person;
        if self.browser.tabs.active_of(&owner, old_space.as_deref()) == Some(id) {
            let fallback = self
                .browser
                .tabs
                .ordered_tabs(&owner, old_space.as_deref())
                .iter()
                .rfind(|live| live.id != id)
                .map(|live| live.id);
            match fallback {
                Some(next) => {
                    self.browser.tabs.activate(next);
                }
                None => {
                    self.browser.tabs.active.remove(&tabs::active_key(&owner, old_space.as_deref()));
                }
            }
        }
        self.browser.tabs.activate(id);
        self.schedule_tabs_write(cx);
        if project == self.current_project_id() && self.mode == crate::layout::Mode::Browse {
            self.browse_session_tab = None;
            self.ensure_tab_page(HOME_KEY, id, window, cx);
        }
        cx.notify();
    }

    /// Close every other person tab in `id`'s project: the tab menu's
    /// Close other tabs row. A closed or session-owned `id` only closes
    /// the menu.
    pub(crate) fn close_other_browse_tabs(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        self.tab_menu = None;
        self.tab_menu_bounds = None;
        let Some(tab) = self.browser.tabs.get(id).cloned() else {
            cx.notify();
            return;
        };
        if tab.owner != Owner::Person {
            cx.notify();
            return;
        }
        // Only the tabs the model actually closes lose their pages:
        // pinned tabs survive (the model spares them), so their live
        // pages stay registered.
        let before: std::collections::HashSet<TabId> =
            self.browser.tabs.tabs.iter().map(|live| live.id).collect();
        self.browser.tabs.close_others(id);
        let closed: Vec<TabId> = before
            .into_iter()
            .filter(|gone| self.browser.tabs.get(*gone).is_none())
            .collect();
        for gone in closed {
            self.browser.states.remove(&gone);
        }
        self.schedule_tabs_write(cx);
        self.browse_session_tab = None;
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        cx.notify();
    }

    /// Drop the dragged person tab before the target row: the sidebar's
    /// drag-reorder. `None` appends at the end. Either unknown id only
    /// closes the drop slot.
    pub(crate) fn reorder_browse_tab(
        &mut self,
        dragged: TabId,
        before: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        self.browse_drop_target = None;
        self.browse_drop_at_end = false;
        if self.browser.tabs.move_before(dragged, before) {
            self.schedule_tabs_write(cx);
        }
        cx.notify();
    }

    /// Open the person tab `id`'s right-click menu as a floating popup
    /// anchored at the clicked row (see
    /// [`Harness::render_tab_menu_popup`]). A closed `id` only closes
    /// whatever stands open. Opening a menu clears the drag slots: a menu
    /// never opens mid-drag, and a cancelled drag's stale line must not
    /// survive under it.
    pub(crate) fn open_tab_menu(&mut self, id: TabId, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        self.tab_menu = self.browser.tabs.get(id).is_some().then_some(id);
        self.tab_menu_bounds = None;
        self.browse_drop_target = None;
        self.browse_drop_at_end = false;
        cx.notify();
    }

    /// Close the open tab menu, if any: Escape and click-outside share
    /// this one path. Answers whether one stood open.
    pub(crate) fn close_tab_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tab_menu.take().is_some() {
            self.tab_menu_bounds = None;
            cx.notify();
            true
        } else {
            false
        }
    }

    /// Flip a From-sessions group open or shut (collapsed by default).
    /// Unknown session ids still flip: the group may arrive a frame
    /// later than the click.
    pub(crate) fn toggle_from_sessions(&mut self, session: String, cx: &mut Context<Self>) {
        if !self.browse_from_expanded.remove(&session) {
            self.browse_from_expanded.insert(session);
        }
        cx.notify();
    }

    /// Show a session-owned tab in the Browse centre (From sessions):
    /// the model activates it for its session and the centre builds its
    /// page — ownership never moves. A closed or person-owned `id` is a
    /// no-op.
    pub(crate) fn select_session_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none_or(|tab| !matches!(tab.owner, Owner::Session(_))) {
            return;
        }
        if let Some(leaving) = self.browse_centre_shown() {
            if leaving != id {
                self.stop_find_on_tab(leaving, cx);
            }
        }
        if self.browser.tabs.activate(id) {
            self.schedule_tabs_write(cx);
        }
        self.browse_session_tab = Some(id);
        // One route at a time: a From-sessions show replaces any carried tab.
        self.browser.centre_override = None;
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        cx.notify();
    }

    /// The Agents strip's Review path: back to Sessions with that
    /// session open — the quiet open a sidebar click does, with no
    /// reveal and no list steering.
    pub(crate) fn review_session(&mut self, session: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != crate::layout::Mode::Sessions {
            self.set_mode(crate::layout::Mode::Sessions, window, cx);
        }
        self.resume_quiet(session, window, cx);
    }

    /// Prepare the shown tab's webview ahead of the pane animating
    /// (B7): `toggle_right`/`show_right` call this when they land open on
    /// Browser, so the opening frame shows the page instead of paying the
    /// construction cost inside `render`. Only the scripted backend can be
    /// prepared without a window — see [`Self::prewarm_browser_in`] for the
    /// real webview, which the windowed open paths build instead. Never
    /// focuses: focus rides only on the person's own open through
    /// [`Self::ensure_browser_person`].
    pub(crate) fn prewarm_browser_if_needed(&mut self, cx: &mut Context<Self>) {
        // B2a-H2b: in Browse the pane never shows a page, so nothing
        // prewarms one for it — the centre builds its own on show.
        if self.mode == crate::layout::Mode::Browse {
            return;
        }
        if !self.layout.right_open || crate::layout::right_kind(&self.layout) != RightKind::Browser
        {
            return;
        }
        if !(self.browser.fake || cfg!(test)) {
            return;
        }
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let id = self.ensure_owner_tab(&owner, cx);
        if self.browser.states.contains_key(&id) {
            return;
        }
        let url = self
            .browser
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        self.browser_builds += 1;
        let state = cx.new(|cx| {
            let mut backend = FakeWebBackend::new();
            if backend.url().as_ref() != initial {
                backend.navigate(initial);
            }
            WebviewState::new(Box::new(backend), cx)
        });
        self.browser.states.insert(id, state.clone());
        let key = self.browser_key(cx);
        self.register_tab_page(&key, id, &state);
    }

    /// Build the real webview outside `render` (B7fix2): the open action's
    /// own construction, with the window in hand — the shipped app's
    /// WKWebView, the scripted page in tests and captures. Callers are the
    /// click/key handlers that land open on Browser, the deferred warmup
    /// behind [`crate::app::Harness::defer_browser_prewarm`], and the first
    /// signed-in frame. Never more than one webview per session (an
    /// existing one is reused), never focused: focus rides only on the
    /// person's own open through [`Self::ensure_browser_person`].
    pub(crate) fn prewarm_browser_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // B2a-H2b: like `prewarm_browser_if_needed` — no pane page in
        // Browse, so no prewarm for one.
        if self.mode == crate::layout::Mode::Browse {
            return;
        }
        if !self.layout.right_open || crate::layout::right_kind(&self.layout) != RightKind::Browser
        {
            return;
        }
        let key = self.browser_key(cx);
        let owner = self.tab_owner_for_key(&key);
        let exists = self.browser.tabs_loaded
            && self
                .browser
                .tabs
                .active_of(&owner, self.owner_space(&owner).as_deref())
                .is_some_and(|id| self.browser.states.contains_key(&id));
        if exists {
            return;
        }
        let _ = self.browser_for(&key, false, window, cx);
    }

    /// The webview the pane shows this frame, creating it on first use.
    /// The caller gates on pane-open-on-Browser, so activation, boot and
    /// every other kind never create a webview as a side effect; the
    /// `browse:` step and agent tools create through [`Self::browser_for`]
    /// directly instead. Focus rides only on the person's own open: render
    /// consumes one armed `show_right`/`toggle_right` onto a blank page,
    /// and a restore leaves the composer's keyboard alone.
    /// Open a link's URL in Baaz's Browser pane (B9): the non-toggling
    /// open onto Browser, then the navigation. The scripted backend needs
    /// no window, so it navigates at once; the real webview navigates on
    /// the next render, when [`Self::ensure_browser_person`] consumes the
    /// pending URL with the window at hand. Never arms URL-field focus: a
    /// link is not the person's own open.
    pub(crate) fn open_url_in_browser(&mut self, url: String, cx: &mut Context<Self>) {
        self.open_right_on(RightKind::Browser, cx);
        // A person's own open earns URL-field focus; a link never does —
        // undo the arm `open_right_on` owes the person.
        self.browser_url_focus_armed = false;
        if self.browser.fake || cfg!(test) {
            self.ensure_tabs_loaded(cx);
            let owner = self.shown_tab_owner(cx);
            let id = self.ensure_owner_tab(&owner, cx);
            let state = self.browser.states.get(&id).cloned().or_else(|| {
                self.prewarm_browser_if_needed(cx);
                self.browser.states.get(&id).cloned()
            });
            if let Some(state) = state {
                if state.read(cx).url().as_ref() != url.as_str() {
                    let title = state.read(cx).title().to_string();
                    state.update(cx, |state, _cx| state.navigate(&url));
                    if self.browser.tabs.set_url_title(id, &url, &title) {
                        self.schedule_tabs_write(cx);
                    }
                }
            } else {
                self.browser_pending_url = Some(url);
            }
        } else {
            self.browser_pending_url = Some(url);
        }
        cx.notify();
    }

    pub(crate) fn ensure_browser_person(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        let focus = std::mem::take(&mut self.browser_url_focus_armed);
        let key = self.browser_key(cx);
        // B7fix: the toggle prewarms the webview before the frame, so the
        // creation-time focus inside `browser_for` no longer fires on a
        // by-hand open — the state already exists by render. Honor the armed
        // focus on the prewarmed blank page here instead. Restores never arm
        // it, so they still leave the composer's keyboard alone, and an
        // armed open onto a navigated page still does not steal focus.
        let owner = self.tab_owner_for_key(&key);
        let existed = self.browser.tabs_loaded
            && self
                .browser
                .tabs
                .active_of(&owner, self.owner_space(&owner).as_deref())
                .is_some_and(|id| self.browser.states.contains_key(&id));
        let state = self.browser_for(&key, focus, window, cx);
        if focus && existed && state.read(cx).url() == BLANK {
            let focus_handle = state.read(cx).focus_handle().clone();
            window.focus(&focus_handle, cx);
        }
        // A link's pending URL lands here on the next render (B9): the
        // event had no window, so the navigation waited for one. Consumed
        // once — later frames reuse the navigated page.
        if let Some(url) = self.browser_pending_url.take() {
            if state.read(cx).url().as_ref() != url.as_str() {
                state.update(cx, |state, _cx| state.navigate(&url));
            }
        }
        state
    }

    /// `browse:<url>`: navigate the active (or home) browser, idempotently —
    /// navigating to the shown URL is a no-op. Free: no turn, no wire.
    pub(crate) fn step_browse(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = rest.trim();
        if url.is_empty() {
            crate::steps::record_step_failure("browse:");
            return;
        }
        let key = self.browser_key(cx);
        let owner = self.tab_owner_for_key(&key);
        let state = self.browser_for(&key, false, window, cx);
        if state.read(cx).url().as_ref() != url {
            let title = state.read(cx).title().to_string();
            state.update(cx, |state, _cx| state.navigate(url));
            let space = self.owner_space(&owner);
            if let Some(id) = self.browser.tabs.active_of(&owner, space.as_deref()) {
                if self.browser.tabs.set_url_title(id, url, &title) {
                    self.schedule_tabs_write(cx);
                }
            }
        }
        cx.notify();
    }

    /// `tab-new:<url>`: open a tab on `url` — a person tab in Browse
    /// mode, otherwise a tab for the shown owner (a session tab in the
    /// active session, a person tab with no session open) — and select
    /// it. Empty records a step failure. A blank URL focuses the
    /// address field, like a by-hand open. Free: no turn, no wire.
    pub(crate) fn step_tab_new(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = rest.trim();
        if url.is_empty() {
            crate::steps::record_step_failure("tab-new:");
            return;
        }
        // A real page opening ends any open address edit (the blank tab
        // entering Browse opens one), exactly as selecting a tab does.
        if url != BLANK {
            self.leave_browse_address(window, cx);
        }
        self.ensure_tabs_loaded(cx);
        // B2a-H2b: in Browse a new tab is always the person's, even with
        // a session open behind the mode — the session's tabs belong to
        // Sessions.
        let owner = if self.mode == crate::layout::Mode::Browse {
            Owner::Person
        } else {
            self.shown_tab_owner(cx)
        };
        let space = self.current_project_id();
        let after = self.browser.tabs.active_of(&owner, space.as_deref());
        let id = self.browser.tabs.open(space, owner.clone(), url, after);
        self.schedule_tabs_write(cx);
        let key = match &owner {
            Owner::Person => HOME_KEY.to_owned(),
            Owner::Session(_) => self.browser_key(cx),
        };
        let state = self.ensure_tab_page(&key, id, window, cx);
        if url == BLANK {
            state.update(cx, |state, cx| state.begin_editing(window, cx));
        }
        cx.notify();
    }

    /// Build (or reuse) the live page for `id` and register it under `key`.
    /// Showing a discarded tab again recreates its page at its URL.
    fn ensure_tab_page(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        if let Some(state) = self.browser.states.get(&id).cloned() {
            self.register_tab_page(key, id, &state);
            return state;
        }
        let url = self
            .browser
            .tabs
            .get(id)
            .map(|tab| tab.url.clone())
            .unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
        self.browser_builds += 1;
        let fake = self.browser.fake || cfg!(test);
        // The `--steps` capture seam: queued scripts land on the fresh
        // scripted backend before it is boxed, then drain through the real
        // poll path. Production never queues, so this takes nothing.
        let scripts = std::mem::take(&mut self.browser.fake_scripts);
        let state = cx.new(|cx| {
            if fake {
                let mut backend = FakeWebBackend::new();
                if backend.url().as_ref() != initial {
                    backend.navigate(initial);
                }
                for script in &scripts {
                    script.apply_to_fake(&mut backend);
                }
                WebviewState::new(Box::new(backend), cx)
            } else {
                let bg = gpui::Rgba::from(cx.aui().colors.surface_1);
                let rgba =
                    [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(
                    &*window,
                    initial,
                    (0.0, 0.0),
                    (0.0, 0.0),
                    rgba,
                ) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!(
                            "browser: no WKWebView ({error}); falling back to the scripted page"
                        );
                        let mut backend = FakeWebBackend::new();
                        if backend.url().as_ref() != initial {
                            backend.navigate(initial);
                        }
                        WebviewState::new(Box::new(backend), cx)
                    }
                }
            }
        });
        self.browser.states.insert(id, state.clone());
        self.register_tab_page(key, id, &state);
        state
    }

    /// `tab-select:<n>`: select the n-th tab of the shown strip (1-based).
    /// Unknown or out-of-range records a step failure. Free.
    pub(crate) fn step_tab_select(
        &mut self,
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.strip_tab_at(rest, cx) {
            Some((key, id)) => self.select_tab(&key, id, window, cx),
            None => crate::steps::record_step_failure(&format!("tab-select:{rest}")),
        }
    }

    /// `tab-close:<n>`: close the n-th tab of the shown strip (1-based),
    /// activating its neighbour. Unknown or out-of-range records a step
    /// failure. Free.
    pub(crate) fn step_tab_close(
        &mut self,
        rest: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((key, id)) = self.strip_tab_at(rest, cx) else {
            crate::steps::record_step_failure(&format!("tab-close:{rest}"));
            return;
        };
        self.close_tab(&key, id, window, cx);
    }

    /// `tab-pin:<n>`: pin (or unpin, when already pinned) the n-th
    /// person tab of the current project (1-based, on-screen order).
    /// Unknown or out-of-range records a step failure. Free.
    pub(crate) fn step_tab_pin(&mut self, rest: &str, cx: &mut Context<Self>) {
        match self.browse_tab_at(rest, cx) {
            Some(id) => {
                self.toggle_pin_browse_tab(id, cx);
            }
            None => crate::steps::record_step_failure(&format!("tab-pin:{rest}")),
        }
    }

    /// `tab-menu:<n>`: open the n-th person tab's right-click menu
    /// (1-based, on-screen order). Unknown or out-of-range records a step
    /// failure. Free.
    pub(crate) fn step_tab_menu(&mut self, rest: &str, cx: &mut Context<Self>) {
        match self.browse_tab_at(rest, cx) {
            Some(id) => self.open_tab_menu(id, cx),
            None => crate::steps::record_step_failure(&format!("tab-menu:{rest}")),
        }
    }

    /// `from-sessions:<n>`: expand the n-th From-sessions group of the
    /// current project (1-based, first-seen order), collapsing the rest —
    /// what the `browse-sidebar` probe seeds before its shot. Empty
    /// collapses every group; unknown or out-of-range records a step
    /// failure. Free.
    pub(crate) fn step_from_sessions(&mut self, rest: &str, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if rest.trim().is_empty() {
            self.browse_from_expanded.clear();
            cx.notify();
            return;
        }
        let space = self.current_project_id();
        let groups = self.browser.tabs.session_tabs(space.as_deref());
        let n: usize = rest.trim().parse().unwrap_or(0);
        let Some((session, _)) = groups.get(n.saturating_sub(1)).filter(|_| n > 0) else {
            crate::steps::record_step_failure(&format!("from-sessions:{rest}"));
            return;
        };
        self.browse_from_expanded.clear();
        self.browse_from_expanded.insert(session.clone());
        cx.notify();
    }

    /// Resolve `rest` to the current project's n-th person tab (1-based,
    /// on-screen order): what `tab-pin:` and `tab-menu:` index into.
    /// `None` for an unparseable or out-of-range `n`.
    fn browse_tab_at(&mut self, rest: &str, cx: &mut Context<Self>) -> Option<TabId> {
        self.ensure_tabs_loaded(cx);
        let n: usize = rest.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let space = self.current_project_id();
        self.browser.tabs.person_tabs(space.as_deref()).get(n - 1).map(|tab| tab.id)
    }

    /// Resolve `rest` to the shown strip's n-th tab (1-based): the session
    /// key plus the tab id. `None` for an unparseable or out-of-range `n`.
    fn strip_tab_at(&mut self, rest: &str, cx: &mut Context<Self>) -> Option<(String, TabId)> {
        self.ensure_tabs_loaded(cx);
        let n: usize = rest.trim().parse().ok()?;
        if n == 0 {
            return None;
        }
        let owner = self.shown_tab_owner(cx);
        let space = self.owner_space(&owner);
        let id = self
            .browser
            .tabs
            .ordered_tabs(&owner, space.as_deref())
            .get(n - 1)
            .map(|tab| tab.id)?;
        Some((self.browser_key(cx), id))
    }

    /// Select `id` for its owner: the model activates, the page builds when
    /// discarded, and the agent tools re-register onto it. A closed `id`
    /// is a no-op, so a × click racing the row's own select never
    /// resurrects the tab.
    pub(crate) fn select_tab(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none() {
            return;
        }
        if self.browser.tabs.activate(id) {
            self.schedule_tabs_write(cx);
        }
        self.ensure_tab_page(key, id, window, cx);
        cx.notify();
    }

    /// Select the strip tab `id` (click): the shown owner's key resolves
    /// here, through [`Self::select_tab`].
    pub(crate) fn select_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.browser_key(cx);
        self.select_tab(&key, id, window, cx);
    }

    /// Close the strip tab `id` (× or middle-click), through
    /// [`Self::close_tab`].
    pub(crate) fn close_browser_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = self.browser_key(cx);
        self.close_tab(&key, id, window, cx);
    }

    /// Record a middle press on the strip tab `id`: the release closes it
    /// only when it lands on this same tab.
    pub(crate) fn note_strip_middle_press(&mut self, id: TabId) {
        self.browser.middle_pressed = Some(id);
    }

    /// A middle release on the strip tab `id`: close it when the press
    /// started on this same tab, never when the press began elsewhere (or
    /// never began — a press-less release closes nothing). Clears the
    /// recorded press either way, so no press survives its release.
    pub(crate) fn close_strip_middle_release(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let pressed = self.browser.middle_pressed.take();
        if middle_release_closes(pressed, id) {
            self.close_browser_tab(id, window, cx);
        }
    }

    /// Close `id`: the closed page is dropped; only when the closed tab
    /// was its owner's active tab does anything else move — the model
    /// picks the neighbour to activate and the agent tools follow it (or
    /// lose the session's page when none is left). Closing a background
    /// tab rebuilds and re-registers nothing. A session's last tab also
    /// clears its stored `browser_url`, so the tab never resurrects from
    /// the old restore — and the migration runs only once per session, so
    /// it stays closed.
    pub(crate) fn close_tab(
        &mut self,
        key: &str,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        let Some(tab) = self.browser.tabs.get(id).cloned() else {
            return;
        };
        // The tab's OWN owner+space: [`Tabs::close`] keys the active
        // pointer on them, and the current project's space is wrong when
        // the close arrives from another space (a person tab closed while
        // a different project is current).
        let was_active = self
            .browser
            .tabs
            .active_of(&tab.owner, tab.space.as_deref())
            == Some(id);
        let next = self.browser.tabs.close(id);
        self.browser.states.remove(&id);
        self.schedule_tabs_write(cx);
        // B2a-H2b: closing the current project's last person tab while
        // in Browse opens a fresh blank one — entering Browse and this
        // close are the two events that create tabs, never render. Other
        // spaces and Sessions mode keep the closed state: emptiness
        // there is honest, and the centre is not showing.
        if self.mode == crate::layout::Mode::Browse
            && tab.owner == Owner::Person
            && tab.space == self.current_project_id()
            && self.browser.tabs.person_tabs(tab.space.as_deref()).is_empty()
        {
            self.new_browse_sidebar_tab(window, cx);
            return;
        }
        if !was_active {
            cx.notify();
            return;
        }
        match next {
            Some(next) => {
                self.ensure_tab_page(key, next, window, cx);
            }
            None => {
                self.browser.agent_tabs.remove(key);
                self.terminal_service.unregister_browser(key);
                if let Owner::Session(session) = tab.owner {
                    if session != key {
                        self.browser.agent_tabs.remove(&session);
                        self.terminal_service.unregister_browser(&session);
                    }
                    self.clear_session_browser_url(&session, cx);
                }
            }
        }
        cx.notify();
    }

    /// Forget a session's stored browser URL (its last tab just closed),
    /// through the cheap debounced write — never the session-list settle.
    /// A no-op when the stored URL is already gone.
    fn clear_session_browser_url(&mut self, session: &str, cx: &mut Context<Self>) {
        let mut right = match self
            .overrides
            .get(session)
            .and_then(|meta| meta.right.clone())
        {
            Some(right) => right,
            None => return,
        };
        if right.browser_url.is_none() {
            return;
        }
        right.browser_url = None;
        if let Some(meta) = self.overrides.get_mut(session) {
            meta.right = Some(right);
        }
        self.schedule_overrides_write(cx);
    }

    /// Open a blank tab for the shown owner and focus its address field:
    /// the strip's `+`.
    pub(crate) fn new_blank_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let space = self.current_project_id();
        let after = self.browser.tabs.active_of(&owner, space.as_deref());
        let id = self.browser.tabs.open(space, owner, BLANK, after);
        self.schedule_tabs_write(cx);
        let key = self.browser_key(cx);
        let state = self.ensure_tab_page(&key, id, window, cx);
        state.update(cx, |state, cx| state.begin_editing(window, cx));
        cx.notify();
    }

    /// `tab-discard-now`: drop every hidden tab's live page at once,
    /// ignoring the idle timer (audible and agent-active pages are still
    /// spared). The live-check hook. Free.
    pub(crate) fn step_tab_discard_now(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let space = self.owner_space(&owner);
        let shown = self.browser.tabs.active_of(&owner, space.as_deref());
        let centre = self.browse_centre_shown();
        self.discard_hidden_tabs(true, shown, centre, cx);
        cx.notify();
    }

    /// Attach `bytes` (a page screenshot) to the active session's composer
    /// draft as an image chip, keeping a durable copy under temp/attachments.
    /// No active session: a toast, never a lossy drop into the void.
    fn attach_browser_screenshot(&mut self, bytes: Vec<u8>, cx: &mut Context<Self>) {
        let _path = write_screenshot_temp(&bytes);
        match self.active.clone() {
            Some(view) => {
                view.update(cx, |view, cx| {
                    view.attach_screenshot("browser-screenshot".to_owned(), bytes, cx);
                });
                cx.notify();
            }
            None => {
                self.overlays.update(cx, |overlays, _| {
                    overlays.toast("Browser", "No open session to attach the screenshot to.");
                });
                cx.notify();
            }
        }
    }

    /// One [`WebviewIntent`]: what the pane asks the app to do. Screenshots
    /// and annotations land in the active session's composer draft as
    /// attachments and text; nothing here ever auto-sends.
    pub(crate) fn handle_browser_intent(
        &mut self,
        intent: WebviewIntent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match intent {
            WebviewIntent::FocusUrl => {
                let key = self.browser_key(cx);
                let state = self.browser_for(&key, false, window, cx);
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            WebviewIntent::Console => {
                self.overlays.update(cx, |overlays, _| {
                    overlays.toast("Browser", "Console is not available yet.");
                });
                cx.notify();
            }
            WebviewIntent::Screenshot => {
                let key = self.browser_key(cx);
                let state = self.browser_for(&key, false, window, cx);
                state.update(cx, |state, _cx| state.capture());
                match state.read(cx).screenshot().map(<[u8]>::to_vec) {
                    Some(bytes) => self.attach_browser_screenshot(bytes, cx),
                    None => {
                        self.browser.shot_pending.insert(key);
                    }
                }
            }
            WebviewIntent::Annotate(_) => {}
            WebviewIntent::SendAnnotations {
                annotations,
                screenshot,
                url,
            } => {
                if let Some(bytes) = screenshot {
                    self.attach_browser_screenshot(bytes, cx);
                }
                let block = annotations_draft_block(&annotations, &url);
                match self.active.clone() {
                    Some(view) => {
                        view.update(cx, |view, cx| {
                            view.append_draft_block(block, window, cx);
                        });
                        cx.notify();
                    }
                    None => {
                        self.overlays.update(cx, |overlays, _| {
                            overlays
                                .toast("Browser", "No open session to attach the annotations to.");
                        });
                        cx.notify();
                    }
                }
            }
        }
    }

    /// The strip the pane draws: the shown owner's tabs with labels —
    /// empty when the owner has no tabs, and the pane draws no strip then.
    pub(crate) fn browser_strip(&self, cx: &gpui::App) -> Vec<StripTab> {
        if !self.browser.tabs_loaded {
            return Vec::new();
        }
        let owner = self.shown_tab_owner(cx);
        let space = self.owner_space(&owner);
        let active = self.browser.tabs.active_of(&owner, space.as_deref());
        // One ordering for every owner: pinned first, then model order.
        let rows = self.browser.tabs.ordered_tabs(&owner, space.as_deref());
        rows.iter()
            .map(|tab| StripTab {
                id: tab.id,
                title: tabs::tab_label(&tab.title, &tab.url),
                initial: tabs::tab_initial(&tab.title, &tab.url),
                active: Some(tab.id) == active,
            })
            .collect()
    }

    /// Drop hidden tabs' live pages. On the frame (`force` false) only
    /// tabs idle past the timeout go; `tab-discard-now` forces every hidden
    /// page at once. Audible pages, pinned tabs and the agent's active
    /// tabs are always spared; the shown tabs — the pane's and Browse's
    /// centre — are never candidates. Showing a dropped tab again
    /// rebuilds its page at its URL.
    fn discard_hidden_tabs(
        &mut self,
        force: bool,
        shown: Option<TabId>,
        centre: Option<TabId>,
        cx: &mut Context<Self>,
    ) {
        let now = tabs::now_ms();
        let agent: std::collections::HashSet<TabId> =
            self.browser.agent_tabs.values().copied().collect();
        let live: Vec<(TabId, bool, bool)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                let tab = self.browser.tabs.get(*id);
                (*id, page.audible(), tab.is_some_and(|tab| tab.pinned))
            })
            .collect();
        let mut dropped = Vec::new();
        for (id, audible, pinned) in live {
            if Some(id) == shown || Some(id) == centre || audible || pinned || agent.contains(&id) {
                continue;
            }
            let last = self.browser.tabs.get(id).and_then(|tab| tab.last_shown);
            // Forced (`tab-discard-now`) drops every hidden page at once;
            // the frame only drops tabs idle past the timeout.
            let due = if force {
                true
            } else {
                tabs::should_discard(now, last, audible, false)
            };
            if due {
                dropped.push(id);
            }
        }
        for id in &dropped {
            self.browser.states.remove(id);
        }
        // A discarded page can never stay registered with the agent tools:
        // agent tabs are spared above, so this fires only if that ever
        // stops holding — and then the registration dies with the page.
        let dropped: std::collections::HashSet<TabId> = dropped.into_iter().collect();
        let stale: Vec<String> = self
            .browser
            .agent_tabs
            .iter()
            .filter(|(_, id)| dropped.contains(id))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.browser.agent_tabs.remove(&key);
            self.terminal_service.unregister_browser(&key);
        }
    }

    /// Set every live page's obscured flag from the shown tabs: the
    /// pane's page shows while the frame is live, Browse's centre page
    /// while the centre is live, every other page hides. Blank pages stay
    /// hidden (WKWebView paints `about:blank` white over the dark pane).
    fn apply_page_visibility(
        &mut self,
        shown: Option<TabId>,
        frame_live: bool,
        centre: Option<TabId>,
        centre_live: bool,
        cx: &mut Context<Self>,
    ) {
        let pages: Vec<(TabId, Entity<WebviewState>)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| (*id, state.clone()))
            .collect();
        for (id, state) in pages {
            let blank = matches!(state.read(cx).url().as_ref(), "" | BLANK);
            let visible =
                (frame_live && Some(id) == shown || centre_live && Some(id) == centre) && !blank;
            state.update(cx, |state, _cx| state.set_obscured(!visible));
        }
    }

    /// Per-frame browser sync, called from `render` after the overlay set is
    /// known: pull live pages into their tabs (persisting on change),
    /// persist real navigations onto their sessions, resolve pending
    /// screenshot attaches, discard long-hidden pages, and hide every
    /// native view that is not the shown tab. Called with the
    /// already-computed overlay elements (`None` is closed), so this reads
    /// state only.
    /// Whether the Browse centre may show its page this frame (B2a-H2b):
    /// the current project's person tab is there and nothing covers it —
    /// the Skills page and a pending provider switch hide it exactly
    /// like the palette, menus, dialogs and settings hide the pane's.
    pub(crate) fn browse_centre_live(&self, cx: &gpui::App) -> bool {
        self.browse_centre_shown().is_some()
            && self.pending_switch_dest(cx).is_none()
            && !self.skills.open
    }

    #[allow(clippy::too_many_arguments)] // one caller's five cover flags, threaded by hand
    pub(crate) fn sync_browser(
        &mut self,
        overflow: bool,
        view_options: bool,
        account: bool,
        project_menu: bool,
        tab_menu: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        // Live pages report into their tabs — debounced to actual changes,
        // never one write per poll.
        let live: Vec<(TabId, String, String)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| {
                let page = state.read(cx);
                (*id, page.url().to_string(), page.title().to_string())
            })
            .collect();
        let mut tabs_dirty = false;
        for (id, url, title) in &live {
            if self.browser.tabs.set_url_title(*id, url, title) {
                tabs_dirty = true;
            }
        }
        // The session's active tab keeps writing `browser_url`, so older
        // builds still restore — debounced to actual changes, never one
        // write per poll. B7: a navigation is not searchable, so it
        // persists through the cheap debounced write — never the
        // session-list settle.
        let urls: HashMap<TabId, String> =
            live.iter().map(|(id, url, _)| (*id, url.clone())).collect();
        let actives: Vec<(Owner, String)> = self
            .browser
            .tabs
            .active
            .values()
            .filter_map(|id| {
                let tab = self.browser.tabs.get(*id)?;
                let url = urls.get(id)?.clone();
                Some((tab.owner.clone(), url))
            })
            .collect();
        for (owner, current) in actives {
            if let Owner::Session(session) = owner {
                let stored = self
                    .overrides
                    .get(&session)
                    .and_then(|meta| meta.right.clone())
                    .and_then(|right| right.browser_url);
                if should_remember(&current, stored.as_deref()) {
                    self.remember_browser_url_cheap(&session, current, cx);
                }
            }
        }
        if tabs_dirty {
            self.schedule_tabs_write(cx);
        }
        // The header toolbar's back/forward stacks follow every live page:
        // whatever the page reports becomes the stack's current URL, so an
        // outside navigation (a link) lands here and clears the forward
        // stack exactly like a fresh one. Whether a load runs is the
        // engine's own `WebviewState::loading`, read where the stop/reload
        // control needs it — never tracked here.
        for (id, url, _) in &live {
            self.browser.histories.entry(*id).or_default().note_navigated(url);
        }
        // B2a-H5, before the shown-tab computation below: popups become
        // tabs (foreground ones select, so the centre follows),
        // downloads stamp their start for the popover's estimates, and
        // closed tabs drop their find strips.
        self.drain_browse_popups(cx);
        self.stamp_browse_downloads(cx);
        let live: HashSet<TabId> = self.browser.tabs.tabs.iter().map(|tab| tab.id).collect();
        self.browser.find_strips.retain(&live);
        // A closed tab drops its strip above: with none standing open the
        // shared input has no reader, so it drops too — the same hand-over
        // `close_browse_find` does for Done/Esc.
        if !self.browser.find_strips.any_open() {
            self.browse_find = None;
            self.browse_find_sub = None;
        }
        // A Screenshot whose bytes were not there yet attaches on arrival.
        // Pending captures key to the tab they were taken on
        // ([`screenshot_tab_key`]): the bytes attach to the active
        // session's draft only while that tab still shows, so a switch
        // can never misattribute them.
        let active_key = self.browser_key(cx);
        let shown_now = self.browse_centre_shown();
        let pending: Vec<String> = self.browser.shot_pending.iter().cloned().collect();
        for key in pending {
            if let Some(id) = screenshot_tab_id(&key) {
                let ready = self
                    .browser
                    .states
                    .get(&id)
                    .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
                if let Some(bytes) = ready {
                    if shown_now == Some(id) {
                        self.browser.shot_pending.remove(&key);
                        self.attach_browser_screenshot(bytes, cx);
                    }
                }
                continue;
            }
            let owner = self.tab_owner_for_key(&key);
            let space = self.owner_space(&owner);
            let bytes = self
                .browser
                .tabs
                .active_of(&owner, space.as_deref())
                .and_then(|id| self.browser.states.get(&id))
                .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec));
            if let Some(bytes) = bytes {
                if key == active_key {
                    self.browser.shot_pending.remove(&key);
                    self.attach_browser_screenshot(bytes, cx);
                }
            }
        }
        // The native-overlay rule: every webview hides unless the frame
        // shows its session's Browser kind with nothing over it.
        let overlays = self.overlays.read(cx);
        let menu_kind = overlays.menu.as_ref().map(|menu| menu.kind);
        let settings_open = self.settings_open() || self.muse_sheet;
        let palette_open = overlays.palette.is_some();
        // The Browse tab menu is pane state, never the overlay stack, so
        // it arrives as its own flag — and, like every other menu, covers
        // the native page while open.
        let menu_open = menu_kind.is_some_and(|kind| kind != MenuKind::Account)
            || overflow
            || view_options
            || project_menu
            || tab_menu;
        let account_menu_open = menu_kind == Some(MenuKind::Account) || account;
        let dialog_open = overlays.dialog.is_some();
        // The composer's `+` menu is plain view state, never the overlay
        // stack — read it off the active view. Audit: caret popovers and
        // chip pickers ride `overlays.menu` above; the row-detail card hangs
        // beside the sidebar, tooltips seat on the header, and the
        // queue/steer strips render inline in the composer — none of them
        // covers the right pane, so the `+` menu is the only extra input.
        let plus_open = self
            .active
            .as_ref()
            .is_some_and(|view| view.read(cx).plus_open());
        let kind = crate::layout::right_kind(&self.layout);
        // Covering is geometric, not "the dock stands open": the dock
        // lives in the centre column and the pane on the right, so their
        // laid-out rects never intersect and an open dock leaves the page
        // live. Both rects derive from the window's content box, so a
        // future full-width dock intersects and hides the page again.
        let content = window_content_bounds(window);
        let pane = self
            .layout
            .right_open
            .then(|| browser_page_bounds(content, gpui::px(self.right_resize.width)));
        let dock = self.layout.terminal_open.then(|| {
            let left = content.origin.x
                + (if self.sidebar_open {
                    gpui::px(self.resize.width)
                } else {
                    gpui::px(0.0)
                });
            let right = content.origin.x + content.size.width
                - (if self.layout.right_open {
                    gpui::px(self.right_resize.width)
                } else {
                    gpui::px(0.0)
                });
            terminal_dock_bounds(content, left, right, self.open_dock_height_px(window))
        });
        // Q4c: while a provider switch is pending no session's page shows —
        // `browser_key` still names the previous session, and its webview
        // must neither draw nor hit-test under the loading centre.
        let pending_switch = self.pending_right_neutral(cx);
        // The native-overlay rule: every page hides unless the frame shows
        // its owner's Browser kind with nothing over it — and only the
        // shown tab's page ever shows. A tab is its owner's page exactly
        // when its owner is the frame's owner.
        let shown_owner = self.tab_owner_for_key(&active_key);
        let shown_key = match &shown_owner {
            Owner::Session(session) => session.clone(),
            Owner::Person => HOME_KEY.to_owned(),
        };
        // Only its owner's page ever shows — and, while a provider switch
        // is pending, not even that one.
        let shown_space = self.owner_space(&shown_owner);
        let shown = browser_session_active(&shown_key, &active_key, pending_switch)
            .then(|| {
                self.browser
                    .tabs
                    .active_of(&shown_owner, shown_space.as_deref())
            })
            .flatten();
        // Who holds the keyboard, once per frame: both the pane's and the
        // centre's liveness read it, and only the really shown page ever
        // counts (see below).
        let holds: HashMap<TabId, bool> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| (*id, state.read(cx).holds_keyboard()))
            .collect();
        // B2a-H5: the chrome popovers drop over the centre page and the
        // Baaz-drawn crashed page covers its rect. The popover half is
        // resolved honestly below, once the centre tab is known: raw
        // flags blanked the page under popovers that render nothing, and
        // leaked into Sessions mode. The pane side never sees them.
        let in_browse = self.mode == crate::layout::Mode::Browse;
        let chrome_popover_open = false;
        let page_error_open = self
            .browse_centre_shown()
            .and_then(|id| self.browser.states.get(&id))
            .is_some_and(|state| state.read(cx).crashed().is_some());
        let base = BrowserVisibility {
            pane_open: self.layout.right_open,
            kind_browser: kind == RightKind::Browser,
            centre_browse: false,
            session_active: false,
            settings_open,
            palette_open,
            menu_open,
            plus_open,
            account_menu_open,
            dialog_open,
            drop_cover: self.browser.drop_cover,
            terminal_covering: terminal_covers_browser(self.layout.terminal_open, dock, pane),
            // The omnibox drops down over the page, so while it renders
            // rows the page must be obscured (its screenshot stand-in, as
            // with the palette) — gpui never paints over it.
            omnibox_open: self.browse_omnibox_open(cx),
            chrome_popover_open,
            page_error_open,
            // A Chromium page is its own child window: clicking it makes
            // the page key and the gpui window inactive. Counting that as
            // inactive hid the page, which handed the keyboard back, so a
            // click could never focus the page (B1b, live). The window is
            // active while it or the currently SHOWN page holds the
            // keyboard — a hidden or obscured page can never keep it
            // "active", or its native view would show under an overlay.
            window_active: shown_window_active(window.is_window_active(), shown, &holds),
        };
        // The frame shows a page exactly when the pane stands open on the
        // Browser kind with nothing over it; ownership rides `shown`, which
        // is the frame owner's active tab (or nothing while pending).
        let frame_live = browser_visible(BrowserVisibility {
            session_active: true,
            ..base
        });
        // B2a-H2b: in Browse the pane never shows a page — one tab, one
        // page — so the session page can neither share the centre's tab
        // nor paint behind the mode. The render path shows the Changes
        // fallback instead, and the sync hides every pane page here.
        let (shown, frame_live) = if in_browse { (None, false) } else { (shown, frame_live) };
        // B2a-H2: in Browse the centre shows the current project's
        // person-tab page under the same covering rules as the pane.
        // The dock stacks below the centre in the same column — beside
        // it, never over it — so no dock intersection can cover the
        // page and `terminal_covering` stays false; the Skills page and
        // a pending provider switch hide it exactly like the
        // palette/menus/dialogs/settings do. The centre rect is stored
        // for the mouse-down hand-back: the single source behind
        // placement and release.
        let centre_shown = self.browse_centre_shown();
        // B2a-H5: a switch dismisses the chrome popovers (they point at
        // the old tab's state), and leaving Browse closes them all; the
        // find strip is per tab and stays. The last-seen tab stores every
        // frame — including from `None` — so the switch check is never
        // dead and no flag leaks into Sessions mode. The first sight sets
        // without dismissing, so a `--steps` popover opens onto its own
        // first frame.
        let switched = self.browser.last_centre_shown.is_some() && self.browser.last_centre_shown != centre_shown;
        if switched || !in_browse {
            self.browser.downloads_open = false;
            self.browser.extensions_open = false;
            self.browser.downloads_rendered = false;
            self.browser.more_open = false;
            self.browser.more_move_open = false;
        }
        self.browser.last_centre_shown = centre_shown;
        // B2a-H5: covering is honest — a popover covers the page only while
        // it actually renders. A flag whose popover cannot render (no
        // downloads, no centre state) clears instead of blanking the page
        // under an invisible popover; the downloads flag clears only once
        // it rendered, so a scripted download's first frame never closes
        // its own capture.
        let centre_has_state = centre_shown.is_some_and(|id| self.browser.states.contains_key(&id));
        let downloads_empty = self.browse_downloads(cx).is_empty();
        if self.browser.downloads_open && centre_shown.is_some() && !downloads_empty {
            self.browser.downloads_rendered = true;
        }
        if self.browser.downloads_open
            && chrome::downloads_flag_clears(centre_shown.is_some(), downloads_empty, self.browser.downloads_rendered)
        {
            self.browser.downloads_open = false;
            self.browser.downloads_rendered = false;
        }
        if self.browser.extensions_open && centre_shown.is_none() {
            self.browser.extensions_open = false;
        }
        if self.browser.more_open && !centre_has_state {
            self.browser.more_open = false;
            self.browser.more_move_open = false;
        }
        let chrome_popover_open = in_browse
            && ((self.browser.downloads_open && centre_shown.is_some() && !self.browse_downloads(cx).is_empty())
                || (self.browser.extensions_open && centre_shown.is_some())
                || (self.browser.more_open && centre_has_state));
        // A crashed page hides the find strip: the highlight stops and the
        // strip closes, so the error page owns the rect alone. First, so
        // the stored rect below never reserves a row for a strip that just
        // closed.
        if let Some(id) = centre_shown {
            if let Some(state) = self.browser.states.get(&id).cloned() {
                if state.read(cx).crashed().is_some() && self.browser.find_strips.for_tab(id).close() {
                    state.update(cx, |state, _| state.stop_find());
                }
            }
        }
        // The find and permission strips are in-flow rows above the page,
        // so the stored page rect starts below them: placement and the
        // mouse-down release share this one rect.
        let find_open = self.browser.find_strips.visible(centre_shown).is_some();
        let permission_open = centre_shown
            .and_then(|id| self.browser.states.get(&id))
            .is_some_and(|state| !state.read(cx).permissions().is_empty());
        let stack = chrome::strip_stack_px(find_open, permission_open);
        let mut centre_page = self.browse_centre_page_rect(content, window);
        if in_browse && stack > 0.0 {
            centre_page.origin.y += gpui::px(stack);
            centre_page.size.height = (centre_page.size.height - gpui::px(stack)).max(gpui::px(0.0));
        }
        self.browser.centre_page_rect = in_browse.then_some(centre_page);
        // Tabs that navigated away (or closed) drop their "while visiting"
        // grants; then trusted origins auto-answer and background asks
        // mark their rows while in Browse.
        self.revoke_stale_visit_grants();
        self.route_browse_permissions(centre_shown, in_browse, cx);
        let centre_live = browser_visible(BrowserVisibility {
            centre_browse: centre_shown.is_some(),
            session_active: self.browse_centre_live(cx),
            terminal_covering: false,
            window_active: shown_window_active(window.is_window_active(), centre_shown, &holds),
            chrome_popover_open,
            ..base
        });
        // Editing ends when the page takes the keyboard (B2a-H4 7b): a click
        // into a Chromium page moves no gpui focus, so the address input
        // never blurs and would keep its editing look. The per-frame sync
        // ends it instead — on the rising edge only, so typing in the field
        // (which the page never holds through) never cancels itself.
        let centre_holds = centre_shown.and_then(|id| holds.get(&id).copied()).unwrap_or(false);
        if in_browse && centre_holds && !self.browse_last_holds {
            // The page just took the keyboard: drop the field with no
            // refocus (the sync owns no `&mut Window`, and handing focus
            // anywhere would steal it straight back from the page).
            self.drop_browse_address(cx);
        }
        self.browse_last_holds = centre_holds;
        self.apply_page_visibility(shown, frame_live, centre_shown, centre_live, cx);
        // The shown pages stamp their tabs when they really show (never a
        // blank), so discard measures real hiding — in memory only, never
        // a write on its own.
        for (id, live) in [(shown, frame_live), (centre_shown, centre_live)] {
            if live {
                if let Some(id) = id {
                    let blank = self
                        .browser
                        .states
                        .get(&id)
                        .map(|state| matches!(state.read(cx).url().as_ref(), "" | BLANK))
                        .unwrap_or(true);
                    if !blank {
                        self.browser.tabs.mark_shown(id, tabs::now_ms());
                    }
                }
            }
        }
        // Long-hidden pages drop their native views (but keep their tabs);
        // the shown tabs are never candidates.
        self.discard_hidden_tabs(false, shown, centre_shown, cx);
        if tabs_dirty {
            self.schedule_tabs_write(cx);
        }
    }

    /// Capture-phase mouse-down anywhere in the window: a click outside
    /// the browser page's rect hands the keyboard back when the page
    /// holds it (the WKWebView keeps the NSWindow first responder after
    /// one click in the page, which used to kill typing app-wide until
    /// relaunch). Never consumes the event — the click still reaches its
    /// target. No-ops unless the right pane shows the Browser, and never
    /// creates a webview as a side effect.
    pub(crate) fn release_browser_keyboard_on_mouse_down(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // In Browse the keyboard sits with the centre page, whose rect
        // is the stored centre rect — the very rect the sync placed —
        // never the pane's and never a second computation. Before the
        // first sync the live computation stands in.
        if self.mode == crate::layout::Mode::Browse {
            let Some(state) = self.browse_centre_state() else {
                return;
            };
            let page = match self.browser.centre_page_rect {
                Some(page) => page,
                None => {
                    let content = window_content_bounds(window);
                    self.browse_centre_page_rect(content, window)
                }
            };
            if should_release_keyboard_on_mouse_down(
                state.read(cx).holds_keyboard(),
                event.position,
                Some(page),
            ) {
                state.update(cx, |state, _| state.release_keyboard());
            }
            return;
        }
        if !browser_pane_showing(
            self.layout.right_open,
            crate::layout::right_kind(&self.layout) == RightKind::Browser,
        ) {
            return;
        }
        let Some(state) = self.existing_browser(cx) else {
            return;
        };
        let content = window_content_bounds(window);
        // The live page sits under the webview's nav row: a press on the
        // row itself is outside the page and takes the keyboard back too.
        let pane = browser_page_bounds(content, gpui::px(self.right_resize.width));
        let page = gpui::Bounds::new(
            gpui::point(pane.origin.x, pane.origin.y + gpui::px(BROWSER_NAV_PX)),
            gpui::size(
                pane.size.width,
                (pane.size.height - gpui::px(BROWSER_NAV_PX)).max(gpui::px(0.)),
            ),
        );
        if should_release_keyboard_on_mouse_down(
            state.read(cx).holds_keyboard(),
            event.position,
            Some(page),
        ) {
            state.update(cx, |state, _| state.release_keyboard());
        }
    }

    /// ⌘L from anywhere at window level: begin editing the address field
    /// (whole URL selected) when the right pane shows the Browser, and
    /// otherwise ignore the key. The webview's own `cmd-l` binding only
    /// fires while gpui holds the keyboard — with the native page focused
    /// the keystroke never reaches the pane, so this root binding carries
    /// it ([`aui_webview::FocusAddress`]). Already editing is a no-op, so
    /// a press that already reached the pane does not restart the edit.
    pub(crate) fn focus_browser_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // In Browse ⌘L reaches the centre toolbar's own field, which needs
        // no open pane: the Harness-owned input, prefilled with the shown
        // URL (empty on a blank tab, where the new-tab field takes it).
        if self.mode == crate::layout::Mode::Browse {
            self.ensure_browse_tab(window, cx);
            let url = self
                .browse_centre_shown()
                .and_then(|id| {
                    self.browser
                        .states
                        .get(&id)
                        .map(|state| state.read(cx).url().to_string())
                        .or_else(|| self.browser.tabs.get(id).map(|tab| tab.url.clone()))
                })
                .unwrap_or_default();
            let prefill = if url.is_empty() || url == BLANK { "" } else { url.as_str() };
            self.focus_browse_address_with(prefill, window, cx);
            return;
        }
        if !browser_pane_showing(
            self.layout.right_open,
            crate::layout::right_kind(&self.layout) == RightKind::Browser,
        ) {
            return;
        }
        // Q4c: while a provider switch is pending the pane is neutral — no
        // webview shows — so ⌘L reaches nothing.
        if self.pending_switch_dest(cx).is_some() {
            return;
        }
        let key = self.browser_key(cx);
        let state = self.browser_for(&key, false, window, cx);
        // Always (re)focus, even when the field still says it is editing: a
        // click into a Chromium page moves no gpui focus, so the field never
        // blurs and stays "editing" — returning early made every later ⌘L a
        // no-op (B1b, live). `begin_editing` reuses the open input.
        state.update(cx, |state, cx| state.begin_editing(window, cx));
    }

    /// Whether the Browse omnibox suggestion list stands open (B2a-H4):
    /// Browse shows the centre page and the field actually ranks rows —
    /// merely editing with no rows obscures nothing. The covering input
    /// in `sync_browser`: the list drops down over the page.
    pub(crate) fn browse_omnibox_open(&self, cx: &gpui::App) -> bool {
        self.mode == crate::layout::Mode::Browse && !self.browse_suggestions(cx).is_empty()
    }

    /// The address field's current text, or empty at rest.
    pub(crate) fn browse_address_text(&self, cx: &gpui::App) -> String {
        self.browse_address
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default()
    }

    /// Focus the Browse address field with `text` selected (⌘L, a click on
    /// the resting field, ⌘T on a blank tab): the one Harness-owned input
    /// the toolbar and the new-tab field share — only one ever renders it
    /// per frame — so the omnibox reads one value everywhere. Reuses the
    /// open input when one exists. Records whether the page held the
    /// keyboard, so leaving the field can hand focus back to it.
    pub(crate) fn focus_browse_address_with(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.browse_omnibox_selected = 0;
        self.browse_omnibox_moved = false;
        self.browse_completed = None;
        self.browse_completed_text = None;
        self.browse_address_page_focus =
            self.browse_centre_state().is_some_and(|state| state.read(cx).holds_keyboard());
        let text = text.to_owned();
        match self.browse_address.clone() {
            Some(input) => {
                input.update(cx, |input, cx| {
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
            }
            None => {
                let input = cx.new(|cx| InputState::new(window, cx));
                input.update(cx, |input, cx| {
                    input.set_placeholder("Search, open a page, or ask", window, cx);
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                    // ⌘↵ arrives here (not through the commit below): the
                    // input's own `secondary-enter` binding emits
                    // `PressEnter { secondary: true }` in the input context,
                    // so Ask never also fires the plain-enter commit.
                    InputEvent::PressEnter { secondary: true, .. } => this.ask_from_browse_address(window, cx),
                    InputEvent::PressEnter { .. } => this.commit_browse_address(window, cx),
                    // Blur only ends editing — never moves focus (the user
                    // clicked something focusable). Focus restore belongs
                    // only to Escape / Enter / row activation.
                    InputEvent::Blur => this.drop_browse_address(cx),
                    InputEvent::Focus => {
                        this.browse_omnibox_selected = 0;
                        this.browse_omnibox_moved = false;
                        this.browse_completed = None;
                        this.browse_completed_text = None;
                        cx.notify();
                    }
                    InputEvent::Change => {
                        // Typing resets the highlight. A Tab completion
                        // survives only its own write: once the text differs
                        // from what Tab wrote, the person is typing something
                        // else and Enter must open that, not the old row.
                        let typed = this.browse_address.as_ref().map(|input| input.read(cx).value().to_string());
                        if this.browse_completed_text.is_some() && this.browse_completed_text != typed {
                            this.browse_completed = None;
                            this.browse_completed_text = None;
                        }
                        this.browse_omnibox_selected = 0;
                        this.browse_omnibox_moved = false;
                        cx.notify();
                    }
                });
                self.browse_address = Some(input);
                self.browse_address_sub = Some(sub);
            }
        }
        cx.notify();
    }

    /// Drop the address input without navigating: the resting view falls
    /// back to the page URL on the next frame. The window-free core of
    /// [`Self::leave_browse_address`], for the project-adoption paths that
    /// own no window.
    pub(crate) fn drop_browse_address(&mut self, cx: &mut Context<Self>) {
        if self.browse_address.is_some() {
            self.browse_address = None;
            self.browse_address_sub = None;
            self.browse_omnibox_selected = 0;
            self.browse_omnibox_moved = false;
            self.browse_completed = None;
            self.browse_completed_text = None;
            cx.notify();
        }
    }

    /// Leave the address field without navigating: the resting view falls
    /// back to the page URL on the next frame. Runs on Escape, on Enter
    /// (commit/Ask paths) and on row activation — never on blur, which
    /// only ends editing without moving focus. While still
    /// in Browse, focus returns to the page when it held the keyboard
    /// before the edit, else to the root — never a destroyed handle (the
    /// page counts only while its state still exists).
    pub(crate) fn leave_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browse_address.is_none() {
            return;
        }
        self.drop_browse_address(cx);
        if self.mode != crate::layout::Mode::Browse {
            return;
        }
        if self.browse_address_page_focus {
            if let Some(state) = self.browse_centre_state() {
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
                return;
            }
        }
        window.focus(&self.focus_root, cx);
    }

    /// Open the find strip on the shown tab (B2a-H5): ⌘F in Browse with a
    /// page shown, or the ⋯ menu's "Find in page". Focuses the input
    /// prefilled with the tab's kept query; a stored query re-searches.
    /// Opening releases the page keyboard first, so typing lands in the
    /// find field instead of the page. A crashed page or a blank tab
    /// opens nothing.
    pub(crate) fn open_browse_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode != crate::layout::Mode::Browse {
            return;
        }
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else { return };
        let Some(state) = self.browse_centre_page(window, cx) else { return };
        if state.read(cx).crashed().is_some() {
            return;
        }
        if matches!(state.read(cx).url().as_ref(), "" | BLANK) {
            return;
        }
        state.update(cx, |state, _| state.release_keyboard());
        let (query, match_case, research) = {
            let strip = self.browser.find_strips.for_tab(id);
            let research = strip.open();
            (strip.query.clone(), strip.match_case, research)
        };
        self.focus_browse_find_input(&query, window, cx);
        if research {
            state.update(cx, |state, _| state.find(&query, true, match_case));
        }
        cx.notify();
    }

    /// Close the find strip (Done, Esc): `stop_find` clears the highlight.
    /// Closing a closed strip is a no-op, never a second `stop_find`. The
    /// shared input lives while any tab's strip stands open, so closing
    /// one tab's strip never strands the others'.
    pub(crate) fn close_browse_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.browse_centre_shown() else { return };
        let stop = self.browser.find_strips.for_tab(id).close();
        if !self.browser.find_strips.any_open() {
            self.browse_find = None;
            self.browse_find_sub = None;
        }
        if stop {
            if let Some(state) = self.browse_centre_state() {
                state.update(cx, |state, _| state.stop_find());
            }
        }
        window.focus(&self.focus_root, cx);
        cx.notify();
    }

    /// Clear the tab's find highlight as it is left behind (a switch or a
    /// crash): the strip itself stays as it stood, so returning restores
    /// the query — only the page's highlight drops.
    fn stop_find_on_tab(&mut self, id: TabId, cx: &mut Context<Self>) {
        let open = self.browser.find_strips.for_tab(id).open;
        if open {
            if let Some(state) = self.browser.states.get(&id).cloned() {
                state.update(cx, |state, _| state.stop_find());
            }
        }
    }

    /// Point the shared find input at the tab's kept query (a tab switch):
    /// the value is set here, on the event — never during render.
    fn sync_find_input_to_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let (open, query) = match self.browser.find_strips.visible(Some(id)) {
            Some((_, strip)) => (true, strip.query.clone()),
            None => return,
        };
        if open {
            if let Some(input) = self.browse_find.clone() {
                input.update(cx, |input, cx| {
                    input.set_value(query, window, cx);
                });
            }
        }
    }

    /// Step the find to the next (`forward`) or previous match: Enter, the
    /// ↑/↓ buttons. A no-op with no open strip or no query.
    pub(crate) fn step_browse_find(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(id) = self.browse_centre_shown() else { return };
        let (query, match_case) = {
            let strip = self.browser.find_strips.for_tab(id);
            if !strip.open || strip.query.is_empty() {
                return;
            }
            (strip.query.clone(), strip.match_case)
        };
        if let Some(state) = self.browse_centre_state() {
            state.update(cx, |state, _| state.find(&query, forward, match_case));
        }
    }

    /// Flip "Match case" and re-search the kept query.
    pub(crate) fn toggle_browse_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.browse_centre_shown() else { return };
        let research = self.browser.find_strips.for_tab(id).toggle_case();
        if research {
            self.step_browse_find(true, cx);
        }
        cx.notify();
    }

    /// Focus the find input with `text`, creating it on first use. The
    /// subscription types live (debounced) and commits Enter to the next
    /// match; Escape never reaches here (the wrapper owns it, like the
    /// address field), and blur drops nothing — the strip closes through
    /// Done, Esc or a tab switch, never by clicking the page.
    fn focus_browse_find_input(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let text = text.to_owned();
        match self.browse_find.clone() {
            Some(input) => {
                input.update(cx, |input, cx| {
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
            }
            None => {
                let input = cx.new(|cx| InputState::new(window, cx));
                input.update(cx, |input, cx| {
                    input.set_placeholder("Find in page", window, cx);
                    input.set_value(text, window, cx);
                    input.select_all(window, cx);
                    input.focus(window, cx);
                });
                let sub = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
                    InputEvent::PressEnter { .. } => this.step_browse_find(true, cx),
                    InputEvent::Change => this.find_input_changed(cx),
                    InputEvent::Blur | InputEvent::Focus => {}
                });
                self.browse_find = Some(input);
                self.browse_find_sub = Some(sub);
            }
        }
        cx.notify();
    }

    /// Live search as the query types: a changed non-empty query searches
    /// 100 ms after the last keystroke; clearing the field stops nothing.
    /// The debounce is per tab: the timer fires only for the tab that
    /// armed it, so a switch never searches a stale query on the new tab.
    fn find_input_changed(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.browse_centre_shown() else { return };
        let typed = self.browse_find.as_ref().map(|input| input.read(cx).value().to_string());
        let Some(typed) = typed else { return };
        if !self.browser.find_strips.for_tab(id).set_query(&typed) {
            return;
        }
        let generation = self.browser.find_strips.arm(id);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(chrome::FIND_DEBOUNCE_MS)).await;
            let _ = this.update(cx, |this, cx| {
                if this.browser.find_strips.armed_current(id, generation)
                    && this.browse_centre_shown() == Some(id)
                {
                    this.step_browse_find(true, cx);
                }
            });
        })
        .detach();
    }

    /// Answer the shown tab's pending permission `id`: Block denies, Allow
    /// once and Allow while visiting grant — the latter remembers the
    /// (origin, kinds) for this run and auto-answers repeats of exactly
    /// those kinds. The grant keys on the full origin (scheme + host +
    /// port): a host-only report borrows them from the tab's URL.
    pub(crate) fn answer_shown_permission(&mut self, id: u64, allow: bool, remember_visit: bool, cx: &mut Context<Self>) {
        let tab_url = self.browse_centre_shown().and_then(|id| self.browser.tabs.get(id).map(|tab| tab.url.clone()));
        let ask = self.browse_centre_state().and_then(|state| {
            state
                .read(cx)
                .permissions()
                .iter()
                .find(|request| request.id == id)
                .map(|request| (request.origin.clone(), request.kinds.clone()))
        });
        if remember_visit && allow {
            if let (Some((origin, kinds)), Some(tab_url)) = (&ask, &tab_url) {
                self.browser.visit_allows.remember(&chrome::normalize_origin(origin, tab_url), kinds);
            }
        }
        if let Some(state) = self.browse_centre_state() {
            let decision = if allow { PermissionDecision::Allow } else { PermissionDecision::Deny };
            state.update(cx, |state, cx| state.answer_permission(id, decision, cx));
        }
        cx.notify();
    }

    /// Route permission asks once per frame: origins trusted "while
    /// visiting" auto-answer on any tab, but only for the kinds granted —
    /// anything else still asks. Any other ask on a background tab marks
    /// that tab's row (the sidebar's "needs you" dot) until the tab is
    /// shown — in Browse only, where the rows live. The strip itself reads
    /// the shown tab's live asks, so a page dismissal drops it with no
    /// Baaz state to clear.
    fn route_browse_permissions(&mut self, shown: Option<TabId>, mark_background: bool, cx: &mut Context<Self>) {
        let live: Vec<(TabId, Vec<LiveAsk>)> = self
            .browser
            .states
            .iter()
            .map(|(id, state)| {
                let asks = state
                    .read(cx)
                    .permissions()
                    .iter()
                    .map(|request| (request.id, request.origin.clone(), request.kinds.clone()))
                    .collect();
                (*id, asks)
            })
            .collect();
        let mut dirty = false;
        for (id, asks) in &live {
            let tab_url = self.browser.tabs.get(*id).map(|tab| tab.url.clone()).unwrap_or_default();
            for (ask, origin, kinds) in asks {
                let origin = chrome::normalize_origin(origin, &tab_url);
                if chrome::auto_answer(&origin, kinds, &self.browser.visit_allows).is_some() {
                    if let Some(state) = self.browser.states.get(id).cloned() {
                        state.update(cx, |state, cx| state.answer_permission(*ask, PermissionDecision::Allow, cx));
                    }
                } else if mark_background && Some(*id) != shown {
                    if let Some(tab) = self.browser.tabs.tabs.iter_mut().find(|tab| tab.id == *id) {
                        if !tab.needs_attention {
                            tab.needs_attention = true;
                            dirty = true;
                        }
                    }
                }
            }
        }
        if let Some(id) = shown {
            let pending = live.iter().find(|(live, _)| *live == id).is_some_and(|(_, asks)| !asks.is_empty());
            if !pending {
                if let Some(tab) = self.browser.tabs.tabs.iter_mut().find(|tab| tab.id == id) {
                    if tab.needs_attention {
                        tab.needs_attention = false;
                        dirty = true;
                    }
                }
            }
        }
        if dirty {
            self.schedule_tabs_write(cx);
        }
    }

    /// Revoke "while visiting" grants the tabs left behind: a tab that
    /// navigated to another origin — or closed — drops that origin's
    /// grants, unless another live tab still shows it. Pure model work,
    /// once per frame.
    fn revoke_stale_visit_grants(&mut self) {
        let live: HashMap<TabId, String> = self
            .browser
            .tabs
            .tabs
            .iter()
            .map(|tab| (tab.id, chrome::normalize_origin(&tab.url, &tab.url)))
            .collect();
        let live_origins: HashSet<String> = live.values().cloned().collect();
        let mut stale = Vec::new();
        for (id, seen) in &self.browser.permission_origins {
            if live.get(id) != Some(seen) {
                stale.push(seen.clone());
            }
        }
        for origin in stale {
            if !live_origins.contains(&origin) {
                self.browser.visit_allows.revoke_origin(&origin);
            }
        }
        self.browser.permission_origins = live;
    }

    /// Drain popups and "open in new tab" requests each frame for every
    /// live tab: each becomes a new tab right after its opener, with the
    /// opener's owner and space, wrapping the given backend for popups — a
    /// fresh tab at that URL for open requests. Only the shown tab's
    /// popups may take the centre: a foreground ask from a background
    /// opener lands in the background instead.
    fn drain_browse_popups(&mut self, cx: &mut Context<Self>) {
        let shown = self.browse_centre_shown();
        let openers: Vec<TabId> = self.browser.states.keys().copied().collect();
        for opener in openers {
            let Some(state) = self.browser.states.get(&opener).cloned() else { continue };
            let popups = state.update(cx, |state, _| state.take_popup_backends());
            for (url, foreground, backend) in popups {
                let foreground = chrome::popup_foreground(foreground, opener, shown);
                let Some(id) = chrome::place_popup_tab(&mut self.browser.tabs, opener, &url, foreground) else { continue };
                let page = cx.new(|cx| WebviewState::new(backend, cx));
                self.browser.states.insert(id, page);
                self.browser.find_strips.remove(id);
                self.schedule_tabs_write(cx);
                cx.notify();
            }
            let opens = state.update(cx, |state, _| state.take_open_requests());
            for (url, foreground) in opens {
                let foreground = chrome::popup_foreground(foreground, opener, shown);
                if chrome::place_popup_tab(&mut self.browser.tabs, opener, &url, foreground).is_some() {
                    self.schedule_tabs_write(cx);
                    cx.notify();
                }
            }
        }
    }

    /// Every download's latest state across the live tabs, with the tab
    /// that owns it (cancels and "saved by" labels route through it).
    pub(crate) fn browse_downloads(&self, cx: &gpui::App) -> Vec<(TabId, Owner, DownloadState)> {
        let mut out = Vec::new();
        for (id, state) in &self.browser.states {
            let owner = self.browser.tabs.get(*id).map(|tab| tab.owner.clone()).unwrap_or(Owner::Person);
            for download in state.read(cx).downloads() {
                out.push((*id, owner.clone(), download.clone()));
            }
        }
        out.sort_by_key(|(_, _, download)| download.id);
        out
    }

    /// Stamp newly seen active downloads and prune ended ones, once per
    /// frame. The popover's "6 s left" reads the elapsed time since the
    /// stamp against the average rate so far.
    fn stamp_browse_downloads(&mut self, cx: &mut Context<Self>) {
        let now = tabs::now_ms();
        let mut live = std::collections::HashSet::new();
        for (id, state) in &self.browser.states {
            for download in state.read(cx).downloads() {
                if download.state == aui_webview::DownloadStatus::Downloading {
                    live.insert((*id, download.id));
                    self.browser.download_started.entry((*id, download.id)).or_insert(now);
                }
            }
        }
        self.browser.download_started.retain(|key, _| live.contains(key));
    }

    /// Rebuild the shown tab's page, consuming queued fake-backend scripts
    /// (the `--steps` capture seam; production never queues). Answers
    /// whether a page was rebuilt.
    fn rebuild_shown_browse_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else { return false };
        self.browser.states.remove(&id);
        self.ensure_tab_page(HOME_KEY, id, window, cx);
        true
    }

    /// Dismiss every Browse chrome popover (downloads, extensions, ⋯):
    /// row activation, the toggle press, click-outside, Escape and tab
    /// switches all land here. Answers whether one stood open.
    pub(crate) fn dismiss_browse_popovers(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.browser.downloads_open
            || self.browser.extensions_open
            || self.browser.more_open
            || self.browser.more_move_open;
        self.browser.downloads_open = false;
        self.browser.extensions_open = false;
        self.browser.more_open = false;
        self.browser.more_move_open = false;
        self.browser.downloads_rendered = false;
        if open {
            cx.notify();
        }
        open
    }

    /// Copy the shown tab's link (⇧⌘C in Browse, the ⋯ menu): the live
    /// page's URL, falling back to the tab's. A no-op with no shown tab
    /// or no URL.
    pub(crate) fn copy_shown_browse_link(&mut self, cx: &mut Context<Self>) {
        let url = self
            .browse_centre_state()
            .map(|state| state.read(cx).url().to_string())
            .filter(|url| !url.is_empty())
            .or_else(|| {
                self.browse_centre_shown().and_then(|id| self.browser.tabs.get(id).map(|tab| tab.url.clone()))
            })
            .unwrap_or_default();
        if !url.is_empty() {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(url));
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Open the shown tab's developer tools (⌥⌘I in Browse, the ⋯ menu).
    /// A no-op with no shown page, or where the backend cannot host them.
    pub(crate) fn open_browse_devtools(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.browse_centre_state() {
            if state.read(cx).supports(aui_webview::BackendFeature::DevTools) {
                state.update(cx, |state, _| state.show_devtools());
            }
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Step the shown tab's zoom along Chrome's ladder (the ⋯ menu's –/+
    /// row): `delta` is a direction, not points — positive climbs one
    /// rung, negative drops one, zero holds.
    pub(crate) fn browse_zoom_step(&mut self, delta: i16, cx: &mut Context<Self>) {
        if delta == 0 {
            return;
        }
        if let Some(state) = self.browse_centre_state() {
            let next = toolbar::zoom_step(state.read(cx).zoom(), delta > 0);
            state.update(cx, |state, _| state.set_zoom(next));
        }
    }

    /// Toggle annotate mode on the shown tab (the ⋯ menu): Browse hides
    /// the library's nav row, so this row is how the intent stays
    /// reachable. A no-op with no shown page.
    pub(crate) fn toggle_browse_annotate(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.browse_centre_state() {
            let on = !state.read(cx).annotating();
            state.update(cx, |state, _| state.set_annotate(on));
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Screenshot the shown tab into the active session's draft (the ⋯
    /// menu): the same capture-or-pend path the library intent takes, read
    /// off the centre page instead of the pane's. The popover closes
    /// first, so the page it covered is visible in the capture; the
    /// pending capture keys to the shown tab, never the session key, so a
    /// switch can never misattribute it.
    pub(crate) fn capture_browse_screenshot(&mut self, cx: &mut Context<Self>) {
        self.dismiss_browse_popovers(cx);
        let shown = self.browse_centre_shown();
        if let Some(state) = self.browse_centre_state() {
            state.update(cx, |state, _cx| state.capture());
            match state.read(cx).screenshot().map(<[u8]>::to_vec) {
                Some(bytes) => self.attach_browser_screenshot(bytes, cx),
                None => {
                    if let Some(id) = shown {
                        self.browser.shot_pending.insert(screenshot_tab_key(id));
                    }
                }
            }
        }
    }

    /// Whether the ⋯ menu offers "Open in Safari": only for `http(s)` pages.
    pub(crate) fn browse_safari_url(&self, cx: &gpui::App) -> Option<String> {
        let url = self
            .browse_centre_state()
            .map(|state| state.read(cx).url().to_string())
            .unwrap_or_default();
        (url.starts_with("http://") || url.starts_with("https://")).then_some(url)
    }

    /// Open the shown tab's URL in Safari (`open -a Safari <url>`).
    /// A no-op unless the URL is `http(s)`.
    pub(crate) fn open_browse_in_safari(&mut self, cx: &mut Context<Self>) {
        if let Some(url) = self.browse_safari_url(cx) {
            let _ = std::process::Command::new("open").args(["-a", "Safari", &url]).spawn();
        }
        self.dismiss_browse_popovers(cx);
    }

    /// Open Settings at the Browser section (the ⋯ menu; the section
    /// exists, so no General fallback runs).
    pub(crate) fn open_browse_settings(&mut self, cx: &mut Context<Self>) {
        self.dismiss_browse_popovers(cx);
        self.open_settings_page("browser", cx);
    }

    /// Reload the shown tab at its own URL (the crashed page's Reload):
    /// the navigation clears the crash, whatever the tab's owner.
    pub(crate) fn reload_browse_shown(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else { return };
        let url = self.browser.tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_default();
        if url.is_empty() {
            return;
        }
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        state.update(cx, |state, _cx| state.navigate(&url));
        cx.notify();
    }

    /// Cancel the download `id` on its owning tab's page.
    pub(crate) fn cancel_browse_download(&mut self, tab: TabId, id: u32, cx: &mut Context<Self>) {
        if let Some(state) = self.browser.states.get(&tab).cloned() {
            state.update(cx, |state, _| state.cancel_download(id));
        }
        cx.notify();
    }

    /// Retry a failed download (its "Try again"): re-navigate the owning
    /// tab to the download's URL, exactly as re-clicking the link would.
    pub(crate) fn retry_browse_download(&mut self, tab: TabId, url: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.tabs.get(tab).is_none() {
            return;
        }
        let state = self.ensure_tab_page(HOME_KEY, tab, window, cx);
        state.update(cx, |state, _cx| state.navigate(url));
        self.dismiss_browse_popovers(cx);
    }

    /// Reveal a landed file in Finder (`open -R -- <path>`). A file that
    /// moved or vanished toasts instead of revealing nothing.
    pub(crate) fn reveal_browse_download(&mut self, path: &str, cx: &mut Context<Self>) {
        if !std::path::Path::new(path).exists() {
            self.overlays.update(cx, |overlays, _| overlays.toast("Downloads", "File moved or deleted"));
            cx.notify();
            return;
        }
        let _ = std::process::Command::new("open").args(["-R", "--", path]).spawn();
    }

    /// Open the downloads folder ("All downloads", `~/Downloads`, resolved
    /// from `$HOME` with no `dirs` dependency). With no `$HOME` there is
    /// nothing to open: log and return.
    pub(crate) fn open_downloads_folder() {
        let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
            crate::baaz_log!("downloads: no $HOME, cannot open the downloads folder");
            return;
        };
        let _ = std::process::Command::new("open").arg(home.join("Downloads")).spawn();
    }

    /// Navigate the Browse centre's shown tab to `url`: the model records
    /// it first so a discarded page rebuilds there, then the live page
    /// navigates when one exists. A navigation ends the address edit, like
    /// committing it does. Whether a load runs is the engine's own
    /// [`WebviewState::loading`](aui_webview::WebviewState::loading), never
    /// a Baaz-tracked set.
    pub(crate) fn navigate_browse_tab(
        &mut self,
        id: TabId,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.leave_browse_address(window, cx);
        self.ensure_tabs_loaded(cx);
        let title = self.browser.tabs.get(id).map(|tab| tab.title.clone()).unwrap_or_default();
        if self.browser.tabs.set_url_title(id, url, &title) {
            self.schedule_tabs_write(cx);
        }
        match self.browser.states.get(&id).cloned() {
            Some(state) => {
                if state.read(cx).url().as_ref() != url {
                    state.update(cx, |state, _cx| state.navigate(url));
                }
            }
            None => {
                self.ensure_tab_page(HOME_KEY, id, window, cx);
            }
        }
        cx.notify();
    }

    /// Commit the address field (Enter): the highlighted omnibox row opens
    /// once the highlight moved off the top Ask row — otherwise the typed
    /// text navigates (a URL, or what looks like a host) or searches with
    /// the default engine — and empty restores the field, never navigating.
    /// Runs on the input's enter.
    pub(crate) fn commit_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A Tab completion remembers its ROW: Enter opens that row even
        // after the ranking moved under it — never an index into the
        // re-ranked list.
        if let Some(target) = self.browse_completed.clone() {
            return self.activate_omnibox_target(target, window, cx);
        }
        if self.browse_omnibox_moved {
            let rows = self.browse_suggestions(cx);
            let index = self.browse_omnibox_selected.min(rows.len().saturating_sub(1));
            return self.activate_omnibox_row(index, window, cx);
        }
        let typed = self.browse_address_text(cx);
        self.leave_browse_address(window, cx);
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else {
            return;
        };
        match toolbar::classify_address_input(&typed) {
            toolbar::NavTarget::Url(url) => self.navigate_browse_tab(id, &url, window, cx),
            toolbar::NavTarget::Search(url) => {
                if !url.is_empty() {
                    self.navigate_browse_tab(id, &url, window, cx);
                } else {
                    cx.notify();
                }
            }
        }
    }

    /// Step the shown tab back within its Baaz-tracked history.
    pub(crate) fn browse_go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else {
            return;
        };
        let url = self.browser.histories.entry(id).or_default().go_back();
        if let Some(url) = url {
            self.navigate_browse_tab(id, &url, window, cx);
        }
    }

    /// Step the shown tab forward within its Baaz-tracked history.
    pub(crate) fn browse_go_forward(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else {
            return;
        };
        let url = self.browser.histories.entry(id).or_default().go_forward();
        if let Some(url) = url {
            self.navigate_browse_tab(id, &url, window, cx);
        }
    }

    /// Reload the shown tab — or stop its load while one is running (the
    /// progress line's button). The button follows the engine's own
    /// [`WebviewState::loading`](aui_webview::WebviewState::loading): stop
    /// exactly while the engine reports a load, reload otherwise. The
    /// library exposes no reload call, so a reload re-navigates the shown
    /// URL.
    pub(crate) fn browse_reload_or_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else {
            return;
        };
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        if state.read(cx).loading() {
            state.update(cx, |state, _cx| state.stop());
        } else {
            let url = state.read(cx).url().to_string();
            if !url.is_empty() {
                state.update(cx, |state, _cx| state.navigate(&url));
            }
        }
        cx.notify();
    }

    /// Reset the shown tab's zoom to 100% (the zoom pill's click).
    pub(crate) fn browse_reset_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browse_centre_shown() else {
            return;
        };
        let state = self.ensure_tab_page(HOME_KEY, id, window, cx);
        state.update(cx, |state, _cx| state.set_zoom(100));
        cx.notify();
    }

    /// The pane's "Browse" button (B2a-H4): the session's active tab shows
    /// full-width in Browse mode with the ↩ chip. The tab stays
    /// session-owned; the same `WebviewState` moves — no reload, the state
    /// stays keyed by tab id and the centre renders it through
    /// `browse_centre_page`.
    pub(crate) fn carry_session_tab_to_browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        if !matches!(owner, Owner::Session(_)) {
            return;
        }
        let space = self.owner_space(&owner);
        let Some(id) = self.browser.tabs.active_of(&owner, space.as_deref()) else {
            return;
        };
        // `set_mode` clears the park, so the carry parks after the switch.
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.browser.centre_override = Some(id);
        self.browse_session_tab = None;
        cx.notify();
    }

    /// The ↩ chip (B2a-H4): the carried tab goes back to its session's pane
    /// — Sessions mode with that session selected — with no reload: the
    /// same `WebviewState` entity renders in the pane once its session
    /// shows again.
    pub(crate) fn browse_chip_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.browser.centre_override.take() else {
            return;
        };
        let session = self.browser.tabs.get(id).and_then(|tab| match &tab.owner {
            Owner::Session(session) => Some(session.clone()),
            Owner::Person => None,
        });
        let Some(session) = session else {
            cx.notify();
            return;
        };
        self.set_mode(crate::layout::Mode::Sessions, window, cx);
        self.resume(session, window, cx);
    }

    /// The session label behind the ↩ chip: the collapsed head label the
    /// Sessions header wears, falling back to the id.
    pub(crate) fn browse_chip_session_name(&self, session: &str, cx: &gpui::App) -> String {
        self.collapsed_head_label(session, cx).unwrap_or_else(|| session.to_owned())
    }

    /// Ask the agent about `query` (the omnibox's Ask row, ⌘↵): a new
    /// session in this project with the text as its first prompt draft —
    /// never sent, never navigated. The draft rides
    /// [`Self::pending_ask_draft`](crate::app::Harness::pending_ask_draft),
    /// keyed by the session id `new_session_in` creates (or the draft id
    /// it reuses) — and lands when that session's view lands (see
    /// `activate`), never on whatever `self.active` happens to be right
    /// now. Any early return in `new_session_in` drops it, so no unrelated
    /// resume ever takes it.
    pub(crate) fn browse_ask(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        let query = query.trim().to_owned();
        if query.is_empty() {
            return;
        }
        self.leave_browse_address(window, cx);
        self.pending_ask_draft = Some(crate::app::PendingAsk { target: None, query });
        self.new_session(window, cx);
        cx.notify();
    }

    /// Key the armed Ask draft to `session`: the id `new_session_in`
    /// created (or the draft id it reused). Called on every path that opens
    /// the Ask's session, before its view lands — so `activate` takes the
    /// draft only for this session.
    pub(crate) fn key_pending_ask(&mut self, session: &str) {
        if let Some(pending) = self.pending_ask_draft.as_mut() {
            pending.target = Some(session.to_owned());
        }
    }

    /// Drop the armed Ask draft: every early return in `new_session_in`
    /// (a switch already pending, no params, a failed or async-never-landing
    /// provider open) drops it, so no unrelated resume ever takes it.
    pub(crate) fn drop_pending_ask(&mut self) {
        self.pending_ask_draft = None;
    }

    /// ⌘↵ inside the address field: Ask about the field's text. Runs from
    /// the input's own `secondary-enter` binding (see
    /// [`Self::focus_browse_address_with`]), so the plain-enter commit
    /// never also fires.
    pub(crate) fn ask_from_browse_address(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.browse_address_text(cx);
        self.browse_ask(&query, window, cx);
    }

    /// The omnibox rows for the field's current text: this project's open
    /// tabs (person tabs plus session tabs) and its sessions — history
    /// stays empty until B2b fills it, which omits the section. Sessions
    /// stay inside this project and out of the hidden list.
    pub(crate) fn browse_suggestions(&self, cx: &gpui::App) -> Vec<OmniRow> {
        let query = self.browse_address_text(cx);
        if query.trim().is_empty() || !self.browser.tabs_loaded {
            return Vec::new();
        }
        let space = self.current_project_id();
        let mut tabs: Vec<(TabId, String, String, u64)> = self
            .browser
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| {
                let label = tabs::tab_label(&tab.title, &tab.url);
                (
                    tab.id,
                    if label.is_empty() { tab.url.clone() } else { label },
                    tab.url.clone(),
                    tab.last_shown.unwrap_or(0),
                )
            })
            .collect();
        for (_, group) in self.browser.tabs.session_tabs(space.as_deref()) {
            for tab in group {
                let label = tabs::tab_label(&tab.title, &tab.url);
                tabs.push((
                    tab.id,
                    if label.is_empty() { tab.url.clone() } else { label },
                    tab.url.clone(),
                    tab.last_shown.unwrap_or(0),
                ));
            }
        }
        let current = self.current_project_id();
        let sessions: Vec<(String, String, u64)> = self
            .sessions
            .iter()
            .filter(|entry| !entry.archived && !entry.hidden && entry.project == current)
            .map(|entry| (entry.id.clone(), entry.label.clone(), recency_ms(&entry.updated)))
            .collect();
        toolbar::rank_omnibox(&query, &tabs, &sessions, &[])
    }

    /// Move the omnibox highlight by `delta`, wrapping over the rows the
    /// field currently ranks. An explicit move supersedes any Tab
    /// completion: the highlight, not the completed row, now owns Enter.
    pub(crate) fn move_omnibox(&mut self, delta: isize, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        if rows.is_empty() {
            return;
        }
        let len = rows.len() as isize;
        let next = (self.browse_omnibox_selected as isize + delta).rem_euclid(len);
        self.browse_omnibox_selected = next as usize;
        self.browse_omnibox_moved = true;
        self.browse_completed = None;
        self.browse_completed_text = None;
        cx.notify();
    }

    /// Complete the highlighted row's text into the field (Tab): tabs write
    /// their URL, sessions their title; Ask and Search rows are not
    /// completable and Tab leaves them alone. The selection is clamped to
    /// the rows and kept, and the completed ROW's identity is remembered —
    /// never an index into the re-ranked list — so Enter opens that row.
    /// The row stays open for Enter or ⌘↵.
    pub(crate) fn complete_omnibox(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        if rows.is_empty() {
            return;
        }
        let index = self.browse_omnibox_selected.min(rows.len() - 1);
        let row = rows[index].clone();
        let Some(text) = row.suggestion.complete.clone() else {
            return;
        };
        if let Some(input) = self.browse_address.clone() {
            self.browse_completed_text = Some(text.clone());
            input.update(cx, |input, cx| {
                input.set_value(text, window, cx);
                input.select_all(window, cx);
                input.focus(window, cx);
            });
        }
        // The input's own Change arm resets the highlight (keeping the
        // completion, which survives the re-rank); the completed row keeps
        // the highlight too, and its identity owns Enter.
        self.browse_omnibox_selected = index;
        self.browse_omnibox_moved = true;
        self.browse_completed = Some(row.target.clone());
        cx.notify();
    }

    /// Activate an omnibox row (Enter opens): Ask asks, a person tab
    /// selects in Browse (clearing any carried tab), a session tab opens
    /// its session in Sessions (updating the sidebar selection there), a
    /// session opens in Sessions, and history/search URLs load in the
    /// shown tab. The index clamps to the rows, so a stale highlight can
    /// never miss into nothing.
    pub(crate) fn activate_omnibox_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let rows = self.browse_suggestions(cx);
        let index = index.min(rows.len().saturating_sub(1));
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        self.activate_omnibox_target(row.target, window, cx);
    }

    /// Activate an omnibox row by identity (a Tab completion's remembered
    /// row): the same destinations as [`Self::activate_omnibox_row`],
    /// without re-resolving an index into a re-ranked list.
    pub(crate) fn activate_omnibox_target(
        &mut self,
        target: OmniTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            OmniTarget::Ask(query) => self.browse_ask(&query, window, cx),
            OmniTarget::Tab(id) => {
                let owner = self.browser.tabs.get(id).map(|tab| tab.owner.clone());
                match owner {
                    Some(Owner::Person) => self.select_browse_sidebar_tab(id, window, cx),
                    Some(Owner::Session(session)) => {
                        self.leave_browse_address(window, cx);
                        // A mouse-down already opened it: the click that
                        // follows is a no-op, never a second open.
                        if self.active_id(cx).as_deref() != Some(session.as_str()) {
                            self.set_mode(crate::layout::Mode::Sessions, window, cx);
                            self.resume(session, window, cx);
                        }
                    }
                    None => {}
                }
            }
            OmniTarget::Session(session) => {
                self.leave_browse_address(window, cx);
                // A mouse-down already opened it: the click that follows
                // is a no-op, never a second open.
                if self.active_id(cx).as_deref() != Some(session.as_str()) {
                    self.set_mode(crate::layout::Mode::Sessions, window, cx);
                    self.resume(session, window, cx);
                }
            }
            OmniTarget::History(url) | OmniTarget::Search(url) => {
                self.leave_browse_address(window, cx);
                self.ensure_tabs_loaded(cx);
                if let Some(id) = self.browse_centre_shown() {
                    self.navigate_browse_tab(id, &url, window, cx);
                }
            }
        }
    }

    /// The new-tab page's pick-up rows (B2a-H4) with what each opens: this
    /// project's open tabs (blank ones excluded — the current empty tab is
    /// not somewhere to pick up), then its recent sessions, then recently
    /// closed tabs — capped at 5.
    pub(crate) fn browse_pickup_full(&self) -> Vec<(toolbar::PickupRow, PickupTarget)> {
        let space = self.current_project_id();
        let project_name = self.current_project().map(|project| project.name.clone()).unwrap_or_default();
        let tab_items: Vec<(TabId, String, String)> = if self.browser.tabs_loaded {
            self.browser
                .tabs
                .person_tabs(space.as_deref())
                .iter()
                .filter(|tab| !tab.url.is_empty() && tab.url != BLANK)
                .map(|tab| {
                    let label = tabs::tab_label(&tab.title, &tab.url);
                    (tab.id, if label.is_empty() { tab.url.clone() } else { label }, project_name.clone())
                })
                .collect()
        } else {
            Vec::new()
        };
        let current = self.current_project_id();
        let session_items: Vec<(String, String, String)> = self
            .sessions
            .iter()
            .filter(|entry| !entry.archived && !entry.hidden && entry.project == current)
            .take(5)
            .map(|entry| (entry.id.clone(), entry.label.clone(), project_name.clone()))
            .collect();
        // Recently closed tabs are this project's person tabs only — another
        // project's tabs never offer here — newest first, and clickable:
        // each row reopens exactly the tab it names.
        let now = tabs::now_ms();
        let closed_items: Vec<(TabId, String, String)> = self
            .browser
            .tabs
            .closed
            .iter()
            .rev()
            .filter(|tab| tab.owner == Owner::Person && tab.space == space)
            .take(5)
            .map(|tab| {
                let ago = tab
                    .last_shown
                    .map(|shown| toolbar::relative_ago(now.saturating_sub(shown)))
                    .unwrap_or_else(|| "a while ago".to_owned());
                let label = tabs::tab_label(&tab.title, &tab.url);
                (tab.id, if label.is_empty() { tab.url.clone() } else { label }, ago)
            })
            .collect();
        let tab_pairs: Vec<(String, String)> =
            tab_items.iter().map(|(_, title, project)| (title.clone(), project.clone())).collect();
        let session_pairs: Vec<(String, String)> =
            session_items.iter().map(|(_, label, project)| (label.clone(), project.clone())).collect();
        let closed_pairs: Vec<(String, String)> =
            closed_items.iter().map(|(_, title, ago)| (title.clone(), ago.clone())).collect();
        let display = toolbar::pickup_rows(&tab_pairs, &session_pairs, &closed_pairs, 5);
        let mut targets = Vec::new();
        for (id, _, _) in &tab_items {
            targets.push(PickupTarget::Tab(*id));
        }
        for (id, _, _) in &session_items {
            targets.push(PickupTarget::Session(id.clone()));
        }
        for (id, _, _) in &closed_items {
            targets.push(PickupTarget::Closed(*id));
        }
        targets.truncate(5);
        display.into_iter().zip(targets).collect()
    }

    /// Reopen the closed tab `id` from a new-tab pick-up row and select it:
    /// selecting clears any carried session tab, so the reopened person tab
    /// shows. A no-op for an unknown id.
    pub(crate) fn reopen_browse_closed_tab(
        &mut self,
        id: TabId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.reopen(id).is_some() {
            self.select_browse_sidebar_tab(id, window, cx);
        }
    }

    /// `omnibox:<text>`: focus the Browse address field and type `text` —
    /// the `browse-omnibox` capture aid. The dropdown stays open on the
    /// typed text. Free: no turn, no wire.
    pub(crate) fn step_omnibox(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.ensure_tabs_loaded(cx);
        if self.browse_centre_page(window, cx).is_none() {
            crate::steps::record_step_failure(&format!("omnibox:{rest}"));
            return;
        }
        self.focus_browse_address_with(rest, window, cx);
    }

    /// `find:<query>`: open the find strip on the shown tab with `query`
    /// typed (a scripted "3 of 12" result lands through the poll path, so
    /// the `browse-find` capture shows the count). Empty records a step
    /// failure. Free: no turn, no wire.
    pub(crate) fn step_find(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let query = rest.trim();
        if query.is_empty() {
            crate::steps::record_step_failure(&format!("find:{rest}"));
            return;
        }
        self.browser.fake_scripts.push(chrome::FakePageScript::Find { query: query.to_owned(), active: 3, total: 12 });
        if !self.rebuild_shown_browse_page(window, cx) {
            crate::steps::record_step_failure(&format!("find:{rest}"));
            return;
        }
        self.open_browse_find(window, cx);
        if let Some(input) = self.browse_find.clone() {
            let query = query.to_owned();
            input.update(cx, |input, cx| input.set_value(query, window, cx));
        }
        self.find_input_changed(cx);
    }

    /// `permission:<kinds>`: script a permission ask (`camera,microphone`)
    /// on the shown tab, so the `browse-permission` capture shows the
    /// strip. Unknown kinds record a step failure. Free.
    pub(crate) fn step_permission(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let Some(kinds) = chrome::parse_permission_kinds(rest) else {
            crate::steps::record_step_failure(&format!("permission:{rest}"));
            return;
        };
        self.browser.fake_scripts.push(chrome::FakePageScript::Permission {
            id: 7,
            origin: "https://example.com".to_owned(),
            kinds,
        });
        if !self.rebuild_shown_browse_page(window, cx) {
            crate::steps::record_step_failure(&format!("permission:{rest}"));
        }
    }

    /// `download:<name>`: script an active download (`done:<name>` lands
    /// it) on the shown tab and open the popover, so the
    /// `browse-downloads` capture shows the list. Empty records a step
    /// failure. Free.
    pub(crate) fn step_download(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let (done, name) = match rest.trim().strip_prefix("done:") {
            Some(done) => (true, done.trim()),
            None => (false, rest.trim()),
        };
        if name.is_empty() {
            crate::steps::record_step_failure(&format!("download:{rest}"));
            return;
        }
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_owned());
        self.browser.fake_scripts.push(chrome::FakePageScript::Download(aui_webview::DownloadState {
            id: 3,
            url: format!("https://cdn.example/{name}"),
            file_name: name.to_owned(),
            path: format!("{home}/Downloads/{name}"),
            received: if done { 10_485_760 } else { 4_400_000 },
            total: Some(10_485_760),
            state: if done { aui_webview::DownloadStatus::Complete } else { aui_webview::DownloadStatus::Downloading },
        }));
        if !self.rebuild_shown_browse_page(window, cx) {
            crate::steps::record_step_failure(&format!("download:{rest}"));
            return;
        }
        self.browser.downloads_open = true;
        self.browser.downloads_rendered = false;
        self.browser.extensions_open = false;
        self.browser.more_open = false;
        cx.notify();
    }

    /// `crashed:<reason>`: script a renderer crash on the shown tab, so
    /// the `browse-crashed` capture shows the Baaz error page. Free.
    pub(crate) fn step_crashed(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        let reason = rest.trim();
        let reason = if reason.is_empty() { "the page crashed (-1)" } else { reason };
        self.browser.fake_scripts.push(chrome::FakePageScript::Crashed(reason.to_owned()));
        if !self.rebuild_shown_browse_page(window, cx) {
            crate::steps::record_step_failure(&format!("crashed:{rest}"));
        }
    }

    /// `more-menu`: open the ⋯ page-actions menu on the shown tab, so the
    /// `browse-more-menu` capture shows it. A no-op with no shown tab
    /// (records a step failure). Free.
    pub(crate) fn step_more_menu(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if self.browse_centre_page(window, cx).is_none() {
            crate::steps::record_step_failure(&format!("more-menu:{rest}"));
            return;
        }
        self.browser.more_open = true;
        self.browser.more_move_open = false;
        self.browser.downloads_open = false;
        self.browser.extensions_open = false;
        cx.notify();
    }

    /// `extensions`: open the extensions popover on the shown tab. A no-op
    /// with no shown tab (records a step failure). Free.
    pub(crate) fn step_extensions(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        if self.browse_centre_page(window, cx).is_none() {
            crate::steps::record_step_failure(&format!("extensions:{rest}"));
            return;
        }
        self.browser.extensions_open = true;
        self.browser.downloads_open = false;
        self.browser.more_open = false;
        cx.notify();
    }

    /// `fixture-session:<name>`: park a local session row plus one session
    /// tab carrying `https://example.com`, and carry it into Browse — the
    /// offline `browse-from-session` capture aid. Free: no turn, no wire.
    pub(crate) fn step_fixture_session(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let name = rest.trim();
        let name = if name.is_empty() { "Fix ledger chart" } else { name };
        let id = format!("fixture-{}", name.to_lowercase().replace(' ', "-"));
        self.ensure_tabs_loaded(cx);
        if !self.sessions.iter().any(|entry| entry.id == id) {
            let now = chrono::Local::now();
            let space = self.current_project_id();
            let workspace =
                self.current_project().map(|project| project.root.to_string_lossy().into_owned());
            self.sessions.push(crate::sidebar::local_started_row(&id, name.to_owned(), space.clone(), workspace, now));
            self.invalidate_list();
        }
        let space = self.current_project_id();
        let tab_id = self.browser.tabs.open(space, Owner::Session(id), "https://example.com", None);
        self.schedule_tabs_write(cx);
        self.ensure_tab_page(HOME_KEY, tab_id, window, cx);
        // `set_mode` clears the park, so the fixture parks after the switch.
        self.set_mode(crate::layout::Mode::Browse, window, cx);
        self.browser.centre_override = Some(tab_id);
        self.browse_session_tab = None;
        cx.notify();
    }

    /// `page-focus`: give the active browser page the keyboard, focusing the
    /// pane's own focus handle — the same hand-over an explicit person open
    /// performs. Free: no turn, no wire.
    pub(crate) fn step_page_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // In Browse the active page is the centre's, not the pane's.
        if self.mode == crate::layout::Mode::Browse {
            self.ensure_browse_tab(window, cx);
            if let Some(state) = self.browse_centre_page(window, cx) {
                let focus = state.read(cx).focus_handle().clone();
                window.focus(&focus, cx);
            }
            return;
        }
        let key = self.browser_key(cx);
        let state = self.browser_for(&key, false, window, cx);
        let focus = state.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        if state.read(cx).holds_keyboard() {
            crate::baaz_log!("browser: page focused");
        } else {
            crate::baaz_log!("browser: page-focus: page did not take the keyboard");
        }
    }

    /// Deliver one keystroke routed out of a focused Chromium page: first
    /// hand the keyboard back to the gpui window through the existing
    /// hand-back path (the active browser's `release_keyboard`), then
    /// dispatch the keystroke into the window — so ⌘L ends with the address
    /// field focused, exactly as if gpui had seen the key. Blocked Chromium
    /// command ids drained here are logged for later mapping.
    ///
    /// Not behind the `cef` feature alone: the delivery (hand-back plus
    /// deferred dispatch) is plain gpui, and the regression test drives it
    /// without a page. Only the blocked-command drain needs CEF.
    #[cfg(any(test, feature = "cef"))]
    pub(crate) fn deliver_cef_keystroke(
        &mut self,
        keystroke: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.existing_browser(cx) {
            state.update(cx, |state, _| state.release_keyboard());
        }
        // In Browse the keyboard may sit with the centre page instead.
        if let Some(state) = self.browse_centre_state() {
            state.update(cx, |state, _| state.release_keyboard());
        }
        #[cfg(feature = "cef")]
        for id in aui_cef::take_blocked_commands() {
            crate::baaz_log!("browser: blocked Chromium command {id}");
        }
        match gpui::Keystroke::parse(keystroke) {
            Ok(parsed) => {
                crate::baaz_log!("browser: page routed {keystroke}");
                // Deferred, never inline: `dispatch_keystroke` runs action
                // listeners synchronously, and every Harness listener
                // re-enters Harness — dispatching here would nest an update
                // inside this one and panic ("already being updated").
                // `Window::defer` runs the dispatch after this update's
                // lease on Harness has ended.
                // And only once AppKit has made the gpui window key again:
                // handing the keyboard back is asynchronous, and a dispatch
                // that lands first has its focus (⌘L's address field) reset
                // when the window's key change arrives (B1b, live: the second
                // ⌘L after clicking the page opened nothing).
                cx.spawn_in(window, async move |this, cx| {
                    for _ in 0..20 {
                        let key = cx
                            .update(|window, _| window.is_window_active())
                            .unwrap_or(true);
                        if key {
                            break;
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(10))
                            .await;
                    }
                    // With gpui focus stale (the address field unmounted when
                    // the page took the keyboard) a keystroke reaches no key
                    // context and no binding: the second ⌘L after clicking the
                    // page did nothing (B1b, live). Focus the root first so
                    // `AuiRoot` bindings resolve. The handle is cloned out
                    // first: dispatching inside a Harness update re-enters it.
                    let root = this.update(cx, |this, _| this.focus_root.clone()).ok();
                    let _ = cx.update(|window, cx| {
                        // Unconditionally: the keystroke came from a page, so
                        // whatever gpui still calls focused is stale — often the
                        // unmounted address input, which `focused()` reports
                        // but no rendered node carries, so it matched nothing.
                        if let Some(root) = &root {
                            window.focus(root, cx);
                        }
                        window.dispatch_keystroke(parsed, cx);
                    });
                })
                .detach();
            }
            Err(_) => {
                crate::baaz_log!("browser: ignoring unparseable routed keystroke {keystroke:?}");
            }
        }
    }

    /// Install the Chromium key router for this window: `claims` is membership
    /// in the current claimed-set snapshot (a thread-local, never gpui state,
    /// which the CEF callback must not borrow) and `deliver` posts into a
    /// channel that one task drains through [`Self::deliver_cef_keystroke`].
    /// Still called where each shell window is created (`open_shell_window`),
    /// never from `cef_backend` — the router must exist before the first page
    /// takes the keyboard — but installed once per process: a second window
    /// re-installing would let its close strand every claimed key. Delivery
    /// resolves the window per keystroke (the active shell window), so the
    /// installing window closing never strands the rest.
    #[cfg(feature = "cef")]
    pub(crate) fn install_cef_key_router(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        static INSTALLED: AtomicBool = AtomicBool::new(false);
        if INSTALLED.swap(true, Ordering::SeqCst) {
            return;
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        aui_cef::set_key_router(crate::keymap::claims_keystroke, move |keystroke| {
            let _ = tx.unbounded_send(keystroke);
        });
        // One task awaiting the channel — no polling timer, no idle wakeups.
        // A `spawn`, not a `spawn_in`: the drain resolves the window on every
        // keystroke instead of pinning the installing one.
        let task = cx.spawn(async move |weak, cx| {
            use futures::StreamExt as _;
            while let Some(keystroke) = rx.next().await {
                let Some(harness) = weak.upgrade() else {
                    crate::baaz_log!(
                        "browser: routed {keystroke} after the window closed; dropping"
                    );
                    break;
                };
                cx.update(|cx| {
                    let Some(target) = cx.active_window() else {
                        crate::baaz_log!(
                            "browser: routed {keystroke} with no live window; dropping"
                        );
                        return;
                    };
                    let _ = target.update(cx, |_, window, cx| {
                        harness.update(cx, |this, cx| {
                            this.deliver_cef_keystroke(&keystroke, window, cx);
                        });
                    });
                });
            }
        });
        self.tasks.push(task);
    }
}

/// What a pick-up row opens: a tab selects, a session opens, and a
/// closed tab reopens exactly the tab the row names.
pub(crate) enum PickupTarget {
    /// Select this person tab.
    Tab(TabId),
    /// Open this session.
    Session(String),
    /// Reopen this closed tab.
    Closed(TabId),
}

/// Where an omnibox dropdown anchors: under the toolbar field (floating
/// over the page top), or in flow under the new-tab page's centred field.
pub(crate) enum OmniAnchor {
    /// The toolbar field's list: absolute, under the toolbar.
    Toolbar,
    /// The new-tab field's list: in flow, under the centred field.
    Newtab,
}

/// All webviews a [`Harness`](crate::app::Harness) holds. Kept here so the
/// registry's shape lives beside the logic that drives it.
pub(crate) struct BrowserRegistry {
    /// One live state per open tab, keyed by tab. Hidden on switch, never
    /// destroyed — except by discard, which drops the state and keeps the
    /// tab's URL for a rebuild on show.
    pub states: HashMap<TabId, Entity<WebviewState>>,
    /// Set at boot: scripted page (`--screenshot`, tests) vs WKWebView.
    pub fake: bool,
    /// An external file drag currently covers the window.
    pub drop_cover: bool,
    /// Sessions whose Screenshot intent still waits for bytes.
    pub shot_pending: HashSet<String>,
    /// The tab model: tabs per session and per project, persisted.
    pub tabs: Tabs,
    /// Whether [`Tabs`] has been loaded and migrated yet. Lazy: the
    /// registry is built before `sessions.json` is read, so the first
    /// browser use loads.
    pub tabs_loaded: bool,
    /// The debounced `tabs.json` write's generation; only the latest fires.
    pub tabs_save_epoch: u64,
    /// Whether a `tabs.json` write is armed.
    pub tabs_save_pending: bool,
    /// The tab the agent tools act on per session key, so discard never
    /// drops the agent's active tab and switches re-register it.
    pub agent_tabs: HashMap<String, TabId>,
    /// The strip tab holding a middle press: set on middle mouse down,
    /// compared on middle mouse up (only the same tab closes), cleared on
    /// every middle up. Never rendered — pure press/release pairing.
    pub middle_pressed: Option<TabId>,
    /// The Browse centre page's rect, stored by `sync_browser` every
    /// frame: the mouse-down hand-back measures against this same rect
    /// (see `browse_centre_page_rect`), never a second computation.
    /// `None` outside Browse and before the first sync.
    pub centre_page_rect: Option<Bounds<Pixels>>,
    /// The session tab carried into Browse full-width (B2a-H4): the
    /// pane's "Browse" button parks the session's active tab here and the
    /// centre shows it — with the ↩ chip — instead of the project's
    /// person tab. The tab stays session-owned; the chip clears this and
    /// hands the same `WebviewState` back to the session's pane.
    pub centre_override: Option<TabId>,
    /// One back/forward stack per tab (B2a-H4): the header toolbar tracks
    /// the URLs the sync reports, since the library exposes no back,
    /// forward or reload calls to drive.
    pub histories: HashMap<TabId, BackForward>,
    /// Every tab's find strip (B2a-H5): per tab, so switching hides the
    /// strip and returning restores the query.
    pub find_strips: chrome::FindStrips,
    /// Origins trusted "while visiting" (B2a-H5): in memory for this run
    /// only — persistence is B2b.
    pub visit_allows: chrome::VisitAllows,
    /// Each live tab's last seen origin: navigating to another origin (or
    /// closing) revokes that origin's "while visiting" grants.
    pub permission_origins: HashMap<TabId, String>,
    /// Which Browse chrome popover stands open (B2a-H5): downloads (the
    /// toolbar button exists only while downloads list), extensions, ⋯.
    /// They drop over the page; see [`BrowserVisibility`].
    pub downloads_open: bool,
    /// The extensions popover stands open.
    pub extensions_open: bool,
    /// Whether the downloads popover has rendered since it opened: an
    /// emptied list clears the flag only after it rendered once, so a
    /// scripted download's first frame never closes its own capture.
    pub downloads_rendered: bool,
    /// The ⋯ page-actions menu stands open.
    pub more_open: bool,
    /// The ⋯ menu's inline "Move tab to project" list, expanded or not.
    pub more_move_open: bool,
    /// Scripted fake-backend events, queued by `--steps` verbs and consumed
    /// when the next scripted page builds (captures only; production never
    /// queues). The events drain through the real poll path, so captures
    /// show the same strips a live page would.
    pub fake_scripts: Vec<chrome::FakePageScript>,
    /// When each live download started arriving, as ms since the epoch
    /// ([`tabs::now_ms`]): the popover's "6 s left" estimates read this.
    /// Stamped on first sight, pruned when the download ends.
    pub download_started: HashMap<(TabId, u32), u64>,
    /// The centre tab the last sync saw: a switch dismisses the chrome
    /// popovers (they point at the old tab's state) but never the find
    /// strip, which is per tab and restores on return.
    pub last_centre_shown: Option<TabId>,
}

impl BrowserRegistry {
    /// Empty: no tab and no webview exists until the Browser kind first
    /// shows. Tabs load lazily on first use (see `ensure_tabs_loaded`).
    pub(crate) fn new(fake: bool) -> Self {
        Self {
            states: HashMap::new(),
            fake,
            drop_cover: false,
            shot_pending: HashSet::new(),
            tabs: Tabs::default(),
            tabs_loaded: false,
            tabs_save_epoch: 0,
            tabs_save_pending: false,
            agent_tabs: HashMap::new(),
            middle_pressed: None,
            centre_page_rect: None,
            centre_override: None,
            histories: HashMap::new(),
            find_strips: chrome::FindStrips::default(),
            visit_allows: chrome::VisitAllows::default(),
            permission_origins: HashMap::new(),
            downloads_open: false,
            extensions_open: false,
            downloads_rendered: false,
            more_open: false,
            more_move_open: false,
            fake_scripts: Vec::new(),
            download_started: HashMap::new(),
            last_centre_shown: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        annotations_draft_block, browser_centre_bounds, browser_page_bounds, browser_pane_showing,
        browser_session_active, browser_visible, initial_url, middle_release_closes,
        should_release_keyboard_on_mouse_down, should_remember, shown_window_active,
        terminal_covers_browser, terminal_dock_bounds, Annotation, BrowserVisibility, FakeWebBackend,
        WebviewState,
    };
    use super::{Owner, TabId};
    use aui_webview::WebBackend as _;
    use std::collections::HashMap;

    fn all_clear() -> BrowserVisibility {
        BrowserVisibility {
            pane_open: true,
            kind_browser: true,
            centre_browse: false,
            session_active: true,
            settings_open: false,
            palette_open: false,
            menu_open: false,
            plus_open: false,
            account_menu_open: false,
            dialog_open: false,
            drop_cover: false,
            terminal_covering: false,
            window_active: true,
            omnibox_open: false,
            chrome_popover_open: false,
            page_error_open: false,
        }
    }

    /// Browse's centre page, all clear: no open pane and no Browser kind
    /// needed — the centre flag carries the showing half on its own.
    fn centre_clear() -> BrowserVisibility {
        BrowserVisibility {
            pane_open: false,
            kind_browser: false,
            centre_browse: true,
            session_active: true,
            settings_open: false,
            palette_open: false,
            menu_open: false,
            plus_open: false,
            account_menu_open: false,
            dialog_open: false,
            drop_cover: false,
            terminal_covering: false,
            window_active: true,
            omnibox_open: false,
            chrome_popover_open: false,
            page_error_open: false,
        }
    }

    #[test]
    fn native_view_shows_only_with_pane_open_on_active_browser() {
        assert!(browser_visible(all_clear()));
    }

    #[test]
    fn browse_centre_shows_with_no_pane_and_no_browser_kind() {
        assert!(
            browser_visible(centre_clear()),
            "the centre page needs no open pane and no Browser kind"
        );
    }

    #[test]
    fn browse_centre_hides_without_a_shown_tab() {
        let mut inputs = centre_clear();
        inputs.session_active = false;
        assert!(
            !browser_visible(inputs),
            "no shown tab, no centre page — like the pane's inactive session"
        );
    }

    #[test]
    fn every_cover_hides_the_browse_centre_too() {
        // The palette, the settings page and a dead window obscure the
        // centre page exactly as they obscure the pane's: the same
        // covering rules, not a second set.
        let mut palette = centre_clear();
        palette.palette_open = true;
        assert!(!browser_visible(palette), "the palette obscures the centre page");
        let mut settings = centre_clear();
        settings.settings_open = true;
        assert!(!browser_visible(settings), "settings obscure the centre page");
        let mut dead = centre_clear();
        dead.window_active = false;
        assert!(!browser_visible(dead), "a dead window hides the centre page");
    }

    #[test]
    fn open_omnibox_obscures_pane_and_centre() {
        // The suggestion list drops down over the page, so while it is open
        // the page must be obscured (its screenshot stand-in, as with the
        // palette) — on the pane and on the Browse centre alike.
        let mut pane = all_clear();
        pane.omnibox_open = true;
        assert!(!browser_visible(pane), "the omnibox obscures the pane's page");
        let mut centre = centre_clear();
        centre.omnibox_open = true;
        assert!(!browser_visible(centre), "the omnibox obscures the centre page");
    }

    #[test]
    fn chrome_popovers_and_error_pages_obscure_pane_and_centre() {
        // The downloads/extensions/⋯ popovers drop over the page, and the
        // Baaz-drawn crashed page covers its rect: both hide the native
        // view on the pane and on the Browse centre alike.
        let mut popover = all_clear();
        popover.chrome_popover_open = true;
        assert!(!browser_visible(popover), "a chrome popover obscures the pane's page");
        let mut centre_popover = centre_clear();
        centre_popover.chrome_popover_open = true;
        assert!(!browser_visible(centre_popover), "a chrome popover obscures the centre page");
        let mut error = all_clear();
        error.page_error_open = true;
        assert!(!browser_visible(error), "the crashed page covers the pane's page");
        let mut centre_error = centre_clear();
        centre_error.page_error_open = true;
        assert!(!browser_visible(centre_error), "the crashed page covers the centre page");
    }

    #[test]
    fn sessions_visibility_needs_no_centre_flag() {
        // Sessions mode leaves the flag false, so its visibility reads
        // exactly as before: closed pane hides, whatever the flag's
        // absence says.
        let mut inputs = all_clear();
        inputs.pane_open = false;
        assert!(!browser_visible(inputs), "a closed pane still hides the page");
        assert!(!inputs.centre_browse, "sessions never sets the centre flag");
    }

    #[test]
    fn centre_bounds_fill_between_sidebar_right_and_dock() {
        // A 1440x900 window, sidebar open at 260, right pane at 400, dock
        // at 260: the centre page is what is left under the 44 px header.
        let content = gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(1440.0), gpui::px(900.0)),
        };
        let page = browser_centre_bounds(
            content,
            gpui::px(260.0),
            gpui::px(400.0),
            gpui::px(260.0),
        );
        assert_eq!(page.origin.x, gpui::px(260.0));
        assert_eq!(page.origin.y, gpui::px(44.0));
        assert_eq!(page.size.width, gpui::px(780.0));
        assert_eq!(page.size.height, gpui::px(900.0 - 44.0 - 260.0));
        // Collapsed chrome is zero, not absent: the full content width.
        let full = browser_centre_bounds(
            content,
            gpui::px(0.0),
            gpui::px(0.0),
            gpui::px(0.0),
        );
        assert_eq!(full.origin.x, gpui::px(0.0));
        assert_eq!(full.size.width, gpui::px(1440.0));
        assert_eq!(full.size.height, gpui::px(900.0 - 44.0));
    }

    #[test]
    fn only_the_shown_page_keeps_the_window_active() {
        let shown = TabId(1);
        let hidden = TabId(2);
        let mut holds = HashMap::new();
        holds.insert(hidden, true);
        // A hidden page holding the keyboard never keeps the window
        // "active": only the shown tab's page counts.
        assert!(!shown_window_active(false, Some(shown), &holds));
        assert!(
            !shown_window_active(false, None, &holds),
            "nothing shown, nothing counts"
        );
        holds.insert(shown, true);
        assert!(
            shown_window_active(false, Some(shown), &holds),
            "the shown page still carries B1b"
        );
        assert!(!shown_window_active(false, Some(shown), &HashMap::new()));
        assert!(
            shown_window_active(true, None, &HashMap::new()),
            "an active window needs no page"
        );
    }

    #[test]
    fn middle_release_closes_only_its_own_press() {
        let pressed = TabId(7);
        let other = TabId(8);
        assert!(
            middle_release_closes(Some(pressed), pressed),
            "press and release on the same tab close"
        );
        assert!(
            !middle_release_closes(Some(pressed), other),
            "a release on another tab closes nothing"
        );
        assert!(
            !middle_release_closes(None, pressed),
            "a press-less release closes nothing"
        );
    }

    /// One truth-table row: which input a case flips off (or on, for covers).
    type VisibilityFlip = fn(BrowserVisibility) -> BrowserVisibility;

    #[test]
    fn every_covering_input_hides_the_native_view() {
        let cases: [(&str, VisibilityFlip); 12] = [
            ("closed pane", |mut inputs| {
                inputs.pane_open = false;
                inputs
            }),
            ("other kind", |mut inputs| {
                inputs.kind_browser = false;
                inputs
            }),
            ("inactive session", |mut inputs| {
                inputs.session_active = false;
                inputs
            }),
            ("settings", |mut inputs| {
                inputs.settings_open = true;
                inputs
            }),
            ("palette", |mut inputs| {
                inputs.palette_open = true;
                inputs
            }),
            ("menu", |mut inputs| {
                inputs.menu_open = true;
                inputs
            }),
            ("plus menu", |mut inputs| {
                inputs.plus_open = true;
                inputs
            }),
            ("account menu", |mut inputs| {
                inputs.account_menu_open = true;
                inputs
            }),
            ("dialog", |mut inputs| {
                inputs.dialog_open = true;
                inputs
            }),
            ("drop overlay", |mut inputs| {
                inputs.drop_cover = true;
                inputs
            }),
            ("covering dock", |mut inputs| {
                // The input now means "intersects", not "open": a
                // full-width dock over the pane still hides the page.
                inputs.terminal_covering = super::terminal_covers_browser(
                    true,
                    Some(full_width_dock_rect()),
                    Some(overlapping_pane_rect()),
                );
                inputs
            }),
            ("dead window", |mut inputs| {
                inputs.window_active = false;
                inputs
            }),
        ];
        for (name, flip) in cases {
            assert!(
                !browser_visible(flip(all_clear())),
                "{name} must obscure the native view"
            );
        }
    }

    /// Laid-out rects for the geometry tests on a 1440x900 window: the
    /// centre-column dock beside the right-edge pane.
    fn dock_rect() -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds {
            origin: gpui::point(gpui::px(260.0), gpui::px(640.0)),
            size: gpui::size(gpui::px(780.0), gpui::px(260.0)),
        }
    }

    /// Today's layout: the pane hangs off the right edge, beside the dock.
    fn beside_pane_rect() -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds {
            origin: gpui::point(gpui::px(1040.0), gpui::px(44.0)),
            size: gpui::size(gpui::px(400.0), gpui::px(856.0)),
        }
    }

    /// A future full-width dock runs under the pane.
    fn full_width_dock_rect() -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(640.0)),
            size: gpui::size(gpui::px(1440.0), gpui::px(260.0)),
        }
    }

    /// The pane a full-width dock runs under.
    fn overlapping_pane_rect() -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds {
            origin: gpui::point(gpui::px(1040.0), gpui::px(44.0)),
            size: gpui::size(gpui::px(400.0), gpui::px(700.0)),
        }
    }

    #[test]
    fn open_dock_beside_the_pane_leaves_the_page_live() {
        // The dock stands open in the centre column while the pane hangs
        // off the right edge. Their rects never meet, so the page stays
        // live: dock open + pane open, no overlap → visible.
        let covering = terminal_covers_browser(true, Some(dock_rect()), Some(beside_pane_rect()));
        assert!(
            !covering,
            "an open centre-column dock never covers the right pane"
        );
        let mut inputs = all_clear();
        inputs.terminal_covering = covering;
        assert!(
            browser_visible(inputs),
            "dock open + pane open, no overlap → the native view shows"
        );
    }

    #[test]
    fn full_width_dock_still_covers_the_page() {
        assert!(
            terminal_covers_browser(
                true,
                Some(full_width_dock_rect()),
                Some(overlapping_pane_rect())
            ),
            "a dock under the pane keeps the input meaningful"
        );
        assert!(
            !terminal_covers_browser(
                false,
                Some(full_width_dock_rect()),
                Some(overlapping_pane_rect())
            ),
            "a closed dock covers nothing even where it would overlap"
        );
        assert!(
            !terminal_covers_browser(true, None, Some(overlapping_pane_rect())),
            "no laid-out dock rect is no cover"
        );
    }

    #[test]
    fn derived_centre_and_right_rects_never_share_pixels() {
        // The sync path derives both rects from the content box: the dock
        // fills the centre run (sidebar 260, pane 400 of a 1440 window),
        // the pane hangs off the right edge under the 44 px header.
        let content = gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(1440.0), gpui::px(900.0)),
        };
        let pane = browser_page_bounds(content, gpui::px(400.0));
        assert_eq!(f32::from(pane.origin.x), 1040.0);
        assert_eq!(f32::from(pane.origin.y), 44.0);
        assert_eq!(f32::from(pane.size.width), 400.0);
        let dock =
            terminal_dock_bounds(content, gpui::px(260.0), gpui::px(1040.0), gpui::px(260.0));
        assert_eq!(f32::from(dock.origin.y), 640.0);
        assert!(
            !terminal_covers_browser(true, Some(dock), Some(pane)),
            "centre dock vs right pane: structurally disjoint"
        );
    }

    #[test]
    fn outside_click_releases_only_while_the_page_holds_the_keyboard() {
        let outside = gpui::point(gpui::px(100.0), gpui::px(100.0));
        let inside = gpui::point(gpui::px(1200.0), gpui::px(200.0));
        assert!(beside_pane_rect().contains(&inside));
        assert!(!beside_pane_rect().contains(&outside));
        assert!(should_release_keyboard_on_mouse_down(
            true,
            outside,
            Some(beside_pane_rect())
        ));
        assert!(!should_release_keyboard_on_mouse_down(
            true,
            inside,
            Some(beside_pane_rect())
        ));
        assert!(!should_release_keyboard_on_mouse_down(
            false,
            outside,
            Some(beside_pane_rect())
        ));
        assert!(!should_release_keyboard_on_mouse_down(true, outside, None));
    }

    /// A fake-backed page holding the keyboard, the way a click in the
    /// page leaves it.
    fn focused_state(cx: &mut gpui::TestAppContext) -> gpui::Entity<WebviewState> {
        use gpui::AppContext as _;
        let mut backend = FakeWebBackend::new();
        backend.point_clicked((1.0, 1.0));
        cx.new(|cx| WebviewState::new(Box::new(backend), cx))
    }

    /// The capture releases a real (fake-backed) page: outside the rect
    /// the keyboard comes back, inside it the page keeps typing.
    #[gpui::test]
    fn outside_click_hands_the_fake_pages_keyboard_back(cx: &mut gpui::TestAppContext) {
        let outside = gpui::point(gpui::px(100.0), gpui::px(100.0));
        let state = focused_state(cx);
        assert!(state.update(cx, |state, _| state.holds_keyboard()));
        let released = state.update(cx, |state, _| {
            if should_release_keyboard_on_mouse_down(
                state.holds_keyboard(),
                outside,
                Some(beside_pane_rect()),
            ) {
                state.release_keyboard();
                true
            } else {
                false
            }
        });
        assert!(released, "an outside click while holding releases");
        assert!(!state.update(cx, |state, _| state.holds_keyboard()));

        let inside = gpui::point(gpui::px(1200.0), gpui::px(200.0));
        let state = focused_state(cx);
        let released = state.update(cx, |state, _| {
            if should_release_keyboard_on_mouse_down(
                state.holds_keyboard(),
                inside,
                Some(beside_pane_rect()),
            ) {
                state.release_keyboard();
                true
            } else {
                false
            }
        });
        assert!(!released, "a click in the page never releases");
        assert!(state.update(cx, |state, _| state.holds_keyboard()));
    }

    #[test]
    fn cmd_l_routes_to_the_browser_only_while_it_shows() {
        assert!(browser_pane_showing(true, true));
        assert!(!browser_pane_showing(false, true), "a closed pane eats ⌘L");
        assert!(!browser_pane_showing(true, false), "another kind eats ⌘L");
    }

    #[test]
    fn pending_switch_hides_every_session_page() {
        assert!(
            browser_session_active("a", "a", false),
            "the active session's page shows outside a switch"
        );
        assert!(
            !browser_session_active("b", "a", false),
            "a parked session's page never shows"
        );
        assert!(
            !browser_session_active("a", "a", true),
            "no page shows under the pending loading centre — not even the still-active session's"
        );
        assert!(
            !browser_session_active("b", "a", true),
            "nor any other session's"
        );
    }

    #[test]
    fn send_annotations_builds_the_expected_draft_text() {
        let pins = vec![
            Annotation::new(1, "div.card.starter", "Price is wrong"),
            Annotation::new(2, "p.lede", "Trim this"),
        ];
        assert_eq!(
            annotations_draft_block(&pins, "https://example.com"),
            "Browser annotations — https://example.com\n1. div.card.starter — Price is wrong\n2. p.lede — Trim this",
        );
    }

    #[test]
    fn send_annotations_without_pins_is_still_labelled() {
        assert_eq!(
            annotations_draft_block(&[], "https://example.com"),
            "Browser annotations — https://example.com",
        );
    }

    #[test]
    fn only_real_navigations_persist() {
        assert!(should_remember("https://example.com", None));
        assert!(should_remember(
            "https://example.com/v2",
            Some("https://example.com")
        ));
        assert!(!should_remember(
            "https://example.com",
            Some("https://example.com")
        ));
        assert!(!should_remember("", None));
        assert!(
            !should_remember("about:blank", None),
            "showing the pane is not a navigation"
        );
        assert!(!should_remember("about:blank", Some("https://example.com")));
    }

    #[test]
    fn screenshot_keys_round_trip_through_their_tab() {
        let id = TabId(41);
        let key = super::screenshot_tab_key(id);
        assert_eq!(super::screenshot_tab_id(&key), Some(id), "the capture keys to its tab");
        assert_eq!(super::screenshot_tab_id("home"), None, "session keys drain the legacy way");
        assert_eq!(super::screenshot_tab_id("browse-tab:abc"), None, "garbage keys match nothing");
    }

    #[test]
    fn fresh_webviews_open_blank() {
        assert_eq!(initial_url(None), "about:blank");
        assert_eq!(
            initial_url(Some("https://example.com")),
            "https://example.com"
        );
    }

    #[test]
    fn browser_url_round_trips_through_sessions_json() {
        let mut state = crate::sessions::RightState {
            open: true,
            kind: crate::layout::RightKind::Browser,
            ..Default::default()
        };
        state.browser_url = Some("https://example.com".to_owned());
        let json = serde_json::to_string(&state).expect("serializes");
        let back: crate::sessions::RightState = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, state);
        assert_eq!(back.browser_url.as_deref(), Some("https://example.com"));
    }

    #[test]
    fn screenshot_pruning_keeps_the_newest_twenty() {
        use std::time::{Duration, SystemTime};
        let dir = std::env::temp_dir().join(format!(
            "baaz-shot-prune-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe screenshot dir");
        let base = SystemTime::now() - Duration::from_secs(120);
        for index in 0..25 {
            let path = dir.join(format!("browser-test-{index:02}.png"));
            std::fs::write(&path, [index as u8]).expect("probe screenshot");
            std::fs::File::options()
                .write(true)
                .open(&path)
                .expect("probe screenshot")
                .set_modified(base + Duration::from_secs(index))
                .expect("probe mtime");
        }
        // A neighbour the prune must not touch.
        std::fs::write(dir.join("draft-image.png"), [0x89]).expect("probe neighbour");
        super::prune_screenshot_dir(&dir, super::MAX_SCREENSHOTS);
        let mut kept: Vec<String> = std::fs::read_dir(&dir)
            .expect("probe screenshot dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        kept.sort();
        assert_eq!(
            kept.len(),
            21,
            "twenty screenshots plus the untouched neighbour"
        );
        assert!(kept.contains(&"draft-image.png".to_owned()));
        for index in 0..5 {
            assert!(
                !kept.contains(&format!("browser-test-{index:02}.png")),
                "the oldest five go"
            );
        }
        for index in 5..25 {
            assert!(
                kept.contains(&format!("browser-test-{index:02}.png")),
                "the newest twenty stay"
            );
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn per_session_urls_never_cross() {
        let mut stored: HashMap<String, crate::sessions::RightState> = HashMap::new();
        stored.insert(
            "session-a".to_owned(),
            crate::sessions::RightState {
                browser_url: Some("https://a.example/".to_owned()),
                ..Default::default()
            },
        );
        stored.insert(
            "session-b".to_owned(),
            crate::sessions::RightState::default(),
        );
        // B navigates; A's URL is untouched.
        if let Some(state) = stored.get_mut("session-b") {
            state.browser_url = Some("https://b.example/".to_owned());
        }
        assert_eq!(
            stored["session-a"].browser_url.as_deref(),
            Some("https://a.example/")
        );
        assert_eq!(
            stored["session-b"].browser_url.as_deref(),
            Some("https://b.example/")
        );
    }

    /// A bootable [`crate::Args`] pointed at a hermetic state dir, mirroring
    /// the app tests' helper: no store read or write escapes the temp dir.
    fn routing_args(dir: &std::path::Path) -> crate::Args {
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
            login: crate::LoginSample::SignedIn,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            bench_out: None,
            sidebar_fixture: None,
            no_project: false,
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        }
    }

    /// A hermetic state dir for the routed-keystroke test: the store lock
    /// plus `BAAZ_STATE_DIR` pointed at a throwaway dir.
    fn routing_state(
        name: &str,
    ) -> (
        std::sync::MutexGuard<'static, ()>,
        Option<std::ffi::OsString>,
        std::path::PathBuf,
    ) {
        let dir = std::env::temp_dir().join(format!("baaz-route-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    /// Undo [`routing_state`]: drop the throwaway dir and hand the
    /// environment back.
    fn routing_restore(
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

    /// A page-routed keystroke dispatches after the Harness lease ends: the
    /// delivery runs inside a Harness update — the drain task's old
    /// `update_in` shape, which held the lease across the dispatch — first
    /// with ⌘L, then with ⌘,. Neither panics ("already being updated"),
    /// the address edit lands and Settings opens.
    /// ⌘W in Browse closes the shown tab — never the window — even with
    /// focus resting on the root (B2a-H3b review: an ancestor `BaazBrowse`
    /// lost to the deeper `AuiRoot` CloseWindow binding). A real keystroke
    /// dispatch, not a call to the handler.
    #[gpui::test]
    fn cmd_w_in_browse_closes_a_tab_not_the_window(cx: &mut gpui::TestAppContext) {
        assert_eq!(
            crate::app::BROWSE_ROOT_CONTEXT,
            format!("{} {}", aui::keys::ROOT_CONTEXT, crate::app::BROWSE_CONTEXT)
        );
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("cmdw");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.set_mode(crate::layout::Mode::Browse, window, cx);
                harness.step_tab_new("https://www.wikipedia.org/", window, cx);
                harness.step_tab_new("https://docs.rs/", window, cx);
            });
        });
        vc.run_until_parked();
        let count = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| {
                let harness = baaz.read(cx);
                let space = harness.current_project_id();
                harness.browser.tabs.person_tabs(space.as_deref()).len()
            })
        };
        let before = count(vc);
        assert!(before >= 2, "two person tabs to close one of: {before}");
        vc.update(|window, cx| {
            let root = baaz.read(cx).focus_root.clone();
            window.focus(&root, cx);
        });
        vc.run_until_parked();
        vc.simulate_keystrokes("cmd-w");
        vc.run_until_parked();
        assert_eq!(count(vc), before - 1, "⌘W closed exactly one tab");
        assert!(
            vc.update(|_, cx| baaz.read(cx).mode == crate::layout::Mode::Browse),
            "still in Browse, window alive"
        );
    }

    #[gpui::test]
    fn routed_page_keystrokes_dispatch_after_the_harness_lease(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("lease");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        });
        // Show the Browser pane and build its (scripted) page. Focus stays
        // where Harness parks it on the first frame (its root): parking it
        // on the page's own handle would resolve outside the rendered tree
        // and gpui would fall back above every handler.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.layout.right_open = true;
                harness.layout.right_kind = Some(crate::layout::RightKind::Browser);
                harness.prewarm_browser_in(window, cx);
            });
        });
        vc.run_until_parked();
        // Past the login screen (the `SignedIn` sample boots that way): it
        // owns the whole window, so the routed keys would resolve against
        // it instead of the shell.
        assert!(
            vc.update(|_, cx| baaz.read(cx).existing_browser(cx).is_some()),
            "the pane built a page to hand the keyboard back from"
        );
        // ⌘L first: an open Settings page swallows it.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.deliver_cef_keystroke("cmd-l", window, cx);
            });
        });
        // Delivery waits (≤ 200 ms) for the window to be key before it
        // dispatches; the test clock only moves when advanced.
        vc.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        vc.run_until_parked();
        let editing = vc.update(|_, cx| {
            baaz.read(cx)
                .existing_browser(cx)
                .map(|page| page.read(cx).is_editing())
                .unwrap_or(false)
        });
        assert!(editing, "⌘L from the page begins editing the address");
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.deliver_cef_keystroke("cmd-,", window, cx);
            });
        });
        // Delivery waits (≤ 200 ms) for the window to be key before it
        // dispatches; the test clock only moves when advanced.
        vc.executor()
            .advance_clock(std::time::Duration::from_millis(300));
        vc.run_until_parked();
        assert!(
            vc.update(|_, cx| baaz.read(cx).settings_page.open),
            "⌘, from the page opens Settings"
        );
        routing_restore(state);
    }

    /// B2a-H1: two tabs in one session are two live pages; only the active
    /// tab's page is unobscured; the agent registration follows the active
    /// tab across a switch; and forcing discard drops the hidden page but
    /// keeps its tab for a rebuild.
    #[gpui::test]
    fn two_session_tabs_hold_two_pages_with_one_live(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("tabs");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        });
        // Two session tabs behind one session key, driven at the registry
        // directly: no session view is open in this harness.
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = super::Owner::Session("tab-probe".to_owned());
                let first =
                    harness
                        .browser
                        .tabs
                        .open(None, owner.clone(), "https://a.example/", None);
                let second = harness.browser.tabs.open(
                    None,
                    owner.clone(),
                    "https://b.example/",
                    Some(first),
                );
                let page_first = harness.ensure_tab_page("tab-probe", first, window, cx);
                let page_second = harness.ensure_tab_page("tab-probe", second, window, cx);
                assert!(page_first != page_second, "each tab holds its own page");
                assert_eq!(
                    harness.browser.states.len(),
                    2,
                    "two tabs in one session are two live pages"
                );
                // The newest tab is active; the agent tools point at it.
                assert_eq!(harness.browser.tabs.active_of(&owner, None), Some(second));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_second.clone()),
                    "registration follows the active tab"
                );
                // Only the active tab's page is unobscured.
                harness.apply_page_visibility(Some(second), true, None, false, cx);
                assert!(
                    !page_second.read(cx).obscured(),
                    "the active tab's page shows"
                );
                assert!(
                    page_first.read(cx).obscured(),
                    "the hidden tab's page is obscured"
                );
                // Switching tabs re-registers the agent tools and swaps
                // which page is live.
                harness.select_tab("tab-probe", first, window, cx);
                assert_eq!(harness.browser.tabs.active_of(&owner, None), Some(first));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_first.clone()),
                    "registration follows the switch"
                );
                harness.apply_page_visibility(Some(first), true, None, false, cx);
                assert!(!page_first.read(cx).obscured());
                assert!(page_second.read(cx).obscured());
                // Scoped discard drops only the hidden page: the tab keeps
                // its URL, and showing it again rebuilds at that URL.
                harness.discard_hidden_tabs(true, Some(first), None, cx);
                assert_eq!(
                    harness.browser.states.len(),
                    1,
                    "the hidden page is discarded, the shown page kept"
                );
                assert!(
                    harness.browser.tabs.get(second).is_some(),
                    "the discarded tab survives with its URL"
                );
                let rebuilt = harness.ensure_tab_page("tab-probe", second, window, cx);
                assert_eq!(rebuilt.read(cx).url().as_ref(), "https://b.example/");
                // Back on the first tab, the step drops the hidden second
                // page at once but spares the agent's active tab — the
                // live probe drives this verb.
                harness.select_tab("tab-probe", first, window, cx);
                harness.step_tab_discard_now(window, cx);
                assert_eq!(
                    harness.browser.states.len(),
                    1,
                    "tab-discard-now drops hidden pages, never the agent's tab"
                );
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_first.clone()),
                    "the spared page is still the agent's tab"
                );
            });
        });
        vc.run_until_parked();
        routing_restore(state);
    }

    /// B2a-H1b: closing a background tab drops only its page — the active
    /// tab, its page and the agent registration stay exactly put — and
    /// closing a session's last tab unregisters the agent tools and clears
    /// the stored `browser_url`, so the tab never resurrects.
    #[gpui::test]
    fn closing_tabs_follows_only_the_active_tab(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("tabs-close");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = super::Owner::Session("tab-close".to_owned());
                harness.overrides.insert(
                    "tab-close".to_owned(),
                    crate::sessions::SessionMeta {
                        right: Some(crate::sessions::RightState {
                            browser_url: Some("https://seed.example/".into()),
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                );
                let first =
                    harness
                        .browser
                        .tabs
                        .open(None, owner.clone(), "https://a.example/", None);
                let second = harness.browser.tabs.open(
                    None,
                    owner.clone(),
                    "https://b.example/",
                    Some(first),
                );
                let page_second = harness.ensure_tab_page("tab-close", second, window, cx);
                assert_eq!(harness.browser.tabs.active_of(&owner, None), Some(second));
                // Closing the background tab: its page drops, and nothing
                // else moves — no rebuild, no re-registration.
                harness.close_tab("tab-close", first, window, cx);
                assert!(!harness.browser.states.contains_key(&first));
                assert_eq!(harness.browser.states.len(), 1);
                assert_eq!(harness.browser.tabs.active_of(&owner, None), Some(second));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-close"),
                    Some(page_second.clone()),
                    "a background close never re-registers"
                );
                // Closing the active last tab: the agent tools lose the
                // session's page and the stored URL is forgotten.
                harness.close_tab("tab-close", second, window, cx);
                assert!(harness.browser.states.is_empty());
                assert!(
                    harness
                        .terminal_service
                        .test_browser_state("tab-close")
                        .is_none(),
                    "no page survives the last close through the agent tools"
                );
                assert!(
                    harness
                        .overrides
                        .get("tab-close")
                        .and_then(|meta| meta.right.clone())
                        .and_then(|right| right.browser_url)
                        .is_none(),
                    "the last close clears the stored browser_url"
                );
            });
        });
        vc.run_until_parked();
        routing_restore(state);
    }

    /// B2a-H1c: closing a person tab while another project is current still
    /// reads `was_active` off the tab's OWN space. The old code asked the
    /// current project's space, so an active tab closed from elsewhere
    /// looked like a background close: the neighbour never built and the
    /// agent tools stayed pointed at the dead tab.
    #[gpui::test]
    fn closing_a_person_tab_from_another_space_follows_its_own_space(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("tabs-space");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = Owner::Person;
                let first = harness.browser.tabs.open(
                    Some("p1".into()),
                    owner.clone(),
                    "https://a.example/",
                    None,
                );
                let second = harness.browser.tabs.open(
                    Some("p1".into()),
                    owner.clone(),
                    "https://b.example/",
                    Some(first),
                );
                assert_eq!(
                    harness.browser.tabs.active_of(&owner, Some("p1")),
                    Some(second)
                );
                let page_first = harness.ensure_tab_page("home", first, window, cx);
                let _page_second = harness.ensure_tab_page("home", second, window, cx);
                // Switch projects: the strip now shows p2 (empty), but the
                // close still targets p1's active tab.
                harness.current_project = Some("p2".into());
                harness.close_tab("home", second, window, cx);
                assert_eq!(
                    harness.browser.tabs.active_of(&owner, Some("p1")),
                    Some(first),
                    "the model neighbours within the tab's own space"
                );
                assert_eq!(
                    harness.browser.agent_tabs.get("home"),
                    Some(&first),
                    "the neighbour takes the agent registration even closed from another space"
                );
                assert_eq!(
                    harness.terminal_service.test_browser_state("home"),
                    Some(page_first),
                    "the agent tools point at the live neighbour, never the dead tab"
                );
            });
        });
        vc.run_until_parked();
        routing_restore(state);
    }
}
