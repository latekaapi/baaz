//! Turning the folded session into transcript elements.
//!
//! Everything here is a pure function of `aui_protocol` data: a [`Turn`] or a
//! [`Block`] in, an element out, with the collapsed/expanded state of the
//! foldable cards handed in by the view and toggles handed back through one
//! closure. No component in here holds state or touches the wire — the fold is
//! the only source of truth, and the view re-renders it whole every frame.
//!
//! Approval and question cards are rendered **read-only** in this phase: the
//! wire round-trip for a decision is Phase 4, and a card that looked actionable
//! and did nothing would be worse than one that plainly is not.

use std::collections::HashSet;
use std::rc::Rc;

use aui::transcript::{
    activity_group, answered_row, approval_card, assistant_turn, error_card, marker_row, plan_card,
    question_card, summary_card, thinking_block, todo_list, tool_card, transcript_card, user_turn,
};
use aui_protocol::{Block, MarkerKind, ThinkingState, Turn, TurnMeta};
use aui_tokens::{scale, ActiveAui, AuiStyled};
use aui_icons::IconName;
use aui_motion::stream_reveal;
use gpui::{div, prelude::*, px, AnyElement, App, ElementId, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};

/// What a collapsible card needs from the view: which cards the person has
/// toggled away from their default, and where a click on a header goes.
///
/// The set holds **overrides**, not open cards, so a card whose default changes
/// under it — a reasoning trace collapses the moment it finishes — still honours
/// a person who opened it by hand.
pub struct Folds {
    /// Keys (`"<turn id>:<block index>"`) of the cards toggled by hand.
    pub toggled: HashSet<String>,
    /// Called with the key of the card whose header was clicked.
    pub toggle: ToggleHandler,
}

/// What a card header click reports: the key of the card that was clicked.
pub type ToggleHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

impl Folds {
    /// Whether the card at `key` is open, given what it does by default.
    fn open(&self, key: &str, default_open: bool) -> bool {
        default_open != self.toggled.contains(key)
    }
}

/// The key that identifies one block for folding and for its element id.
pub fn block_key(turn_id: &str, index: usize) -> String {
    format!("{turn_id}:{index}")
}

/// Render one turn: the person's bubble, or every block of an assistant reply.
///
/// `settled` is false only for the newest turn, so history does not replay the
/// reveal animation when the window opens or a session is resumed.
pub fn turn(turn: &Turn, settled: bool, folds: &Folds, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    match turn {
        Turn::User { id, text, .. } => vec![div()
            .w_full()
            .flex()
            .justify_end()
            .child(user_turn(SharedString::from(id.clone()), text.clone()))
            .into_any_element()],
        Turn::Assistant { id, blocks, meta } => {
            let last = blocks.len().saturating_sub(1);
            blocks
                .iter()
                .enumerate()
                .map(|(index, b)| {
                    let key = block_key(id, index);
                    let reveal = stream_reveal(ElementId::from(SharedString::from(key.clone())), index, settled, window, cx);
                    let body = block(&key, b, index == last, meta, folds, cx);
                    div().w_full().relative().top(reveal.offset_y).opacity(reveal.opacity).child(body).into_any_element()
                })
                .collect()
        }
    }
}

/// One block of an assistant turn.
///
/// `last` and `meta` exist for one reason: the per-turn token footer belongs
/// under the reply, and [`assistant_turn`] is the component that draws it, so
/// the turn's closing text block is the one that carries the meta.
fn block(
    key: &str,
    block: &Block,
    last: bool,
    meta: &TurnMeta,
    folds: &Folds,
    cx: &mut App,
) -> AnyElement {
    let p = cx.aui().colors;
    let id = ElementId::from(SharedString::from(key.to_owned()));
    let toggle = {
        let toggle = folds.toggle.clone();
        let key = key.to_owned();
        move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| toggle(key.clone(), window, cx)
    };
    match block {
        Block::Text { text, streaming } => {
            let mut turn = assistant_turn(id, text.clone()).streaming(*streaming);
            // A finished turn signs off with its footer; a running one has no
            // final numbers to show yet, and neither has a turn the server
            // measured nothing for.
            if last && !*streaming && meta != &TurnMeta::default() {
                turn = turn.meta(meta.clone());
            }
            turn.into_any_element()
        }
        Block::Thinking { text, elapsed_ms, summary, state } => {
            let done = *state == ThinkingState::Done;
            let mut card = thinking_block(id, text.clone(), elapsed(*elapsed_ms), *state)
                // A live trace stays open; a finished one collapses to its
                // summary until the person asks for it.
                .expanded(folds.open(key, !done))
                .on_toggle(toggle);
            if let Some(summary) = summary {
                card = card.summary(summary.clone());
            }
            card.into_any_element()
        }
        Block::Activity { steps, summary, elapsed_ms, state } => {
            activity_group(id, steps.clone(), summary.clone(), elapsed(*elapsed_ms), *state)
                .open(folds.open(key, false))
                .on_toggle(toggle)
                .into_any_element()
        }
        Block::ToolCall { kind: _, verb, target, status, duration_ms, body, .. } => {
            tool_card(id, verb.clone(), target.clone(), *status, body.clone())
                .duration_ms(*duration_ms)
                .open(folds.open(key, true))
                .on_intent({
                    let toggle = folds.toggle.clone();
                    let key = key.to_owned();
                    move |_, window, cx| toggle(key.clone(), window, cx)
                })
                .into_any_element()
        }
        // Read-only until Phase 4 wires the decision back to `approval/decide`.
        Block::Approval { tool, command, reason, cwd, capabilities, scope, state, rule, .. } => {
            approval_card(id, tool.clone(), command.clone(), state.clone())
                .reason(reason.clone())
                .cwd(cwd.clone())
                .capabilities(capabilities.clone())
                .scope(*scope)
                .rule(rule.clone().unwrap_or_default())
                .into_any_element()
        }
        Block::Question { prompt, subtitle, options, multi, allow_other, answer, .. } => {
            if let Some(answer) = answer {
                let chips: Vec<SharedString> = answer
                    .selected
                    .iter()
                    .filter_map(|i| options.get(*i))
                    .map(|o| SharedString::from(o.label.clone()))
                    .chain(answer.other.clone().map(SharedString::from))
                    .collect();
                return answered_row(id, chips).into_any_element();
            }
            question_card(id, prompt.clone(), options.clone())
                .subtitle(subtitle.clone())
                .multi(*multi)
                .allow_other(*allow_other)
                .into_any_element()
        }
        Block::Plan { items, state, .. } => plan_card(id, items.clone()).state(*state).into_any_element(),
        Block::Todo { items } => todo_list(id, items.clone()).open(folds.open(key, true)).on_toggle(toggle).into_any_element(),
        Block::Summary { title, files, checks, duration_ms, cost_usd } => {
            summary_card(id, title.clone(), format!("{} · ${cost_usd:.2}", elapsed(*duration_ms)))
                .files(files.clone())
                .checks(checks.clone())
                .into_any_element()
        }
        Block::Error { title, detail, .. } => error_card(id, title.clone(), detail.clone()).into_any_element(),
        Block::Goal { objective, status, percent_complete, current_work, .. } => {
            let mut header = h_flex().w_full().gap(px(scale::SP_2)).items_center();
            header = header
                .child(div().ui(scale::FS_13).semibold().text_color(p.ink).child(objective.clone()))
                .child(div().flex_1())
                .child(div().mono(scale::FS_11).text_color(p.ink_3).child(match percent_complete {
                    Some(percent) => format!("{status} · {percent:.0}%"),
                    None => status.clone(),
                }));
            let mut card = transcript_card(id, true).chevron(false).hover_tint(false).header(header);
            if let Some(work) = current_work {
                card = card.body(div().w_full().p(px(scale::SP_4)).ui(scale::FS_12).text_color(p.ink_2).child(work.clone()));
            }
            card.into_any_element()
        }
        // MSP's item kinds are an open set and mandate exactly this fallback:
        // the kind, the status and the server's own one-line text.
        Block::Generic { kind, status, text } => transcript_card(id, true)
            .chevron(false)
            .hover_tint(false)
            .header(
                h_flex()
                    .w_full()
                    .gap(px(scale::SP_2))
                    .child(div().ui(scale::FS_13).semibold().text_color(p.ink).child(kind.clone()))
                    .child(div().flex_1())
                    .child(div().mono(scale::FS_11).text_color(p.ink_3).child(status.clone())),
            )
            .body(div().w_full().p(px(scale::SP_4)).ui(scale::FS_12).text_color(p.ink_2).child(text.clone()))
            .into_any_element(),
        Block::Marker { kind, text } => marker(id, kind, text, cx),
    }
}

/// A hairline marker row, tinted only where the marker carries a warning.
///
/// The tint is the design rules' one exception: status colour carries meaning,
/// so a retry and a hole in the transcript are warning-coloured and everything
/// else is the muted line iconography every other marker uses.
fn marker(id: ElementId, kind: &MarkerKind, text: &str, cx: &mut App) -> AnyElement {
    let p = cx.aui().colors;
    let row = marker_row(id);
    match kind {
        MarkerKind::SessionStarted => row.glyph(IconName::Play, None).text(text.to_owned()),
        MarkerKind::ContextCompacted => row.glyph(IconName::Layout, None).text(text.to_owned()),
        MarkerKind::PermissionModeChanged { mode } => {
            row.glyph(IconName::Shield, None).text(text.to_owned()).strong(mode.label())
        }
        MarkerKind::TurnCancelled | MarkerKind::TurnRetracted => row.glyph(IconName::X, None).text(text.to_owned()),
        MarkerKind::RetryScheduled => row.glyph(IconName::Refresh, Some(p.warning)).text(text.to_owned()),
        MarkerKind::ViewGap => row.glyph(IconName::Shield, Some(p.warning)).text(text.to_owned()),
        MarkerKind::ForkedFrom => row.glyph(IconName::Git, None).text(text.to_owned()),
        // Muse is one provider, so the fold never raises a hand-off; if a
        // later provider does, the plain row still says what happened.
        MarkerKind::HandOff { .. } => row.glyph(IconName::ArrowRight, None).text(text.to_owned()),
    }
    .into_any_element()
}

/// `12.4 s`, `1 m 12 s` — the library's own formatting, so every duration in
/// the app reads the same.
pub fn elapsed(ms: u64) -> SharedString {
    SharedString::from(aui::transcript::format_duration(ms))
}

/// The empty transcript: what a brand-new session shows before the first turn.
pub fn empty_state(workspace: &str, cx: &mut App) -> AnyElement {
    let p = cx.aui().colors;
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(scale::SP_3))
        .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child("New session"))
        .child(div().ui(scale::FS_12).text_color(p.ink_3).child(format!("Muse runs in {workspace}.")))
        .into_any_element()
}
