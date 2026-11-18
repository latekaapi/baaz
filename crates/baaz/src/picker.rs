//! One picker (plan 4.6, audit A R5): a query, a filtered and ranked list,
//! keyboard selection, confirm and dismiss — the same gesture wherever a
//! list is picked from: the ⌘K palette and its lists, the Projects palette,
//! the composer's model picker, the `@` file mention and the agent card's
//! hand-off picker.
//!
//! [`Picker`] is the view and owns what every one of those had its own copy
//! of: where the selection is, the query field (its own, one it borrows, or
//! none when the query is typed elsewhere — the `@` token in the draft), the
//! arrows, ↩, Escape, the pointer's hover. A [`PickerDelegate`] owns what
//! differs: the rows, how a query narrows them, what confirming one yields,
//! and how they look. The look is the library's: `aui` already has the
//! stateless lists (`command_palette`, the composer's `PickerMenu`, the
//! `mention_picker`), so a delegate's [`PickerDelegate::render_match`]
//! answers the component's own row type and [`PickerDelegate::render_picker`]
//! wraps them in that component — nothing is drawn twice, and every picker
//! keeps its pixels.
//!
//! Zed's `picker` crate was read for the shape only (it is GPL).
//!
//! A pick leaves as [`Picked`] — or, for an owner that needs the window to
//! act on it (an `@` pick edits the draft), through the hook it gave
//! ([`Picker::on_pick`]) — and a dismissal as [`gpui::DismissEvent`]: the
//! owner maps the pick onto its own work, and the dismissal closes the
//! picker's overlay-stack entry (`Harness::push_overlay_view`). Moving the
//! selection, typing in the field and hovering a row redraw the picker
//! alone — never its owner.

use std::rc::Rc;

use gpui::prelude::*;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, InteractiveElement,
    IntoElement, Render, SharedString, Subscription, Window,
};
use gpui_kit::base::input::{InputEvent, TextareaState};

/// What confirming a row hands the picker's owner.
pub(crate) struct Picked<P>(pub P);

/// What one picker lists and how: the rows, the narrowing, the pick and the
/// look (see the module docs).
pub(crate) trait PickerDelegate: Sized + 'static {
    /// One row as the delegate's library component draws it.
    type Row;
    /// What confirming a row yields to the owner.
    type Pick: 'static;

    /// How many rows the current query leaves.
    fn match_count(&self) -> usize;

    /// Narrow (and rank) the rows to `query`. A delegate whose rows arrive
    /// from its owner (a background search) leaves this alone.
    fn update_matches(&mut self, _query: &str) {}

    /// Row `ix`'s stable id: what a click on it names.
    fn match_id(&self, ix: usize) -> Option<SharedString>;

    /// What confirming row `ix` yields — `ix` may be past the last row (↩
    /// on an empty list), which most delegates answer with nothing.
    fn confirm(&self, ix: usize) -> Option<Self::Pick>;

    /// What a click on the row `id` yields: the row with that id, confirmed.
    fn confirm_id(&self, id: &SharedString) -> Option<Self::Pick> {
        let ix = (0..self.match_count()).find(|ix| self.match_id(*ix).as_ref() == Some(id))?;
        self.confirm(ix)
    }

    /// Row `ix`, drawn the way this picker's component draws its rows.
    fn render_match(&self, ix: usize, selected: bool) -> Self::Row;

    /// The whole picker around its `rows`: the component, wired to the
    /// picker through `frame`.
    fn render_picker(
        &self,
        rows: Vec<Self::Row>,
        frame: PickerFrame,
        window: &mut Window,
        cx: &mut Context<Picker<Self>>,
    ) -> gpui::AnyElement;
}

/// Where a picker's query comes from.
pub(crate) enum PickerQuery {
    /// Typed elsewhere and handed in ([`Picker::set_query`]): the `@` token
    /// in the draft, or no query at all (the chip menus, the hand-off list).
    Typed(String),
    /// A field the picker draws inside its own card: its own, or one its
    /// owner keeps because the owner's work reads it (the search palette).
    Field(Entity<TextareaState>),
    /// The picker's own field, not made yet: a field needs the window, and
    /// a picker can open from a handler that has none. Its first frame
    /// makes it (with this placeholder) and hands it the keyboard.
    Unmade(&'static str),
}

type IdHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type IndexHandler = Rc<dyn Fn(usize, &mut Window, &mut App)>;
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The picker, as its delegate's component needs it this frame.
pub(crate) struct PickerFrame {
    /// The highlighted row.
    pub(crate) selected: usize,
    /// The query field, chromeless, for the component's query slot (`None`
    /// when the query is typed elsewhere).
    pub(crate) query: Option<gpui::AnyElement>,
    /// The query field itself, for a component that draws its own.
    pub(crate) field: Option<Entity<TextareaState>>,
    /// A click on a row, by its id: confirms it.
    pub(crate) select: IdHandler,
    /// The pointer moved onto row `ix`: it becomes the selection.
    pub(crate) hover: IndexHandler,
    /// The component's own dismissal (a click outside, its ✕).
    pub(crate) dismiss: Handler,
    /// The picker's own focus, which [`Self::keyed`] tracks.
    focus: FocusHandle,
    on_next: Handler,
    on_prev: Handler,
    on_confirm: Handler,
}

impl PickerFrame {
    /// `element` with the picker's keyboard: the menu key context, the
    /// picker's focus, and ↑ ↓ ↩ Escape bound to it. A picker that keeps
    /// the composer's caret (a chip menu, the `@` list) never takes this:
    /// the composer's own menu keys drive it there.
    pub(crate) fn keyed<E: InteractiveElement>(&self, element: E) -> E {
        let (next, prev, confirm, dismiss) =
            (self.on_next.clone(), self.on_prev.clone(), self.on_confirm.clone(), self.dismiss.clone());
        element
            .key_context(aui::keys::MENU_CONTEXT)
            .track_focus(&self.focus)
            .on_action(move |_: &aui::keys::SelectNext, window, cx| next(window, cx))
            .on_action(move |_: &aui::keys::SelectPrev, window, cx| prev(window, cx))
            .on_action(move |_: &aui::keys::Confirm, window, cx| confirm(window, cx))
            .on_action(move |_: &aui::keys::Cancel, window, cx| dismiss(window, cx))
    }
}

/// The picker view (see the module docs).
pub(crate) struct Picker<D: PickerDelegate> {
    /// What it lists, and how.
    pub(crate) delegate: D,
    query: Option<PickerQuery>,
    selected: usize,
    focus: FocusHandle,
    _query_changed: Option<Subscription>,
    /// Where a pick goes when the owner takes it with the window (the
    /// composer's pickers: an `@` pick edits the draft); otherwise a pick
    /// is a [`Picked`] event.
    on_pick: Option<PickHook<D::Pick>>,
}

/// An owner's pick handler, with the window.
type PickHook<P> = Rc<dyn Fn(&P, &mut Window, &mut App)>;

impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}
impl<D: PickerDelegate> EventEmitter<Picked<D::Pick>> for Picker<D> {}

impl<D: PickerDelegate> Picker<D> {
    /// A picker whose query is typed elsewhere (or that has none).
    pub(crate) fn new(delegate: D, cx: &mut Context<Self>) -> Self {
        Picker { delegate, query: Some(PickerQuery::Typed(String::new())), selected: 0, focus: cx.focus_handle(), _query_changed: None, on_pick: None }
    }

    /// Hand picks to `hook` (with the window) instead of the [`Picked`]
    /// event.
    pub(crate) fn on_pick(mut self, hook: impl Fn(&D::Pick, &mut Window, &mut App) + 'static) -> Self {
        self.on_pick = Some(Rc::new(hook));
        self
    }

    /// A picker with a query field drawn inside it: `field` (its own, or
    /// one its owner keeps). Typing narrows the rows and redraws the
    /// picker; a selection past the new end wraps to the head.
    pub(crate) fn with_field(mut delegate: D, field: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        let query = field.read(cx).value().to_string();
        delegate.update_matches(&query);
        let changed = cx.subscribe(&field, |picker: &mut Self, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                picker.requery(&field, cx);
            }
        });
        Picker { delegate, query: Some(PickerQuery::Field(field)), selected: 0, focus: cx.focus_handle(), _query_changed: Some(changed), on_pick: None }
    }

    /// A picker with a query field of its own, drawn inside it, its
    /// placeholder `placeholder` (see [`Self::with_field`]).
    pub(crate) fn with_own_field(delegate: D, placeholder: &'static str, cx: &mut Context<Self>) -> Self {
        Picker { delegate, query: Some(PickerQuery::Unmade(placeholder)), selected: 0, focus: cx.focus_handle(), _query_changed: None, on_pick: None }
    }

    /// Make the picker's own field on its first frame: it takes the
    /// keyboard when the picker holds it.
    fn make_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(PickerQuery::Unmade(placeholder)) = self.query else { return };
        let field = cx.new(|cx| aui::composer::composer_state_rows(placeholder, 1, 1, window, cx));
        let changed = cx.subscribe(&field, |picker: &mut Self, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                picker.requery(&field, cx);
            }
        });
        if self.focus.is_focused(window) {
            window.focus(&field.focus_handle(cx), cx);
        }
        self.query = Some(PickerQuery::Field(field));
        self._query_changed = Some(changed);
    }

    /// Change the delegate with what the owner pushed this frame — the
    /// picker draws right after, in the same frame, so nothing is notified.
    /// The selection stays where it is (a keystroke or the owner moves it).
    pub(crate) fn push_facts(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut D)) {
        change(&mut self.delegate);
        let query = self.query(cx);
        self.delegate.update_matches(&query);
    }

    /// The query as it stands.
    pub(crate) fn query(&self, cx: &App) -> String {
        match &self.query {
            Some(PickerQuery::Field(field)) => field.read(cx).value().to_string(),
            Some(PickerQuery::Typed(query)) => query.clone(),
            Some(PickerQuery::Unmade(_)) | None => String::new(),
        }
    }

    /// The query field, when the picker draws one.
    pub(crate) fn field(&self) -> Option<&Entity<TextareaState>> {
        match &self.query {
            Some(PickerQuery::Field(field)) => Some(field),
            _ => None,
        }
    }

    /// The query field's text changed: narrow to it, the selection kept
    /// inside the rows there are.
    fn requery(&mut self, field: &Entity<TextareaState>, cx: &mut Context<Self>) {
        let query = field.read(cx).value().to_string();
        self.delegate.update_matches(&query);
        self.clamp();
        cx.notify();
    }

    /// A query typed elsewhere changed: narrow to it, selection on the head.
    pub(crate) fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if let Some(PickerQuery::Typed(typed)) = &mut self.query {
            typed.clear();
            typed.push_str(query);
        }
        self.delegate.update_matches(query);
        self.selected = 0;
        cx.notify();
    }


    /// The highlighted row.
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// Put the selection on row `ix`.
    pub(crate) fn select(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.selected != ix {
            self.selected = ix;
            cx.notify();
        }
    }

    /// Move the selection by `delta`, wrapping over the rows there are.
    pub(crate) fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.delegate.match_count();
        let ix = if count == 0 {
            0
        } else {
            let count = count as isize;
            (((self.selected as isize + delta) % count + count) % count) as usize
        };
        self.select(ix, cx);
    }


    /// ↩: confirm the highlighted row.
    pub(crate) fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pick) = self.delegate.confirm(self.selected) {
            self.pick(pick, window, cx);
        }
    }

    /// A click on the row `id`.
    pub(crate) fn confirm_id(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pick) = self.delegate.confirm_id(id) {
            self.pick(pick, window, cx);
        }
    }

    /// Hand the owner `pick`: a row confirmed, or something the picker's
    /// card offers besides its rows (the Projects palette's folder card).
    pub(crate) fn pick(&mut self, pick: D::Pick, window: &mut Window, cx: &mut Context<Self>) {
        match self.on_pick.clone() {
            Some(hook) => hook(&pick, window, cx),
            None => cx.emit(Picked(pick)),
        }
    }

    /// Escape, a click outside: ask the stack to close it.
    pub(crate) fn dismiss(&mut self, cx: &mut Context<Self>) {
        cx.emit(DismissEvent);
    }

    fn clamp(&mut self) {
        if self.selected >= self.delegate.match_count() {
            self.selected = 0;
        }
    }

    /// The handlers a frame hands the component, each running on this picker.
    fn frame(&self, window: &mut Window, cx: &mut Context<Self>) -> PickerFrame {
        let me = cx.entity().downgrade();
        let on = |f: fn(&mut Self, &mut Window, &mut Context<Self>)| -> Handler {
            let me = me.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |picker, cx| f(picker, window, cx));
            })
        };
        let select: IdHandler = {
            let me = me.clone();
            Rc::new(move |id: &SharedString, window: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |picker, cx| picker.confirm_id(id, window, cx));
            })
        };
        let hover: IndexHandler = {
            let me = me.clone();
            Rc::new(move |ix: usize, _: &mut Window, cx: &mut App| {
                let _ = me.update(cx, |picker, cx| picker.select(ix, cx));
            })
        };
        let query = self.field().map(|field| {
            gpui_kit::component::input::Textarea::new(field)
                .appearance(false)
                .bordered(false)
                .text_size(aui_tokens::TextRole::Subtitle.size())
                .h_auto()
                .whitespace_nowrap()
                .into_any_element()
        });
        let _ = window;
        PickerFrame {
            selected: self.selected,
            query,
            field: self.field().cloned(),
            select,
            hover,
            dismiss: on(|picker, _, cx| picker.dismiss(cx)),
            focus: self.focus.clone(),
            on_next: on(|picker, _, cx| picker.move_selection(1, cx)),
            on_prev: on(|picker, _, cx| picker.move_selection(-1, cx)),
            on_confirm: on(Self::confirm),
        }
    }
}

impl<D: PickerDelegate> Focusable for Picker<D> {
    /// The query field when the picker draws one (typing goes there and the
    /// keys bubble to the picker), else the picker itself.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self.field() {
            Some(field) => field.focus_handle(cx),
            None => self.focus.clone(),
        }
    }
}

impl<D: PickerDelegate> Render for Picker<D> {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.make_field(window, cx);
        let rows = (0..self.delegate.match_count()).map(|ix| self.delegate.render_match(ix, ix == self.selected)).collect();
        let frame = self.frame(window, cx);
        self.delegate.render_picker(rows, frame, window, cx)
    }
}
