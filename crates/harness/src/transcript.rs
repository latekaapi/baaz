//! Turning the folded session into transcript elements.
//!
//! Everything here is a pure function of `aui_protocol` data: a [`Turn`] or a
//! [`Block`] in, an element out, with the collapsed/expanded state of the
//! foldable cards handed in by the view and toggles handed back through one
//! closure. No component in here holds state or touches the wire — the fold is
//! the only source of truth, and the view re-renders it whole every frame.
//!
//! The approval and question cards are **live** from Phase 4 on: a choice
//! becomes `approval/decide`, an answer becomes `userInput/answer`, and both go
//! out through [`Cards`], which the session view fills in. A replayed capture
//! wires them up too — opening a preview and picking an option are local, and a
//! command that would reach the wire is refused with a banner rather than
//! quietly doing nothing.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use aui::transcript::{
    activity_group, answered_row, approval_card, assistant_turn, error_card, generic_item_card,
    goal_card, marker_row, plan_card, question_card, summary_card, thinking_block, todo_list,
    tool_card, user_turn, QuestionOutcome, ToolCardIntent,
};
use aui_protocol::{Answer, Block, MarkerKind, ThinkingState, ToolBody, ToolCall, Turn, TurnMeta};
use aui_tokens::scale;
use aui_icons::IconName;
use aui_motion::stream_reveal;
use gpui::{div, prelude::*, px, relative, AnyElement, App, ElementId, SharedString, Window};
use gpui_kit::base::{h_flex, v_flex};

/// The title the pending approval card asks its question with.
///
/// Deliberately names Muse: the library's default is provider-free, because the
/// library does not know whose command it is and this app does.
pub const APPROVAL_TITLE: &str = "Allow Muse to run this command?";

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
    /// Called when a plan card's action row is used. `None` renders the plan
    /// card read-only, which is what a replayed transcript wants.
    pub plan: Option<PlanHandler>,
    /// The live approval and question wiring. `None` renders both read-only.
    pub cards: Option<Cards>,
    /// Session id → the label the sidebar shows for it, so a `ForkedFrom`
    /// marker can name the session it came from rather than its uuid.
    pub titles: HashMap<String, String>,
    /// Tool block id → what its truncated server-side output offers. Only
    /// blocks whose item carried `truncated: true` with an `outputRef` appear
    /// here; every other tool card keeps the plain fold toggle.
    pub full_output: HashMap<String, FullOutput>,
    /// "Show full output" on a truncated tool card: the block's id, out. The
    /// card never fetches itself — the app pages `item/readOutput` on a
    /// background task and replaces the body on the server's result.
    pub show_full_output: Option<CardHandler>,
    /// Draw the cards settled rather than entering, for a `--screenshot` run
    /// that renders a few frames and quits.
    pub at_rest: bool,
}

/// What a card header click reports: the key of the card that was clicked.
pub type ToggleHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// What the person chose on a plan card (spec §3.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanAction {
    /// Restore the previous approval mode and implement the plan.
    Accept,
    /// Keep plan mode and go back to the composer.
    Refine,
    /// Restore the previous approval mode and do nothing else.
    Reject,
}

/// Called with the plan block's id and the action taken.
pub type PlanHandler = Rc<dyn Fn(String, PlanAction, &mut Window, &mut App)>;

/// An intent that names one card and nothing else: the block's id, out.
///
/// "Continue", "Skip", "Explain instead" and "Retry" all have this shape — the
/// card is the whole argument, because which card it was is the only thing the
/// app needs in order to know what to send.
pub type CardHandler = Rc<dyn Fn(String, &mut Window, &mut App)>;

/// An intent that names a card and a row inside it: `(block id, index)`.
///
/// An option was clicked; an option's preview was toggled.
pub type RowHandler = Rc<dyn Fn(String, usize, &mut Window, &mut App)>;

/// What a tool card offers when the server truncated its visible output but
/// kept the full bytes under an `outputRef`.
pub struct FullOutput {
    /// The fold holds the item's `outputRef`, so a fetch would serve bytes.
    pub fetchable: bool,
    /// The fetch the app runs, and what it returned.
    pub state: FullOutputState,
}

/// Where a truncated tool card's full-output fetch stands.
pub enum FullOutputState {
    /// Nothing fetched yet; the fold row fetches.
    Idle,
    /// Pages are arriving on the app's background task.
    Fetching,
    /// The server's bytes, as lines, with whether the 2 MiB cap cut them.
    Ready {
        lines: Vec<String>,
        capped: bool,
    },
}

/// A server-minted approval choice was pressed:
/// `(approvalId, choiceId, feedback)`.
pub type ChooseHandler = Rc<dyn Fn(String, String, Option<String>, &mut Window, &mut App)>;

/// Open (`Some(choiceId)`) or close (`None`) an approval's feedback field, by
/// `approvalId`.
pub type FeedbackToggleHandler = Rc<dyn Fn(String, Option<String>, &mut Window, &mut App)>;

/// Everything a pending approval or question card needs to be answerable.
///
/// It is one struct rather than a dozen parameters because the two cards share
/// the same shape: some state the app owns (what is selected, which field is
/// open), one element the app owns (the text field), and a handful of intents
/// out. The two `RefCell<Option<AnyElement>>` slots are the fields themselves:
/// only one card can have one open at a time, so the first card that matches
/// takes it.
pub struct Cards {
    /// A server-minted choice was pressed: `(approvalId, choiceId, feedback)`.
    pub choose: ChooseHandler,
    /// Open (`Some(choiceId)`) or close (`None`) an approval's feedback field.
    pub feedback_toggle: FeedbackToggleHandler,
    /// Which approval has a feedback field open, and for which choice.
    pub feedback_open: Option<(String, String)>,
    /// That field. Taken by the card that owns it.
    pub feedback_slot: RefCell<Option<AnyElement>>,
    /// What the field currently holds, so "Send" can carry it.
    pub feedback_text: String,
    /// An option row was clicked: `(question block id, option index)`.
    pub select: RowHandler,
    /// What is selected on each pending question, by block id.
    pub selections: HashMap<String, Vec<usize>>,
    /// An option's preview chevron: `(question block id, option index)`.
    pub toggle_preview: RowHandler,
    /// Which previews are open, by block id.
    pub previews: HashMap<String, Vec<usize>>,
    /// "Continue" on a question: its block id.
    pub answer: CardHandler,
    /// "Skip" on a question: its block id.
    pub skip: CardHandler,
    /// "Explain instead": the question's block id, or the confirmation of an
    /// already-open field.
    pub clarify: CardHandler,
    /// Which question has its clarification field open.
    pub clarify_open: Option<String>,
    /// That field.
    pub clarify_slot: RefCell<Option<AnyElement>>,
    /// How long each pending question has left, by block id, against how long
    /// it was given. The app ticks the clock; the card only draws it.
    pub countdowns: HashMap<String, (u64, u64)>,
    /// "Retry" on an error card: the id of the turn that failed.
    pub retry: CardHandler,
    /// Which failed turns still have their prompt text, so the button is only
    /// offered where pressing it would do something.
    pub retryable_turns: HashSet<String>,
}

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

/// The reasoning tokens a finished turn billed without showing any work, if any.
///
/// A turn can bill a reasoning budget and emit no `reasoning` item at all —
/// the fold records the count on [`TurnMeta::reasoning_tokens`] either way —
/// so a count with no thinking card means the model thought silently. Returns
/// the count when the turn is an assistant turn with `reasoning_tokens > 0`
/// and no [`Block::Thinking`], and `None` otherwise (user turns, no billed
/// reasoning, or a visible trace that speaks for itself).
pub fn silent_reasoning(turn: &Turn) -> Option<u64> {
    let Turn::Assistant { blocks, meta, .. } = turn else { return None };
    if meta.reasoning_tokens == 0 {
        return None;
    }
    if blocks.iter().any(|block| matches!(block, Block::Thinking { .. })) {
        return None;
    }
    Some(meta.reasoning_tokens)
}

/// The footer's reasoning cell for a turn [`silent_reasoning`] fired on.
///
/// The library footer draws `"419 reasoning"`; on a silent turn the same cell
/// reads `"419 reasoning, thought silently"`, on the same line in the same
/// style. Turns with a visible thinking card keep the library's cell
/// unchanged.
pub fn silent_reasoning_text(count: u64) -> String {
    format!("{count} reasoning, thought silently")
}

/// The library footer's cells for a silent turn: a mirror of the library's
/// private `footer_items` (`aui/src/transcript/turns.rs`) with the reasoning
/// cell replaced by [`silent_reasoning_text`].
///
/// The library draws the footer from `TurnMeta` with no per-cell hook, so a
/// silent turn carries no `assistant_turn(..).meta(..)` and gets this row
/// instead — same cells, same order, same separators, same style. Keep in
/// sync with the library.
fn silent_footer_items(meta: &TurnMeta, count: u64) -> Vec<String> {
    let tokens = meta.tokens_in + meta.tokens_out;
    let tokens = if tokens >= 1000 {
        format!("{:.1}k tokens", tokens as f64 / 1000.0)
    } else {
        format!("{tokens} tokens")
    };
    let mut items = vec![
        meta.model.clone(),
        format!("{:.1} s", meta.duration_ms as f64 / 1000.0),
        tokens,
    ];
    items.retain(|item| !item.is_empty());
    items.push(silent_reasoning_text(count));
    if meta.cost_usd > 0.0 {
        items.push(format!("${:.2}", meta.cost_usd));
    }
    items
}

/// The single footer line for a silent turn: the library footer's own row
/// (mono, `FS_11`, `ink_4`, `·` separators) with the silent reasoning cell.
fn silent_footer_row(meta: &TurnMeta, count: u64, cx: &mut App) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let mut footer = h_flex()
        .w_full()
        .mt(px(10.0))
        .gap(px(10.0))
        .font_family(scale::FONT_MONO)
        .text_px(scale::FS_11)
        .line_height(relative(1.0))
        .medium()
        .text_color(p.ink_4);
    for (i, item) in silent_footer_items(meta, count).into_iter().enumerate() {
        if i > 0 {
            footer = footer.child("·");
        }
        footer = footer.child(item);
    }
    footer.into_any_element()
}

/// Render one turn: the person's bubble, or every block of an assistant reply.
///
/// `settled` is false only for the newest turn, so history does not replay the
/// reveal animation when the window opens or a session is resumed.
pub fn turn(turn: &Turn, settled: bool, folds: &Folds, window: &mut Window, cx: &mut App) -> Vec<AnyElement> {
    // A turn can bill reasoning tokens and emit no reasoning item at all
    // (improvement candidate 3 in docs/09-handoff-improvements.md §8). On a
    // silent turn this row *is* the footer — the library's cells with the
    // reasoning cell saying what the count meant — so the library must not
    // draw its own underneath.
    let silent = silent_reasoning(turn);
    match turn {
        Turn::User { id, text, .. } => vec![div()
            .w_full()
            .flex()
            .justify_end()
            .child(user_turn(SharedString::from(id.clone()), text.clone()))
            .into_any_element()],
        Turn::Assistant { id, blocks, meta } => {
            let last = blocks.len().saturating_sub(1);
            // A silent turn gets the harness's own footer row, so its blocks
            // carry no library footer.
            let library_meta = if silent.is_some() { None } else { Some(meta) };
            let mut rows: Vec<AnyElement> = blocks
                .iter()
                .enumerate()
                .map(|(index, b)| {
                    let key = block_key(id, index);
                    let reveal = stream_reveal(ElementId::from(SharedString::from(key.clone())), index, settled, window, cx);
                    let body = block(&key, id, b, index == last, library_meta, folds, cx);
                    div().w_full().relative().top(reveal.offset_y).opacity(reveal.opacity).child(body).into_any_element()
                })
                .collect();
            if let Some(count) = silent {
                rows.push(silent_footer_row(meta, count, cx));
            }
            rows
        }
    }
}

/// One block of an assistant turn.
///
/// `last` and `meta` exist for one reason: the per-turn token footer belongs
/// under the reply, and [`assistant_turn`] is the component that draws it, so
/// the turn's closing text block is the one that carries the meta. `meta` is
/// `None` on a silent turn, which gets the harness's own footer row instead
/// (see [`silent_footer_row`]): the library must not draw its own underneath.
fn block(
    key: &str,
    turn_id: &str,
    block: &Block,
    last: bool,
    meta: Option<&TurnMeta>,
    folds: &Folds,
    cx: &mut App,
) -> AnyElement {
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
            // measured nothing for. A silent turn carries no library footer —
            // `meta` is `None` there — because the harness draws its own row.
            if let Some(meta) = meta {
                if last && !*streaming && meta != &TurnMeta::default() {
                    turn = turn.meta(meta.clone());
                }
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
        Block::ToolCall { .. } => {
            let Some(call) = block.as_tool_call() else {
                return generic_item_card(id, "tool", "done", String::new()).into_any_element();
            };
            tool_call_card(key, id, &call, folds)
        }
        // A grouped run renders as its individual cards until the fold learns
        // the group's own open state: same card, same toggles, one per call.
        Block::ToolGroup { calls, .. } => {
            let cards = calls
                .iter()
                .enumerate()
                .map(|(index, call)| {
                    let sub_key = format!("{key}:{index}");
                    let sub_id = ElementId::from(SharedString::from(sub_key.clone()));
                    tool_call_card(&sub_key, sub_id, call, folds)
                })
                .collect::<Vec<_>>();
            v_flex().w_full().gap(px(8.0)).children(cards).into_any_element()
        }
        Block::Approval { id: approval_id, tool, command, reason, cwd, capabilities, scope, state, rule, choices, stages, current_stage, badges, feedback, resolved_by } => {
            let mut card = approval_card(id, tool.clone(), command.clone(), state.clone())
                .title(APPROVAL_TITLE)
                .reason(reason.clone())
                .cwd(cwd.clone())
                .capabilities(capabilities.clone())
                .scope(*scope)
                .rule(rule.clone().unwrap_or_default())
                .choices(choices.clone())
                .stages(stages.clone(), *current_stage)
                .badges(*badges)
                .resolved_by(*resolved_by);
            if folds.at_rest {
                card = card.at_rest();
            }
            if let Some(feedback) = feedback {
                card = card.feedback(feedback.clone());
            }
            let Some(cards) = &folds.cards else { return card.into_any_element() };
            let open_here = cards.feedback_open.as_ref().filter(|(a, _)| a == approval_id);
            if let Some((_, choice_id)) = open_here {
                card = card.feedback_open(Some(choice_id.clone())).feedback_text(cards.feedback_text.clone());
                if let Some(slot) = cards.feedback_slot.borrow_mut().take() {
                    card = card.feedback_slot(slot);
                }
            }
            let choose = cards.choose.clone();
            let toggle_feedback = cards.feedback_toggle.clone();
            let (a, b) = (approval_id.clone(), approval_id.clone());
            card.on_choose(move |choice_id, feedback, window, cx| choose(a.clone(), choice_id, feedback, window, cx))
                .on_feedback_toggle(move |choice_id, window, cx| toggle_feedback(b.clone(), choice_id, window, cx))
                .into_any_element()
        }
        Block::Question { id: question_id, header, prompt, subtitle, options, multi, allow_other, answer, timeout_ms } => {
            if let Some(answer) = answer {
                return settled_row(id, options, answer);
            }
            let mut card = question_card(id, prompt.clone(), options.clone())
                .header(header.clone())
                .subtitle(subtitle.clone())
                .multi(*multi)
                .allow_other(*allow_other);
            // The deadline is the block's, so the pill is drawn whether or not
            // anything is wired up to answer; the *countdown* below needs the
            // app's clock and replaces it.
            if let Some(total) = timeout_ms {
                card = card.timeout(*total, *total);
            }
            let Some(cards) = &folds.cards else { return card.into_any_element() };
            if let Some(selected) = cards.selections.get(question_id) {
                card = card.selected(selected.clone());
            }
            if let Some(open) = cards.previews.get(question_id) {
                card = card.previews_open(open.clone());
            }
            // The card draws the pill; the clock is the app's, because MSP sends
            // a duration and never a deadline.
            if let Some((remaining, total)) = cards.countdowns.get(question_id) {
                card = card.timeout(*remaining, *total);
            }
            if cards.clarify_open.as_deref() == Some(question_id.as_str()) {
                card = card.clarify_open(true);
                if let Some(slot) = cards.clarify_slot.borrow_mut().take() {
                    card = card.clarify_slot(slot);
                }
            }
            let (select, answer, skip, clarify, preview) =
                (cards.select.clone(), cards.answer.clone(), cards.skip.clone(), cards.clarify.clone(), cards.toggle_preview.clone());
            let ids = std::iter::repeat_n(question_id.clone(), 5).collect::<Vec<_>>();
            card.on_select({
                let id = ids[0].clone();
                move |index, window, cx| select(id.clone(), index, window, cx)
            })
            .on_toggle_preview({
                let id = ids[1].clone();
                move |index, window, cx| preview(id.clone(), index, window, cx)
            })
            .on_answer({
                let id = ids[2].clone();
                move |_, window, cx| answer(id.clone(), window, cx)
            })
            .on_skip({
                let id = ids[3].clone();
                move |_, window, cx| skip(id.clone(), window, cx)
            })
            .on_clarify({
                let id = ids[4].clone();
                move |_, window, cx| clarify(id.clone(), window, cx)
            })
            .into_any_element()
        }
        Block::Plan { id: plan_id, items, sections, state } => {
            let mut card = plan_card(id, items.clone()).sections(sections.clone()).state(*state);
            if let Some(handler) = folds.plan.clone() {
                let act = |action: PlanAction| {
                    let handler = handler.clone();
                    let plan_id = plan_id.clone();
                    move |_: &gpui::ClickEvent, window: &mut Window, cx: &mut App| {
                        handler(plan_id.clone(), action, window, cx)
                    }
                };
                card = card
                    .on_accept(act(PlanAction::Accept))
                    .on_edit(act(PlanAction::Refine))
                    .on_reject(act(PlanAction::Reject));
            }
            card.into_any_element()
        }
        Block::Todo { items } => todo_list(id, items.clone()).open(folds.open(key, true)).on_toggle(toggle).into_any_element(),
        Block::Summary { title, files, checks, duration_ms, cost_usd } => {
            summary_card(id, title.clone(), format!("{} · ${cost_usd:.2}", elapsed(*duration_ms)))
                .files(files.clone())
                .checks(checks.clone())
                .into_any_element()
        }
        Block::Error { title, detail, retryable } => {
            let mut card = error_card(id, title.clone(), detail.clone());
            // The retry resends the failed turn's own input, so it is only
            // offered when the app still has that text: a button that would
            // send an empty prompt is worse than no button.
            if *retryable {
                if let Some(cards) = &folds.cards {
                    if cards.retryable_turns.contains(turn_id) {
                        let retry = cards.retry.clone();
                        let turn_id = turn_id.to_owned();
                        card = card.on_retry(move |_, window, cx| retry(turn_id.clone(), window, cx));
                    }
                }
            }
            card.into_any_element()
        }
        Block::Goal { objective, status, percent_complete, current_work, next_work } => {
            let mut card = goal_card(id, objective.clone(), status.clone()).percent(*percent_complete);
            if let Some(work) = current_work {
                card = card.current_work(work.clone());
            }
            if let Some(work) = next_work {
                card = card.next_work(work.clone());
            }
            card.into_any_element()
        }
        // MSP's item kinds are an open set and mandate exactly this fallback:
        // the kind, the status and the server's own one-line text.
        Block::Generic { kind, status, text } => {
            generic_item_card(id, kind.clone(), status.clone(), text.clone()).into_any_element()
        }
        Block::Marker { kind, text } => marker(id, kind, text, folds, cx),
    }
}

/// One tool invocation as its card: the body the lone `Block::ToolCall` would
/// have shown, with the same fold toggle and full-output fetch keyed by the
/// call's own id (so grouped calls behave like lone ones).
fn tool_call_card(key: &str, id: ElementId, call: &ToolCall, folds: &Folds) -> AnyElement {
    let block_id = call.id.clone();
    let full = folds.full_output.get(&block_id);
    let mut body = call.body.clone();
    // A fetched full output replaces the truncated visible text on the
    // server's result only (D4): the fold never changes, the card just
    // renders what the fetch returned.
    if let (ToolBody::Shell { output_lines, .. }, Some(full)) = (&mut body, full) {
        if let FullOutputState::Ready { lines, capped } = &full.state {
            *output_lines = lines.clone();
            if *capped {
                output_lines.push(crate::full_output::CAPPED_MARKER.to_owned());
            }
        }
    }
    // The card's own action slot: its fold row already emits Unfold,
    // so on a truncated card that intent is "Show full output" and
    // fetches; everywhere else every intent still just toggles, as
    // before. The row's label stays the library's ("N more lines") —
    // the library owns the card's text and this app does not change
    // it.
    let (fetchable, idle) = full
        .map(|full| (full.fetchable, matches!(full.state, FullOutputState::Idle)))
        .unwrap_or((false, false));
    let show = folds.show_full_output.clone();
    tool_card(id, call.verb.clone(), call.target.clone(), call.status, body)
        .duration_ms(call.duration_ms)
        .open(folds.open(key, true))
        .on_intent({
            let toggle = folds.toggle.clone();
            let key = key.to_owned();
            move |intent, window, cx| match intent {
                ToolCardIntent::Unfold if fetchable && idle => {
                    if let Some(show) = &show {
                        show(block_id.clone(), window, cx);
                    } else {
                        toggle(key.clone(), window, cx);
                    }
                }
                _ => toggle(key.clone(), window, cx),
            }
        })
        .into_any_element()
}

/// The collapsed row a settled question leaves behind.
///
/// MSP settles a prompt six ways and only one of them is an answer, so the row
/// says which: an empty `Answer` after a cancel would otherwise read as
/// "Answered:" with nothing after it.
fn settled_row(id: ElementId, options: &[aui_protocol::QuestionOption], answer: &Answer) -> AnyElement {
    let chips: Vec<SharedString> = answer
        .selected
        .iter()
        .filter_map(|i| options.get(*i))
        .map(|o| SharedString::from(o.label.clone()))
        .collect();
    let outcome = match (chips.is_empty(), &answer.other) {
        // A settlement with free text and no chosen option is a clarification:
        // the person wrote instead of picking.
        (true, Some(text)) => QuestionOutcome::Clarified(SharedString::from(text.clone())),
        (true, None) => QuestionOutcome::Skipped,
        (false, _) => QuestionOutcome::Answered,
    };
    let mut chips = chips;
    chips.extend(answer.other.clone().filter(|_| outcome == QuestionOutcome::Answered).map(SharedString::from));
    answered_row(id, chips).outcome(outcome).into_any_element()
}

/// A hairline marker row, tinted only where the marker carries a warning.
///
/// The tint is the design rules' one exception: status colour carries meaning,
/// so a retry and a hole in the transcript are warning-coloured and everything
/// else is the muted line iconography every other marker uses.
fn marker(id: ElementId, kind: &MarkerKind, text: &str, folds: &Folds, cx: &mut App) -> AnyElement {
    use aui_tokens::ActiveAui;
    let p = cx.aui().colors;
    let row = marker_row(id);
    match kind {
        MarkerKind::SessionStarted => row.glyph(IconName::Play, None).text(text.to_owned()),
        MarkerKind::ContextCompacted => row.glyph(IconName::Layout, None).text(text.to_owned()),
        // The fold's own text already ends in the mode's label, so the row does
        // not name it twice; the emphasis goes on the label inside the text.
        MarkerKind::PermissionModeChanged { mode } => {
            let head = text.strip_suffix(mode.label()).unwrap_or(text);
            row.glyph(IconName::Shield, None).text(head.to_owned()).strong(mode.label())
        }
        MarkerKind::TurnCancelled => row.glyph(IconName::X, None).text("Turn interrupted").strong(text.to_owned()),
        MarkerKind::TurnRetracted => row.glyph(IconName::X, None).text("Prompt retracted"),
        MarkerKind::RetryScheduled => row.glyph(IconName::Refresh, Some(p.warning)).text(text.to_owned()),
        MarkerKind::ViewGap => row
            .glyph(IconName::Shield, Some(p.warning))
            .text("Some events were missed while disconnected"),
        // The fold knows the source session's id and nothing else; the sidebar
        // knows what it is called, so the title is joined in here.
        MarkerKind::ForkedFrom => {
            let source = text.strip_prefix("Forked from ").unwrap_or(text);
            let label = folds.titles.get(source).cloned().unwrap_or_else(|| id_group(source));
            row.glyph(IconName::Git, None).text("Forked from ").strong(label)
        }
        // Muse is one provider, so the fold never raises a hand-off; if a
        // later provider does, the plain row still says what happened.
        MarkerKind::HandOff { .. } => row.glyph(IconName::ArrowRight, None).text(text.to_owned()),
    }
    .into_any_element()
}

/// The first group of a uuid, which is what a session with no title is called
/// everywhere else in this app.
fn id_group(id: &str) -> String {
    id.split('-').next().unwrap_or(id).to_owned()
}

/// `12.4 s`, `1 m 12 s` — the library's own formatting, so every duration in
/// the app reads the same.
pub fn elapsed(ms: u64) -> SharedString {
    SharedString::from(aui::transcript::format_duration(ms))
}

/// The empty transcript: what a brand-new session shows before the first turn.
pub fn empty_state(
    workspace: &str,
    on_pick: Option<PickSuggestion>,
    cx: &mut App,
) -> AnyElement {
    use aui_tokens::{ActiveAui, AuiStyled};
    let p = cx.aui().colors;
    let name = std::path::Path::new(workspace)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| workspace.to_owned());
    let mut column = v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(scale::SP_3))
        .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child("New session"))
        .child(div().ui(scale::FS_12).text_color(p.ink_3).child(format!("Muse runs in {name}.")));
    // Three ways in, for a person looking at a blank page. They are prompts
    // about the workspace itself, so none of them assumes a project this is
    // not — and picking one only fills the composer, it never sends.
    if let Some(on_pick) = on_pick {
        column = column.child(
            aui::composer::suggestion_chips("empty-suggestions", SUGGESTIONS.iter().map(|s| (*s).into()).collect())
                .on_pick(move |index, window, cx| on_pick(index, window, cx)),
        );
    }
    column.into_any_element()
}

/// A suggestion chip was picked: the caller puts its text in the composer.
pub type PickSuggestion = std::rc::Rc<dyn Fn(usize, &mut gpui::Window, &mut App)>;

/// The text of the chip at `index`, for the caller that has to put it in the
/// composer.
pub fn suggestion(index: usize) -> Option<&'static str> {
    SUGGESTIONS.get(index).copied()
}

/// The three chips a fresh session offers.
///
/// Short, about the workspace rather than about a project the harness has not
/// looked at, and each one is a thing a person genuinely opens a session for.
const SUGGESTIONS: [&str; 3] = [
    "What is in this workspace?",
    "Explain how this project is laid out",
    "Find the entry point and walk me through it",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(blocks: Vec<Block>, reasoning_tokens: u64) -> Turn {
        Turn::Assistant {
            id: "turn-1".to_owned(),
            blocks,
            meta: TurnMeta {
                model: "muse-spark-1.3-contributor".to_owned(),
                duration_ms: 17_339,
                tokens_in: 58_527,
                tokens_out: 625,
                reasoning_tokens,
                cost_usd: 0.0,
            },
        }
    }

    fn text_block() -> Block {
        Block::Text { text: "done".to_owned(), streaming: false }
    }

    fn thinking_block() -> Block {
        Block::Thinking {
            text: "hmm".to_owned(),
            elapsed_ms: 0,
            summary: None,
            state: ThinkingState::Done,
        }
    }

    #[test]
    fn billed_reasoning_with_no_thinking_card_is_silent() {
        assert_eq!(silent_reasoning(&assistant(vec![text_block()], 419)), Some(419));
    }

    #[test]
    fn no_billed_reasoning_is_not_silent() {
        assert_eq!(silent_reasoning(&assistant(vec![text_block()], 0)), None);
    }

    #[test]
    fn a_visible_thinking_card_speaks_for_itself() {
        assert_eq!(
            silent_reasoning(&assistant(vec![thinking_block(), text_block()], 419)),
            None
        );
    }

    #[test]
    fn a_user_turn_never_thinks_silently() {
        let turn = Turn::User {
            id: "user-1".to_owned(),
            text: "hi".to_owned(),
            attachments: Vec::new(),
            mentions: Vec::new(),
        };
        assert_eq!(silent_reasoning(&turn), None);
    }

    #[test]
    fn the_silent_cell_names_the_footer_count() {
        assert_eq!(silent_reasoning_text(419), "419 reasoning, thought silently");
    }

    #[test]
    fn the_silent_footer_keeps_the_library_cells() {
        let Turn::Assistant { meta, .. } = assistant(vec![text_block()], 419) else {
            unreachable!("test helper builds an assistant turn")
        };
        assert_eq!(
            silent_footer_items(&meta, 419),
            vec![
                "muse-spark-1.3-contributor".to_owned(),
                "17.3 s".to_owned(),
                "59.2k tokens".to_owned(),
                "419 reasoning, thought silently".to_owned(),
            ]
        );
    }
}
