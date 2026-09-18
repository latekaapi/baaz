//! The sidebar column: what it draws, and the two menus that hang off it.
//!
//! Everything here is composition over state [`Harness`] already holds — the
//! joined session rows, the filter toggles, the open rename, the signed-in
//! identity and the probed tier. It is the shell's left half plus the
//! session-list policy that decides which rows are shown and what the column
//! says when it shows none; it never touches login, the wire, or the session
//! lifecycle, and the only thing it writes is a filter toggle.
//!
//! [`crate::sidebar`] next door is the pure half: rows in, a [`Grouping`] out.
//! This file is the half that needs a `Window`.
//!
//! Two popovers belong to the column rather than to the overlay stack, because
//! each is anchored to the affordance that opens it (the design rule in
//! [`crate::overlays`]): the Sessions caption's view menu, where list
//! management lives, and the footer's account menu, where Sign out lives.
//!
//! [`Grouping`]: aui::nav::Grouping

use std::rc::Rc;

use aui::data::{icon_button, ButtonSize};
use aui::nav::{
    SESSION_DETAIL_GAP, anchored_session_detail_at_sidebar, dense_field, ensure_row_visible, flatten_sidebar,
    group_row, nav_item, rail, row_index_for_session, sidebar_footer, view_menu, virtual_sidebar_view, GroupAction,
    MenuRow, RailItem, RowAction, SidebarRow,
};
use aui::overlay::{anchored_menu, popover_layer, MenuAlign, MenuSide};
use aui_icons::{IconName, Provider};
use aui_motion::pulse_phase;
use aui_tokens::{scale, ActiveAui, AgentState, AuiStyled, Palette};
use gpui::{
    div, prelude::*, px, AnyElement, Bounds, Context, ElementId, Entity, Focusable, ListOffset, Pixels, Render,
    SharedString, WeakEntity, Window,
};
use gpui_kit::base::input::TextareaState;
use gpui_kit::base::v_flex;
use muse_client::schema::AccountStateKind;

use crate::app::{ConfirmRename, Harness, RENAME_CONTEXT};
use crate::login::Auth;
use crate::overlays::MenuKind;
use crate::sidebar::{Grouping, SessionEntry};

/// How many sessions the collapsed rail shows: enough to reach the ones a
/// person switches between, few enough to stay a rail.
const RAIL_SESSIONS: usize = 8;

/// Actions of the Sessions caption's view menu, in row order.
#[derive(Clone, Copy)]
enum ViewAction {
    GroupByProject,
    ToggleEmpty,
    ToggleHidden,
    ClearEmpty,
    ToggleArchived,
    SearchAllProjects,
}

/// The sidebar column as its own view.
///
/// `Harness::render` used to rebuild the sidebar element on every frame —
/// including every wheel notify, whose transcript centre is the only thing
/// that moved. Embedded with gpui's `.cached(size_full)`, a clean pane
/// reuses its retained subtree, so a transcript notify re-renders `Harness`
/// (composition only) and the `SessionView` centre, not the column. The pane
/// reads Baaz live through a weak handle and renders exactly what
/// `Harness::render_sidebar` renders — that method keeps its shape and its
/// listeners, so what the column draws cannot drift. [`SidebarKey`] is what
/// re-arms it: [`Harness::sync_sidebar_pane`] notifies it from `on_frame`
/// whenever its inputs change, and notifies from inside its own subtree
/// (hover, its scroll container, the rename editor, and — since the reveal
/// steers through `ListState` rather than prepaint intents — the reveal's
/// own outside-the-draw pane-notify task in [`Harness::reveal_sidebar_row`])
/// dirty it directly through the view tree.
pub(crate) struct SidebarPane {
    baaz: WeakEntity<Harness>,
}

impl SidebarPane {
    pub(crate) fn new(baaz: WeakEntity<Harness>) -> Self {
        Self { baaz }
    }
}

/// Sidebar-pane renders since process start: the
/// sidebar analogue of `WHEEL_SCROLL_BYS`. Relaxed atomics, drained per
/// `sidebar-wheel:` log line, so a burst's pane/root renders per event are
/// readable off a scripted run.
static SIDEBAR_PANE_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BAAZ_ROOT_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the pane-render count above, for the `sidebar-wheel:` report.
pub(crate) fn take_sidebar_pane_renders() -> u64 {
    SIDEBAR_PANE_RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Pane rebuilds since the last traced root tick:
/// a dedicated counter so the frame trace's per-row drain never steals from
/// the `sidebar-wheel:`/`resize-sweep:` steps' own accumulation, which spans
/// many ticks between their own explicit drains.
static TRACE_PANE_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the trace-only pane count above, for one frame-trace row.
pub(crate) fn take_trace_pane_ticks() -> u64 {
    TRACE_PANE_TICKS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Drain the root-render count below, for the `sidebar-wheel:` report.
pub(crate) fn take_baaz_root_renders() -> u64 {
    BAAZ_ROOT_RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// How long a sidebar wheel gesture stays open after its last event
///: the momentum tail arrives at 60 Hz, so 150 ms
/// covers a missed sample. Deliberately the same 150 ms the transcript
/// keeps; while it is in the future the pane presents every tick and no
/// reveal may move the list.
const SIDEBAR_GESTURE_HORIZON: std::time::Duration = std::time::Duration::from_millis(150);

/// How many consecutive `reveal_sidebar_row` misses an id with no row yet
/// gets before the reveal gives up: a
/// bound, not a real deadline — the row-birth sites (the `turn/started`
/// local-row insert, a `load_sessions` reply) already notify, so a normal
/// wait is one or two attempts; this only guards against an id that never
/// arrives at all (a failed fork, a send that never lands).
pub(crate) const REVEAL_UNKNOWN_FRAMES: u32 = 120;

/// The bookkeeping [`Harness::reveal_sidebar_row`] does for an unknown
/// reveal id, pulled out pure so it is unit-testable without a window
///: a miss for the *same* id the
/// previous call saw extends its streak; a miss for a *different* id (a
/// fresh arm since) starts a new one at 1.
fn next_reveal_unknown_streak(current: Option<(String, u32)>, reveal_id: &str) -> (String, u32) {
    match current {
        Some((id, streak)) if id == reveal_id => (id, streak + 1),
        _ => (reveal_id.to_owned(), 1),
    }
}

/// Sidebar drains actually applied since process start:
/// one per frame that had accumulated travel, against one offset write per
/// event before. Drained per `sidebar-wheel:` line, like `take_wheel_scroll_bys`.
static SIDEBAR_WHEEL_DRAINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the applied-drain count above, for the `sidebar-wheel:` report.
pub(crate) fn take_sidebar_wheel_drains() -> u64 {
    SIDEBAR_WHEEL_DRAINS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The sidebar wheel's input-side state: what the
/// capture handler writes and `render_sidebar` applies. Shared behind
/// `RefCell` (see `Harness::sidebar_wheel`) so the push never borrows the
/// entity — a wheel event can arrive inside a `Harness` update, where an
/// entity update panics.
#[derive(Debug, Default)]
pub(crate) struct SidebarWheelState {
    /// Wheel travel accumulated since the last sidebar frame, in pixels.
    /// The capture handler only adds; the drain takes it whole.
    pending: Pixels,
    /// How long the current gesture stays open after its last event.
    gesture_until: Option<std::time::Instant>,
    /// A wheel landed since the last frame: the armed reveal (if any) is
    /// stale and the scrolled-since-armed bit wants setting.
    scrolled: bool,
}

/// Count one `Harness::render`. Called first in the root render, beside the
/// whole-frame instrument's start.
pub(crate) fn note_baaz_render() {
    BAAZ_ROOT_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl Render for SidebarPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SIDEBAR_PANE_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if crate::session::frame_trace_enabled() {
            TRACE_PANE_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        match self.baaz.upgrade() {
            Some(baaz) => baaz.update(cx, |baaz, cx| baaz.render_sidebar(window, cx)),
            None => div().into_any_element(),
        }
    }
}

/// What the sidebar pane shows, as an equality key.
///
/// Every input `render_sidebar` reads: the cached rows and grouping behind
/// their `Rc` pointers (this comparison calls both getters every frame, so
/// an `invalidate_list` rebuild — or the minute rollover regrouping — shows
/// up as a new pointer here), the selection, the rename, the one-shot
/// reveal, the footer's identity and tier, and the current project. Compared
/// in `on_frame`, before anything draws; a few small clones per frame, no
/// rebuild. Nested entities inside the column (the rename editor, hover and
/// scroll state) notify through the view tree on their own and need no key.
#[derive(PartialEq, Eq)]
pub(crate) struct SidebarKey {
    visible: usize,
    grouping: usize,
    selected: Option<String>,
    renaming: Option<String>,
    reveal: Option<String>,
    auth: (u8, String, String, String, bool),
    tier_args: bool,
    tier: Option<(String, bool, Option<u32>)>,
    current_project: Option<String>,
}

impl SidebarKey {
    /// The key the pane would render this frame.
    pub(crate) fn current(baaz: &Harness, cx: &gpui::App) -> Self {
        let visible = baaz.visible_sessions(cx);
        let grouping = baaz.sidebar_grouping(cx);
        let selected = baaz
            .pending_id
            .clone()
            .or_else(|| baaz.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let auth = match &baaz.auth {
            Auth::Probing => (0, String::new(), String::new(), String::new(), false),
            Auth::SignedOut => (1, String::new(), String::new(), String::new(), false),
            Auth::SignedIn(identity) => (
                2,
                identity.initial(),
                identity.footer_name(),
                identity.email.clone(),
                identity.is_api_key(),
            ),
        };
        let tier = baaz.tier.as_ref().map(|tier| {
            (tier.footer_label(), tier.is_warning(), tier.weekly_fraction().map(f32::to_bits))
        });
        Self {
            visible: Rc::as_ptr(&visible) as usize,
            grouping: Rc::as_ptr(&grouping) as usize,
            selected,
            renaming: baaz.renaming.clone(),
            reveal: baaz.reveal.clone(),
            auth,
            tier_args: baaz.args.tier.is_some(),
            tier,
            current_project: baaz.current_project.clone(),
        }
    }
}

/// The sidebar's empty-state copy, in pure form so it stays honest about
/// loading: while the session list or the index has not landed yet, an
/// empty column means "not here yet", never "nothing exists" — the rows
/// paint the moment either lands.
pub(crate) fn empty_state_text(
    sessions_loaded: bool,
    index_loaded: bool,
    hidden_only: bool,
    empty_only: bool,
) -> (&'static str, &'static str) {
    if !sessions_loaded || !index_loaded {
        return ("Loading sessions…", "Your sessions appear as soon as they arrive.");
    }
    match (hidden_only, empty_only) {
        (true, _) => (
            "Every session here is hidden",
            "Turn on \u{201c}Show hidden\u{201d} in the Sessions menu above.",
        ),
        (false, true) => (
            "Only empty sessions here",
            "Turn on \u{201c}Show empty\u{201d} in the Sessions menu above.",
        ),
        (false, false) => ("No sessions yet", "\u{2318}N starts one."),
    }
}

/// What a regroup or filter change looks like to the virtual list (owner
/// round 6): the grouping mode and the three list-management toggles.
/// [`Harness::sync_sidebar_list`] resets the list state when this changes
/// and splices everything else, so group open/close and fold expand keep
/// the offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SidebarRegroupKey {
    group_by: crate::layout::GroupBy,
    show_hidden: bool,
    show_empty: bool,
    show_archived: bool,
}

/// The hover card's seated anchor for a row: the sidebar pane's right edge
/// plus the card gap, top-aligned with the row — regardless of where in the
/// row the pointer is, so the card never overlaps the sidebar. `None` before
/// the pane's first prepaint lays its edge out. The vertical clamp (a bottom
/// row's card shifting up to stay inside the window) rides in the seat
/// element's own window fit.
pub(crate) fn row_detail_seat(
    sidebar: &Option<Bounds<Pixels>>,
    row: &Bounds<Pixels>,
) -> Option<gpui::Point<Pixels>> {
    let edge = (*sidebar)?;
    Some(gpui::point(edge.origin.x + edge.size.width + px(SESSION_DETAIL_GAP), row.origin.y))
}

impl Harness {
    /// Re-arm the sidebar pane when its inputs changed.
    ///
    /// Called from `on_frame`, before anything draws: while the key matches,
    /// wheel notifies leave the pane clean and gpui reuses its cached
    /// element instead of rebuilding the column. The comparison itself is a
    /// few pointer reads and small clones — no list rebuild.
    pub(crate) fn sync_sidebar_pane(&mut self, cx: &mut Context<Self>) {
        let key = SidebarKey::current(self, cx);
        if self.sidebar_key.as_ref() != Some(&key) {
            self.sidebar_key = Some(key);
            crate::log::boot_mark("pane-notify");
            self.sidebar_pane.update(cx, |_, cx| cx.notify());
        }
    }

    /// The sessions list's current scroll position: what
    /// `sidebar-wheel:` and the resize drag log sample. The list walks down
    /// as `item_ix` grows, with `offset_in_item` the pixels into that row —
    /// the sidebar analogue of `SessionView::bench_list_top`.
    pub(crate) fn sidebar_list_top(&self) -> ListOffset {
        self.sidebar_list.logical_scroll_top()
    }

    /// Whether a sidebar wheel gesture is in flight:
    /// an event landed within the horizon. While this holds the pane
    /// presents every tick and no reveal installs.
    pub(crate) fn sidebar_gesture_active(&self) -> bool {
        self.sidebar_wheel.borrow().gesture_until.is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Push one frame-paced sweep delta into the sidebar's accumulator
    ///: the same three writes
    /// [`sidebar_wheel_capture`]'s real event handler makes — accumulate,
    /// re-arm the gesture horizon, mark scrolled — so a `sidebar-scroll-
    /// sweep:` tick is indistinguishable from a real wheel event to
    /// everything downstream (the drain, the reveal disarm, the trace).
    /// Called once per tick from `on_frame`, never from a dispatched event,
    /// so it never borrows the entity.
    pub(crate) fn push_sidebar_scroll_sweep(&mut self, dy: f32, cx: &mut Context<Self>) {
        {
            let mut state = self.sidebar_wheel.borrow_mut();
            state.pending += px(dy);
            state.gesture_until = Some(std::time::Instant::now() + SIDEBAR_GESTURE_HORIZON);
            state.scrolled = true;
        }
        self.sidebar_pane.update(cx, |_, cx| cx.notify());
    }

    /// Apply the capture handler's input:
    /// take the accumulated travel into exactly one `ListState::scroll_by`
    /// per frame, disarm any armed reveal (the user's scroll wins), and mark
    /// scrolled-since-armed so none reinstalls until the next activation.
    /// Called once per pane render from `render_sidebar`, before the list
    /// lays out — the sidebar twin of `SessionView::drain_pending_wheel`.
    /// Returns whether the user scrolled this frame: a scroll dismisses an
    /// open group menu (the calm option) and is what the `sbwheel` probe
    /// line reports.
    pub(crate) fn drain_sidebar_wheel(&mut self) -> bool {
        let (pending, scrolled) = {
            let mut state = self.sidebar_wheel.borrow_mut();
            (std::mem::replace(&mut state.pending, px(0.0)), std::mem::replace(&mut state.scrolled, false))
        };
        if scrolled {
            self.reveal = None;
            self.reveal_unknown = None;
            self.sidebar_user_scrolled = true;
        }
        if pending == px(0.0) {
            return scrolled;
        }
        // The transcript's sign: a positive wheel delta climbs toward the
        // head, so the pixel walk takes it negated.
        self.sidebar_list.scroll_by(-pending);
        SIDEBAR_WHEEL_DRAINS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        scrolled
    }

    /// The rows above the Sessions caption: New session, Add project, and
    /// Automations behind a Soon tag until it has somewhere to go. No side
    /// inset of its own: the library's gutter positions the nav rows, and
    /// Baaz block inset shifted them 4 px off the session rows'
    /// gutter (measured: nav icon centre 41 vs session dot centre 36).
    fn render_nav_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let new_session = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.new_session(window, cx));
        let add_project =
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.open_projects(false, window, cx));
        let automations = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.overlays.update(cx, |overlays, _| {
                overlays.toast("Automations", "Automations are not wired up yet.");
            });
            cx.notify();
        });
        v_flex()
            .w_full()
            .flex_none()
            .pt(px(scale::SP_2))
            .child(nav_item("nav-new", IconName::Plus, "New session").on_click(new_session))
            .child(nav_item("nav-projects", IconName::Folder, "Add project").on_click(add_project))
            .child(nav_item("nav-automations", IconName::Zap, "Automations").count("Soon").on_click(automations))
            .into_any_element()
    }

    /// The Sessions caption, fixed above the scrolling list: the header stays put with its spacing at any scroll offset,
    /// so the list below it always clips at its own top edge and never butts
    /// against the nav block. The row is the library's caption row with the
    /// same 8 px top margin and 28 px height it had as the scroll content's
    /// first child, so the unscrolled list sits pixel-identical; only the
    /// scrolled states change (the header no longer scrolls away). The
    /// wrapper reports the row's own rect, which is what the Sessions view
    /// menu seats at — under the sliders icon, never following the scroll.
    fn render_sessions_caption(&self, cx: &mut Context<Self>) -> AnyElement {
        // The sliders icon toggles the view menu like every other popover.
        let open_view = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.open_menu(MenuKind::ViewOptions, cx);
        });
        let caption_report = cx.entity().downgrade();
        div()
            .w_full()
            .flex_none()
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = caption_report.update(cx, |this, cx| {
                        Harness::note_trigger_bounds(&mut this.sessions_caption, bounds, cx);
                    });
                }
            })
            .child(group_row("sessions-caption", "Sessions").on_view_options(move |_, w, cx| open_view(&(), w, cx)))
            .into_any_element()
    }

    pub(crate) fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // One travel per frame. The capture
        // handler only accumulates; this drain is the frame's single
        // `scroll_by`, before the list lays out.
        let user_scrolled = self.drain_sidebar_wheel();
        // A list that moved under an open group menu leaves it mis-seated:
        // group menus dismiss on scroll.
        // The header menu seats from the fixed crumb and stays. Mid-render
        // the close needs no notify: the overlay reads it below on this
        // same frame.
        if user_scrolled {
            self.dismiss_group_menu(cx);
            // A list that moved under an open hover card leaves it
            // mis-seated: hover cards dismiss on scroll, like group menus.
            self.close_row_detail();
        }
        // Keep presenting through the tail: a frame every tick while the
        // gesture is open, so the momentum tail is never cut — what the
        // transcript does for its own gesture. A settled sidebar requests
        // nothing and the cached pane is reused untouched.
        if self.sidebar_gesture_active() {
            window.request_animation_frame();
        }
        static FIRST_ROWS_DONE: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let visible = self.visible_sessions(cx);
        if self.sessions_loaded
            && self.index_loaded
            && !visible.is_empty()
            && !FIRST_ROWS_DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            crate::log::boot_mark(&format!("first-rows-painted rows={}", visible.len()));
        }
        let empty = self.render_sidebar_empty(&visible, cx);
        // Both come from the window's cache: built once per change and once
        // per minute, not once per frame (findings `performance-5`,
        // `support-2`). The library takes the grouping behind an `Rc`
        // (finding `performance-13`), so the frame hands the cached one
        // straight over and clones nothing.
        let grouping = self.sidebar_grouping(cx);
        // The flattened rows, re-derived every frame (an index walk, no
        // summaries cloned), with the caller-owned list state synced to
        // their length: `reset` after a regroup or filter change, `splice`
        // after a local insert or remove, `remeasure_items` after a
        // text-only height change.
        let rows = flatten_sidebar(&grouping, false);
        self.sync_sidebar_list(&rows, &grouping);
        // No render-time hover poll: the rows' own hover reports below own
        // the hover state, and a pane render must never disarm it (a live
        // trace showed every poll reading GPUI hover None while the pointer
        // rested on a row, clearing the armed row before the delay elapsed).
        // The click's target first: the row highlights on the click's own
        // frame, before the new view (or any page) exists.
        let selected =
            self.pending_id.clone().or_else(|| self.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            // A sidebar click never arms the reveal: the clicked row is
            // under the cursor, hence painted inside the viewport (owner
            // round 6). It always dismisses the hover card: the click is
            // the row's, not the card's.
            this.close_row_detail();
            this.forced_detail = None;
            this.resume_quiet(id.to_string(), window, cx);
        });
        let act = cx.listener(|this: &mut Self, (id, action): &(SharedString, RowAction), window, cx| {
            this.close_row_detail();
            this.forced_detail = None;
            match action {
                RowAction::Rename => this.start_rename(id.to_string(), window, cx),
                RowAction::Pin => this.toggle_pin(id.to_string(), cx),
                // The tray carries one archive affordance: on a listed session
                // it asks first, on an archived one it puts it straight back.
                RowAction::Archive => {
                    let archived =
                        this.sessions.iter().find(|e| e.id == id.as_ref()).is_some_and(|e| e.archived);
                    if archived {
                        this.unarchive_session(id.to_string(), cx);
                    } else {
                        this.open_archive_dialog(id.to_string(), cx);
                    }
                }
                _ => {}
            }
        });
        let toggle = cx.listener(|this: &mut Self, id: &SharedString, _, cx| {
            this.toggle_group(id.to_string(), cx);
        });
        // A session row's hover report: the hover card's arm and seat, and
        // the only thing that arms it. The rows fire this from their own
        // hover events carrying the row's own bounds, so the delay arms —
        // and the seat lands — even when the sidebar pane renders nothing (a
        // settled sidebar re-renders on no pointer movement at all): the
        // timer's notify reaches only the root, whose cached pane never
        // re-renders. Pane renders never touch the hover state: only this
        // report, a wheel or scroll, or a click may arm or close the card.
        let hover = cx.listener(
            |this: &mut Self, (id, hovered, row): &(SharedString, bool, Bounds<Pixels>), _, cx| {
                crate::hover_trace!(
                    "report id={id} {} row={:.0},{:.0}",
                    if *hovered { "entered" } else { "left" },
                    f32::from(row.origin.x),
                    f32::from(row.origin.y)
                );
                this.note_row_hover(id.to_string(), *hovered, Some(*row), cx);
            },
        );
        // A group row's hover tray: `+` starts a session in that project
        // (and makes it current), `…` opens its project menu. On "Other
        // workspaces" `+` has nowhere to start, so it offers adoption, and
        // `…` carries the one row that does the same.
        let group = cx.listener(
            |this: &mut Self, (id, action): &(SharedString, GroupAction), window, cx| match action {
                GroupAction::New => {
                    if id.as_ref() == crate::sidebar::OTHER_GROUP {
                        this.open_projects(false, window, cx);
                    } else {
                        this.new_session_in(Some(id.to_string()), window, cx);
                    }
                }
                // The folded group's "Show N more" / "Show less" row flips
                // the group's id in `expanded_groups` and regroups.
                GroupAction::ToggleMore => {
                    this.toggle_expanded(id.to_string(), cx);
                }
                GroupAction::Menu => {
                    if id.as_ref() == crate::sidebar::OTHER_GROUP {
                        this.open_project_menu(None, false, cx);
                    } else {
                        this.open_project_menu(Some(id.to_string()), false, cx);
                    }
                }
            },
        );
        // Every rendered group row's tray `…` button reports its window bounds
        // once per frame: a group row's project menu seats at its own `…`,
        // never under the header. New group ids notify once so a menu opened
        // before the first prepaint appears on the next frame; steady bounds
        // never schedule work of their own.
        let menu_report = cx.entity().downgrade();
        // No caption on the view: the Sessions header is fixed above the
        // list (see `render_sessions_caption`), so it never scrolls away —
        // the flattened rows start at the first group. Row bounds follow
        // scrolled rows through the per-frame menu intents below, which is
        // what seats a group menu at its own `…` at any offset.
        // The selected row's bounds, once per frame: what the scripted
        // hover card seats at. Stored without notifying (the overlay reads
        // it below on this same frame); only a newly selected id wakes the
        // pane, for the capture step waiting on it.
        let selected_report = cx.entity().downgrade();
        let mut view = virtual_sidebar_view(Self::SIDEBAR_VIEW_ID, Rc::clone(&grouping), self.sidebar_list.clone())
            .row_actions(vec![RowAction::Pin, RowAction::Rename, RowAction::Archive])
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_toggle(move |id, w, cx| toggle(id, w, cx))
            .on_group_action(move |id, action, w, cx| group(&(id.clone(), action), w, cx))
            .on_group_menu_prepainted(move |id, bounds, _, cx| {
                let id = id.to_string();
                let _ = menu_report.update(cx, |this, cx| {
                    let fresh = !this.group_menu_bounds.contains_key(&id);
                    this.group_menu_bounds.insert(id, bounds);
                    if fresh {
                        cx.notify();
                    }
                });
            })
            .on_selected_prepainted(move |id, bounds, _, cx| {
                let id = id.to_string();
                let _ = selected_report.update(cx, |this, cx| {
                    let fresh = this.selected_row_bounds.as_ref().is_none_or(|(known, _)| *known != id);
                    this.selected_row_bounds = Some((id, bounds));
                    if fresh {
                        cx.notify();
                    }
                });
            })
            .on_action(move |id, action, w, cx| act(&(id.clone(), action), w, cx))
            .on_row_hover_bounds(move |id, hovered, row, w, cx| hover(&(id.clone(), hovered, row), w, cx));
        if let Some(renaming) = self.renaming.clone() {
            // The library builds the renaming row more than once per frame,
            // so the editor arrives as a builder it calls on every build —
            // never as one element the first build would claim. Everything
            // the builder needs is resolved here, once per pane render; the
            // per-build closure only builds.
            let rename = self.rename.clone();
            let palette = cx.aui().colors;
            let focused = rename.focus_handle(cx).is_focused(window);
            let baaz = cx.entity().downgrade();
            view = view.editing(renaming, move |_, _| {
                Self::rename_editor(&rename, palette, focused, baaz.clone())
            });
        }
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        // The running dots sample one phase per frame and request no frames
        // of their own: the pulse loop's ticks (see `ensure_pulse_task`) are
        // what re-render the pane while a dot is on screen. Unset under
        // reduced motion, where the dots rest as plain dots.
        if let Some(phase) = self.pulse_phase_value(cx) {
            view = view.pulse_phase(phase);
        }
        // The one-shot reveal (armed only by outside-the-sidebar
        // activations, steering the virtual list to the row with the least
        // move). Never from a
        // sidebar click, a list refresh, a regroup, during a wheel gesture,
        // or after the user has scrolled — the flag only exists between an
        // outside activation and the row reporting visible, and a wheel
        // disarms it outright. A resize drag owns the list the same way:
        // nothing steers while one is in flight.
        if !self.sidebar_user_scrolled && !self.sidebar_gesture_active() && !self.resize.active {
            if let Some(reveal_id) = self.reveal.clone() {
                self.reveal_sidebar_row(&rows, &grouping, &reveal_id, cx);
            }
        }
        // No quick-filter field: ⌘⇧F and the sidebar search icon open the
        // full-text search palette instead, so the two can never share the
        // sidebar.
        // The pane's own rect, once per frame: the hover card's side seat
        // hangs off its right edge. The wrapper is a plain full-size column
        // around the column the pane already drew, so the geometry is
        // unchanged; stored without notifying past the first prepaint (the
        // overlay reads it on this same frame).
        let pane_report = cx.entity().downgrade();
        let column = v_flex()
            .size_full()
            .child(self.render_nav_block(cx))
            .child(self.render_sessions_caption(cx))
            // The list's positioned wrapper: `relative` only establishes the containing block — the
            // capture canvas below resolves against this instead of the
            // window (the same load-bearing `relative` the transcript
            // wrapper wears for its own `wheel_capture`). A flex column
            // otherwise, so the virtual list's `flex_1` fills exactly the
            // rect the scroll div filled before: the fixed caption above is
            // outside it, so the list clips at its own top edge and nothing
            // from it ever reaches the nav rows.
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .relative()
                    .flex()
                    .flex_col()
                    .child(sidebar_wheel_capture(self))
                    .child(view)
                    .children(empty),
            )
            .child(self.render_footer(cx));
        div()
            .size_full()
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = pane_report.update(cx, |this, cx| {
                        Harness::note_trigger_bounds(&mut this.sidebar_bounds, bounds, cx);
                    });
                }
            })
            .child(column)
            .into_any_element()
    }

    /// Option B's hover detail, at the window root: the full picture for
    /// the hovered row (past its delay) or the scripted row
    /// (`row-detail:<id>`), through the library's
    /// `anchored_session_detail_at_sidebar` — the card's left edge at the
    /// sidebar pane's right edge plus the card gap, top-aligned with the
    /// row, sliding up near the window bottom, in `popover_layer` so it
    /// escapes the sidebar's clipping and paints above everything.
    ///
    /// The card never takes focus (no focus handle, no key context — the
    /// library builds none) and never covers the sidebar (it hangs beside
    /// it), so the row's click still lands; a click that does land on the
    /// card only dismisses it. It closes on leave, scroll and click through
    /// [`Self::close_row_detail`] and the listeners that call it.
    pub(crate) fn render_row_detail(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (element, reason) = self.build_row_detail(cx);
        let shown = element.is_some();
        // Diagnosis only (`BAAZ_HOVER_TRACE=1`): one line per result
        // change, never per frame — the root calls this every frame.
        let changed = self.hover_trace_last_shown != Some(shown);
        self.hover_trace_last_shown = Some(shown);
        if changed {
            crate::hover_trace!(
                "render result={} reason={reason} id={:?} hovered={:?} shown={} trigger={}",
                if shown { "Some" } else { "None" },
                self.forced_detail.clone().or_else(|| self.hovered_row.clone()),
                self.hovered_row,
                self.hover_shown,
                self.hover_trigger.is_some()
            );
        }
        element
    }

    /// The card builder behind [`Self::render_row_detail`], with the reason
    /// a missing card is missing: `not-shown` (still inside the delay),
    /// `no-hovered-row`, `no-trigger`, `no-sidebar-edge`, or the forced
    /// pin's own misses and unknown rows under `unknown-row` / `forced-*`.
    fn build_row_detail(&mut self, cx: &mut Context<Self>) -> (Option<AnyElement>, &'static str) {
        let (id, row) = match self.forced_detail.clone() {
            Some(id) => {
                let Some((known, bounds)) = self.selected_row_bounds.clone() else {
                    return (None, "forced-no-bounds");
                };
                if known != id {
                    return (None, "forced-id-mismatch");
                }
                (id, bounds)
            }
            None => {
                if !self.hover_shown {
                    return (None, "not-shown");
                }
                let Some(hovered) = self.hovered_row.clone() else {
                    return (None, "no-hovered-row");
                };
                let Some(row) = self.hover_trigger else {
                    return (None, "no-trigger");
                };
                (hovered, row)
            }
        };
        // The seat's other half: the laid-out pane's right edge. Before the
        // first prepaint there is no edge, so there is no card this frame.
        let Some(edge) = self.sidebar_bounds else {
            return (None, "no-sidebar-edge");
        };
        let sidebar_right = edge.origin.x + edge.size.width;
        let visible = self.visible_sessions(cx);
        // The visible rows first, then the whole list: a card pinned to a
        // filtered-out row still names it honestly.
        let Some(entry) = visible
            .iter()
            .find(|entry| entry.id == id)
            .or_else(|| self.sessions.iter().find(|entry| entry.id == id))
        else {
            return (None, "unknown-row");
        };
        let now = crate::sidebar::grouping_now(&visible);
        // The open session's own fold is fresher than the joined row: no
        // wire event will ever sync it on a `--replay` window, and the
        // pending words live only there. A presentation-local overlay, never
        // a write to the list — the wire sync owns the entries.
        let mut entry = entry.clone();
        if let Some(view) = self.active.clone() {
            let view = view.read(cx);
            if view.session_id == id {
                let (approval, question) = view.row_pending();
                if approval.is_some() {
                    entry.approval_command = approval;
                }
                if question.is_some() {
                    entry.pending_question = question;
                }
                if entry.last_ask.as_deref().map(str::trim).is_none_or(|s| s.is_empty()) {
                    entry.last_ask = view.last_user_text();
                }
                if entry.description.trim().is_empty() {
                    if let Some(summary) = view.last_summary_text() {
                        entry.description = summary;
                    }
                }
            }
        }
        let data = entry.detail_data(now);
        let card_id: ElementId = (ElementId::from("row-detail"), SharedString::from(id.clone())).into();
        let mut card = aui::nav::session_detail(card_id);
        if let Some(title) = data.title {
            card = card.title(title);
        }
        if let Some(ask) = data.ask {
            card = card.ask(ask);
        }
        if let Some(reply) = data.reply {
            card = card.reply(reply);
        }
        if let Some(status) = data.status {
            card = card.status(status.kind, status.detail);
        }
        if let Some(project) = data.project {
            card = card.project(project);
        }
        if let Some(branch) = data.branch {
            card = card.branch(branch);
        }
        if let Some(turns) = data.turns {
            card = card.turns(turns);
        }
        if let Some(updated) = data.updated {
            card = card.updated(updated);
        }
        if let Some(workspace) = data.workspace {
            card = card.workspace(workspace);
        }
        if let Some(question) = data.pending_question {
            card = card.pending_question(question);
        }
        if let Some(approval) = data.pending_approval {
            card = card.pending_approval(approval);
        }
        // `anchored_session_detail_at_sidebar` takes the card itself, so no
        // wrapper rides along: the card carries no click handler of its own,
        // and a click that lands on it is a no-op rather than a row action.
        // That corner is nearly unreachable — reaching the card means leaving
        // the row, and leaving closes it — while every row click still
        // lands on its row (the card hangs beside the sidebar, never over
        // it) and dismisses through the select/action listeners; the row's
        // own leave report and scroll close it.
        (
            Some(anchored_session_detail_at_sidebar(row, sidebar_right, card).into_any_element()),
            "shown",
        )
    }

    /// What a regroup or filter change looks like to the virtual list
    ///: the mode and the three list-management toggles.
    /// Group open/close and fold expand are NOT in the key — they splice
    /// like any other local insert or remove, so the offset survives them.
    /// Compared in `render_sidebar` before anything draws; a change resets
    /// the list state to the flattened length.
    pub(crate) fn regroup_key(&self) -> SidebarRegroupKey {
        SidebarRegroupKey {
            group_by: self.effective_group_by(),
            show_hidden: self.show_hidden,
            show_empty: self.show_empty,
            show_archived: self.show_archived,
        }
    }

    /// Keep the caller-owned list state on the flattened rows:
    /// re-flattened per frame by the caller; `reset` after a regroup or
    /// filter change (the one sync that
    /// drops the offset — the old offset is meaningless against a rebuilt
    /// model), `splice` after a local insert or remove (open/close, fold
    /// expand, sessions arriving or leaving), `remeasure_items` after a
    /// text-only height change (same rows, new grouping pointer). A
    /// mismatch never panics in the list — it paints blanks past the model
    /// — so the debug assertion below is the contract, not a guardrail.
    pub(crate) fn sync_sidebar_list(&mut self, rows: &[SidebarRow], grouping: &Rc<Grouping>) {
        let key = self.regroup_key();
        if self.prev_sidebar_regroup.as_ref() != Some(&key) {
            self.sidebar_list.reset(rows.len());
            self.prev_sidebar_regroup = Some(key);
        } else if rows != self.prev_sidebar_rows.as_slice() {
            // The common prefix and suffix stay; the middle was swapped.
            // Moves degrade to remove-plus-insert, which still lands the
            // count exactly — only the preserved offset approximates.
            let prev = &self.prev_sidebar_rows;
            let mut prefix = 0usize;
            while prefix < prev.len() && prefix < rows.len() && prev[prefix] == rows[prefix] {
                prefix += 1;
            }
            let mut suffix = 0usize;
            while suffix < prev.len() - prefix
                && suffix < rows.len() - prefix
                && prev[prev.len() - 1 - suffix] == rows[rows.len() - 1 - suffix]
            {
                suffix += 1;
            }
            self.sidebar_list
                .splice(prefix..prev.len() - suffix, rows.len() - prefix - suffix);
        } else if self
            .prev_sidebar_grouping
            .as_ref()
            .is_none_or(|cached| !Rc::ptr_eq(cached, grouping))
        {
            // Same rows, rebuilt grouping: text moved under stable rows
            // (a rename, a new left line, a branch appearing), so cached
            // heights may lie. Remeasure the window the list asks for.
            if !rows.is_empty() {
                self.sidebar_list.remeasure_items(0..rows.len());
            }
        }
        self.prev_sidebar_rows = rows.to_vec();
        self.prev_sidebar_grouping = Some(Rc::clone(grouping));
        debug_assert_eq!(
            self.sidebar_list.item_count(),
            rows.len(),
            "sidebar list out of sync: {} items for {} rows",
            self.sidebar_list.item_count(),
            rows.len()
        );
    }

    /// The id `render_sidebar` hands the virtual list.
    pub(crate) const SIDEBAR_VIEW_ID: &'static str = "sessions";

    /// Fold one row hover report into the hover state: entering a new row
    /// arms the detail's delay and seats it from the row's own bounds —
    /// never the pointer — leaving the armed row closes it, and anything
    /// else changes nothing. This is the only writer: pane renders never
    /// touch the hover state, so a render can neither arm nor disarm a row.
    /// Fires from the rows' own hover events, so this runs even when the
    /// sidebar pane itself renders nothing — the timer's notify reaches only
    /// the root, whose cached pane never re-renders, which is why the row
    /// bounds land here. Entering never notifies (nothing shows until the
    /// delay elapses); the timer notifies when the card opens, and leaving
    /// notifies when an open card closes.
    pub(crate) fn note_row_hover(
        &mut self,
        id: String,
        hovered: bool,
        row: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if self.hovered_row.as_deref() == Some(id.as_str()) {
                crate::hover_trace!("note id={id} entered already-armed");
                return;
            }
            let Some(row) = row else {
                crate::hover_trace!("note id={id} entered no-bounds");
                return;
            };
            // The seated anchor the card opens at: the sidebar's right edge
            // plus the card gap, top-aligned with the row.
            match row_detail_seat(&self.sidebar_bounds, &row) {
                Some(seat) => crate::hover_trace!(
                    "note id={id} entered arm seat={:.0},{:.0}",
                    f32::from(seat.x),
                    f32::from(seat.y)
                ),
                None => crate::hover_trace!(
                    "note id={id} entered arm no-sidebar-edge row={:.0},{:.0}",
                    f32::from(row.origin.x),
                    f32::from(row.origin.y)
                ),
            }
            self.hovered_row = Some(id.clone());
            self.hover_since = Some(std::time::Instant::now());
            self.hover_shown = false;
            self.hover_trigger = Some(row);
            let pending = cx.entity().downgrade();
            self.tasks.push(cx.spawn(async move |_, cx| {
                cx.background_executor().timer(aui::nav::SESSION_DETAIL_DELAY).await;
                let _ = pending.update(cx, |this, cx| {
                    let matches = this.hovered_row.as_deref() == Some(&id);
                    if matches && !this.hover_shown {
                        this.hover_shown = true;
                        cx.notify();
                    }
                    crate::hover_trace!(
                        "timer src=note id={id} matches={matches} shown={}",
                        this.hover_shown
                    );
                });
            }));
            return;
        }
        if self.hovered_row.as_deref() == Some(id.as_str()) {
            crate::hover_trace!("note id={id} left close shown={}", self.hover_shown);
            self.close_row_detail();
            cx.notify();
        } else {
            crate::hover_trace!("note id={id} left ignored armed={:?}", self.hovered_row);
        }
    }

    /// Close the hover card: pointer left, wheel moved, row clicked, card
    /// clicked. The scripted pin ([`Harness::forced_detail`]) is not hover
    /// state and survives this; clicks clear it where they land.
    pub(crate) fn close_row_detail(&mut self) {
        self.hovered_row = None;
        self.hover_since = None;
        self.hover_shown = false;
        self.hover_trigger = None;
    }

    /// Steer the virtual list to the outside-activated session, moving the
    /// least distance that shows its row whole: the row
    /// itself when the flattened model has one — folds rescue held-back
    /// rows for the pending target, so no expansion dance — else its group
    /// head for a closed group (never auto-expanded, like before), else
    /// wait for an id with no row yet (a draft before its first send, a
    /// fork before `load_sessions`'s reply lands) up to
    /// [`REVEAL_UNKNOWN_FRAMES`] attempts, else clear a flag with nowhere
    /// to go (a *known* session that still has no landing row gives up
    /// on the first miss, same as before — only an unrecognised id gets
    /// the grace period). A sidebar
    /// click never sets the flag; wheel and resize disarm it. Called on
    /// selection change and the frames after, until the row reports
    /// visible: before the first layout the viewport is unknown and the
    /// steer is a no-op, and a fully visible row moves nothing.
    fn reveal_sidebar_row(
        &mut self,
        rows: &[SidebarRow],
        grouping: &Grouping,
        reveal_id: &str,
        cx: &mut Context<Self>,
    ) {
        let wanted = SharedString::from(reveal_id);
        let target = row_index_for_session(rows, grouping, &wanted)
            .or_else(|| self.reveal_head_row(rows, grouping, reveal_id));
        let Some(ix) = target else {
            if self.sessions.iter().any(|e| e.id == reveal_id) {
                // A known session with nowhere to go: a
                // waiting flag outlives its activation and can move a list
                // the user scrolled meanwhile.
                self.reveal = None;
                self.reveal_unknown = None;
                return;
            }
            // An id this window has never listed: most likely its row is
            // simply not born yet. The sites that create it (the
            // `turn/started` local-row insert, a `load_sessions` reply)
            // already `invalidate_list`/notify, which is what brings this
            // function around again with fresh rows — so waiting here
            // costs nothing extra, just a bound so a reveal for an id that
            // never arrives (a failed fork, a send that never lands)
            // cannot spin forever.
            let (id, streak) = next_reveal_unknown_streak(self.reveal_unknown.take(), reveal_id);
            self.reveal_unknown = Some((id, streak));
            if streak > REVEAL_UNKNOWN_FRAMES {
                self.reveal = None;
                self.reveal_unknown = None;
            }
            return;
        };
        self.reveal_unknown = None;
        // A steer that moves the list under an open group menu mis-seats
        // it: dismiss first, like a user scroll does.
        let steers = self.sidebar_list.item_is_above_viewport(ix) == Some(true)
            || self.sidebar_list.item_is_below_viewport(ix) == Some(true);
        if steers {
            self.dismiss_group_menu(cx);
        }
        ensure_row_visible(&self.sidebar_list, ix);
        let above = self.sidebar_list.item_is_above_viewport(ix);
        let below = self.sidebar_list.item_is_below_viewport(ix);
        if above == Some(false) && below == Some(false) {
            self.reveal = None;
        } else if above.is_none() || below.is_none() {
            // The list has not laid out yet: a degenerate zero-height sidebar rect would otherwise
            // have every pane render spawn another pane-notify task, which
            // renders, which pokes again, forever — `bench-idle` never
            // settling. Leave the flag armed with nothing scheduled;
            // whatever eventually lays the list out (a resize, a normal
            // render) brings this function around again for a fresh read.
        } else {
            // The steer above only stages the list's pending scroll: no
            // frame applies it on its own (a cached pane runs none), and a
            // notify from inside the draw schedules nothing — so wake the
            // pane from a task outside the draw, the old reveal's poke in
            // new clothes. The poke stops the frame the row reports visible
            // (the flag consumes above), so a settled list never spins.
            let pane = self.sidebar_pane.clone();
            self.tasks.push(cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |_, cx| {
                    pane.update(cx, |_, cx| cx.notify());
                });
            }));
        }
    }

    /// The group head (or header) row for a session with no session row: a
    /// closed group holds its sessions out of the flattened model. Mirrors
    /// the old current-group fallback — reveal the head, never auto-expand.
    fn reveal_head_row(&self, rows: &[SidebarRow], grouping: &Grouping, reveal_id: &str) -> Option<usize> {
        match grouping {
            Grouping::Project(groups) => {
                let entry = self.sessions.iter().find(|e| e.id == reveal_id)?;
                let project = entry.project.as_deref()?;
                let group = groups.iter().position(|g| g.id.as_ref() == project)?;
                rows.iter().position(|row| matches!(row, SidebarRow::ProjectHead { group: g } if *g == group))
            }
            Grouping::Date(groups) => {
                let bucket = groups
                    .iter()
                    .position(|group| group.sessions.iter().any(|s| s.id.as_ref() == reveal_id))?;
                let pinned = groups[bucket].sessions.iter().all(|s| s.pinned);
                rows.iter().position(|row| match row {
                    SidebarRow::DateHeader { group: g, .. } if *g == bucket => true,
                    SidebarRow::PinnedHeader if pinned => true,
                    _ => false,
                })
            }
            Grouping::Status(groups) => {
                let group = groups
                    .iter()
                    .position(|group| group.sessions.iter().any(|s| s.id.as_ref() == reveal_id))?;
                rows.iter().position(|row| matches!(row, SidebarRow::StatusHeader { group: g } if *g == group))
            }
        }
    }

    /// Dismiss an open group-row project menu, if any. The header menu
    /// seats from the fixed crumb and is never dismissed by list movement.
    fn dismiss_group_menu(&mut self, cx: &mut Context<Self>) {
        if self
            .overlays
            .read(cx)
            .menu
            .as_ref()
            .is_some_and(|menu| menu.kind == MenuKind::Project && !menu.project_header)
        {
            self.overlays.update(cx, |overlays, _| overlays.menu = None);
        }
    }

    /// The field the row being renamed holds: the library's dense recipe —
    /// a borderless, chromeless single line at the row-title size, with the
    /// 1 px focus border on the wrapper instead of the component. The wrapper
    /// is a flex row centring its child, fixed to the 22 px box the module
    /// docs in `aui::nav::session_row` prescribe (20 px of field in 4 px of
    /// row padding is exactly the 30 px row): the editor's own line box plus
    /// its internal padding used to stand taller, so the row grew and the
    /// list below jumped. Clipping is horizontal only, so a long name scrolls
    /// under the caret instead of spilling a second line, and the row keeps
    /// its own height while a rename is open, so siblings never move. The
    /// commit path is unchanged. The header title takes one element built
    /// here; the sidebar row takes a builder (see the `editing` call in
    /// `render_sidebar`) because the library builds that row more than once
    /// per frame.
    pub(crate) fn rename_field(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        Self::rename_editor(
            &self.rename,
            cx.aui().colors,
            self.rename.focus_handle(cx).is_focused(window),
            cx.entity().downgrade(),
        )
    }

    /// One rename editor, built from its pieces with no view context: the
    /// per-build half of [`Self::rename_field`]. Entity-agnostic on purpose —
    /// the virtual list calls the row's builder with `(&mut Window, &mut
    /// App)` inside layout, where there is no `Context` to bind a listener
    /// with, so the confirm action upgrades the weak handle instead. Builds
    /// only, never notifies: a builder runs inside layout.
    fn rename_editor(
        rename: &Entity<TextareaState>,
        p: Palette,
        focused: bool,
        baaz: WeakEntity<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(move |_: &ConfirmRename, window, cx| {
                let _ = baaz.update(cx, |this, cx| this.commit_rename(window, cx));
            })
            .child(
                div()
                    .w_full()
                    .h(px(22.0))
                    .flex()
                    .items_center()
                    .overflow_x_hidden()
                    .py(px(0.0))
                    .px(px(6.0))
                    .rounded(px(scale::R_SM))
                    .border_1()
                    .border_color(if focused { p.accent } else { p.line })
                    .bg(p.surface_1)
                    // The editor keeps its natural line box (`h_auto`): the
                    // library's 20 px recipe stands its glyphs on the bottom
                    // edge of a 22 px wrapper and clips their descenders. The
                    // wrapper is the fixed box that keeps the row's height;
                    // the editor centres in it and may overhang the border by
                    // a pixel or two of line box, which draws nothing.
                    .child(dense_field(rename).h_auto().whitespace_nowrap().overflow_x_hidden()),
            )
            .into_any_element()
    }

    /// One tick of the pulse loop: 20 Hz, so a running dot's ring advances in
    /// 50 ms steps — smooth to the eye over its 2 s cycle, at a sixth of the
    /// display rate it used to cost as a looping animation.
    const PULSE_TICK: std::time::Duration = std::time::Duration::from_millis(50);

    /// The sampled pulse phase for this pane render, or `None` under reduced
    /// motion (the dots' own resting state then shows the plain dot, and no
    /// timer runs). Sampled once per render and shared by every dot, so the
    /// frame agrees with itself.
    pub(crate) fn pulse_phase_value(&self, cx: &gpui::App) -> Option<f32> {
        if cx.reduce_motion() {
            return None;
        }
        Some(pulse_phase(self.pulse_epoch, cx.background_executor().now()))
    }

    /// Whether a sampled pulse dot is on screen: some session is running and
    /// its row, its project's rolled-up head, or its rail cell is visible.
    /// A row counts as visible while the list has no viewport yet (pre-layout
    /// reports `None`): tick until the first layout says otherwise.
    pub(crate) fn pulse_needed(&self, cx: &gpui::App) -> bool {
        if cx.reduce_motion() {
            return false;
        }
        let running: Vec<String> =
            self.sessions.iter().filter(|e| e.running).map(|e| e.id.clone()).collect();
        if running.is_empty() {
            return false;
        }
        let grouping = self.sidebar_grouping(cx);
        let rows = flatten_sidebar(&grouping, false);
        let visible = |ix: usize| {
            !matches!(self.sidebar_list.item_is_above_viewport(ix), Some(true))
                && !matches!(self.sidebar_list.item_is_below_viewport(ix), Some(true))
        };
        for id in &running {
            let id: SharedString = id.clone().into();
            if let Some(ix) = row_index_for_session(&rows, &grouping, &id) {
                if visible(ix) {
                    return true;
                }
            }
        }
        // The rolled-up head dot of a project with a running session.
        if let Grouping::Project(groups) = &*grouping {
            for (ix, row) in rows.iter().enumerate() {
                if let SidebarRow::ProjectHead { group } = row {
                    if groups.get(*group).is_some_and(|g| g.state == Some(AgentState::Running))
                        && visible(ix)
                    {
                        return true;
                    }
                }
            }
        }
        // The rail's cells are on screen whenever the rail renders: mirror
        // its shown set (pinned first, then newest, plus the open session).
        let visible = self.visible_sessions(cx);
        let mut ordered: Vec<&SessionEntry> = visible.iter().filter(|e| e.pinned).collect();
        ordered.extend(visible.iter().filter(|e| !e.pinned));
        let mut shown: Vec<&SessionEntry> = ordered.into_iter().take(RAIL_SESSIONS).collect();
        if let Some(open) = self.active_id(cx) {
            if !shown.iter().any(|e| e.id == open) {
                if let Some(entry) = visible.iter().find(|e| e.id == open) {
                    shown.insert(0, entry);
                }
            }
        }
        shown.iter().any(|e| e.running)
    }

    /// Starts the pulse loop while a sampled dot is on screen. The loop
    /// notifies the sidebar pane once per [`Self::PULSE_TICK`] and ends
    /// itself the first tick nothing needs it, clearing the handle so a
    /// later need restarts it. Called from `on_frame`: one `is_some` while
    /// running, one scan while nobody runs.
    pub(crate) fn ensure_pulse_task(&mut self, cx: &mut Context<Self>) {
        if self.pulse_task.is_some() {
            return;
        }
        if !self.pulse_needed(cx) {
            return;
        }
        self.pulse_task = Some(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Self::PULSE_TICK).await;
                let ticking = this
                    .update(cx, |this, cx| {
                        if this.pulse_needed(cx) {
                            this.sidebar_pane.update(cx, |_, cx| cx.notify());
                            true
                        } else {
                            false
                        }
                    })
                    .unwrap_or(false);
                if !ticking {
                    break;
                }
            }
            let _ = this.update(cx, |this, _| {
                this.pulse_task = None;
            });
        }));
    }

    /// What the list says when it has nothing to show, and why (spec §5).
    fn render_sidebar_empty(&self, visible: &[SessionEntry], cx: &mut Context<Self>) -> Option<AnyElement> {
        if !visible.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        // What each filter alone is keeping out, past the other one: the
        // empty text names its own toggle rather than borrowing hidden's.
        // (There is no sidebar text filter — search lives in the palette —
        // so no "no match" state exists here.)
        let hidden_only = !self.show_hidden && self.sessions.iter().any(|e| e.hidden);
        let empty_only = !self.show_empty
            && self.sessions.iter().any(|e| (self.show_hidden || !e.hidden) && e.is_empty());
        let (title, detail) = empty_state_text(self.sessions_loaded, self.index_loaded, hidden_only, empty_only);
        Some(
            v_flex()
                .w_full()
                .px(px(scale::SP_5))
                .py(px(scale::SP_6))
                .gap(px(scale::SP_2))
                .child(div().ui(scale::FS_12).medium().text_color(p.ink_2).child(title))
                .child(div().ui(scale::FS_11).text_color(p.ink_4).child(detail))
                .into_any_element(),
        )
    }

    /// "Signed in as", in the library's shape: avatar, name, the email it is
    /// really reporting, the plan row, and the provider usage meter with the
    /// chevron. The whole footer opens the account menu — Sign out lives
    /// there now, and the list-management toggles live in the Sessions menu.
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Auth::SignedIn(identity) = &self.auth else {
            return div().into_any_element();
        };
        let account = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_menu(MenuKind::Account, cx);
        });
        let mut footer = sidebar_footer("account", identity.initial(), identity.footer_name())
            .on_click(move |e, w, cx| account(e, w, cx));
        if !identity.email.is_empty() {
            footer = footer.detail(identity.email.clone());
        }
        // The third row: what this login is entitled to. Warning-tinted for
        // anything that is not a plan in force, because that is the case where
        // the next turn costs money nobody expected. The key lanes say so
        // without a probe: a stored key or `META_API_KEY` is pay-as-you-go by
        // construction.
        let meter = self.tier.as_ref().and_then(|tier| tier.weekly_fraction());
        // `--tier` fakes the probe it names: the footer reads the faked tier
        // like any other probe answer, even on the key lanes.
        if self.args.tier.is_some() {
            if let Some(tier) = &self.tier {
                footer = footer.plan(tier.footer_label(), tier.is_warning());
            }
        } else if identity.is_api_key() {
            footer = footer.plan("Pay-as-you-go · API key", true);
        } else if let Some(tier) = &self.tier {
            footer = footer.plan(tier.footer_label(), tier.is_warning());
        }
        // The meter is the weekly fraction the probe already reports; with no
        // reading there is no meter. Either way the chevron stands, so the
        // account menu stays discoverable — the row's own click opens it too.
        if let Some(fraction) = meter {
            footer = footer.meter(Provider::Muse, fraction);
        } else {
            footer = footer.trailing(
                icon_button("account-chevron", IconName::ChevronDown)
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon_size(px(12.0)),
            );
        }
        // The wrapper reports the footer row's own rect (its only child),
        // which is what the account menu seats at — above the footer's top
        // edge, right edges aligned, at any sidebar width.
        let footer_report = cx.entity().downgrade();
        div()
            .w_full()
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = footer_report.update(cx, |this, cx| {
                        Harness::note_trigger_bounds(&mut this.footer_bounds, bounds, cx);
                    });
                }
            })
            .child(footer)
            .into_any_element()
    }

    /// The collapsed rail: new-session and search cells, a separator, then
    /// the sessions a person reaches for from a rail — the open one and the
    /// most recent of the visible list, each a tile bearing its initial with
    /// the title as its tooltip, running ones pulsing — and the account
    /// avatar. A rail of two glyphs and an empty column served nothing.
    pub(crate) fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_id(cx);
        let mut items = vec![
            RailItem::nav("new", IconName::Plus),
            RailItem::nav("search", IconName::Search),
            RailItem::nav("projects", IconName::Folder),
            RailItem::separator(),
        ];
        // The visible list is already newest-first; pinned rows lead it so
        // the rail keeps what the person keeps.
        let visible = self.visible_sessions(cx);
        let mut ordered: Vec<&SessionEntry> = visible.iter().filter(|e| e.pinned).collect();
        ordered.extend(visible.iter().filter(|e| !e.pinned));
        let mut shown: Vec<&SessionEntry> = ordered.into_iter().take(RAIL_SESSIONS).collect();
        if let Some(open) = active.as_deref() {
            if !shown.iter().any(|e| e.id == open) {
                if let Some(entry) = visible.iter().find(|e| e.id == open) {
                    shown.insert(0, entry);
                }
            }
        }
        for entry in shown {
            let state = if entry.running { AgentState::Running } else { AgentState::Idle };
            // A plain initial on the surface step: rail tiles wear no label
            // tint anywhere.
            let mut cell = RailItem::session(entry.id.clone(), state).label(entry.label.clone());
            if entry.running {
                cell = cell.pulse();
            }
            if active.as_deref() == Some(entry.id.as_str()) {
                cell = cell.selected(true);
            }
            items.push(cell);
        }
        let mut rail = rail("rail", items).flat(true);
        // Sampled with the sidebar's phase: the rail's running cells pulse
        // on the same ticks, and request no frames of their own either.
        if let Some(phase) = self.pulse_phase_value(cx) {
            rail = rail.pulse_phase(phase);
        }
        if let Auth::SignedIn(identity) = &self.auth {
            rail = rail.avatar(identity.initial());
        }
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            // The rail's rows are visible by definition, like the sidebar's:
            // no reveal.
            this.resume_quiet(id.to_string(), window, cx);
        });
        let action = cx.listener(|this: &mut Self, name: &str, window, cx| match name {
            "new" => this.new_session(window, cx),
            // The rail cell opens the full-text search palette, like the
            // header search icon and ⌘⇧F.
            "search" => this.open_search(window, cx),
            "projects" => this.open_projects(false, window, cx),
            "account" => this.open_menu(MenuKind::Account, cx),
            _ => {}
        });
        rail
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |name, w, cx| action(name, w, cx))
            .into_any_element()
    }

    /// The Sessions caption's view menu: where list management lives now that
    /// the footer is the library's account row again.
    ///
    /// Anchored under the fixed caption's sliders icon (see
    /// [`Self::render_sessions_caption`]), right edge aligned to the
    /// sidebar's content edge. The caption never scrolls, so the seat comes
    /// straight from its tracked bounds and ignores the list's scroll; before
    /// the first prepaint there are no bounds, so there is no menu this frame
    /// (never a seat at a fixed corner). The seat keeps its measured
    /// positioning rather than moving to `anchored_menu`.
    pub(crate) fn render_view_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::ViewOptions) {
            return None;
        }
        // Title side sessions never reach the counts: they are hidden by
        // record, but they are not the person's hidden sessions.
        let hidden = self.sessions.iter().filter(|e| e.hidden && !self.is_side_session(&e.id)).count();
        let empty = self
            .sessions
            .iter()
            .filter(|e| !e.archived && !self.is_side_session(&e.id) && e.is_empty())
            .count();
        let archived =
            self.sessions.iter().filter(|e| e.archived && !self.is_side_session(&e.id)).count();
        let mut rows: Vec<MenuRow> = Vec::new();
        let mut actions: Vec<Option<ViewAction>> = Vec::new();
        // The grouping first: it decides what the list below the menu is.
        rows.push(MenuRow::Toggle {
            label: "Group by project".into(),
            checked: self.effective_group_by() == crate::layout::GroupBy::Project,
        });
        actions.push(Some(ViewAction::GroupByProject));
        // Either toggle only appears once it has something to show: an
        // affordance for an empty set is a question nobody asked.
        if empty > 0 || self.show_empty {
            let label = if self.show_empty {
                "Hide empty".to_owned()
            } else {
                format!("Show empty ({empty})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_empty });
            actions.push(Some(ViewAction::ToggleEmpty));
        }
        if hidden > 0 || self.show_hidden {
            let label = if self.show_hidden {
                "Hide hidden".to_owned()
            } else {
                format!("Show hidden ({hidden})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_hidden });
            actions.push(Some(ViewAction::ToggleHidden));
        }
        if empty > 0 {
            rows.push(MenuRow::Toggle { label: "Clear empty".into(), checked: false });
            actions.push(Some(ViewAction::ClearEmpty));
        }
        if !rows.is_empty() {
            rows.push(MenuRow::Separator);
            actions.push(None);
        }
        let archived_label = if self.show_archived {
            "Hide archived".to_owned()
        } else {
            format!("Show archived ({archived})")
        };
        rows.push(MenuRow::Toggle { label: archived_label.into(), checked: self.show_archived });
        actions.push(Some(ViewAction::ToggleArchived));
        rows.push(MenuRow::Separator);
        actions.push(None);
        // Stored here, read by package 2's palette: the toggle lands now so
        // the menu already says what search will do.
        rows.push(MenuRow::Toggle {
            label: "Search all projects".into(),
            checked: self.layout.search_all_projects,
        });
        actions.push(Some(ViewAction::SearchAllProjects));
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            match actions.get(*index).copied().flatten() {
                // Toggles keep the menu open, so the check is seen to change.
                Some(ViewAction::GroupByProject) => {
                    this.toggle_group_by(cx);
                }
                Some(ViewAction::SearchAllProjects) => {
                    this.toggle_search_scope(cx);
                }
                Some(ViewAction::ToggleEmpty) => {
                    this.show_empty = !this.show_empty;
                    cx.notify();
                }
                Some(ViewAction::ToggleHidden) => {
                    this.show_hidden = !this.show_hidden;
                    cx.notify();
                }
                Some(ViewAction::ToggleArchived) => {
                    this.show_archived = !this.show_archived;
                    cx.notify();
                }
                Some(ViewAction::ClearEmpty) => {
                    this.overlays.update(cx, |overlays, _| overlays.menu = None);
                    this.clear_empty(cx);
                }
                None => {}
            }
        });
        // The menu seats under the fixed caption's sliders icon: the caption
        // never scrolls, so the seat takes no scroll offset and never needs
        // the viewport clamp the scrolling caption did. Right edge aligned
        // to the caption's right edge under its 12 px of row padding (both
        // read out of `aui::nav::parts`, which keeps them private), 4 px
        // under it; the menu itself is a fixed 250 px (`aui::nav::view_menu`,
        // private `MENU_W`), clamped into the window so a sidebar narrower
        // than the menu never clips its left side.
        let placed: Option<(f32, f32)> = self.sessions_caption.map(view_menu_seat);
        // No bounds yet (before the first prepaint, or the rail): no menu
        // this frame, never a fixed corner. The notify covers the cold open;
        // the rail guard keeps a scripted menu there from repainting forever.
        let Some((top, left)) = placed else {
            if self.sidebar_open {
                cx.notify();
            }
            return None;
        };
        let pop = div().absolute().top(px(top)).left(px(left));
        // A click anywhere outside closes it: the catcher is a sibling of
        // the menu inside the same deferred draw.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
        });
        let catcher = div()
            .id("sessions-view-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .on_click(move |_, w, cx| dismiss(&(), w, cx));
        Some(
            popover_layer(
                div().absolute().inset_0().child(catcher).child(
                    pop.child(
                        view_menu("sessions-view", rows).at_rest().on_activate(move |i, w, cx| {
                            activate(&i, w, cx)
                        }),
                    ),
                ),
            )
            .into_any_element(),
        )
    }

    /// The footer's account menu: Settings at the top, then Sign out. The
    /// environment lane names itself: `META_API_KEY` survives a sign-out, so
    /// the row says where the credential really comes from (D28).
    pub(crate) fn render_account_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Account) {
            return None;
        }
        let label = match &self.auth {
            Auth::SignedIn(identity) if identity.lane == AccountStateKind::EnvKey => {
                "Sign out (set by META_API_KEY)"
            }
            _ => "Sign out",
        };
        let rows = vec![
            MenuRow::Toggle { label: "Settings…".into(), checked: false },
            MenuRow::Toggle { label: label.into(), checked: false },
        ];
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            if *index == 0 {
                this.overlays.update(cx, |overlays, _| overlays.menu = None);
                this.open_settings(0, cx);
            } else if *index == 1 {
                this.overlays.update(cx, |overlays, _| overlays.menu = None);
                this.logout(cx);
            }
        });
        // Above the footer's top edge, right edges aligned, at any sidebar
        // width — `anchored_menu` flips below and slides inside the window
        // when the seat would overflow. No footer bounds yet: no menu this
        // frame (the rail guard keeps a scripted menu there from repainting
        // forever).
        let Some(trigger) = self.footer_bounds else {
            if self.sidebar_open {
                cx.notify();
            }
            return None;
        };
        // A click anywhere outside closes it: the catcher is a sibling of
        // the menu inside the same draw.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
        });
        let catcher = div()
            .id("account-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .on_click(move |_, w, cx| dismiss(&(), w, cx));
        Some(
            div().absolute().inset_0().child(catcher).child(anchored_menu(
                trigger,
                MenuSide::Above,
                MenuAlign::End,
                view_menu("account", rows).at_rest().on_activate(move |i, w, cx| {
                    activate(&i, w, cx)
                }),
            ))
            .into_any_element(),
        )
    }
}

/// The sidebar's wheel, taken in the capture phase:
/// the transcript's `wheel_capture` twin. Capture runs in registration
/// order ahead of every bubble handler, so this stops the event before the
/// scroll div's own listener can apply it per event; the div keeps its
/// handler for whatever this yields (an overlay on top, a mostly
/// horizontal gesture — the sidebar has no horizontal scroller, but
/// yielding keeps a diagonal gesture's x with the platform). The canvas is
/// the wrapper's first child, so it paints under the rows — clicks still
/// land on them — while its own hitbox covers the list's viewport rect and
/// `should_handle_scroll` yields to whatever occludes it (the palette
/// scrim, a group menu), exactly like the transcript's. Taken events
/// accumulate into `sidebar_pending` and notify ONLY the pane;
/// `render_sidebar` drains one offset write per frame.
fn sidebar_wheel_capture(baaz: &Harness) -> gpui::AnyElement {
    // The shared input cell, not the entity: a wheel event can arrive
    // inside a `Harness` update (a scripted `sidebar-wheel:` step dispatches
    // from one), and updating the entity re-entrantly panics — the cell
    // never borrows it, so the push stays synchronous and N events between
    // paints coalesce into one drain. The pane is never borrowed during a
    // dispatch either (events dispatch outside updates and renders), so its
    // notify is direct too: ONLY the pane, never the root.
    let state = Rc::clone(&baaz.sidebar_wheel);
    let pane = baaz.sidebar_pane.clone();
    gpui::canvas(
        // Prepaint: gpui's own hitbox, so the wheel goes to whatever is
        // actually on top. An open palette or menu over the list owns the
        // pointer, and `should_handle_scroll` is the same test the scroll
        // div itself uses.
        move |bounds, window, _cx| window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal),
        move |_bounds, hitbox, window, _cx| {
            window.on_mouse_event(move |event: &gpui::ScrollWheelEvent, phase, window, cx| {
                if phase != gpui::DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                    return;
                }
                let delta = event.delta.pixel_delta(window.line_height());
                // A gesture that is more sideways than not belongs to
                // whatever is under it.
                if delta.y.abs() < delta.x.abs() {
                    return;
                }
                cx.stop_propagation();
                {
                    let mut state = state.borrow_mut();
                    state.pending += delta.y;
                    state.gesture_until = Some(std::time::Instant::now() + SIDEBAR_GESTURE_HORIZON);
                    state.scrolled = true;
                }
                pane.update(cx, |_, cx| cx.notify());
            });
        },
    )
    .absolute()
    .size_full()
    .into_any_element()
}

/// The Sessions view menu's seat for a fixed caption:
/// 4 px under the caption's bottom edge, right edge aligned to the caption's
/// right edge under its 12 px of row padding; the menu is a fixed 250 px
/// wide, so a sidebar narrower than that clamps the seat's left at 8 px into
/// the window. Pure in the caption bounds — the seat never moves with the
/// list's scroll, which is what pins the menu under the sliders icon that
/// opened it at any scroll offset.
fn view_menu_seat(caption: Bounds<Pixels>) -> (f32, f32) {
    const CAPTION_PAD_X: f32 = 12.0;
    const MENU_GAP: f32 = 4.0;
    const MENU_W: f32 = 250.0;
    let top = f32::from(caption.origin.y) + f32::from(caption.size.height) + MENU_GAP;
    let right_edge = f32::from(caption.origin.x) + f32::from(caption.size.width) - CAPTION_PAD_X;
    (top, (right_edge - MENU_W).max(8.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;
    use gpui::TestAppContext;

    use std::cell::Cell;
    use std::collections::{HashMap, HashSet};
    use std::path::Path;

    use aui::nav::{ensure_row_visible, row_index_for_session, sidebar_list_state, virtual_sidebar_view};

    use crate::layout::Layout;
    use crate::projects::Projects;
    use crate::sidebar::{grouping_by_project, GroupView, SessionEntry};

    fn caption(x: f32, y: f32, w: f32, h: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), gpui::size(px(w), px(h)))
    }

    /// The hover card's seated anchor: the sidebar pane's right edge plus the
    /// card gap, top-aligned with the hovered row — regardless of where in
    /// the row the pointer is, so the card never overlaps the sidebar.
    #[test]
    fn row_detail_seat_sits_at_the_sidebar_edge_aligned_to_its_row() {
        let sidebar = caption(0.0, 0.0, 252.0, 800.0);
        let row = caption(8.0, 140.0, 236.0, 62.0);
        assert_eq!(row_detail_seat(&Some(sidebar), &row), Some(gpui::point(px(256.0), px(140.0))));
    }

    /// A narrow and a wide sidebar seat the card at their own right edge.
    #[test]
    fn row_detail_seat_follows_narrow_and_wide_sidebars() {
        let row = caption(8.0, 140.0, 164.0, 62.0);
        assert_eq!(
            row_detail_seat(&Some(caption(0.0, 0.0, 180.0, 800.0)), &row),
            Some(gpui::point(px(184.0), px(140.0)))
        );
        assert_eq!(
            row_detail_seat(&Some(caption(0.0, 0.0, 400.0, 800.0)), &row),
            Some(gpui::point(px(404.0), px(140.0)))
        );
    }

    /// Before the pane's first prepaint there is no edge, so there is no
    /// seat — and no card.
    #[test]
    fn row_detail_seat_needs_the_pane_edge() {
        assert_eq!(row_detail_seat(&None, &caption(8.0, 140.0, 236.0, 62.0)), None);
    }

    /// An empty column while the list or the index is still landing is
    /// "not here yet", never "nothing exists": the loading copy wins over
    /// every filter state, and the settled copy is unchanged once both
    /// have landed.
    #[test]
    fn empty_state_names_loading_until_both_landed() {
        let (title, _) = empty_state_text(false, false, false, false);
        assert_eq!(title, "Loading sessions…");
        let (title, _) = empty_state_text(false, true, false, false);
        assert_eq!(title, "Loading sessions…");
        let (title, _) = empty_state_text(true, false, false, false);
        assert_eq!(title, "Loading sessions…");
        // Loading wins over the filter states too.
        let (title, _) = empty_state_text(false, false, true, true);
        assert_eq!(title, "Loading sessions…");
        // Settled: the existing copy, untouched.
        let (title, detail) = empty_state_text(true, true, false, false);
        assert_eq!((title, detail), ("No sessions yet", "\u{2318}N starts one."));
        let (title, _) = empty_state_text(true, true, true, false);
        assert_eq!(title, "Every session here is hidden");
        let (title, _) = empty_state_text(true, true, false, true);
        assert_eq!(title, "Only empty sessions here");
    }

    /// Item 1: session rows report hover enter/leave to the caller. A
    /// settled sidebar re-renders on no pointer movement, so the
    /// render-time poll alone could never arm the detail card — the rows'
    /// own hover events must reach the caller. One status group with one
    /// session: sweep the pointer down until the session reports entered
    /// (no redraw between moves, the way a settled sidebar behaves), then
    /// park it past the content and require the matching leave.
    struct HoverProbe {
        grouping: Grouping,
        state: gpui::ListState,
        entered: Rc<std::cell::RefCell<Vec<String>>>,
        left: Rc<std::cell::RefCell<Vec<String>>>,
    }

    impl Render for HoverProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let entered = self.entered.clone();
            let left = self.left.clone();
            v_flex().w(px(300.)).h(px(700.)).child(
                virtual_sidebar_view("hover-probe", self.grouping.clone(), self.state.clone()).on_row_hover(
                    move |id, hovered, _, _| {
                        if hovered {
                            entered.borrow_mut().push(id.to_string());
                        } else {
                            left.borrow_mut().push(id.to_string());
                        }
                    },
                ),
            )
        }
    }

    fn draw_hover(
        vc: &mut gpui::VisualTestContext,
        grouping: &Grouping,
        state: &gpui::ListState,
        entered: &Rc<std::cell::RefCell<Vec<String>>>,
        left: &Rc<std::cell::RefCell<Vec<String>>>,
    ) {
        let probe = HoverProbe { grouping: grouping.clone(), state: state.clone(), entered: entered.clone(), left: left.clone() };
        vc.draw(point(px(0.), px(0.)), gpui::size(px(300.), px(700.)), |_, cx| {
            cx.new(|_| probe).into_any_element()
        });
    }

    #[gpui::test]
    fn session_rows_report_hover_enter_and_leave(cx: &mut TestAppContext) {
        use aui::nav::{SessionSummary, StatusGroup};
        use aui_tokens::AgentState;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let grouping = Grouping::Status(vec![StatusGroup::new(
            "done",
            "Done",
            "1",
            vec![SessionSummary::new("s-1", "checkout-flow-v2", AgentState::Idle, "now")],
        )]);
        let total = flatten_sidebar(&grouping, false).len();
        let state = sidebar_list_state(total);
        let entered = Rc::new(std::cell::RefCell::new(Vec::new()));
        let left = Rc::new(std::cell::RefCell::new(Vec::new()));
        let vc = cx.add_empty_window();
        draw_hover(vc, &grouping, &state, &entered, &left);
        // Park past the content first: whatever the platform's initial
        // pointer position hovered reports at most a leave, never an enter.
        vc.simulate_mouse_move(point(px(150.), px(690.)), None::<gpui::MouseButton>, gpui::Modifiers::default());
        assert!(entered.borrow().is_empty(), "empty list area reports no enter");
        entered.borrow_mut().clear();
        left.borrow_mut().clear();
        let mut hit = false;
        for y in (10..300).step_by(10) {
            vc.simulate_mouse_move(
                point(px(150.), px(y as f32)),
                None::<gpui::MouseButton>,
                gpui::Modifiers::default(),
            );
            if entered.borrow().iter().any(|id| id == "s-1") {
                hit = true;
                break;
            }
        }
        assert!(hit, "some sweep position must hover the one session row");
        vc.simulate_mouse_move(point(px(150.), px(690.)), None::<gpui::MouseButton>, gpui::Modifiers::default());
        assert!(left.borrow().iter().any(|id| id == "s-1"), "leaving the row reports the leave");
    }

    /// Re-render the sidebar pane on demand. A test draw reuses the cached
    /// pane's retained subtree while its bounds match, so redraws at an
    /// unchanged size never rebuild it: growing the draw size a pixel per
    /// pass busts the cache key, and the next draw re-renders down through
    /// the cached pane — `render_sidebar` runs for real. The returned count
    /// proves the renders happened — a hover test that never rendered its
    /// pane would pass vacuously.
    fn redraw_sidebar_pane(vc: &mut gpui::VisualTestContext, baaz: &Entity<Harness>) -> u64 {
        take_sidebar_pane_renders();
        for step in 0..3 {
            let size = gpui::size(px(1280.0), px(801.0 + step as f32));
            vc.draw(point(px(0.), px(0.)), size, |_, _| {
                baaz.clone().into_any_element()
            });
            vc.run_until_parked();
        }
        take_sidebar_pane_renders()
    }

    /// One hover-fixture row: the live trace rested on one row at a time,
    /// and the move-off case needs a second row to move to.
    fn hover_entry(id: &str) -> SessionEntry {
        SessionEntry {
            id: id.into(),
            label: format!("Session {id}"),
            updated: chrono::Local::now(),
            running: false,
            turns: 3,
            hidden: false,
            pinned: false,
            archived: false,
            description: "the ask".into(),
            replayed: false,
            named: true,
            needs_title: false,
            title_pending: false,
            last_ask: Some("the ask".into()),
            local: false,
            provisional: false,
            workspace: None,
            project: None,
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            branch: None,
        }
    }

    /// A live trace, replayed: a row armed through the real report
    /// path stays armed across pane renders that observe no GPUI hover, and
    /// the delay timer opens its card. Before the fix the render-time poll
    /// read `gpui-hover=None` on every pane render and cleared the armed
    /// row, so every timer ended `matches=false shown=false` and no card
    /// ever opened. A `left` report closes the card; a second row's
    /// `entered` moves it.
    #[gpui::test]
    fn pane_renders_never_disarm_an_armed_hover(cx: &mut TestAppContext) {
        use aui::nav::SESSION_DETAIL_DELAY;
        use muse_client::schema::AccountStateKind;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        // Hermetic state: boot adopts the workspace into the projects
        // store, so the temp dir keeps the real store untouched.
        let dir = std::env::temp_dir().join(format!("baaz-armed-hover-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock().lock().expect("test env lock");
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let args = crate::Args {
            workspace: dir.clone(),
            workspace_explicit: true,
            provider: "echo".into(),
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
        };
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            let baaz =
                cx.new(|cx| Harness::new(args, crate::shot::CaptureToken::default(), window, cx));
            baaz.update(cx, |h, _| {
                h.auth = crate::login::Auth::SignedIn(crate::auth::Identity {
                    lane: AccountStateKind::AccountLogin,
                    name: "Probe".into(),
                    email: String::new(),
                });
                h.sessions.push(hover_entry("s-hover-1"));
                h.sessions.push(hover_entry("s-hover-2"));
                h.invalidate_list();
            });
            baaz
        });
        // The real root as the app builds it: the cached pane, the app's
        // own row handlers, the card mounted from `render_row_detail`.
        take_sidebar_pane_renders();
        vc.draw(point(px(0.), px(0.)), gpui::size(px(1280.), px(800.)), |_, _| {
            baaz.clone().into_any_element()
        });
        // A neutral pointer: no row is GPUI-hovered, so the removed poll
        // would have read `gpui-hover=None` here — the trace's readout.
        vc.simulate_mouse_move(
            point(px(1200.), px(780.)),
            None::<gpui::MouseButton>,
            gpui::Modifiers::default(),
        );
        // Arm through the real report path: what the row's own hover event
        // hands the app — the row's bounds, never the pointer.
        let row_1 = caption(8.0, 140.0, 236.0, 62.0);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.note_row_hover("s-hover-1".into(), true, Some(row_1), cx);
            });
        });
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).hovered_row.clone()).as_deref(),
            Some("s-hover-1"),
            "the report arms the row"
        );
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).hover_trigger),
            Some(row_1),
            "the report seats the row's own bounds, never the pointer"
        );
        // The seated anchor hangs off the laid-out pane's right edge,
        // top-aligned with the row: the pane really painted above, so its
        // edge is tracked.
        let sidebar = vc
            .update(|_, cx| baaz.read(cx).sidebar_bounds)
            .expect("the pane prepaint lays the sidebar edge out");
        assert_eq!(
            row_detail_seat(&Some(sidebar), &row_1),
            Some(gpui::point(
                sidebar.origin.x + sidebar.size.width + px(SESSION_DETAIL_GAP),
                row_1.origin.y
            )),
            "the seat hangs off the sidebar edge at the row's top"
        );
        // Pane renders while GPUI reports no hover: the armed row survives.
        let renders = redraw_sidebar_pane(vc, &baaz);
        assert!(renders > 0, "the test must really render the pane, or it proves nothing");
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).hovered_row.clone()).as_deref(),
            Some("s-hover-1"),
            "pane renders never disarm a reported hover"
        );
        // Past the delay: the timer opens the card and the builder mounts it.
        vc.cx
            .executor()
            .advance_clock(SESSION_DETAIL_DELAY + std::time::Duration::from_millis(100));
        vc.run_until_parked();
        let (shown, card) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| (h.hover_shown, h.render_row_detail(cx).is_some()))
        });
        assert!(shown, "the delay timer opens the card for the still-armed row");
        assert!(card, "the card element is mounted once the delay elapses");
        // The row's own leave closes it.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.note_row_hover("s-hover-1".into(), false, None, cx);
            });
        });
        let (armed, shown, card) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                (h.hovered_row.clone(), h.hover_shown, h.render_row_detail(cx).is_some())
            })
        });
        assert!(armed.is_none() && !shown && !card, "the leave report closes the card");
        // A second row's enter moves the arm; its card opens past the delay.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.note_row_hover("s-hover-2".into(), true, Some(caption(8.0, 210.0, 236.0, 62.0)), cx);
            });
        });
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).hovered_row.clone()).as_deref(),
            Some("s-hover-2"),
            "a second row's enter moves the arm"
        );
        vc.cx
            .executor()
            .advance_clock(SESSION_DETAIL_DELAY + std::time::Duration::from_millis(100));
        vc.run_until_parked();
        let (shown, card) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| (h.hover_shown, h.render_row_detail(cx).is_some()))
        });
        assert!(shown && card, "the moved row's card opens past the delay");
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The hover report itself seats the card, across pane renders. A
    /// settled sidebar re-renders nothing, so the delay timer's notify
    /// reaches only the root (the pane stays cached): the row's own bounds
    /// come from the row's hover report. The real root as the app builds it
    /// (the cached pane, the app's own handlers): sweep the pointer over a
    /// row with no redraw between moves, require the trigger set to the
    /// row's bounds and the seat hung off the sidebar edge, force pane
    /// renders, re-sweep to restore the resting hover, let
    /// `SESSION_DETAIL_DELAY` elapse, and require the card builder to return
    /// its element (what the root mounts).
    #[gpui::test]
    fn hover_report_seats_the_card_across_pane_renders(cx: &mut TestAppContext) {
        use aui::nav::SESSION_DETAIL_DELAY;
        use muse_client::schema::AccountStateKind;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        // Hermetic state: boot adopts the workspace into the projects
        // store, so the temp dir keeps the real store untouched.
        let dir = std::env::temp_dir().join(format!("baaz-hover-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock().lock().expect("test env lock");
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let args = crate::Args {
            workspace: dir.clone(),
            workspace_explicit: true,
            provider: "echo".into(),
            program: "muse".into(),
            theme: aui_tokens::ThemeKind::Dark,
            screenshot: None,
            delay: std::time::Duration::from_millis(500),
            session: None,
            send: None,
            offline: true,
            replay: None,
            steps: Vec::new(),
            tier: None,
            print_tier: false,
            approval_mode: None,
            login: crate::LoginSample::Choose,
            login_steps: Vec::new(),
            bench: None,
            bench_cadence: std::time::Duration::from_millis(4),
            bench_scroll: crate::bench::BenchScroll::Sweep,
            bench_frames: 600,
            bench_out: None,
            bench_open_turn: false,
            bench_bare: false,
            bench_shell: false,
            sidebar_fixture: None,
            no_project: false,
        };
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            let baaz =
                cx.new(|cx| Harness::new(args, crate::shot::CaptureToken::default(), window, cx));
            baaz.update(cx, |h, _| {
                h.auth = crate::login::Auth::SignedIn(crate::auth::Identity {
                    lane: AccountStateKind::AccountLogin,
                    name: "Probe".into(),
                    email: String::new(),
                });
                h.sessions.push(SessionEntry {
                    id: "s-hover-1".into(),
                    label: "Rewrite the router".into(),
                    updated: chrono::Local::now(),
                    running: false,
                    turns: 3,
                    hidden: false,
                    pinned: false,
                    archived: false,
                    description: "the ask".into(),
                    replayed: false,
                    named: true,
                    needs_title: false,
                    title_pending: false,
                    last_ask: Some("the ask".into()),
                    local: false,
                    provisional: false,
                    workspace: None,
                    project: None,
                    project_name: None,
                    attention: Vec::new(),
                    approval_command: None,
                    pending_question: None,
                    turn_started: None,
                    last_error: None,
                    branch: None,
                });
                h.invalidate_list();
            });
            baaz
        });
        // The real root as the app builds it: the cached pane, the app's
        // own row handlers, the card mounted from `render_row_detail`.
        take_sidebar_pane_renders();
        vc.draw(point(px(0.), px(0.)), gpui::size(px(1280.), px(800.)), |_, _| {
            baaz.clone().into_any_element()
        });
        // Park where no session row lives, so the sweep below starts from
        // a neutral pointer with nothing armed.
        vc.simulate_mouse_move(
            point(px(1200.), px(780.)),
            None::<gpui::MouseButton>,
            gpui::Modifiers::default(),
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).hovered_row.clone()).is_none(),
            "the parked pointer arms no row"
        );
        // The settled sidebar: no redraw between moves.
        take_sidebar_pane_renders();
        let mut hit: Option<f32> = None;
        for y in (100..780).step_by(10) {
            vc.simulate_mouse_move(
                point(px(100.), px(y as f32)),
                None::<gpui::MouseButton>,
                gpui::Modifiers::default(),
            );
            let hovered = vc.update(|_, cx| baaz.read(cx).hovered_row.clone());
            if hovered.as_deref() == Some("s-hover-1") {
                hit = Some(y as f32);
                break;
            }
        }
        let y = hit.expect("some sweep position must hover the session row");
        assert_eq!(
            take_sidebar_pane_renders(),
            0,
            "a settled sidebar re-renders on no pointer movement"
        );
        // The report itself seats the card: the trigger is the row's own
        // bounds — never the reporting pointer — and the seat hangs off the
        // laid-out pane's right edge at the row's top.
        let trigger = vc
            .update(|_, cx| baaz.read(cx).hover_trigger)
            .expect("the hover report must seat the card's trigger");
        let sidebar = vc
            .update(|_, cx| baaz.read(cx).sidebar_bounds)
            .expect("the pane prepaint lays the sidebar edge out");
        assert!(
            f32::from(trigger.origin.x) < 100.0 && f32::from(trigger.size.height) > 1.0,
            "the trigger is the row's rect, not the reporting pointer, got {trigger:?}"
        );
        assert!(
            f32::from(trigger.origin.y) <= y
                && y < f32::from(trigger.origin.y) + f32::from(trigger.size.height),
            "the reporting pointer sits inside the trigger row, got {trigger:?} for a report at (100, {y})"
        );
        assert_eq!(
            row_detail_seat(&Some(sidebar), &trigger),
            Some(gpui::point(
                sidebar.origin.x + sidebar.size.width + px(SESSION_DETAIL_GAP),
                trigger.origin.y
            )),
            "the seat hangs off the sidebar edge at the row's top"
        );
        // Pane renders while a row is armed: the renders really happen, and
        // the report path re-arms afterwards. A test draw drops the retained
        // tree, so the rebuilt rows report the teardown as a leave although
        // the pointer never moved — production reuses retained state across
        // frames (the live trace shows no resting leave), so no leave fires
        // there. Re-sweep to restore the resting hover the renders found.
        let renders = redraw_sidebar_pane(vc, &baaz);
        assert!(renders > 0, "the test must really render the pane, or it proves nothing");
        take_sidebar_pane_renders();
        let mut rehit: Option<f32> = None;
        for y in (100..780).step_by(10) {
            vc.simulate_mouse_move(
                point(px(100.), px(y as f32)),
                None::<gpui::MouseButton>,
                gpui::Modifiers::default(),
            );
            let hovered = vc.update(|_, cx| baaz.read(cx).hovered_row.clone());
            if hovered.as_deref() == Some("s-hover-1") {
                rehit = Some(y as f32);
                break;
            }
        }
        let y = rehit.expect("the rebuilt rows must report hover again after pane renders");
        let trigger = vc
            .update(|_, cx| baaz.read(cx).hover_trigger)
            .expect("the hover report must seat the card's trigger again");
        assert!(
            f32::from(trigger.origin.x) < 100.0 && f32::from(trigger.size.height) > 1.0,
            "the trigger is the row's rect, not the reporting pointer, got {trigger:?}"
        );
        assert!(
            f32::from(trigger.origin.y) <= y
                && y < f32::from(trigger.origin.y) + f32::from(trigger.size.height),
            "the reporting pointer sits inside the trigger row, got {trigger:?} for a report at (100, {y})"
        );
        // Past the delay with still no redraw: the timer opens the card
        // for the still-hovered row, and the builder returns its element.
        vc.cx
            .executor()
            .advance_clock(SESSION_DETAIL_DELAY + std::time::Duration::from_millis(100));
        vc.run_until_parked();
        assert_eq!(
            take_sidebar_pane_renders(),
            0,
            "the delay timer must not need a pane render to open the card"
        );
        let (shown, card) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| (h.hover_shown, h.render_row_detail(cx).is_some()))
        });
        assert!(shown, "the delay timer opens the card for the still-hovered row");
        assert!(card, "the card element is mounted once the delay elapses");
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn view_menu_seat_sits_under_the_caption_right_aligned() {
        // A 400 px caption: the menu hangs 4 px under its bottom edge with
        // its right edge 12 px inside the caption's (400 - 12 - 250).
        assert_eq!(view_menu_seat(caption(0.0, 138.0, 400.0, 28.0)), (170.0, 138.0));
    }

    #[test]
    fn view_menu_seat_clamps_into_a_narrow_sidebar() {
        // A 200 px sidebar is narrower than the 250 px menu: the seat's left
        // stops at 8 px instead of running off the window's left edge.
        assert_eq!(view_menu_seat(caption(0.0, 100.0, 200.0, 28.0)), (132.0, 8.0));
    }

    /// The virtual sidebar builds only visible rows for a stress sidebar
    ///: 200 sessions across 8 projects through the
    /// baaz's own grouping (flags, folds, rescue) into the library's
    /// virtual view — the per-frame build budget the div-scroll path could
    /// never keep. Reverting the sessions area to div-scroll builds all
    /// 100+ rows and fails this test.
    struct VirtualProbe {
        grouping: Grouping,
        state: gpui::ListState,
        built: Rc<Cell<usize>>,
    }

    impl Render for VirtualProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let built = self.built.clone();
            v_flex().w(px(300.)).h(px(700.)).child(
                virtual_sidebar_view("probe", self.grouping.clone(), self.state.clone())
                    .on_row_built(move |_| {
                        built.set(built.get() + 1);
                    }),
            )
        }
    }

    fn draw_probe(
        vc: &mut gpui::VisualTestContext,
        grouping: &Grouping,
        state: &gpui::ListState,
        built: &Rc<Cell<usize>>,
    ) -> usize {
        built.set(0);
        let host = VirtualProbe { grouping: grouping.clone(), state: state.clone(), built: built.clone() };
        vc.draw(point(px(0.), px(0.)), gpui::size(px(300.), px(700.)), |_, cx| {
            cx.new(|_| host).into_any_element()
        });
        built.get()
    }

    #[gpui::test]
    fn virtual_sidebar_builds_only_visible_rows_for_a_stress_sidebar(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        // On disk, or availability hides every group and the model shrinks
        // to one closed row.
        for i in 0..8 {
            std::fs::create_dir_all(format!("/tmp/r6c-probe-{i}")).expect("temp root");
        }
        let mut projects = Projects::default();
        let ids: Vec<String> =
            (0..8).map(|i| projects.add(Path::new(&format!("/tmp/r6c-probe-{i}"))).id.clone()).collect();
        let now = chrono::Local::now();
        let entries: Vec<SessionEntry> = ids
            .iter()
            .enumerate()
            .flat_map(|(p, id)| {
                (0..25).map(move |s| SessionEntry {
                    id: format!("p{p}-s{s}"),
                    label: "x".into(),
                    updated: now,
                    running: false,
                    turns: 0,
                    hidden: false,
                    pinned: false,
                    archived: false,
                    description: String::new(),
                    replayed: false,
                    named: false,
                    needs_title: false,
                    title_pending: false,
                    last_ask: None,
                    local: false,
                    provisional: false,
                    workspace: None,
                    project: Some(id.clone()),
                    project_name: None,
                    attention: Vec::new(),
                    approval_command: None,
                    pending_question: None,
                    turn_started: None,
                    last_error: None,
                    branch: None,
                })
            })
            .collect();
        let closed = HashSet::new();
        let expanded: HashSet<String> = [ids[7].clone()].into_iter().collect();
        let view = GroupView { closed: &closed, expanded: &expanded, active: Some("p6-s12"), pending: None };
        let grouping = grouping_by_project(&entries, &projects, &HashMap::new(), &view, &Layout::default(), now);
        let rows = flatten_sidebar(&grouping, false);
        let total = rows.len();
        assert!(total > 48, "the stress model must stay large enough to prove virtualisation: {total}");
        let deep = row_index_for_session(&rows, &grouping, &"p6-s12".into());
        assert!(deep.is_some_and(|ix| ix > total / 2), "a deep row exists: {deep:?}");

        let state = sidebar_list_state(total);
        let built = Rc::new(Cell::new(0usize));
        let vc = cx.add_empty_window();

        // Same budget as the library's own test: ~24 rows visible, the
        // overdraw runway, one boundary row — under a quarter of what a
        // non-virtualised frame builds.
        let cold = draw_probe(vc, &grouping, &state, &built);
        assert!(cold <= 48, "cold frame built {cold} rows of {total}");
        assert!(cold > 10, "cold frame built suspiciously few rows: {cold}");

        let warm = draw_probe(vc, &grouping, &state, &built);
        assert!(warm <= 48, "warm frame built {warm} rows of {total}");

        state.scroll_by(px(2000.));
        let scrolled = draw_probe(vc, &grouping, &state, &built);
        assert!(scrolled <= 48, "scrolled frame built {scrolled} rows of {total}");
        assert!(state.logical_scroll_top().item_ix > 0, "scroll_by moved the list");

        let last = row_index_for_session(&rows, &grouping, &"p7-s24".into()).expect("last session has a row");
        ensure_row_visible(&state, last);
        draw_probe(vc, &grouping, &state, &built);
        draw_probe(vc, &grouping, &state, &built);
        assert!(state.bounds_for_item(last).is_some(), "revealed row has bounds");
        assert_eq!(state.item_is_below_viewport(last), Some(false), "revealed row is on screen");
    }

    #[test]
    fn render_counters_drain_what_they_counted() {
        // The idle instrument's contract: `take_*` zeroes,
        // so the take after a window holds only that window's renders. The
        // counters are process-global and only the render paths note them,
        // which unit tests never reach — drain first to stay hermetic.
        take_baaz_root_renders();
        take_sidebar_pane_renders();
        note_baaz_render();
        note_baaz_render();
        assert_eq!(take_baaz_root_renders(), 2);
        assert_eq!(take_baaz_root_renders(), 0);
        assert_eq!(take_sidebar_pane_renders(), 0);
    }

    #[test]
    fn view_menu_seat_for_the_default_sidebar() {
        // The standard 252 px sidebar with the caption at y 146: the menu
        // opens 4 px under it with its left clamped at 8 px (252 - 12 is
        // narrower than the 250 px menu). The seat takes no scroll offset,
        // so it stays under the sliders icon at any scroll position.
        assert_eq!(view_menu_seat(caption(0.0, 146.0, 252.0, 28.0)), (178.0, 8.0));
    }

    /// An id with no row yet keeps its reveal armed across misses instead
    /// of dropping it on the first one:
    /// a draft before its first send, a fork before `load_sessions`'s
    /// reply lands. The streak counts up for the same id and past
    /// `REVEAL_UNKNOWN_FRAMES` the caller gives up.
    #[test]
    fn an_unknown_reveal_id_streaks_instead_of_dropping_on_the_first_miss() {
        let mut state = None;
        for _ in 0..5 {
            state = Some(next_reveal_unknown_streak(state, "s-draft"));
        }
        assert_eq!(state, Some(("s-draft".to_owned(), 5)));
    }

    #[test]
    fn a_fresh_id_resets_the_streak_instead_of_extending_the_old_one() {
        let after_old = next_reveal_unknown_streak(Some(("s-old".to_owned(), 40)), "s-new");
        assert_eq!(after_old, ("s-new".to_owned(), 1));
    }

    #[test]
    fn the_streak_eventually_passes_the_bound() {
        let mut state = None;
        for _ in 0..=REVEAL_UNKNOWN_FRAMES {
            state = Some(next_reveal_unknown_streak(state, "s-never-arrives"));
        }
        let (_, streak) = state.expect("streak recorded");
        assert!(streak > REVEAL_UNKNOWN_FRAMES, "streak {streak} did not pass the bound");
    }
}
