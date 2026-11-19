//! The agent card's float window (R3, docs/27 §4.5): the library's
//! `agent_card` floating bottom-right over the Browse page, or
//! `agent_column` docked beside it, showing the browser chat. The handoff
//! picker (R3 board 3a) opens over the card, inside the same window.

use gpui::{
    div, prelude::*, AnyWindowHandle, App, Bounds, Context, Pixels, Window, WindowBackgroundAppearance, WindowBounds, WindowKind,
    WindowOptions,
};

use super::{drop_native_chrome, Overlay, OverlayKind, OverlayView, SHADOW_PAD};
use crate::app::agent_window::{CardUi, COLUMN_W};
use crate::app::Harness;
use aui_tokens::ActiveAui;
use crate::layout::LayoutGlobal as _;

/// The library card's measure (aui `agent_window::CARD_W`/`CARD_H`).
const CARD_W: f32 = 400.0;
const CARD_H: f32 = 520.0;
/// The card's inset from the page's right and bottom edges.
const CARD_INSET: f32 = 16.0;

impl Harness {
    /// Where the card (or column) window goes now, in screen space.
    fn wanted_card_float(&self, window: &Window, cx: &App) -> Option<Bounds<Pixels>> {
        if crate::layout::Layout::get(cx).mode != crate::layout::Mode::Browse || self.browser.read(cx).agent_window.card_ui == CardUi::Closed || self.settings_open(cx) {
            return None;
        }
        let page = self.browser.read(cx).centre_page_rect?;
        let origin = window.bounds().origin;
        let pad = gpui::px(SHADOW_PAD);
        match self.browser.read(cx).agent_window.card_ui {
            CardUi::Column => {
                // The column takes the strip the page gave up beside it
                // (`browse_centre_page_rect` shrinks the page by it).
                // The strip right of the page, the page's height (the page
                // rect is the centre column, docs/83 §3).
                let left = page.right() + gpui::px(6.0);
                Some(Bounds::new(origin + gpui::point(left, page.top()), gpui::size(gpui::px(COLUMN_W), page.size.height)))
            }
            _ => {
                // Above the line when one shows, so Take over stays in reach
                // (owner 8 Nov: the line stays under the card).
                // The line's own window, when it shows: the card sits 8 above it.
                let line = self
                    .page_float_top(cx)
                    .map(|top| f32::from(page.bottom() + origin.y - top) - CARD_INSET + 8.0)
                    .filter(|gap| *gap > 0.0)
                    .unwrap_or(0.0);
                let width = gpui::px(CARD_W.min(f32::from(page.size.width) - 2.0 * CARD_INSET)) + pad * 2.0;
                let height = gpui::px(CARD_H.min(f32::from(page.size.height) - 2.0 * CARD_INSET - line)) + pad * 2.0;
                let right = page.right() - gpui::px(CARD_INSET) + pad;
                let bottom = page.bottom() - gpui::px(CARD_INSET + line) + pad;
                Some(Bounds::new(origin + gpui::point(right - width, bottom - height), gpui::size(width, height)))
            }
        }
    }

    /// Open, move or close the card window, as the page float does.
    pub(crate) fn sync_card_float(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.browser.read(cx).agent_window.agent_card_input.is_none() {
            self.browser.update(cx, |browser, cx| browser.agent_window.agent_card_input = Some(cx.new(|cx| aui::composer::composer_state_rows("Steer, or ⌘↩ to queue", 1, 4, window, cx))));
        }
        let wanted = self.wanted_card_float(window, cx);
        let current = self.browser.read(cx).agent_window.card_float.as_ref().map(|o| o.bounds);
        if wanted == current {
            if let Some(handle) = self.browser.read(cx).agent_window.card_float.as_ref().and_then(|float| float.handle) {
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        let old = self.browser.update(cx, |browser, _| {
            std::mem::replace(&mut browser.agent_window.card_float, wanted.map(|bounds| Overlay::opening(OverlayKind::AgentCard, bounds)))
        });
        if let Some(handle) = old.and_then(|old| old.handle) {
            super::remove_window(handle, cx);
        }
        let Some(bounds) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        super::never_main_panels();
        let baaz = cx.entity().downgrade();
        let input = self.browser.read(cx).agent_window.agent_card_input.clone();
        let docked = self.browser.read(cx).agent_window.card_ui == CardUi::Column;
        cx.spawn(async move |this, cx| {
            let opened = cx.update(|cx| {
                let options = WindowOptions {
                    titlebar: None,
                    focus: true,
                    show: true,
                    kind: WindowKind::PopUp,
                    is_movable: false,
                    // The floating card loses the frame inset (see `r3_frame`);
                    // the docked column, measured live, does not.
                    window_bounds: Some(WindowBounds::Windowed(if docked { bounds } else { super::r3_frame(bounds) })),
                    window_background: WindowBackgroundAppearance::Transparent,
                    ..Default::default()
                };
                cx.open_window(options, move |window, cx| {
                    cx.new(|cx| {
                        let focus = cx.focus_handle();
                        window.focus(&focus, cx);
                        if let Some(input) = input {
                            input.update(cx, |input, cx| input.focus(window, cx));
                        }
                        OverlayView { baaz, kind: OverlayKind::AgentCard, focus, seen_active: false }
                    })
                })
            });
            // The native chrome comes off between updates, never inside one.
            if let Ok(opened) = &opened {
                drop_native_chrome((*opened).into(), OverlayKind::AgentCard, cx);
            }
            let _ = this.update(cx, |this, cx| {
                let opened = opened.ok().map(AnyWindowHandle::from);
                let unwanted = this.browser.update(cx, |browser, _| super::land(&mut browser.agent_window.card_float, OverlayKind::AgentCard, bounds, opened, 0));
                if let Some(unwanted) = unwanted {
                    super::remove_window(unwanted, cx);
                }
                // What moved while it opened is the next sync's to catch.
                this.queue_floats(cx);
            });
        })
        .detach();
    }

    /// The chat's title for the card: its name or generated title, else its
    /// first ask, short.
    fn browser_chat_title(&self, cx: &App) -> String {
        let Some(view) = self.registry.browser_chat() else { return "New chat".into() };
        let view = view.read(cx);
        if let Some(meta) = self.session_list.read(cx).overrides.get(&view.rt(cx).session_id) {
            if let Some(title) = meta.name.clone().or_else(|| meta.generated_title.clone()) {
                return title;
            }
        }
        let first = view.rt(cx).transcript_session().and_then(|s| {
            s.turns.iter().find_map(|turn| match turn {
                aui_protocol::Turn::User { text, .. } => Some(crate::app::agent_window::strip_page_prompt(text.as_str()).to_owned()),
                _ => None,
            })
        });
        match first {
            Some(text) => {
                let line = text.lines().next().unwrap_or("").trim().to_owned();
                if line.chars().count() > 48 { format!("{}…", line.chars().take(47).collect::<String>()) } else { line }
            }
            None => "New chat".into(),
        }
    }

    /// The card's content: the chat as the library's card or column, and the
    /// handoff picker over it while it is open. Float windows have no themed
    /// root, so the wrapper sets the ink.
    pub(crate) fn render_agent_card(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let input = self.browser.read(cx).agent_window.agent_card_input.clone()?;
        let title = self.browser_chat_title(cx);
        let (transcript, model) = match self.registry.browser_chat() {
            Some(view) => {
                let view = view.read(cx);
                let busy = view.rt(cx).busy() || view.rt(cx).is_starting();
                let model = model_label(&view.rt(cx).model());
                let transcript = match view.rt(cx).transcript_session() {
                    Some(session) => crate::app::agent_window::agent_transcript(session, &title, &model, busy),
                    None => empty_transcript(&title, &model, busy),
                };
                (transcript, model)
            }
            None => {
                let model = self.assistant_choice(cx).map(|c| c.label.to_owned()).unwrap_or_default();
                (empty_transcript(&title, &model, false), model)
            }
        };
        let _ = model;
        let mut transcript = transcript;
        // The provider's own approval (a tool, a file write) waits in the
        // input's place, as the card's questions do (docs/27 §4.5).
        let asking = self.registry.browser_chat().and_then(|view| {
            let view = view.read(cx);
            // Its own browser tools are accepted for it (`auto_accept_browser_tools`).
            match view.rt(cx).pending_external_approvals().last() {
                Some((_, headline)) if headline.contains("mcp__baaz__browser_") => None,
                Some((_, headline)) => {
                    let line = view.rt(cx).pending_approval_line().unwrap_or_else(|| headline.clone());
                    // No detail on the provider's card: the waiting call's own.
                    match view.rt(cx).transcript_session().and_then(crate::app::agent_window::pending_call_detail) {
                        Some(detail) if !line.contains(" — ") => Some(format!("{line} — {detail}")),
                        _ => Some(line),
                    }
                }
                None => view.rt(cx).pending_approval_line(),
            }
        });
        if let Some(line) = asking {
            transcript.items.push(aui::browse::agent_window::AgentItem::Question {
                text: format!("Allow: {line}?").into(),
                choices: vec!["Allow".into(), "Don't allow".into()],
            });
        }
        if let Some((_, label, detail)) = self.browser_chat_id(cx).and_then(|id| self.browser.read(cx).agent_window.browser_handoffs.get(&id).cloned()) {
            transcript.items.push(aui::browse::agent_window::AgentItem::HandedOff { title: label.into(), detail: detail.into() });
        }
        if self.browser.read(cx).agent_window.line_page_chip {
            transcript.chips = vec![aui::composer::ComposerChip {
                id: "this-page".into(),
                kind: aui::composer::ComposerChipKind::File,
                label: "This page".into(),
                removable: true,
                thumbnail: None,
                detail: None,
                state: Default::default(),
            }];
        }
        let baaz = cx.weak_entity();
        let on_intent = move |intent: aui::browse::agent_window::CardIntent, _: &mut Window, cx: &mut App| {
            let _ = baaz.update(cx, |this, cx| this.run_on_main(cx, move |this, window, cx| this.card_intent(intent, window, cx)));
        };
        let card = match self.browser.read(cx).agent_window.card_ui {
            CardUi::Column => aui::browse::agent_window::agent_column("agent-card", &transcript, &input).on_intent(on_intent).into_any_element(),
            _ => aui::browse::agent_window::agent_card("agent-card", &transcript, &input).on_intent(on_intent).into_any_element(),
        };
        let ink = cx.aui().colors.ink;
        let pad = if self.browser.read(cx).agent_window.card_ui == CardUi::Column { 0.0 } else { SHADOW_PAD };
        let mut root = div().size_full().relative().p(gpui::px(pad)).text_color(ink).child(card);
        if let Some(picker) = self.render_handoff_picker(cx) {
            root = root.child(picker);
        }
        Some(root.into_any_element())
    }
}

/// The card before any turn: the title and model, nothing in the body (or
/// one working step while the chat opens).
fn empty_transcript(title: &str, model: &str, busy: bool) -> aui::browse::agent_window::AgentTranscript {
    use aui::browse::agent_window::{AgentItem, AgentTranscript, Step, StepState};
    let items = if busy { vec![AgentItem::Steps(vec![Step::new("Starting", StepState::Running)])] } else { Vec::new() };
    AgentTranscript { title: title.to_owned().into(), model: model.to_owned().into(), chips: Vec::new(), items }
}

/// A model id as the card names it: the assistant's label when it is one
/// of its choices (`haiku` → Claude Haiku), else the id itself.
fn model_label(model: &str) -> String {
    crate::providers::ProviderId::all()
        .into_iter()
        .flat_map(crate::browser::assistant::model_choices)
        .find(|choice| !choice.model.is_empty() && choice.model == model)
        .map(|choice| choice.label.to_owned())
        .unwrap_or_else(|| model.to_owned())
}
