//! The browser pane's engine (Z7a, tabs B2a-H1): one webview per tab.
//!
//! [`WebviewState`](aui_webview::WebviewState) holds a
//! [`WebBackend`](aui_webview::WebBackend) and what the pane knows about it.
//! This module owns the tab registry — one live state per open tab, created
//! lazily the first time the Browser kind shows that tab — inside
//! [`BrowserView`] (`view.rs`), Browse mode's entity, held by `Harness` as
//! one handle. Its window-side actions live by seam in the sibling files
//! (`tabs_actions`, `pages`, `sync`, `omnibox`, `find`, `permissions`,
//! `downloads`, `page_actions`, `keys`, `steps`, `agent_tabs`; task 4.5),
//! what covers the native page in `occlusion`, and `browser.db` behind the
//! [`data::BrowserData`] service. The tab model
//! itself ([`tabs`]) is pure: tabs belong to a space and to an
//! owner (the person, or one session). Switching tabs hides the old tab's
//! webview; a tab hidden long enough is discarded (its state dropped, its
//! URL kept) and rebuilt on show.
//!
//! `browser_key` lookups mean "the active tab of the active session (or the
//! person's active tab in the current space when no session is open)".
//!
//! Backend: [`WryBackend`](aui_webview::WryBackend) (a WKWebView child of the
//! gpui window) in the normal app, [`FakeWebBackend`](aui_webview::FakeWebBackend)
//! when running a `--screenshot` capture or tests — a native view never
//! appears in a gpui screenshot, and captures must be deterministic. The
//! choice rides on [`BrowserView::fake`], an explicit flag set at boot, never
//! on environment sniffed in render.
//!
//! # The native-overlay rule
//!
//! The native view is composited ABOVE the gpui scene, so anything gpui draws
//! over the page is invisible. The host therefore calls
//! [`set_obscured`](aui_webview::WebviewState::set_obscured) with `true`
//! while any overlay covers the pane. One owner decides it
//! ([`occlusion::NativeOcclusion`]), and `occlusion::browser_visible` is the
//! one pure predicate behind it: the native view is visible only when the
//! pane is open on the Browser kind for the active session with nothing over
//! it.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use aui::workbench::Annotation;
use aui_tokens::ActiveAui;
use aui_webview::{DownloadState, FakeWebBackend, PermissionDecision, WebBackend, WebviewIntent, WebviewState, WryBackend};
use gpui::{AppContext as _, Bounds, Context, Entity, Pixels, Point, Window};
use gpui_kit::base::input::{InputEvent, InputState};

use crate::app::Harness;
use crate::layout::RightKind;

pub(crate) mod adblock;
pub(crate) use baaz_core::browser_prefs::agents;
pub(crate) mod assistant;
pub(crate) mod chrome;
pub(crate) use baaz_browser_core::bookmarks;
pub(crate) use baaz_browser_core::chrome_import;
pub(crate) use baaz_browser_core::favicons;
pub(crate) use baaz_browser_core::history;
pub(crate) mod peek_split;
pub(crate) use baaz_browser_core::automation;
pub use baaz_browser_core::clone_sweep;
mod sidebar_actions;
mod view;
mod occlusion;
pub(crate) mod data;
mod agent_tabs;
mod downloads;
mod find;
mod keys;
mod omnibox;
pub(crate) use omnibox::OmniboxStep;
mod page_actions;
mod pages;
mod permissions;
#[cfg(feature = "probe")]
mod steps;
mod sync;
mod tabs_actions;
pub(crate) use baaz_browser_core::tabs;
pub(crate) mod toolbar;

pub(crate) use view::{BrowserEvent, BrowserView};
#[cfg(test)]
pub(crate) use occlusion::PageShow;
#[cfg(test)]
use occlusion::{browser_visible, centre_visibility, BrowserVisibility};
pub(crate) use tabs::{Owner, TabId, Tabs};
pub(crate) use toolbar::{BackForward, OmniRow, OmniTarget, SuggestKind};

/// The fake backend's URL key for the no-session/home webview.
pub(crate) const HOME_KEY: &str = "home";

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

/// The shell header's height in pixels (docs/83 §2.5: the bar, 44 incl.
/// its line): the centre's content and the right pane's page hang under it.
const HEADER_PX: f32 = aui_tokens::scale::BAR_DEFAULT;

/// A region divider (docs/83 §1 rule 2): the right column's 1 px left edge,
/// which the pane's page sits past.
const DIVIDER_PX: f32 = 1.0;

/// The Browse page's rect for the window's `content` box: the flat shell's
/// centre column edge to edge (docs/83 §3) — past the `sidebar` column (0
/// when closed; its divider is its own last pixel), short of the `right`
/// column (0 when closed; its divider is its own first pixel) and above the
/// open dock. Pure, for tests.
pub(crate) fn column_page_rect(content: Bounds<Pixels>, sidebar: Pixels, right: Pixels, dock: Pixels) -> Bounds<Pixels> {
    Bounds {
        origin: Point { x: content.origin.x + sidebar, y: content.origin.y },
        size: gpui::size(
            (content.size.width - sidebar - right).max(gpui::px(0.0)),
            (content.size.height - dock).max(gpui::px(0.0)),
        ),
    }
}

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

/// The browser pane's page in window coordinates, derived from the window's
/// content box: the right column hangs off the content's right edge,
/// `right_width` wide, its first pixel the divider; the page sits under the
/// pane's tab row (the column's 44 px header) and the Page's 36 px nav row
/// ([`aui::workbench::PAGE_NAV_H`]). Pure so tests can drive it.
pub(crate) fn browser_page_bounds(content: Bounds<Pixels>, right_width: Pixels) -> Bounds<Pixels> {
    let bar = gpui::px(aui_tokens::scale::BAR_DEFAULT + aui::workbench::PAGE_NAV_H);
    Bounds {
        origin: Point {
            x: content.origin.x + content.size.width - right_width + gpui::px(DIVIDER_PX),
            y: content.origin.y + bar,
        },
        size: gpui::size(
            (right_width - gpui::px(DIVIDER_PX)).max(gpui::px(0.0)),
            (content.size.height - bar).max(gpui::px(0.0)),
        ),
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
/// The name an annotation batch's screenshot chip carries, so the sent
/// turn draws it as the batch's strip rather than a tile (D4).
pub(crate) const ANNOTATION_SHOT_NAME: &str = "annotated page.png";

/// The batch a sent annotation turn draws (D4, docs/41 §2.2): the page's
/// `host/path`, the screenshot, each note with its pin's place.
pub(crate) fn annotation_batch(
    annotations: &[Annotation],
    url: &str,
    pins: &[Option<(f32, f32)>],
    page_size: (f32, f32),
    screenshot: Option<String>,
) -> crate::sent::SentAnnotations {
    let address = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url).trim_end_matches('/').to_owned();
    crate::sent::SentAnnotations {
        screenshot,
        size: page_size,
        address,
        notes: annotations
            .iter()
            .enumerate()
            .map(|(i, a)| crate::sent::SentNote {
                index: a.index,
                at: pins.get(i).copied().flatten().unwrap_or((-100.0, -100.0)),
                selector: a.selector.to_string(),
                note: a.note.to_string(),
            })
            .collect(),
    }
}

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

pub(crate) use baaz_browser_core::tabs::should_remember;

/// The URL a fresh webview for a session opens on: its last URL, else blank.
pub(crate) fn initial_url(stored: Option<&str>) -> &str {
    stored.unwrap_or(BLANK)
}

/// Chromium's own default canvas: white. A page that paints no background
/// of its own (HN's margins, many articles) shows this, as in Chrome. It was
/// Baaz's dark surface — a WKWebView-era guard against a white about:blank —
/// which drew black bands beside such pages and baked dark grounds into
/// every cover snapshot (owner screenshots 2026-10-11). Blank tabs never
/// show their native view (the new-tab page is gpui), so white costs nothing.
#[cfg(feature = "cef")]
const PAGE_CANVAS: (u8, u8, u8, u8) = (255, 255, 255, 255);

/// How many page screenshots one run keeps: enough to re-read the last
/// few captures, bounded so a long session never fills the disk.
/// The Chromium page engine for a new pane page, when the bundle carries CEF
/// and `BAAZ_BROWSER_ENGINE=webkit` does not ask for WebKit. `None` means
/// "use WKWebView": CEF unavailable, or it failed to open a page (logged).
/// The host's ground goes to WKWebView only; Chromium paints white.
#[cfg(feature = "cef")]
pub(crate) fn cef_backend(window: &Window, url: &str, _host_ground: (u8, u8, u8, u8), profile: Option<&str>) -> Option<Box<dyn WebBackend>> {
    let rgba = PAGE_CANVAS;
    if !aui_cef::available() || std::env::var("BAAZ_BROWSER_ENGINE").as_deref() == Ok("webkit") {
        return None;
    }
    match aui_cef::CefBackend::new_at_in(window, url, (0.0, 0.0), (0.0, 0.0), rgba, profile) {
        Ok(backend) => Some(Box::new(backend)),
        Err(error) => {
            crate::baaz_log!("browser: no Chromium page ({error}); falling back to WKWebView");
            None
        }
    }
}

#[cfg(not(feature = "cef"))]
pub(crate) fn cef_backend(
    _window: &Window,
    _url: &str,
    _rgba: (u8, u8, u8, u8),
    _profile: Option<&str>,
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


#[cfg(feature = "cef")]
thread_local! {
    /// Whether the right pane shows its Page in Sessions mode this frame:
    /// the one place a Shift + sideways swipe over a native page is the
    /// pane's (set by the render, read by the native wheel router).
    pub(crate) static PANE_PAGE_LIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a Shift-held wheel over a native page is the pane strip's: the
/// pane shows its Page, and the swipe is sideways (|dx| > 2|dy|, the strip
/// listener's own rule). Plain state only — it runs inside AppKit's monitor.
#[cfg(feature = "cef")]
pub(crate) fn page_wheel_claimed(wheel: &aui_cef::PageWheel) -> bool {
    PANE_PAGE_LIVE.with(|live| live.get()) && wheel.dx.abs() > 2.0 * wheel.dy.abs()
}

/// The gpui wheel event a claimed page swipe replays as: Shift held, the
/// precise deltas as pixels (a mouse's as lines), its phase.
#[cfg(feature = "cef")]
pub(crate) fn page_wheel_event(wheel: &aui_cef::PageWheel) -> gpui::ScrollEvent {
    let delta = if wheel.precise {
        gpui::ScrollDelta::Pixels(gpui::point(gpui::px(wheel.dx as f32), gpui::px(wheel.dy as f32)))
    } else {
        gpui::ScrollDelta::Lines(gpui::point(wheel.dx as f32, wheel.dy as f32))
    };
    gpui::ScrollEvent {
        position: gpui::point(gpui::px(wheel.x as f32), gpui::px(wheel.y as f32)),
        delta,
        modifiers: gpui::Modifiers { shift: true, ..Default::default() },
        touch_phase: match wheel.phase {
            aui_cef::WheelPhase::Started => gpui::TouchPhase::Started,
            aui_cef::WheelPhase::Moved => gpui::TouchPhase::Moved,
            aui_cef::WheelPhase::Ended => gpui::TouchPhase::Ended,
        },
        source: Default::default(),
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

/// The Field's input placeholder outside the Bookmarks scope.
pub(crate) const FIELD_PLACEHOLDER: &str = "Search, open a page, or ask";

/// What a Field command does: one of the page's own actions, or an R4
/// history command Baaz owns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldAction {
    Page(aui::screens::sidebar::PageAction),
    /// Never record the shown page's site again, and forget it.
    ForgetSite,
    /// Clear all browsing history (asks first).
    ClearHistory,
    /// ⌘D: bookmark the shown page.
    Bookmark,
    /// Browse bookmarks in the Field (the scoped view).
    Bookmarks,
    /// Allow (or block again) ads on the shown page's site.
    AdsHere,
}

/// What the Field finds by name (docs/27 §4.4): the page's own commands.
const FIELD_COMMANDS: [(&str, &str, FieldAction); 13] = [
    ("new-tab", "New tab", FieldAction::Page(aui::screens::sidebar::PageAction::NewTab)),
    ("find", "Find in page", FieldAction::Page(aui::screens::sidebar::PageAction::Find)),
    ("copy-link", "Copy link", FieldAction::Page(aui::screens::sidebar::PageAction::CopyLink)),
    ("safari", "Open in Safari", FieldAction::Page(aui::screens::sidebar::PageAction::OpenInSafari)),
    ("annotate", "Annotate page", FieldAction::Page(aui::screens::sidebar::PageAction::Annotate)),
    ("screenshot", "Take a screenshot", FieldAction::Page(aui::screens::sidebar::PageAction::Screenshot)),
    ("devtools", "Developer tools", FieldAction::Page(aui::screens::sidebar::PageAction::DevTools)),
    ("settings", "Browser settings", FieldAction::Page(aui::screens::sidebar::PageAction::Settings)),
    ("forget-site", "Never remember this site", FieldAction::ForgetSite),
    ("clear-history", "Clear history…", FieldAction::ClearHistory),
    ("bookmark", "Bookmark this page", FieldAction::Bookmark),
    ("bookmarks", "Bookmarks", FieldAction::Bookmarks),
    ("ads-here", "Allow ads on this site", FieldAction::AdsHere),
];


/// Whether the line's activity `label` is the agent typing into the page
/// (the face looks down at the keys), from `browser_activity_label`'s words.
pub(crate) fn face_types(label: &str) -> bool {
    label.starts_with("typing") || label.starts_with("pressing")
}

#[cfg(test)]
mod tests {
    use crate::layout::LayoutGlobal as _;
    use super::{
        annotations_draft_block, browser_centre_bounds, browser_page_bounds, browser_pane_showing,
        browser_session_active, browser_visible, initial_url,
        should_release_keyboard_on_mouse_down, should_remember,
        Annotation,
        BrowserVisibility, FakeWebBackend, WebviewState,
    };
    use super::{Owner, TabId};
    use aui_webview::WebBackend as _;
    use std::collections::HashMap;

    /// Flat (docs/83 §3): the Browse page fills its column edge to edge —
    /// from the sidebar column's edge to the right column's, top to bottom,
    /// above the dock.
    #[test]
    fn column_page_rect_is_the_column_edge_to_edge() {
        use gpui::{point, px, size, Bounds};
        let content = Bounds { origin: point(px(0.0), px(0.0)), size: size(px(1440.0), px(900.0)) };
        let page = super::column_page_rect(content, px(256.0), px(400.0), px(120.0));
        assert_eq!(page.origin, point(px(256.0), px(0.0)));
        assert_eq!(page.size, size(px(1440.0 - 256.0 - 400.0), px(900.0 - 120.0)));
        let closed = super::column_page_rect(content, px(0.0), px(0.0), px(0.0));
        assert_eq!(closed, content, "both columns closed: the whole window");
    }

    fn all_clear() -> BrowserVisibility {
        BrowserVisibility {
            pane_open: true,
            kind_browser: true,
            centre_browse: false,
            session_active: true,
            settings_open: false,
            overlay_open: false,
            drop_cover: false,
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
            overlay_open: false,
            drop_cover: false,
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
    fn stale_sessions_covers_never_reach_the_browse_centre() {
        let mut base = all_clear();
        base.drop_cover = true;
        assert!(!browser_visible(base), "it still covers the pane's page");
        assert!(
            browser_visible(super::centre_visibility(base, true, true, false)),
            "Browse draws no drop target, so it never freezes its page"
        );
        // What the main window really draws over the centre still covers.
        base.overlay_open = true;
        assert!(!browser_visible(super::centre_visibility(base, true, true, false)));
        base.overlay_open = false;
        assert!(!browser_visible(super::centre_visibility(base, true, true, true)), "a chrome popover");
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
        // The palette and the settings page obscure the centre page
        // exactly as they obscure the pane's: the same covering rules,
        // not a second set.
        let mut palette = centre_clear();
        palette.overlay_open = true;
        assert!(!browser_visible(palette), "an overlay (the palette) obscures the centre page");
        let mut settings = centre_clear();
        settings.settings_open = true;
        assert!(!browser_visible(settings), "settings obscure the centre page");
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


    /// One truth-table row: which input a case flips off (or on, for covers).
    type VisibilityFlip = fn(BrowserVisibility) -> BrowserVisibility;

    #[test]
    fn every_covering_input_hides_the_native_view() {
        let cases: [(&str, VisibilityFlip); 6] = [
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
            ("overlay", |mut inputs| {
                inputs.overlay_open = true;
                inputs
            }),
            ("drop overlay", |mut inputs| {
                inputs.drop_cover = true;
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

    /// A 1440x900 window's pane, hanging off the right edge.
    fn beside_pane_rect() -> gpui::Bounds<gpui::Pixels> {
        gpui::Bounds {
            origin: gpui::point(gpui::px(1040.0), gpui::px(44.0)),
            size: gpui::size(gpui::px(400.0), gpui::px(856.0)),
        }
    }

    #[test]
    fn the_pane_page_sits_past_the_divider_under_the_bar() {
        let content = gpui::Bounds {
            origin: gpui::point(gpui::px(0.0), gpui::px(0.0)),
            size: gpui::size(gpui::px(1440.0), gpui::px(900.0)),
        };
        let pane = browser_page_bounds(content, gpui::px(400.0));
        assert_eq!(f32::from(pane.origin.x), 1041.0, "past the column's 1 px divider");
        assert_eq!(f32::from(pane.origin.y), 80.0, "under the pane's 44 px tab row and the 36 px nav row");
        assert_eq!(f32::from(pane.size.width), 399.0);
        assert_eq!(f32::from(pane.size.height), 900.0 - 80.0);
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
            theme: Some(aui_tokens::ThemeKind::Dark),
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

    /// Entering Browse lands on the new-tab page with its field focused
    /// (B2a live pass: typing went nowhere after ⌥⌘B and after ⌘T).
    #[gpui::test]
    fn entering_browse_focuses_the_new_tab_field(cx: &mut gpui::TestAppContext) {
        use gpui::Focusable as _;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("ntfocus");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.run_until_parked();
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.set_mode(crate::layout::Mode::Browse, window, cx));
        });
        vc.run_until_parked();
        let focused = vc.update(|window, cx| {
            let harness = baaz.read(cx);
            harness.browser.read(cx).browse_address.as_ref().is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
        });
        assert!(focused, "the new-tab field holds the keyboard after entering Browse");
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.new_browse_sidebar_tab(window, cx));
        });
        vc.run_until_parked();
        let focused = vc.update(|window, cx| {
            let harness = baaz.read(cx);
            harness.browser.read(cx).browse_address.as_ref().is_some_and(|input| input.read(cx).focus_handle(cx).is_focused(window))
        });
        assert!(focused, "the new-tab field holds the keyboard after ⌘T");
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
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.set_mode(crate::layout::Mode::Browse, window, cx);
                harness.step_tab_new("https://www.wikipedia.org/", window, cx).expect("tab-new opens wikipedia");
                harness.step_tab_new("https://docs.rs/", window, cx).expect("tab-new opens docs.rs");
            });
        });
        vc.run_until_parked();
        let count = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| {
                let harness = baaz.read(cx);
                let space = harness.browser.read(cx).browse_space();
                harness.browser.read(cx).tabs.person_tabs(space.as_deref()).len()
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
            vc.update(|_, cx| crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse),
            "still in Browse, window alive"
        );
    }

    #[gpui::test]
    fn cmd_w_on_the_last_tab_closes_the_window_and_leaves_a_blank_tab(cx: &mut gpui::TestAppContext) {
        assert_eq!(
            crate::app::BROWSE_ROOT_CONTEXT,
            format!("{} {}", aui::keys::ROOT_CONTEXT, crate::app::BROWSE_CONTEXT)
        );
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("cmdw-last");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.set_mode(crate::layout::Mode::Browse, window, cx);
                harness.step_tab_new("https://www.wikipedia.org/", window, cx).expect("tab-new opens wikipedia");
            });
        });
        vc.run_until_parked();
        let count = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| {
                let harness = baaz.read(cx);
                let space = harness.browser.read(cx).browse_space();
                harness.browser.read(cx).tabs.person_tabs(space.as_deref()).len()
            })
        };
        // Entering Browse made the blank tab; ⌘T made the real one. Close
        // the real one, then the last: the window closes (CloseWindow,
        // the same hide as ⌘W in Sessions) and one fresh blank tab stands
        // ready for its return. Only the real page joins the closed list.
        let before = count(vc);
        assert_eq!(before, 2, "a blank tab and the real one: {before}");
        vc.update(|window, cx| {
            let root = baaz.read(cx).focus_root.clone();
            window.focus(&root, cx);
        });
        vc.run_until_parked();
        vc.simulate_keystrokes("cmd-w");
        vc.run_until_parked();
        assert_eq!(count(vc), 1, "⌘W closed the real tab");
        vc.simulate_keystrokes("cmd-w");
        vc.run_until_parked();
        assert_eq!(count(vc), 1, "⌘W on the last tab leaves one fresh blank tab");
        let closed: Vec<String> = vc.update(|_, cx| baaz.read(cx).browser.read(cx).tabs.closed.iter().map(|tab| tab.url.clone()).collect());
        assert_eq!(closed, vec!["https://www.wikipedia.org/".to_owned()], "the blank tabs never join the closed list");
        assert!(
            vc.update(|_, cx| baaz.read(cx).browser.read(cx).tabs.person_tabs(baaz.read(cx).browser.read(cx).browse_space().as_deref()).iter().all(|tab| super::tabs::is_blank_url(&tab.url))),
            "the tab left behind is blank"
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
                crate::runtime::CaptureToken::default(),
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
                crate::layout::Layout::stage(cx, |layout| {
                    layout.right_open = true;
                    layout.right_kind = Some(crate::layout::RightKind::Browser);
                });
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
            vc.update(|_, cx| baaz.read(cx).settings_open(cx)),
            "⌘, from the page opens Settings"
        );
        routing_restore(state);
    }

    /// B2a-H1: two tabs in one session are two live pages; only the active
    /// tab's page is unobscured; the agent registration follows the active
    /// tab across a switch; and forcing discard drops the hidden page but
    /// keeps its tab for a rebuild.
    /// D3: a Browse sidebar row routes by owner — a task-folder tab
    /// selects into the centre and closes under its session's key (the
    /// person paths refuse it), a person tab still takes the person path.
    #[gpui::test]
    fn browse_sidebar_tab_routes_session_tabs_by_owner(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("sidebar-owner");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = super::Owner::Session("owner-probe".to_owned());
                let tab = harness.browser.update(cx, |browser, _| browser.tabs.open(None, owner, "https://a.example/", None));
                harness.browse_sidebar_tab(tab, false, window, cx);
                assert_eq!(harness.browser.read(cx).browse_session_tab, Some(tab), "a session tab selects into the centre");
                harness.browse_sidebar_tab(tab, true, window, cx);
                assert!(harness.browser.read(cx).tabs.get(tab).is_none(), "a session tab closes");
                assert_eq!(harness.browser.read(cx).browse_session_tab, None, "closing the shown session tab clears the centre");
            });
        });
    }

    #[gpui::test]
    fn two_session_tabs_hold_two_pages_with_one_live(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(|cx| cx.bind_keys(crate::keymap::build_bindings()));
        let state = routing_state("tabs");
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                routing_args(&state.2),
                crate::runtime::CaptureToken::default(),
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
                let first = harness.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://a.example/", None));
                let second = harness.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://b.example/", Some(first)));
                let page_first = harness.ensure_tab_page("tab-probe", first, window, cx);
                let page_second = harness.ensure_tab_page("tab-probe", second, window, cx);
                assert!(page_first != page_second, "each tab holds its own page");
                assert_eq!(
                    harness.browser.read(cx).states.len(),
                    2,
                    "two tabs in one session are two live pages"
                );
                // The newest tab is active; the agent tools point at it.
                assert_eq!(harness.browser.read(cx).tabs.active_of(&owner, None), Some(second));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_second.clone()),
                    "registration follows the active tab"
                );
                // Only the active tab's page is unobscured.
                harness.browser.update(cx, |browser, cx| browser.apply_page_visibility(Some(second), true, None, false, cx));
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
                assert_eq!(harness.browser.read(cx).tabs.active_of(&owner, None), Some(first));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-probe"),
                    Some(page_first.clone()),
                    "registration follows the switch"
                );
                harness.browser.update(cx, |browser, cx| browser.apply_page_visibility(Some(first), true, None, false, cx));
                assert!(!page_first.read(cx).obscured());
                assert!(page_second.read(cx).obscured());
                // Scoped discard drops only the hidden page: the tab keeps
                // its URL, and showing it again rebuilds at that URL.
                harness.discard_hidden_tabs(true, Some(first), None, cx);
                assert_eq!(
                    harness.browser.read(cx).states.len(),
                    1,
                    "the hidden page is discarded, the shown page kept"
                );
                assert!(
                    harness.browser.read(cx).tabs.get(second).is_some(),
                    "the discarded tab survives with its URL"
                );
                let rebuilt = harness.ensure_tab_page("tab-probe", second, window, cx);
                assert_eq!(rebuilt.read(cx).url().as_ref(), "https://b.example/");
                // Back on the first tab, the step drops the hidden second
                // page at once but spares the agent's active tab — the
                // live probe drives this verb.
                harness.select_tab("tab-probe", first, window, cx);
                harness.step_tab_discard_now(window, cx).expect("tab-discard-now runs");
                assert_eq!(
                    harness.browser.read(cx).states.len(),
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
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = super::Owner::Session("tab-close".to_owned());
                harness.session_list.update(cx, |list, _| {
                    list.overrides.insert(
                        "tab-close".to_owned(),
                        crate::sessions::SessionMeta {
                            right: Some(crate::sessions::RightState {
                                browser_url: Some("https://seed.example/".into()),
                                ..Default::default()
                            }),
                            ..Default::default()
                        },
                    );
                });
                let first = harness.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://a.example/", None));
                let second = harness.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://b.example/", Some(first)));
                let page_second = harness.ensure_tab_page("tab-close", second, window, cx);
                assert_eq!(harness.browser.read(cx).tabs.active_of(&owner, None), Some(second));
                // Closing the background tab: its page drops, and nothing
                // else moves — no rebuild, no re-registration.
                harness.close_tab("tab-close", first, window, cx);
                assert!(!harness.browser.read(cx).states.contains_key(&first));
                assert_eq!(harness.browser.read(cx).states.len(), 1);
                assert_eq!(harness.browser.read(cx).tabs.active_of(&owner, None), Some(second));
                assert_eq!(
                    harness.terminal_service.test_browser_state("tab-close"),
                    Some(page_second.clone()),
                    "a background close never re-registers"
                );
                // Closing the active last tab: the agent tools lose the
                // session's page and the stored URL is forgotten.
                harness.close_tab("tab-close", second, window, cx);
                assert!(harness.browser.read(cx).states.is_empty());
                assert!(
                    harness
                        .terminal_service
                        .test_browser_state("tab-close")
                        .is_none(),
                    "no page survives the last close through the agent tools"
                );
                assert!(
                    harness
                        .session_list.read(cx).overrides
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
                crate::runtime::CaptureToken::default(),
                window,
                cx,
            )
        });
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.ensure_tabs_loaded(cx);
                let owner = Owner::Person;
                let first = harness.browser.update(cx, |browser, _| browser.tabs.open(Some("p1".into()), owner.clone(), "https://a.example/", None));
                let second = harness.browser.update(cx, |browser, _| browser.tabs.open(Some("p1".into()), owner.clone(), "https://b.example/", Some(first)));
                assert_eq!(
                    harness.browser.read(cx).tabs.active_of(&owner, Some("p1")),
                    Some(second)
                );
                let page_first = harness.ensure_tab_page("home", first, window, cx);
                let _page_second = harness.ensure_tab_page("home", second, window, cx);
                // Switch projects: the strip now shows p2 (empty), but the
                // close still targets p1's active tab.
                harness.current_project = Some("p2".into());
                harness.close_tab("home", second, window, cx);
                assert_eq!(
                    harness.browser.read(cx).tabs.active_of(&owner, Some("p1")),
                    Some(first),
                    "the model neighbours within the tab's own space"
                );
                assert_eq!(
                    harness.browser.read(cx).agent_tabs.get("home"),
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

#[cfg(all(test, feature = "cef"))]
mod page_wheel_tests {
    use super::{page_wheel_claimed, page_wheel_event, PANE_PAGE_LIVE};
    use aui_cef::{PageWheel, WheelPhase};

    fn swipe(dx: f64, dy: f64, phase: WheelPhase) -> PageWheel {
        PageWheel { x: 1200.0, y: 300.0, dx, dy, phase, precise: true }
    }

    /// D6 follow-up: a Shift + sideways swipe over the pane's live page is
    /// the strip's; a vertical one, or any swipe while the pane shows no
    /// page (Browse mode's page, a closed pane), stays the page's.
    #[test]
    fn only_a_sideways_swipe_over_the_panes_page_is_claimed() {
        PANE_PAGE_LIVE.with(|live| live.set(true));
        assert!(page_wheel_claimed(&swipe(-40.0, 3.0, WheelPhase::Moved)));
        assert!(!page_wheel_claimed(&swipe(4.0, 30.0, WheelPhase::Moved)), "vertical scrolls the page");
        PANE_PAGE_LIVE.with(|live| live.set(false));
        assert!(!page_wheel_claimed(&swipe(-40.0, 3.0, WheelPhase::Moved)), "not the pane's page");
    }

    /// The replay is the event the strip's listener expects: Shift held,
    /// pixel deltas, the gesture's phase, at the pointer.
    #[test]
    fn a_claimed_swipe_replays_as_a_shift_wheel() {
        let event = page_wheel_event(&swipe(-40.0, 3.0, WheelPhase::Started));
        assert!(event.modifiers.shift);
        assert_eq!(event.touch_phase, gpui::TouchPhase::Started);
        assert_eq!(event.position, gpui::point(gpui::px(1200.0), gpui::px(300.0)));
        assert!(matches!(event.delta, gpui::ScrollDelta::Pixels(p) if p == gpui::point(gpui::px(-40.0), gpui::px(3.0))));
    }
}
