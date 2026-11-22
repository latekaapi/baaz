//! The overlay window: a borderless pop-up NSPanel over the main window that
//! hosts what must draw above a native web page — Browse's right-click
//! menus today, the Field (⌘L) next (owner decision 2026-10-11; the aui-cef
//! spike 3e91eb3 proved a gpui `PopUp` orders above the CEF child window).
//!
//! One overlay at a time. What it shows is the overlay stack's
//! ([`crate::overlay_stack`]): the entries hosted in the panel (Browse's
//! menu, site information, the bookmark popover, a modal in Browse) and the
//! footer popovers, plus the Field while the address is edited.
//! [`Harness::sync_overlay`] runs from what moves it — the overlay stack, the
//! Browse view, the layout, the window's bounds — never from a frame
//! ([`Harness::sync_floats`], task 4.9): it opens the window when something
//! wants one, moves it when the anchor moves, and closes it when nothing
//! does. A `PopUp` does not follow its parent, so the main window's frame is
//! part of the anchor.
//!
//! The panel takes the keyboard when it opens, so a click anywhere else (the
//! window, the page, another app) resigns it and closes the menu, and Escape
//! inside it closes it too. Intents raised inside the overlay are queued and
//! run against the main window once the event finishes
//! ([`Harness::run_on_main`]): focus, pages and the rename editor all live
//! there, never in the panel.

use aui::interact::menu_panel;
use aui::screens::sidebar::{browse_menu, browse_menu_size, SidebarIntent};
use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
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

/// The key window and its first responder, for `BAAZ_OVERLAY_TRACE` lines.
#[cfg(target_os = "macos")]
pub(crate) fn key_window_desc() -> String {
    use objc2::{msg_send, runtime::AnyObject};
    // SAFETY: plain AppKit getters on the main thread; nil is checked.
    unsafe {
        let app: *mut AnyObject = msg_send![objc2::class!(NSApplication), sharedApplication];
        let key: *mut AnyObject = msg_send![app, keyWindow];
        if key.is_null() {
            return String::from("key=none");
        }
        let name = |object: *mut AnyObject| -> String {
            if object.is_null() {
                return String::from("nil");
            }
            let class: *mut AnyObject = msg_send![object, className];
            let utf8: *const std::os::raw::c_char = msg_send![class, UTF8String];
            std::ffi::CStr::from_ptr(utf8).to_string_lossy().into_owned()
        };
        let responder: *mut AnyObject = msg_send![key, firstResponder];
        let number: isize = msg_send![key, windowNumber];
        format!("key={}#{number} responder={}", name(key), name(responder))
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn key_window_desc() -> String {
    String::new()
}

/// What an overlay window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayKind {
    /// Browse's open right-click menu (the overlay stack's `BrowseMenu`).
    BrowseMenu,
    /// The Field (⌘L, a click on the address line): the address input and
    /// its rows, anchored on the sidebar's address line, over the live page.
    Field,
    /// The site-information panel (the shield): connection and certificate,
    /// under the address line, over the live page.
    SiteInfo,
    /// The sidebar's downloads list, under its button, over the live page.
    Downloads,
    /// R4: the bookmark popover (⌘D, the ★), under the address line. It
    /// types (the name), so it takes the keyboard like the Field.
    Bookmark,
    /// Browse's modals — the ⌘K palettes and the dialogs — with their scrim,
    /// over the whole window and the live page. Drawn in the main window
    /// they could only cover the page with a picture of it, which arrived
    /// after the scrim had already dimmed the sidebar (owner recording
    /// 2026-10-11 5.23 PM); here the page stays live and dims with the rest.
    Modal,
    /// The corner stack (D4, docs/41 §2.11) in Browse: the newest toast,
    /// 12 above the page's bottom and 12 from its
    /// right, over the live page. Its own window beside the one above —
    /// it coexists with a menu or the Field — and it never takes the
    /// keyboard, so it never closes on a resign: it lives while it has
    /// something to show.
    Corner,
    /// D6 (docs/43 §2.7): what floats over the right pane's native page —
    /// the agent pill while the agent acts, the notes panel while
    /// annotating. Never takes the keyboard, like the corner.
    PageFloat,
    /// R3 (docs/27 §4.5): the agent card floating over the page, or the
    /// same card docked as a column beside it. Takes the keyboard (its
    /// steering input); hides with Baaz like the page float.
    AgentCard,
    /// docs/82 8.6: the agent cursor over a native page — a clear window the
    /// size of the page that ignores the pointer, so every click reaches
    /// the page under it. Never takes the keyboard.
    AgentCursor,
}

mod bookmark;
mod card;
pub(crate) use bookmark::BookmarkView;
use crate::layout::LayoutGlobal as _;

/// A notes-panel click, run on the main window.
type NotesClick = std::rc::Rc<dyn Fn(&gpui::ClickEvent, &mut Window, &mut App)>;

/// The site-information panel's width: FF's dropdown `w-80`, 320.
const SITE_INFO_WIDTH: f32 = scale::SP_7 * 10.0;

/// The Field panel's width (canvas board "Field", 2026-10-11): 600.
/// The order the Field draws Baaz's ranked rows in: the caption-less rows
/// (Ask, Search, open) first, then each captioned group in the order its
/// first row ranked. Returns indices into `rows`.
fn field_display_order(rows: &[aui::browse::field::FieldRow]) -> Vec<usize> {
    use aui::browse::field::FieldRowKind;
    let group = |kind: &FieldRowKind| match kind {
        FieldRowKind::Tab { .. } => 1,
        FieldRowKind::Folder { .. } => 2,
        FieldRowKind::Bookmark { .. } => 3,
        FieldRowKind::History { .. } => 4,
        FieldRowKind::Command { .. } => 5,
        _ => 0,
    };
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| group(&rows[i].kind));
    order
}

/// The frame to ask for so an R3 float's content box is `bounds`. Measured
/// live (8 Nov): a non-modal float comes out 6 px in from its requested frame
/// on every side once `drop_native_chrome` swaps its style mask — the Field
/// asked 664 wide and got 652, the line 355 tall and got 343 — so the line
/// wrapped at a width it was not measured at and its edge was cut.
pub(crate) fn r3_frame(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    let inset = gpui::px(FRAME_LOSS);
    Bounds::new(bounds.origin - gpui::point(inset, inset), bounds.size + gpui::size(inset * 2.0, inset * 2.0))
}

/// What a non-modal float loses from its requested frame, per side.
const FRAME_LOSS: f32 = 6.0;

/// Spare height above the line in its float window (see `wanted_agent_line`).
const LINE_SLACK: f32 = 0.0;

/// The tallest the Field's window gets: the input row, the 512 px body and
/// the footer. Its width and top are the palette's (docs/83 §7:
/// `aui::overlay::palette_width`, `palette_top`).
const FIELD_MAX_H: f32 = 46.0 + 512.0 + 32.0 + 2.0;

/// The clear margin around every overlay panel inside its window: room for
/// the near part of the float shadow, which the window's edge would
/// otherwise clip (the panel's edge is its own 1 px `line-strong` border;
/// the window draws none of its own, [`drop_native_chrome`]). 6 px: `SP_1` is
/// 2 px, and the 2 px margin it was clipped the drop (and the dropdown's
/// 4 px enter slide).
pub(crate) const SHADOW_PAD: f32 = scale::SP_2 + scale::SP_1;

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
///
/// **Never inside an App update** (Gate 4, 19 Nov: gpui logged "handling
/// resize … RefCell already borrowed"). Swapping the style mask resizes the
/// panel's view (the 6 px per side [`r3_frame`] makes up for), and AppKit
/// tells gpui at once, from inside the `setStyleMask:` call: gpui's resize
/// callback borrows the App, so while `open_window`'s build closure (or any
/// update) held it the resize was dropped and the float kept laying out at
/// the size it asked for. The open tasks call this after `open_window`
/// returns, between updates: the window is borrowed only to read its view,
/// and the AppKit setters run with nothing borrowed.
#[cfg(target_os = "macos")]
fn drop_native_chrome(handle: AnyWindowHandle, kind: OverlayKind, cx: &mut gpui::AsyncApp) {
    use objc2::{msg_send, runtime::{AnyObject, Bool}};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let view = handle.update(cx, |_, window, _| {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::AppKit(appkit) => Some(appkit.ns_view.as_ptr() as *mut AnyObject),
            _ => None,
        }
    });
    let Ok(Some(view)) = view else {
        return;
    };
    // SAFETY: `view` is the live NSView gpui made for this window (it was
    // read above, and no run-loop turn has passed since), on the main
    // thread; these are plain AppKit setters on its window.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if ns_window.is_null() {
            return;
        }
        let _: () = msg_send![ns_window, setHasShadow: false];
        // No float may draw over other apps: each hides with Baaz and
        // returns with it. The corner, line and card never close on a
        // resign; the Field, menus and modals do — but only after they have
        // held the keyboard (`seen_active`), so one that opened while Baaz
        // was not frontmost stayed up over every app, Chrome included
        // (owner, 9 Nov: the Field over the whole screen).
        let _: () = msg_send![ns_window, setHidesOnDeactivate: true];
        if kind == OverlayKind::AgentCursor {
            // Clicks fall through to the page: the cursor only shows.
            let _: () = msg_send![ns_window, setIgnoresMouseEvents: true];
        }
        if kind != OverlayKind::Modal {
            // Borderless (no titled bit), keeping the two bits gpui's panel
            // relies on: full-size content (its frame maths) and
            // non-activating (a click in it never activates the app anew).
            const FULL_SIZE_CONTENT: usize = 1 << 15;
            const NON_ACTIVATING_PANEL: usize = 1 << 7;
            let _: () = msg_send![ns_window, setStyleMask: FULL_SIZE_CONTENT | NON_ACTIVATING_PANEL];
            // Changing the style mask leaves the panel itself first responder,
            // so keystrokes stopped at the window and never reached gpui's view:
            // nothing typed in the Field landed and its arrows did nothing
            // (owner 2026-11-06). The modal keeps its mask and was never hit.
            let _: Bool = msg_send![ns_window, makeFirstResponder: view];
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
fn drop_native_chrome(_: AnyWindowHandle, _: OverlayKind, _: &mut gpui::AsyncApp) {}

/// One overlay window's slot: what it shows and where (screen space), and
/// its window once the open task has made it. One value for "wanted" and
/// "open": a slot whose window is still opening has no handle yet, so there
/// is no second "pending" record to keep in step with it.
pub(crate) struct Overlay {
    /// The window, once its open task landed; `None` while it opens.
    handle: Option<AnyWindowHandle>,
    kind: OverlayKind,
    bounds: Bounds<Pixels>,
    /// What the content showed at its last refresh ([`Harness::overlay_print`]).
    print: u64,
}

impl Overlay {
    /// What the slot is for.
    #[cfg(all(test, feature = "probe"))]
    pub(crate) fn kind(&self) -> OverlayKind {
        self.kind
    }

    /// A slot whose window is opening for `kind` at `bounds`.
    fn opening(kind: OverlayKind, bounds: Bounds<Pixels>) -> Self {
        Overlay { handle: None, kind, bounds, print: 0 }
    }

    /// The window to repaint when the content's fingerprint moved off
    /// `print` (recording it), or `None` when nothing changed or the window
    /// is still opening.
    fn stale(&mut self, print: u64) -> Option<AnyWindowHandle> {
        let handle = self.handle.filter(|_| self.print != print)?;
        self.print = print;
        Some(handle)
    }
}

/// Settle `slot` after the open task for `(kind, bounds)` finished. A slot
/// still waiting on exactly that window takes it; a window nobody waits for
/// any more (closed, moved or replaced while it opened) comes back to be
/// removed. A failed open empties the slot that waited on it.
fn land(slot: &mut Option<Overlay>, kind: OverlayKind, bounds: Bounds<Pixels>, opened: Option<AnyWindowHandle>, print: u64) -> Option<AnyWindowHandle> {
    let waiting = slot.as_ref().is_some_and(|open| open.handle.is_none() && open.kind == kind && open.bounds == bounds);
    match opened {
        Some(handle) if waiting => {
            if let Some(open) = slot.as_mut() {
                open.handle = Some(handle);
                open.print = print;
            }
            None
        }
        Some(handle) => Some(handle),
        None => {
            if waiting {
                *slot = None;
            }
            None
        }
    }
}

/// Remove an overlay window, past the current update.
fn remove_window(handle: AnyWindowHandle, cx: &mut App) {
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    });
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
        // The R3 floats draw library components sized in rems: give them
        // the main window's rem (its gpui-kit Root sets the body size), not
        // gpui's 16 px default, so they match the approved gallery and the
        // height functions the windows are sized by.
        let r3_float = matches!(kind, OverlayKind::PageFloat | OverlayKind::AgentCard | OverlayKind::Field);
        if r3_float {
            // What gpui-kit's Root does for the main window: the theme's rem
            // and, on the wrapper below, its font family.
            window.set_rem_size(gpui_kit::component::ActiveTheme::theme(&**cx).font_size);
        }
        let font = gpui_kit::component::ActiveTheme::theme(&**cx).font_family.clone();
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
        // The Field's keyboard: the shared address input, held focused here in
        // the panel on every render, as the modal's query field is.
        // The bookmark popover's keyboard: its name input.
        if kind == OverlayKind::Bookmark {
            let view = self.baaz.read_with(cx, |baaz, _| baaz.overlays.read(cx).stack.bookmark().cloned()).ok().flatten();
            if let Some(view) = view {
                let handle = gpui::Focusable::focus_handle(view.read(cx), cx);
                if !handle.is_focused(window) {
                    window.focus(&handle, cx);
                }
            }
        }
        if kind == OverlayKind::Field {
            let input = self.baaz.read_with(cx, |baaz, _| baaz.browser.read(cx).browse_address.clone()).ok().flatten();
            if let Some(input) = input {
                let handle = gpui::Focusable::focus_handle(input.read(cx), cx);
                trace(format_args!("field input focused={}", handle.is_focused(window)));
                if !handle.is_focused(window) {
                    window.focus(&handle, cx);
                }
            }
        }
        trace(format_args!("{}", key_window_desc()));
        trace(format_args!("render {kind:?} content={} active={}", content.is_some(), window.is_window_active()));
        if kind == OverlayKind::Corner {
            return div().size_full().p(gpui::px(SHADOW_PAD)).flex().flex_col().justify_end().items_end().children(content);
        }
        if kind == OverlayKind::AgentCursor {
            return div().size_full().relative().children(content);
        }
        if kind == OverlayKind::PageFloat && self.baaz.read_with(cx, |baaz, cx| baaz.right_pane.read(cx).page_menu_open(cx) && !baaz.pane_page_covered(cx)).unwrap_or(false) {
            return div().size_full().children(content);
        }
        // The agent window's keys work while its own float holds the
        // keyboard (the main window's Browse bindings cannot see it): ⌘J
        // the line, ⌘E the card, ⌘⇧E the column, Escape closes.
        let agent_keys = {
            let baaz = self.baaz.clone();
            move |event: &gpui::KeyDownEvent, _: &mut Window, cx: &mut App| {
                let k = &event.keystroke;
                let cmd = k.modifiers.platform && !k.modifiers.control && !k.modifiers.alt;
                let _ = baaz.update(cx, |baaz, cx| match (cmd, k.modifiers.shift, k.key.as_str()) {
                    (true, false, "j") => baaz.run_on_main(cx, |baaz, window, cx| baaz.toggle_agent_line(window, cx)),
                    (true, false, "e") => baaz.toggle_agent_card(cx),
                    (true, true, "e") => baaz.toggle_agent_column(cx),
                    (false, false, "escape") => baaz.escape_agent_window(kind, cx),
                    _ => {}
                });
            }
        };
        if kind == OverlayKind::PageFloat {
            return div().size_full().font_family(font).p(gpui::px(SHADOW_PAD)).flex().flex_col().justify_end().items_center().on_key_down(agent_keys).children(content);
        }
        if kind == OverlayKind::AgentCard {
            return div().size_full().font_family(font).track_focus(&self.focus).on_key_down(agent_keys).children(content);
        }
        let baaz = self.baaz.clone();
        div()
            .size_full()
            .when(r3_float, |d| d.font_family(font))
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
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse {
            return None;
        }
        if self.modal_overlay_wanted(cx) {
            // The whole window: the scrim covers everything the in-window one
            // did. The panel is titled like the main window, so the window
            // server rounds it the same.
            return Some((OverlayKind::Modal, window.bounds()));
        }
        if self.field_overlay_wanted(cx) {
            // One fixed transparent rect, centred on the window with its top
            // at 18 % of the height, like the palette (docs/83 §7): the panel
            // draws at its top and grows inside it, so typing never resizes
            // the window; a click on the clear part is outside.
            let pad = gpui::px(SHADOW_PAD);
            let win = window.bounds().size;
            let top = aui::overlay::palette_top(win.height);
            let width = aui::overlay::palette_width(win.width) + pad * 2.0;
            let left = (win.width - width) / 2.0;
            let origin = window.bounds().origin + gpui::point(left, top - pad);
            let height = (win.height - top - gpui::px(16.0)).min(gpui::px(FIELD_MAX_H)) + pad;
            return Some((OverlayKind::Field, Bounds::new(origin, gpui::size(width, height))));
        }
        if self.overlays.read(cx).stack.bookmark().is_some() && crate::layout::Layout::get(cx).sidebar_open && self.browse_centre_state(cx).is_some() {
            let metrics = cx.aui().metrics;
            let pad = gpui::px(SHADOW_PAD);
            let top = metrics.header + metrics.item_px / 2.0 + metrics.control_lg + gpui::px(scale::SP_1);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(460.0)) + pad;
            return Some((OverlayKind::Bookmark, Bounds::new(origin, gpui::size(gpui::px(bookmark::BOOKMARK_W) + pad * 2.0, height))));
        }
        if self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::SiteInfo) && crate::layout::Layout::get(cx).sidebar_open && self.browse_centre_state(cx).is_some() {
            // Under the address line, inside a clear rect like the Field's.
            let metrics = cx.aui().metrics;
            let pad = gpui::px(SHADOW_PAD);
            let top = metrics.header + metrics.item_px / 2.0 + metrics.control_lg + gpui::px(scale::SP_1);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(scale::SP_7 * 15.0)) + pad;
            return Some((OverlayKind::SiteInfo, Bounds::new(origin, gpui::size(gpui::px(SITE_INFO_WIDTH) + pad * 2.0, height))));
        }
        if self.browser.read(cx).downloads_overlay_wanted(cx) {
            // Under the button, inside a clear rect like the Field's.
            let anchor = self.browser.read(cx).downloads_anchor?;
            let pad = gpui::px(SHADOW_PAD);
            // R4: the buttons sit in the footer, so the popover opens above
            // (its content sits at the window's bottom edge); a button high
            // in the window still opens below it.
            let above = anchor.top() > window.bounds().size.height / 2.0;
            let width = gpui::px(320.0);
            let left = anchor.left().min(window.bounds().size.width - width - gpui::px(8.0)).max(gpui::px(8.0));
            let (top, room) = if above {
                let room = (anchor.top() - gpui::px(12.0)).min(gpui::px(420.0));
                (anchor.top() - gpui::px(6.0) - room, room)
            } else {
                (anchor.bottom() + gpui::px(6.0), (window.bounds().size.height - anchor.bottom() - gpui::px(12.0)).min(gpui::px(420.0)))
            };
            let origin = window.bounds().origin + gpui::point(left - pad, top - pad);
            let size = gpui::size(width + pad * 2.0, room + pad * 2.0);
            return Some((OverlayKind::Downloads, Bounds::new(origin, size)));
        }
        let menu = &self.browse_menu_now(cx)?;
        let space = self.browser.read(cx).browse_space().unwrap_or_default();
        let folders = self.browser.read(cx).tabs.folders_in(&space).len();
        let size = browse_menu_size(menu, folders, cx);
        // The menu sits at the pointer; its window reaches the shadow
        // margin past it on every side ([`OverlayView`] pads the content).
        let pad = gpui::px(SHADOW_PAD);
        // Kept inside the window: a menu opened near the bottom (the space
        // chips) flips up so its last row ("Delete space") is never cut off.
        let win = window.bounds().size;
        let mut at = menu.at;
        if at.y + size.height > win.height - gpui::px(8.0) {
            at.y = (at.y - size.height).max(gpui::px(8.0));
        }
        if at.x + size.width > win.width - gpui::px(8.0) {
            at.x = (win.width - size.width - gpui::px(8.0)).max(gpui::px(8.0));
        }
        let origin = window.bounds().origin + at - gpui::point(pad, pad);
        Some((OverlayKind::BrowseMenu, Bounds::new(origin, gpui::size(size.width + pad * 2.0, size.height + pad * 2.0))))
    }

    /// Open, move or close the overlay window to match what is wanted.
    /// The window opens from a task and closes deferred, never inside this
    /// update (the overlay's first draw reads Harness).
    pub(crate) fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_overlay(window, cx);
        let current = self.browser.read(cx).overlay.as_ref().map(|o| (o.kind, o.bounds));
        if wanted == current {
            // Nothing open, nothing wanted: no print to take (the palette's
            // rows are not cheap).
            if current.is_none() {
                return;
            }
            // The content reads Harness state: repaint the panel only when what
            // it shows changed (a blind refresh per main frame looped — the
            // panel's render dirtied the main window again). Its own hover and
            // typing repaint it through the entities it reads.
            let print = self.overlay_print(cx);
            let stale = self.browser.update(cx, |browser, _| browser.overlay.as_mut().and_then(|overlay| overlay.stale(print)));
            if let Some(handle) = stale {
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        let old = self.browser.update(cx, |browser, _| std::mem::replace(&mut browser.overlay, wanted.map(|(kind, bounds)| Overlay::opening(kind, bounds))));
        if let Some(handle) = old.and_then(|old| old.handle) {
            remove_window(handle, cx);
        }
        trace(format_args!("want {wanted:?} (had {current:?})"));
        let Some((kind, bounds)) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        never_main_panels();
        // Opened from a task, at app level: never inside this update (the
        // overlay's first draw reads Harness) and never inside render.
        let baaz = cx.entity().downgrade();
        let address = if kind == OverlayKind::Field { self.browser.read(cx).browse_address.clone() } else { None };
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: true,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(if kind == OverlayKind::Field { r3_frame(bounds) } else { bounds })),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
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
            // The native chrome comes off between updates, never inside one.
            if let Ok(opened) = &opened {
                drop_native_chrome((*opened).into(), kind, cx);
            }
            trace(format_args!("opened {kind:?}: {}", if opened.is_ok() { "ok" } else { "failed" }));
            let _ = this.update(cx, |this, cx| {
                // Still wanted? Keep it; otherwise it goes straight away.
                let print = this.overlay_print(cx);
                let opened = opened.ok().map(AnyWindowHandle::from);
                if let Some(unwanted) = this.browser.update(cx, |browser, _| land(&mut browser.overlay, kind, bounds, opened, print)) {
                    remove_window(unwanted, cx);
                }
                // What moved while it opened is the next sync's to catch.
                this.queue_floats(cx);
            });
        })
        .detach();
    }

    /// Where the corner window should be, in screen space, while Browse
    /// has a stack to show: 12 above the page's bottom, 12 from its right,
    /// the window reaching the shadow margin past the pill. Wide enough
    /// for the widest toast; the pill sits at its bottom-right and the
    /// clear rest passes the pointer through to the page.
    fn wanted_corner(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if !self.corner_in_overlay(cx) || self.overlays.read(cx).toasts.is_empty() {
            return None;
        }
        let sheet = self.browser.read(cx).centre_page_rect?;
        let pad = gpui::px(SHADOW_PAD);
        let height = gpui::px(aui::feedback::PILL_H) + pad * 2.0;
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
        let current = self.browser.read(cx).corner.as_ref().map(|o| o.bounds);
        if wanted == current {
            let print = self.corner_print(cx);
            let stale = self.browser.update(cx, |browser, _| browser.corner.as_mut().and_then(|corner| corner.stale(print)));
            if let Some(handle) = stale {
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        let old = self.browser.update(cx, |browser, _| std::mem::replace(&mut browser.corner, wanted.map(|bounds| Overlay::opening(OverlayKind::Corner, bounds))));
        if let Some(handle) = old.and_then(|old| old.handle) {
            remove_window(handle, cx);
        }
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
                cx.open_window(options, move |_, cx| {
                    cx.new(|cx| OverlayView { baaz, kind: OverlayKind::Corner, focus: cx.focus_handle(), seen_active: false })
                })
            });
            // The native chrome comes off between updates, never inside one.
            if let Ok(opened) = &opened {
                drop_native_chrome((*opened).into(), OverlayKind::Corner, cx);
            }
            trace(format_args!("opened Corner: {}", if opened.is_ok() { "ok" } else { "failed" }));
            let _ = this.update(cx, |this, cx| {
                let print = this.corner_print(cx);
                let opened = opened.ok().map(AnyWindowHandle::from);
                if let Some(unwanted) = this.browser.update(cx, |browser, _| land(&mut browser.corner, OverlayKind::Corner, bounds, opened, print)) {
                    remove_window(unwanted, cx);
                }
                this.queue_floats(cx);
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
        if self.settings_open(cx) {
            return None;
        }
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            return self.wanted_agent_line(self.browser.read(cx).centre_page_rect?, window, cx);
        }
        let pane = self.right_pane.read(cx);
        if !crate::layout::Layout::get(cx).right_open || pane.strip_moving {
            return None;
        }
        if crate::layout::right_kind(crate::layout::Layout::get(cx)) != crate::layout::RightKind::Browser || pane.cache.pushed.contains_key(&2) {
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
        if pane.page_menu_open(cx) && !pane.page_covered(page) {
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
        self.wanted_agent_line(rect, window, cx)
    }

    /// The session whose work the line shows: the page on screen's owner, else
    /// whichever session is waiting on the person.
    pub(crate) fn line_session(&self, cx: &App) -> Option<String> {
        let shown = if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            self.browser.read(cx).browse_centre_shown(cx)
                .and_then(|id| self.browser.read(cx).tabs.get(id))
                .and_then(|tab| match &tab.owner {
                    crate::browser::tabs::Owner::Session(session) => Some(session.clone()),
                    crate::browser::tabs::Owner::Person => None,
                })
        } else {
            self.active.as_ref().map(|view| view.read(cx).rt(cx).session_id.clone())
        };
        shown
            .filter(|session| self.terminal_service.agent_activity(session).is_some())
            .or_else(|| self.terminal_service.session_asking())
            // A browser chat working on the person's tab (its grant) drives
            // the line too (R3).
            .or_else(|| {
                (crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse)
                    .then(|| self.browser_chat_id(cx))
                    .flatten()
                    .filter(|session| self.terminal_service.agent_activity(session).is_some())
            })
    }

    /// A session's title as the person named it (or as the list shows it),
    /// held to a line's length: how the driving bar and the agent cursor
    /// name who acts on the page — never the provider (rule 14).
    pub(crate) fn session_title(&self, session: &str, cx: &App) -> String {
        let list = self.session_list.read(cx);
        let title = list
            .overrides
            .get(session)
            .and_then(|meta| meta.name.clone())
            .or_else(|| list.sessions.iter().find(|entry| entry.id == session).map(|entry| entry.label.clone()))
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| "An agent".to_owned());
        const MAX: usize = 40;
        if title.chars().count() > MAX {
            format!("{}\u{2026}", title.chars().take(MAX - 1).collect::<String>().trim_end())
        } else {
            title
        }
    }

    /// Where the agent cursor sits over the Browse page (docs/82 8.6): the
    /// point a session's page tool last acted on, on the page shown, while
    /// that tool runs and the person has not taken the wheel — in the
    /// page's own coordinates (its zoom applied) — and its accessible name.
    pub(crate) fn agent_cursor_spot(&self, cx: &App) -> Option<(gpui::Point<Pixels>, String)> {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse || self.settings_open(cx) || self.library_open(cx) {
            return None;
        }
        let state = self.browse_centre_state(cx)?;
        let (session, (x, y)) = self.terminal_service.driving(&state)?;
        let scale = f32::from(state.read(cx).zoom()) / 100.0;
        let at = gpui::point(gpui::px(x as f32 * scale), gpui::px(y as f32 * scale));
        Some((at, format!("{} is acting here", self.session_title(&session, cx))))
    }

    /// The agent cursor in a layer the size of the page, for whichever
    /// window draws over it: the main window over a drawn page, the cursor
    /// float over a native one.
    pub(crate) fn agent_cursor_layer(&self, window: &mut Window, cx: &mut App) -> Option<gpui::AnyElement> {
        let (at, label) = self.agent_cursor_spot(cx)?;
        Some(div().absolute().inset_0().child(aui::browse::agent_cursor("browse-agent-cursor", at, label, window, cx)).into_any_element())
    }

    /// The cursor float's frame over a native page, in screen space, and a
    /// print of what it shows. `None` when no cursor shows or the page is
    /// drawn in the window (the main window draws the cursor then).
    fn wanted_cursor_float(&self, window: &Window, cx: &App) -> Option<(Bounds<Pixels>, u64)> {
        use std::hash::{Hash, Hasher};
        let state = self.browse_centre_state(cx)?;
        if !state.read(cx).is_native() || state.read(cx).obscured() {
            return None;
        }
        let (at, label) = self.agent_cursor_spot(cx)?;
        let rect = self.browser.read(cx).centre_page_rect?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (f32::from(at.x).to_bits(), f32::from(at.y).to_bits(), label).hash(&mut hasher);
        Some((Bounds::new(window.bounds().origin + rect.origin, rect.size), hasher.finish()))
    }

    /// The cursor float: over the native page, the size of the page, never
    /// taking the pointer or the keyboard (the page under it keeps both).
    fn sync_cursor_float(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_cursor_float(window, cx);
        let current = self.browser.read(cx).cursor_float.as_ref().map(|o| o.bounds);
        if wanted.map(|w| w.0) == current {
            if let Some((_, print)) = wanted {
                let stale = self.browser.update(cx, |browser, _| browser.cursor_float.as_mut().and_then(|float| float.stale(print)));
                if let Some(handle) = stale {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.refresh());
                    });
                }
            }
            return;
        }
        let old = self.browser.update(cx, |browser, _| std::mem::replace(&mut browser.cursor_float, wanted.map(|(bounds, _)| Overlay::opening(OverlayKind::AgentCursor, bounds))));
        if let Some(handle) = old.and_then(|old| old.handle) {
            remove_window(handle, cx);
        }
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
                    window_bounds: Some(WindowBounds::Windowed(r3_frame(bounds))),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |_, cx| cx.new(|cx| OverlayView { baaz, kind: OverlayKind::AgentCursor, focus: cx.focus_handle(), seen_active: false }))
            });
            if let Ok(opened) = &opened {
                drop_native_chrome((*opened).into(), OverlayKind::AgentCursor, cx);
            }
            let _ = this.update(cx, |this, cx| {
                let opened = opened.ok().map(AnyWindowHandle::from);
                if let Some(unwanted) = this.browser.update(cx, |browser, _| land(&mut browser.cursor_float, OverlayKind::AgentCursor, bounds, opened, print)) {
                    remove_window(unwanted, cx);
                }
                this.queue_floats(cx);
            });
        })
        .detach();
    }

    /// What the line shows now: the driving bar for a session acting on the
    /// tab on screen (R2), else the line's own state (R3) in Browse. The
    /// session is `Some` for the driving bar.
    fn line_view(&self, cx: &App) -> Option<(aui::browse::agent_window::LineState, Option<String>)> {
        if let Some(session) = self.line_session(cx) {
            if let Some(activity) = self.terminal_service.agent_activity(&session) {
                // The session by its title, never its provider (rule 14).
                let name = self.session_title(&session, cx);
                let host = self.terminal_service.page_host(&session, cx).unwrap_or_default();
                return Some((Self::agent_line_state(&activity, &name, &host), Some(session)));
            }
        }
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse || self.settings_open(cx) {
            return None;
        }
        self.own_line_state(cx).map(|state| (state, None))
    }

    /// The line's window over `rect`, a page's rect: centred on its foot, as wide as
    /// the line's measure allows, tall enough for the card it grows into.
    fn wanted_agent_line(&self, rect: Bounds<Pixels>, window: &Window, cx: &App) -> Option<(Bounds<Pixels>, u64)> {
        let (state, session) = self.line_view(cx)?;
        trace(format_args!("line wanted: {:?} driving={}", std::mem::discriminant(&state), session.is_some()));
        let pad = gpui::px(SHADOW_PAD);
        let origin = window.bounds().origin;
        let inner = f32::min(640.0, f32::from(rect.size.width) - 24.0);
        let width = gpui::px(inner) + pad * 2.0;
        // The state's own height at this width (the library's exact
        // function): in a narrow pane the approval stacks and an answer
        // wraps longer. A fixed height cut the approval's buttons off once,
        // and the agent waited on a click nobody could make.
        let body = aui::browse::agent_window::line_height(&state, Self::line_compact(inner), inner, window, cx);
        if let aui::browse::agent_window::LineState::Answer { answer: Some(answer), .. } = &state {
            trace(format_args!(
                "line answer: inner={inner} lines={} body={body} rem={:?}",
                aui::browse::agent_window::answer_lines(answer, inner, window, cx),
                window.rem_size()
            ));
        }
        // A little room above the measured height (seen live: the inline
        // answer came out ~12 px taller than measured and its foot was cut).
        // The line sits on the window's foot and never shrinks, so any spare
        // is transparent space above it.
        let height = gpui::px(body + LINE_SLACK) + pad * 2.0;
        let bottom = rect.bottom() - gpui::px(16.0) + pad;
        let left = rect.left() + (rect.size.width - width) / 2.0;
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        session.hash(&mut h);
        format!("{state:?}").hash(&mut h);
        self.browser.read(cx).agent_window.line_wants_keys.hash(&mut h);
        Some((Bounds::new(origin + gpui::point(left, bottom - height), gpui::size(width, height)), h.finish()))
    }

    /// The top of the line's float in screen space, without its shadow pad,
    /// while it shows (or is opening).
    pub(crate) fn page_float_top(&self, cx: &gpui::App) -> Option<Pixels> {
        let bounds = self.browser.read(cx).page_float.as_ref().map(|o| o.bounds)?;
        Some(bounds.top() + gpui::px(SHADOW_PAD))
    }

    /// Whether a line `inner` px wide (shadow pad excluded) lays out narrow.
    fn line_compact(inner: f32) -> bool {
        inner < aui::browse::agent_window::APPROVAL_COMPACT_BELOW
    }

    /// What the line says for `session`'s `activity`.
    fn agent_line_state(activity: &crate::terminal::service::AgentActivity, name: &str, page_host: &str) -> aui::browse::agent_window::LineState {
        use aui::browse::agent_window::{ApprovalData, LineState};
        if activity.held {
            return LineState::YouDrive;
        }
        if let Some(ask) = &activity.asking {
            if !ask.approval {
                return LineState::NeedsYou { text: ask.detail.clone().into() };
            }
            let title = match (ask.kind.as_str(), ask.detail.trim()) {
                // Ask mode (Settings › Browser › Agents): any action asks, so
                // the title says which, from the line's own words.
                ("Act", _) => format!("{name} asks before {}", activity.label),
                // Your own tabs › Ask first: the first use of this tab here.
                ("Use your tab", _) => format!("{name} wants to use this tab"),
                ("Open", target) => format!("{name} wants to open {}", crate::browser::automation::policy::host_of(target)),
                (_, "") => format!("{name} wants to go ahead"),
                ("Run a script", _) => format!("{name} wants to run a script on this page"),
                ("Upload", files) => format!("{name} wants to upload {files}"),
                (_, target) => format!("{name} wants to press \u{201C}{target}\u{201D}"),
            };
            return LineState::Approval(ApprovalData {
                title: title.into(),
                host: page_host.to_owned().into(),
                line: page_host.to_owned().into(),
                kind_pill: ask.kind.clone().into(),
                hint: match ask.kind.as_str() {
                    "Act" | "Open" => "Ask is on in Settings › Browser › Agents".into(),
                    "Use your tab" => "Ask first is on for your tabs in Settings › Browser › Agents".into(),
                    kind => format!("{kind} always asks").into(),
                },
                deny: "Don't allow".into(),
                allow: if matches!(ask.kind.as_str(), "Act" | "Use your tab" | "Open") { "Allow".into() } else { ask.kind.clone().into() },
                // A known host can be trusted for good (Settings › Browser ›
                // Agents › Sites, R3); `a` takes it.
                allow_always: (!page_host.is_empty()).then(|| "Always on this site".to_owned()),
            });
        }
        LineState::from_task(aui::browse::TaskFace::Reading, format!("{name} is {}", activity.label))
    }

    /// Open, move or close the Page float window (the corner's dance).
    pub(crate) fn sync_page_float(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The line's input lives here, so the float that opens for the ask
        // row can take the keyboard on its first frame.
        if self.browser.read(cx).agent_window.agent_line_input.is_none() {
            self.browser.update(cx, |browser, cx| browser.agent_window.agent_line_input = Some(cx.new(|cx| aui::composer::composer_state_rows("Ask about this page", 1, 1, window, cx))));
        }
        self.sync_line_float(window, cx);
        // The card seats itself above the line's window, so it follows it.
        self.sync_card_float(window, cx);
        self.sync_cursor_float(window, cx);
    }

    /// The line's (and the right pane's page) float window.
    fn sync_line_float(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.wanted_page_float(window, cx);
        let current = self.browser.read(cx).page_float.as_ref().map(|o| o.bounds);
        if wanted.map(|w| w.0) == current {
            if let Some((_, print)) = wanted {
                let stale = self.browser.update(cx, |browser, _| browser.page_float.as_mut().and_then(|float| float.stale(print)));
                if let Some(handle) = stale {
                    cx.defer(move |cx| {
                        let _ = handle.update(cx, |_, window, _| window.refresh());
                    });
                }
            }
            return;
        }
        let old = self.browser.update(cx, |browser, _| std::mem::replace(&mut browser.page_float, wanted.map(|(bounds, _)| Overlay::opening(OverlayKind::PageFloat, bounds))));
        if let Some(handle) = old.and_then(|old| old.handle) {
            remove_window(handle, cx);
        }
        let Some((bounds, print)) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        never_main_panels();
        let baaz = cx.entity().downgrade();
        // The ask row types: its float takes the keyboard (R3). Every other
        // state leaves it with the page.
        let keys = self.browser.read(cx).agent_window.line_wants_keys && crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let input = keys.then(|| self.browser.read(cx).agent_window.agent_line_input.clone()).flatten();
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: keys,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    window_bounds: Some(WindowBounds::Windowed(r3_frame(bounds))),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        if let Some(input) = input {
                            window.focus(&focus, cx);
                            input.update(cx, |input, cx| input.focus(window, cx));
                        }
                        OverlayView { baaz, kind: OverlayKind::PageFloat, focus, seen_active: false }
                    })
                })
            });
            // The native chrome comes off between updates, never inside one.
            if let Ok(opened) = &opened {
                drop_native_chrome((*opened).into(), OverlayKind::PageFloat, cx);
            }
            let _ = this.update(cx, |this, cx| {
                let opened = opened.ok().map(AnyWindowHandle::from);
                if let Some(unwanted) = this.browser.update(cx, |browser, _| land(&mut browser.page_float, OverlayKind::PageFloat, bounds, opened, print)) {
                    remove_window(unwanted, cx);
                }
                this.queue_floats(cx);
            });
        })
        .detach();
    }

    /// The Page float's content: the notes panel or the agent pill. Their
    /// clicks run on the main window, where the page's state lives.
    fn render_page_float(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse {
            return self.render_agent_line(window, cx);
        }
        let state = self.existing_browser(cx)?;
        let page = state.read(cx);
        let agent = self.active.as_ref().map(|v| v.read(cx).rt(cx).session_id.clone()).and_then(|id| self.session_lane_provider(&id, cx));
        let name = agent.map_or(crate::providers::THE_AGENT, |p| p.label());
        let menu_open = {
            let pane = self.right_pane.read(cx);
            pane.page_menu_open(cx) && !pane.page_covered(page)
        };
        if menu_open {
            let rows = crate::right_page::recent_rows(page);
            let width = page.page_rect().map(|r| f32::from(r.size.width)).unwrap_or(400.0);
            let menu = crate::right_page::recent_menu(&rows, crate::right_page::recent_menu_room(true, width), self.right_pane.downgrade());
            let close = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
                this.right_pane.update(cx, |pane, cx| pane.set_page_menu(false, cx));
            });
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
            let main = self.window_sync.handle;
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
        self.render_agent_line(window, cx)
    }

    /// The line: what the agent is doing, Take over and Hand back, and the card it
    /// grows into when the person must decide. Clicks run on the main thread's
    /// service, which owns the waiting tool.
    fn render_agent_line(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (state, session) = self.line_view(cx)?;
        let input = self.browser.update(cx, |browser, cx| browser.agent_window.agent_line_input.get_or_insert_with(|| cx.new(|cx| aui::composer::composer_state_rows("Ask about this page", 1, 1, window, cx))).clone());
        let baaz = cx.weak_entity();
        let inner = f32::from(window.bounds().size.width) - SHADOW_PAD * 2.0;
        let line = aui::browse::agent_window::agent_line("page-float-line", &state, &input).compact(Self::line_compact(inner)).on_intent(move |intent, _, cx| {
            use aui::browse::agent_window::LineIntent;
            let session = session.clone();
            let _ = baaz.update(cx, |this, cx| {
                match (session, intent) {
                    // The driving bar: the waiting tool is the service's.
                    (Some(session), LineIntent::TakeOver) => this.terminal_service.take_over(&session),
                    (Some(session), LineIntent::HandBack) => this.terminal_service.hand_back(&session),
                    (Some(session), LineIntent::AllowAlways) => {
                        if let Some(host) = this.terminal_service.page_host(&session, cx).filter(|h| !h.is_empty()) {
                            crate::layout::Layout::update(cx, |layout| {
                                layout.agents.set_site(&host, Some(crate::browser::agents::SiteRule::Always))
                            });
                        }
                        this.terminal_service.answer_person(&session, true);
                    }
                    (Some(session), LineIntent::Allow | LineIntent::Done) => this.terminal_service.answer_person(&session, true),
                    (Some(session), LineIntent::Deny) => this.terminal_service.answer_person(&session, false),
                    (Some(_), LineIntent::Expand) => {
                        this.browser.update(cx, |browser, _| browser.agent_window.card_ui = crate::app::agent_window::CardUi::Card);
                    }
                    (Some(_), _) => {}
                    // The line's own states run on the main window.
                    (None, intent) => this.run_on_main(cx, move |this, window, cx| this.own_line_intent(intent, window, cx)),
                }
                cx.notify();
            });
        });
        // Seated on the window's foot, next to the page's. Float windows have
        // no themed root: the wrapper sets the ink.
        let ink = cx.aui().colors.ink;
        Some(div().p(gpui::px(SHADOW_PAD)).size_full().flex().flex_col().justify_end().text_color(ink).child(div().flex_none().w_full().child(line)).into_any_element())
    }

    /// A fingerprint of what the corner shows: the toasts.
    fn corner_print(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        for toast in &self.overlays.read(cx).toasts {
            toast.id.to_string().hash(&mut hasher);
        }
        hasher.finish()
    }

    /// A fingerprint of what the overlay content shows: the open menu and the
    /// folders it lists, or the Field's rows and highlight.
    fn overlay_print(&self, cx: &App) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        format!("{:?}", self.browse_menu_now(cx)).hash(&mut hasher);
        self.browser.read(cx).tabs.generation.hash(&mut hasher);
        self.overlays.read(cx).stack.is_open(crate::overlay_stack::LayerKind::SiteInfo).hash(&mut hasher);
        // The bookmark popover is its own view: its edits redraw the panel
        // through it.
        if let Some(edit) = self.overlays.read(cx).stack.bookmark() {
            edit.entity_id().hash(&mut hasher);
        }
        self.browser.read(cx).browse_centre_shown(cx).hash(&mut hasher);
        if self.browser.read(cx).browse_address.is_some() {
            self.browser.read(cx).browse_address_text(cx).hash(&mut hasher);
            self.browser.read(cx).browse_omnibox_selected.hash(&mut hasher);
        }
        // The hosted modal: which palette and the rows the root would push
        // it now (results land later than the keystroke), and which dialog.
        // The palette is its own view: its typing, arrows and hover redraw
        // the panel through it, so only what the root pushes in (the rows,
        // the search status) is this print's to see.
        let overlays = self.overlays.read(cx);
        if let Some(view) = self.palette_view(cx) {
            let kind = view.read(cx).delegate.kind;
            let mut rows = crate::palette::PaletteDelegate::new(kind, self.palette_facts(kind, cx), true);
            crate::picker::PickerDelegate::update_matches(&mut rows, &view.read(cx).query(cx));
            format!("{kind:?}").hash(&mut hasher);
            rows.ids().hash(&mut hasher);
            rows.status().hash(&mut hasher);
        }
        if let Some(dialog) = overlays.dialog() {
            dialog.title.hash(&mut hasher);
            dialog.detail.hash(&mut hasher);
        }
        // The attach picker is its own view too: only the rows the root
        // pushes in are this print's to see.
        self.attach_picker_print(cx).hash(&mut hasher);
        hasher.finish()
    }

    /// Whether Browse's modal (a palette, a dialog or the attach picker) draws in the overlay
    /// window rather than the main one: its entry on the overlay stack is a
    /// `Modal`, which the panel hosts in Browse (one host per mode, so a page
    /// loading under an open palette never moves it).
    pub(crate) fn modal_overlay_wanted(&self, cx: &App) -> bool {
        use crate::overlay_stack::LayerKind;
        let browse = crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse;
        let stack = &self.overlays.read(cx).stack;
        [LayerKind::Palette, LayerKind::Dialog, LayerKind::AttachPicker].into_iter().any(|kind| stack.hosted_in_panel(kind, browse))
    }

    /// The handle the hosted modal's keyboard belongs to: the palette's own
    /// query field where it has one, else the palette or the dialog.
    pub(crate) fn modal_focus(&self, cx: &App) -> Option<FocusHandle> {
        use gpui::Focusable as _;
        let overlays = self.overlays.read(cx);
        if let Some(view) = self.palette_view(cx) {
            return Some(view.focus_handle(cx));
        }
        if let Some(view) = self.attach_picker_view(cx) {
            return Some(view.focus_handle(cx));
        }
        let _ = overlays;
        self.dialog_focus(cx)
    }

    /// Whether the Field shows as an overlay: the address is being edited in
    /// Browse with the sidebar (vertical tabs) open and the page is not the new-tab page
    /// (which carries the Field inline).
    pub(crate) fn field_overlay_wanted(&self, cx: &App) -> bool {
        crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse
            && crate::layout::Layout::get(cx).sidebar_open
            && !self.settings_open(cx)
            && self.browser.read(cx).browse_address.is_some()
            && !(self.browse_centre_state(cx).is_some_and(|state| matches!(state.read(cx).url().as_ref(), "" | crate::browser::tabs::BLANK))
                && self.browse_centre_tab_blank(cx))
    }

    /// The overlay's content for `kind`: the Browse menu panel, its intents
    /// queued for the main window.
    pub(crate) fn overlay_content(&mut self, kind: OverlayKind, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        match kind {
            OverlayKind::BrowseMenu => {
                let browse = (crate::layout::Layout::get(cx).mode == crate::layout::Mode::Browse)
                    .then(|| self.browse_model(cx))?;
                let baaz = cx.entity().downgrade();
                let on_intent = std::rc::Rc::new(move |intent: SidebarIntent, _: &mut Window, cx: &mut App| {
                    let _ = baaz.update(cx, |baaz, cx| baaz.queue_sidebar_intent(intent, cx));
                });
                let menu = browse_menu("overlay-browse-menu", &browse, Some(on_intent), window, cx)?;
                Some(div().p(gpui::px(SHADOW_PAD)).child(menu).into_any_element())
            }
            OverlayKind::Field => Some(self.render_field(cx)),
            OverlayKind::SiteInfo => Some(self.render_site_info(cx)),
            OverlayKind::Bookmark => self.render_bookmark_popover(cx),
            OverlayKind::Downloads => {
                self.hosting_overlay = true;
                let list = self.render_browse_downloads(window, cx);
                self.hosting_overlay = false;
                let above = self.browser.read(cx).downloads_anchor.is_some_and(|a| a.top() > window.bounds().size.height / 2.0);
                let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
                    this.dismiss_browse_popovers(cx);
                });
                Some(
                    div()
                        .size_full()
                        .flex()
                        .flex_col()
                        .when(above, |d| d.justify_end())
                        .p(gpui::px(SHADOW_PAD))
                        .on_mouse_down(gpui::MouseButton::Left, leave)
                        .children(list)
                        .into_any_element(),
                )
            }
            OverlayKind::Corner => {
                self.hosting_overlay = true;
                let stack = self.render_corner_stack(cx);
                self.hosting_overlay = false;
                stack
            }
            OverlayKind::PageFloat => self.render_page_float(window, cx),
            OverlayKind::AgentCursor => self.agent_cursor_layer(window, cx),
            OverlayKind::AgentCard => self.render_agent_card(window, cx),
            OverlayKind::Modal => {
                // The same elements the main window draws, their listeners
                // routed back to it (`main_listener`).
                self.hosting_overlay = true;
                let modal = self.compose_palette(cx).or_else(|| self.compose_attach_picker(cx)).or_else(|| self.compose_dialog(cx));
                self.hosting_overlay = false;
                Some(div().size_full().relative().children(modal).into_any_element())
            }
        }
    }

    /// The Field (docs/27 §4.4): the library's `browse/field` component the
    /// owner approved on 5 Nov, centred near the window's top (owner 8 Nov:
    /// "centred, as prototype"). Baaz keeps its own ranking and activation;
    /// the rows only borrow the component's kinds for their glyph and group.
    /// ↑/↓ move, Tab completes, Enter opens, Escape leaves.
    fn render_field(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = cx.aui().colors;
        let panel = self.field_panel(cx);
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx));
        });
        // The clear rest of the rect is outside: a press there leaves the Field.
        div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .p(gpui::px(SHADOW_PAD))
            .text_color(p.ink)
            .on_mouse_down(gpui::MouseButton::Left, leave)
            .child(panel)
            .into_any_element()
    }

    /// The Field's panel itself — the library component with Baaz's rows,
    /// keys and intents — for the overlay window ([`Self::render_field`])
    /// and for the main window when the sidebar is hidden
    /// ([`Self::render_field_in_window`]): one Field, two hosts.
    pub(crate) fn field_panel(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        use aui::browse::field::{field, FieldIntent, FieldModel, FieldRow, FieldRowKind};
        let p = cx.aui().colors;
        let omni = self.browse_suggestions(cx);
        let selected = self.browser.read(cx).browse_omnibox_selected.min(omni.len().saturating_sub(1));
        let rows: Vec<FieldRow> = omni
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let kind = match row.suggestion.kind {
                    crate::browser::SuggestKind::Ask => FieldRowKind::Ask,
                    crate::browser::SuggestKind::Search => FieldRowKind::Search,
                    crate::browser::SuggestKind::Tab | crate::browser::SuggestKind::Session => FieldRowKind::Tab { id: index.to_string().into() },
                    crate::browser::SuggestKind::Command => FieldRowKind::Command { id: index.to_string().into() },
                    crate::browser::SuggestKind::History => FieldRowKind::History { url: row.suggestion.complete.clone().unwrap_or_default().into() },
                    crate::browser::SuggestKind::Bookmark => FieldRowKind::Bookmark { url: row.suggestion.complete.clone().unwrap_or_default().into() },
                };
                // Every row confirms with ↩ unless Baaz names another key; an
                // empty hint drew an empty keycap.
                let hint = if row.suggestion.hint.is_empty() { "↩".to_string() } else { row.suggestion.hint.clone() };
                FieldRow { kind, title: row.suggestion.title.clone().into(), detail: row.suggestion.detail.clone().map(Into::into), hint: hint.into() }
            })
            .collect();
        // The component groups by caption, and the caption-less rows (Ask,
        // Search) lead, as the prototype draws them; activation still maps
        // each row back to Baaz's own index.
        let order = field_display_order(&rows);
        let query = self.browser.read(cx).browse_address_text(cx);
        let mut model = FieldModel::new(query, order.iter().map(|&i| rows[i].clone()).collect());
        model.selected = order.iter().position(|&i| i == selected).unwrap_or(0);
        let walk = order.clone();
        let keys = cx.listener(move |this: &mut Self, event: &gpui::KeyDownEvent, window, cx| match event.keystroke.key.as_str() {
            "up" | "down" => {
                // Walk the rows as drawn, not in Baaz's ranking order.
                let step: isize = if event.keystroke.key == "up" { -1 } else { 1 };
                let at = walk.iter().position(|&i| i == this.browser.read(cx).browse_omnibox_selected).unwrap_or(0) as isize;
                let next = (at + step).clamp(0, walk.len().saturating_sub(1) as isize) as usize;
                if let Some(&index) = walk.get(next) {
                    // Enter opens the highlighted row once the arrows moved
                    // it (`commit_browse_address`), not the typed text.
                    this.browser.update(cx, |browser, cx| browser.move_omnibox(crate::browser::OmniboxStep::Arrow { index, forget_completion: false }, cx));
                }
            }
            "tab" => this.complete_omnibox(window, cx),
            "escape" => this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
            _ => {}
        });
        let input = self.browser.read(cx).browse_address.clone().map(|input| {
            aui::a11y_text::a11y_text_input("field-input", "Search or type a URL", &input, cx).flex_1().min_w(gpui::px(0.0)).text_color(p.ink).into_any_element()
        });
        let baaz = cx.entity().downgrade();
        let hover = baaz.clone();
        let index_of = move |row: &FieldRow| rows.iter().position(|r| r == row);
        let mut panel = field("browse-field", &model)
            .placeholder("Search or type a URL")
            .on_hover(move |shown, _, cx| {
                let Some(&index) = order.get(shown) else { return };
                let _ = hover.update(cx, |this, cx| this.browser.update(cx, |browser, cx| browser.move_omnibox(crate::browser::OmniboxStep::Hover(index), cx)));
            })
            .on_intent(move |intent, _, cx| {
                let _ = baaz.update(cx, |this, cx| match intent {
                    FieldIntent::Open(row) | FieldIntent::Background(row) => {
                        if let Some(index) = index_of(&row) {
                            this.run_on_main(cx, move |this, window, cx| this.activate_omnibox_row(index, window, cx));
                        }
                    }
                    FieldIntent::Complete => this.run_on_main(cx, |this, window, cx| this.complete_omnibox(window, cx)),
                    FieldIntent::Close => this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
                    // Baaz never scopes the Field (one Bookmarks row opens
                    // the Bookmarks & History page instead).
                    FieldIntent::Unscope | FieldIntent::SearchExact(_) => {}
                });
            });
        if let Some(input) = input {
            panel = panel.query_slot(input);
        }
        div()
            .id("field-panel")
            .debug_selector(|| "field-panel".into())
                    .occlude()
                    .role(gpui::Role::Dialog)
                    .aria_label("Open a page, switch tab or ask")
                    .on_key_down(keys)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(panel)
            .into_any_element()
    }

    /// The Field in the main window, for Browse with the sidebar hidden
    /// (no overlay window then): the same panel, 640 wide, centred on the
    /// window with its top at 18 % of the height (docs/83 §7), lifted above
    /// the page's stand-in. `None` while it has no rows, as the covering
    /// rule (`omnibox_open`) only covers the page then.
    pub(crate) fn render_field_in_window(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !self.browse_omnibox_open(cx) {
            return None;
        }
        let win = window.viewport_size();
        let width = aui::overlay::palette_width(win.width);
        let top = aui::overlay::palette_top(win.height);
        let panel = self.field_panel(cx);
        Some(
            aui::overlay::popover_layer(
                gpui::anchored()
                    .position(gpui::point((win.width - width) / 2.0, top))
                    .child(div().w(width).child(panel)),
            )
            .into_any_element(),
        )
    }

    /// The site-information panel (owner 2026-10-11: "certificate details
    /// like Chrome"): the host, whether the connection is secure, and the
    /// certificate — issued to, issued by, validity, protocol, SHA-256 — on
    /// FF's dropdown chrome (container radius, p-1, caption group labels,
    /// border/60 separators). Read from the engine when it opens.
    fn render_site_info(&mut self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let state = self.browse_centre_state(cx);
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
                .child(div().text_role(TextRole::Body).text_color(p.fg).child(value))
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
                    .text_role(TextRole::Body)
                    .semibold()
                    .text_color(p.fg)
                    .truncate()
                    .child(host.clone()),
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
                            .child(div().text_role(TextRole::Body).text_color(if secure { p.fg } else { p.danger }).child(headline))
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
        // docs/82 8.6: what the site may do, kept from the person's answers,
        // and its zoom — its own Settings page, on the main window.
        if baaz_browser_core::sites::origin_of(&url).is_some() {
            let open = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::SiteInfo, cx); });
                this.run_on_main(cx, |this, _, cx| this.open_shown_site_settings(cx));
            });
            panel = panel.child(div().my(inset).mx(-inset).h(gpui::px(1.0)).bg(separator)).child(
                div().px(metrics.item_px).pb(metrics.item_px).child(
                    aui::data::button("site-info-settings", "Site settings\u{2026}")
                        .sm()
                        .accessibility_label(format!("Site settings for {host}"))
                        .on_click(open),
                ),
            );
        }
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            this.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::SiteInfo, cx); });
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(gpui::px(SHADOW_PAD))
            .on_mouse_down(gpui::MouseButton::Left, leave)
            .child(div().w_full().occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(menu_panel(panel, cx)))
            .into_any_element()
    }

    /// Run `f` against the main window once the current event has finished:
    /// focus, pages and the address field's owner all live there.
    pub(crate) fn run_on_main(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static) {
        let Some(main) = self.window_sync.handle else {
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
            OverlayKind::Corner | OverlayKind::PageFloat | OverlayKind::AgentCard | OverlayKind::AgentCursor => {}
            // Leaving the Field also hands focus back: on the main window.
            OverlayKind::Field => self.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
            OverlayKind::SiteInfo => {
                self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::SiteInfo, cx); });
            }
            // The popover's own dismissal (it keeps what is typed).
            OverlayKind::Bookmark => {
                if let Some(view) = self.overlays.read(cx).stack.bookmark().cloned() {
                    view.update(cx, |_, cx| cx.emit(gpui::DismissEvent));
                }
            }
            OverlayKind::Downloads => {
                self.dismiss_browse_popovers(cx);
            }
            // The hosted modals' own dismissal, palette, attach picker,
            // then dialog.
            OverlayKind::Modal => {
                for kind in [crate::overlay_stack::LayerKind::Palette, crate::overlay_stack::LayerKind::AttachPicker, crate::overlay_stack::LayerKind::Dialog] {
                    let view = self.overlays.read(cx).stack.view(kind);
                    if let Some(view) = view {
                        self.dismiss_overlay_view(view.entity_id(), cx);
                    }
                }
            }
        }
    }

    /// An intent from inside the overlay: run against the main window (where
    /// focus and pages live) once this event has finished.
    pub(crate) fn queue_sidebar_intent(&mut self, intent: SidebarIntent, cx: &mut Context<Self>) {
        self.run_on_main(cx, move |baaz, window, cx| baaz.sidebar_intent(intent, window, cx));
    }
}

#[cfg(all(test, feature = "probe"))]
mod field_order_tests {
    use super::field_display_order;
    use aui::browse::field::{FieldRow, FieldRowKind};

    fn row(kind: FieldRowKind) -> FieldRow {
        FieldRow { kind, title: "t".into(), detail: None, hint: "↩".into() }
    }

    #[test]
    fn captionless_rows_lead_and_groups_keep_their_rank() {
        let rows = vec![
            row(FieldRowKind::Ask),
            row(FieldRowKind::Tab { id: "1".into() }),
            row(FieldRowKind::Command { id: "2".into() }),
            row(FieldRowKind::Tab { id: "3".into() }),
            row(FieldRowKind::Search),
        ];
        assert_eq!(field_display_order(&rows), vec![0, 4, 1, 3, 2]);
    }
}
