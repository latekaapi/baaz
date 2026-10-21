//! The overlay window: a borderless pop-up NSPanel over the main window that
//! hosts what must draw above a native web page — Browse's right-click
//! menus today, the Field (⌘L) next (owner decision 2026-10-11; the aui-cef
//! spike 3e91eb3 proved a gpui `PopUp` orders above the CEF child window).
//!
//! One overlay at a time. [`Harness::sync_overlay`] runs every frame from
//! `on_frame`: it opens the window when something wants one, moves it when
//! the anchor moves, and closes it when nothing does. A `PopUp` does not
//! follow its parent, so the main window's frame is part of the anchor.
//!
//! The panel takes the keyboard when it opens, so a click anywhere else (the
//! window, the page, another app) resigns it and closes the menu, and Escape
//! inside it closes it too. Intents raised inside the overlay are queued and
//! run on the main window's next frame: focus, pages and the rename editor
//! all live there, never in the panel.

use aui::interact::{menu_surface, Shape};
use aui::screens::sidebar::{browse_menu, browse_menu_size, SidebarIntent};
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, Density, TextRole};
use gpui_kit::base::{h_flex, v_flex};
use gpui::{
    div, prelude::*, AnyWindowHandle, App, Bounds, Context, FocusHandle, Pixels, Render, WeakEntity, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use crate::app::Harness;

/// `BAAZ_OVERLAY_TRACE=1`: one stderr line per overlay open / close (live checks).
fn trace(what: impl std::fmt::Display) {
    if std::env::var("BAAZ_OVERLAY_TRACE").as_deref() == Ok("1") {
        eprintln!("overlay: {what}");
    }
}

/// What an overlay window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayKind {
    /// Browse's open right-click menu (`Harness::browse_menu`).
    BrowseMenu,
    /// The Field (⌘L, a click on the address line): the address input and
    /// its rows, anchored on the sidebar's address line, over the live page.
    Field,
    /// The site-information panel (the shield): connection and certificate,
    /// under the address line, over the live page.
    SiteInfo,
    /// Browse's modals — the ⌘K palettes and the dialogs — with their scrim,
    /// over the whole window and the live page. Drawn in the main window
    /// they could only cover the page with a picture of it, which arrived
    /// after the scrim had already dimmed the sidebar (owner recording
    /// 2026-10-11 5.23 PM); here the page stays live and dims with the rest.
    Modal,
    /// The corner stack (D4, docs/41 §2.11) in Browse: the newest toast
    /// over the feedback slot, 12 above the sheet's bottom and 12 from its
    /// right, over the live page. Its own window beside the one above —
    /// it coexists with a menu or the Field — and it never takes the
    /// keyboard, so it never closes on a resign: it lives while it has
    /// something to show.
    Corner,
    /// D6 (docs/43 §2.7): what floats over the right pane's native page —
    /// the agent pill while the agent acts, the notes panel while
    /// annotating. Never takes the keyboard, like the corner.
    PageFloat,
    /// D6 follow-up (docs/60 §3 item 3): a new pin's note popover over the
    /// right pane's native page — Cancel / Save and the note's text field.
    /// The one page overlay that takes the keyboard: drawn in the main
    /// window it sat under the page, and nothing could be typed.
    PageNote,
}

/// A notes-panel click, run on the main window.
type NotesClick = std::rc::Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

/// The site-information panel's width: FF's dropdown `w-80`, 320.
const SITE_INFO_WIDTH: f32 = scale::SP_7 * 10.0;

/// The Field panel's width (canvas board "Field", 2026-10-11): 600.
const FIELD_WIDTH: f32 = scale::SP_7 * 18.75;

/// The clear margin around every overlay panel inside its window: room for
/// FF's shadow-3 (a 1 px outer ring and a drop reaching 4.5 px below), which
/// the window's edge would otherwise clip — the panel's only edge, since the
/// window draws none of its own ([`drop_native_chrome`]). 6 px: `SP_1` is
/// 2 px, and the 2 px margin it was clipped the drop (and the dropdown's
/// 4 px enter slide).
const SHADOW_PAD: f32 = scale::SP_2 + scale::SP_1;
/// The note popover's box (aui `note_popover`: 236 wide; path, a two-line
/// field and the buttons).
const NOTE_W: f32 = 236.0;
const NOTE_H: f32 = 132.0;

/// Take macOS's own chrome off an overlay window, and keep it from ever
/// becoming the main window.
///
/// - **Shadow.** gpui opens a `PopUp` as a titled panel, so the window server
///   shadows the WHOLE window rect: around the Field's fixed transparent rect
///   that drew an empty box below the rows (owner screenshot 2026-10-11).
/// - **Border.** A titled window also draws its own 1 px rim — the faint line
///   that stayed above the tab menu after the shadow went (owner screenshot
///   2026-10-11 7.14 PM). Menus, the Field and site information drop the
///   titled bit and become borderless panels (gpui's panel class still
///   answers `canBecomeKeyWindow`, so the Field types). The modal keeps it:
///   it covers the whole window and needs the window server's rounded
///   corners, and its rim lies on the main window's own.
/// - **Main.** gpui's panel class answers YES to `canBecomeMainWindow`, so a
///   panel taking the keyboard took main from Baaz's window and greyed its
///   traffic lights (owner, same screenshot: Claude Desktop's menus never do).
///   A panel is a utility: key, never main ([`never_main_panels`]).
///
/// Every overlay window goes through here, whatever it shows.
#[cfg(target_os = "macos")]
fn drop_native_chrome(window: &Window, kind: OverlayKind) {
    use objc2::{msg_send, runtime::AnyObject};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let view = appkit.ns_view.as_ptr() as *mut AnyObject;
    // SAFETY: `view` is the live NSView gpui just made for this window, on
    // the main thread; these are plain AppKit setters on its window.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setHasShadow: false];
        if matches!(kind, OverlayKind::Corner | OverlayKind::PageFloat) {
            // The corner never closes on a resign, so it must not float
            // over other apps: it hides with Baaz and returns with it.
            let _: () = msg_send![ns_window, setHidesOnDeactivate: true];
        }
        if kind != OverlayKind::Modal {
            // Borderless (no titled bit), keeping the two bits gpui's panel
            // relies on: full-size content (its frame maths) and
            // non-activating (a click in it never activates the app anew).
            const FULL_SIZE_CONTENT: usize = 1 << 15;
            const NON_ACTIVATING_PANEL: usize = 1 << 7;
            let _: () = msg_send![ns_window, setStyleMask: FULL_SIZE_CONTENT | NON_ACTIVATING_PANEL];
        }
    }
}

/// Makes gpui's panel class answer NO to `canBecomeMainWindow`, once, before
/// the first overlay opens: gpui makes a new panel key — and, unpatched,
/// main — while creating it, so patching from the window's own setup left
/// the first panel of a run main (seen live). The class is registered when
/// the binary loads (gpui's `build_classes` ctor). Every gpui panel in Baaz
/// is one of these overlays (Baaz opens no `Floating` or `Dialog` window), so
/// the class-wide change is exactly the overlays'.
#[cfg(target_os = "macos")]
fn never_main_panels() {
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    static ONCE: std::sync::Once = std::sync::Once::new();
    extern "C-unwind" fn no(_: *mut AnyObject, _: Sel) -> Bool {
        Bool::NO
    }
    ONCE.call_once(|| {
        let Some(class) = AnyClass::get(c"GPUIPanel") else {
            return;
        };
        // SAFETY: gpui's `GPUIPanel` (a `ClassDecl` class), a selector with
        // the `B@:` signature `no` implements; main thread.
        unsafe {
            let class = class as *const AnyClass as *mut AnyClass;
            let imp: Imp = std::mem::transmute::<extern "C-unwind" fn(*mut AnyObject, Sel) -> Bool, Imp>(no);
            objc2::ffi::class_replaceMethod(class, objc2::sel!(canBecomeMainWindow), imp, c"B@:".as_ptr());
        }
    });
}

#[cfg(not(target_os = "macos"))]
fn drop_native_chrome(_: &Window, _: OverlayKind) {}

/// The open overlay: its window, what it shows, and where (screen space).
pub(crate) struct Overlay {
    handle: AnyWindowHandle,
    kind: OverlayKind,
    bounds: Bounds<Pixels>,
    /// What the content showed at its last refresh ([`Harness::overlay_print`]).
    print: u64,
}

/// The overlay's root view: draws the Harness-built content for its kind.
pub(crate) struct OverlayView {
    baaz: WeakEntity<Harness>,
    kind: OverlayKind,
    focus: FocusHandle,
    /// Whether the panel has held the keyboard yet: only a resign after that
    /// closes it (a panel that never became key — the app was in the
    /// background — must not close on its own birth).
    seen_active: bool,
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let content = self.baaz.update(cx, |baaz, cx| baaz.overlay_content(kind, window, cx)).ok().flatten();
        // The hosted modal's keyboard: its query field, the palette or the
        // dialog, focused here in the panel (the main window never draws it).
        if kind == OverlayKind::Modal {
            if let Some(handle) = self.baaz.read_with(cx, |baaz, cx| baaz.modal_focus(cx)).ok().flatten() {
                if !handle.is_focused(window) {
                    window.focus(&handle, cx);
                }
            }
        }
        // The note's field: made by the pane's render, which can land after
        // this window opened, so it is focused here on each render (as the
        // modal's query field is) rather than once at open.
        if kind == OverlayKind::PageNote {
            let input = self.baaz.read_with(cx, |baaz, cx| baaz.existing_browser(cx).and_then(|state| state.read(cx).open_note()).and_then(|note| note.input)).ok().flatten();
            if let Some(input) = input {
                let handle = gpui::Focusable::focus_handle(input.read(cx), cx);
                if !handle.is_focused(window) {
                    // The input's own focus (as the Field's), not a bare
                    // handle focus: it also arms the input's text handling.
                    input.update(cx, |input, cx| input.focus(window, cx));
                }
                trace(format_args!("note field focused={}", handle.is_focused(window)));
            }
        }
        trace(format_args!("render {kind:?} content={} active={}", content.is_some(), window.is_window_active()));
        if kind == OverlayKind::Corner {
            return div().size_full().p(gpui::px(SHADOW_PAD)).flex().flex_col().justify_end().items_end().children(content);
        }
        if kind == OverlayKind::PageFloat && self.baaz.read_with(cx, |baaz, cx| baaz.right_cache.page_menu && !baaz.pane_page_covered(cx)).unwrap_or(false) {
            return div().size_full().children(content);
        }
        if kind == OverlayKind::PageFloat {
            return div().size_full().p(gpui::px(SHADOW_PAD)).flex().flex_col().justify_end().items_center().children(content);
        }
        let baaz = self.baaz.clone();
        div()
            .size_full()
            .track_focus(&self.focus)
            .on_key_down(move |event: &gpui::KeyDownEvent, _, cx: &mut App| {
                if event.keystroke.key == "escape" {
                    let _ = baaz.update(cx, |baaz, cx| baaz.close_overlay_content(kind, cx));
                }
            })
            .children(content)
    }
}

impl Harness {
    /// What the overlay should show now, and where in screen space: the
    /// Browse menu at its pointer anchor (window coordinates, offset by the
    /// main window's origin), sized for the menu and its submenu.
    fn wanted_overlay(&self, window: &Window, cx: &App) -> Option<(OverlayKind, Bounds<Pixels>)> {
        if self.mode != crate::layout::Mode::Browse {
            return None;
        }
        if self.modal_overlay_wanted(cx) {
            // The whole window: the scrim covers everything the in-window one
            // did. The panel is titled like the main window, so the window
            // server rounds it the same.
            return Some((OverlayKind::Modal, window.bounds()));
        }
        if self.field_overlay_wanted(cx) {
            // One fixed transparent rect from the address line down: the
            // panel draws at its top and grows inside it, so typing never
            // resizes the window; a click on the clear part is outside.
            let metrics = cx.aui().metrics;
            let top = metrics.header + metrics.item_px / 2.0;
            // The panel sits at (item_px, top); the window reaches the
            // shadow margin past it on every side.
            let pad = gpui::px(SHADOW_PAD);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(scale::SP_7 * 15.0)) + pad;
            return Some((OverlayKind::Field, Bounds::new(origin, gpui::size(gpui::px(FIELD_WIDTH) + pad * 2.0, height))));
        }
        if self.site_info_open && self.sidebar_open && self.browse_centre_state().is_some() {
            // Under the address line, inside a clear rect like the Field's.
            let metrics = cx.aui().metrics;
            let pad = gpui::px(SHADOW_PAD);
            let top = metrics.header + metrics.item_px / 2.0 + metrics.control_lg + gpui::px(scale::SP_1);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(scale::SP_7 * 15.0)) + pad;
            return Some((OverlayKind::SiteInfo, Bounds::new(origin, gpui::size(gpui::px(SITE_INFO_WIDTH) + pad * 2.0, height))));
        }
        let menu = self.browse_menu.as_ref()?;
        let space = self.browse_space().unwrap_or_default();
        let folders = self.browser.tabs.folders_in(&space).len();
        let size = browse_menu_size(menu, folders, cx);
        // The menu sits at the pointer; its window reaches the shadow
        // margin past it on every side ([`OverlayView`] pads the content).
        let pad = gpui::px(SHADOW_PAD);
        let origin = window.bounds().origin + menu.at - gpui::point(pad, pad);
        Some((OverlayKind::BrowseMenu, Bounds::new(origin, gpui::size(size.width + pad * 2.0, size.height + pad * 2.0))))
    }

    /// Open, move or close the overlay window to match what is wanted.
    /// Window changes are deferred past this frame (this runs inside render).
    pub(crate) fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.main_window.get_or_insert_with(|| window.window_handle());
        let wanted = self.wanted_overlay(window, cx);
        let current = self.overlay.as_ref().map(|o| (o.kind, o.bounds)).or(self.overlay_pending);
        if wanted == current {
            // The content reads Harness state: repaint the panel only when what
            // it shows changed (a blind refresh per main frame looped — the
            // panel's render dirtied the main window again). Its own hover and
            // typing repaint it through the entities it reads.
            let print = self.overlay_print(cx);
            if let Some(overlay) = self.overlay.as_mut().filter(|o| o.print != print) {
                overlay.print = print;
                let handle = overlay.handle;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        if let Some(old) = self.overlay.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.overlay_pending = wanted;
        trace(format_args!("want {wanted:?} (had {current:?})"));
        let Some((kind, bounds)) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        never_main_panels();
        // Opened from a task, at app level: never inside this update (the
        // overlay's first draw reads Harness) and never inside render.
        let baaz = cx.entity().downgrade();
        let address = if kind == OverlayKind::Field { self.browse_address.clone() } else { None };
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: true,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    drop_native_chrome(window, kind);
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        // The Field types into the shared address input, here.
                        if let Some(input) = address {
                            input.update(cx, |input, cx| input.focus(window, cx));
                        }
                        // Resigning key (a click outside, a switch away) closes it.
                        cx.observe_window_activation(window, move |this: &mut OverlayView, window, cx| {
                            if window.is_window_active() {
                                this.seen_active = true;
                            } else if this.seen_active
                                && this.kind != OverlayKind::Modal
                                && std::env::var("BAAZ_OVERLAY_STICKY").as_deref() != Ok("1")
                            {
                                // A modal stays through a switch away, as it
                                // did in the main window; its scrim answers
                                // clicks and Escape closes it.
                                // `BAAZ_OVERLAY_STICKY=1` (captures of a background
                                // launch, where the app never holds the keyboard)
                                // keeps the panel up past a resign.
                                let kind = this.kind;
                                let _ = this.baaz.update(cx, |baaz, cx| baaz.close_overlay_content(kind, cx));
                            }
                        })
                        .detach();
                        OverlayView { baaz, kind, focus, seen_active: window.is_window_active() }
                    })
                })
            });
            trace(format_args!("opened {kind:?}: {}", if opened.is_ok() { "ok" } else { "failed" }));
            let _ = this.update(cx, |this, cx| {
                // Still wanted? Keep it; otherwise it goes straight away.
                match opened {
                    Ok(handle) if this.overlay_pending == Some((kind, bounds)) => {
                        this.overlay_pending = None;
                        let print = this.overlay_print(cx);
                        this.overlay = Some(Overlay { handle: handle.into(), kind, bounds, print });
                    }
                    Ok(handle) => {
                        let handle: AnyWindowHandle = handle.into();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        });
                    }
                    Err(_) => this.overlay_pending = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Where the corner window should be, in screen space, while Browse
    /// has a stack to show: 12 above the sheet's bottom, 12 from its right,
    /// the window reaching the shadow margin past the pills. Wide enough
    /// for the widest toast; the pills sit at its bottom-right and the
    /// clear rest passes the pointer through to the page.
    fn wanted_corner(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if !self.corner_in_overlay() || self.feedback.capturing {
            return None;
        }
        let toast = !self.overlays.read(cx).toasts.is_empty();
        let slot = self.layout.feedback_mode || self.feedback.forced_on;
        let pills = usize::from(toast) + usize::from(slot);
        if pills == 0 {
            return None;
        }
        let sheet = self.browser.centre_page_rect?;
        let pad = gpui::px(SHADOW_PAD);
        let pill = aui::feedback::PILL_H;
        let height = gpui::px(pill * pills as f32 + 8.0 * (pills as f32 - 1.0)) + pad * 2.0;
        let width = gpui::px(aui::feedback::TOAST_MAX_W) + pad * 2.0;
        let right = sheet.right() - gpui::px(12.0) + pad;
        let bottom = sheet.bottom() - gpui::px(12.0) + pad;
        let origin = window.bounds().origin + gpui::point(right - width, bottom - height);
        Some(Bounds::new(origin, gpui::size(width, height)))
    }

    /// Open, move or close the corner window to match [`Self::wanted_corner`]
    /// — the same deferred dance as [`Self::sync_overlay`], minus the
    /// keyboard: the panel opens unfocused and nothing it does resigns it.
    pub(crate) fn sync_corner(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_corner(window, cx);
        if wanted.is_some() {
            // The slot draws in this window now: the main window's record
            // of it would silence the capture layer at a stale spot.
            self.feedback.slot_bounds = None;
        }
        let current = self.corner.as_ref().map(|o| o.bounds).or(self.corner_pending);
        if wanted == current {
            let print = self.corner_print(cx);
            if let Some(corner) = self.corner.as_mut().filter(|o| o.print != print) {
                corner.print = print;
                let handle = corner.handle;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        if let Some(old) = self.corner.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.corner_pending = wanted;
        trace(format_args!("corner want {wanted:?} (had {current:?})"));
        let Some(bounds) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        never_main_panels();
        let baaz = cx.entity().downgrade();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: false,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    drop_native_chrome(window, OverlayKind::Corner);
                    cx.new(|cx| OverlayView { baaz, kind: OverlayKind::Corner, focus: cx.focus_handle(), seen_active: false })
                })
            });
            trace(format_args!("opened Corner: {}", if opened.is_ok() { "ok" } else { "failed" }));
            let _ = this.update(cx, |this, cx| {
                match opened {
                    Ok(handle) if this.corner_pending == Some(bounds) => {
                        this.corner_pending = None;
                        let print = this.corner_print(cx);
                        this.corner = Some(Overlay { handle: handle.into(), kind: OverlayKind::Corner, bounds, print });
                    }
                    Ok(handle) => {
                        let handle: AnyWindowHandle = handle.into();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        });
                    }
                    Err(_) => this.corner_pending = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// What the Page screen floats over its native page (docs/43 §2.7),
    /// and where, in screen space: the notes panel 8 in from the page's
    /// foot while annotating, else the agent pill 16 above it while the
    /// agent acts. `None` when the pane is not on a native page, the strip
    /// is moving, or nothing floats.
    fn wanted_page_float(&self, window: &Window, cx: &App) -> Option<(Bounds<Pixels>, u64)> {
        if self.mode != crate::layout::Mode::Sessions || !self.layout.right_open || self.right_strip_moving || self.settings_page.open {
            return None;
        }
        if crate::layout::right_kind(&self.layout) != crate::layout::RightKind::Browser || self.right_cache.pushed.contains_key(&2) {
            return None;
        }
        let state = self.existing_browser(cx)?;
        let page = state.read(cx);
        if !page.is_native() || page.obscured() {
            return None;
        }
        let rect = page.page_rect()?;
        let pad = gpui::px(SHADOW_PAD);
        let origin = window.bounds().origin;
        if self.right_cache.page_menu && !crate::right_page::page_covered(page, &self.right_cache) {
            // The Recent pages menu: the window covers the nav row and the
            // page, so a click anywhere over them outside the menu closes it.
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            ("menu", page.url().to_string(), page.recent_pages().len()).hash(&mut h);
            let top = rect.top() - gpui::px(aui::workbench::PAGE_NAV_H);
            let size = gpui::size(rect.size.width, rect.size.height + gpui::px(aui::workbench::PAGE_NAV_H));
            return Some((Bounds::new(origin + gpui::point(rect.left(), top), size), h.finish()));
        }
        if page.annotating() {
            let rows = page.annotations().len() as f32;
            let height = gpui::px(12.0 + 48.0 * rows + 40.0) + pad * 2.0;
            let width = rect.size.width - gpui::px(16.0) + pad * 2.0;
            let bottom = rect.bottom() - gpui::px(8.0) + pad;
            let left = rect.left() + gpui::px(8.0) - pad;
            let print = 1 + page.annotations().len() as u64 * 7;
            return Some((Bounds::new(origin + gpui::point(left, bottom - height), gpui::size(width, height)), print));
        }
        let session = self.active.as_ref()?.read(cx).session_id.clone();
        let label = self.terminal_service.browser_activity(&session)?;
        let width = gpui::px((70.0 + 7.2 * label.chars().count() as f32).min(f32::from(rect.size.width) - 16.0)) + pad * 2.0;
        let height = gpui::px(32.0) + pad * 2.0;
        let bottom = rect.bottom() - gpui::px(16.0) + pad;
        let left = rect.left() + (rect.size.width - width) / 2.0;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        label.hash(&mut h);
        Some((Bounds::new(origin + gpui::point(left, bottom - height), gpui::size(width, height)), h.finish()))
    }

    /// Where the note popover floats, in screen space: clear of the pin's
    /// element (14 right of it, 14 down — the pane's own placement), held
    /// inside the page. `None` unless a note is open over the pane's live
    /// native page.
    fn wanted_page_note(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if self.mode != crate::layout::Mode::Sessions || !self.layout.right_open || self.right_strip_moving || self.settings_page.open {
            return None;
        }
        if crate::layout::right_kind(&self.layout) != crate::layout::RightKind::Browser || self.right_cache.pushed.contains_key(&2) {
            return None;
        }
        let state = self.existing_browser(cx)?;
        let page = state.read(cx);
        if !page.is_native() || page.obscured() {
            return None;
        }
        let note = page.open_note()?;
        let (origin, size) = (note.origin, note.size);
        let rect = page.page_rect()?;
        let pad = gpui::px(SHADOW_PAD);
        let (w, h) = (gpui::px(NOTE_W) + pad * 2.0, gpui::px(NOTE_H) + pad * 2.0);
        let left = (rect.left() + gpui::px(origin.0 + size.0 + 14.0) - pad).min(rect.right() - w).max(rect.left());
        let top = (rect.top() + gpui::px(origin.1 + 14.0) - pad).min(rect.bottom() - h).max(rect.top());
        Some(Bounds::new(window.bounds().origin + gpui::point(left, top), gpui::size(w, h)))
    }

    /// Open, move or close the note window. It takes the keyboard on open
    /// and puts it in the note's field; it closes when the note does (Save,
    /// Cancel, Escape) and moves by reopening, like the float.
    pub(crate) fn sync_page_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_page_note(window, cx);
        let current = self.page_note.as_ref().map(|o| o.bounds).or(self.page_note_pending);
        if wanted == current {
            return;
        }
        if let Some(old) = self.page_note.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.page_note_pending = wanted;
        let Some(bounds) = wanted else {
            return;
        };
        let input = self.existing_browser(cx).and_then(|state| state.read(cx).open_note()).and_then(|note| note.input);
        #[cfg(target_os = "macos")]
        never_main_panels();
        let baaz = cx.entity().downgrade();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: true,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    drop_native_chrome(window, OverlayKind::PageNote);
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        if let Some(input) = input {
                            input.update(cx, |input, cx| input.focus(window, cx));
                        }
                        OverlayView { baaz, kind: OverlayKind::PageNote, focus, seen_active: true }
                    })
                })
            });
            let _ = this.update(cx, |this, cx| {
                match opened {
                    Ok(handle) if this.page_note_pending == Some(bounds) => {
                        this.page_note_pending = None;
                        this.page_note = Some(Overlay { handle: handle.into(), kind: OverlayKind::PageNote, bounds, print: 0 });
                    }
                    Ok(handle) => {
                        let handle: AnyWindowHandle = handle.into();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        });
                    }
                    Err(_) => this.page_note_pending = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The note popover in its window: the pin's path, the shared note
    /// field, Cancel / Save. Its buttons answer the page's state.
    fn render_page_note(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let state = self.existing_browser(cx)?;
        let aui_webview::OpenNote { path, input, .. } = state.read(cx).open_note()?;
        let mut popover = aui::workbench::note_popover("page-note", path, "").on_action(move |action, _, cx| {
            state.update(cx, |state, cx| state.answer_note(action, cx));
        });
        if let Some(input) = input {
            popover = popover.input(input);
        }
        Some(div().p(gpui::px(SHADOW_PAD)).child(popover).into_any_element())
    }

    /// Open, move or close the Page float window (the corner's dance).
    pub(crate) fn sync_page_float(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_page_float(window, cx);
        let current = self.page_float.as_ref().map(|o| o.bounds).or(self.page_float_pending);
        if wanted.map(|w| w.0) == current {
            if let (Some((_, print)), Some(float)) = (wanted, self.page_float.as_mut()) {
                if float.print != print {
                    float.print = print;
                    let handle = float.handle;
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.refresh());
                    });
                }
            }
            return;
        }
        if let Some(old) = self.page_float.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.page_float_pending = wanted.map(|w| w.0);
        let Some((bounds, print)) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        never_main_panels();
        let baaz = cx.entity().downgrade();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: false,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    drop_native_chrome(window, OverlayKind::PageFloat);
                    cx.new(|cx| OverlayView { baaz, kind: OverlayKind::PageFloat, focus: cx.focus_handle(), seen_active: false })
                })
            });
            let _ = this.update(cx, |this, cx| {
                match opened {
                    Ok(handle) if this.page_float_pending == Some(bounds) => {
                        this.page_float_pending = None;
                        this.page_float = Some(Overlay { handle: handle.into(), kind: OverlayKind::PageFloat, bounds, print });
                    }
                    Ok(handle) => {
                        let handle: AnyWindowHandle = handle.into();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        });
                    }
                    Err(_) => this.page_float_pending = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The Page float's content: the notes panel or the agent pill. Their
    /// clicks run on the main window, where the page's state lives.
    fn render_page_float(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let state = self.existing_browser(cx)?;
        let page = state.read(cx);
        let agent = self.active.as_ref().map(|v| v.read(cx).session_id.clone()).map(|id| self.session_lane_provider(&id));
        let name = agent.map(|p| p.label()).unwrap_or("Muse");
        if self.right_cache.page_menu && !crate::right_page::page_covered(page, &self.right_cache) {
            let rows = crate::right_page::recent_rows(page);
            let width = page.page_rect().map(|r| f32::from(r.size.width)).unwrap_or(400.0);
            let menu = crate::right_page::recent_menu(&rows, crate::right_page::recent_menu_room(true, width), cx);
            let close = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| this.set_page_menu(false, cx));
            return Some(
                div()
                    .size_full()
                    .relative()
                    .on_mouse_down(gpui::MouseButton::Left, close)
                    .child({
                        // The window starts at the bar's top: the menu hangs
                        // from the address field, 2 px under the bar.
                        let (left, top) = crate::right_page::recent_menu_seat(true, width);
                        div().absolute().left(gpui::px(left)).top(gpui::px(top)).on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(menu)
                    })
                    .into_any_element(),
            );
        }
        if page.annotating() {
            let notes: Vec<aui::workbench::PageNote> = page
                .annotations()
                .iter()
                .map(|a| aui::workbench::PageNote { index: a.index, selector: a.selector.clone(), note: a.note.clone() })
                .collect();
            let weak = cx.weak_entity();
            let main = self.main_window;
            let act = |action: aui::workbench::AnnotatorAction| -> NotesClick {
                let state = state.clone();
                let baaz = weak.clone();
                std::rc::Rc::new(move |_, _, cx: &mut App| {
                    let state = state.clone();
                    let baaz = baaz.clone();
                    if let Some(main) = main {
                        let _ = main.update(cx, move |_, window, cx| {
                            let intent = state.update(cx, |s, cx| s.annotator_action(action, cx));
                            if let Some(intent) = intent {
                                let _ = baaz.update(cx, |h, cx| h.handle_browser_intent(intent, window, cx));
                            }
                        });
                    }
                })
            };
            let clear = act(aui::workbench::AnnotatorAction::Clear);
            let send = act(aui::workbench::AnnotatorAction::Send);
            // Under 360 the footer cannot hold the hint and "Send N to <agent>".
            let compact = page.page_rect().is_some_and(|r| f32::from(r.size.width) < 360.0);
            return Some(aui::workbench::notes_panel("page-float-notes", &notes, name, Some(clear), Some(send), compact, cx).into_any_element());
        }
        let session = self.active.as_ref()?.read(cx).session_id.clone();
        let label = self.terminal_service.browser_activity(&session)?;
        let mark = match agent {
            Some(crate::providers::ProviderId::ClaudeCode) => aui_icons::Provider::Claude,
            Some(crate::providers::ProviderId::Codex) => aui_icons::Provider::Codex,
            _ => aui_icons::Provider::Muse,
        };
        Some(aui::workbench::agent_pill("page-float-pill", mark, format!("{name} is {label}"), cx).into_any_element())
    }

    /// A fingerprint of what the corner shows: the toasts, and the
    /// feedback slot's phase, count, arm and flash.
    fn corner_print(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for toast in &self.overlays.read(cx).toasts {
            toast.id.to_string().hash(&mut hasher);
        }
        format!("{:?}", self.feedback.phase).hash(&mut hasher);
        self.feedback.notes_label().hash(&mut hasher);
        self.feedback.discard_armed_live().hash(&mut hasher);
        self.feedback.done_enabled(cx).hash(&mut hasher);
        self.feedback.saved_flash.as_ref().is_some_and(|f| f.at.elapsed() < crate::app::feedback_ui::FEEDBACK_SAVED_FLASH).hash(&mut hasher);
        hasher.finish()
    }

    /// A fingerprint of what the overlay content shows: the open menu and the
    /// folders it lists, or the Field's rows and highlight.
    fn overlay_print(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.browse_menu).hash(&mut hasher);
        self.browser.tabs.generation.hash(&mut hasher);
        self.site_info_open.hash(&mut hasher);
        self.browse_centre_shown().hash(&mut hasher);
        if self.browse_address.is_some() {
            self.browse_address_text(cx).hash(&mut hasher);
            self.browse_omnibox_selected.hash(&mut hasher);
        }
        // The hosted modal: which palette and row, what is typed, how many
        // rows the query found (results land later than the keystroke), and
        // which dialog.
        let overlays = self.overlays.read(cx);
        if let Some(palette) = overlays.palette.as_ref() {
            format!("{:?}", palette.kind).hash(&mut hasher);
            palette.selected.hash(&mut hasher);
            self.palette_rows(palette.kind, cx).len().hash(&mut hasher);
            self.search_status(cx).hash(&mut hasher);
            for query in [&self.search_query, &self.projects_query, &self.commands_query] {
                query.read(cx).value().hash(&mut hasher);
            }
        }
        if let Some(dialog) = overlays.dialog.as_ref() {
            dialog.title.hash(&mut hasher);
            dialog.detail.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Whether Browse's modal (a palette or a dialog) draws in the overlay
    /// window rather than the main one: always, in Browse — one host per
    /// mode, so a page loading under an open palette never moves it.
    pub(crate) fn modal_overlay_wanted(&self, cx: &App) -> bool {
        let overlays = self.overlays.read(cx);
        self.mode == crate::layout::Mode::Browse && (overlays.palette.is_some() || overlays.dialog.is_some())
    }

    /// The handle the hosted modal's keyboard belongs to: the palette's own
    /// query field where it has one, else the palette or the dialog.
    pub(crate) fn modal_focus(&self, cx: &App) -> Option<FocusHandle> {
        use crate::overlays::PaletteKind;
        use gpui::Focusable as _;
        let overlays = self.overlays.read(cx);
        if let Some(palette) = overlays.palette.as_ref() {
            return Some(match palette.kind {
                PaletteKind::Search => self.search_query.focus_handle(cx),
                PaletteKind::Projects => self.projects_query.focus_handle(cx),
                PaletteKind::Commands => self.commands_query.focus_handle(cx),
                _ => self.focus_palette.clone(),
            });
        }
        overlays.dialog.is_some().then(|| self.focus_dialog.clone())
    }

    /// Whether the Field shows as an overlay: the address is being edited in
    /// Browse with the sidebar (vertical tabs) open and the page is not the new-tab page
    /// (which carries the Field inline).
    pub(crate) fn field_overlay_wanted(&self, cx: &App) -> bool {
        self.mode == crate::layout::Mode::Browse
            && self.sidebar_open
            && !self.settings_open()
            && self.browse_address.is_some()
            && !(self.browse_centre_state().is_some_and(|state| matches!(state.read(cx).url().as_ref(), "" | crate::browser::tabs::BLANK))
                && self.browse_centre_tab_blank())
    }

    /// The overlay's content for `kind`: the Browse menu panel, its intents
    /// queued for the main window.
    pub(crate) fn overlay_content(&mut self, kind: OverlayKind, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        match kind {
            OverlayKind::BrowseMenu => {
                let model = self.sidebar_model(cx);
                let browse = model.browse?;
                let baaz = cx.entity().downgrade();
                let on_intent = std::rc::Rc::new(move |intent: SidebarIntent, _: &mut Window, cx: &mut App| {
                    let _ = baaz.update(cx, |baaz, cx| baaz.queue_sidebar_intent(intent, cx));
                });
                let menu = browse_menu("overlay-browse-menu", &browse, Some(on_intent), window, cx)?;
                Some(div().p(gpui::px(SHADOW_PAD)).child(menu).into_any_element())
            }
            OverlayKind::Field => Some(self.render_field(window, cx)),
            OverlayKind::SiteInfo => Some(self.render_site_info(cx)),
            OverlayKind::Corner => {
                self.hosting_overlay = true;
                let stack = self.render_corner_stack(cx);
                self.hosting_overlay = false;
                stack
            }
            OverlayKind::PageFloat => self.render_page_float(cx),
            OverlayKind::PageNote => self.render_page_note(cx),
            OverlayKind::Modal => {
                // The same elements the main window draws, their listeners
                // routed back to it (`main_listener`).
                self.hosting_overlay = true;
                let modal = self.render_palette(cx).or_else(|| self.render_dialog(window, cx));
                self.hosting_overlay = false;
                Some(div().size_full().relative().children(modal).into_any_element())
            }
        }
    }

    /// The Field panel (canvas board "Field"): FF's dropdown chrome, the
    /// address input pinned on top (FF InputField focused: `ring-border`),
    /// then the ranked rows in groups — open tabs, then search and ask —
    /// on 36 px rows, the highlighted one on the hover overlay. ↑/↓ move,
    /// Tab completes, Enter opens, ⌘↵ asks, Escape leaves.
    fn render_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let rows = self.browse_suggestions(cx);
        let selected = self.browse_omnibox_selected.min(rows.len().saturating_sub(1));
        let keys = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
            "up" => this.move_omnibox(-1, cx),
            "down" => this.move_omnibox(1, cx),
            "tab" => this.complete_omnibox(window, cx),
            "escape" => this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
            _ => {}
        });
        let input = self.browse_address.clone().map(|input| {
            aui::a11y_text::a11y_text_input("field-input", "Search or type a URL", &input, cx).flex_1().min_w(gpui::px(0.0)).into_any_element()
        });
        // FF `DropdownSearch` (dropdown-search.tsx): the field is a row of
        // the popup, not a box inside it — full bleed through the 4 px
        // padding (`-mx-1 -mt-1`), 36 tall, no ring, a `border/60` divider
        // under it and 2 px (`mb-0.5`) before the first row; its icon is lit
        // (foreground, stroke 2) while it holds focus, which is always here.
        // Adaptation: the leading slot keeps the rows' 24 px icon axis, so
        // the icon and text line up with the rows below.
        // With no rows (an empty field) the input is the whole panel: no
        // divider, no gap, bleeding through the bottom padding too — a
        // divider over nothing read as a double edge (owner 2026-10-11).
        let inset = metrics.item_px / 2.0;
        let alone = rows.is_empty();
        let field = h_flex()
            .id("field-row")
            .h(metrics.control)
            .mx(-inset)
            .mt(-inset)
            .when(alone, |d| d.mb(-inset))
            .when(!alone, |d| {
                d.mb(gpui::px(2.0)).border_b_1().border_color({
                    let mut line = p.line;
                    line.a *= 0.6;
                    line
                })
            })
            .pl(inset)
            .pr(metrics.item_px + inset)
            .items_center()
            .gap(metrics.gap)
            .text_color(p.fg)
            .text_role_in(TextRole::Body, Density::Standard)
            .on_key_down(keys)
            .child(div().flex_none().w(metrics.item_px + metrics.icon).flex().justify_center().child(icon(IconName::Search).size(metrics.icon).color(p.fg).lit(true)))
            .children(input)
            .child(div().flex_none().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child("esc"));
        // FF DropdownSearch: one fluid highlight the pointer and the arrows
        // both drive; a hover moves the selection (it was keyboard-only).
        let report = cx.entity().downgrade();
        let mut rows_list = aui::interact::fluid_list("field-rows", aui::interact::Axis::Y)
            .label("Suggestions")
            .gap(gpui::px(0.0))
            .highlighted(Some(selected))
            .on_highlight(move |index, _, cx| {
                if let Some(index) = index {
                    let _ = report.update(cx, |this, cx| {
                        if this.browse_omnibox_selected != index {
                            this.browse_omnibox_selected = index;
                            cx.notify();
                        }
                    });
                }
            });
        let mut last: Option<&'static str> = None;
        for (index, row) in rows.iter().enumerate() {
            let group = match row.suggestion.kind {
                // The ranking puts Ask first and Search last (the omnibox
                // rule), so each kind keeps its own label.
                crate::browser::SuggestKind::Ask => "Ask",
                crate::browser::SuggestKind::Tab | crate::browser::SuggestKind::Session => "Switch to",
                crate::browser::SuggestKind::History => "History",
                crate::browser::SuggestKind::Search => "Search",
            };
            if last != Some(group) {
                rows_list = rows_list.item_static(
                    Harness::OMNI_HEADER_BASE + index,
                    div().h(metrics.control_md).mt(metrics.item_px / 2.0).px(metrics.item_px).flex().items_center().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(group),
                );
                last = Some(group);
            }
            let lit = index == selected;
            let glyph = match row.suggestion.kind {
                crate::browser::SuggestKind::Ask => IconName::Sparkle,
                crate::browser::SuggestKind::Search => IconName::Search,
                crate::browser::SuggestKind::Session => IconName::List,
                _ => IconName::Globe,
            };
            let press = cx.listener(move |this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
                this.run_on_main(cx, move |this, window, cx| this.activate_omnibox_row(index, window, cx));
            });
            let mut item = h_flex()
                .h(metrics.control)
                .w_full()
                .items_center()
                .gap(metrics.gap)
                .pr(metrics.item_px)
                .child(div().flex_none().w(metrics.item_px + metrics.icon).flex().justify_center().child(icon(glyph).size(metrics.icon).color(if lit { p.fg } else { p.fg_muted }).lit(lit)))
                .child(
                    div()
                        .flex_shrink(1.0)
                        .min_w(gpui::px(0.0))
                        .truncate()
                        .text_role_in(TextRole::Body, Density::Standard)
                        .text_color(if lit { p.fg } else { p.fg_muted })
                        .child(row.suggestion.title.clone()),
                )
                .child(div().flex_1());
            if !row.suggestion.hint.is_empty() {
                item = item.child(div().flex_none().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child(row.suggestion.hint.clone()));
            }
            rows_list = rows_list.item(
                index,
                div()
                    .id(("field-row", index))
                    .role(gpui::Role::ListBoxOption)
                    .aria_label(row.suggestion.title.clone())
                    .cursor_pointer()
                    .on_mouse_down(gpui::MouseButton::Left, press)
                    .child(Shape::Item.rounded(item)),
            );
        }
        let list = v_flex().w_full().gap(gpui::px(0.0)).child(field).child(rows_list);
        let _ = window;
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx));
        });
        // The clear rest of the rect is outside: a press there leaves the Field.
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(gpui::px(SHADOW_PAD))
            .on_mouse_down(gpui::MouseButton::Left, leave)
            .child(
                div()
                    .w_full()
                    .occlude()
                    .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(menu_surface(div().id("field-panel").w_full().p(metrics.item_px / 2.0).role(gpui::Role::Dialog).aria_label("Open a page, switch tab or ask").child(list))),
            )
            .into_any_element()
    }

    /// The site-information panel (owner 2026-10-11: "certificate details
    /// like Chrome"): the host, whether the connection is secure, and the
    /// certificate — issued to, issued by, validity, protocol, SHA-256 — on
    /// FF's dropdown chrome (container radius, p-1, caption group labels,
    /// border/60 separators). Read from the engine when it opens.
    fn render_site_info(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let state = self.browse_centre_state();
        let (url, security, cert) = match state.as_ref() {
            Some(state) => {
                let page = state.read(cx);
                (page.url().to_string(), page.security(), page.certificate())
            }
            None => (String::new(), aui_webview::PageSecurity::Unknown, None),
        };
        let host = url.split("://").nth(1).and_then(|rest| rest.split('/').next()).unwrap_or(&url).to_owned();
        let secure = cert.is_some() || security == aui_webview::PageSecurity::Secure;
        let mut separator = p.line;
        separator.a *= 0.6;
        let inset = metrics.item_px / 2.0;
        let label = |text: &'static str, cx: &App| {
            div().h(metrics.control_md).px(metrics.item_px).flex().items_center().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(text)
        };
        let field = |name: &'static str, value: String, cx: &App| {
            v_flex()
                .px(metrics.item_px)
                .py(gpui::px(scale::SP_1))
                .child(div().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(name))
                .child(div().text_role_in(TextRole::Body, Density::Standard).text_color(p.fg).child(value))
        };
        let (headline, detail) = if secure {
            ("Connection is secure", "Information you send to this site, like passwords or card numbers, stays private.")
        } else {
            ("Connection is not secure", "Don't enter passwords or card details on this site: others can read what you send.")
        };
        let mut panel = v_flex()
            .id("site-info")
            .w_full()
            .p(inset)
            .role(gpui::Role::Dialog)
            .aria_label(format!("Site information for {host}"))
            .child(
                div()
                    .px(metrics.item_px)
                    .pt(metrics.item_px)
                    .pb(gpui::px(scale::SP_1))
                    .text_role_in(TextRole::Body, Density::Standard)
                    .semibold()
                    .text_color(p.fg)
                    .truncate()
                    .child(host),
            )
            .child(
                h_flex()
                    .px(metrics.item_px)
                    .py(gpui::px(scale::SP_2))
                    .gap(metrics.gap)
                    .items_start()
                    .child(div().flex_none().pt(gpui::px(2.0)).child(icon(IconName::Shield).size(metrics.icon).color(if secure { p.fg } else { p.danger }).lit(true)))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w(gpui::px(0.0))
                            .child(div().text_role_in(TextRole::Body, Density::Standard).text_color(if secure { p.fg } else { p.danger }).child(headline))
                            .child(div().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(detail)),
                    ),
            );
        if let Some(cert) = cert {
            let date = |secs: i64| {
                chrono::DateTime::from_timestamp(secs, 0).map(|d| d.with_timezone(&chrono::Local).format("%-d %b %Y").to_string()).unwrap_or_default()
            };
            let named = |name: &str, org: &Option<String>| match org {
                Some(org) if org != name => format!("{name} · {org}"),
                _ => name.to_owned(),
            };
            panel = panel
                .child(div().my(inset).mx(-inset).h(gpui::px(1.0)).bg(separator))
                .child(label("Certificate", cx))
                .child(field("Issued to", named(&cert.subject, &cert.subject_org), cx))
                .child(field("Issued by", named(&cert.issuer, &cert.issuer_org), cx))
                .child(field("Valid", format!("{} – {}", date(cert.valid_from), date(cert.valid_to)), cx));
            if let Some(protocol) = cert.protocol.clone() {
                panel = panel.child(field("Connection", protocol, cx));
            }
            if !cert.sha256.is_empty() {
                panel = panel.child(
                    v_flex()
                        .px(metrics.item_px)
                        .pt(gpui::px(scale::SP_1))
                        .pb(metrics.item_px)
                        .child(div().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child("SHA-256 fingerprint"))
                        .child(div().role_in(TextRole::Micro, cx).font_family(aui_tokens::scale::FONT_MONO).text_color(p.fg).child(cert.sha256.clone())),
                );
            }
        }
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            this.site_info_open = false;
            cx.notify();
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(gpui::px(SHADOW_PAD))
            .on_mouse_down(gpui::MouseButton::Left, leave)
            .child(div().w_full().occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(menu_surface(panel)))
            .into_any_element()
    }

    /// Run `f` against the main window once the current event has finished:
    /// focus, pages and the address field's owner all live there.
    pub(crate) fn run_on_main(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static) {
        let Some(main) = self.main_window else {
            return;
        };
        let baaz = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| {
                let _ = baaz.update(cx, |baaz, cx| f(baaz, window, cx));
            });
        });
    }

    /// `cx.listener` for content the overlay window may host: drawn in the
    /// main window it is exactly `cx.listener`; drawn in the overlay
    /// ([`Harness::hosting_overlay`]) the handler runs on the main window's
    /// next turn instead, so whatever it opens, focuses or measures lands
    /// there — never in the panel that is about to close.
    pub(crate) fn main_listener<E: Clone + 'static>(
        &self,
        cx: &mut Context<Self>,
        f: impl Fn(&mut Self, &E, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl Fn(&E, &mut Window, &mut App) + 'static {
        let hosted = self.hosting_overlay;
        let baaz = cx.entity().downgrade();
        let f = std::rc::Rc::new(f);
        move |event: &E, window: &mut Window, cx: &mut App| {
            if hosted {
                let (event, f) = (event.clone(), f.clone());
                let _ = baaz.update(cx, |this, cx| this.run_on_main(cx, move |this, window, cx| f(this, &event, window, cx)));
            } else {
                let _ = baaz.update(cx, |this, cx| f(this, event, window, cx));
            }
        }
    }

    /// Close what the overlay shows (Escape inside it, or it lost the key).
    pub(crate) fn close_overlay_content(&mut self, kind: OverlayKind, cx: &mut Context<Self>) {
        trace(format_args!("close {kind:?}"));
        match kind {
            OverlayKind::BrowseMenu => self.close_browse_menu(cx),
            OverlayKind::Corner | OverlayKind::PageFloat => {}
            // Escape in the note cancels it, as the popover's Cancel does.
            OverlayKind::PageNote => {
                if let Some(state) = self.existing_browser(cx) {
                    state.update(cx, |state, cx| state.answer_note(aui::workbench::NoteAction::Cancel, cx));
                }
                cx.notify();
            }
            // Leaving the Field also hands focus back: on the main window.
            OverlayKind::Field => self.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
            OverlayKind::SiteInfo => {
                self.site_info_open = false;
                cx.notify();
            }
            OverlayKind::Modal => {
                self.overlays.update(cx, |overlays, _| overlays.palette = None);
                if self.overlays.read(cx).dialog.is_some() {
                    self.close_dialog(cx);
                }
                cx.notify();
            }
        }
    }

    /// An intent from inside the overlay: run against the main window (where
    /// focus and pages live) once this event has finished.
    pub(crate) fn queue_sidebar_intent(&mut self, intent: SidebarIntent, cx: &mut Context<Self>) {
        self.run_on_main(cx, move |baaz, window, cx| baaz.sidebar_intent(intent, window, cx));
    }
}
