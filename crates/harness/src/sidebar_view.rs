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
    dense_field, ensure_row_visible, flatten_sidebar, group_row, nav_item, rail, row_index_for_session,
    sidebar_footer, view_menu, virtual_sidebar_view, GroupAction, MenuRow, RailItem, RowAction, SidebarRow,
};
use aui::overlay::{anchored_menu, popover_layer, MenuAlign, MenuSide};
use aui_icons::{IconName, Provider};
use aui_motion::pulse_phase;
use aui_tokens::{scale, ActiveAui, AgentState, AuiStyled, Palette};
use gpui::{
    div, prelude::*, px, AnyElement, Bounds, Context, Entity, Focusable, ListOffset, Pixels, Render,
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

/// The sidebar column as its own view (owner round 4 §3).
///
/// `Harness::render` used to rebuild the sidebar element on every frame —
/// including every wheel notify, whose transcript centre is the only thing
/// that moved. Embedded with gpui's `.cached(size_full)`, a clean pane
/// reuses its retained subtree, so a transcript notify re-renders `Harness`
/// (composition only) and the `SessionView` centre, not the column. The pane
/// reads the harness live through a weak handle and renders exactly what
/// `Harness::render_sidebar` renders — that method keeps its shape and its
/// listeners, so what the column draws cannot drift. [`SidebarKey`] is what
/// re-arms it: [`Harness::sync_sidebar_pane`] notifies it from `on_frame`
/// whenever its inputs change, and notifies from inside its own subtree
/// (hover, its scroll container, the rename editor, and — since owner
/// round 6 replaced the old reveal prepaint intents with `ListState`
/// steering — the reveal's own outside-the-draw pane-notify task in
/// [`Harness::reveal_sidebar_row`]) dirty it directly through the view
/// tree.
pub(crate) struct SidebarPane {
    harness: WeakEntity<Harness>,
}

impl SidebarPane {
    pub(crate) fn new(harness: WeakEntity<Harness>) -> Self {
        Self { harness }
    }
}

/// Sidebar-pane renders since process start (owner round 5 §A1.6): the
/// sidebar analogue of `WHEEL_SCROLL_BYS`. Relaxed atomics, drained per
/// `sidebar-wheel:` log line, so a burst's pane/root renders per event are
/// readable off a scripted run.
static SIDEBAR_PANE_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static HARNESS_ROOT_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the pane-render count above, for the `sidebar-wheel:` report.
pub(crate) fn take_sidebar_pane_renders() -> u64 {
    SIDEBAR_PANE_RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Pane rebuilds since the last traced root tick (owner round 6, part C3):
/// a dedicated counter so the frame trace's per-row drain never steals from
/// the `sidebar-wheel:`/`resize-sweep:` steps' own accumulation, which spans
/// many ticks between their own explicit drains.
static TRACE_PANE_TICKS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the trace-only pane count above, for one frame-trace row.
pub(crate) fn take_trace_pane_ticks() -> u64 {
    TRACE_PANE_TICKS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Drain the root-render count below, for the `sidebar-wheel:` report.
pub(crate) fn take_harness_root_renders() -> u64 {
    HARNESS_ROOT_RENDERS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// How long a sidebar wheel gesture stays open after its last event
/// (owner round 5 §A1): the momentum tail arrives at 60 Hz, so 150 ms
/// covers a missed sample. Deliberately the same 150 ms the transcript
/// keeps; while it is in the future the pane presents every tick and no
/// reveal may move the list.
const SIDEBAR_GESTURE_HORIZON: std::time::Duration = std::time::Duration::from_millis(150);

/// How many consecutive `reveal_sidebar_row` misses an id with no row yet
/// gets before the reveal gives up (owner round 6, part C4 review #1): a
/// bound, not a real deadline — the row-birth sites (the `turn/started`
/// local-row insert, a `load_sessions` reply) already notify, so a normal
/// wait is one or two attempts; this only guards against an id that never
/// arrives at all (a failed fork, a send that never lands).
pub(crate) const REVEAL_UNKNOWN_FRAMES: u32 = 120;

/// The bookkeeping [`Harness::reveal_sidebar_row`] does for an unknown
/// reveal id, pulled out pure so it is unit-testable without a window
/// (owner round 6, part C4 review #1): a miss for the *same* id the
/// previous call saw extends its streak; a miss for a *different* id (a
/// fresh arm since) starts a new one at 1.
fn next_reveal_unknown_streak(current: Option<(String, u32)>, reveal_id: &str) -> (String, u32) {
    match current {
        Some((id, streak)) if id == reveal_id => (id, streak + 1),
        _ => (reveal_id.to_owned(), 1),
    }
}

/// Sidebar drains actually applied since process start (owner round 5 §A1):
/// one per frame that had accumulated travel, against one offset write per
/// event before. Drained per `sidebar-wheel:` line, like `take_wheel_scroll_bys`.
static SIDEBAR_WHEEL_DRAINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Drain the applied-drain count above, for the `sidebar-wheel:` report.
pub(crate) fn take_sidebar_wheel_drains() -> u64 {
    SIDEBAR_WHEEL_DRAINS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The sidebar wheel's input-side state (owner round 5 §A1): what the
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
pub(crate) fn note_harness_render() {
    HARNESS_ROOT_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

impl Render for SidebarPane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SIDEBAR_PANE_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if crate::session::frame_trace_enabled() {
            TRACE_PANE_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        match self.harness.upgrade() {
            Some(harness) => harness.update(cx, |harness, cx| harness.render_sidebar(window, cx)),
            None => div().into_any_element(),
        }
    }
}

/// What the sidebar pane shows, as an equality key (owner round 4 §3).
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
    pub(crate) fn current(harness: &Harness, cx: &gpui::App) -> Self {
        let visible = harness.visible_sessions(cx);
        let grouping = harness.sidebar_grouping(cx);
        let selected = harness
            .pending_id
            .clone()
            .or_else(|| harness.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let auth = match &harness.auth {
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
        let tier = harness.tier.as_ref().map(|tier| {
            (tier.footer_label(), tier.is_warning(), tier.weekly_fraction().map(f32::to_bits))
        });
        Self {
            visible: Rc::as_ptr(&visible) as usize,
            grouping: Rc::as_ptr(&grouping) as usize,
            selected,
            renaming: harness.renaming.clone(),
            reveal: harness.reveal.clone(),
            auth,
            tier_args: harness.args.tier.is_some(),
            tier,
            current_project: harness.current_project.clone(),
        }
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

impl Harness {
    /// Re-arm the sidebar pane when its inputs changed (owner round 4 §3).
    ///
    /// Called from `on_frame`, before anything draws: while the key matches,
    /// wheel notifies leave the pane clean and gpui reuses its cached
    /// element instead of rebuilding the column. The comparison itself is a
    /// few pointer reads and small clones — no list rebuild.
    pub(crate) fn sync_sidebar_pane(&mut self, cx: &mut Context<Self>) {
        let key = SidebarKey::current(self, cx);
        if self.sidebar_key.as_ref() != Some(&key) {
            self.sidebar_key = Some(key);
            self.sidebar_pane.update(cx, |_, cx| cx.notify());
        }
    }

    /// The sessions list's current scroll position (owner round 6): what
    /// `sidebar-wheel:` and the resize drag log sample. The list walks down
    /// as `item_ix` grows, with `offset_in_item` the pixels into that row —
    /// the sidebar analogue of `SessionView::bench_list_top`.
    pub(crate) fn sidebar_list_top(&self) -> ListOffset {
        self.sidebar_list.logical_scroll_top()
    }

    /// Whether a sidebar wheel gesture is in flight (owner round 5 §A1):
    /// an event landed within the horizon. While this holds the pane
    /// presents every tick and no reveal installs.
    pub(crate) fn sidebar_gesture_active(&self) -> bool {
        self.sidebar_wheel.borrow().gesture_until.is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Push one frame-paced sweep delta into the sidebar's accumulator
    /// (owner round 6, part C3): the same three writes
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

    /// Apply the capture handler's input (owner round 5 §A1, owner round 6):
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
    /// the harness block inset shifted them 4 px off the session rows'
    /// gutter (ruler on the round-4 captures: nav icon centre 41 vs session
    /// dot centre 36 before; owner round 4, O3).
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

    /// The Sessions caption, fixed above the scrolling list (owner round 4
    /// fixup): the header stays put with its spacing at any scroll offset,
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
        // Owner round 5 §A1, owner round 6: one travel per frame. The capture
        // handler only accumulates; this drain is the frame's single
        // `scroll_by`, before the list lays out.
        let user_scrolled = self.drain_sidebar_wheel();
        // A list that moved under an open group menu leaves it mis-seated:
        // group menus dismiss on scroll (owner round 6, the calm option —
        // the bounds intents re-seat a reopened menu on the next frame).
        // The header menu seats from the fixed crumb and stays. Mid-render
        // the close needs no notify: the overlay reads it below on this
        // same frame.
        if user_scrolled {
            self.dismiss_group_menu(cx);
        }
        // Keep presenting through the tail: a frame every tick while the
        // gesture is open, so the momentum tail is never cut — what the
        // transcript does for its own gesture. A settled sidebar requests
        // nothing and the cached pane is reused untouched.
        if self.sidebar_gesture_active() {
            window.request_animation_frame();
        }
        let visible = self.visible_sessions(cx);
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
        // The click's target first: the row highlights on the click's own
        // frame, before the new view (or any page) exists.
        let selected =
            self.pending_id.clone().or_else(|| self.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            // A sidebar click never arms the reveal: the clicked row is
            // under the cursor, hence painted inside the viewport (owner
            // round 6).
            this.resume_quiet(id.to_string(), window, cx);
        });
        let act = cx.listener(|this: &mut Self, (id, action): &(SharedString, RowAction), window, cx| {
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
        let mut view = virtual_sidebar_view("sessions", Rc::clone(&grouping), self.sidebar_list.clone())
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
            .on_action(move |id, action, w, cx| act(&(id.clone(), action), w, cx));
        if let Some(renaming) = self.renaming.clone() {
            // The library builds the renaming row more than once per frame,
            // so the editor arrives as a builder it calls on every build —
            // never as one element the first build would claim. Everything
            // the builder needs is resolved here, once per pane render; the
            // per-build closure only builds.
            let rename = self.rename.clone();
            let palette = cx.aui().colors;
            let focused = rename.focus_handle(cx).is_focused(window);
            let harness = cx.entity().downgrade();
            view = view.editing(renaming, move |_, _| {
                Self::rename_editor(&rename, palette, focused, harness.clone())
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
        // The one-shot reveal (owner round 4, O6; owner round 5 §A1; owner
        // round 6: armed only by outside-the-sidebar activations, steering
        // the virtual list to the row with the least move). Never from a
        // sidebar click, a list refresh, a regroup, during a wheel gesture,
        // or after the user has scrolled — the flag only exists between an
        // outside activation and the row reporting visible, and a wheel
        // disarms it outright. A resize drag owns the list the same way:
        // nothing steers while one is in flight (owner round 6).
        if !self.sidebar_user_scrolled && !self.sidebar_gesture_active() && !self.resize.active {
            if let Some(reveal_id) = self.reveal.clone() {
                self.reveal_sidebar_row(&rows, &grouping, &reveal_id, cx);
            }
        }
        // No quick-filter field: ⌘⇧F and the sidebar search icon open the
        // full-text search palette instead, so the two can never share the
        // sidebar.
        v_flex()
            .size_full()
            .child(self.render_nav_block(cx))
            .child(self.render_sessions_caption(cx))
            // The list's positioned wrapper (owner round 5 §A1, owner round
            // 6): `relative` only establishes the containing block — the
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
            .child(self.render_footer(cx))
            .into_any_element()
    }

    /// What a regroup or filter change looks like to the virtual list
    /// (owner round 6): the mode and the three list-management toggles.
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

    /// Keep the caller-owned list state on the flattened rows (owner round
    /// 6, the library's adoption guide): re-flattened per frame by the
    /// caller; `reset` after a regroup or filter change (the one sync that
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

    /// Steer the virtual list to the outside-activated session, moving the
    /// least distance that shows its row whole (owner round 6): the row
    /// itself when the flattened model has one — folds rescue held-back
    /// rows for the pending target, so no expansion dance — else its group
    /// head for a closed group (never auto-expanded, like before), else
    /// wait for an id with no row yet (a draft before its first send, a
    /// fork before `load_sessions`'s reply lands) up to
    /// [`REVEAL_UNKNOWN_FRAMES`] attempts, else clear a flag with nowhere
    /// to go (owner round 6, part C4 review #1: a *known* session that
    /// still has no landing row gives up on the first miss, same as
    /// before — only an unrecognised id gets the grace period). A sidebar
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
                // A known session with nowhere to go (owner round 5 §A1): a
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
            // The list has not laid out yet (owner round 6, part C4 review
            // #4): a degenerate zero-height sidebar rect would otherwise
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
        harness: WeakEntity<Self>,
    ) -> AnyElement {
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(move |_: &ConfirmRename, window, cx| {
                let _ = harness.update(cx, |this, cx| this.commit_rename(window, cx));
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
        let active = self.active_id(cx);
        // What each filter alone is keeping out, past the other one: the
        // empty text names its own toggle rather than borrowing hidden's.
        // (There is no sidebar text filter — search lives in the palette —
        // so no "no match" state exists here.)
        let hidden_only = !self.show_hidden && self.sessions.iter().any(|e| e.hidden);
        let empty_only = !self.show_empty
            && self
                .sessions
                .iter()
                .any(|e| (self.show_hidden || !e.hidden) && e.is_empty(active.as_deref()));
        let (title, detail) = match (hidden_only, empty_only) {
            (true, _) => {
                ("Every session here is hidden", "Turn on \u{201c}Show hidden\u{201d} in the Sessions menu above.")
            }
            (false, true) => {
                ("Only empty sessions here", "Turn on \u{201c}Show empty\u{201d} in the Sessions menu above.")
            }
            (false, false) => ("No sessions yet", "\u{2318}N starts one."),
        };
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
    /// avatar. A rail of two glyphs and an empty column served nothing
    /// (owner round 2026-09-13, follow-up).
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
            // tint anywhere (owner round 4, O4).
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
            // no reveal (owner round 6).
            this.resume_quiet(id.to_string(), window, cx);
        });
        let action = cx.listener(|this: &mut Self, name: &str, window, cx| match name {
            "new" => this.new_session(window, cx),
            // Task E has landed: the rail cell opens the full-text search
            // palette, like the header search icon and ⌘⇧F.
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
        let active = self.active_id(cx);
        let hidden = self.sessions.iter().filter(|e| e.hidden).count();
        let empty = self.sessions.iter().filter(|e| !e.archived && e.is_empty(active.as_deref())).count();
        let archived = self.sessions.iter().filter(|e| e.archived).count();
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
        // the menu inside the same deferred draw (owner round 2, P4).
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
        // the menu inside the same draw (owner round 2, P4).
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

/// The sidebar's wheel, taken in the capture phase (owner round 5 §A1):
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
fn sidebar_wheel_capture(harness: &Harness) -> gpui::AnyElement {
    // The shared input cell, not the entity: a wheel event can arrive
    // inside a `Harness` update (a scripted `sidebar-wheel:` step dispatches
    // from one), and updating the entity re-entrantly panics — the cell
    // never borrows it, so the push stays synchronous and N events between
    // paints coalesce into one drain. The pane is never borrowed during a
    // dispatch either (events dispatch outside updates and renders), so its
    // notify is direct too: ONLY the pane, never the root.
    let state = Rc::clone(&harness.sidebar_wheel);
    let pane = harness.sidebar_pane.clone();
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

/// The Sessions view menu's seat for a fixed caption (owner round 4 fixup):
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
    /// (owner round 6): 200 sessions across 8 projects through the
    /// harness's own grouping (flags, folds, rescue) into the library's
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
        let mut projects = Projects::default();
        let ids: Vec<String> = (0..8)
            .map(|i| projects.add(Path::new(&format!("/tmp/r6c-probe-{i}"))).id.clone())
            .collect();
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
                    local: false,
                    workspace: None,
                    project: Some(id.clone()),
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
        // The idle instrument's contract (owner round 6): `take_*` zeroes,
        // so the take after a window holds only that window's renders. The
        // counters are process-global and only the render paths note them,
        // which unit tests never reach — drain first to stay hermetic.
        take_harness_root_renders();
        take_sidebar_pane_renders();
        note_harness_render();
        note_harness_render();
        assert_eq!(take_harness_root_renders(), 2);
        assert_eq!(take_harness_root_renders(), 0);
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
    /// of dropping it on the first one (owner round 6, part C4 review #1):
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
