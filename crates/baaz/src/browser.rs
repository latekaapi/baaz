//! The browser pane's engine (Z7a): one webview per session, remembered.
//!
//! [`WebviewState`](aui_webview::WebviewState) holds a
//! [`WebBackend`](aui_webview::WebBackend) and what the pane knows about it.
//! This module owns the per-session registry — one live state per session id,
//! plus one for the no-session/home case — created lazily the first time the
//! Browser kind is shown for that session. Switching sessions hides the old
//! session's webview; it never destroys it.
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
use aui_webview::{FakeWebBackend, WebBackend, WebviewIntent, WebviewState, WryBackend};
use gpui::{AppContext as _, Bounds, Context, Entity, Pixels, Point, Window};

use crate::app::Harness;
use crate::layout::RightKind;
use crate::overlays::MenuKind;

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
    /// The Settings dialog or the Providers sheet stands open.
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

    /// The webview for `key`, creating it on first use. A new webview opens
    /// on its session's last URL (else `about:blank`); only an explicit
    /// person open (`focus_fresh`) takes the keyboard into a fresh blank
    /// URL field, so ⌘L is already where the person looks. Activation,
    /// restore and `browse:` steps pass `false`: the composer keeps focus.
    fn browser_for(&mut self, key: &str, focus_fresh: bool, window: &mut Window, cx: &mut Context<Self>) -> Entity<WebviewState> {
        if key == HOME_KEY {
            if let Some(home) = self.browser.home.clone() {
                self.terminal_service.register_browser(key, home.clone());
                return home;
            }
        } else if let Some(state) = self.browser.states.get(key).cloned() {
            self.terminal_service.register_browser(key, state.clone());
            return state;
        }
        let stored = if key == HOME_KEY {
            None
        } else {
            self.overrides.get(key).and_then(|meta| meta.right.clone()).and_then(|right| right.browser_url)
        };
        let initial = initial_url(stored.as_deref()).to_owned();
        let fresh_blank = initial == BLANK;
        // Explicit boot flag, never environment sniffed in render: captures
        // and tests run the scripted page, the app runs WKWebView.
        let fake = self.browser.fake || cfg!(test);
        let state = cx.new(|cx| {
            if fake {
                let mut backend = FakeWebBackend::new();
                if backend.url().as_ref() != initial.as_str() {
                    backend.navigate(&initial);
                }
                WebviewState::new(Box::new(backend), cx)
            } else {
                match WryBackend::new_at(&*window, &initial, (0.0, 0.0), (0.0, 0.0)) {
                    Ok(backend) => WebviewState::new(Box::new(backend), cx),
                    Err(error) => {
                        crate::baaz_log!("browser: no WKWebView ({error}); falling back to the scripted page");
                        let mut backend = FakeWebBackend::new();
                        if backend.url().as_ref() != initial.as_str() {
                            backend.navigate(&initial);
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
        if key == HOME_KEY {
            self.browser.home = Some(state.clone());
        } else {
            self.browser.states.insert(key.to_owned(), state.clone());
        }
        // The agent's `browser_*` tools act on this same state through the
        // socket service, keyed by session id.
        self.terminal_service.register_browser(key, state.clone());
        state
    }

    /// The live webview for the key the pane shows, when one already
    /// exists — never creating. The closing frame uses this: the page's
    /// last snapshot stays on screen while the column slides out instead
    /// of the "Opening the page" placeholder (B7).
    pub(crate) fn existing_browser(&self, cx: &gpui::App) -> Option<Entity<WebviewState>> {
        let key = self.browser_key(cx);
        if key == HOME_KEY {
            self.browser.home.clone()
        } else {
            self.browser.states.get(&key).cloned()
        }
    }

    /// Prepare the active session's webview ahead of the pane animating
    /// (B7): `toggle_right`/`show_right` call this when they land open on
    /// Browser, so the opening frame shows the page instead of paying the
    /// construction cost inside `render`. Only the scripted backend can be
    /// prepared without a window — the real WKWebView needs one, so on a
    /// real window the first open still creates lazily in `render` (noted
    /// in the report); tests and captures prewarm here and never in
    /// `render`. Never focuses: focus rides only on the person's own open
    /// through [`Self::ensure_browser_person`].
    pub(crate) fn prewarm_browser_if_needed(&mut self, cx: &mut Context<Self>) {
        if !self.layout.right_open || crate::layout::right_kind(&self.layout) != RightKind::Browser {
            return;
        }
        if !(self.browser.fake || cfg!(test)) {
            return;
        }
        let key = self
            .active
            .as_ref()
            .map(|view| view.read(cx).session_id.clone())
            .unwrap_or_else(|| HOME_KEY.to_owned());
        if key == HOME_KEY {
            if self.browser.home.is_some() {
                return;
            }
        } else if self.browser.states.contains_key(&key) {
            return;
        }
        let stored = if key == HOME_KEY {
            None
        } else {
            self.overrides.get(&key).and_then(|meta| meta.right.clone()).and_then(|right| right.browser_url)
        };
        let initial = initial_url(stored.as_deref()).to_owned();
        let state = cx.new(|cx| {
            let mut backend = FakeWebBackend::new();
            if backend.url().as_ref() != initial.as_str() {
                backend.navigate(&initial);
            }
            WebviewState::new(Box::new(backend), cx)
        });
        if key == HOME_KEY {
            self.browser.home = Some(state.clone());
        } else {
            self.browser.states.insert(key.clone(), state.clone());
        }
        self.terminal_service.register_browser(&key, state);
    }

    /// The webview the pane shows this frame, creating it on first use.
    /// The caller gates on pane-open-on-Browser, so activation, boot and
    /// every other kind never create a webview as a side effect; the
    /// `browse:` step and agent tools create through [`Self::browser_for`]
    /// directly instead. Focus rides only on the person's own open: render
    /// consumes one armed `show_right`/`toggle_right` onto a blank page,
    /// and a restore leaves the composer's keyboard alone.
    pub(crate) fn ensure_browser_person(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WebviewState> {
        let focus = std::mem::take(&mut self.browser_url_focus_armed);
        let key = self.browser_key(cx);
        self.browser_for(&key, focus, window, cx)
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
        let state = self.browser_for(&key, false, window, cx);
        if state.read(cx).url().as_ref() != url {
            state.update(cx, |state, _cx| state.navigate(url));
        }
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

    /// Per-frame browser sync, called from `render` after the overlay set is
    /// known: persist real navigations onto their sessions, resolve pending
    /// screenshot attaches, and hide every native view the frame covers.
    /// Called with the already-computed overlay elements (`None` is closed),
    /// so this reads state only.
    pub(crate) fn sync_browser(
        &mut self,
        overflow: bool,
        view_options: bool,
        account: bool,
        project_menu: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        // Real navigations persist onto their sessions — debounced to actual
        // changes, never one write per poll.
        let urls: Vec<(String, String)> = self
            .browser.states
            .iter()
            .map(|(key, state)| (key.clone(), state.read(cx).url().to_string()))
            .collect();
        for (key, current) in urls {
            let stored = self
                .overrides
                .get(&key)
                .and_then(|meta| meta.right.clone())
                .and_then(|right| right.browser_url);
            // B7: a navigation is not searchable, so it persists through
            // the cheap debounced write — never the session-list settle.
            if should_remember(&current, stored.as_deref()) {
                self.remember_browser_url_cheap(&key, current, cx);
            }
        }
        // A Screenshot whose bytes were not there yet attaches on arrival.
        // Stays pending across session switches: it attaches when its own
        // session is active again, so a switch can never misattribute it.
        let active_key = self.browser_key(cx);
        let pending: Vec<String> = self.browser.shot_pending.iter().cloned().collect();
        for key in pending {
            let bytes = if key == HOME_KEY {
                self.browser.home.as_ref().and_then(|home| home.read(cx).screenshot().map(<[u8]>::to_vec))
            } else {
                self.browser.states
                    .get(&key)
                    .and_then(|state| state.read(cx).screenshot().map(<[u8]>::to_vec))
            };
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
        let settings_open = overlays.settings.is_some() || self.muse_sheet;
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
        let mut keys: Vec<(String, Entity<WebviewState>)> = self
            .browser.states
            .iter()
            .map(|(key, state)| (key.clone(), state.clone()))
            .collect();
        if let Some(home) = self.browser.home.clone() {
            keys.push((HOME_KEY.to_owned(), home));
        }
        for (key, state) in keys {
            let visible = browser_visible(BrowserVisibility {
                session_active: key == active_key,
                ..base
            });
            state.update(cx, |state, _cx| state.set_obscured(!visible));
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
        let key = self.browser_key(cx);
        let state = if key == HOME_KEY {
            self.browser.home.clone()
        } else {
            self.browser.states.get(&key).cloned()
        };
        let Some(state) = state else { return };
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
        let key = self.browser_key(cx);
        let state = self.browser_for(&key, false, window, cx);
        if state.read(cx).is_editing() {
            return;
        }
        state.update(cx, |state, cx| state.begin_editing(window, cx));
    }
}

/// All webviews a [`Harness`](crate::app::Harness) holds. Kept here so the
/// registry's shape lives beside the logic that drives it.
pub(crate) struct BrowserRegistry {
    /// One live state per session id. Hidden on switch, never destroyed.
    pub states: HashMap<String, Entity<WebviewState>>,
    /// The no-session/home webview.
    pub home: Option<Entity<WebviewState>>,
    /// Set at boot: scripted page (`--screenshot`, tests) vs WKWebView.
    pub fake: bool,
    /// An external file drag currently covers the window.
    pub drop_cover: bool,
    /// Sessions whose Screenshot intent still waits for bytes.
    pub shot_pending: HashSet<String>,
}

impl BrowserRegistry {
    /// Empty: no webview exists until its session first shows the Browser kind.
    pub(crate) fn new(fake: bool) -> Self {
        Self { states: HashMap::new(), home: None, fake, drop_cover: false, shot_pending: HashSet::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        annotations_draft_block, browser_page_bounds, browser_pane_showing, browser_visible, initial_url,
        should_release_keyboard_on_mouse_down, should_remember, terminal_covers_browser, terminal_dock_bounds,
        Annotation, BrowserVisibility, FakeWebBackend, WebviewState,
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
}
