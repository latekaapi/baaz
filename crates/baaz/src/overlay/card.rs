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

/// The library card's measure (aui `agent_window::CARD_W`/`CARD_H`).
const CARD_W: f32 = 400.0;
const CARD_H: f32 = 520.0;
/// The card's inset from the page's right and bottom edges.
const CARD_INSET: f32 = 16.0;

impl Harness {
    /// Where the card (or column) window goes now, in screen space.
    fn wanted_card_float(&self, window: &Window, _cx: &App) -> Option<Bounds<Pixels>> {
        if self.mode != crate::layout::Mode::Browse || self.card_ui == CardUi::Closed || self.settings_open() {
            return None;
        }
        let page = self.browser.centre_page_rect?;
        let origin = window.bounds().origin;
        let pad = gpui::px(SHADOW_PAD);
        match self.card_ui {
            CardUi::Column => {
                // The column takes the strip the page gave up beside it
                // (`browse_centre_page_rect` shrinks the page by it).
                // The strip right of the page, the sheet's height (the page
                // rect already sits inside the sheet).
                let left = page.right() + gpui::px(6.0);
                Some(Bounds::new(origin + gpui::point(left, page.top()), gpui::size(gpui::px(COLUMN_W), page.size.height)))
            }
            _ => {
                // Above the line when one shows, so Take over stays in reach
                // (owner 8 Nov: the line stays under the card).
                // The line's own window, when it shows: the card sits 8 above it.
                let line = self
                    .page_float_top()
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
        if self.agent_card_input.is_none() {
            self.agent_card_input = Some(cx.new(|cx| aui::composer::composer_state_rows("Steer, or ⌘↩ to queue", 1, 4, window, cx)));
        }
        let wanted = self.wanted_card_float(window, cx);
        let current = self.card_float.as_ref().map(|o| o.bounds).or(self.card_float_pending);
        if wanted == current {
            if let Some(float) = self.card_float.as_ref() {
                let handle = float.handle;
                cx.defer(move |cx| {
                    let _ = handle.update(cx, |_, window, _| window.refresh());
                });
            }
            return;
        }
        if let Some(old) = self.card_float.take() {
            let handle = old.handle;
            cx.defer(move |cx| {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            });
        }
        self.card_float_pending = wanted;
        let Some(bounds) = wanted else {
            return;
        };
        #[cfg(target_os = "macos")]
        super::never_main_panels();
        let baaz = cx.entity().downgrade();
        let input = self.agent_card_input.clone();
        let docked = self.card_ui == CardUi::Column;
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
                    drop_native_chrome(window, OverlayKind::AgentCard);
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
            let _ = this.update(cx, |this, cx| {
                match opened {
                    Ok(handle) if this.card_float_pending == Some(bounds) => {
                        this.card_float_pending = None;
                        this.card_float = Some(Overlay { handle: handle.into(), kind: OverlayKind::AgentCard, bounds, print: 0 });
                    }
                    Ok(handle) => {
                        let handle: AnyWindowHandle = handle.into();
                        cx.defer(move |cx| {
                            let _ = handle.update(cx, |_, window, _| window.remove_window());
                        });
                    }
                    Err(_) => this.card_float_pending = None,
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// The chat's title for the card: its name or generated title, else its
    /// first ask, short.
    fn browser_chat_title(&self, cx: &App) -> String {
        let Some(view) = self.browser_chat.as_ref() else { return "New chat".into() };
        let view = view.read(cx);
        if let Some(meta) = self.overrides.get(&view.session_id) {
            if let Some(title) = meta.name.clone().or_else(|| meta.generated_title.clone()) {
                return title;
            }
        }
        let first = view.transcript_session().and_then(|s| {
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
    pub(crate) fn render_agent_card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let input = self.agent_card_input.clone()?;
        let title = self.browser_chat_title(cx);
        let (transcript, model) = match self.browser_chat.as_ref() {
            Some(view) => {
                let view = view.read(cx);
                let busy = view.busy() || view.is_starting();
                let model = model_label(&view.model());
                let transcript = match view.transcript_session() {
                    Some(session) => crate::app::agent_window::agent_transcript(session, &title, &model, busy),
                    None => empty_transcript(&title, &model, busy),
                };
                (transcript, model)
            }
            None => {
                let model = self.assistant_choice().map(|c| c.label.to_owned()).unwrap_or_default();
                (empty_transcript(&title, &model, false), model)
            }
        };
        let _ = model;
        let mut transcript = transcript;
        if let Some((_, label, detail)) = self.browser_chat_id(cx).and_then(|id| self.browser_handoffs.get(&id).cloned()) {
            transcript.items.push(aui::browse::agent_window::AgentItem::HandedOff { title: label.into(), detail: detail.into() });
        }
        if self.line_page_chip {
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
        let card = match self.card_ui {
            CardUi::Column => aui::browse::agent_window::agent_column("agent-card", &transcript, &input).on_intent(on_intent).into_any_element(),
            _ => aui::browse::agent_window::agent_card("agent-card", &transcript, &input).on_intent(on_intent).into_any_element(),
        };
        let ink = cx.aui().colors.ink;
        let pad = if self.card_ui == CardUi::Column { 0.0 } else { SHADOW_PAD };
        let mut root = div().size_full().relative().p(gpui::px(pad)).text_color(ink).child(card);
        if let Some(picker) = self.render_handoff_picker(window, cx) {
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
