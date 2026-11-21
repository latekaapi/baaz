//! The sidebar column as its own view: what it draws, what it owns, and
//! what it asks the root for.
//!
//! [`SidebarPane`] owns the column's own state — the virtual list's scroll
//! position, what the pointer is over, the hover card's arm, the wheel's
//! accumulator, the one-shot reveal, the mode-switch fade, the Pinned
//! section's disclosure — and renders itself from three sources:
//!
//! - the [`SessionList`] model entity, which it subscribes to: a list
//!   change (a row born, a title landing, a pin) re-renders the column from
//!   the list's own event, with no root notify and no frame-time comparison;
//! - the layout global (grouping, sort, summary line, mode, collapse), which
//!   it observes;
//! - [`SidebarFacts`]: what only the root knows (the open session, the
//!   footer's account, the menus standing open, Browse mode's tabs), pushed
//!   by `Harness::compose_sidebar` without notifying the root. The pane
//!   re-renders when they differ from the last ones, never otherwise.
//!
//! It holds no handle to the root. What a person does in the column goes
//! out as a [`SidebarEvent`] the root maps (`Harness::on_sidebar_event`):
//! open, new, rename, pin, archive, project actions, mode switch and every
//! Browse action are the screen's own typed intents
//! ([`SidebarEvent::Intent`]).
//!
//! [`crate::sidebar`] next door is the pure half: rows in, a [`Grouping`] out.
//! The root's half — the facts, the intents' handlers, the hover card it
//! seats beside the column — is `app/sidebar_host.rs`.
//!
//! [`Grouping`]: aui::nav::Grouping

use std::collections::HashMap;
use std::rc::Rc;

use aui::nav::{
    SESSION_DETAIL_GAP, ensure_row_visible, flatten_sidebar, row_index_for_session, sidebar_list_state, GroupAction,
    SidebarRow,
};
use aui::screens::sidebar::{
    screen_item_count, sidebar_screen, Account, AccountStatus, BrowseIntent, BrowseMenu, BrowseMenuTarget, BrowseModel, Bylines,
    EditingState, GroupKey, Mode, NavItem, RowMenuOffers, SidebarIntent, SidebarModel, SortKey, ViewOptions,
};
use aui_icons::IconName;
use aui_tokens::AgentState;
use gpui::{
    div, prelude::*, px, Bounds, Context, Entity, EventEmitter, ListOffset, ListState, Pixels, Render, SharedString,
    Subscription, Task, Window,
};
use gpui_kit::base::input::TextareaState;

use crate::session_list::{ListFacts, SessionList, SessionListEvent};
use crate::sidebar::{Grouping, SessionEntry};
use crate::tier::Tier;
use crate::layout::LayoutGlobal as _;

/// How long the sidebar's new rows take to arrive after a mode switch.
pub(crate) const MODE_FADE: std::time::Duration = std::time::Duration::from_millis(80);

/// What only the root knows and the column draws, pushed by
/// `Harness::compose_sidebar` on every root render. The pane re-renders when
/// these differ from the last ones it drew, and only then: a root notify
/// that changed none of them leaves the cached column alone.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SidebarFacts {
    /// The open session, the awaited click, the drafts and the running
    /// tasks — what the list's visible rows are keyed on.
    pub(crate) list: ListFacts,
    /// The selected row: the chain head of the awaited click, else of the
    /// open session (one identity per chain, Y2a).
    pub(crate) selected: Option<String>,
    /// The current project: a rowless session's reveal falls back to its head.
    pub(crate) current_project: Option<String>,
    /// The footer and its menu (its usage cards only while it is open).
    pub(crate) account: Account,
    /// How many skills are on: the Skills row's count (D54).
    pub(crate) skills_on: usize,
    /// Whether the Skills row shows at all
    /// ([`crate::skills_page::skills_offered`]).
    pub(crate) skills_offered: bool,
    /// The Sessions caption's view menu stands open.
    pub(crate) view_menu_open: bool,
    /// The session whose row menu stands open (its `⋯`, or a right-click).
    pub(crate) row_menu: Option<String>,
    /// What that menu offers for the session (docs/82 8.5).
    pub(crate) row_menu_offers: RowMenuOffers,
    /// Where a right-click opened it; `None` under the row's `⋯`.
    pub(crate) row_menu_at: Option<gpui::Point<Pixels>>,
    /// The open Browse right-click menu: its row stays lit.
    pub(crate) browse_menu: Option<BrowseMenu>,
    /// The session row being renamed in place.
    pub(crate) renaming: Option<String>,
    /// Browse mode's rows, in Browse only (its naming draft left empty: the
    /// field draws itself, so typing never re-renders the column).
    pub(crate) browse: Option<BrowseModel>,
    /// The minute the grouping's elapsed tags read against: its rollover
    /// regroups.
    pub(crate) minute: i64,
}

impl Default for SidebarFacts {
    /// Nothing known yet: the root pushes its facts before the pane's first
    /// render (the root composes the shell before its children draw).
    fn default() -> Self {
        Self {
            list: ListFacts::default(),
            selected: None,
            current_project: None,
            account: Account {
                name: SharedString::default(),
                email: SharedString::default(),
                plan: SharedString::default(),
                initial: SharedString::default(),
                status: AccountStatus::Checking,
                menu_open: false,
                usage: Vec::new(),
            },
            skills_on: 0,
            skills_offered: false,
            view_menu_open: false,
            row_menu: None,
            row_menu_offers: RowMenuOffers::default(),
            row_menu_at: None,
            browse_menu: None,
            renaming: None,
            browse: None,
            minute: 0,
        }
    }
}

/// What the column asks the root for.
#[derive(Clone, Debug)]
pub(crate) enum SidebarEvent {
    /// One of the screen's intents the root acts on: open, new, rename,
    /// pin, archive, project actions, menus, and every Browse action.
    Intent(SidebarIntent),
    /// The person scrolled the column: a group row's menu would mis-seat.
    Scrolled,
    /// The hover card opened or closed: the root seats it beside the column.
    DetailChanged,
    /// A scripted wheel sweep advanced one tick: the root's frame trace
    /// takes its row.
    SweepTick,
}

/// The hover card's state: which row armed it, since when, whether it is
/// past its delay, and the seat. The root draws the card beside the column
/// (`Harness::render_row_detail`); the pane owns the arm.
#[derive(Debug, Default)]
pub(crate) struct RowDetail {
    /// The sidebar row under the pointer, if any: the hover detail's owner.
    /// Written only by the rows' own hover reports
    /// ([`SidebarPane::note_row_hover`]); pane renders never touch it.
    pub(crate) hovered_row: Option<String>,
    /// When the current hover started: the detail opens past
    /// [`aui::nav::SESSION_DETAIL_DELAY`], so a pointer travelling past
    /// rows never flashes the card.
    pub(crate) hover_since: Option<std::time::Instant>,
    /// The hover card is past its delay and showing.
    pub(crate) shown: bool,
    /// Where the showing card seats: the hovered row's own window bounds,
    /// carried by the row's hover report and frozen so the card never chases
    /// the mouse. The seat itself hangs off [`Self::pane_bounds`].
    pub(crate) trigger: Option<Bounds<Pixels>>,
    /// The laid-out pane's window bounds, from its own prepaint: what the
    /// hover card's side seat hangs off (the pane's right edge plus the
    /// card gap).
    pub(crate) pane_bounds: Option<Bounds<Pixels>>,
    /// Capture aid (`row-detail:<id>`): pins the hover card open for one
    /// row, seated at the selected row's bounds — free, no pointer.
    pub(crate) forced: Option<String>,
    /// The selected row's window bounds: what the pinned card seats at.
    pub(crate) selected_row_bounds: Option<(String, Bounds<Pixels>)>,
    /// Hover-trace state (`BAAZ_HOVER_TRACE=1`, diagnosis only): the last
    /// card result the root drew, so the trace logs only on change.
    pub(crate) trace_last_shown: Option<bool>,
}

/// The sidebar column's view.
pub(crate) struct SidebarPane {
    /// The rows: subscribed, so a list change re-renders the column.
    list: Entity<SessionList>,
    /// The inline rename field (shared with the header's project rename).
    rename: Entity<TextareaState>,
    /// What the root last pushed ([`SidebarFacts`]).
    facts: SidebarFacts,
    /// The divider's width and whatever drag is in flight over it: the
    /// pane owns it, so a drag redraws the column (and the shell around it,
    /// an ancestor) without the root (docs/82 4.9c).
    pub(crate) resize: crate::resize::ResizeDrag,
    /// The sessions list's scroll state: the caller-owned `ListState` the
    /// virtualised sidebar lays out through. Kept in sync with the
    /// flattened rows on every render by [`Self::sync_list`]: `reset` after
    /// a regroup or filter change, `splice` after a local insert or remove,
    /// `remeasure_items` after a text-only height change — `item_count`
    /// always equals the flattened length.
    pub(crate) list_state: ListState,
    /// What the pointer is over, as the screen reports it: a session id, a
    /// Browse tab key, or a reserved chrome key. The row's trailing slot
    /// swaps to its actions on it.
    pub(crate) hovered: Option<String>,
    /// The project head the pointer is over: it shows its actions.
    hovered_group: Option<String>,
    /// Whether the Projects header shows its actions (hovered).
    hovered_projects_header: bool,
    /// Whether Browse's Today header shows its actions (hovered).
    hovered_today_header: bool,
    /// Whether the Pinned section is open.
    pinned_open: bool,
    /// The flattened rows the list state was last synced to: the pane
    /// re-flattens per render (an index walk, no summaries cloned) and
    /// splices the difference, so a regroup is the only sync that drops
    /// the offset.
    pub(crate) prev_rows: Vec<SidebarRow>,
    /// The grouping those rows were flattened from, behind its cached
    /// `Rc`: a new pointer with equal rows means text changed under stable
    /// rows, which is what asks for `remeasure_items`.
    prev_grouping: Option<Rc<Grouping>>,
    /// The regroup key the list was last reset for.
    prev_regroup: Option<SidebarRegroupKey>,
    /// The wheel's input side: what a scripted sweep pushes and the render
    /// drains, once per frame.
    wheel: SidebarWheelState,
    /// Whether the user has scrolled the sidebar since the reveal was last
    /// armed. A reveal never moves a user-scrolled list: a wheel sets this,
    /// arming clears it, and no reveal installs while it holds.
    pub(crate) user_scrolled: bool,
    /// The pending "ensure visible" row id (`scrollIntoView({ block:
    /// "nearest" })` semantics). Armed only by outside-the-sidebar
    /// activations — the palette, New session / `+`, fork, boot
    /// `--session`, the `open:` step ([`Self::set_reveal`]) — and consumed
    /// once the row reports visible. A sidebar click never arms it (the
    /// clicked row is under the cursor, hence visible), and neither does a
    /// list refresh, a regroup, a user scroll or a resize drag.
    pub(crate) reveal: Option<String>,
    /// An id [`Self::reveal`] named that has no row yet, and how many
    /// consecutive reveal attempts it has missed on (owner round 6, part C4
    /// review #1): a draft before its first send, a fork before
    /// `load_sessions`'s reply lands. Distinct from "nowhere to go" (a
    /// known session whose landing row genuinely cannot be found), which
    /// still gives up on the first miss — this one waits up to
    /// [`REVEAL_UNKNOWN_FRAMES`] attempts for the row to be born.
    pub(crate) reveal_unknown: Option<(String, u32)>,
    /// When the last mode switch happened: the new rows fade in over 80 ms
    /// from it (FF's swap arrival).
    mode_fade_since: Option<std::time::Instant>,
    /// A frame-paced wheel sweep in flight (`sidebar-scroll-sweep:` step):
    /// the in-process fallback for a real `CGEvent` gesture the
    /// environment cannot deliver. Advanced once per pane render.
    scroll_sweep: Option<crate::resize::ScrollSweep>,
    /// The hover card's arm and seat ([`RowDetail`]).
    pub(crate) detail: RowDetail,
    /// Every project group row's `…` seat, keyed by group id: where the
    /// pointer stood when it opened the row's project menu. Entries are
    /// kept, so a menu opened for a since-scrolled group still seats.
    pub(crate) group_menu_bounds: HashMap<String, Bounds<Pixels>>,
    /// The hover card's delay timer; replacing it cancels the last one.
    hover_timer: Option<Task<()>>,
    /// The reveal's wake-up outside the draw (see [`Self::reveal_row`]).
    reveal_poke: Option<Task<()>>,
    /// The list subscription and the layout observer.
    _subscriptions: Vec<Subscription>,
    /// This pane's own renders, for the tests that prove what re-renders it
    /// (the process-wide counters above are shared across parallel tests).
    #[cfg(test)]
    pub(crate) renders: u64,
}

impl EventEmitter<SidebarEvent> for SidebarPane {}

/// Sidebar-pane renders since process start: the
/// sidebar analogue of `WHEEL_SCROLL_BYS`. Relaxed atomics, drained per
/// `sidebar-wheel:` log line, so a burst's pane/root renders per event are
/// readable off a scripted run.
static SIDEBAR_PANE_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
static BAAZ_ROOT_RENDERS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "probe")]
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

#[cfg(feature = "probe")]
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

/// How many consecutive reveal misses an id with no row yet
/// gets before the reveal gives up: a
/// bound, not a real deadline — the row-birth sites (the `turn/started`
/// local-row insert, a `load_sessions` reply) already invalidate the list,
/// so a normal wait is one or two attempts; this only guards against an id
/// that never arrives at all (a failed fork, a send that never lands).
pub(crate) const REVEAL_UNKNOWN_FRAMES: u32 = 120;

/// Steer the sidebar list toward row `ix`; returns whether it jumped.
///
/// `ensure_row_visible` (gpui's `scroll_to_reveal_item`) steers by the
/// heights the list has measured: a target far below everything measured
/// moved the top a few rows per frame, and the target itself read "not laid
/// out", so the reveal stalled with the row out of sight (an imported
/// session near the bottom of a 2,400-row Unfiled, QA 2026-10-30). Such a
/// target is placed at the top of the viewport by index instead, which
/// needs no measurement; the next frame measures it. Only once the list
/// has measured its top: before the first layout nothing is measured, and a
/// jump then put even a near row at the very top, hiding the rows above it
/// (its group header) — the measured steer handles that row next frame.
fn steer_to_row(list: &gpui::ListState, ix: usize) -> bool {
    let top = list.logical_scroll_top();
    let laid_out = list.viewport_bounds().size.height > px(0.0);
    let measured_top = list.bounds_for_item(top.item_ix).is_some();
    if laid_out && measured_top && ix > top.item_ix && list.bounds_for_item(ix).is_none() {
        list.scroll_to(gpui::ListOffset { item_ix: ix, offset_in_item: px(0.0) });
        return true;
    }
    ensure_row_visible(list, ix);
    false
}

/// The bookkeeping [`SidebarPane::reveal_row`] does for an unknown
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

/// Whether a missed reveal stays armed: only while its id is the open
/// session's and the window holds no row for it yet (a brand-new session
/// before its first send). Pure, so tests pin the boundary without a
/// window; [`SidebarPane::reveal_row`] reads it on every miss.
pub(crate) fn rowless_reveal_stays_armed(
    active_id: Option<&str>,
    reveal_id: &str,
    known: bool,
) -> bool {
    active_id == Some(reveal_id) && !known
}

/// The group head (or header) row for a session with no session row: a
/// closed group holds its sessions out of the flattened model. Pure — the
/// pane passes the list's stores — so tests drive each trigger's scroll
/// target without a window.
///
/// A rowless session still names its project: the override Baaz files for
/// a draft before its first send, then the provider-lane record, then the
/// current project (a new session starts in it) — before giving up. The
/// head is revealed, never auto-expanded.
pub(crate) fn reveal_head_row(
    sessions: &[SessionEntry],
    overrides: &crate::sessions::Overrides,
    current_project: Option<&str>,
    rows: &[SidebarRow],
    grouping: &Grouping,
    reveal_id: &str,
) -> Option<usize> {
    match grouping {
        Grouping::Project(groups) => {
            let project = crate::sidebar::rowless_session_project(
                reveal_id,
                sessions,
                overrides,
            )
            .or_else(|| current_project.map(str::to_owned))?;
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

/// Sidebar drains actually applied since process start:
/// one per frame that had accumulated travel, against one offset write per
/// event before. Drained per `sidebar-wheel:` line, like `take_wheel_scroll_bys`.
static SIDEBAR_WHEEL_DRAINS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(feature = "probe")]
/// Drain the applied-drain count above, for the `sidebar-wheel:` report.
pub(crate) fn take_sidebar_wheel_drains() -> u64 {
    SIDEBAR_WHEEL_DRAINS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The sidebar wheel's input-side state: what a scripted sweep pushes and
/// the pane's render applies. Plain state on the pane: the push is a pane
/// update, which never re-enters the root.
#[derive(Debug, Default)]
pub(crate) struct SidebarWheelState {
    /// Wheel travel accumulated since the last sidebar frame, in pixels.
    /// The push only adds; the drain takes it whole.
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

/// What a regroup or filter change looks like to the virtual list (owner
/// round 6): the grouping mode and the two list-management toggles.
/// [`SidebarPane::sync_list`] resets the list state when this changes
/// and splices everything else, so group open/close and fold expand keep
/// the offset.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct SidebarRegroupKey {
    group_by: crate::layout::GroupBy,
    sort_by: crate::layout::SortBy,
    show_empty: bool,
    show_archived: bool,
    /// The summary line changes every session row's height: the list's
    /// measured heights are stale, so it resets like a regroup.
    summary_line: bool,
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

impl SidebarPane {
    /// The id the pane hands the virtual list.
    pub(crate) const SIDEBAR_VIEW_ID: &'static str = "sessions";

    /// The column over `list`, typing renames in `rename`, `width` wide
    /// (what `layout.json` restored).
    pub(crate) fn new(list: Entity<SessionList>, rename: Entity<TextareaState>, width: f32, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            // A list change re-renders the column from the list's own
            // event. An event, not a notify: gpui-pre drops an observer's
            // notify raised inside a draw, while an event always reaches
            // its subscribers once the draw is over.
            cx.subscribe(&list, |_, _, _: &SessionListEvent, cx| cx.notify()),
            // The window preferences the column draws (grouping, sort,
            // summary line, mode, collapse) live in the layout global: a
            // change re-renders the cached pane with no notify at the write.
            cx.observe_global::<crate::layout::LayoutStore>(|_, cx| cx.notify()),
        ];
        Self {
            list,
            rename,
            facts: SidebarFacts::default(),
            resize: crate::resize::ResizeDrag::restored(width),
            list_state: sidebar_list_state(0),
            hovered: None,
            hovered_group: None,
            hovered_projects_header: false,
            hovered_today_header: false,
            pinned_open: true,
            prev_rows: Vec::new(),
            prev_grouping: None,
            prev_regroup: None,
            wheel: SidebarWheelState::default(),
            user_scrolled: false,
            reveal: None,
            reveal_unknown: None,
            mode_fade_since: None,
            scroll_sweep: None,
            detail: RowDetail::default(),
            group_menu_bounds: HashMap::new(),
            hover_timer: None,
            reveal_poke: None,
            _subscriptions: subscriptions,
            #[cfg(test)]
            renders: 0,
        }
    }

    /// Take what the root knows this frame. Never notifies the root; it
    /// notifies the pane only when the facts differ from the last ones —
    /// answering whether they did. Called from the root's render: a notify
    /// raised inside the draw lands on the next frame, so the caller asks
    /// for one.
    pub(crate) fn push_facts(&mut self, facts: SidebarFacts, cx: &mut Context<Self>) -> bool {
        if self.facts == facts {
            return false;
        }
        self.facts = facts;
        cx.notify();
        true
    }

    /// Arm the one-shot reveal for `id`, an outside activation, or clear
    /// any armed one (`None`: a quiet click). A `fresh` arm owns the list
    /// again: the next user scroll disarms it.
    pub(crate) fn set_reveal(&mut self, id: Option<String>, fresh: bool, cx: &mut Context<Self>) {
        if id.is_none() && self.reveal.is_none() && self.reveal_unknown.is_none() {
            return;
        }
        if fresh && id.is_some() {
            self.user_scrolled = false;
        }
        self.reveal = id;
        self.reveal_unknown = None;
        cx.notify();
    }

    /// The press on the divider: arm the drag from the grab point. A drag
    /// owns the list like a wheel gesture does: an armed reveal is
    /// disarmed, and none installs while it is in flight.
    pub(crate) fn begin_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        self.user_owns_list();
        self.resize.begin(x);
        cx.notify();
    }

    /// A move with the button held: the divider follows, clamped.
    pub(crate) fn drag_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if self.resize.drag_to(x) {
            cx.notify();
        }
    }

    /// The release, wherever it lands: disarm, settle, persist. The write
    /// is a layout-global update, whose observers (this pane, the root)
    /// redraw.
    pub(crate) fn end_resize(&mut self, cx: &mut Context<Self>) {
        if self.resize.end() {
            self.resize.persist(cx);
        }
    }

    /// The window went inactive mid-drag: a release outside it never reaches
    /// the overlay, so a pointer drag settles where it stands (a scripted
    /// one has no pointer, and only its own `resize-end` settles it).
    pub(crate) fn settle_resize_on_deactivate(&mut self, cx: &mut Context<Self>) {
        if self.resize.active && !self.resize.scripted {
            self.resize.active = false;
            self.resize.persist(cx);
        }
    }

    #[cfg(feature = "probe")]
    /// `sidebar-width:` — a width settled like a released drag, minus the
    /// pointer (persisted: the layout global's observers redraw).
    pub(crate) fn settle_width(&mut self, width: f32, cx: &mut Context<Self>) {
        self.resize.width = aui::shell::clamp_sidebar_width(width);
        self.resize.active = false;
        self.resize.persist(cx);
    }

    #[cfg(feature = "probe")]
    /// `resize-begin` steps: the drag has no pointer behind it.
    pub(crate) fn mark_resize_scripted(&mut self) {
        self.resize.scripted = true;
    }

    #[cfg(feature = "probe")]
    /// Start a frame-paced width sweep (`resize-sweep:`): it owns the list
    /// like a drag and never persists.
    pub(crate) fn start_resize_sweep(&mut self, sweep: crate::resize::ResizeSweep, cx: &mut Context<Self>) {
        self.user_owns_list();
        self.resize.sweep = Some(sweep);
        self.resize.active = true;
        self.resize.scripted = true;
        cx.notify();
    }

    #[cfg(feature = "probe")]
    /// One sweep tick: the divider marches one step. Answers the new width
    /// and whether the sweep is over (at the target, when the clamp stops
    /// progress, or past any reasonable drag), or `None` when no sweep
    /// runs. Over, it settles without persisting.
    pub(crate) fn tick_resize_sweep(&mut self, cx: &mut Context<Self>) -> Option<(f32, bool)> {
        let from = self.resize.width;
        let sweep = self.resize.sweep.as_mut()?;
        let next = sweep.advance(from);
        let done = next == from || next == sweep.target || sweep.ticks > 10_000;
        self.resize.width = next;
        if done {
            self.resize.sweep = None;
            self.resize.active = false;
            self.resize.scripted = false;
        }
        cx.notify();
        Some((next, done))
    }

    /// The person owns the list (a divider drag, a scripted sweep): any
    /// armed reveal is disarmed and none reinstalls until the next arm.
    pub(crate) fn user_owns_list(&mut self) {
        self.reveal = None;
        self.reveal_unknown = None;
        self.user_scrolled = true;
    }

    /// A provisional session id became a real one: an armed reveal follows it.
    pub(crate) fn rekey_reveal(&mut self, old: &str, new: &str) {
        if self.reveal.as_deref() == Some(old) {
            self.reveal = Some(new.to_owned());
        }
        if let Some((id, _)) = self.reveal_unknown.as_mut().filter(|(id, _)| id == old) {
            *id = new.to_owned();
        }
    }

    /// The sessions list's current scroll position: what `sidebar-wheel:`
    /// and the resize drag log sample. The list walks down as `item_ix`
    /// grows, with `offset_in_item` the pixels into that row — the sidebar
    /// analogue of `SessionView::bench_list_top`.
    pub(crate) fn list_top(&self) -> ListOffset {
        self.list_state.logical_scroll_top()
    }

    /// Whether a sidebar wheel gesture is in flight: an event landed within
    /// the horizon. While this holds the pane presents every tick and no
    /// reveal installs.
    pub(crate) fn gesture_active(&self) -> bool {
        self.wheel.gesture_until.is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Start the mode switch's fade: the new rows arrive over [`MODE_FADE`].
    /// No notify: the switch itself is a layout write, which the pane
    /// observes.
    pub(crate) fn start_mode_fade(&mut self, at: std::time::Instant) {
        self.mode_fade_since = Some(at);
    }

    #[cfg(feature = "probe")]
    /// Start a frame-paced wheel sweep (`sidebar-scroll-sweep:`).
    pub(crate) fn start_scroll_sweep(&mut self, sweep: crate::resize::ScrollSweep, cx: &mut Context<Self>) {
        self.scroll_sweep = Some(sweep);
        cx.notify();
    }

    /// Push one sweep delta into the accumulator: the same three writes a
    /// real wheel gesture makes — accumulate, re-arm the gesture horizon,
    /// mark scrolled — so a sweep tick is indistinguishable from a real
    /// wheel event to everything downstream (the drain, the reveal disarm,
    /// the trace).
    fn push_wheel(&mut self, dy: f32) {
        self.wheel.pending += px(dy);
        self.wheel.gesture_until = Some(std::time::Instant::now() + SIDEBAR_GESTURE_HORIZON);
        self.wheel.scrolled = true;
    }

    /// Advance a scripted sweep one tick, asking for the next frame while it
    /// runs; the root's frame trace takes its row per tick.
    fn advance_sweep(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sweep) = self.scroll_sweep.as_mut() else { return };
        match sweep.advance() {
            Some(dy) => {
                self.push_wheel(dy);
                aui_motion::request_frame(window);
                cx.emit(SidebarEvent::SweepTick);
            }
            None => self.scroll_sweep = None,
        }
    }

    /// A real wheel event over the sidebar: the person scrolled. Disarms any
    /// armed reveal and marks the list user-scrolled until the next
    /// activation, and dismisses the group menu and the hover card.
    pub(crate) fn note_wheel(&mut self, cx: &mut Context<Self>) {
        self.reveal = None;
        self.reveal_unknown = None;
        self.user_scrolled = true;
        cx.emit(SidebarEvent::Scrolled);
        self.close_row_detail(cx);
    }

    /// Apply the wheel's input: take the accumulated travel into exactly
    /// one `ListState::scroll_by` per frame, disarm any armed reveal (the
    /// user's scroll wins), and mark scrolled-since-armed so none reinstalls
    /// until the next activation. Called once per pane render, before the
    /// list lays out — the sidebar twin of `SessionView::drain_pending_wheel`.
    /// Returns whether the user scrolled this frame: a scroll dismisses an
    /// open group menu (the calm option) and is what the `sbwheel` probe
    /// line reports.
    fn drain_wheel(&mut self) -> bool {
        let pending = std::mem::replace(&mut self.wheel.pending, px(0.0));
        let scrolled = std::mem::replace(&mut self.wheel.scrolled, false);
        if scrolled {
            self.reveal = None;
            self.reveal_unknown = None;
            self.user_scrolled = true;
        }
        if pending == px(0.0) {
            return scrolled;
        }
        // The transcript's sign: a positive wheel delta climbs toward the
        // head, so the pixel walk takes it negated.
        self.list_state.scroll_by(-pending);
        SIDEBAR_WHEEL_DRAINS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        scrolled
    }

    /// Every intent the screen reports. What is the column's own — hover,
    /// the Pinned disclosure, the hover card, a group menu's seat — it
    /// settles here; everything else goes to the root as
    /// [`SidebarEvent::Intent`].
    pub(crate) fn intent(&mut self, intent: SidebarIntent, window: &mut Window, cx: &mut Context<Self>) {
        // What the pointer is over is the column's alone: settled here,
        // never sent on, and a redraw only when it moved. `(redraw, mine)`.
        let (redraw, mine) = match &intent {
            SidebarIntent::Hover(id, bounds) => {
                let id = id.as_ref().map(|id| id.to_string());
                let moved = self.hovered != id;
                if moved {
                    // The hover card follows session rows only: leaving one
                    // disarms it, entering one arms the delay with the row's
                    // bounds (the card seats beside the pane at its top).
                    if let Some(previous) = self.hovered.take() {
                        self.note_row_hover(previous, false, None, cx);
                    }
                    if let Some(next) = id.clone().filter(|id| self.list.read(cx).has_row(id)) {
                        self.note_row_hover(next, true, *bounds, cx);
                    }
                    self.hovered = id;
                }
                (moved, true)
            }
            SidebarIntent::HoverGroup(id) => {
                let id = id.as_ref().map(|id| id.to_string());
                (std::mem::replace(&mut self.hovered_group, id.clone()) != id, true)
            }
            SidebarIntent::HoverProjectsHeader(on) => {
                (std::mem::replace(&mut self.hovered_projects_header, *on) != *on, true)
            }
            SidebarIntent::Browse(BrowseIntent::HoverTodayHeader(on)) => {
                (std::mem::replace(&mut self.hovered_today_header, *on) != *on, true)
            }
            // The Pinned disclosure is the column's own; the root still
            // hears the intent (any intent but the menus' closes them).
            SidebarIntent::TogglePinned => {
                self.pinned_open = !self.pinned_open;
                (true, false)
            }
            // A click is the row's, not the card's: it dismisses the hover
            // card, and the scripted pin with it.
            SidebarIntent::Select(_) | SidebarIntent::RowAction(..) => {
                self.close_row_detail(cx);
                self.detail.forced = None;
                (false, false)
            }
            // The screen reports no bounds for a group head's `…`; the
            // pointer stands on it at the click, so its project menu seats
            // there (a keyboard open seats wherever the pointer is).
            SidebarIntent::GroupAction(id, GroupAction::Menu) => {
                let at = window.mouse_position();
                self.group_menu_bounds.insert(id.to_string(), Bounds::new(at, gpui::size(px(1.0), px(1.0))));
                (false, false)
            }
            _ => (false, false),
        };
        if redraw {
            cx.notify();
        }
        if !mine {
            cx.emit(SidebarEvent::Intent(intent));
        }
    }

    #[cfg(feature = "probe")]
    /// `sidebar-hover:<id>`: stand the pointer state on `id`'s row, as its
    /// own hover report would (the hover card is not armed). An id with no
    /// session fails the step.
    pub(crate) fn hover_step(&mut self, id: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if !self.list.read(cx).has_row(id) {
            return Err(format!("no session row `{id}`"));
        }
        self.hovered = Some(id.to_owned());
        cx.notify();
        Ok(())
    }

    /// Fold one row hover report into the hover card's state: entering a
    /// new row arms the card's delay and seats it from the row's own bounds
    /// — never the pointer — leaving the armed row closes it, and anything
    /// else changes nothing. This is the only writer: pane renders never
    /// touch the hover state, so a render can neither arm nor disarm a row.
    /// Entering never redraws (nothing shows until the delay elapses); the
    /// timer says [`SidebarEvent::DetailChanged`] when the card opens, and
    /// leaving says it when an open card closes — the root draws the card.
    pub(crate) fn note_row_hover(
        &mut self,
        id: String,
        hovered: bool,
        row: Option<Bounds<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if self.detail.hovered_row.as_deref() == Some(id.as_str()) {
                crate::hover_trace!("note id={id} entered already-armed");
                return;
            }
            let Some(row) = row else {
                crate::hover_trace!("note id={id} entered no-bounds");
                return;
            };
            // The seated anchor the card opens at: the sidebar's right edge
            // plus the card gap, top-aligned with the row.
            match row_detail_seat(&self.detail.pane_bounds, &row) {
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
            self.detail.hovered_row = Some(id.clone());
            self.detail.hover_since = Some(std::time::Instant::now());
            self.detail.shown = false;
            self.detail.trigger = Some(row);
            self.hover_timer = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(aui::nav::SESSION_DETAIL_DELAY).await;
                let _ = this.update(cx, |pane, cx| {
                    let matches = pane.detail.hovered_row.as_deref() == Some(&id);
                    if matches && !pane.detail.shown {
                        pane.detail.shown = true;
                        cx.emit(SidebarEvent::DetailChanged);
                    }
                    crate::hover_trace!("timer src=note id={id} matches={matches} shown={}", pane.detail.shown);
                });
            }));
            return;
        }
        if self.detail.hovered_row.as_deref() == Some(id.as_str()) {
            crate::hover_trace!("note id={id} left close shown={}", self.detail.shown);
            self.close_row_detail(cx);
            cx.emit(SidebarEvent::DetailChanged);
        } else {
            crate::hover_trace!("note id={id} left ignored armed={:?}", self.detail.hovered_row);
        }
    }

    /// Close the hover card: pointer left, wheel moved, row clicked, card
    /// clicked. The scripted pin ([`RowDetail::forced`]) is not hover state
    /// and survives this; clicks clear it where they land. A card that was
    /// showing says so, so the root redraws without it.
    pub(crate) fn close_row_detail(&mut self, cx: &mut Context<Self>) {
        let was_shown = self.detail.shown;
        self.detail.hovered_row = None;
        self.detail.hover_since = None;
        self.detail.shown = false;
        self.detail.trigger = None;
        self.hover_timer = None;
        if was_shown {
            cx.emit(SidebarEvent::DetailChanged);
        }
    }

    /// What a regroup or filter change looks like to the virtual list: the
    /// mode and the two list-management toggles. Group open/close and
    /// fold expand are NOT in the key — they splice like any other local
    /// insert or remove, so the offset survives them. Compared on every
    /// render before anything draws; a change resets the list state to the
    /// flattened length.
    fn regroup_key(&self, cx: &gpui::App) -> SidebarRegroupKey {
        let list = self.list.read(cx);
        SidebarRegroupKey {
            group_by: SessionList::effective_group_by(cx),
            sort_by: crate::layout::Layout::get(cx).sort_by,
            show_empty: list.show_empty,
            show_archived: list.show_archived,
            summary_line: crate::layout::Layout::get(cx).summary_line,
        }
    }

    /// Keep the caller-owned list state on the flattened rows: re-flattened
    /// per render; `reset` after a regroup or filter change (the one sync
    /// that drops the offset — the old offset is meaningless against a
    /// rebuilt model), `splice` after a local insert or remove (open/close,
    /// fold expand, sessions arriving or leaving), `remeasure_items` after a
    /// text-only height change (same rows, new grouping pointer). A mismatch
    /// never panics in the list — it paints blanks past the model — so the
    /// debug assertion below is the contract, not a guardrail.
    pub(crate) fn sync_list(&mut self, rows: &[SidebarRow], grouping: &Rc<Grouping>, cx: &gpui::App) {
        // The splice below tracks the top item by index, so rows inserted
        // above carry a steered or parked top away from its row; repaired
        // after the sync, once the new rows are in.
        let top_before = self.list_state.logical_scroll_top();
        // The render bends the count to the screen's skeleton rows while
        // loading; put the state back on the previous flattened rows so the
        // splice below diffs against what the list really holds.
        if self.list_state.item_count() != self.prev_rows.len() {
            self.list_state.reset(self.prev_rows.len());
        }
        let key = self.regroup_key(cx);
        if self.prev_regroup.as_ref() != Some(&key) {
            self.list_state.reset(rows.len());
            self.prev_regroup = Some(key);
        } else if rows != self.prev_rows.as_slice() {
            // The common prefix and suffix stay; the middle was swapped.
            // Moves degrade to remove-plus-insert, which still lands the
            // count exactly — only the preserved offset approximates.
            let prev = &self.prev_rows;
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
            self.list_state.splice(prefix..prev.len() - suffix, rows.len() - prefix - suffix);
        } else if self.prev_grouping.as_ref().is_none_or(|cached| !Rc::ptr_eq(cached, grouping)) {
            // Same rows, rebuilt grouping: text moved under stable rows
            // (a rename, a new left line, a branch appearing), so cached
            // heights may lie. Remeasure the window the list asks for.
            if !rows.is_empty() {
                self.list_state.remeasure_items(0..rows.len());
            }
        }
        self.prev_rows = rows.to_vec();
        self.prev_grouping = Some(Rc::clone(grouping));
        debug_assert_eq!(
            self.list_state.item_count(),
            rows.len(),
            "sidebar list out of sync: {} items for {} rows",
            self.list_state.item_count(),
            rows.len()
        );
        self.repair_scroll(top_before, rows, grouping, cx);
    }

    /// Whether the list may move on its own: never under a user scroll, a
    /// gesture or a divider drag.
    fn list_is_free(&self) -> bool {
        !self.user_scrolled && !self.gesture_active() && !self.resize.active
    }

    /// Re-anchor the scroll position after [`Self::sync_list`] spliced:
    /// gpui's splice keeps the top *item by index*, so rows the staged boot
    /// loads insert above carry a steered or parked top away from where it
    /// belongs. While the user has not scrolled, an armed reveal re-steers
    /// to its row by id (a reveal for an id with no row waits without moving
    /// anything), and a top-parked list with nothing to steer to stays
    /// parked at row 0. A user-scrolled, gesture-driven or resizing list is
    /// never touched — same gate as the render.
    fn repair_scroll(&mut self, before: ListOffset, rows: &[SidebarRow], grouping: &Grouping, cx: &gpui::App) {
        if !self.list_is_free() {
            return;
        }
        if let Some(reveal_id) = self.reveal.clone() {
            let wanted = SharedString::from(reveal_id.as_str());
            let target = row_index_for_session(rows, grouping, &wanted)
                .or_else(|| self.head_row(rows, grouping, &reveal_id, cx));
            if let Some(ix) = target {
                steer_to_row(&self.list_state, ix);
                return;
            }
            // No row (yet): the wait itself must not move the list, nor let
            // the splice carry a parked top away under it.
        }
        if before.item_ix == 0 && before.offset_in_item == px(0.0) {
            let after = self.list_state.logical_scroll_top();
            if after.item_ix != 0 || after.offset_in_item != px(0.0) {
                ensure_row_visible(&self.list_state, 0);
            }
        }
    }

    /// The group head (or header) row for a session with no session row: a
    /// closed group holds its sessions out of the flattened model. Mirrors
    /// the old current-group fallback — reveal the head, never auto-expand.
    fn head_row(&self, rows: &[SidebarRow], grouping: &Grouping, reveal_id: &str, cx: &gpui::App) -> Option<usize> {
        let list = self.list.read(cx);
        reveal_head_row(
            &list.sessions,
            &list.overrides,
            self.facts.current_project.as_deref(),
            rows,
            grouping,
            reveal_id,
        )
    }

    /// Steer the virtual list to the outside-activated session, moving the
    /// least distance that shows its row whole: the row itself when the
    /// flattened model has one — folds rescue held-back rows for the
    /// pending target, so no expansion dance — else its group head for a
    /// closed group (never auto-expanded, like before) or for a rowless
    /// session whose project resolves past the rows (a draft before its
    /// first send reveals its project head), else wait for an id with no
    /// row yet (a fork before `load_sessions`'s reply lands) up to
    /// [`REVEAL_UNKNOWN_FRAMES`] attempts, else clear a flag with nowhere
    /// to go (a *known* session that still has no landing row gives up on
    /// the first miss, same as before — only an unrecognised id gets the
    /// grace period; the rowless *active* session never gives up while it
    /// stays open). A sidebar click never sets the flag; wheel and resize
    /// disarm it. Called on every render while armed, until the row reports
    /// visible once the rows have settled (`sessions_loaded`): before the
    /// first layout the viewport is unknown and the steer is a no-op, and a
    /// fully visible row moves nothing.
    fn reveal_row(&mut self, rows: &[SidebarRow], grouping: &Grouping, reveal_id: &str, cx: &mut Context<Self>) {
        let wanted = SharedString::from(reveal_id);
        // A pinned session sits in the Pinned section above the list, always
        // in view: its reveal is done, and its project head is never the
        // target (it used to land there, the row being lifted out).
        let pinned = self.list.read(cx).pinned(&self.facts.list, cx);
        if pinned.iter().any(|row| row.id == wanted) {
            self.reveal = None;
            self.reveal_unknown = None;
            return;
        }
        let target = row_index_for_session(rows, grouping, &wanted)
            .or_else(|| self.head_row(rows, grouping, reveal_id, cx));
        let Some(ix) = target else {
            // The rowless active session keeps its arm: a brand-new session
            // has no row until its first send, and the reveal falls back to
            // its project head only once the head resolves — disarming here
            // would strand the scroll-follow before the row is born. Any
            // other miss disarms or streaks as before; a later switch, a
            // quiet click or a user scroll still clears the arm.
            let known = self.list.read(cx).has_row(reveal_id);
            if rowless_reveal_stays_armed(self.facts.list.active.as_deref(), reveal_id, known) {
                self.reveal_unknown = None;
                return;
            }
            if known {
                // A known session with nowhere to go: a waiting flag
                // outlives its activation and can move a list the user
                // scrolled meanwhile.
                self.reveal = None;
                self.reveal_unknown = None;
                return;
            }
            // An id this window has never listed: most likely its row is
            // simply not born yet. The sites that create it (the
            // `turn/started` local-row insert, a `load_sessions` reply)
            // invalidate the list, which brings this function around again
            // with fresh rows — so waiting here costs nothing extra, just a
            // bound so a reveal for an id that never arrives (a failed
            // fork, a send that never lands) cannot spin forever.
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
        let steers = self.list_state.item_is_above_viewport(ix) == Some(true)
            || self.list_state.item_is_below_viewport(ix) == Some(true);
        if steers {
            cx.emit(SidebarEvent::Scrolled);
        }
        let jumped = steer_to_row(&self.list_state, ix);
        let above = self.list_state.item_is_above_viewport(ix);
        let below = self.list_state.item_is_below_viewport(ix);
        if above == Some(false) && below == Some(false) {
            // The row is on screen — but while the first `session/list` is
            // still landing, later stages reindex around it and the splice
            // carries the steered top away (see `repair_scroll`, which
            // tracks the row by id meanwhile): stay armed until the rows
            // settle. No poke meanwhile — nothing is outstanding.
            if self.list.read(cx).sessions_loaded {
                self.reveal = None;
            }
        } else if (above.is_none() || below.is_none()) && !jumped {
            // The list has not laid out yet: a degenerate zero-height
            // sidebar rect would otherwise have every pane render spawn
            // another poke, which renders, which pokes again, forever —
            // `bench-idle` never settling. Leave the flag armed with nothing
            // scheduled; whatever eventually lays the list out (a resize, a
            // normal render) brings this function around again.
        } else {
            // The steer above only stages the list's pending scroll: no
            // frame applies it on its own (a cached pane runs none), and a
            // notify from inside the draw schedules nothing — so wake the
            // pane from a task outside the draw. The poke stops the frame
            // the row reports visible (the flag consumes above), so a
            // settled list never spins.
            self.reveal_poke = Some(cx.spawn(async move |this, cx| {
                let _ = this.update(cx, |_, cx| cx.notify());
            }));
        }
    }

    /// The screen's model: the grouping and pinned rows from the list, the
    /// nav rows, the selection and rename, the hover facts the intents
    /// write, the view options from the list's filters, the account and
    /// Browse mode's rows from the root's facts.
    pub(crate) fn model(&self, cx: &gpui::App) -> SidebarModel {
        let list = self.list.read(cx);
        let facts = &self.facts;
        let layout = crate::layout::Layout::get(cx);
        let editing = facts.renaming.clone().map(|session| EditingState {
            session: session.into(),
            draft: self.rename.read(cx).value().to_string().into(),
        });
        // The Skills row wears the on-count (D54): how many skills are on;
        // the screen draws none at zero (docs/83 §4). It shows only while the
        // Skills page works ([`crate::skills_page::skills_offered`], rule
        // 13). Automations is gone until the feature exists (plan stage P).
        let mut nav = vec![NavItem { id: NAV_PROJECTS.into(), label: "Add project".into(), icon: IconName::Folder, count: None, active: false }];
        if facts.skills_offered {
            nav.push(NavItem { id: NAV_SKILLS.into(), label: "Skills".into(), icon: IconName::Slash, count: Some(facts.skills_on as u32), active: false });
        }
        // "Needs you" (task 3.4): first among the nav rows while anything
        // waits on the person, and while its filter is on (so it can be
        // turned off when the last one is answered).
        let waiting = crate::attention::waiting(list.visible(&facts.list, cx).iter()).len();
        let filter = crate::attention::Notifier::filter_on(cx);
        if waiting > 0 || filter {
            nav.insert(0, NavItem { id: NAV_NEEDS_YOU.into(), label: "Needs you".into(), icon: IconName::Bell, count: Some(waiting as u32), active: filter });
        }
        let group_by = match SessionList::effective_group_by(cx) {
            crate::layout::GroupBy::Project => GroupKey::Project,
            crate::layout::GroupBy::Date => GroupKey::Date,
        };
        let sort_by = match layout.sort_by {
            crate::layout::SortBy::Recent => SortKey::Recent,
            crate::layout::SortBy::Created => SortKey::Created,
            crate::layout::SortBy::Name => SortKey::Name,
        };
        let mode = match layout.mode {
            crate::layout::Mode::Sessions => Mode::Sessions,
            crate::layout::Mode::Browse => Mode::Browse,
        };
        // Browse's rows as the root pushed them, with what the column owns:
        // the Today header's hover and the naming field's live draft.
        let browse = (mode == Mode::Browse).then(|| facts.browse.clone()).flatten().map(|mut browse| {
            browse.hovered_today_header = self.hovered_today_header;
            if let Some(editing) = browse.editing.as_mut() {
                editing.draft = self.rename.read(cx).value().to_string().into();
            }
            browse
        });
        SidebarModel {
            mode,
            search: None,
            nav,
            pinned: (*list.pinned(&facts.list, cx)).clone(),
            pinned_open: self.pinned_open,
            grouping: list.grouping(&facts.list, cx),
            selected: facts.selected.clone().map(SharedString::from),
            // The row a right-click menu belongs to stays lit while it is open
            // (canvas Menus board).
            hovered: match facts.browse_menu.as_ref().map(|menu| &menu.target) {
                Some(BrowseMenuTarget::Tab { id, .. }) if mode == Mode::Browse => Some(id.clone()),
                Some(BrowseMenuTarget::Folder(id)) if mode == Mode::Browse => Some(format!("folder-{id}").into()),
                Some(BrowseMenuTarget::TaskFolder { session, .. }) if mode == Mode::Browse => {
                    Some(format!("folder-{session}").into())
                }
                // A session row's menu (its `⋯` or a right-click) keeps its
                // row lit too, wherever the pointer went (docs/82 8.5).
                _ if mode == Mode::Sessions && facts.row_menu.is_some() => facts.row_menu.clone().map(SharedString::from),
                _ => self.hovered.clone().map(SharedString::from),
            },
            hovered_group: self.hovered_group.clone().map(SharedString::from),
            hovered_projects_header: self.hovered_projects_header,
            highlighted: None,
            editing,
            row_menu: facts.row_menu.clone().map(SharedString::from),
            row_menu_offers: facts.row_menu_offers.clone(),
            row_menu_at: facts.row_menu_at,
            loading: list.loading(),
            view_menu_open: facts.view_menu_open,
            view_options: ViewOptions {
                group_by,
                sort_by,
                show_empty: list.show_empty,
                show_archived: list.show_archived,
                search_all_projects: layout.search_all_projects,
            },
            // Settings › Sidebar › "Show a summary line" (docs/83 §4).
            bylines: if layout.summary_line { Bylines::Summary } else { Bylines::Off },
            account: facts.account.clone(),
            browse,
        }
    }
}

impl Render for SidebarPane {
    /// The sidebar column as the library's v2 screen (D3): a
    /// [`SidebarModel`] built on every pane render, the screen's intents
    /// routed through [`SidebarPane::intent`]. The pane owns the one
    /// `ListState` the screen renders through, created once and kept; it
    /// resets on a regroup ([`SidebarPane::sync_list`]) and matches
    /// `screen_item_count` otherwise.
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        SIDEBAR_PANE_RENDERS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(test)]
        {
            self.renders += 1;
        }
        if crate::session::frame_trace_enabled() {
            TRACE_PANE_TICKS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
        // One travel per frame (the scripted sweep feeds the accumulator;
        // a real wheel scrolls the library list directly).
        self.advance_sweep(window, cx);
        let user_scrolled = self.drain_wheel();
        if user_scrolled {
            cx.emit(SidebarEvent::Scrolled);
            self.close_row_detail(cx);
        }
        if self.gesture_active() {
            aui_motion::request_frame(window);
        }
        static FIRST_ROWS_DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        let (visible_len, loading) = {
            let list = self.list.read(cx);
            (list.visible(&self.facts.list, cx).len(), list.loading())
        };
        if !loading && visible_len > 0 && !FIRST_ROWS_DONE.swap(true, std::sync::atomic::Ordering::Relaxed) {
            crate::log::boot_mark(&format!("first-rows-painted rows={visible_len}"));
        }
        // The cached grouping (pinned rows lifted out of it) and its
        // flattened rows: the caller-owned list state is synced to their
        // length — `reset` after a regroup, `splice` after a local insert.
        let grouping = self.list.read(cx).grouping(&self.facts.list, cx);
        let rows = flatten_sidebar(&grouping, false);
        let model = self.model(cx);
        // Browse draws no session list: leave its state (and its scroll
        // offset) exactly as Sessions left it, so switching back lands where
        // the person was. Sessions syncs it to the rows; while loading the
        // screen draws skeleton rows the grouping does not have, and the
        // state follows the screen's count.
        if model.mode == Mode::Sessions {
            self.sync_list(&rows, &grouping, cx);
            let count = screen_item_count(&model);
            if self.list_state.item_count() != count {
                self.list_state.reset(count);
            }
        }
        // The one-shot reveal: outside activations only, never during a
        // gesture, a user scroll or a resize drag.
        if model.mode == Mode::Sessions && self.list_is_free() {
            if let Some(reveal_id) = self.reveal.clone() {
                self.reveal_row(&rows, &grouping, &reveal_id, cx);
            }
        }
        let pane = cx.entity().downgrade();
        let screen = sidebar_screen(Self::SIDEBAR_VIEW_ID, model, self.list_state.clone())
            .width(px(self.resize.width))
            .rename_textarea(&self.rename)
            // v3: the sidebar has no ground of its own — on the frame in the
            // column, on Floating in the peek card; the shell's bar carries
            // the toggle and the switch.
            .transparent()
            // Browse's right-click menus draw in the overlay window, over the
            // native page (`overlay.rs`).
            .external_menus(true)
            .on_intent(move |intent, window, cx| {
                let _ = pane.update(cx, |pane, cx| pane.intent(intent, window, cx));
            });
        // The pane's own rect, once per frame: the hover card's side seat
        // hangs off its right edge.
        let pane_report = cx.entity().downgrade();
        let wheel = cx.entity().downgrade();
        // A mode switch: the new rows arrive on FF's swap timing (80 ms
        // ease-out), the one motion the Sessions | Browse switch has beside
        // its indicator's slide. The pane is cached, so it asks for its own
        // next frame until the fade has run.
        let fade = match self.mode_fade_since {
            Some(since) => {
                let k = (cx.background_executor().now().duration_since(since).as_secs_f32() / MODE_FADE.as_secs_f32()).clamp(0.0, 1.0);
                if k < 1.0 {
                    aui_motion::request_frame(window);
                } else {
                    self.mode_fade_since = None;
                }
                aui_tokens::Easing::OUT.sample(k)
            }
            None => 1.0,
        };
        div()
            .size_full()
            .opacity(fade)
            // A real wheel over the sidebar is the person's scroll: it
            // disarms a pending reveal (theirs wins) and dismisses the group
            // menu and the hover card, like the scripted sweep does. The
            // library list scrolls itself; this only listens.
            .on_scroll(move |_, _, cx| {
                let _ = wheel.update(cx, |pane, cx| pane.note_wheel(cx));
            })
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = pane_report.update(cx, |pane, _| pane.detail.pane_bounds = Some(bounds));
                }
            })
            .child(screen)
    }
}

/// The tier's plan label for the footer and its menu, or `None` while
/// the tier is unknown: the footer never reads "Plan unknown".
pub(crate) fn muse_plan_label(tier: Option<&Tier>) -> Option<String> {
    match tier {
        Some(Tier::Unavailable(_)) | None => None,
        Some(tier) => Some(tier.footer_label()),
    }
}

/// The nav row ids the screen reports back through `SidebarIntent::Nav`.
pub(crate) const NAV_PROJECTS: &str = "projects";
pub(crate) const NAV_SKILLS: &str = "skills";
/// The sidebar's "Needs you" filter row (task 3.4).
pub(crate) const NAV_NEEDS_YOU: &str = "needs-you";

/// A Browse tab's id as the screen's stable row key, and back.
pub(crate) fn browse_tab_key(id: crate::browser::TabId) -> String {
    format!("tab-{}", id.0)
}

pub(crate) fn browse_tab_id(key: &str) -> Option<crate::browser::TabId> {
    key.strip_prefix("tab-")?.parse().ok().map(crate::browser::TabId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;
    use gpui_kit::base::v_flex;
    use gpui::TestAppContext;
    use crate::app::Harness;

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
                    created: now,
                    running: false,
                    turns: 0,
                    hidden: false,
                    pinned: false,
                    archived: false,
                    description: String::new(),
                    replayed: false,
                    provider: crate::providers::ProviderId::Muse,
                    named: false,
                    needs_title: false,
                    side_marker: false,
                    title_pending: false,
                    last_ask: None,
                    local: false,
                    provisional: false,
                    turns_unknown: false,
                    workspace: None,
                    project: Some(id.clone()),
                    project_name: None,
                    attention: Vec::new(),
                    approval_command: None,
                    pending_question: None,
                    turn_started: None,
                    last_error: None,
                    unread: false,
                    branch: None,
                    tasks_running: 0,
                    pages: 0,
                    pull_request: None,
                })
            })
            .collect();
        let closed = HashSet::new();
        let expanded: HashSet<String> = [ids[7].clone()].into_iter().collect();
        let view = GroupView { closed: &closed, expanded: &expanded, active: Some("p6-s12"), active_project: None, pending: None };
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

    /// Only the rowless open session keeps a missed reveal armed: any
    /// other miss disarms or streaks as before, so a quiet click (which
    /// arms nothing) can never move the list.
    #[test]
    fn only_the_rowless_open_session_keeps_its_missed_reveal() {
        assert!(rowless_reveal_stays_armed(Some("s"), "s", false));
        assert!(!rowless_reveal_stays_armed(Some("s"), "s", true), "a known row resolves");
        assert!(!rowless_reveal_stays_armed(Some("other"), "s", false));
        assert!(!rowless_reveal_stays_armed(None, "s", false));
    }

    /// Two projects on disk for the scroll-target tests below.
    fn scroll_projects(tag: &str) -> (Vec<String>, Projects) {
        let mut projects = Projects::default();
        let ids: Vec<String> = (0..2)
            .map(|i| {
                let dir =
                    std::env::temp_dir().join(format!("baaz-scroll-{tag}-{i}-{}", std::process::id()));
                std::fs::create_dir_all(&dir).expect("temp root");
                projects.add(&dir).id.clone()
            })
            .collect();
        (ids, projects)
    }

    /// One turned row in `project`, built the way the stress test builds
    /// its entries.
    fn scroll_entry(id: &str, project: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned(),
            label: "x".into(),
            updated: crate::clock::now_local(),
            created: crate::clock::now_local(),
            running: false,
            turns: 1,
            hidden: false,
            pinned: false,
            archived: false,
            description: String::new(),
            replayed: false,
            provider: crate::providers::ProviderId::Muse,
            named: false,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace: None,
            project: Some(project.to_owned()),
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            unread: false,
            branch: None,
            tasks_running: 0,
            pages: 0,
            pull_request: None,
        }
    }

    /// The project grouping over `entries`, flattened for the target
    /// lookups.
    fn scroll_rows(entries: &[SessionEntry], projects: &Projects) -> (crate::sidebar::Grouping, Vec<SidebarRow>) {
        let closed = HashSet::new();
        let expanded = HashSet::new();
        let view = GroupView { closed: &closed, expanded: &expanded, active: None, active_project: None, pending: None };
        let grouping =
            grouping_by_project(entries, projects, &HashMap::new(), &view, &Layout::default(), crate::clock::now_local());
        let rows = flatten_sidebar(&grouping, false);
        (grouping, rows)
    }

    /// The head row index of `project` in a flattened model.
    fn project_head_ix(grouping: &crate::sidebar::Grouping, rows: &[SidebarRow], project: &str) -> usize {
        let crate::sidebar::Grouping::Project(groups) = grouping else {
            panic!("project grouping must yield project groups");
        };
        let group = groups.iter().position(|g| g.id.as_ref() == project).expect("the group");
        rows.iter()
            .position(|row| matches!(row, SidebarRow::ProjectHead { group: g } if *g == group))
            .expect("the head row")
    }

    /// Opening (or switching to) a listed session steers to its own row.
    #[test]
    fn open_and_switch_scroll_to_the_existing_row() {
        let (ids, projects) = scroll_projects("open");
        let entries = vec![scroll_entry("s-row", &ids[0])];
        let (grouping, rows) = scroll_rows(&entries, &projects);
        let wanted: SharedString = "s-row".into();
        let ix = row_index_for_session(&rows, &grouping, &wanted).expect("the row lists");
        assert!(matches!(rows[ix], SidebarRow::Session { .. }), "open steers to the row, not a head");
    }

    /// A first send inserts the row, and the re-armed reveal lands on it.
    #[test]
    fn first_send_scrolls_to_the_just_born_row() {
        let (ids, projects) = scroll_projects("first");
        let entries = vec![scroll_entry("s-row", &ids[0]), scroll_entry("s-born", &ids[1])];
        let (grouping, rows) = scroll_rows(&entries, &projects);
        let wanted: SharedString = "s-born".into();
        let ix = row_index_for_session(&rows, &grouping, &wanted).expect("the born row lists");
        assert!(matches!(rows[ix], SidebarRow::Session { .. }), "first send steers to the new row");
    }

    /// A brand-new session has no row: the reveal falls back to its
    /// project head — filed by its facts first (a draft's before its first
    /// send, a provider session's from its open), then by the current
    /// project — and gives up past both.
    #[test]
    fn new_session_scrolls_to_its_project_head() {
        let (ids, projects) = scroll_projects("new");
        let entries = vec![scroll_entry("s-row", &ids[0])];
        let (grouping, rows) = scroll_rows(&entries, &projects);
        // Filed by its facts: a draft's project before its first send.
        let mut overrides = crate::sessions::Overrides::new();
        overrides.insert(
            "s-new".to_owned(),
            crate::sessions::SessionMeta { project: Some(ids[1].clone()), ..Default::default() },
        );
        assert_eq!(
            reveal_head_row(&entries, &overrides, None, &rows, &grouping, "s-new"),
            Some(project_head_ix(&grouping, &rows, &ids[1])),
            "the facts file the rowless session"
        );
        // Filed by the current project: a new session starts in it.
        assert_eq!(
            reveal_head_row(&entries, &overrides, Some(ids[0].as_str()), &rows, &grouping, "s-unknown"),
            Some(project_head_ix(&grouping, &rows, &ids[0])),
            "the current project files an otherwise unknown session"
        );
        // Past both: nowhere to go.
        assert_eq!(reveal_head_row(&entries, &overrides, None, &rows, &grouping, "s-unknown"), None);
    }

    /// An open view on `session_id`, the way a draft opens before its
    /// first send: no child, no wire, offline throughout.
    fn active_view(vc: &mut gpui::VisualTestContext, workspace: &str, session_id: &str) -> Entity<crate::session::SessionView> {
        let workspace = workspace.to_owned();
        let session_id = session_id.to_owned();
        vc.update(|window, cx| {
            cx.new(|cx| {
                crate::session::SessionView::new(session_id, crate::session::SessionHost {
                        provider_id: "muse".to_owned(),
                        workspace,
                        overlays: cx.new(|_| crate::overlays::Overlays::default()),
                        capture: crate::runtime::CaptureToken::default(),
                        terminal_host: None,
                    },
                    window,
                    cx,
                )
            })
        })
    }

    /// The selected row and the current group the sidebar draws, read the
    /// way the frame reads them.
    fn selected_and_current(
        baaz: &Entity<Harness>,
        vc: &mut gpui::VisualTestContext,
    ) -> (Option<String>, Option<String>) {
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
            let selected = h.sidebar_facts(cx).selected;
            let grouping = h.sidebar_grouping(cx);
            let crate::sidebar::Grouping::Project(groups) = &*grouping else {
                panic!("two adoptions group by project");
            };
            let current = groups.iter().find(|g| g.current).map(|g| g.id.to_string());
            (selected, current)
            })
        })
    }

    /// The active session's row is selected and its project group current:
    /// first for a session with a row, then for a brand-new session with
    /// none (its project resolves past the rows through the override).
    #[gpui::test]
    fn active_row_selected_and_project_current_for_existing_and_new(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("active");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let ids: Vec<String> = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                (0..2)
                    .map(|i| {
                        let root = dir.join(format!("proj-{i}"));
                        std::fs::create_dir_all(&root).expect("temp root");
                        h.session_list.update(cx, |list, _| list.projects.add(&root).id.clone())
                    })
                    .collect()
            })
        });
        // One turned row in the first project, open in the window; the
        // list has landed.
        let workspace = dir.to_string_lossy().into_owned();
        let open = active_view(vc, &workspace, "s-row");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.push(scroll_entry("s-row", &ids[0])));
                h.session_list.update(cx, |list, _| list.sessions_loaded = true);
                h.session_list.update(cx, |list, _| list.index_loaded = true);
                h.active = Some(open.clone());
                h.invalidate_list(cx);
            })
        });
        let (selected, current) = selected_and_current(&baaz, vc);
        assert_eq!(selected.as_deref(), Some("s-row"), "the open session's row is selected");
        assert_eq!(current.as_deref(), Some(ids[0].as_str()), "its project group is current");
        // Brand-new: no row yet, filed in the second project by override.
        let draft = active_view(vc, &workspace, "s-new");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.set_override("s-new", |m| m.project = Some(ids[1].clone()), cx);
                h.active = Some(draft.clone());
                h.invalidate_list(cx);
            })
        });
        let (selected, current) = selected_and_current(&baaz, vc);
        assert_eq!(selected.as_deref(), Some("s-new"), "a new session still names its selection");
        assert_eq!(current.as_deref(), Some(ids[1].as_str()), "its project group is current");
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H2: the cached pane re-renders on a mode switch, so the switch
    /// row at the top of the column never goes stale. The mode is a window
    /// preference: the pane observes the layout global (task 4.1), so the
    /// write alone notifies it — no fact, no notify at the write.
    #[gpui::test]
    fn sidebar_pane_rearms_on_mode_switch(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("mode-key");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.run_until_parked();
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        let notified = Rc::new(std::cell::Cell::new(0u32));
        let _watch = vc.update(|_, cx| {
            let notified = notified.clone();
            cx.observe(&pane, move |_, _| notified.set(notified.get() + 1))
        });
        vc.update(|_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse));
        vc.run_until_parked();
        assert!(notified.get() > 0, "the mode switch re-arms the cached pane");
        assert_eq!(vc.update(|_, cx| crate::layout::Layout::get(cx).mode), crate::layout::Mode::Browse);
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H3: the cached pane re-renders when the Browse sidebar's inputs
    /// change — a select, a pin, a close and the open tab menu all move the
    /// facts the root pushes (`Harness::sidebar_facts`).
    #[gpui::test]
    fn sidebar_facts_change_with_browse_tabs(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-key");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                // Browse's rows are facts only in Browse mode (a Sessions
                // frame never walks the tab model), so the test enters
                // Browse the way the switch does.
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None));
                h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::Owner::Person, "https://b.example/", None));
            })
        });
        let opened = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx)));
        // A select moves the facts.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let space = h.browser.read(cx).browse_space();
                let id = h.browser.read(cx).tabs.person_tabs(space.as_deref())[0].id;
                h.browser.update(cx, |browser, _| browser.tabs.activate(id));
            })
        });
        let selected = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx)));
        assert!(opened != selected, "a tab select moves the facts, so the cached pane re-renders");
        // A pin moves the facts (the pinned tile appears).
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let space = h.browser.read(cx).browse_space();
                let id = h.browser.read(cx).tabs.person_tabs(space.as_deref())[0].id;
                h.browser.update(cx, |browser, _| browser.tabs.set_pinned(id, true));
            })
        });
        let pinned = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx)));
        assert!(selected != pinned, "a pin moves the facts");
        // A close moves the facts.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let id = h.browser.read(cx).tabs.tabs[0].id;
                h.browser.update(cx, |browser, _| browser.tabs.close(id));
            })
        });
        let closed = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx)));
        assert!(pinned != closed, "a close moves the facts");
        // The open tab menu (Browse's right-click menu on a tab, an entry
        // on the overlay stack) is a fact too.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                let id = h.browser.read(cx).tabs.tabs.first().map(|tab| tab.id).expect("a tab");
                h.open_browse_menu(
                    aui::screens::sidebar::BrowseMenu {
                        target: aui::screens::sidebar::BrowseMenuTarget::Tab { id: format!("tab-{}", id.0).into(), pinned: false, in_folder: false, offers: Default::default() },
                        at: gpui::point(gpui::px(10.0), gpui::px(10.0)),
                    },
                    cx,
                );
            })
        });
        let menu = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx)));
        assert!(closed != menu, "the tab menu moves the facts");
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H3b item 1: with focus on the focus root in Browse, ⌘W's
    /// handler closes the active person tab; the same handler in
    /// Sessions is a no-op (the belt), so ⌘W falls through to
    /// CloseWindow there. The context placement behind the dispatch —
    /// `BaazBrowse` on an ancestor of the focus-root div, Browse-only —
    /// is pinned in `browse_h3.rs`; this pins the handler half with
    /// focus resting on the root (the first frame, after overlays
    /// close), which is the case the ancestor fixes.
    #[gpui::test]
    fn browse_close_shortcut_closes_a_tab_only_in_browse(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-close");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None));
                h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::Owner::Person, "https://b.example/", None));
            })
        });
        vc.update(|window, cx| window.focus(&baaz.read(cx).focus_root.clone(), cx));
        vc.update(|window, cx| {
            assert!(
                baaz.read(cx).focus_root.is_focused(window),
                "focus rests on the focus root"
            );
            baaz.update(cx, |h, cx| h.close_browse_shortcut(window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let tabs = h.browser.read(cx).tabs.person_tabs(h.browser.read(cx).browse_space().as_deref());
            assert_eq!(tabs.len(), 1, "⌘W in Browse closes a tab, not the window");
        });
        // Sessions: the same handler must not touch the tabs — the belt
        // holds, and ⌘W keeps closing the window there.
        vc.update(|_, cx| {
            baaz.update(cx, |_, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Sessions);
            })
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.close_browse_shortcut(window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let tabs = h.browser.read(cx).tabs.person_tabs(h.browser.read(cx).browse_space().as_deref());
            assert_eq!(tabs.len(), 1, "⌘W in Sessions closes no tab");
        });
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H3b item 6: while a session tab shows, ⌘W stops showing
    /// it (back to the person's active tab, closing nothing) and the
    /// moves are no-ops (they would act on the hidden person tab).
    /// Owner screenshots 2026-10-11: the page's own row had no selected
    /// pill. Each session's active tab also read "active", lighting every
    /// agent row and taking the pill. Only the centre's tab is active now.
    #[gpui::test]
    fn only_the_centres_tab_is_active_in_browse(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("only-centre-active");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                let mine = h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None));
                let agent = h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::Owner::Session("s-1".to_owned()), "https://b.example/", None));
                h.browser.update(cx, |browser, _| browser.tabs.activate(agent));
                h.browser.update(cx, |browser, _| browser.tabs.activate(mine));
                let model = h.browse_model(cx);
                let actives: Vec<_> = model
                    .tabs
                    .iter()
                    .chain(model.task_folders.iter().flat_map(|folder| folder.tabs.iter()))
                    .filter(|tab| tab.active)
                    .map(|tab| tab.id.clone())
                    .collect();
                assert_eq!(actives, vec![SharedString::from(browse_tab_key(mine))], "one active row: the centre's");
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// A tab already navigating is never the new-tab page, even while its
    /// live page still reports about:blank (slow first load, 2026-10-11).
    #[gpui::test]
    fn a_navigating_tab_is_not_blank(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("navigating-not-blank");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                let blank = h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, crate::browser::tabs::BLANK, None));
                h.browser.update(cx, |browser, _| browser.tabs.activate(blank));
                assert!(h.browse_centre_tab_blank(cx), "a fresh tab is blank");
                let going = h.browser.update(cx, |bv, _| bv.tabs.open(space, crate::browser::Owner::Person, "https://news.example/", Some(blank)));
                h.browser.update(cx, |browser, _| browser.tabs.activate(going));
                assert!(!h.browse_centre_tab_blank(cx), "a tab opened on a URL is not blank");
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// Owner 2026-10-11: New tab opens next to the current tab, and inside
    /// its folder when the current tab sits in one.
    #[gpui::test]
    fn new_tab_opens_beside_the_current_tab_in_its_folder(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("new-tab-beside");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let (first, folded) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                let first = h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None));
                h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, "https://b.example/", None));
                let folded = h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, "https://c.example/", None));
                let folder = h.browser.update(cx, |browser, _| browser.tabs.add_folder(space.as_deref().unwrap_or_default(), "Reading"));
                h.browser.update(cx, |browser, _| browser.tabs.set_folder(folded, Some(&folder)));
                h.browser.update(cx, |browser, _| browser.tabs.activate(first));
                (first, folded)
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.new_browse_sidebar_tab(window, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let ids: Vec<_> = h.browser.read(cx).tabs.person_tabs(h.browser.read(cx).browse_space().as_deref()).iter().map(|tab| tab.id).collect();
            let at = ids.iter().position(|id| *id == first).expect("first");
            let new = ids[at + 1];
            assert!(h.browser.read(cx).tabs.get(new).is_some_and(|tab| tab.url == crate::browser::tabs::BLANK && tab.folder.is_none()), "a Today tab's new tab sits right after it, in Today");
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.browser.update(cx, |browser, _| browser.tabs.activate(folded))));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.new_browse_sidebar_tab(window, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let ids: Vec<_> = h.browser.read(cx).tabs.person_tabs(h.browser.read(cx).browse_space().as_deref()).iter().map(|tab| tab.id).collect();
            let at = ids.iter().position(|id| *id == folded).expect("folded");
            let new = h.browser.read(cx).tabs.get(ids[at + 1]).expect("a tab after the folded one");
            assert_eq!(new.url, crate::browser::tabs::BLANK);
            assert_eq!(new.folder, h.browser.read(cx).tabs.get(folded).and_then(|tab| tab.folder.clone()), "a folder tab's new tab joins its folder");
        });
        b3c_teardown(&dir, guard, old);
    }

    #[gpui::test]
    fn browse_shortcuts_defer_to_the_shown_session_tab(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-session-tab");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                let space = h.browser.read(cx).browse_space();
                h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None));
                h.browser.update(cx, |bv, _| bv.tabs.open(space.clone(), crate::browser::Owner::Person, "https://b.example/", None));
                let shown = h.browser.update(cx, |browser, _| browser.tabs.open(space, crate::browser::Owner::Session("s-1".to_owned()), "https://c.example/", None));
                h.browser.update(cx, |browser, _| browser.tabs.activate(shown));
                h.browser.update(cx, |browser, cx| browser.set_session_tab(Some(shown), cx));
            })
        });
        let before = vc.update(|_, cx| baaz.read(cx).browser.read(cx).tabs.generation);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.move_browse_shortcut(1, window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.browser.read(cx).browse_session_tab.is_some(), "the session show survives the move");
            assert_eq!(
                h.browser.read(cx).tabs.generation, before,
                "the move touches no person tab while a session tab shows"
            );
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.close_browse_shortcut(window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.browser.read(cx).browse_session_tab.is_none(), "⌘W stops showing the session tab");
            let tabs = h.browser.read(cx).tabs.person_tabs(h.browser.read(cx).browse_space().as_deref());
            assert_eq!(tabs.len(), 2, "⌘W closes nothing while a session tab shows");
        });
        b3c_teardown(&dir, guard, old);
    }

    /// B3c: one date-bucket summary, pinned on request.
    fn b3c_summary(id: &str, pinned: bool) -> aui::nav::SessionSummary {
        use aui_tokens::AgentState;
        let mut summary = aui::nav::SessionSummary::new(id, id, AgentState::Idle, "now");
        summary.pinned = pinned;
        summary
    }

    /// B3c: hermetic state plus boot args for a throwaway window, for
    /// driving `SidebarPane::sync_list` the way the staged boot loads do. The
    /// caller builds the window itself: the empty window borrows the test
    /// context, so it cannot come back out of a helper.
    fn b3c_state(
        tag: &str,
    ) -> (
        std::path::PathBuf,
        std::sync::MutexGuard<'static, ()>,
        Option<std::ffi::OsString>,
        crate::Args,
    ) {
        let dir = std::env::temp_dir().join(format!("baaz-b3c-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        let args = crate::Args {
            workspace: dir.clone(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: Some(aui_tokens::ThemeKind::Dark),
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
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        };
        (dir, guard, old, args)
    }

    /// B3c: a throwaway signed-in window on the hermetic state above.
    fn b3c_window(vc: &mut gpui::VisualTestContext, args: crate::Args) -> Entity<Harness> {
        vc.update(|window, cx| {
            let baaz =
                cx.new(|cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
            baaz.update(cx, |h, _| {
                h.auth = crate::login::Auth::SignedIn(crate::auth::Identity {
                    lane: provider::AccountLane::AccountLogin,
                    name: "Probe".into(),
                    email: String::new(),
                });
            });
            baaz
        })
    }

    /// B3c: put the hermetic state back.
    fn b3c_teardown(
        dir: &std::path::Path,
        guard: std::sync::MutexGuard<'static, ()>,
        old: Option<std::ffi::OsString>,
    ) {
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B3c: the harness's own list state, drawn small enough that a deep
    /// row starts below the fold.
    struct B3cListProbe {
        grouping: Grouping,
        state: gpui::ListState,
    }

    impl Render for B3cListProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            v_flex().w(px(300.)).h(px(120.)).child(virtual_sidebar_view(
                "b3c-probe",
                self.grouping.clone(),
                self.state.clone(),
            ))
        }
    }

    fn draw_b3c_list(vc: &mut gpui::VisualTestContext, grouping: &Grouping, state: &gpui::ListState) {
        let host = B3cListProbe { grouping: grouping.clone(), state: state.clone() };
        vc.draw(point(px(0.), px(0.)), gpui::size(px(300.), px(120.)), |_, cx| {
            cx.new(|_| host).into_any_element()
        });
        vc.run_until_parked();
    }

    /// B3c, no active session: the staged boot loads splice rows above a
    /// top-parked list (a newly pinned row lifts a Pinned section over the
    /// date bucket the first batch painted), and gpui's splice carries the
    /// parked top down with the insertion. Launch must still open at row 0.
    #[gpui::test]
    fn b3c_staged_loads_keep_a_top_parked_list_at_row_zero(cx: &mut TestAppContext) {
        use aui::nav::DateGroup;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("top");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        // First batch: one date bucket, nothing pinned.
        let g1 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![b3c_summary("u-1", false), b3c_summary("u-2", false), b3c_summary("u-3", false)],
        )]);
        let rows1 = flatten_sidebar(&g1, false);
        assert_eq!(rows1.len(), 4, "header plus three sessions");
        let g1 = Rc::new(g1);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                assert!(h.reveal.is_none(), "no session is active, so nothing arms a reveal");
                h.sync_list(&rows1, &g1, cx);
                assert_eq!(h.list_state.item_count(), rows1.len());
                // Park the list the way a painted top leaves it: the splice
                // below only carries a materialised scroll top.
                h.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                assert_eq!(h.list_top().item_ix, 0, "the first paint parks at the top");
            })
        });
        // Second batch: the wire list adds a pinned session, which lifts a
        // Pinned section above the bucket the first batch painted — a pure
        // insertion at index 0 with the old rows as a verbatim suffix.
        let g2 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![
                b3c_summary("u-1", false),
                b3c_summary("u-2", false),
                b3c_summary("u-3", false),
                b3c_summary("p-0", true),
            ],
        )]);
        let rows2 = flatten_sidebar(&g2, false);
        assert_eq!(rows2.len(), 6, "pinned header, pinned row, then the old rows");
        let g2 = Rc::new(g2);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.sync_list(&rows2, &g2, cx);
                assert_eq!(h.list_state.item_count(), rows2.len());
                assert_eq!(
                    h.list_top().item_ix,
                    0,
                    "staged loads must not carry a top-parked list with no active session"
                );
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// B3c, boot reveal for a rowless id: a draft's reveal arms before its
    /// row exists and waits while the staged loads land around it. The wait
    /// must neither steer the list nor let the splices carry a top-parked
    /// list away: with no session active the top stays row 0 and the arm
    /// survives for the row's eventual birth.
    #[gpui::test]
    fn b3c_rowless_boot_reveal_waits_without_moving_the_list(cx: &mut TestAppContext) {
        use aui::nav::DateGroup;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("unknown");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        let g1 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![b3c_summary("u-1", false), b3c_summary("u-2", false), b3c_summary("u-3", false)],
        )]);
        let rows1 = flatten_sidebar(&g1, false);
        let g1 = Rc::new(g1);
        // Boot order: the activation arms the reveal before any rows land.
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.reveal = Some("s-draft".to_owned());
                h.reveal_unknown = None;
                h.user_scrolled = false;
                h.sync_list(&rows1, &g1, cx);
                // Park the list the way a painted top leaves it (a parked
                // scroll, not a reveal steer — the draft has no row).
                h.list_state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                assert_eq!(h.list_top().item_ix, 0, "the first paint parks at the top");
            })
        });
        let g2 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![
                b3c_summary("u-1", false),
                b3c_summary("u-2", false),
                b3c_summary("u-3", false),
                b3c_summary("p-0", true),
            ],
        )]);
        let rows2 = flatten_sidebar(&g2, false);
        let g2 = Rc::new(g2);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.sync_list(&rows2, &g2, cx);
                // The wait itself never steers, even called every frame: the
                // draft has no row anywhere yet.
                h.reveal_row(&rows2, &g2, "s-draft", cx);
                assert_eq!(
                    h.list_top().item_ix,
                    0,
                    "a reveal for an id with no row must not move the list"
                );
                assert_eq!(
                    h.reveal.as_deref(),
                    Some("s-draft"),
                    "the rowless reveal keeps waiting for its row"
                );
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// B3c, boot session open: the outside activation arms the reveal before
    /// the rows land; the first batch shows the row, later batches reindex
    /// around it. The reveal must track its row by id until the rows settle
    /// (`sessions_loaded`), then complete — never strand the list mid-list
    /// with the arm already consumed.
    #[gpui::test]
    fn b3c_boot_reveal_tracks_its_row_through_staged_loads(cx: &mut TestAppContext) {
        use aui::nav::StatusGroup;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("reveal");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        // First batch: three sessions, the boot session last — small enough
        // that its row is on screen, so the reveal below completes (or, once
        // fixed, waits out the staging) before the big batch lands.
        let first: Vec<aui::nav::SessionSummary> =
            (0..3).map(|i| b3c_summary(&format!("s-{i}"), false)).collect();
        let g1 = Grouping::Status(vec![StatusGroup::new("g", "G", "3", first.clone())]);
        let rows1 = flatten_sidebar(&g1, false);
        assert_eq!(rows1.len(), 4, "header plus three sessions");
        let g1 = Rc::new(g1);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                assert!(!h.list.read(cx).sessions_loaded, "the wire list has not landed yet");
                h.sync_list(&rows1, &g1, cx);
                assert_eq!(h.list_state.item_count(), rows1.len());
            })
        });
        let state = vc.update(|_, cx| pane.read(cx).list_state.clone());
        draw_b3c_list(vc, &g1, &state);
        // Boot order: the activation arms the reveal while staging.
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.reveal = Some("s-2".to_owned());
                h.reveal_unknown = None;
                h.user_scrolled = false;
                h.reveal_row(&rows1, &g1, "s-2", cx);
            })
        });
        draw_b3c_list(vc, &g1, &state);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.reveal_row(&rows1, &g1, "s-2", cx);
            })
        });
        // Second batch: twenty newer sessions land above (provider rows,
        // then the wire list), reindexing every batch-1 row and burying the
        // boot row far below a top-parked list.
        let mut second: Vec<aui::nav::SessionSummary> =
            (0..20).map(|i| b3c_summary(&format!("n-{i}"), false)).collect();
        second.extend(first);
        let g2 = Grouping::Status(vec![StatusGroup::new("g", "G", "23", second)]);
        let rows2 = flatten_sidebar(&g2, false);
        assert_eq!(rows2.len(), 24, "header plus twenty-three sessions");
        let g2 = Rc::new(g2);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.sync_list(&rows2, &g2, cx);
                assert_eq!(h.list_state.item_count(), rows2.len());
            })
        });
        // Frame pump, the way the renderer drives the armed reveal: each
        // frame steers by id again (measuring further each time) until the
        // row reports visible. A consumed reveal steers no more frames.
        let target = row_index_for_session(&rows2, &g2, &"s-2".into()).expect("the boot row has an index");
        for _ in 0..30 {
            vc.update(|_, cx| {
                pane.update(cx, |h, cx| {
                    if let Some(id) = h.reveal.clone() {
                        h.reveal_row(&rows2, &g2, &id, cx);
                    }
                })
            });
            draw_b3c_list(vc, &g2, &state);
            let settled = vc.update(|_, cx| {
                pane.read(cx).list_state.item_is_above_viewport(target) == Some(false)
                    && pane.read(cx).list_state.item_is_below_viewport(target) == Some(false)
            });
            if settled {
                break;
            }
        }
        vc.update(|_, cx| {
            pane.update(cx, |h, _| {
                assert_eq!(
                    h.reveal.as_deref(),
                    Some("s-2"),
                    "the staged reveal stays armed until the rows settle"
                );
                assert_eq!(
                    h.list_state.item_is_above_viewport(target),
                    Some(false),
                    "the active row is not above the viewport after the loads settle"
                );
                assert_eq!(
                    h.list_state.item_is_below_viewport(target),
                    Some(false),
                    "the active row is visible after the loads settle"
                );
                assert!(
                    h.list_top().item_ix > 0,
                    "the list steered to the active row, not row 0"
                );
            })
        });
        // The wire list lands: rows settle, the same reveal completes.
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.list.update(cx, |list, _| list.sessions_loaded = true);
                h.reveal_row(&rows2, &g2, "s-2", cx);
                assert_eq!(h.reveal, None, "the settled reveal completes once its row is visible");
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// A reveal for a row far below anything the list has measured (an
    /// imported session near the bottom of a 2,400-row Unfiled, QA
    /// 2026-10-30) lands in a few frames. `scroll_to_reveal_item` steers by
    /// measured heights only, so each frame crept a few rows and the
    /// unmeasured target read "not laid out" — the reveal stalled with the
    /// row out of sight, and opening the session or "Show in sidebar"
    /// never showed it.
    #[gpui::test]
    fn a_reveal_for_a_far_unmeasured_row_lands_in_a_few_frames(cx: &mut TestAppContext) {
        use aui::nav::StatusGroup;

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("far");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        let rows: Vec<aui::nav::SessionSummary> = (0..2000).map(|i| b3c_summary(&format!("s-{i}"), false)).collect();
        let g = Grouping::Status(vec![StatusGroup::new("g", "G", "2000", rows)]);
        let flat = flatten_sidebar(&g, false);
        let g = Rc::new(g);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.list.update(cx, |list, _| list.sessions_loaded = true);
                h.sync_list(&flat, &g, cx);
            })
        });
        let state = vc.update(|_, cx| pane.read(cx).list_state.clone());
        draw_b3c_list(vc, &g, &state);
        let target = row_index_for_session(&flat, &g, &"s-1950".into()).expect("the far row has an index");
        vc.update(|_, cx| {
            pane.update(cx, |h, _| {
                h.reveal = Some("s-1950".to_owned());
                h.reveal_unknown = None;
                h.user_scrolled = false;
            })
        });
        let mut settled = false;
        for _ in 0..6 {
            vc.update(|_, cx| {
                pane.update(cx, |h, cx| {
                    if let Some(id) = h.reveal.clone() {
                        h.reveal_row(&flat, &g, &id, cx);
                    }
                })
            });
            draw_b3c_list(vc, &g, &state);
            settled = vc.update(|_, cx| {
                pane.read(cx).list_state.item_is_above_viewport(target) == Some(false)
                    && pane.read(cx).list_state.item_is_below_viewport(target) == Some(false)
            });
            if settled {
                break;
            }
        }
        let top = vc.update(|_, cx| pane.read(cx).list_top());
        assert!(settled, "the far row is on screen within a few frames (top {top:?}, target {target})");
        // A near target keeps the measured steer: no jump that would put
        // it at the very top and hide the rows above it (its group header).
        state.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
        draw_b3c_list(vc, &g, &state);
        assert!(!steer_to_row(&state, 2), "a near row is steered to, not jumped to");
        // Nor before the first layout measured anything: a fresh list's
        // first reveal steers, it does not jump.
        let fresh = gpui::ListState::new(2000, gpui::ListAlignment::Top, px(100.0));
        assert!(!steer_to_row(&fresh, 1), "nothing measured yet: no jump");
        b3c_teardown(&dir, guard, old);
    }

    /// B1: a new session gets no sidebar row until its first message is
    /// sent — even while it is the open session. Runs `visible_sessions`
    /// (not just `is_empty`): the fresh active zero-turn row stays out,
    /// the first send reveals it, and a real handoff head with zero own
    /// turns keeps its row.
    #[gpui::test]
    fn b1_fresh_active_session_has_no_row_until_its_first_send(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("b1-empty-row");
        let workspace = dir.to_string_lossy().into_owned();
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        fn empty_entry(id: &str) -> SessionEntry {
            SessionEntry {
                id: id.into(),
                label: crate::sidebar::UNNAMED.into(),
                updated: chrono::Local::now(),
                created: chrono::Local::now(),
                running: false,
                turns: 0,
                hidden: false,
                pinned: false,
                archived: false,
                description: String::new(),
                replayed: false,
                provider: crate::providers::ProviderId::Muse,
                named: false,
                needs_title: false,
                side_marker: false,
                title_pending: false,
                last_ask: None,
                local: false,
                provisional: false,
                turns_unknown: false,
                workspace: None,
                project: None,
                project_name: None,
                attention: Vec::new(),
                approval_command: None,
                pending_question: None,
                turn_started: None,
                last_error: None,
                unread: false,
                branch: None,
                tasks_running: 0,
                pages: 0,
                pull_request: None,
            }
        }
        // The open session, with no turns yet: the muse 1.3.0 shape, where
        // `session/list` already lists the draft as a zero-turn wire row.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.push(empty_entry("s-fresh")));
                // A new session is a draft until a turn starts: what keeps
                // the open session's own exemption off it.
                h.open_state.drafts.insert("p-fresh".into(), "s-fresh".into());
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("s-fresh".into(), host, window, cx));
                h.active = Some(view);
                h.invalidate_list(cx);
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx)
                .visible_sessions(cx)
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        });
        assert!(
            !visible.contains(&"s-fresh".to_owned()),
            "a fresh active zero-turn session has no row, got {visible:?}"
        );
        // The first send reveals it: `first_send_update` marks the row
        // running (`sidebar.rs:939`), which fails `is_empty` on its own.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| {
                    let row = list.sessions.iter_mut().find(|entry| entry.id == "s-fresh").expect("fresh row");
                    assert!(
                        crate::sidebar::first_send_update(row, Some("Fix the header"), chrono::Local::now(), false),
                        "the first send touches a zero-turn row"
                    );
                });
                // The turn started: the session is no draft any more.
                h.open_state.drafts.retain(|_, id| id != "s-fresh");
                h.invalidate_list(cx);
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx)
                .visible_sessions(cx)
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        });
        assert!(visible.contains(&"s-fresh".to_owned()), "the row appears on the first send");
        // A new session with no project (an unadopted root) has no
        // `drafts` entry to keep the exemption off it: `unsent` does,
        // until its first turn starts.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.push(empty_entry("s-root")));
                h.open_state.unsent.insert("s-root".into());
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("s-root".into(), host, window, cx));
                h.active = Some(view);
                h.invalidate_list(cx);
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx).visible_sessions(cx).iter().map(|entry| entry.id.clone()).collect::<Vec<_>>()
        });
        assert!(!visible.contains(&"s-root".to_owned()), "an unsent root session has no row, got {visible:?}");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| {
                    let row = list.sessions.iter_mut().find(|entry| entry.id == "s-root").expect("root row");
                    crate::sidebar::first_send_update(row, Some("Map the folder"), chrono::Local::now(), false);
                });
                h.open_state.unsent.remove("s-root");
                h.invalidate_list(cx);
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx).visible_sessions(cx).iter().map(|entry| entry.id.clone()).collect::<Vec<_>>()
        });
        assert!(visible.contains(&"s-root".to_owned()), "the root session's row appears on its first send");
        // A real handoff head with zero own turns keeps its row: the
        // two-member chain exempts the head from the empty filter.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.push(empty_entry("h-src")));
                h.session_list.update(cx, |list, _| list.sessions.push(empty_entry("h-dst")));
                h.set_override("h-src", |meta| meta.handoff_to = Some("h-dst".into()), cx);
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("h-dst".into(), host, window, cx));
                h.active = Some(view);
                h.invalidate_list(cx);
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx)
                .visible_sessions(cx)
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>()
        });
        assert!(
            visible.contains(&"h-dst".to_owned()),
            "a real handoff head with zero own turns keeps its row, got {visible:?}"
        );
        b3c_teardown(&dir, guard, old);
    }

    /// A session that was sent to keeps its row at zero turns (live,
    /// 2026-10-16): the open session's first turn never settled, so once
    /// `running` read false the empty filter dropped the selected row. The
    /// open session is never filtered once it is no draft, and a row the
    /// stores record a send for — a byline ask, a provider first prompt —
    /// is never empty, open or not.
    #[gpui::test]
    fn a_sent_session_keeps_its_row_when_its_first_turn_never_settles(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("sent-row");
        let workspace = dir.to_string_lossy().into_owned();
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let open = active_view(vc, &workspace, "s-live");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                // The open session: zero turns, not running, unnamed, no
                // draft — the turn started and never settled.
                let mut live = scroll_entry("s-live", "p");
                live.turns = 0;
                live.project = None;
                h.session_list.update(cx, |list, _| list.sessions.push(live));
                // Another session with a recorded ask and zero turns.
                let mut asked = scroll_entry("s-asked", "p");
                asked.turns = 0;
                asked.project = None;
                asked.last_ask = Some("Explain the schema".into());
                h.session_list.update(cx, |list, _| list.sessions.push(asked));
                h.session_list.update(cx, |list, _| {
                    // A provider session admitted once: its record holds the
                    // first prompt, nothing else.
                    crate::provider_sessions::upsert_open(
                        &mut list.provider_sessions, &mut list.overrides,
                        "claude-code",
                        "s-admitted",
                        None,
                        None,
                        None,
                    );
                });
                h.session_list.update(cx, |list, _| crate::provider_sessions::note_first_prompt(&list.provider_sessions, &mut list.overrides, "s-admitted", "Draw the db"));
                h.merge_provider_rows(cx);
                // And a true noise row, which still goes.
                let mut noise = scroll_entry("s-noise", "p");
                noise.turns = 0;
                noise.project = None;
                h.session_list.update(cx, |list, _| list.sessions.push(noise));
                h.session_list.update(cx, |list, _| list.sessions_loaded = true);
                h.session_list.update(cx, |list, _| list.index_loaded = true);
                h.active = Some(open.clone());
                h.invalidate_list(cx);
            })
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx).visible_sessions(cx).iter().map(|entry| entry.id.clone()).collect::<Vec<_>>()
        });
        for id in ["s-live", "s-asked", "s-admitted"] {
            assert!(visible.iter().any(|v| v == id), "{id} keeps its row, got {visible:?}");
        }
        assert!(!visible.iter().any(|v| v == "s-noise"), "a never-sent zero-turn row still goes, got {visible:?}");
        b3c_teardown(&dir, guard, old);
    }

    /// A session row for the real-path tests below.
    fn list_entry(id: &str, pinned: bool) -> SessionEntry {
        let now = chrono::Local::now();
        SessionEntry {
            id: id.to_owned(),
            label: id.to_owned(),
            updated: now,
            created: now,
            running: false,
            turns: 1,
            hidden: false,
            pinned,
            archived: false,
            description: String::new(),
            replayed: false,
            provider: crate::providers::ProviderId::Muse,
            named: true,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            turns_unknown: false,
            workspace: None,
            project: None,
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            unread: false,
            branch: None,
            tasks_running: 0,
            pages: 0,
            pull_request: None,
        }
    }

    /// Fill the harness with `n` date-grouped sessions (the last pinned) and
    /// mark the lists loaded.
    fn load_sessions(h: &mut Harness, n: usize, cx: &mut gpui::App) {
        crate::layout::Layout::stage(cx, |layout| layout.group_by = Some(crate::layout::GroupBy::Date));
        h.session_list.update(cx, |list, _| list.sessions = (0..n).map(|i| list_entry(&format!("s{i}"), i == n - 1)).collect());
        h.session_list.update(cx, |list, _| list.sessions_loaded = true);
        h.session_list.update(cx, |list, _| list.index_loaded = true);
        h.invalidate_list(cx);
    }

    fn render_once(vc: &mut gpui::VisualTestContext, baaz: &Entity<Harness>) {
        // The root pushes its facts, then the pane renders itself.
        let pane = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.push_sidebar_facts(cx);
                h.sidebar_pane.clone()
            })
        });
        vc.update(|window, cx| {
            pane.update(cx, |pane, cx| {
                let _ = pane.render(window, cx);
            })
        });
    }

    #[gpui::test]
    fn browse_mode_render_leaves_the_session_list_untouched(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-keeps-list");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        vc.update(|_, cx| baaz.update(cx, |h, cx| load_sessions(h, 40, cx)));
        render_once(vc, &baaz);
        let (count, top) = vc.update(|_, cx| {
            pane.update(cx, |h, _| {
                h.list_state.scroll_to(gpui::ListOffset { item_ix: 9, offset_in_item: px(0.0) });
                (h.list_state.item_count(), h.list_state.logical_scroll_top())
            })
        });
        assert!(count > 20, "the sessions area has rows: {count}");
        assert_eq!(top.item_ix, 9);
        vc.update(|_, cx| baaz.update(cx, |_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse)));
        render_once(vc, &baaz);
        let (browse_count, browse_top) =
            vc.update(|_, cx| pane.update(cx, |h, _| (h.list_state.item_count(), h.list_state.logical_scroll_top())));
        assert_eq!(browse_count, count, "Browse must not resize the Sessions list state");
        assert_eq!(browse_top.item_ix, top.item_ix);
        assert_eq!(browse_top.offset_in_item, top.offset_in_item);
        vc.update(|_, cx| baaz.update(cx, |_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Sessions)));
        render_once(vc, &baaz);
        let back = vc.update(|_, cx| pane.read(cx).list_top());
        assert_eq!(back.item_ix, top.item_ix, "switching back lands where the person was");
        b3c_teardown(&dir, guard, old);
    }

    #[gpui::test]
    fn a_sidebar_wheel_disarms_the_reveal_and_marks_user_scrolled(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("wheel-disarm");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.reveal = Some("s1".to_owned());
                h.reveal_unknown = Some(("s1".to_owned(), 2));
                h.user_scrolled = false;
                h.note_wheel(cx);
                assert!(h.reveal.is_none());
                assert!(h.reveal_unknown.is_none());
                assert!(h.user_scrolled);
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    #[gpui::test]
    fn a_pinned_sessions_reveal_clears_without_scrolling(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("pinned-reveal");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        vc.update(|_, cx| baaz.update(cx, |h, cx| load_sessions(h, 40, cx)));
        render_once(vc, &baaz);
        vc.update(|_, cx| {
            pane.update(cx, |h, cx| {
                h.list_state.scroll_to(gpui::ListOffset { item_ix: 6, offset_in_item: px(0.0) });
                let before = h.list_state.logical_scroll_top();
                let (pinned, grouping) = {
                    let list = h.list.read(cx);
                    (list.pinned(&h.facts.list, cx), list.grouping(&h.facts.list, cx))
                };
                assert!(pinned.iter().any(|row| row.id.as_ref() == "s39"), "s39 is pinned");
                let rows = flatten_sidebar(&grouping, false);
                h.reveal = Some("s39".to_owned());
                h.reveal_row(&rows, &grouping, "s39", cx);
                assert!(h.reveal.is_none(), "a pinned row is always in view");
                let after = h.list_state.logical_scroll_top();
                assert_eq!((after.item_ix, after.offset_in_item), (before.item_ix, before.offset_in_item));
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// Task 4.7: a window whose root view is the real `Harness`, so the
    /// column renders through the shell's `.cached(...)` embed, the way the
    /// app draws it.
    fn rooted_window(
        cx: &mut TestAppContext,
        args: crate::Args,
    ) -> (Entity<Harness>, Entity<SidebarPane>, &mut gpui::VisualTestContext) {
        let (baaz, vc) =
            cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let pane = vc.update(|_, cx| baaz.read(cx).sidebar_pane.clone());
        (baaz, pane, vc)
    }

    /// The session rows the column draws, `(id, label)`, pinned first: what
    /// its model says, date-grouped.
    fn drawn_rows(vc: &mut gpui::VisualTestContext, pane: &Entity<SidebarPane>) -> Vec<(String, String)> {
        vc.update(|_, cx| {
            let model = pane.read(cx).model(cx);
            let Grouping::Date(groups) = &*model.grouping else { panic!("the probe groups by date") };
            model
                .pinned
                .iter()
                .chain(groups.iter().flat_map(|group| group.sessions.iter()))
                .map(|row| (row.id.to_string(), row.name.to_string()))
                .collect()
        })
    }

    /// Task 4.7, done-when: the sidebar re-renders from model events only. A
    /// new session row, a title landing and a pin each reach the column
    /// through the session list's own event — the pane re-renders and draws
    /// the change — with not one notify of the root.
    #[gpui::test]
    fn a_list_change_redraws_the_column_with_no_root_notify(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("list-redraw");
        let (baaz, pane, vc) = rooted_window(cx, args);
        vc.update(|_, cx| baaz.update(cx, |h, cx| load_sessions(h, 5, cx)));
        vc.run_until_parked();
        let list = vc.update(|_, cx| baaz.read(cx).session_list.clone());
        let root_notifies = Rc::new(Cell::new(0u32));
        let count = root_notifies.clone();
        let _watch = vc.update(|_, cx| cx.observe(&baaz, move |_, _| count.set(count.get() + 1)));
        let renders = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| pane.read(cx).renders);

        // A new session lands in the list.
        let before = renders(vc);
        vc.update(|_, cx| {
            list.update(cx, |list, cx| {
                list.sessions.push(list_entry("s-new", false));
                list.invalidate(cx);
            })
        });
        vc.run_until_parked();
        assert!(renders(vc) > before, "a new row re-renders the column");
        assert!(drawn_rows(vc, &pane).iter().any(|(id, _)| id == "s-new"), "the column draws the new row");

        // A title lands: a one-row change (`RowChanged`).
        let ids: HashSet<String> = std::iter::once("s1".to_owned()).collect();
        let before = renders(vc);
        vc.update(|_, cx| {
            list.update(cx, |list, cx| {
                list.overrides.entry("s1".to_owned()).or_default().name = Some("Landed title".into());
                list.rejoin_scoped(Some(&ids), cx);
            })
        });
        vc.run_until_parked();
        assert!(renders(vc) > before, "a landed title re-renders the column");
        assert!(
            drawn_rows(vc, &pane).iter().any(|(id, label)| id == "s1" && label == "Landed title"),
            "the column draws the landed title"
        );

        // A pin: the row moves to the Pinned section.
        let before = renders(vc);
        vc.update(|_, cx| {
            list.update(cx, |list, cx| {
                list.overrides.entry("s2".to_owned()).or_default().pinned = true;
                list.rejoin_scoped(Some(&std::iter::once("s2".to_owned()).collect()), cx);
            })
        });
        vc.run_until_parked();
        assert!(renders(vc) > before, "a pin re-renders the column");
        assert!(
            vc.update(|_, cx| pane.read(cx).model(cx).pinned.iter().any(|row| row.id.as_ref() == "s2")),
            "the column draws the pin"
        );

        assert_eq!(root_notifies.get(), 0, "no list change notified the root");
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.9 (rule 13): the Skills nav row shows only while the
    /// Skills page works — the provider that manages skills set up — and
    /// goes again when it is not.
    #[gpui::test]
    fn the_skills_row_shows_only_while_skills_are_set_up(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("skills-nav");
        let (baaz, pane, vc) = rooted_window(cx, args);
        let has_skills = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| {
                baaz.update(cx, |h, cx| {
                    h.push_sidebar_facts(cx);
                });
                pane.read(cx).model(cx).nav.iter().any(|item| item.id.as_ref() == NAV_SKILLS)
            })
        };
        vc.update(|_, cx| crate::skills_page::note_statuses(&[], cx));
        assert!(!has_skills(vc), "no Skills row while nothing manages skills");
        vc.update(|_, cx| {
            let mut muse = crate::provider_status::ProviderStatus::checking(crate::providers::ProviderId::Muse);
            muse.installed = crate::provider_status::Installed::Yes { version: "1".into(), path: "/x/muse".into() };
            crate::skills_page::note_statuses(&[muse], cx);
        });
        assert!(has_skills(vc), "the Skills row once it is set up");
        vc.update(|_, cx| crate::skills_page::note_statuses(&[], cx));
        assert!(!has_skills(vc), "and gone again when it is not");
        b3c_teardown(&dir, guard, old);
    }

    /// Task 4.7, the other half: a root notify that changed nothing the
    /// column draws re-renders the root (composition) and leaves the cached
    /// column alone — no frame-time key, no re-arm. A root fact that did
    /// move (a row menu opening) re-renders it.
    #[gpui::test]
    fn a_root_notify_with_no_list_change_leaves_the_column_alone(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("root-notify");
        let (baaz, pane, vc) = rooted_window(cx, args);
        vc.update(|_, cx| baaz.update(cx, |h, cx| load_sessions(h, 5, cx)));
        vc.run_until_parked();
        let renders = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| pane.read(cx).renders);
        let before = renders(vc);
        take_baaz_root_renders();
        for _ in 0..3 {
            vc.update(|_, cx| baaz.update(cx, |_, cx| cx.notify()));
            vc.run_until_parked();
        }
        assert!(take_baaz_root_renders() > 0, "the root re-rendered");
        assert_eq!(renders(vc), before, "the cached column did not re-render");
        // A fact only the root knows moved: the column takes it.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_row_menu_for("s1".to_owned(), cx)));
        vc.run_until_parked();
        assert!(renders(vc) > before, "an opened row menu re-renders the column");
        assert_eq!(vc.update(|_, cx| pane.read(cx).model(cx).row_menu), Some("s1".into()));
        b3c_teardown(&dir, guard, old);
    }

    /// Plan 4.9b: the owners notify, not the root. A notify of an owner
    /// the root reads in every frame — the overlay stack, the browser, the
    /// right pane, the Settings page — re-renders the root on its own, in
    /// Sessions and in Browse, so a handler that changed one of them (and
    /// facts of the root's own on the same path) needs no root notify.
    #[gpui::test]
    fn an_owner_the_root_draws_redraws_it_without_a_root_notify(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("owner-redraws-root");
        let (baaz, _pane, vc) = rooted_window(cx, args);
        vc.run_until_parked();
        let root_notifies = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let count = root_notifies.clone();
        let _watch = vc.update(|_, cx| cx.observe(&baaz, move |_, _| count.set(count.get() + 1)));
        for mode in [crate::layout::Mode::Sessions, crate::layout::Mode::Browse] {
            vc.update(|_, cx| crate::layout::Layout::update(cx, |layout| layout.mode = mode));
            vc.run_until_parked();
            let owners: Vec<(&str, gpui::AnyEntity)> = vc.update(|_, cx| {
                let h = baaz.read(cx);
                vec![
                    ("overlays", h.overlays.clone().into_any()),
                    ("browser", h.browser.clone().into_any()),
                    ("right pane", h.right_pane.clone().into_any()),
                    ("settings page", h.settings_page.clone().into_any()),
                    ("session list", h.session_list.clone().into_any()),
                    ("connect screen", h.connect_screen.clone().into_any()),
                    ("skills page", h.skills_page.clone().into_any()),
                    ("provider model", h.provider_model.clone().into_any()),
                    ("terminal dock", h.terminal_dock.clone().into_any()),
                ]
            });
            for (name, owner) in owners {
                take_baaz_root_renders();
                root_notifies.set(0);
                vc.update(|_, cx| cx.notify(owner.entity_id()));
                vc.run_until_parked();
                assert!(take_baaz_root_renders() > 0, "a {name} notify re-rendered the root in {mode:?}");
                assert_eq!(root_notifies.get(), 0, "the root itself was never notified ({name}, {mode:?})");
            }
        }
        // The control: an entity the root never reads redraws nothing.
        let stranger = vc.update(|_, cx| cx.new(|_| ()));
        vc.run_until_parked();
        take_baaz_root_renders();
        vc.update(|_, cx| stranger.update(cx, |_, cx| cx.notify()));
        vc.run_until_parked();
        assert_eq!(take_baaz_root_renders(), 0, "a notify of an entity the root does not read redraws nothing");
        b3c_teardown(&dir, guard, old);
    }

    /// Plan 4.9c: the sign-in flow is its own entity. A step of the flow
    /// (here the device flow failing) notifies the `Login` the Muse sheet
    /// draws: the window redraws, the sheet reads the new state, and the
    /// root is never notified.
    #[gpui::test]
    fn the_login_flow_redraws_its_sheet_without_notifying_the_root(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("login-entity-redraws");
        let (baaz, _pane, vc) = rooted_window(cx, args);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_muse_sheet(cx)));
        vc.run_until_parked();
        let login = vc.update(|_, cx| baaz.read(cx).login.clone());
        let (root_notifies, login_notifies) = (std::rc::Rc::new(std::cell::Cell::new(0usize)), std::rc::Rc::new(std::cell::Cell::new(0usize)));
        let (r, l) = (root_notifies.clone(), login_notifies.clone());
        let _watch = vc.update(|_, cx| {
            (cx.observe(&baaz, move |_, _| r.set(r.get() + 1)), cx.observe(&login, move |_, _| l.set(l.get() + 1)))
        });
        take_baaz_root_renders();
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.set_login_state(aui::screens::LoginState::Error { message: "Sign-in failed.".into(), method: None }, cx)
            })
        });
        vc.run_until_parked();
        assert!(login_notifies.get() > 0, "the flow's own entity was notified");
        assert!(take_baaz_root_renders() > 0, "the window redrew the sheet");
        assert_eq!(root_notifies.get(), 0, "the root itself was never notified");
        assert_eq!(vc.update(|_, cx| login.read(cx).state_name()), "error");
        b3c_teardown(&dir, guard, old);
    }

    /// Plan 4.9c: the sidebar pane owns the divider's drag. A press and a
    /// move notify the pane, whose shell (the root, its ancestor) redraws
    /// at the new width; the release persists through the layout global.
    /// The root is never notified along the way.
    #[gpui::test]
    fn a_sidebar_drag_redraws_the_shell_without_notifying_the_root(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("sidebar-drag-redraws");
        let (baaz, pane, vc) = rooted_window(cx, args);
        let root_notifies = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let count = root_notifies.clone();
        let _watch = vc.update(|_, cx| cx.observe(&baaz, move |_, _| count.set(count.get() + 1)));
        let start = vc.update(|_, cx| baaz.read(cx).sidebar_width(cx));
        take_baaz_root_renders();
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.begin_resize(start, cx)));
        vc.run_until_parked();
        assert!(vc.update(|_, cx| pane.read(cx).resize.active), "the pane holds the drag");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.drag_resize(start + 24.0, cx)));
        vc.run_until_parked();
        assert!(take_baaz_root_renders() > 0, "the shell redrew under the drag");
        let moved = vc.update(|_, cx| baaz.read(cx).sidebar_width(cx));
        assert!((moved - start).abs() > 1.0, "the divider followed the pointer: {start} -> {moved}");
        assert_eq!(root_notifies.get(), 0, "the press and the move never notified the root");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.end_resize(cx)));
        vc.run_until_parked();
        assert!(!vc.update(|_, cx| pane.read(cx).resize.active), "the release settled it");
        assert_eq!(
            vc.update(|_, cx| crate::layout::Layout::get(cx).sidebar_width),
            Some(moved),
            "the release persisted the width"
        );
        // The release's layout write reaches the root through the global's
        // observer (every preference write does), never a notify of the
        // drag's own.
        b3c_teardown(&dir, guard, old);
    }

    #[gpui::test]
    fn the_browse_model_files_tabs_into_folders_pins_and_task_folders(cx: &mut TestAppContext) {
        use crate::browser::Owner;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-model");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let (filed, pinned, session) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
                h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
                h.browser.update(cx, |browser, _| browser.tabs.ensure_spaces(|_| None));
                let space = h.browser.read(cx).browse_space();
                let folder = h.browser.update(cx, |bv, _| bv.tabs.add_folder(crate::browser::tabs::PERSONAL, "Reading"));
                let filed = h.browser.update(cx, |browser, _| browser.tabs.open(space.clone(), Owner::Person, "https://filed.example/", None));
                let pinned = h.browser.update(cx, |browser, _| browser.tabs.open(space, Owner::Person, "https://pinned.example/", None));
                let session = h.browser.update(cx, |browser, _| browser.tabs.open(None, Owner::Session("sess-1".into()), "https://task.example/", None));
                h.browser.update(cx, |browser, _| assert!(browser.tabs.set_folder(filed, Some(&folder))));
                h.browser.update(cx, |browser, _| assert!(browser.tabs.set_pinned(pinned, true)));
                (filed, pinned, session)
            })
        });
        let browse = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx).browse));
        let browse = browse.expect("Browse mode carries a browse model");
        assert_eq!(browse.folders.len(), 1);
        assert_eq!(browse.folders[0].name.as_ref(), "Reading");
        let ids: Vec<String> = browse.folders[0].tabs.iter().map(|t| t.id.to_string()).collect();
        assert_eq!(ids, vec![browse_tab_key(filed)]);
        assert_eq!(browse.pins.len(), 1);
        assert_eq!(browse.pins[0].id.as_ref(), browse_tab_key(pinned));
        assert!(browse.tabs.is_empty(), "nothing is left loose in Today");
        assert_eq!(browse.task_folders.len(), 1);
        assert_eq!(browse.task_folders[0].id.as_ref(), "sess-1");
        assert_eq!(browse.task_folders[0].tabs[0].id.as_ref(), browse_tab_key(session));
        b3c_teardown(&dir, guard, old);
    }

    /// Five sessions, newest first by `updated` (s0 newest), s2 waiting on
    /// an approval; Browse mode with its tabs loaded.
    fn context_sessions(h: &mut Harness, cx: &mut gpui::App) {
        let now = chrono::Local::now();
        h.session_list.update(cx, |list, _| {
            list.sessions = (0..5)
                .map(|i| {
                    let mut entry = list_entry(&format!("s{i}"), false);
                    entry.updated = now - chrono::Duration::minutes(i);
                    if i == 2 {
                        entry.approval_command = Some("cargo test".into());
                    }
                    entry
                })
                .collect();
            list.sessions_loaded = true;
            list.index_loaded = true;
        });
        h.invalidate_list(cx);
        h.browser.update(cx, |browser, _| browser.tabs_loaded = true);
        h.browser.update(cx, |browser, _| browser.tabs.ensure_spaces(|_| None));
    }

    /// docs/82 8.4: Browse's footer says how many sessions wait and names
    /// the session Sessions stood on — the newest one with nothing open,
    /// the open one once a session is open — with its row's state.
    #[gpui::test]
    fn the_browse_footer_names_what_waits_and_the_session_left(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-footer");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let footer = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_facts(cx).browse.expect("Browse carries its model")))
        };
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse);
            })
        });
        let browse = footer(vc);
        assert_eq!(browse.needs_you, 1, "one session waits");
        let last = browse.last_session.expect("a session to go back to");
        assert_eq!(last.id.as_ref(), "s0", "nothing open: the newest session");
        assert_eq!(last.state, aui_tokens::AgentState::Idle);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.resume_quiet("s2".into(), window, cx)));
        vc.update(|_, cx| baaz.update(cx, |_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = crate::layout::Mode::Browse)));
        let last = footer(vc).last_session.expect("the open session");
        assert_eq!(last.id.as_ref(), "s2", "the open session is the one Sessions stood on");
        assert_eq!(last.state, aui_tokens::AgentState::Waiting, "its row's mark: needs you");
        // Nothing waiting: no "Needs you" row.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.session_list.update(cx, |list, _| list.sessions.iter_mut().for_each(|e| e.approval_command = None));
                h.invalidate_list(cx);
            })
        });
        assert_eq!(footer(vc).needs_you, 0);
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.4: the footer's rows go back to Sessions — the last
    /// session on that session, "Needs you" with its filter on and the
    /// first waiting session open.
    #[gpui::test]
    fn the_browse_footer_rows_go_back_to_sessions(cx: &mut TestAppContext) {
        use aui::screens::sidebar::{BrowseIntent, SidebarIntent};
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-footer-routes");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                h.set_mode(crate::layout::Mode::Browse, window, cx);
                h.sidebar_intent(SidebarIntent::Browse(BrowseIntent::LastSession("s1".into())), window, cx);
                assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Sessions);
                assert_eq!(h.active_id(cx).as_deref(), Some("s1"), "back on that session");
                h.set_mode(crate::layout::Mode::Browse, window, cx);
                h.sidebar_intent(SidebarIntent::Browse(BrowseIntent::NeedsYou), window, cx);
                assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Sessions);
                assert!(crate::attention::Notifier::filter_on(cx), "only what waits is listed");
                assert_eq!(h.active_id(cx).as_deref(), Some("s2"), "the first waiting session opens");
                h.toggle_needs_you_filter(cx);
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.4: a row's page count is the tabs its session owns, and
    /// it follows ownership — a kept tab (`Tabs::move_to`) leaves the
    /// count, a closed one too.
    #[gpui::test]
    fn a_rows_page_count_follows_tab_ownership(cx: &mut TestAppContext) {
        use crate::browser::Owner;
        use crate::sidebar::SessionRow as _;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("page-count");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let pages = |vc: &mut gpui::VisualTestContext, id: &str| {
            vc.update(|_, cx| {
                baaz.update(cx, |h, cx| {
                    let row = h.visible_sessions(cx).iter().find(|e| e.id == id).cloned().expect("the row");
                    assert_eq!(row.summary(chrono::Local::now()).pages, row.pages, "the summary carries it");
                    row.pages
                })
            })
        };
        let (a, b) = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                let owner = Owner::Session("s1".into());
                let a = h.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://a.example/", None));
                let b = h.browser.update(cx, |browser, _| browser.tabs.open(None, owner, "https://b.example/", None));
                (a, b)
            })
        });
        assert_eq!(pages(vc, "s1"), 2);
        assert_eq!(pages(vc, "s0"), 0, "a session with no tabs owns no pages");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.browser.update(cx, |browser, _| assert!(browser.tabs.move_to(a, Owner::Person, None)))));
        assert_eq!(pages(vc, "s1"), 1, "a kept tab is the person's");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.browser.update(cx, |browser, _| browser.tabs.close(b))));
        assert_eq!(pages(vc, "s1"), 0, "a closed tab leaves the count");
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.4: "N pages" opens them — one on the session's Page in
    /// Sessions, several in Browse on the session's own tab.
    #[gpui::test]
    fn a_page_count_opens_the_page_or_browse(cx: &mut TestAppContext) {
        use crate::browser::Owner;
        use aui::nav::RowAction;
        use aui::screens::sidebar::SidebarIntent;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("page-count-open");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                let owner = Owner::Session("s1".into());
                let one = h.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://a.example/", None));
                h.sidebar_intent(SidebarIntent::RowAction("s1".into(), RowAction::Pages), window, cx);
                assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Sessions);
                assert_eq!(h.active_id(cx).as_deref(), Some("s1"), "the session opens");
                assert!(crate::layout::Layout::get(cx).right_open, "its pane opens");
                assert_eq!(crate::layout::right_kind(crate::layout::Layout::get(cx)), crate::layout::RightKind::Browser, "on its Page");
                assert_eq!(h.browser.read(cx).tabs.active_of(&owner, None), Some(one));
                let two = h.browser.update(cx, |browser, _| browser.tabs.open(None, owner.clone(), "https://b.example/", None));
                h.sidebar_intent(SidebarIntent::RowAction("s1".into(), RowAction::Pages), window, cx);
                assert_eq!(crate::layout::Layout::get(cx).mode, crate::layout::Mode::Browse, "several open in Browse");
                assert_eq!(h.browser.read(cx).browse_session_tab, Some(two), "on the session's active page");
                assert!(!h.open_session_pages("s0".into(), window, cx), "no pages: nothing opens");
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// Two adopted projects under the hermetic state dir, s0..s4 listed in
    /// the first (their folder its root).
    fn menu_projects(h: &mut Harness, dir: &std::path::Path, cx: &mut gpui::App) -> (String, String) {
        let (a, b) = (dir.join("alpha"), dir.join("beta"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let (a_id, b_id) = h.session_list.update(cx, |list, _| (list.projects.add(&a).id.clone(), list.projects.add(&b).id.clone()));
        let root = crate::projects::canonical_str(&a.to_string_lossy());
        h.session_list.update(cx, |list, _| {
            for entry in &mut list.sessions {
                entry.workspace = Some(root.clone());
                entry.project = Some(a_id.clone());
            }
        });
        h.invalidate_list(cx);
        (a_id, b_id)
    }

    /// docs/82 8.5: the row menu offers per capability and state — Fork only
    /// where the provider forks and a turn exists, Continue in… only on a
    /// started session, nothing but Unarchive / Reveal / Delete on an
    /// archived row, Delete only where Baaz keeps the record.
    #[gpui::test]
    fn the_row_menu_offers_what_works_for_the_session(cx: &mut TestAppContext) {
        use crate::providers::ProviderId;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("row-menu-offers");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                let (alpha, beta) = menu_projects(h, &dir, cx);
                let offers = h.row_menu_offers("s1", cx);
                let fork = crate::providers::offers(ProviderId::Muse, provider::Capability::ForkSession);
                assert_eq!(offers.fork, fork, "Fork follows the provider's capability");
                assert!(offers.reveal, "its folder exists");
                assert!(!offers.delete, "Baaz keeps no record of a session its provider lists");
                assert!(!offers.archived);
                let targets: Vec<(Option<String>, bool)> =
                    offers.move_to.iter().map(|t| (t.project.as_ref().map(|p| p.to_string()), t.current)).collect();
                assert!(targets.contains(&(Some(alpha), true)), "its own project, checked: {targets:?}");
                assert!(targets.contains(&(Some(beta), false)), "the other one: {targets:?}");
                assert!(targets.iter().all(|(project, _)| project.is_some()), "no Unfiled under a project root: {targets:?}");
                // A provider that cannot fork offers no Fork.
                let cannot = ProviderId::all().into_iter().find(|id| !crate::providers::offers(*id, provider::Capability::ForkSession));
                if let Some(cannot) = cannot {
                    h.session_list.update(cx, |list, _| list.sessions.iter_mut().filter(|e| e.id == "s1").for_each(|e| e.provider = cannot));
                    assert!(!h.row_menu_offers("s1", cx).fork, "{cannot:?} cannot fork");
                }
                // A draft (no turns) has nothing to fork or continue.
                h.session_list.update(cx, |list, _| list.sessions.iter_mut().filter(|e| e.id == "s3").for_each(|e| e.turns = 0));
                let draft = h.row_menu_offers("s3", cx);
                assert!(!draft.fork && !draft.continue_in, "a draft offers no Fork and no Continue in…");
                // An archived row: Unarchive, Reveal, Delete only.
                h.session_list.update(cx, |list, _| list.sessions.iter_mut().filter(|e| e.id == "s4").for_each(|e| e.archived = true));
                let archived = h.row_menu_offers("s4", cx);
                assert!(archived.archived && !archived.fork && !archived.continue_in && archived.move_to.is_empty());
                // A recorded session (a provider with no list of its own) can be deleted.
                h.session_list.update(cx, |list, _| {
                    crate::provider_sessions::upsert_open(&mut list.provider_sessions, &mut list.overrides, "codex", "s0", None, None, None);
                });
                assert!(h.row_menu_offers("s0", cx).delete);
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.5: a right-click opens the same menu as the `⋯`, at the
    /// pointer, with the same offers.
    #[gpui::test]
    fn a_right_click_opens_the_rows_menu_at_the_pointer(cx: &mut TestAppContext) {
        use aui::screens::sidebar::SidebarIntent;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("row-menu-right-click");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let at = gpui::point(px(120.0), px(340.0));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                h.sidebar_intent(SidebarIntent::RowAction("s1".into(), aui::nav::RowAction::More), window, cx);
                let dots = h.sidebar_facts(cx);
                assert_eq!(dots.row_menu.as_deref(), Some("s1"));
                assert_eq!(dots.row_menu_at, None, "the ⋯ seats it under itself");
                h.sidebar_intent(SidebarIntent::RowMenuAt("s1".into(), at), window, cx);
                let right = h.sidebar_facts(cx);
                assert_eq!(right.row_menu.as_deref(), Some("s1"), "the same row's menu");
                assert_eq!(right.row_menu_at, Some(at), "at the pointer");
                assert_eq!(right.row_menu_offers, dots.row_menu_offers, "the same rows");
                h.sidebar_intent(SidebarIntent::RowMenuAt("s2".into(), at), window, cx);
                assert_eq!(h.open_row_menu(cx).as_deref(), Some("s2"), "another row's right-click replaces it");
            })
        });
        let model = vc.update(|_, cx| {
            baaz.update(cx, |h, cx| h.push_sidebar_facts(cx));
            baaz.read(cx).sidebar_pane.read(cx).model(cx)
        });
        assert_eq!(model.row_menu.as_deref(), Some("s2"));
        assert_eq!(model.row_menu_at, Some(at), "the screen draws it at the pointer");
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.5: Delete… asks first and says exactly what goes; its
    /// button forgets the session; Move to project lists the row under
    /// the project picked; Reveal folder hands the folder to the helper.
    #[gpui::test]
    fn delete_move_and_reveal_do_what_the_rows_say(cx: &mut TestAppContext) {
        use aui::screens::sidebar::SidebarIntent;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("row-menu-actions");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                let (_, beta) = menu_projects(h, &dir, cx);
                // Move to project: listed under beta now, its folder unchanged.
                h.sidebar_intent(SidebarIntent::MoveToProject("s1".into(), Some(beta.clone().into())), window, cx);
                let row = h.session_list.read(cx).sessions.iter().find(|e| e.id == "s1").cloned().unwrap();
                assert_eq!(row.project.as_deref(), Some(beta.as_str()), "listed under the project picked");
                assert!(row.workspace.as_deref().is_some_and(|w| w.ends_with("alpha")), "its folder stays");
                assert!(!h.move_session_to_project("s1", None, cx), "Unfiled is not offered under a project root");
                // Reveal folder: the helper gets the session's folder.
                crate::open_in::REVEALED.with(|r| r.borrow_mut().clear());
                h.sidebar_intent(SidebarIntent::RowAction("s1".into(), aui::nav::RowAction::Reveal), window, cx);
                let revealed = crate::open_in::REVEALED.with(|r| r.borrow().clone());
                assert_eq!(revealed.len(), 1);
                assert!(revealed[0].ends_with("alpha"), "{revealed:?}");
                // Delete…: the confirm first, naming what goes.
                h.session_list.update(cx, |list, _| {
                    crate::provider_sessions::upsert_open(&mut list.provider_sessions, &mut list.overrides, "codex", "s0", None, None, None);
                });
                h.sidebar_intent(SidebarIntent::RowAction("s0".into(), aui::nav::RowAction::Delete), window, cx);
                let dialog = h.overlays.read(cx).dialog().cloned().expect("Delete asks first");
                assert_eq!(dialog.action, crate::overlays::DialogAction::DeleteSession);
                assert_eq!(dialog.archive_target.as_deref(), Some("s0"));
                assert_eq!(dialog.primary, "Delete");
                for goes in ["turns", "attachments", "pictures", "hand-off snapshot", "checkpoints", "Trash"] {
                    assert!(dialog.detail.contains(goes), "the confirm names {goes}: {}", dialog.detail);
                }
                assert!(!dialog.title.contains("Codex") && !dialog.detail.contains("Codex"), "no provider name in the copy");
                assert!(h.session_list.read(cx).sessions.iter().any(|e| e.id == "s0"), "nothing goes before the button");
                h.confirm_delete_dialog(dialog.archive_target.clone(), window, cx);
                assert!(!h.session_list.read(cx).sessions.iter().any(|e| e.id == "s0"), "its button deletes");
                assert!(!h.session_list.read(cx).provider_sessions.contains_key("s0"));
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// docs/82 8.5: Fork and Continue in… from a row open that session and
    /// then its fork picker / its provider menu (the hand-off).
    #[gpui::test]
    fn fork_and_continue_in_open_the_session_then_act(cx: &mut TestAppContext) {
        use aui::screens::sidebar::SidebarIntent;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("row-menu-follow-ups");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                h.sidebar_intent(SidebarIntent::RowAction("s3".into(), aui::nav::RowAction::ContinueIn), window, cx);
                assert_eq!(h.active_id(cx).as_deref(), Some("s3"), "the session opens");
                assert!(h.overlays.read(cx).is_open(crate::overlays::MenuKind::Provider), "its provider menu opens: Continue in…");
                h.sidebar_intent(SidebarIntent::RowAction("s1".into(), aui::nav::RowAction::Fork), window, cx);
                assert_eq!(h.active_id(cx).as_deref(), Some("s1"));
                assert_eq!(
                    h.overlays.read(cx).palette().map(|p| p.kind),
                    Some(crate::overlays::PaletteKind::Fork),
                    "its fork picker opens"
                );
            })
        });
        b3c_teardown(&dir, guard, old);
    }

    /// The follow-up the Stage 6 gate listed: after Archive, the centre
    /// header named the archived session — the open target outlived the
    /// view. It names nothing (or what opened in its place) now.
    #[gpui::test]
    fn the_header_after_archive_names_what_is_open(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("archive-header");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                context_sessions(h, cx);
                h.resume_quiet("s1".into(), window, cx);
                assert_eq!(h.session_header_label(cx).as_deref(), Some("s1"));
                h.archive_session("s1".into(), Some(window), cx);
                let header = h.session_header_label(cx);
                assert_ne!(header.as_deref(), Some("s1"), "the archived title leaves the header");
                assert_eq!(header, h.active_id(cx).and_then(|id| h.collapsed_head_label(&id, cx)), "it names what is open");
                assert_eq!(h.sidebar_facts(cx).selected, h.active_id(cx), "the selection agrees");
            })
        });
        b3c_teardown(&dir, guard, old);
    }
}

/// The footer's `(name, email, initial, plan, tile)`: the connected account
/// of `preferred` (the new-session provider), else of the first connected
/// enabled provider in the one provider order; "Checking providers…" while nothing
/// has been probed (the tile spins); "No provider connected" otherwise (the
/// tile's amber dot: the provider problem, docs/83 §4). Muse is one
/// provider among three here, never the app's account. Pure, so tests
/// drive it.
pub(crate) fn footer_identity(
    statuses: &[crate::provider_status::ProviderStatus],
    preferred: &str,
    muse_plan: impl Fn() -> String,
) -> (String, String, String, String, AccountStatus) {
    use crate::provider_status::{Auth as ProviderAuth, Headline};
    use crate::providers::ProviderId;
    let connected = |status: &&crate::provider_status::ProviderStatus| {
        status.enabled && matches!(status.auth, ProviderAuth::SignedIn { .. })
    };
    let chosen = statuses
        .iter()
        .filter(connected)
        .find(|status| status.provider.as_str() == preferred)
        .or_else(|| ProviderId::all().into_iter().find_map(|id| statuses.iter().filter(connected).find(|s| s.provider == id)));
    if let Some(status) = chosen {
        if let ProviderAuth::SignedIn { email, plan, .. } = &status.auth {
            let name = email.clone().unwrap_or_else(|| status.provider.label().to_owned());
            let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            // The billing-tier provider's plan reads its tier card first.
            let plan = if status.provider.has(provider::ProviderTrait::BillingTier) {
                Some(muse_plan()).filter(|label| !label.is_empty()).or_else(|| plan.clone())
            } else {
                plan.clone()
            };
            return (name, email.clone().unwrap_or_default(), initial, plan.unwrap_or_default(), AccountStatus::Ready);
        }
    }
    let checking = statuses.iter().filter(|s| s.enabled).all(|s| s.headline() == Headline::Checking);
    let (name, tile) = if checking && !statuses.is_empty() {
        ("Checking providers…", AccountStatus::Checking)
    } else {
        ("No provider connected", AccountStatus::Attention)
    };
    (name.to_owned(), String::new(), "?".to_owned(), String::new(), tile)
}

#[cfg(test)]
mod footer_identity_tests {
    use super::footer_identity;
    use aui::screens::sidebar::AccountStatus;
    use crate::provider_status::{Auth, Installed, ProviderStatus};
    use crate::providers::ProviderId;

    fn status(id: ProviderId, auth: Auth) -> ProviderStatus {
        let mut status = ProviderStatus::checking(id);
        status.installed = Installed::yes("1.0", "/bin/x");
        status.auth = auth;
        status.checked_at = Some(1);
        status
    }

    fn signed_in(email: &str, plan: &str) -> Auth {
        Auth::SignedIn { email: Some(email.into()), plan: Some(plan.into()), method: None, source: None }
    }

    /// Muse signed out, Claude Code and Codex connected: the footer names
    /// the new-session provider's account, not "Not signed in".
    #[test]
    fn a_signed_out_muse_is_not_the_apps_account() {
        let statuses = [
            status(ProviderId::Muse, Auth::SignedOut),
            status(ProviderId::ClaudeCode, signed_in("ada@example.com", "Claude Max")),
            status(ProviderId::Codex, signed_in("bo@example.com", "prolite")),
        ];
        let (name, _, initial, plan, tile) = footer_identity(&statuses, "codex", String::new);
        assert_eq!((name.as_str(), initial.as_str(), plan.as_str()), ("bo@example.com", "B", "prolite"));
        assert_eq!(tile, AccountStatus::Ready);
        let (name, ..) = footer_identity(&statuses, "muse", String::new);
        assert_eq!(name, "ada@example.com", "a signed-out preferred provider falls through to the first connected");
    }

    #[test]
    fn nothing_connected_says_so_without_naming_a_provider() {
        let statuses = [status(ProviderId::Muse, Auth::SignedOut), status(ProviderId::Codex, Auth::SignedOut)];
        let none = footer_identity(&statuses, "muse", String::new);
        assert_eq!((none.0.as_str(), none.4), ("No provider connected", AccountStatus::Attention), "the amber dot, never a red tile");
        let checking = [ProviderStatus::checking(ProviderId::Muse), ProviderStatus::checking(ProviderId::Codex)];
        let probing = footer_identity(&checking, "muse", String::new);
        assert_eq!((probing.0.as_str(), probing.4), ("Checking providers…", AccountStatus::Checking), "the tile spins");
    }
}


/// A task folder's state and face (docs/27 §4.3, owner 8 Nov: round faces
/// with the word): needs you, then paused while the person drives, then
/// typing or reading while it works, else done.
pub(crate) fn task_face_for(waiting: bool, held: bool, running: bool, typing: bool) -> (AgentState, aui::browse::TaskFace) {
    use aui::browse::TaskFace;
    if waiting {
        (AgentState::Waiting, TaskFace::NeedsYou)
    } else if held {
        (AgentState::Running, TaskFace::Paused)
    } else if running && typing {
        (AgentState::Running, TaskFace::Typing)
    } else if running {
        (AgentState::Running, TaskFace::Reading)
    } else {
        (AgentState::Idle, TaskFace::Done)
    }
}

#[cfg(test)]
mod task_face_tests {
    use super::task_face_for;
    use aui::browse::TaskFace;

    #[test]
    fn needs_you_wins_then_paused_then_work_then_done() {
        assert_eq!(task_face_for(true, true, true, true).1, TaskFace::NeedsYou);
        assert_eq!(task_face_for(false, true, true, true).1, TaskFace::Paused);
        assert_eq!(task_face_for(false, false, true, true).1, TaskFace::Typing);
        assert_eq!(task_face_for(false, false, true, false).1, TaskFace::Reading);
        assert_eq!(task_face_for(false, false, false, false).1, TaskFace::Done);
        assert_eq!(TaskFace::Done.status_word(), "done");
    }
}
