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
use aui_webview::{FakeWebBackend, WebBackend, WebviewIntent, WebviewState, WryBackend};
use gpui::{AppContext as _, Bounds, Context, Entity, Pixels, Point, Window};

use crate::app::Harness;
use crate::layout::RightKind;
use crate::overlays::MenuKind;

pub(crate) mod tabs;

pub(crate) use tabs::{Owner, StripTab, TabId, Tabs};

/// The fake backend's URL key for the no-session/home webview.
const HOME_KEY: &str = "home";
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
}

/// Whether `inputs`' native view may show: open, on the Browser kind, for
/// the active session, with no overlay over the pane and a live window.
pub(crate) fn browser_visible(inputs: BrowserVisibility) -> bool {
    inputs.pane_open
        && inputs.kind_browser
        && inputs.session_active
        && !inputs.settings_open
        && !inputs.palette_open
        && !inputs.menu_open
        && !inputs.plus_open
        && !inputs.account_menu_open
        && !inputs.dialog_open
        && !inputs.drop_cover
        && !inputs.terminal_covering
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
    Bounds { origin: Point::default(), size: window.viewport_size() }
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
        origin: Point { x: centre_left, y: content.origin.y + content.size.height - height },
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
    terminal_open && dock.zip(pane).is_some_and(|(dock, pane)| dock.intersects(&pane))
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

/// The draft text a `SendAnnotations` intent appends: the URL plus one line
/// per pin (`<index>. <element path> — <note>`). Never auto-sent; the person
/// reviews the draft first.
pub(crate) fn annotations_draft_block(
    annotations: &[Annotation],
    url: &str,
) -> String {
    let mut out = format!("Browser annotations — {url}");
    for annotation in annotations {
        out.push_str(&format!("\n{}. {} — {}", annotation.index, annotation.selector, annotation.note));
    }
    out
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
fn cef_backend(_window: &Window, _url: &str, _rgba: (u8, u8, u8, u8)) -> Option<Box<dyn WebBackend>> {
    None
}

pub(crate) const MAX_SCREENSHOTS: usize = 20;

/// This run's page screenshots live here: `$BAAZ_STATE_DIR/attachments/browser/`
/// (else the system temp dir), away from the composer's own attachments.
fn screenshot_dir() -> Option<PathBuf> {
    let base =
        std::env::var_os("BAAZ_STATE_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
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
            let modified =
                entry.metadata().ok()?.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
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

    /// The owner's active tab, opening its first when it has none: a
    /// session restores its last URL, everyone starts blank. The strip
    /// shows only when the owner has at least one tab.
    fn ensure_owner_tab(&mut self, owner: &Owner, cx: &mut Context<Self>) -> TabId {
        if let Some(id) = self.browser.tabs.active_of(owner) {
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
        let id = self.browser.tabs.open(space, owner.clone(), initial_url(stored.as_deref()), None);
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

    /// Arm the debounced `tabs.json` write: only the latest generation
    /// fires, serialising the CURRENT tabs then — a switch inside the
    /// window lands on disk instead of being overwritten by stale state.
    pub(crate) fn schedule_tabs_write(&mut self, cx: &mut Context<Self>) {
        self.browser.tabs_save_epoch += 1;
        self.browser.tabs_save_pending = true;
        let epoch = self.browser.tabs_save_epoch;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(750)).await;
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
            snapshot.save();
        })
        .detach();
    }

    /// The webview for `key`, creating it on first use. `key` names an
    /// owner (a session id, or [`HOME_KEY`] for the person), and the page
    /// is that owner's active tab's — opening the tab first when the owner
    /// has none. A new page opens on the tab's URL; only an explicit
    /// person open (`focus_fresh`) takes the keyboard into a fresh blank
    /// URL field, so ⌘L is already where the person looks. Activation,
    /// restore and `browse:` steps pass `false`: the composer keeps focus.
    fn browser_for(&mut self, key: &str, focus_fresh: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<WebviewState> {
        self.ensure_tabs_loaded(cx);
        let owner = self.tab_owner_for_key(key);
        let id = self.ensure_owner_tab(&owner, cx);
        if let Some(state) = self.browser.states.get(&id).cloned() {
            self.register_tab_page(key, id, &state);
            return state;
        }
        let url = self.browser.tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_else(|| BLANK.to_owned());
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
                let rgba = [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(&*window, initial, (0.0, 0.0), (0.0, 0.0), rgba) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!("browser: no WKWebView ({error}); falling back to the scripted page");
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
        self.browser.tabs.active_of(&owner).and_then(|id| self.browser.states.get(&id).cloned())
    }

    /// The session's live page: its active tab's state, when built. The
    /// existing pane tests reach the session's page through this instead
    /// of the tab-keyed registry.
    #[cfg(test)]
    pub(crate) fn session_page(&self, session_id: &str) -> Option<Entity<WebviewState>> {
        let owner = Owner::Session(session_id.to_owned());
        self.browser.tabs.active_of(&owner).and_then(|id| self.browser.states.get(&id).cloned())
    }

    /// Whether this session key has a live page: its active tab's state
    /// exists. The existing pane tests pin boot and switch on this.
    #[cfg(test)]
    pub(crate) fn session_has_page(&self, session_id: &str) -> bool {
        self.session_page(session_id).is_some()
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
        if !self.layout.right_open || crate::layout::right_kind(&self.layout) != RightKind::Browser {
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
        let url =
            self.browser.tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_else(|| BLANK.to_owned());
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
        if !self.layout.right_open || crate::layout::right_kind(&self.layout) != RightKind::Browser {
            return;
        }
        let key = self.browser_key(cx);
        let owner = self.tab_owner_for_key(&key);
        let exists = self.browser.tabs_loaded
            && self
                .browser
                .tabs
                .active_of(&owner)
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
                .active_of(&owner)
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
            if let Some(id) = self.browser.tabs.active_of(&owner) {
                if self.browser.tabs.set_url_title(id, url, &title) {
                    self.schedule_tabs_write(cx);
                }
            }
        }
        cx.notify();
    }

    /// `tab-new:<url>`: open a tab on `url` for the shown owner — a session
    /// tab in the active session, a person tab with no session open — and
    /// select it. Empty records a step failure. A blank URL focuses the
    /// address field, like a by-hand open. Free: no turn, no wire.
    pub(crate) fn step_tab_new(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let url = rest.trim();
        if url.is_empty() {
            crate::steps::record_step_failure("tab-new:");
            return;
        }
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let space = self.current_project_id();
        let after = self.browser.tabs.active_of(&owner);
        let id = self.browser.tabs.open(space, owner.clone(), url, after);
        self.schedule_tabs_write(cx);
        let key = self.browser_key(cx);
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
        let url =
            self.browser.tabs.get(id).map(|tab| tab.url.clone()).unwrap_or_else(|| BLANK.to_owned());
        let initial = if url.is_empty() { BLANK } else { url.as_str() };
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
                let bg = gpui::Rgba::from(cx.aui().colors.surface_1);
                let rgba = [bg.r, bg.g, bg.b, bg.a].map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
                let rgba = (rgba[0], rgba[1], rgba[2], rgba[3]);
                if let Some(backend) = cef_backend(&*window, initial, rgba) {
                    return WebviewState::new(backend, cx);
                }
                match WryBackend::new_at_with_background(&*window, initial, (0.0, 0.0), (0.0, 0.0), rgba) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!("browser: no WKWebView ({error}); falling back to the scripted page");
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
    pub(crate) fn step_tab_select(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        match self.strip_tab_at(rest, cx) {
            Some((key, id)) => self.select_tab(&key, id, window, cx),
            None => crate::steps::record_step_failure(&format!("tab-select:{rest}")),
        }
    }

    /// `tab-close:<n>`: close the n-th tab of the shown strip (1-based),
    /// activating its neighbour. Unknown or out-of-range records a step
    /// failure. Free.
    pub(crate) fn step_tab_close(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some((key, id)) = self.strip_tab_at(rest, cx) else {
            crate::steps::record_step_failure(&format!("tab-close:{rest}"));
            return;
        };
        self.close_tab(&key, id, window, cx);
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
        let id = self.browser.tabs.tabs_of(&owner).get(n - 1).map(|tab| tab.id)?;
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
    pub(crate) fn select_browser_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.browser_key(cx);
        self.select_tab(&key, id, window, cx);
    }

    /// Close the strip tab `id` (× or middle-click), through
    /// [`Self::close_tab`].
    pub(crate) fn close_browser_tab(&mut self, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        let key = self.browser_key(cx);
        self.close_tab(&key, id, window, cx);
    }

    /// Close `id`: the model picks the neighbour to activate, the closed
    /// page is dropped, and the agent tools follow the neighbour (or lose
    /// the session's page when none is left).
    pub(crate) fn close_tab(&mut self, key: &str, id: TabId, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        if self.browser.tabs.get(id).is_none() {
            return;
        }
        let next = self.browser.tabs.close(id);
        self.browser.states.remove(&id);
        self.schedule_tabs_write(cx);
        match next {
            Some(next) => {
                self.ensure_tab_page(key, next, window, cx);
            }
            None => {
                self.browser.agent_tabs.remove(key);
            }
        }
        cx.notify();
    }

    /// Open a blank tab for the shown owner and focus its address field:
    /// the strip's `+`.
    pub(crate) fn new_blank_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let owner = self.shown_tab_owner(cx);
        let space = self.current_project_id();
        let after = self.browser.tabs.active_of(&owner);
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
        let shown = self.browser.tabs.active_of(&owner);
        self.discard_hidden_tabs(true, shown, cx);
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
            WebviewIntent::SendAnnotations { annotations, screenshot, url } => {
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
                            overlays.toast("Browser", "No open session to attach the annotations to.");
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
        let active = self.browser.tabs.active_of(&owner);
        let rows = match &owner {
            // The person's strip leads with pinned tabs.
            Owner::Person => self.browser.tabs.person_tabs(self.current_project_id().as_deref()),
            _ => self.browser.tabs.tabs_of(&owner),
        };
        rows
            .iter()
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
    /// tabs are always spared; the shown tab is never a candidate.
    /// Showing a dropped tab again rebuilds its page at its URL.
    fn discard_hidden_tabs(&mut self, force: bool, shown: Option<TabId>, cx: &mut Context<Self>) {
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
            if Some(id) == shown || audible || pinned || agent.contains(&id) {
                continue;
            }
            let last = self.browser.tabs.get(id).and_then(|tab| tab.last_shown);
            // Forced (`tab-discard-now`) drops every hidden page at once;
            // the frame only drops tabs idle past the timeout.
            let due = if force { true } else { tabs::should_discard(now, last, audible, false) };
            if due {
                dropped.push(id);
            }
        }
        for id in dropped {
            self.browser.states.remove(&id);
        }
    }

    /// Set every live page's obscured flag from the shown tab: the shown
    /// tab's page shows, every other page hides. Blank pages stay hidden
    /// (WKWebView paints `about:blank` white over the dark pane).
    fn apply_page_visibility(&mut self, shown: Option<TabId>, frame_live: bool, cx: &mut Context<Self>) {
        let pages: Vec<(TabId, Entity<WebviewState>)> =
            self.browser.states.iter().map(|(id, state)| (*id, state.clone())).collect();
        for (id, state) in pages {
            let blank = matches!(state.read(cx).url().as_ref(), "" | BLANK);
            let visible = frame_live && Some(id) == shown && !blank;
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
    pub(crate) fn sync_browser(
        &mut self,
        overflow: bool,
        view_options: bool,
        account: bool,
        project_menu: bool,
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
        // A Screenshot whose bytes were not there yet attaches on arrival.
        // Stays pending across tab switches: it attaches when its own
        // session is active again, so a switch can never misattribute it.
        let active_key = self.browser_key(cx);
        let pending: Vec<String> = self.browser.shot_pending.iter().cloned().collect();
        for key in pending {
            let owner = self.tab_owner_for_key(&key);
            let bytes = self
                .browser
                .tabs
                .active_of(&owner)
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
        let menu_open = menu_kind.is_some_and(|kind| kind != MenuKind::Account)
            || overflow
            || view_options
            || project_menu;
        let account_menu_open = menu_kind == Some(MenuKind::Account) || account;
        let dialog_open = overlays.dialog.is_some();
        // The composer's `+` menu is plain view state, never the overlay
        // stack — read it off the active view. Audit: caret popovers and
        // chip pickers ride `overlays.menu` above; the row-detail card hangs
        // beside the sidebar, tooltips seat on the header, and the
        // queue/steer strips render inline in the composer — none of them
        // covers the right pane, so the `+` menu is the only extra input.
        let plus_open = self.active.as_ref().is_some_and(|view| view.read(cx).plus_open());
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
                + (if self.sidebar_open { gpui::px(self.resize.width) } else { gpui::px(0.0) });
            let right = content.origin.x + content.size.width
                - (if self.layout.right_open {
                    gpui::px(self.right_resize.width)
                } else {
                    gpui::px(0.0)
                });
            let centre_height = f32::from(window.bounds().size.height).max(0.0) - HEADER_PX;
            let want = self.layout.terminal_height.unwrap_or(crate::terminal::DOCK_DEFAULT_HEIGHT);
            terminal_dock_bounds(
                content,
                left,
                right,
                gpui::px(crate::terminal::clamp_dock_height(want, centre_height)),
            )
        });
        let base = BrowserVisibility {
            pane_open: self.layout.right_open,
            kind_browser: kind == RightKind::Browser,
            session_active: false,
            settings_open,
            palette_open,
            menu_open,
            plus_open,
            account_menu_open,
            dialog_open,
            drop_cover: self.browser.drop_cover,
            terminal_covering: terminal_covers_browser(self.layout.terminal_open, dock, pane),
            window_active: window.is_window_active(),
        };
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
        let shown = browser_session_active(&shown_key, &active_key, pending_switch)
            .then(|| self.browser.tabs.active_of(&shown_owner))
            .flatten();
        // The frame shows a page exactly when the pane stands open on the
        // Browser kind with nothing over it; ownership rides `shown`, which
        // is the frame owner's active tab (or nothing while pending).
        let frame_live = browser_visible(BrowserVisibility { session_active: true, ..base });
        self.apply_page_visibility(shown, frame_live, cx);
        // The shown page stamps its tab when it really shows (never a
        // blank), so discard measures real hiding.
        if frame_live {
            if let Some(id) = shown {
                let blank = self
                    .browser
                    .states
                    .get(&id)
                    .map(|state| matches!(state.read(cx).url().as_ref(), "" | BLANK))
                    .unwrap_or(true);
                if !blank && self.browser.tabs.mark_shown(id, tabs::now_ms()) {
                    tabs_dirty = true;
                }
            }
        }
        // Long-hidden pages drop their native views (but keep their tabs);
        // the shown tab is never a candidate.
        self.discard_hidden_tabs(false, shown, cx);
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
        if !browser_pane_showing(
            self.layout.right_open,
            crate::layout::right_kind(&self.layout) == RightKind::Browser,
        ) {
            return;
        }
        let Some(state) = self.existing_browser(cx) else { return };
        let content = window_content_bounds(window);
        // The live page sits under the webview's nav row: a press on the
        // row itself is outside the page and takes the keyboard back too.
        let pane = browser_page_bounds(content, gpui::px(self.right_resize.width));
        let page = gpui::Bounds::new(
            gpui::point(pane.origin.x, pane.origin.y + gpui::px(BROWSER_NAV_PX)),
            gpui::size(pane.size.width, (pane.size.height - gpui::px(BROWSER_NAV_PX)).max(gpui::px(0.))),
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
        if state.read(cx).is_editing() {
            return;
        }
        state.update(cx, |state, cx| state.begin_editing(window, cx));
    }

    /// `page-focus`: give the active browser page the keyboard, focusing the
    /// pane's own focus handle — the same hand-over an explicit person open
    /// performs. Free: no turn, no wire.
    pub(crate) fn step_page_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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
                window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(parsed, cx);
                });
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
                    crate::baaz_log!("browser: routed {keystroke} after the window closed; dropping");
                    break;
                };
                cx.update(|cx| {
                    let Some(target) = cx.active_window() else {
                        crate::baaz_log!("browser: routed {keystroke} with no live window; dropping");
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
        }
    }

}

#[cfg(test)]
mod tests {
    use super::{
        annotations_draft_block, browser_page_bounds, browser_pane_showing, browser_session_active,
        browser_visible, initial_url, should_release_keyboard_on_mouse_down, should_remember,
        terminal_covers_browser, terminal_dock_bounds, Annotation, BrowserVisibility, FakeWebBackend,
        WebviewState,
    };
    use aui_webview::WebBackend as _;
    use std::collections::HashMap;

    fn all_clear() -> BrowserVisibility {
        BrowserVisibility {
            pane_open: true,
            kind_browser: true,
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
        }
    }

    #[test]
    fn native_view_shows_only_with_pane_open_on_active_browser() {
        assert!(browser_visible(all_clear()));
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
            assert!(!browser_visible(flip(all_clear())), "{name} must obscure the native view");
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
        assert!(!covering, "an open centre-column dock never covers the right pane");
        let mut inputs = all_clear();
        inputs.terminal_covering = covering;
        assert!(browser_visible(inputs), "dock open + pane open, no overlap → the native view shows");
    }

    #[test]
    fn full_width_dock_still_covers_the_page() {
        assert!(
            terminal_covers_browser(true, Some(full_width_dock_rect()), Some(overlapping_pane_rect())),
            "a dock under the pane keeps the input meaningful"
        );
        assert!(
            !terminal_covers_browser(false, Some(full_width_dock_rect()), Some(overlapping_pane_rect())),
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
        let dock = terminal_dock_bounds(content, gpui::px(260.0), gpui::px(1040.0), gpui::px(260.0));
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
        assert!(should_release_keyboard_on_mouse_down(true, outside, Some(beside_pane_rect())));
        assert!(!should_release_keyboard_on_mouse_down(true, inside, Some(beside_pane_rect())));
        assert!(!should_release_keyboard_on_mouse_down(false, outside, Some(beside_pane_rect())));
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
        assert!(!browser_session_active("b", "a", true), "nor any other session's");
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
        assert!(should_remember("https://example.com/v2", Some("https://example.com")));
        assert!(!should_remember("https://example.com", Some("https://example.com")));
        assert!(!should_remember("", None));
        assert!(!should_remember("about:blank", None), "showing the pane is not a navigation");
        assert!(!should_remember("about:blank", Some("https://example.com")));
    }

    #[test]
    fn fresh_webviews_open_blank() {
        assert_eq!(initial_url(None), "about:blank");
        assert_eq!(initial_url(Some("https://example.com")), "https://example.com");
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
        assert_eq!(kept.len(), 21, "twenty screenshots plus the untouched neighbour");
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
        stored.insert("session-b".to_owned(), crate::sessions::RightState::default());
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
    ) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
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

    /// A page-routed keystroke dispatches after the Harness lease ends: the
    /// delivery runs inside a Harness update — the drain task's old
    /// `update_in` shape, which held the lease across the dispatch — first
    /// with ⌘L, then with ⌘,. Neither panics ("already being updated"),
    /// the address edit lands and Settings opens.
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
        vc.run_until_parked();
        let editing = vc.update(|_, cx| {
            baaz
                .read(cx)
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
                let first = harness.browser.tabs.open(None, owner.clone(), "https://a.example/", None);
                let second =
                    harness.browser.tabs.open(None, owner.clone(), "https://b.example/", Some(first));
                let page_first = harness.ensure_tab_page("tab-probe", first, window, cx);
                let page_second = harness.ensure_tab_page("tab-probe", second, window, cx);
                assert!(page_first != page_second, "each tab holds its own page");
                assert_eq!(
                    harness.browser.states.len(),
                    2,
                    "two tabs in one session are two live pages"
                );
                // The newest tab is active; the agent tools point at it.
                assert_eq!(harness.browser.tabs.active_of(&owner), Some(second));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_second.clone()),
                    "registration follows the active tab"
                );
                // Only the active tab's page is unobscured.
                harness.apply_page_visibility(Some(second), true, cx);
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
                assert_eq!(harness.browser.tabs.active_of(&owner), Some(first));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_first.clone()),
                    "registration follows the switch"
                );
                harness.apply_page_visibility(Some(first), true, cx);
                assert!(!page_first.read(cx).obscured());
                assert!(page_second.read(cx).obscured());
                // Scoped discard drops only the hidden page: the tab keeps
                // its URL, and showing it again rebuilds at that URL.
                harness.discard_hidden_tabs(true, Some(first), cx);
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
}
