//! Find in the open transcript (docs/82 7.5): ⌘F with a session open.
//!
//! **What is searched** is what the transcript draws as text: the person's
//! bubbles and the assistant's prose, through the library's
//! [`aui::transcript::markdown_find`] — the same cells, the same shaped text
//! (a link's label, never its URL) — so "3 of 12" never counts a word nobody
//! can see. Tool cards, reasoning traces, plan and question cards are not
//! searched: they draw their own text in their own ways, mostly folded.
//!
//! **Folded turns count only what is shown** (the decision the plan left
//! open). A settled turn under "Fold finished turns" shows its answer and
//! hides the work behind "Worked for …"; find counts the answer and not the
//! hidden notes. Opening the fold (or flipping the setting) makes the next
//! step search again, so the hidden notes join the count once they are drawn.
//! Expanding the turn holding a match was the alternative; it would move the
//! person's own folds under them on every ⏎.
//!
//! **Cost.** Nothing runs while the bar is closed: the transcript's frame reads
//! one `Option` (no marks), and no search runs per frame. A search runs when
//! the query changes (and when a step finds the transcript changed since): a
//! transcript under [`FIND_SYNC_BYTES`] of text is searched in place; a
//! longer one on the background executor, the latest query winning.
//!
//! **State** is its own entity, [`TranscriptFind`], held by
//! [`Overlays`](crate::overlays::Overlays)' `transcript_find` — one bar per
//! window, naming the session it belongs to — never a field of
//! [`SessionView`] (standing rule 9). Every change is one write to that
//! owner (`Entity::as_mut`, which notifies once); the transcript reads the
//! owner while it renders, so only a changed search or step redraws it. The
//! bar is composed live by the application root above the cached column
//! ([`SessionView::render_find_bar`]), so typing in it never rebuilds the
//! transcript; the marks reach the transcript through
//! [`Folds`](crate::transcript::Folds).

use super::*;
use std::ops::Range;

use aui::data::button;
use aui::transcript::{FindMark, SelectionKey};
use aui_tokens::{ActiveAui as _, AuiStyled as _, TextRole};
use gpui_kit::base::input::InputState;

/// Above this much transcript text the search leaves the UI thread.
pub(crate) const FIND_SYNC_BYTES: usize = 256 * 1024;

/// One markdown view the transcript draws, as find reads it: the turn and
/// the row within it (the list's `(turn, row)`), the view's key (a user
/// turn's id, an assistant text block's [`transcript::block_key`]), its
/// source and whether it draws settled.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FindSource {
    pub turn: usize,
    pub row: usize,
    pub view: String,
    pub text: String,
    pub settled: bool,
}

/// One match: where it is drawn (turn, row, view, cell, byte range over the
/// cell's shaped text) and how far through its view it starts (0–1).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FindMatch {
    pub turn: usize,
    pub row: usize,
    pub view: String,
    pub cell: SelectionKey,
    pub range: Range<usize>,
    pub position: f32,
}

/// What a search ran over: a step after any of these changed searches again.
#[derive(Clone)]
struct Searched {
    turns: Rc<Vec<Rc<Turn>>>,
    toggled: Rc<HashSet<String>>,
    fold: bool,
    live_tail: bool,
}

impl Searched {
    fn same(&self, other: &Searched) -> bool {
        Rc::ptr_eq(&self.turns, &other.turns)
            && Rc::ptr_eq(&self.toggled, &other.toggled)
            && self.fold == other.fold
            && self.live_tail == other.live_tail
    }
}

/// The open find bar: its session, its field, and what the field found.
pub(crate) struct TranscriptFind {
    /// The session whose transcript the bar searches; any other session's
    /// view draws no bar and no marks.
    pub(crate) session: String,
    pub(crate) input: Entity<InputState>,
    _subscription: gpui::Subscription,
    /// The query the matches were found for.
    pub(crate) query: String,
    pub(crate) match_case: bool,
    pub(crate) matches: Rc<Vec<FindMatch>>,
    /// Index into `matches` of the current match.
    pub(crate) current: Option<usize>,
    /// The marks each markdown view paints, by view key.
    pub(crate) marks: Rc<HashMap<String, Rc<Vec<FindMark>>>>,
    searched: Option<Searched>,
    /// Bumped per search, so a background result for an older query is
    /// dropped.
    generation: u64,
}

/// The views a transcript draws as text, in order: every user bubble (but
/// one drawn as an annotation batch, `annotated`), and every assistant text
/// block that takes a row under the current fold mapping. `live_tail` marks
/// the newest turn as still arriving (its rows map live).
pub(crate) fn find_sources(
    turns: &[Rc<Turn>],
    toggled: &HashSet<String>,
    fold: bool,
    live_tail: bool,
    annotated: &dyn Fn(&str) -> bool,
) -> Vec<FindSource> {
    let last = turns.len().saturating_sub(1);
    let mut out = Vec::new();
    for (turn_ix, turn) in turns.iter().enumerate() {
        let settled = turn_ix != last || !live_tail;
        match turn.as_ref() {
            Turn::User { id, text, .. } => {
                if !text.trim().is_empty() && !annotated(id) {
                    out.push(FindSource { turn: turn_ix, row: 0, view: id.clone(), text: text.clone(), settled: true });
                }
            }
            Turn::Assistant { id, blocks, .. } => {
                let rows = transcript::turn_mapped_rows(turn, settled, toggled, fold);
                for row in 0..rows {
                    let Some(transcript::TurnSlot::Block(index)) = transcript::turn_slot_at(turn, settled, toggled, fold, row) else {
                        continue;
                    };
                    if let Some(Block::Text { text, streaming }) = blocks.get(index) {
                        out.push(FindSource {
                            turn: turn_ix,
                            row,
                            view: transcript::block_key(id, index),
                            text: text.clone(),
                            settled: !streaming,
                        });
                    }
                }
            }
        }
    }
    out
}

/// Every match of `query` over `sources`, in transcript order.
pub(crate) fn search_sources(sources: &[FindSource], query: &str, match_case: bool) -> Vec<FindMatch> {
    let mut out = Vec::new();
    for source in sources {
        for found in aui::transcript::markdown_find(&source.text, source.settled, query, match_case) {
            out.push(FindMatch {
                turn: source.turn,
                row: source.row,
                view: source.view.clone(),
                cell: found.cell,
                range: found.range,
                position: found.position,
            });
        }
    }
    out
}

/// The marks each view paints: every match, the current one flagged.
pub(crate) fn build_marks(matches: &[FindMatch], current: Option<usize>) -> HashMap<String, Rc<Vec<FindMark>>> {
    let mut by_view: HashMap<String, Vec<FindMark>> = HashMap::new();
    for (ix, found) in matches.iter().enumerate() {
        by_view.entry(found.view.clone()).or_default().push(FindMark {
            cell: found.cell.clone(),
            range: found.range.clone(),
            current: Some(ix) == current,
        });
    }
    by_view.into_iter().map(|(view, marks)| (view, Rc::new(marks))).collect()
}

/// The next (`forward`) or previous match from `current` among `total`,
/// wrapping at both ends; from no current match, the first or the last.
pub(crate) fn wrap_index(current: Option<usize>, total: usize, forward: bool) -> Option<usize> {
    if total == 0 {
        return None;
    }
    Some(match (current, forward) {
        (None, true) => 0,
        (None, false) => total - 1,
        (Some(at), true) => (at + 1) % total,
        (Some(at), false) => (at + total - 1) % total,
    })
}

/// Where a fresh search starts: the first match at or below the list row
/// the reader is looking at (`top_item`), else the first one (wrapping).
pub(crate) fn first_from(matches: &[FindMatch], rows: &[(usize, usize)], top_item: usize) -> Option<usize> {
    if matches.is_empty() {
        return None;
    }
    let anchor = rows.get(top_item).copied();
    let at = anchor.and_then(|(turn, row)| matches.iter().position(|m| (m.turn, m.row) >= (turn, row)));
    Some(at.unwrap_or(0))
}

/// The list item a match is drawn in.
pub(crate) fn scroll_item(rows: &[(usize, usize)], found: &FindMatch) -> Option<usize> {
    rows.iter().position(|&(turn, row)| turn == found.turn && row == found.row)
}

/// The count beside the field: "3 of 12", "No matches", nothing before a
/// query.
pub(crate) fn count_label(query: &str, current: Option<usize>, total: usize) -> String {
    if query.is_empty() {
        return String::new();
    }
    match (current, total) {
        (_, 0) => "No matches".to_owned(),
        (Some(at), total) => format!("{} of {total}", at + 1),
        (None, total) => format!("{total} matches"),
    }
}

impl SessionView {
    /// This view's open find, if the window's bar belongs to it.
    fn find_state<'a>(&self, cx: &'a gpui::App) -> Option<&'a TranscriptFind> {
        let session = &self.rt(cx).session_id;
        self.overlays.read(cx).transcript_find.as_ref().map(|find| find.read(cx)).filter(|find| &find.session == session)
    }

    /// The bar's owner entity, when it is this view's. Every change goes
    /// through it (`as_mut`), whose write notifies once; the transcript reads
    /// it while it renders, so that notify is what redraws the column.
    fn find_entity(&self, cx: &gpui::App) -> Option<Entity<TranscriptFind>> {
        let session = &self.rt(cx).session_id;
        self.overlays.read(cx).transcript_find.clone().filter(|find| &find.read(cx).session == session)
    }

    /// Whether this view's find bar stands open.
    pub(crate) fn find_open(&self, cx: &gpui::App) -> bool {
        self.find_state(cx).is_some()
    }

    /// The marks this frame paints: empty (one shared map, no work) unless
    /// this view's bar is open.
    pub(super) fn find_marks_for_frame(&self, cx: &gpui::App) -> Rc<HashMap<String, Rc<Vec<FindMark>>>> {
        thread_local! {
            static NONE: Rc<HashMap<String, Rc<Vec<FindMark>>>> = Rc::new(HashMap::new());
        }
        match self.find_state(cx) {
            Some(find) => Rc::clone(&find.marks),
            None => NONE.with(Rc::clone),
        }
    }

    /// ⌘F: open the bar on this transcript with the field focused and its
    /// text selected, or refocus it. A kept query searches again.
    pub(crate) fn open_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(find) = self.find_state(cx) {
            let input = find.input.clone();
            input.update(cx, |input, cx| {
                input.select_all(window, cx);
                input.focus(window, cx);
            });
            return;
        }
        let session = self.rt(cx).session_id.clone();
        let input = cx.new(|cx| InputState::new(window, cx));
        input.update(cx, |input, cx| {
            input.set_placeholder("Find in transcript", window, cx);
            input.focus(window, cx);
        });
        let subscription = cx.subscribe_in(&input, window, |this: &mut Self, _, event: &InputEvent, _, cx| match event {
            InputEvent::PressEnter { .. } => {
                this.find_step(true, cx);
            }
            InputEvent::Change => this.find_query_changed(cx),
            InputEvent::Blur | InputEvent::Focus => {}
        });
        let find = TranscriptFind {
            session,
            input,
            _subscription: subscription,
            query: String::new(),
            match_case: false,
            matches: Rc::new(Vec::new()),
            current: None,
            marks: Rc::new(HashMap::new()),
            searched: None,
            generation: 0,
        };
        let find = cx.new(|_| find);
        self.overlays.as_mut(cx).transcript_find = Some(find);
    }

    /// Esc, Done: close the bar and clear every mark; the keyboard goes back
    /// to the composer. A no-op when this view's bar is not open.
    pub(crate) fn close_find(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.find_open(cx) {
            return false;
        }
        self.overlays.as_mut(cx).transcript_find = None;
        self.focus_composer(window, cx);
        true
    }

    /// What the search reads now.
    fn find_inputs(&self, cx: &gpui::App) -> Searched {
        Searched {
            turns: Rc::clone(&self.cached_turns),
            toggled: Rc::clone(&self.toggled),
            fold: crate::layout::Layout::get(cx).fold_finished_turns,
            live_tail: self.runtime.read(cx).working() && !crate::runtime::captures(),
        }
    }

    /// The field changed: search for the new text.
    fn find_query_changed(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find_state(cx) else { return };
        let typed = find.input.read(cx).value().to_string();
        if typed == find.query {
            return;
        }
        self.run_find(typed, true, cx);
    }

    /// Search `query` over the transcript as it is drawn now; a fresh
    /// search (`fresh`) starts at the first match from the reader's place,
    /// a re-search keeps the current match's ordinal.
    fn run_find(&mut self, query: String, fresh: bool, cx: &mut Context<Self>) {
        let Some(find) = self.find_state(cx) else { return };
        let match_case = find.match_case;
        let generation = find.generation + 1;
        let kept = find.current;
        let inputs = self.find_inputs(cx);
        let sent = Rc::clone(&self.sent_pairs);
        let annotated = move |id: &str| sent.get(id).is_some_and(|item| item.annotations.is_some());
        let sources = find_sources(&inputs.turns, &inputs.toggled, inputs.fold, inputs.live_tail, &annotated);
        let bytes: usize = sources.iter().map(|s| s.text.len()).sum();
        if let Some(entity) = self.find_entity(cx) {
            let mut find = entity.as_mut(cx);
            find.query = query.clone();
            find.generation = generation;
            find.searched = Some(inputs);
        }
        if query.is_empty() {
            self.apply_find(generation, Vec::new(), None, cx);
            return;
        }
        if bytes <= FIND_SYNC_BYTES {
            let matches = search_sources(&sources, &query, match_case);
            let current = self.find_start(&matches, fresh, kept);
            self.apply_find(generation, matches, current, cx);
            return;
        }
        cx.spawn(async move |this, cx| {
            let matches = cx.background_executor().spawn(async move { search_sources(&sources, &query, match_case) }).await;
            let _ = this.update(cx, |this, cx| {
                let current = this.find_start(&matches, fresh, kept);
                this.apply_find(generation, matches, current, cx);
            });
        })
        .detach();
    }

    /// Where a search's current match lands.
    fn find_start(&self, matches: &[FindMatch], fresh: bool, kept: Option<usize>) -> Option<usize> {
        if matches.is_empty() {
            return None;
        }
        if !fresh {
            return Some(kept.unwrap_or(0).min(matches.len() - 1));
        }
        first_from(matches, &self.rows, self.list_state.logical_scroll_top().item_ix)
    }

    /// Land a search's result (dropped if a newer one started) and bring
    /// the current match into view.
    fn apply_find(&mut self, generation: u64, matches: Vec<FindMatch>, current: Option<usize>, cx: &mut Context<Self>) {
        let target = current.and_then(|at| matches.get(at).cloned());
        let Some(entity) = self.find_entity(cx).filter(|find| find.read(cx).generation == generation) else { return };
        {
            let mut find = entity.as_mut(cx);
            find.marks = Rc::new(build_marks(&matches, current));
            find.matches = Rc::new(matches);
            find.current = current;
        }
        if let Some(target) = target {
            self.scroll_to_find_match(&target);
        }
    }

    /// ⏎ / ⇧⏎ / ⌘G / ⇧⌘G: the next or previous match, wrapping, scrolled
    /// into view. A transcript that changed since the search (a new turn, a
    /// fold opened) is searched again first. `false` when nothing moved: no
    /// bar on this view, an empty query, or no match.
    pub(crate) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let Some(find) = self.find_state(cx) else { return false };
        if find.query.is_empty() {
            return false;
        }
        let stale = find.searched.as_ref().is_none_or(|searched| !searched.same(&self.find_inputs(cx)));
        if stale {
            let query = find.query.clone();
            self.run_find(query, false, cx);
        }
        let Some(find) = self.find_state(cx) else { return false };
        let Some(next) = wrap_index(find.current, find.matches.len(), forward) else { return false };
        let target = find.matches[next].clone();
        let matches = Rc::clone(&find.matches);
        if let Some(entity) = self.find_entity(cx) {
            let mut find = entity.as_mut(cx);
            find.current = Some(next);
            find.marks = Rc::new(build_marks(&matches, Some(next)));
        }
        self.scroll_to_find_match(&target);
        true
    }

    /// Flip "Match case" and search again.
    fn toggle_find_case(&mut self, cx: &mut Context<Self>) {
        let Some(find) = self.find_state(cx) else { return };
        let query = find.query.clone();
        if let Some(entity) = self.find_entity(cx) {
            let mut find = entity.as_mut(cx);
            find.match_case = !find.match_case;
        }
        self.run_find(query, true, cx);
    }

    /// Bring `found` into view: nothing when it is already visible;
    /// otherwise its row at the list's top (a message taller than the
    /// viewport: scrolled so the match sits a third of the way down). The
    /// bar stands above the list, never over it, so the row's top is clear.
    fn scroll_to_find_match(&mut self, found: &FindMatch) {
        let Some(ix) = scroll_item(&self.rows, found) else { return };
        let viewport = self.list_state.viewport_bounds();
        let height = self.list_state.bounds_for_item(ix).map(|b| (f32::from(b.top()), f32::from(b.size.height)));
        if let Some((top, h)) = height {
            let y = top + found.position * h;
            let visible_bottom = f32::from(viewport.bottom()) - self.bottom_inset - self.stack_h.get();
            if viewport.size.height > px(0.0) && y >= f32::from(viewport.top()) && y + scale::SP_6 <= visible_bottom {
                return;
            }
        }
        let view_h = f32::from(viewport.size.height).max(1.0);
        let inner = match height {
            Some((_, h)) if h > view_h * 0.6 => (found.position * h - view_h / 3.0).max(0.0),
            _ => 0.0,
        };
        self.follow = false;
        self.at_end_before = false;
        // The bar opening shrinks the list by its height on the next layout;
        // a list that stood at its end would follow its end through that
        // change (`note_list_height`) and undo this scroll. Forgetting the
        // last height makes that layout a first measurement, not a change.
        self.list_height = None;
        self.list_state.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: px(inner) });
    }

    /// The find bar above the transcript: the field, "3 of 12", ↑ ↓, Match
    /// case, Done. Composed live by the root above the cached column (the
    /// list gives up the bar's height while it stands). Esc closes; ⏎ steps
    /// on, ⇧⏎ back.
    pub fn render_find_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.find_state(cx)?;
        let p = cx.aui().colors;
        let input = find.input.clone();
        let match_case = find.match_case;
        let total = find.matches.len();
        let current = find.current;
        let label = count_label(&find.query, current, total);
        let keys = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, window, cx| {
            if event.keystroke.key.as_str() == "escape" {
                this.close_find(window, cx);
                cx.stop_propagation();
            }
        });
        // Shift+Enter steps back in the capture phase and stops there, so
        // the field never also sees it and steps forward.
        let back = cx.listener(|this: &mut Self, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key.as_str() == "enter" && event.keystroke.modifiers.shift {
                this.find_step(false, cx);
                cx.stop_propagation();
            }
        });
        let count_ax = match (current, total) {
            (_, 0) if label.is_empty() => "No search yet".to_owned(),
            (_, 0) => "No matches".to_owned(),
            (Some(at), total) => format!("{} of {total} matches", at + 1),
            (None, total) => format!("{total} matches"),
        };
        let prev = button("transcript-find-prev", "↑")
            .ghost()
            .sm()
            .accessibility_label("Previous match")
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.find_step(false, cx);
            }));
        let next = button("transcript-find-next", "↓")
            .ghost()
            .sm()
            .accessibility_label("Next match")
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.find_step(true, cx);
            }));
        let case = button("transcript-find-case", "Match case")
            .sm()
            .accessibility_label(if match_case { "Match case on" } else { "Match case off" })
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.toggle_find_case(cx)));
        let case = if match_case { case.primary() } else { case.ghost() };
        let done = button("transcript-find-done", "Done")
            .sm()
            .accessibility_label("Close find")
            .on_click(cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
                this.close_find(window, cx);
            }));
        let row = h_flex()
            .id("transcript-find")
            .w_full()
            .flex_none()
            .items_center()
            .h(px(scale::H_XL))
            .px(px(scale::SP_3))
            .gap(px(scale::SP_2))
            .bg(p.surface_1)
            .border_b_1()
            .border_color(p.line)
            .role(gpui::Role::Group)
            .aria_label("Find in transcript")
            .debug_selector(|| "transcript-find".into())
            .capture_key_down(back)
            .on_key_down(keys)
            .child(
                aui::data::field_frame("transcript-find-frame", &input, window, cx)
                    .flex_1()
                    .min_w_0()
                    .gap(px(scale::SP_2))
                    .child(aui_icons::icon(IconName::Search).size(px(scale::ICON_SM)).color(p.fg_muted))
                    .child(aui::a11y_text::a11y_text_input("transcript-find-field", "Find in transcript", &input, cx).flex_1().min_w_0()),
            )
            .child(
                div()
                    .id("transcript-find-count")
                    .flex_none()
                    .ui(TextRole::Caption)
                    .text_color(p.fg_muted)
                    .role(gpui::Role::Label)
                    .aria_label(count_ax)
                    .child(label),
            )
            .child(prev)
            .child(next)
            .child(case)
            .child(done);
        Some(row.into_any_element())
    }

    /// Probe: type `query` into the bar (opening it first), as a person
    /// would. `Err` when the bar did not open or the query is empty.
    #[cfg(feature = "probe")]
    pub(crate) fn step_find(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        if query.is_empty() {
            return Err("find: an empty query finds nothing".into());
        }
        self.open_find(window, cx);
        let input = self.find_state(cx).map(|find| find.input.clone()).ok_or("find: the bar did not open")?;
        input.update(cx, |input, cx| input.set_value(query.to_owned(), window, cx));
        // `set_value` emits no Change: search as the field's subscription would.
        if self.find_state(cx).is_some_and(|find| find.query != query) {
            self.run_find(query.to_owned(), true, cx);
        }
        let find = self.find_state(cx).ok_or("find: the bar closed")?;
        Ok(count_label(&find.query, find.current, find.matches.len()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::TurnMeta;

    fn user(id: &str, text: &str) -> Rc<Turn> {
        Rc::new(Turn::User { id: id.into(), text: text.into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None })
    }

    fn assistant(id: &str, texts: &[&str]) -> Rc<Turn> {
        let blocks = texts.iter().map(|t| Block::Text { text: (*t).into(), streaming: false }).collect();
        Rc::new(Turn::Assistant { id: id.into(), blocks, meta: TurnMeta::default(), timestamp: None })
    }

    fn transcript() -> Vec<Rc<Turn>> {
        vec![
            user("u1", "Find the Needle please"),
            assistant("a1", &["A needle is here.", "And NEEDLE again, needle."]),
            user("u2", "no match"),
            assistant("a2", &["last needle"]),
        ]
    }

    fn rows_for(turns: &[Rc<Turn>]) -> Vec<(usize, usize)> {
        let toggled = HashSet::new();
        let mut rows = Vec::new();
        for (ix, turn) in turns.iter().enumerate() {
            let n = transcript::turn_mapped_rows(turn, true, &toggled, false);
            rows.extend((0..n).map(|r| (ix, r)));
        }
        rows
    }

    #[test]
    fn counts_matches_case_insensitively_unless_asked() {
        let turns = transcript();
        let sources = find_sources(&turns, &HashSet::new(), false, false, &|_| false);
        assert_eq!(sources.len(), 5, "two bubbles and three text blocks");
        let all = search_sources(&sources, "needle", false);
        assert_eq!(all.len(), 5);
        assert_eq!(all.iter().map(|m| m.view.as_str()).collect::<Vec<_>>(), ["u1", "a1:0", "a1:1", "a1:1", "a2:0"]);
        let exact = search_sources(&sources, "needle", true);
        assert_eq!(exact.len(), 3, "Needle and NEEDLE drop out");
        assert!(search_sources(&sources, "", false).is_empty());
        assert_eq!(count_label("needle", Some(2), 5), "3 of 5");
        assert_eq!(count_label("zzz", None, 0), "No matches");
        assert_eq!(count_label("", None, 0), "");
    }

    #[test]
    fn annotated_bubbles_are_not_searched() {
        let turns = transcript();
        let sources = find_sources(&turns, &HashSet::new(), false, false, &|id| id == "u1");
        assert!(sources.iter().all(|s| s.view != "u1"));
    }

    #[test]
    fn next_and_previous_wrap_at_both_ends() {
        assert_eq!(wrap_index(None, 3, true), Some(0));
        assert_eq!(wrap_index(None, 3, false), Some(2));
        assert_eq!(wrap_index(Some(2), 3, true), Some(0), "past the last wraps to the first");
        assert_eq!(wrap_index(Some(0), 3, false), Some(2), "before the first wraps to the last");
        assert_eq!(wrap_index(Some(1), 3, true), Some(2));
        assert_eq!(wrap_index(Some(0), 0, true), None);
    }

    #[test]
    fn the_current_match_scrolls_to_its_own_row_and_starts_from_the_reader() {
        let turns = transcript();
        let rows = rows_for(&turns);
        let sources = find_sources(&turns, &HashSet::new(), false, false, &|_| false);
        let matches = search_sources(&sources, "needle", false);
        // Each match's list item is the row drawing its view.
        for m in &matches {
            let ix = scroll_item(&rows, m).expect("a drawn row");
            assert_eq!(rows[ix], (m.turn, m.row));
        }
        assert_eq!(scroll_item(&rows, &matches[4]), Some(rows.len() - 1));
        // A fresh search starts at the first match at or below the top row.
        assert_eq!(first_from(&matches, &rows, 0), Some(0));
        let second_block = rows.iter().position(|r| *r == (1, 1)).unwrap();
        assert_eq!(first_from(&matches, &rows, second_block), Some(2));
        // Past the last match, it wraps to the first.
        assert_eq!(first_from(&matches, &rows, rows.len() + 5), Some(0));
    }

    #[test]
    fn marks_flag_only_the_current_match_and_vanish_without_a_query() {
        let turns = transcript();
        let sources = find_sources(&turns, &HashSet::new(), false, false, &|_| false);
        let matches = search_sources(&sources, "needle", false);
        let marks = build_marks(&matches, Some(2));
        assert_eq!(marks.values().map(|m| m.len()).sum::<usize>(), 5);
        let current: Vec<_> = marks.values().flat_map(|m| m.iter()).filter(|m| m.current).collect();
        assert_eq!(current.len(), 1);
        assert_eq!(marks["a1:1"].iter().filter(|m| m.current).count(), 1);
        assert!(build_marks(&[], None).is_empty());
    }

    /// The bar end to end on a real view: ⌘F's open, typing searches (case
    /// folded), ⏎ and ⇧⏎ wrap, Match case narrows, Esc's close drops every
    /// mark from the transcript's frame and the bar.
    #[gpui::test]
    fn the_bar_counts_steps_and_clears_its_marks_on_close(cx: &mut gpui::TestAppContext) {
        use gpui::AppContext as _;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id: "muse".to_owned(),
                workspace: "/tmp/transcript-find".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            cx.new(|cx| SessionView::new("s-find".to_owned(), host, window, cx))
        });
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, _| {
                    rt.store.ensure_live(aui_protocol::Provider::Muse, "m", "/tmp/transcript-find");
                    let mut deltas = Vec::new();
                    for turn in transcript() {
                        let id = turn.id().to_owned();
                        let assistant = matches!(turn.as_ref(), Turn::Assistant { .. });
                        deltas.push(aui_protocol::Delta::TurnStarted { turn: (*turn).clone() });
                        if assistant {
                            deltas.push(aui_protocol::Delta::TurnFinished { turn_id: id, meta: TurnMeta::default() });
                        }
                    }
                    rt.store.apply_deltas(deltas);
                });
                view.sync_render_cache(cx);
                assert_eq!(view.cached_turns.len(), 4, "the transcript folded in");
            });
        });
        // Closed: nothing searched, no marks, no bar, steps do nothing.
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(!view.find_open(cx));
                assert!(view.find_marks_for_frame(cx).is_empty());
                assert!(view.render_find_bar(window, cx).is_none());
                assert!(!view.find_step(true, cx), "no bar: ⌘G does nothing");
            });
        });
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.open_find(window, cx);
                assert!(view.find_open(cx));
                view.run_find("NEEDLE".to_owned(), true, cx);
                let find = view.find_state(cx).expect("open");
                assert_eq!(find.matches.len(), 5, "case folded by default");
                assert_eq!(count_label(&find.query, find.current, find.matches.len()), "1 of 5");
                assert_eq!(view.find_marks_for_frame(cx).values().map(|m| m.len()).sum::<usize>(), 5);
                assert!(view.render_find_bar(window, cx).is_some());
                // ⏎ on, ⇧⏎ back, wrapping both ways.
                assert!(view.find_step(false, cx));
                assert_eq!(view.find_state(cx).unwrap().current, Some(4), "back from the first wraps to the last");
                assert!(view.find_step(true, cx));
                assert_eq!(view.find_state(cx).unwrap().current, Some(0), "on from the last wraps to the first");
                assert!(view.find_step(true, cx));
                let marks = view.find_marks_for_frame(cx);
                assert_eq!(marks["a1:0"].iter().filter(|m| m.current).count(), 1, "the second match is current");
                // Match case narrows to the exact spelling.
                view.toggle_find_case(cx);
                assert_eq!(view.find_state(cx).unwrap().matches.len(), 1, "only a1:1 spells it NEEDLE");
            });
        });
        vc.update(|window, cx| {
            view.update(cx, |view, cx| {
                assert!(view.close_find(window, cx));
                assert!(!view.find_open(cx));
                assert!(view.find_marks_for_frame(cx).is_empty(), "closing clears every highlight");
                assert!(view.render_find_bar(window, cx).is_none());
                assert!(!view.close_find(window, cx), "closing a closed bar does nothing");
            });
        });
    }
}
