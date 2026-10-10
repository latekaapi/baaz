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

use aui::screens::sidebar::{browse_menu, browse_menu_size, SidebarIntent};
use gpui::{
    div, prelude::*, AnyWindowHandle, App, Bounds, Context, FocusHandle, Pixels, Render, WeakEntity, Window,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use crate::app::Harness;

/// What an overlay window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OverlayKind {
    /// Browse's open right-click menu (`Harness::browse_menu`).
    BrowseMenu,
}

/// The open overlay: its window, what it shows, and where (screen space).
pub(crate) struct Overlay {
    handle: AnyWindowHandle,
    kind: OverlayKind,
    bounds: Bounds<Pixels>,
}

/// The overlay's root view: draws the Harness-built content for its kind.
pub(crate) struct OverlayView {
    baaz: WeakEntity<Harness>,
    kind: OverlayKind,
    focus: FocusHandle,
}

impl Render for OverlayView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let kind = self.kind;
        let content = self.baaz.update(cx, |baaz, cx| baaz.overlay_content(kind, window, cx)).ok().flatten();
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
        let menu = self.browse_menu.as_ref()?;
        let space = self.browse_space().unwrap_or_default();
        let folders = self.browser.tabs.folders_in(&space).len();
        let size = browse_menu_size(menu, folders, cx);
        let origin = window.bounds().origin + menu.at;
        Some((OverlayKind::BrowseMenu, Bounds::new(origin, size)))
    }

    /// Open, move or close the overlay window to match what is wanted.
    /// Window changes are deferred past this frame (this runs inside render).
    pub(crate) fn sync_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.main_window.get_or_insert_with(|| window.window_handle());
        let wanted = self.wanted_overlay(window, cx);
        let current = self.overlay.as_ref().map(|o| (o.kind, o.bounds));
        if wanted == current {
            if let Some(overlay) = self.overlay.as_ref() {
                // The content reads Harness state: repaint it with the window.
                let _ = overlay.handle.update(cx, |_, window, _| window.refresh());
            }
            return;
        }
        if let Some(old) = self.overlay.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        let Some((kind, bounds)) = wanted else {
            return;
        };
        let baaz = cx.entity().downgrade();
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
        let opened = cx.open_window(options, move |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                // Resigning key (a click outside, a switch away) closes it.
                cx.observe_window_activation(window, move |this: &mut OverlayView, window, cx| {
                    if !window.is_window_active() {
                        let kind = this.kind;
                        let _ = this.baaz.update(cx, |baaz, cx| baaz.close_overlay_content(kind, cx));
                    }
                })
                .detach();
                OverlayView { baaz, kind, focus }
            })
        });
        if let Ok(handle) = opened {
            self.overlay = Some(Overlay { handle: handle.into(), kind, bounds });
        }
    }

    /// The overlay's content for `kind`: the Browse menu panel, its intents
    /// queued for the main window.
    pub(crate) fn overlay_content(&mut self, kind: OverlayKind, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        match kind {
            OverlayKind::BrowseMenu => {
                let model = self.sidebar_model(false, cx);
                let browse = model.browse?;
                let baaz = cx.entity().downgrade();
                let on_intent = std::rc::Rc::new(move |intent: SidebarIntent, _: &mut Window, cx: &mut App| {
                    let _ = baaz.update(cx, |baaz, cx| baaz.queue_sidebar_intent(intent, cx));
                });
                browse_menu("overlay-browse-menu", &browse, Some(on_intent), window, cx)
            }
        }
    }

    /// Close what the overlay shows (Escape inside it, or it lost the key).
    pub(crate) fn close_overlay_content(&mut self, kind: OverlayKind, cx: &mut Context<Self>) {
        match kind {
            OverlayKind::BrowseMenu => self.close_browse_menu(cx),
        }
    }

    /// An intent from inside the overlay: run against the main window (where
    /// focus and pages live) once this event has finished.
    pub(crate) fn queue_sidebar_intent(&mut self, intent: SidebarIntent, cx: &mut Context<Self>) {
        let Some(main) = self.main_window else {
            return;
        };
        let baaz = cx.entity().downgrade();
        cx.defer(move |cx| {
            let _ = main.update(cx, |_, window, cx| {
                let _ = baaz.update(cx, |baaz, cx| baaz.sidebar_intent(intent, window, cx));
            });
        });
    }
}
