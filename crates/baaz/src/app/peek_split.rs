//! R4 Peek and Split in the Browse centre (see `browser::peek_split` for the
//! model). The native pages fill their rects; the ×s and the caption are
//! gpui, shown through holes cut in the pages where they overlap.

use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{div, prelude::*, px, relative, AnyElement, Context, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};

use super::Harness;
use crate::browser::peek_split::{self, CLOSE, CLOSE_INSET, PEEK_H, PEEK_W};
use crate::browser::TabId;

impl Harness {
    /// The round × the Peek and each Split half carry: 24 px, a floating
    /// disc (`overlay`, a `line-strong` edge, the float shadow) so it reads
    /// over any page in either theme.
    fn page_close(&self, id: SharedString, label: &'static str, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = cx.aui().colors;
        aui::data::record_ax_label(label);
        div()
            .id(id)
            .size(px(CLOSE))
            .rounded_full()
            .bg(p.overlay)
            .border_1()
            .border_color(p.line_strong)
            .shadow(p.floating_shadow())
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(label)
            .child(icon(IconName::X).size(px(12.0)).color(p.fg))
            .hover(move |s| s.bg(p.surface_4))
    }

    /// The centre as a Split when the shown tab is a half of the open one:
    /// `page` (the shown tab's pane) and its partner side by side, the
    /// original left tab always on the left. `Err(page)` when there is none.
    pub(crate) fn render_split(&mut self, page: AnyElement, window: &mut Window, cx: &mut Context<Self>) -> Result<AnyElement, AnyElement> {
        let (Some(pair), Some(shown), Some(partner)) = (self.browser.read(cx).split, self.browser.read(cx).browse_centre_shown(cx), self.browser.read(cx).split_partner(cx)) else {
            return Err(page);
        };
        let state = self.ensure_tab_page(crate::browser::HOME_KEY, partner, window, cx);
        // Each half is page, square at the column's edges (docs/83 §3).
        state.update(cx, |state, _| state.set_corner_radius(0.0));
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
        let ratio = peek_split::clamp_split(self.browser.read(cx).split_ratio);
        let centre = self.browser.read(cx).centre_page_rect;
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
            if let Some(centre) = this.browser.read(cx).centre_page_rect {
                this.browser.update(cx, |browser, _| browser.split_ratio = peek_split::split_ratio_at(centre, x));
                cx.notify();
            }
        };
        let start = cx.listener(|this: &mut Self, _: &f32, _, cx| {
            this.browser.update(cx, |browser, _| browser.split_dragging = true);
            cx.notify();
        });
        let moved = cx.listener(move |this: &mut Self, x: &f32, _, cx| drag(*x, this, cx));
        let end = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.browser.update(cx, |browser, _| browser.split_dragging = false);
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
        if self.browser.read(cx).split_dragging {
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
        let peek = self.browser.read(cx).peek.as_ref()?;
        let p = cx.aui().colors;
        let state = peek.state.clone();
        let url = state.read(cx).url().to_string();
        let url = if url.is_empty() { peek.url.clone() } else { url };
        let host = crate::browser::history::host_of(&url);
        let caption: SharedString = format!("{host}  ·  ⌘↩ open as a tab  ·  esc closes").into();
        aui::data::record_ax_label(&caption);
        let close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.browser.update(cx, |browser, cx| browser.close_peek(cx)));
        let scrim_close = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.browser.update(cx, |browser, cx| browser.close_peek(cx)));
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
                        .bg(aui::overlay::scrim())
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
                        // It floats over the scrim: a `line-strong` edge and
                        // the one float shadow (docs/83 §2.3).
                        .border_1()
                        .border_color(p.line_strong)
                        .shadow(p.floating_shadow())
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
                                // A float over the scrim in the theme's own
                                // colours: `overlay`, its `line-strong` edge
                                // and the float shadow (docs/83 §7).
                                .bg(p.overlay)
                                .border_1()
                                .border_color(p.line_strong)
                                .shadow(p.floating_shadow())
                                .child(div().role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(caption))
                                .child(
                                    div()
                                        .id("browse-peek-to-tab")
                                        .px(px(scale::SP_2))
                                        .py(px(2.0))
                                        .rounded(px(scale::R_SM))
                                        .bg(p.surface_4)
                                        .cursor_pointer()
                                        .role(gpui::Role::Button)
                                        .aria_label("Open Peek as a tab")
                                        .text_role(TextRole::Caption)
                                        .text_color(p.fg)
                                        .child("Open as tab")
                                        .on_click(to_tab),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

}
