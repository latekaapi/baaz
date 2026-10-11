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
}

/// The site-information panel's width: FF's dropdown `w-80`, 320.
const SITE_INFO_WIDTH: f32 = scale::SP_7 * 10.0;

/// The Field panel's width (canvas board "Field", 2026-10-11): 600.
const FIELD_WIDTH: f32 = scale::SP_7 * 18.75;

/// The clear margin around the Field panel inside its window: room for FF's
/// shadow-3 (a 1 px outer ring and a drop reaching 4.5 px below), which the
/// window's edge would otherwise clip — the panel's only edge, since the
/// window draws none of its own ([`drop_native_chrome`]).
const FIELD_SHADOW_PAD: f32 = scale::SP_1;

/// Take macOS's own rim and shadow off an overlay window. gpui opens a
/// `PopUp` as a titled panel, so the window server outlines the WHOLE window
/// rect: around the Field's fixed transparent rect that drew an empty box
/// below the rows and a second edge on top of the panel's FF ring (owner
/// screenshot 2026-10-11). The panel's own FF shadow is its only edge.
#[cfg(target_os = "macos")]
fn drop_native_chrome(window: &Window) {
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
    // the main thread; `window` and `setHasShadow:` are plain AppKit calls.
    unsafe {
        let ns_window: *mut AnyObject = msg_send![view, window];
        if !ns_window.is_null() {
            let _: () = msg_send![ns_window, setHasShadow: false];
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn drop_native_chrome(_: &Window) {}

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
        trace(format_args!("render {kind:?} content={} active={}", content.is_some(), window.is_window_active()));
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
        if self.field_overlay_wanted(cx) {
            // One fixed transparent rect from the address line down: the
            // panel draws at its top and grows inside it, so typing never
            // resizes the window; a click on the clear part is outside.
            let metrics = cx.aui().metrics;
            let top = metrics.header + metrics.item_px / 2.0;
            // The panel sits at (item_px, top); the window reaches the
            // shadow margin past it on every side.
            let pad = gpui::px(FIELD_SHADOW_PAD);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(scale::SP_7 * 15.0)) + pad;
            return Some((OverlayKind::Field, Bounds::new(origin, gpui::size(gpui::px(FIELD_WIDTH) + pad * 2.0, height))));
        }
        if self.site_info_open && self.sidebar_open && self.browse_centre_state().is_some() {
            // Under the address line, inside a clear rect like the Field's.
            let metrics = cx.aui().metrics;
            let pad = gpui::px(FIELD_SHADOW_PAD);
            let top = metrics.header + metrics.item_px / 2.0 + metrics.control_lg + gpui::px(scale::SP_1);
            let origin = window.bounds().origin + gpui::point(metrics.item_px - pad, top - pad);
            let height = (window.bounds().size.height - top - metrics.item_px).min(gpui::px(scale::SP_7 * 15.0)) + pad;
            return Some((OverlayKind::SiteInfo, Bounds::new(origin, gpui::size(gpui::px(SITE_INFO_WIDTH) + pad * 2.0, height))));
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
                    if matches!(kind, OverlayKind::Field | OverlayKind::SiteInfo) {
                        drop_native_chrome(window);
                    }
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
                            } else if this.seen_active && std::env::var("BAAZ_OVERLAY_STICKY").as_deref() != Ok("1") {
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
        hasher.finish()
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
                let model = self.sidebar_model(false, cx);
                let browse = model.browse?;
                let baaz = cx.entity().downgrade();
                let on_intent = std::rc::Rc::new(move |intent: SidebarIntent, _: &mut Window, cx: &mut App| {
                    let _ = baaz.update(cx, |baaz, cx| baaz.queue_sidebar_intent(intent, cx));
                });
                browse_menu("overlay-browse-menu", &browse, Some(on_intent), window, cx)
            }
            OverlayKind::Field => Some(self.render_field(window, cx)),
            OverlayKind::SiteInfo => Some(self.render_site_info(cx)),
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
        let mut list = v_flex().w_full().gap(gpui::px(0.0)).child(field);
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
                list = list.child(
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
                .when(lit, |d| d.bg(p.hover_overlay()))
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
            list = list.child(
                div()
                    .id(("field-row", index))
                    .role(gpui::Role::ListBoxOption)
                    .aria_label(row.suggestion.title.clone())
                    .cursor_pointer()
                    .on_mouse_down(gpui::MouseButton::Left, press)
                    .child(Shape::Item.rounded(item)),
            );
        }
        let _ = window;
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| {
            this.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx));
        });
        // The clear rest of the rect is outside: a press there leaves the Field.
        div()
            .size_full()
            .flex()
            .flex_col()
            .p(gpui::px(FIELD_SHADOW_PAD))
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
            .p(gpui::px(FIELD_SHADOW_PAD))
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

    /// Close what the overlay shows (Escape inside it, or it lost the key).
    pub(crate) fn close_overlay_content(&mut self, kind: OverlayKind, cx: &mut Context<Self>) {
        trace(format_args!("close {kind:?}"));
        match kind {
            OverlayKind::BrowseMenu => self.close_browse_menu(cx),
            // Leaving the Field also hands focus back: on the main window.
            OverlayKind::Field => self.run_on_main(cx, |this, window, cx| this.leave_browse_address(window, cx)),
            OverlayKind::SiteInfo => {
                self.site_info_open = false;
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
