//! R4 Peek and Split in the Browse centre (see `browser::peek_split` for the
//! model). The native pages fill their rects; the ×s and the caption are
//! gpui, shown through holes cut in the pages where they overlap.

use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, Density, TextRole};
use gpui::{div, prelude::*, px, relative, AnyElement, Context, Pixels, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};

use super::Harness;
use crate::browser::peek_split::{self, CLOSE, CLOSE_INSET, PEEK_H, PEEK_W};
use crate::browser::TabId;

impl Harness {
    /// The round × the Peek and each Split half carry (24 px, a dark disc so
    /// it reads over any page).
    fn page_close(&self, id: SharedString, label: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = cx.aui().colors;
        aui::data::record_ax_label(label);
        div()
            .id(id)
            .size(px(CLOSE))
            .rounded_full()
            .bg(gpui::rgba(0x1b1b1fe6))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(label)
            .child(icon(IconName::X).size(px(12.0)).color(gpui::white()))
            .hover(move |s| s.bg(p.fg_muted))
    }

    /// The centre as a Split when the shown tab is a half of the open one:
    /// `page` (the shown tab's pane) and its partner side by side, the
    /// original left tab always on the left. `Err(page)` when there is none.
    pub(crate) fn render_split(&mut self, page: AnyElement, window: &mut Window, cx: &mut Context<Self>) -> Result<AnyElement, AnyElement> {
        let (Some(pair), Some(shown), Some(partner)) = (self.browser.split, self.browse_centre_shown(), self.split_partner()) else {
            return Err(page);
        };
        let state = self.ensure_tab_page(crate::browser::HOME_KEY, partner, window, cx);
        let radius = if self.browse_page_card() { aui::shell::SHEET_RADIUS } else { 0.0 };
        state.update(cx, |state, _| state.set_corner_radius(radius));
        let harness = cx.weak_entity();
        let other = aui_webview::webview_pane("browse-split-page", &state)
            .without_nav()
            .on_intent(move |intent, window, cx| {
                if let Some(harness) = harness.upgrade() {
                    harness.update(cx, |harness, cx| harness.handle_browser_intent(intent, window, cx));
                }
            })
            .into_any_element();
        let half = |this: &Self, id: TabId, side: &'static str, content: AnyElement, cx: &mut Context<Self>| -> AnyElement {
            let close = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.close_split_half(id, window, cx));
            let label = if side == "left" { "Close the left half of Split" } else { "Close the right half of Split" };
            let _ = this;
            div()
                .id(SharedString::from(format!("browse-split-{side}")))
                .relative()
                .flex_1()
                .h_full()
                .min_w(px(0.0))
                .role(gpui::Role::Group)
                .aria_label(if side == "left" { "Split, left half" } else { "Split, right half" })
                .child(content)
                .child(
                    div()
                        .absolute()
                        .top(px(CLOSE_INSET))
                        .right(px(CLOSE_INSET))
                        .child(this.page_close(SharedString::from(format!("browse-split-close-{side}")), label, cx).on_click(close)),
                )
                .into_any_element()
        };
        let (left, right) = if shown == pair.left { (page, other) } else { (other, page) };
        let left = half(self, pair.left, "left", left, cx);
        let right = half(self, pair.right, "right", right, cx);
        let ground = cx.aui().colors.surface_1;
        // The left half takes its dragged share; the right the rest. Before
        // the centre has been measured, an even split.
        let ratio = peek_split::clamp_split(self.browser.split_ratio);
        let centre = self.browser.centre_page_rect;
        let left = div()
            .h_full()
            .flex_none()
            .map(|d| match centre {
                Some(centre) => d.w(peek_split::split_left_width(centre, ratio)),
                None => d.w(relative(ratio)),
            })
            .child(left);
        let right = div().h_full().flex_1().min_w(px(0.0)).child(right);
        // The divider: the gap itself, which no page covers, so gpui takes
        // the press; AppKit keeps sending the drag to this window even over
        // the pages. The capture layer keeps it once the pointer outruns
        // the strip.
        let drag = |x: f32, this: &mut Self, cx: &mut Context<Self>| {
            if let Some(centre) = this.browser.centre_page_rect {
                this.browser.split_ratio = peek_split::split_ratio_at(centre, x);
                cx.notify();
            }
        };
        let start = cx.listener(|this: &mut Self, _: &f32, _, cx| {
            this.browser.split_dragging = true;
            cx.notify();
        });
        let moved = cx.listener(move |this: &mut Self, x: &f32, _, cx| drag(*x, this, cx));
        let end = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.browser.split_dragging = false;
            cx.notify();
        });
        let moved_strip = std::rc::Rc::new(moved);
        let moved_layer = moved_strip.clone();
        let end_strip = std::rc::Rc::new(end);
        let end_layer = end_strip.clone();
        let divider = div()
            .id("browse-split-divider")
            .h_full()
            .flex_none()
            .role(gpui::Role::Splitter)
            .aria_label("Resize Split")
            .child(
                aui::shell::resize_handle("browse-split-resize")
                    .on_drag_start(move |x, w, cx| start(&x, w, cx))
                    .on_drag(move |x, w, cx| moved_strip(&x, w, cx))
                    .on_drag_end(move |w, cx| end_strip(&(), w, cx)),
            );
        let mut row = div()
            .relative()
            .size_full()
            .child(h_flex().size_full().bg(ground).child(left).child(divider).child(right));
        if self.browser.split_dragging {
            row = row.child(
                aui::shell::drag_capture_overlay("browse-split-capture")
                    .on_drag(move |x, w, cx| moved_layer(&x, w, cx))
                    .on_drag_end(move |w, cx| end_layer(&(), w, cx)),
            );
        }
        Ok(row.into_any_element())
    }

    /// The Peek layer over the centre: the scrim (a click closes), the card
    /// with the live page, its × and the caption under it.
    pub(crate) fn render_peek(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let peek = self.browser.peek.as_ref()?;
        let p = cx.aui().colors;
        let state = peek.state.clone();
        let url = state.read(cx).url().to_string();
        let url = if url.is_empty() { peek.url.clone() } else { url };
        let host = crate::browser::history::host_of(&url);
        let caption: SharedString = format!("{host}  ·  ⌘↩ open as a tab  ·  esc closes").into();
        aui::data::record_ax_label(&caption);
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_peek(cx));
        let scrim_close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_peek(cx));
        let to_tab = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.peek_to_tab(cx));
        let pane = aui_webview::webview_pane("browse-peek-page", &state).without_nav().into_any_element();
        let left = relative((1.0 - PEEK_W) / 2.0);
        let top = relative((1.0 - PEEK_H) / 2.0);
        Some(
            div()
                .id("browse-peek-layer")
                .absolute()
                .inset_0()
                .occlude()
                .role(gpui::Role::Dialog)
                .aria_label(SharedString::from(format!("Peek, {host}")))
                .child(
                    div()
                        .id("browse-peek-scrim")
                        .absolute()
                        .inset_0()
                        .bg(gpui::black().opacity(0.35))
                        .cursor_pointer()
                        .role(gpui::Role::Button)
                        .aria_label("Close Peek")
                        .on_click(scrim_close),
                )
                .child(
                    div()
                        .id("browse-peek-card")
                        .absolute()
                        .left(left)
                        .top(top)
                        .w(relative(PEEK_W))
                        .h(relative(PEEK_H))
                        .rounded(px(12.0))
                        .shadow(p.shadow(3))
                        .overflow_hidden()
                        .child(pane)
                        .child(
                            div()
                                .absolute()
                                .top(px(CLOSE_INSET))
                                .right(px(CLOSE_INSET))
                                .child(self.page_close("browse-peek-close".into(), "Close Peek", cx).on_click(close)),
                        ),
                )
                .child(
                    v_flex()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px(0.0))
                        .h(top)
                        .items_center()
                        .justify_center()
                        .child(
                            h_flex()
                                .gap(px(scale::SP_2))
                                .items_center()
                                .pl(px(scale::SP_3))
                                .pr(px(scale::SP_1))
                                .py(px(scale::SP_1))
                                .rounded(px(scale::R_LG))
                                .bg(gpui::rgba(0x1b1b1ff0))
                                .shadow(p.shadow(2))
                                .child(div().role_in(TextRole::Caption, cx).text_color(gpui::white().opacity(0.85)).child(caption))
                                .child(
                                    div()
                                        .id("browse-peek-to-tab")
                                        .px(px(scale::SP_2))
                                        .py(px(2.0))
                                        .rounded(px(scale::R_SM))
                                        .bg(gpui::white().opacity(0.14))
                                        .cursor_pointer()
                                        .role(gpui::Role::Button)
                                        .aria_label("Open Peek as a tab")
                                        .text_role_in(TextRole::Body, Density::Compact)
                                        .text_color(gpui::white())
                                        .child("Open as tab")
                                        .on_click(to_tab),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    /// The × holes for this frame (window coordinates, radius): the Peek's
    /// on its own page, and each Split half's on its page. The centre page's
    /// hole is returned for `set_browse_holes` to merge with its own.
    pub(crate) fn peek_split_holes(&mut self, cx: &mut Context<Self>) -> Option<(gpui::Bounds<Pixels>, Pixels)> {
        let centre = self.browser.centre_page_rect?;
        let r = px(CLOSE / 2.0);
        if let Some(peek) = self.browser.peek.as_ref() {
            let card = peek_split::peek_rect(centre);
            let hole = [peek_split::close_rect(card)].map(|b| [f32::from(b.origin.x), f32::from(b.origin.y), CLOSE, CLOSE, CLOSE / 2.0]);
            peek.state.update(cx, |state, _| state.set_holes(hole.to_vec()));
        }
        let pair = self.browser.split?;
        let shown = self.browse_centre_shown()?;
        let partner = self.split_partner()?;
        let (left, right) = peek_split::split_rects(centre, self.browser.split_ratio);
        let close = |half: gpui::Bounds<Pixels>| {
            gpui::Bounds::new(
                gpui::point(half.origin.x + half.size.width - px(CLOSE + CLOSE_INSET), half.origin.y + px(CLOSE_INSET)),
                gpui::size(px(CLOSE), px(CLOSE)),
            )
        };
        let (shown_rect, partner_rect) = if shown == pair.left { (close(left), close(right)) } else { (close(right), close(left)) };
        if let Some(state) = self.browser.states.get(&partner).cloned() {
            let b = partner_rect;
            state.update(cx, |state, _| state.set_holes(vec![[f32::from(b.origin.x), f32::from(b.origin.y), CLOSE, CLOSE, CLOSE / 2.0]]));
        }
        Some((shown_rect, r))
    }
}
