//! The sidebar column: what it draws, and the two menus that hang off it.
//!
//! Everything here is composition over state [`Harness`] already holds — the
//! joined session rows, the filter toggles, the open rename, and the
//! signed-in identity. It is the shell's left half plus the
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

use aui::browse::split_address;
use aui::composer::provider_display_name;
use aui::data::{icon_button, ButtonSize};
use aui::nav::{
    SESSION_DETAIL_GAP, anchored_session_detail_at_sidebar, dense_field, ensure_row_visible, flatten_sidebar,
    row_index_for_session, GroupAction, RowAction, SidebarRow,
};
use aui::screens::UsageRowState;
use aui::screens::sidebar::{
    screen_item_count, sidebar_screen, Account, AccountAction,
    BrowseEditing, BrowseFolder, BrowseIntent, BrowseModel, BrowseNaming, BrowsePin, BrowseSpace, BrowseTab, Bylines,
    EditingState, FolderKind, GroupKey, Mode, NavItem, ProviderUsage, SidebarIntent, SidebarModel, SortKey,
    TaskFolderAction, UsageLine, ViewOption, ViewOptions,
};
use aui_tokens::AgentState;
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, Palette};
use gpui::{
    div, prelude::*, px, AnyElement, Bounds, Context, ElementId, Entity, Focusable, ListOffset, Pixels, Render,
    SharedString, WeakEntity, Window,
};
use gpui_kit::base::input::TextareaState;

use crate::app::{ConfirmRename, Harness, RENAME_CONTEXT};
use crate::login::Auth;
use crate::overlays::MenuKind;
use crate::sidebar::{Grouping, SessionEntry};
use crate::tier::Tier;

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
/// How long the sidebar's new rows take to arrive after a mode switch.
pub(crate) const MODE_FADE: std::time::Duration = std::time::Duration::from_millis(80);

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

/// Whether a missed reveal stays armed: only while its id is the open
/// session's and the window holds no row for it yet (a brand-new session
/// before its first send). Pure, so tests pin the boundary without a
/// window; [`Harness::reveal_sidebar_row`] reads it on every miss.
pub(crate) fn rowless_reveal_stays_armed(
    active_id: Option<&str>,
    reveal_id: &str,
    known: bool,
) -> bool {
    active_id == Some(reveal_id) && !known
}

/// The group head (or header) row for a session with no session row: a
/// closed group holds its sessions out of the flattened model. Pure — the
/// [`Harness`] method below passes its own stores — so tests drive each
/// trigger's scroll target without a window.
///
/// A rowless session still names its project: the override Baaz files for
/// a draft before its first send, then the provider-lane record, then the
/// current project (a new session starts in it) — before giving up. The
/// head is revealed, never auto-expanded.
pub(crate) fn reveal_head_row(
    sessions: &[SessionEntry],
    overrides: &crate::sessions::Overrides,
    provider_sessions: &crate::provider_sessions::ProviderSessionStore,
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
                provider_sessions,
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

/// One From-sessions group as the cached sidebar keys it: the session and
/// its tabs' `(id, session-active)`. Labels never enter the key: a retitle
/// moves the tabs-model generation (and the visible-rows pointer on a
/// session rename), which is what the key compares.
pub(crate) type BrowseGroupPrint = (String, Vec<(u64, bool)>);

/// What the sidebar pane shows, as an equality key.
///
/// Every input `render_sidebar` reads: the cached rows and grouping behind
/// their `Rc` pointers (this comparison calls both getters every frame, so
/// an `invalidate_list` rebuild — or the minute rollover regrouping — shows
/// up as a new pointer here), the selection, the rename, the one-shot
/// reveal, the footer's identity, and the current project. Compared
/// in `on_frame`, before anything draws; a few small clones per frame, no
/// rebuild. Nested entities inside the column (the rename editor, hover and
/// scroll state) notify through the view tree on their own and need no key.
/// The tier is deliberately not an input: the footer shows the person,
/// never the plan or the meter, so a usage tick must not dirty the column.
#[derive(PartialEq)]
pub(crate) struct SidebarKey {
    visible: usize,
    grouping: usize,
    selected: Option<String>,
    renaming: Option<String>,
    reveal: Option<String>,
    auth: (u8, String, String, String, bool),
    current_project: Option<String>,
    mode: crate::layout::Mode,
    browse_tabs: Vec<(u64, bool, bool, bool)>,
    tabs_generation: u64,
    browse_flags: Vec<(u64, bool, bool, bool, bool, bool)>,
    browse_groups: Vec<BrowseGroupPrint>,
    browse_expanded: Vec<String>,
    browse_session_tab: Option<u64>,
    tab_menu: Option<u64>,
    /// Bumped as favicons land, so the column repaints with them.
    favicons: u64,
    /// The v2 screen's own inputs: the lifted pinned rows, what the
    /// pointer is over (the screen swaps trailing slots on it), the
    /// sections' and menus' open state, the view options, the loading
    /// and collapsed flags, the Skills count, and the plan label.
    pinned: usize,
    hovered: Option<String>,
    hovered_group: Option<String>,
    hovered_projects_header: bool,
    hovered_today_header: bool,
    pinned_open: bool,
    view_menu_open: bool,
    account_menu_open: bool,
    view_options: (bool, bool, bool, crate::layout::GroupBy, crate::layout::SortBy, bool, usize),
    browse_menu: Option<aui::screens::sidebar::BrowseMenu>,
    browse_naming: Option<aui::screens::sidebar::BrowseNaming>,
    current_space: String,
    loading: bool,
    sidebar_open: bool,
    skills_on: usize,
    plan: Option<String>,
    /// The usage cards tick while the account menu is open: the minute.
    usage_minute: i64,
}

/// One person tab the Browse sidebar draws, with the live page facts
/// behind its row state. Shared with the horizontal strip's chips.
pub(crate) struct BrowseTabSnap {
    pub(crate) id: crate::browser::TabId,
    pub(crate) title: String,
    pub(crate) initial: String,
    pub(crate) active: bool,
    pub(crate) pinned: bool,
    pub(crate) loading: bool,
    /// The favicon's local file, once fetched ([`crate::browser::favicons`]).
    pub(crate) favicon: Option<String>,
}

/// One session tab under From sessions, still owned by its session.
pub(crate) struct BrowseSessionTabSnap {
    favicon: Option<String>,
    id: crate::browser::TabId,
    title: String,
    initial: String,
    active: bool,
}

impl SidebarKey {
    /// The Browse sidebar's fingerprint: the current project's person
    /// tabs as `(id, active, pinned, attention)` quadruples, beside the
    /// tabs-model generation. A select, a retitle, a (un)pin, an
    /// open/close or a background permission ask moves the key — retitles
    /// through the generation, never through a label string built per
    /// frame — so the cached pane repaints.
    pub(crate) fn browse_tabs_fingerprint(baaz: &Harness) -> Vec<(u64, bool, bool, bool)> {
        if !baaz.browser.tabs_loaded {
            return Vec::new();
        }
        let space = baaz.browse_space();
        let active =
            baaz.browser.tabs.active_of(&crate::browser::Owner::Person, space.as_deref());
        baaz.browser
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| (tab.id.0, Some(tab.id) == active, tab.pinned, tab.needs_attention))
            .collect()
    }

    /// The live page facts behind each person tab's row state, as
    /// `(id, loading, audible, crashed, discarded, attention)` — read off
    /// the registered webviews (attention off the tab), so a load
    /// finishing, a crash landing or a background permission ask repaints
    /// the cached pane. A tab with no live page reads discarded (asleep);
    /// a blank tab with no progress yet reads neither loading nor
    /// discarded.
    pub(crate) fn browse_flags_fingerprint(
        baaz: &Harness,
        cx: &gpui::App,
    ) -> Vec<(u64, bool, bool, bool, bool, bool)> {
        if !baaz.browser.tabs_loaded {
            return Vec::new();
        }
        let space = baaz.browse_space();
        baaz.browser
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| {
                let page = baaz.browser.states.get(&tab.id).map(|state| state.read(cx));
                let (loading, audible, crashed, live) = match page {
                    Some(page) => {
                        let progress = page.progress();
                        (
                            progress > 0.0 && progress < 1.0,
                            page.audible(),
                            page.crashed().is_some(),
                            true,
                        )
                    }
                    None => (false, false, false, false),
                };
                let discarded = !live && tab.url != crate::browser::tabs::BLANK && !tab.url.is_empty();
                (tab.id.0, loading, audible, crashed, discarded, tab.needs_attention)
            })
            .collect()
    }

    /// The From-sessions groups: one `(session, tabs)` pair per session
    /// in the current project that owns tabs, in first-seen order, each
    /// tab as `(id, session-active)`. A session tab opening or closing
    /// moves the key directly; a retitle moves it through the
    /// tabs-model generation.
    pub(crate) fn browse_groups_fingerprint(baaz: &Harness) -> Vec<BrowseGroupPrint> {
        if !baaz.browser.tabs_loaded {
            return Vec::new();
        }
        let space = baaz.browse_space();
        baaz.browser
            .tabs
            .session_tabs(space.as_deref())
            .iter()
            .map(|(session, tabs)| {
                let owner = crate::browser::Owner::Session(session.clone());
                let active = baaz.browser.tabs.active_of(&owner, None);
                let rows = tabs.iter().map(|tab| (tab.id.0, Some(tab.id) == active)).collect();
                (session.clone(), rows)
            })
            .collect()
    }
}

impl SidebarKey {
    /// The key the pane would render this frame.
    pub(crate) fn current(baaz: &Harness, cx: &gpui::App) -> Self {
        let visible = baaz.visible_sessions(cx);
        let grouping = baaz.sidebar_grouping(cx);
        let selected = baaz
            .pending_id
            .clone()
            .or_else(|| baaz.active.as_ref().map(|a| a.read(cx).session_id.clone()))
            .map(|id| baaz.chain_head(&id));
        let auth = match &baaz.auth {
            Auth::Probing => (0, String::new(), String::new(), String::new(), false),
            Auth::SignedOut => (1, String::new(), String::new(), String::new(), false),
            Auth::SignedIn(identity) => (
                2,
                identity.initial(),
                identity.display_name(),
                identity.email.clone(),
                identity.is_api_key(),
            ),
        };
        // The mode switch shows both counts, so both are key inputs: the
        // sessions in the current project (the switch's own count,
        // including rowless sessions created this run) and the project's
        // person tabs.
        let current_project = baaz.current_project.clone();
        // The Browse fingerprints cost model walks and label-free triples,
        // but only Browse shows them: outside Browse they stay empty, so
        // a Sessions frame never pays for the tab model at all.
        let browse_mode = baaz.mode == crate::layout::Mode::Browse;
        let (browse_tabs, tabs_generation, browse_flags, browse_groups) = if browse_mode {
            (
                Self::browse_tabs_fingerprint(baaz),
                baaz.browser.tabs.generation,
                Self::browse_flags_fingerprint(baaz, cx),
                Self::browse_groups_fingerprint(baaz),
            )
        } else {
            (Vec::new(), 0, Vec::new(), Vec::new())
        };
        let mut browse_expanded: Vec<String> = baaz.browse_from_expanded.iter().cloned().collect();
        browse_expanded.sort();
        let browse_session_tab = baaz.browse_session_tab.map(|id| id.0);
        let tab_menu = baaz.tab_menu.map(|id| id.0);
        let account_menu_open = baaz.overlays.read(cx).is_open(MenuKind::Account);
        Self {
            visible: Rc::as_ptr(&visible) as usize,
            grouping: Rc::as_ptr(&grouping) as usize,
            selected,
            renaming: baaz.renaming.clone(),
            reveal: baaz.reveal.clone(),
            auth,
            current_project,
            mode: baaz.mode,
            browse_tabs,
            tabs_generation,
            browse_flags,
            browse_groups,
            browse_expanded,
            browse_session_tab,
            tab_menu,
            favicons: baaz.browser.favicons.generation,
            pinned: Rc::as_ptr(&baaz.sidebar_pinned(cx)) as usize,
            hovered: baaz.sidebar_hovered.clone(),
            hovered_group: baaz.hovered_group.clone(),
            hovered_projects_header: baaz.hovered_projects_header,
            hovered_today_header: baaz.hovered_today_header,
            pinned_open: baaz.pinned_open,
            view_menu_open: baaz.overlays.read(cx).is_open(MenuKind::ViewOptions),
            account_menu_open,
            view_options: (
                baaz.show_empty,
                baaz.show_hidden,
                baaz.show_archived,
                baaz.effective_group_by(),
                baaz.layout.sort_by,
                baaz.layout.search_all_projects,
                baaz.hidden_count(),
            ),
            browse_menu: baaz.browse_menu.clone(),
            browse_naming: baaz.browse_naming.clone(),
            current_space: baaz.browser.tabs.current_space.clone(),
            loading: !(baaz.sessions_loaded && baaz.index_loaded),
            sidebar_open: baaz.sidebar_open,
            skills_on: baaz.skills.catalog.on_count(),
            plan: muse_plan_label(baaz.tier.as_ref()),
            usage_minute: if account_menu_open { crate::account_usage::now_secs() / 60 } else { 0 },
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
    sort_by: crate::layout::SortBy,
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
    /// Answers whether the key moved (the pane was re-armed).
    pub(crate) fn sync_sidebar_pane(&mut self, cx: &mut Context<Self>) -> bool {
        let key = SidebarKey::current(self, cx);
        if self.sidebar_key.as_ref() != Some(&key) {
            self.sidebar_key = Some(key);
            crate::log::boot_mark("pane-notify");
            self.sidebar_pane.update(cx, |_, cx| cx.notify());
            return true;
        }
        false
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

    /// A real wheel event over the sidebar: the person scrolled. Disarms any
    /// armed reveal and marks the list user-scrolled until the next
    /// activation, and dismisses the group menu and the hover card.
    pub(crate) fn note_sidebar_wheel(&mut self, cx: &mut Context<Self>) {
        self.reveal = None;
        self.reveal_unknown = None;
        self.sidebar_user_scrolled = true;
        self.dismiss_group_menu(cx);
        self.close_row_detail();
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
    /// The local favicon file for `tab`, once its icon has been fetched.
    fn favicon_file(&self, tab: &crate::browser::tabs::Tab) -> Option<String> {
        let url = tab.favicon.as_deref()?;
        Some(self.browser.favicons.file(url)?.to_string_lossy().into_owned())
    }

    pub(crate) fn browse_person_snaps(&self, cx: &gpui::App) -> Vec<BrowseTabSnap> {
        if !self.browser.tabs_loaded {
            return Vec::new();
        }
        let space = self.browse_space();
        let active =
            self.browser.tabs.active_of(&crate::browser::Owner::Person, space.as_deref());
        self.browser
            .tabs
            .person_tabs(space.as_deref())
            .iter()
            .map(|tab| {
                let page = self.browser.states.get(&tab.id);
                let loading = match page {
                    Some(state) => {
                        let progress = state.read(cx).progress();
                        progress > 0.0 && progress < 1.0
                    }
                    None => false,
                };
                BrowseTabSnap {
                    id: tab.id,
                    title: crate::browser::tabs::tab_label(&tab.title, &tab.url),
                    initial: crate::browser::tabs::tab_initial(&tab.title, &tab.url),
                    active: Some(tab.id) == active,
                    pinned: tab.pinned,
                    loading,
                    favicon: self.favicon_file(tab),
                }
            })
            .collect()
    }

    /// Every session in the current project that owns tabs, in
    /// first-seen order: the session id, its sidebar name (or "New
    /// session" when it has no row yet), and its tabs.
    fn browse_session_groups(&self, cx: &gpui::App) -> Vec<(String, String, Vec<BrowseSessionTabSnap>)> {
        if !self.browser.tabs_loaded {
            return Vec::new();
        }
        let visible = self.visible_sessions(cx);
        self.browser
            .tabs
            .session_tabs(None)
            .iter()
            .map(|(session, tabs)| {
                let owner = crate::browser::Owner::Session(session.clone());
                let active = self.browser.tabs.active_of(&owner, None);
                // The sidebar name, or "New session" while the session
                // has no row yet (an unsent session is filtered from the
                // list) — never the bare "Session".
                let label = visible
                    .iter()
                    .find(|entry| entry.id == *session)
                    .map(|entry| entry.label.clone())
                    .unwrap_or_else(|| crate::sidebar::UNNAMED.to_owned());
                let rows = tabs
                    .iter()
                    .map(|tab| BrowseSessionTabSnap {
                        favicon: self.favicon_file(tab),
                        id: tab.id,
                        title: crate::browser::tabs::tab_label(&tab.title, &tab.url),
                        initial: crate::browser::tabs::tab_initial(&tab.title, &tab.url),
                        active: Some(tab.id) == active,
                    })
                    .collect();
                (session.clone(), label, rows)
            })
            .collect()
    }

    /// The sidebar column as the library's v2 screen (D3): a
    /// [`SidebarModel`] built from Baaz state on every pane render, the
    /// screen's intents routed back to the handlers in this file. The host
    /// owns the one `ListState` ([`Self::sidebar_list`]) and the one
    /// `FluidHandle` ([`Self::sidebar_fluid`]) the screen renders through,
    /// both created once and kept; the list state resets on a regroup
    /// (`sync_sidebar_list`) and matches `screen_item_count` otherwise.
    pub(crate) fn render_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // One travel per frame (the scripted sweep feeds the accumulator;
        // a real wheel scrolls the library list directly).
        let user_scrolled = self.drain_sidebar_wheel();
        if user_scrolled {
            self.dismiss_group_menu(cx);
            self.close_row_detail();
        }
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
        // The cached grouping (pinned rows lifted out of it) and its
        // flattened rows: the caller-owned list state is synced to their
        // length — `reset` after a regroup, `splice` after a local insert.
        let grouping = self.sidebar_grouping(cx);
        let rows = flatten_sidebar(&grouping, false);
        let model = self.sidebar_model(cx);
        // Browse draws no session list: leave its state (and its scroll
        // offset) exactly as Sessions left it, so switching back lands where
        // the person was. Sessions syncs it to the rows; while loading the
        // screen draws skeleton rows the grouping does not have, and the
        // state follows the screen's count.
        if model.mode == Mode::Sessions {
            self.sync_sidebar_list(&rows, &grouping);
            let count = screen_item_count(&model);
            if self.sidebar_list.item_count() != count {
                self.sidebar_list.reset(count);
            }
        }
        // The one-shot reveal, exactly as before: outside activations only,
        // never during a gesture, a user scroll or a resize drag.
        if model.mode == Mode::Sessions && !self.sidebar_user_scrolled && !self.sidebar_gesture_active() && !self.resize.active {
            if let Some(reveal_id) = self.reveal.clone() {
                self.reveal_sidebar_row(&rows, &grouping, &reveal_id, cx);
            }
        }
        let baaz = cx.entity().downgrade();
        let screen = sidebar_screen(Self::SIDEBAR_VIEW_ID, model, self.sidebar_list.clone(), self.sidebar_fluid.clone())
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
                let _ = baaz.update(cx, |this, cx| this.sidebar_intent(intent, window, cx));
            });
        // The pane's own rect, once per frame: the hover card's side seat
        // hangs off its right edge.
        let pane_report = cx.entity().downgrade();
        let wheel = cx.entity().downgrade();
        // A mode switch: the new rows arrive on FF's swap timing (80 ms
        // ease-out), the one motion the Sessions | Browse switch has beside
        // its indicator's slide.
        let fade = match self.mode_fade_since {
            Some(since) => {
                let k = (cx.background_executor().now().duration_since(since).as_secs_f32() / MODE_FADE.as_secs_f32()).clamp(0.0, 1.0);
                if k < 1.0 {
                    window.request_animation_frame();
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
            .on_scroll_wheel(move |_, _, cx| {
                let _ = wheel.update(cx, |this, cx| this.note_sidebar_wheel(cx));
            })
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = pane_report.update(cx, |this, cx| {
                        Harness::note_trigger_bounds(&mut this.sidebar_bounds, bounds, cx);
                    });
                }
            })
            .child(screen)
            .into_any_element()
    }

    /// The sidebar's toggle, drawn by the shell in the sidebar's chrome row
    /// (open) and in the peek card's row (closed).
    pub(crate) fn render_sidebar_toggle(&self, cx: &mut Context<Self>) -> AnyElement {
        let show = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.toggle_sidebar(cx));
        // One label in both states: a state-specific one is captured when the
        // tooltip arms and then reads wrong for a moment after the click flips
        // the state (seen live 2026-10-13: "Show sidebar" over an open sidebar).
        let label = "Toggle sidebar  ⌘B";
        div()
            .id("hd-minimal-sidebar")
            .debug_selector(|| "hd-minimal-sidebar".into())
            .flex_none()
            .child(
                icon_button("sidebar-show", IconName::Sidebar)
                    .ghost()
                    .size(ButtonSize::Sm)
                    .accessibility_label(label)
                    .on_click(move |e, w, cx| show(e, w, cx)),
            )
            .into_any_element()
    }

    /// The screen's model from Baaz state: the grouping and pinned rows
    /// from the list cache, the nav rows, the selection and rename, the
    /// hover facts the intents below write, the view options from the
    /// filter flags, the account from the signed-in identity (its usage
    /// cards only while the menu is open), and Browse mode's rows.
    pub(crate) fn sidebar_model(&self, cx: &mut Context<Self>) -> SidebarModel {
        let view_menu_open = self.overlays.read(cx).is_open(MenuKind::ViewOptions);
        let account_menu_open = self.overlays.read(cx).is_open(MenuKind::Account);
        // One identity per chain: the selected row is the head (Y2a).
        let selected = self
            .pending_id
            .clone()
            .or_else(|| self.active.as_ref().map(|a| a.read(cx).session_id.clone()))
            .map(|id| self.chain_head(&id));
        let editing = self.renaming.clone().map(|session| EditingState {
            session: session.into(),
            draft: self.rename.read(cx).value().to_string().into(),
        });
        // The Skills row wears the on-count (D54): how many skills are on.
        let on = self.skills.catalog.on_count();
        let nav = vec![
            NavItem { id: NAV_PROJECTS.into(), label: "Add project".into(), icon: IconName::Folder, count: None, soon: false },
            NavItem { id: NAV_SKILLS.into(), label: "Skills".into(), icon: IconName::Slash, count: Some(on as u32), soon: false },
            NavItem { id: NAV_AUTOMATIONS.into(), label: "Automations".into(), icon: IconName::Zap, count: None, soon: true },
        ];
        let group_by = match self.effective_group_by() {
            crate::layout::GroupBy::Project => GroupKey::Project,
            crate::layout::GroupBy::Date => GroupKey::Date,
        };
        let sort_by = match self.layout.sort_by {
            crate::layout::SortBy::Recent => SortKey::Recent,
            crate::layout::SortBy::Created => SortKey::Created,
            crate::layout::SortBy::Name => SortKey::Name,
        };
        let mode = match self.mode {
            crate::layout::Mode::Sessions => Mode::Sessions,
            crate::layout::Mode::Browse => Mode::Browse,
        };
        let browse = (mode == Mode::Browse).then(|| self.browse_model(cx));
        SidebarModel {
            mode,
            search: None,
            nav,
            pinned: (*self.sidebar_pinned(cx)).clone(),
            pinned_open: self.pinned_open,
            grouping: self.sidebar_grouping(cx),
            selected: selected.map(SharedString::from),
            // The row a right-click menu belongs to stays lit while it is open
            // (canvas Menus board).
            hovered: match self.browse_menu.as_ref().map(|menu| &menu.target) {
                Some(aui::screens::sidebar::BrowseMenuTarget::Tab { id, .. }) if self.mode == crate::layout::Mode::Browse => Some(id.clone()),
                Some(aui::screens::sidebar::BrowseMenuTarget::Folder(id)) if self.mode == crate::layout::Mode::Browse => Some(format!("folder-{id}").into()),
                Some(aui::screens::sidebar::BrowseMenuTarget::TaskFolder { session, .. }) if self.mode == crate::layout::Mode::Browse => {
                    Some(format!("folder-{session}").into())
                }
                _ => self.sidebar_hovered.clone().map(SharedString::from),
            },
            hovered_group: self.hovered_group.clone().map(SharedString::from),
            hovered_projects_header: self.hovered_projects_header,
            highlighted: None,
            editing,
            loading: !(self.sessions_loaded && self.index_loaded),
            view_menu_open,
            view_options: ViewOptions {
                group_by,
                sort_by,
                show_empty: self.show_empty,
                show_hidden: self.show_hidden,
                hidden_count: self.hidden_count(),
                show_archived: self.show_archived,
                // Baaz reads no PR data yet: the row stays, disabled, "Soon"
                // (owner 2026-10-11).
                show_pr_status: false,
                pr_status_available: false,
                search_all_projects: self.layout.search_all_projects,
            },
            bylines: Bylines::All,
            account: self.sidebar_account(account_menu_open),
            browse,
        }
    }

    /// The footer and its menu: the signed-in person (or the signed-out
    /// placeholder), the tier's plan label, and — only while the menu is
    /// open — one usage card per enabled provider from `usage_rows`.
    fn sidebar_account(&self, menu_open: bool) -> Account {
        let (name, email, initial) = match &self.auth {
            Auth::SignedIn(identity) => (identity.display_name(), identity.email.clone(), identity.initial()),
            Auth::Probing => ("Signing in…".to_owned(), String::new(), "·".to_owned()),
            Auth::SignedOut => ("Not signed in".to_owned(), String::new(), "?".to_owned()),
        };
        let usage = if menu_open { self.sidebar_usage() } else { Vec::new() };
        Account {
            name: name.into(),
            email: email.into(),
            plan: muse_plan_label(self.tier.as_ref()).unwrap_or_default().into(),
            initial: initial.into(),
            tint: 0,
            menu_open,
            usage,
        }
    }

    /// The account menu's usage cards: `usage_rows` (the Providers page's
    /// rows) re-shaped as the screen's `ProviderUsage` — one meter line per
    /// window, or one unavailable line carrying the reason.
    fn sidebar_usage(&self) -> Vec<ProviderUsage> {
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let now = crate::account_usage::now_secs();
        let muse_feed = signed_in.then(|| crate::account_usage::MuseFeed { tier: self.tier.clone() });
        crate::account_usage::usage_rows(&crate::provider_status::live_statuses(), muse_feed, now)
            .into_iter()
            .enumerate()
            .map(|(index, row)| {
                let lines = match row.state {
                    UsageRowState::Windows(windows, _) => windows
                        .into_iter()
                        .map(|w| UsageLine { label: w.label, fraction: w.used_fraction, resets: w.resets_at_text })
                        .collect(),
                    UsageRowState::Unavailable(reason) => {
                        vec![UsageLine { label: reason, fraction: 0.0, resets: SharedString::default() }]
                    }
                };
                ProviderUsage {
                    name: provider_display_name(row.provider),
                    plan: row.plan.unwrap_or_default(),
                    initial: row.provider.letter().into(),
                    tint: index as u8,
                    lines,
                }
            })
            .collect()
    }

    /// Browse mode's rows: the current space's person tabs (pinned ones as
    /// tiles, the rest in Today or in your folders), every session that owns
    /// tabs as a task folder, the shown page's address and history, the
    /// spaces, and the open menu / name field.
    fn browse_model(&self, cx: &mut Context<Self>) -> BrowseModel {
        let space = self.browse_space().unwrap_or_default();
        let snaps = self.browse_person_snaps(cx);
        let folder_of: std::collections::HashMap<crate::browser::TabId, Option<String>> =
            self.browser.tabs.tabs.iter().map(|tab| (tab.id, tab.folder.clone())).collect();
        // One tab is active in Browse: the one the centre shows. Each session
        // also has an active tab of its own, and counting those lit every
        // agent tab's row and moved the selected pill onto the last of them
        // (owner screenshots 2026-10-11: the page's own row had none).
        let shown = self.browse_centre_shown();
        let tab = |id: crate::browser::TabId, title: String, initial: String, _owner_active: bool, loading: bool, favicon: Option<String>| BrowseTab {
            id: browse_tab_key(id).into(),
            title: title.into(),
            initial: initial.into(),
            active: Some(id) == shown,
            loading,
            favicon: favicon.map(Into::into),
        };
        let mut pins = Vec::new();
        let mut today = Vec::new();
        let mut in_folders: std::collections::HashMap<String, Vec<BrowseTab>> = std::collections::HashMap::new();
        for snap in snaps {
            if snap.pinned {
                pins.push(BrowsePin { id: browse_tab_key(snap.id).into(), initial: snap.initial.into(), label: snap.title.into(), favicon: snap.favicon.map(Into::into) });
                continue;
            }
            let folder = folder_of.get(&snap.id).cloned().flatten();
            let row = tab(snap.id, snap.title, snap.initial, snap.active, snap.loading, snap.favicon);
            match folder.filter(|id| self.browser.tabs.folders.iter().any(|f| &f.id == id)) {
                Some(folder) => in_folders.entry(folder).or_default().push(row),
                None => today.push(row),
            }
        }
        let folders = self
            .browser
            .tabs
            .folders_in(&space)
            .into_iter()
            .map(|folder| BrowseFolder {
                id: folder.id.clone().into(),
                name: folder.name.clone().into(),
                kind: FolderKind::Yours,
                open: folder.open,
                tabs: in_folders.remove(&folder.id).unwrap_or_default(),
            })
            .collect();
        let task_folders = self
            .browse_session_groups(cx)
            .into_iter()
            .map(|(session, label, rows)| {
                let entry = self.sessions.iter().find(|entry| entry.id == session);
                let running = entry.is_some_and(|entry| entry.running);
                let waiting = entry.is_some_and(|entry| !entry.attention.is_empty() || entry.approval_command.is_some() || entry.pending_question.is_some());
                let (state, status) = if waiting {
                    (AgentState::Waiting, "needs you")
                } else if running {
                    (AgentState::Running, "working")
                } else {
                    (AgentState::Idle, "")
                };
                BrowseFolder {
                    id: session.into(),
                    name: label.into(),
                    kind: FolderKind::Task { state, status: status.into(), needs_you: waiting },
                    open: true,
                    // A session's tabs load in its own pane; their rows keep
                    // the favicon (the groups key carries no page facts).
                    tabs: rows.into_iter().map(|row| tab(row.id, row.title, row.initial, row.active, false, row.favicon)).collect(),
                }
            })
            .collect();
        let shown = self.browse_centre_shown();
        // The live page's URL, falling back to the tab's — the same read
        // the toolbar makes, so the two never disagree.
        let live_url = self.browse_centre_state().map(|state| state.read(cx).url().to_string()).unwrap_or_default();
        let url = if live_url.is_empty() {
            shown.and_then(|id| self.browser.tabs.get(id)).map(|tab| tab.url.clone()).unwrap_or_default()
        } else {
            live_url
        };
        let address = split_address(&url);
        let history = shown.and_then(|id| self.browser.histories.get(&id));
        let spaces: Vec<BrowseSpace> = self
            .browser
            .tabs
            .spaces
            .iter()
            .map(|space| BrowseSpace { id: space.id.clone().into(), name: space.name.clone().into() })
            .collect();
        let current = self.browser.tabs.spaces.iter().position(|s| s.id == space).unwrap_or(0);
        let editing = self.browse_naming.clone().map(|target| BrowseEditing { target, draft: self.rename.read(cx).value().to_string().into() });
        // D4: the downloads button exists while downloads list; its dot
        // says one is running.
        let downloads = self.browse_downloads(cx);
        let downloads = (!downloads.is_empty())
            .then(|| downloads.iter().any(|(_, _, d)| d.state == aui_webview::DownloadStatus::Downloading));
        BrowseModel {
            host: address.host,
            path: address.path,
            can_go_back: history.is_some_and(|h| h.can_back()),
            can_go_forward: history.is_some_and(|h| h.can_forward()),
            pins,
            tabs: today,
            folders,
            task_folders,
            spaces: if spaces.is_empty() { vec![BrowseSpace { id: crate::browser::tabs::PERSONAL.into(), name: "Personal".into() }] } else { spaces },
            downloads,
            space: current,
            hovered_today_header: self.hovered_today_header,
            editing,
            menu: self.browse_menu.clone(),
        }
    }

    /// Every intent the screen reports, routed to the handler Baaz already
    /// had for it. Any intent that is not the menus' own closes an open
    /// view-options or account menu: the screen draws the menus, the host
    /// decides when they go (outside clicks and Escape are its to detect).
    pub(crate) fn sidebar_intent(&mut self, intent: SidebarIntent, window: &mut Window, cx: &mut Context<Self>) {
        let keeps_menu = matches!(
            intent,
            SidebarIntent::ViewOptions
                | SidebarIntent::ViewOption(_)
                | SidebarIntent::Account
                | SidebarIntent::AccountAction(_)
                | SidebarIntent::MenuBackdrop
                | SidebarIntent::DismissMenus
                | SidebarIntent::Hover(..)
                | SidebarIntent::HoverGroup(_)
                | SidebarIntent::HoverProjectsHeader(_)
                | SidebarIntent::Browse(BrowseIntent::HoverTodayHeader(_))
        );
        if !keeps_menu {
            let open = self.overlays.read(cx).is_open(MenuKind::ViewOptions)
                || self.overlays.read(cx).is_open(MenuKind::Account);
            if open {
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
            }
        }
        match intent {
            SidebarIntent::Search => self.open_search(window, cx),
            SidebarIntent::NewSession => self.new_session(window, cx),
            SidebarIntent::Nav(id) => match id.as_ref() {
                NAV_PROJECTS => self.open_projects(false, window, cx),
                NAV_SKILLS => self.open_skills(cx),
                _ => {
                    self.overlays.update(cx, |overlays, _| {
                        overlays.toast("Automations", "Automations are not wired up yet.");
                    });
                }
            },
            SidebarIntent::Select(id) => {
                // A sidebar click never arms the reveal: the clicked row is
                // under the cursor (owner round 6). It dismisses the hover
                // card: the click is the row's, not the card's.
                self.close_row_detail();
                self.forced_detail = None;
                self.resume_quiet(id.to_string(), window, cx);
            }
            SidebarIntent::Toggle(id) => self.toggle_group(id.to_string(), cx),
            SidebarIntent::TogglePinned => self.pinned_open = !self.pinned_open,
            SidebarIntent::ViewOptions => {
                if !self.just_dismissed(MenuKind::ViewOptions) {
                    self.open_menu(MenuKind::ViewOptions, cx);
                }
            }
            SidebarIntent::ViewOption(option) => self.apply_view_option(option, cx),
            SidebarIntent::MenuBackdrop => {}
            SidebarIntent::DismissMenus => {
                // Remember what the press closed: when the press was on the
                // menu's own trigger, the click that follows must not reopen it.
                let closed = self.overlays.read(cx).menu.as_ref().map(|menu| menu.kind);
                self.menu_dismissed = closed.map(|kind| (kind, std::time::Instant::now()));
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
            }
            SidebarIntent::AddProject => self.open_projects(false, window, cx),
            // A group row's actions: `+` starts a session in that project
            // (and makes it current), `…` opens its project menu. On "Other
            // workspaces" `+` has nowhere to start, so it offers adoption.
            SidebarIntent::GroupAction(id, GroupAction::New) => {
                if id.as_ref() == crate::sidebar::OTHER_GROUP {
                    self.open_projects(false, window, cx);
                } else {
                    self.new_session_in(Some(id.to_string()), window, cx);
                }
            }
            SidebarIntent::GroupAction(id, GroupAction::Menu) => {
                // The screen reports no bounds for the head's `…`; the
                // pointer stands on it at the click, so the menu seats
                // there (a keyboard open seats wherever the pointer is).
                let at = window.mouse_position();
                self.group_menu_bounds.insert(id.to_string(), Bounds::new(at, gpui::size(px(1.0), px(1.0))));
                if id.as_ref() == crate::sidebar::OTHER_GROUP {
                    self.open_project_menu(None, false, cx);
                } else {
                    self.open_project_menu(Some(id.to_string()), false, cx);
                }
            }
            SidebarIntent::GroupAction(id, GroupAction::ToggleMore) | SidebarIntent::ToggleMore(id) => {
                self.toggle_expanded(id.to_string(), cx);
            }
            SidebarIntent::RowAction(id, action) => {
                self.close_row_detail();
                self.forced_detail = None;
                match action {
                    RowAction::Rename => self.start_rename(id.to_string(), window, cx),
                    RowAction::Pin => self.toggle_pin(id.to_string(), cx),
                    // One archive affordance: a listed session asks first,
                    // an archived one goes straight back.
                    RowAction::Archive => {
                        let archived =
                            self.sessions.iter().find(|e| e.id == id.as_ref()).is_some_and(|e| e.archived);
                        if archived {
                            self.unarchive_session(id.to_string(), cx);
                        } else {
                            self.open_archive_dialog(id.to_string(), cx);
                        }
                    }
                    _ => {}
                }
            }
            SidebarIntent::Rename(id, text) => {
                self.rename_session(id.to_string(), Some(text.to_string()), cx);
                self.focus_composer = true;
            }
            SidebarIntent::RenameCancel => {
                self.renaming = None;
                self.focus_composer = true;
            }
            SidebarIntent::Account => {
                if !self.just_dismissed(MenuKind::Account) {
                    self.open_menu(MenuKind::Account, cx);
                }
            }
            SidebarIntent::AccountAction(action) => {
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
                match action {
                    AccountAction::Settings => self.open_settings(0, cx),
                    AccountAction::Providers => self.open_providers(cx),
                    AccountAction::SignOut if matches!(self.auth, Auth::SignedIn(_)) => self.logout(cx),
                    // The login screen is never the app's first screen, but
                    // it stays reachable here.
                    AccountAction::SignOut => {
                        self.login.reset_to_choose();
                        self.muse_sheet = true;
                    }
                }
            }
            SidebarIntent::Hover(id, bounds) => {
                let id = id.map(|id| id.to_string());
                if self.sidebar_hovered != id {
                    // The hover card follows session rows only: leaving one
                    // disarms it, entering one arms the delay with the row's
                    // bounds (the card seats beside the pane at its top).
                    if let Some(previous) = self.sidebar_hovered.take() {
                        self.note_row_hover(previous, false, None, cx);
                    }
                    if let Some(next) = id.clone().filter(|id| self.sessions.iter().any(|entry| &entry.id == id)) {
                        self.note_row_hover(next, true, bounds, cx);
                    }
                    self.sidebar_hovered = id;
                    self.sidebar_pane.update(cx, |_, cx| cx.notify());
                }
                return;
            }
            SidebarIntent::HoverGroup(id) => {
                let id = id.map(|id| id.to_string());
                if self.hovered_group != id {
                    self.hovered_group = id;
                    self.sidebar_pane.update(cx, |_, cx| cx.notify());
                }
                return;
            }
            SidebarIntent::HoverProjectsHeader(on) => {
                if self.hovered_projects_header != on {
                    self.hovered_projects_header = on;
                    self.sidebar_pane.update(cx, |_, cx| cx.notify());
                }
                return;
            }
            SidebarIntent::Browse(intent) => self.browse_intent(intent, window, cx),
        }
        cx.notify();
    }

    /// Whether `kind` was dismissed by the press of the click now arriving:
    /// a trigger's own click after an outside-press close toggles it shut,
    /// never back open.
    fn just_dismissed(&mut self, kind: MenuKind) -> bool {
        match self.menu_dismissed.take() {
            Some((closed, at)) => closed == kind && at.elapsed() < std::time::Duration::from_millis(400),
            None => false,
        }
    }

    /// One row of the view-options menu (or its Group by / Sort by
    /// submenus). Toggles keep the menu open, so the check is seen to
    /// change; a submenu choice and Clear empty close it.
    fn apply_view_option(&mut self, option: ViewOption, cx: &mut Context<Self>) {
        match option {
            ViewOption::GroupBy(key) => {
                self.set_group_by(
                    match key {
                        GroupKey::Project => crate::layout::GroupBy::Project,
                        GroupKey::Date => crate::layout::GroupBy::Date,
                    },
                    cx,
                );
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
            }
            ViewOption::SortBy(key) => {
                self.set_sort_by(
                    match key {
                        SortKey::Recent => crate::layout::SortBy::Recent,
                        SortKey::Created => crate::layout::SortBy::Created,
                        SortKey::Name => crate::layout::SortBy::Name,
                    },
                    cx,
                );
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
            }
            ViewOption::ToggleEmpty => {
                self.show_empty = !self.show_empty;
                self.invalidate_list();
            }
            ViewOption::ToggleHidden => {
                self.show_hidden = !self.show_hidden;
                self.invalidate_list();
            }
            ViewOption::ToggleArchived => {
                self.show_archived = !self.show_archived;
                self.invalidate_list();
            }
            // Disabled while Baaz reads no PR data: never reported.
            ViewOption::TogglePrStatus => {}
            ViewOption::ToggleSearchAll => self.toggle_search_scope(cx),
            ViewOption::ClearEmpty => {
                self.overlays.update(cx, |overlays, _| overlays.menu = None);
                self.clear_empty(cx);
            }
        }
    }

    /// Browse mode's intents, routed to the browser's own handlers.
    fn browse_intent(&mut self, intent: BrowseIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            BrowseIntent::Address => {
                let url = self.browse_centre_state().map(|state| state.read(cx).url().to_string()).unwrap_or_default();
                self.focus_browse_address_with(&url, window, cx);
            }
            BrowseIntent::SiteInfo => {
                self.site_info_open = self.browse_centre_state().is_some();
                cx.notify();
            }
            BrowseIntent::Back => self.browse_go_back(window, cx),
            BrowseIntent::Forward => self.browse_go_forward(window, cx),
            BrowseIntent::Reload => {
                if let Some(id) = self.browse_centre_shown() {
                    self.reload_browse_tab(id, window, cx);
                }
            }
            BrowseIntent::NewTab => self.new_browse_sidebar_tab(window, cx),
            BrowseIntent::PageMenu(anchor) => self.toggle_browse_page_menu(anchor, window, cx),
            BrowseIntent::Downloads(anchor) => self.toggle_browse_downloads(anchor, cx),
            BrowseIntent::Page(action) => self.browse_page_action(action, window, cx),
            BrowseIntent::OpenPin(id) | BrowseIntent::SelectTab(id) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_sidebar_tab(id, false, window, cx);
                }
            }
            BrowseIntent::CloseTab(id) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_sidebar_tab(id, true, window, cx);
                }
            }
            BrowseIntent::ToggleFolder(id) => {
                if self.browser.tabs.toggle_folder(&id) {
                    self.schedule_tabs_write(cx);
                }
            }
            BrowseIntent::OpenTaskChat(session) => self.browse_task_folder_action(&session, TaskFolderAction::OpenChat, window, cx),
            BrowseIntent::HoverTodayHeader(on) => {
                if self.hovered_today_header != on {
                    self.hovered_today_header = on;
                    self.sidebar_pane.update(cx, |_, cx| cx.notify());
                }
            }
            BrowseIntent::Ask => self.browse_ask_page(window, cx),
            BrowseIntent::SwitchSpace(index) => self.browse_switch_space(index, window, cx),
            BrowseIntent::NewSpace => self.start_browse_naming(BrowseNaming::Space(None), "", window, cx),
            BrowseIntent::NewFolder => self.browse_new_folder(None, window, cx),
            BrowseIntent::CloseAll => self.browse_close_all(window, cx),
            BrowseIntent::OpenMenu(menu) => self.open_browse_menu(menu, cx),
            BrowseIntent::CloseMenu => self.close_browse_menu(cx),
            BrowseIntent::Tab(id, action) => {
                if let Some(id) = browse_tab_id(&id) {
                    self.browse_tab_action(id, action, window, cx);
                }
            }
            BrowseIntent::Folder(id, action) => self.browse_folder_action(&id, action, window, cx),
            BrowseIntent::TaskFolder(session, action) => self.browse_task_folder_action(&session, action, window, cx),
            BrowseIntent::Space(id, action) => self.browse_space_action(&id, action, window, cx),
            BrowseIntent::Name(target, text) => self.commit_browse_name(target, &text, cx),
            BrowseIntent::NameCancel => self.cancel_browse_name(cx),
        }
    }
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
            sort_by: self.layout.sort_by,
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
        // The splice below tracks the top item by index, so rows inserted
        // above carry a steered or parked top away from its row; repaired
        // after the sync, once the new rows are in.
        let top_before = self.sidebar_list.logical_scroll_top();
        // `render_sidebar` bends the count to the screen's skeleton rows
        // while loading; put the state back on the previous flattened rows
        // so the splice below diffs against what the list really holds.
        if self.sidebar_list.item_count() != self.prev_sidebar_rows.len() {
            self.sidebar_list.reset(self.prev_sidebar_rows.len());
        }
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
        self.repair_sidebar_scroll(top_before, rows, grouping);
    }

    /// Re-anchor the scroll position after [`Self::sync_sidebar_list`]
    /// spliced: gpui's splice keeps the top *item by index*, so rows the
    /// staged boot loads insert above carry a steered or parked top away
    /// from where it belongs. While the user has not scrolled, an armed
    /// reveal re-steers to its row by id (a reveal for an id with no row
    /// waits without moving anything), and a top-parked list with nothing
    /// to steer to stays parked at row 0. A user-scrolled, gesture-driven
    /// or resizing list is never touched — same gate as the frame renderer.
    fn repair_sidebar_scroll(&mut self, before: ListOffset, rows: &[SidebarRow], grouping: &Grouping) {
        if self.sidebar_user_scrolled || self.sidebar_gesture_active() || self.resize.active {
            return;
        }
        if let Some(reveal_id) = self.reveal.clone() {
            let wanted = SharedString::from(reveal_id.as_str());
            let target = row_index_for_session(rows, grouping, &wanted)
                .or_else(|| self.reveal_head_row(rows, grouping, &reveal_id));
            if let Some(ix) = target {
                ensure_row_visible(&self.sidebar_list, ix);
                return;
            }
            // No row (yet): the wait itself must not move the list, nor let
            // the splice carry a parked top away under it.
        }
        if before.item_ix == 0 && before.offset_in_item == px(0.0) {
            let after = self.sidebar_list.logical_scroll_top();
            if after.item_ix != 0 || after.offset_in_item != px(0.0) {
                ensure_row_visible(&self.sidebar_list, 0);
            }
        }
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
    /// head for a closed group (never auto-expanded, like before) or for a
    /// rowless session whose project resolves past the rows (a draft before
    /// its first send reveals its project head), else wait for an id with
    /// no row yet (a fork before `load_sessions`'s reply lands) up to
    /// [`REVEAL_UNKNOWN_FRAMES`] attempts, else clear a flag with nowhere
    /// to go (a *known* session that still has no landing row gives up
    /// on the first miss, same as before — only an unrecognised id gets
    /// the grace period; the rowless *active* session never gives up while
    /// it stays open). A sidebar
    /// click never sets the flag; wheel and resize disarm it. Called on
    /// selection change and the frames after, until the row reports visible
    /// once the rows have settled (`sessions_loaded`): before the first
    /// layout the viewport is unknown and the steer is a no-op, and a fully
    /// visible row moves nothing.
    fn reveal_sidebar_row(
        &mut self,
        rows: &[SidebarRow],
        grouping: &Grouping,
        reveal_id: &str,
        cx: &mut Context<Self>,
    ) {
        let wanted = SharedString::from(reveal_id);
        // A pinned session sits in the Pinned section above the list, always
        // in view: its reveal is done, and its project head is never the
        // target (it used to land there, the row being lifted out).
        if self.sidebar_pinned(cx).iter().any(|row| row.id == wanted) {
            self.reveal = None;
            self.reveal_unknown = None;
            return;
        }
        let target = row_index_for_session(rows, grouping, &wanted)
            .or_else(|| self.reveal_head_row(rows, grouping, reveal_id));
        let Some(ix) = target else {
            // The rowless active session keeps its arm: a brand-new session
            // has no row until its first send, and the reveal falls back to
            // its project head only once the head resolves — disarming here
            // would strand the scroll-follow before the row is born. Any
            // other miss disarms or streaks as before; a later switch, a
            // quiet click or a user scroll still clears the arm.
            let active_id = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
            let known = self.sessions.iter().any(|e| e.id == reveal_id);
            if rowless_reveal_stays_armed(active_id.as_deref(), reveal_id, known) {
                self.reveal_unknown = None;
                return;
            }
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
            // The row is on screen — but while the first `session/list` is
            // still landing, later stages reindex around it and the splice
            // carries the steered top away (see `repair_sidebar_scroll`,
            // which tracks the row by id meanwhile): stay armed until the
            // rows settle. No poke meanwhile — nothing is outstanding.
            if self.sessions_loaded {
                self.reveal = None;
            }
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
        reveal_head_row(
            &self.sessions,
            &self.overrides,
            &self.provider_sessions,
            self.current_project.as_deref(),
            rows,
            grouping,
            reveal_id,
        )
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
                    .border_color(if focused { p.accent_ring } else { p.line })
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
const NAV_PROJECTS: &str = "projects";
const NAV_SKILLS: &str = "skills";
const NAV_AUTOMATIONS: &str = "automations";

/// A Browse tab's id as the screen's stable row key, and back.
fn browse_tab_key(id: crate::browser::TabId) -> String {
    format!("tab-{}", id.0)
}

fn browse_tab_id(key: &str) -> Option<crate::browser::TabId> {
    key.strip_prefix("tab-")?.parse().ok().map(crate::browser::TabId)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;
    use gpui_kit::base::v_flex;
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
                    provider: None,
                    named: false,
                    needs_title: false,
                    side_marker: false,
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
                    terminals_running: 0,
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
            provider: None,
            named: false,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
            local: false,
            provisional: false,
            workspace: None,
            project: Some(project.to_owned()),
            project_name: None,
            attention: Vec::new(),
            approval_command: None,
            pending_question: None,
            turn_started: None,
            last_error: None,
            branch: None,
            terminals_running: 0,
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
    /// project head — filed by override first, then by provider record,
    /// then by the current project — and gives up past all three.
    #[test]
    fn new_session_scrolls_to_its_project_head() {
        let (ids, projects) = scroll_projects("new");
        let entries = vec![scroll_entry("s-row", &ids[0])];
        let (grouping, rows) = scroll_rows(&entries, &projects);
        let records = crate::provider_sessions::ProviderSessionStore::new();
        // Filed by override: a draft's project before its first send.
        let mut overrides = crate::sessions::Overrides::new();
        overrides.insert(
            "s-new".to_owned(),
            crate::sessions::SessionMeta { project: Some(ids[1].clone()), ..Default::default() },
        );
        assert_eq!(
            reveal_head_row(&entries, &overrides, &records, None, &rows, &grouping, "s-new"),
            Some(project_head_ix(&grouping, &rows, &ids[1])),
            "the override files the rowless session"
        );
        // Filed by provider record.
        let overrides = crate::sessions::Overrides::new();
        let mut records = crate::provider_sessions::ProviderSessionStore::new();
        records.insert(
            "s-lane".to_owned(),
            crate::provider_sessions::ProviderSessionRecord {
                provider: "codex".to_owned(),
                session_id: "s-lane".to_owned(),
                workspace: None,
                project: Some(ids[1].clone()),
                created_ms: 0,
                updated_ms: 0,
                turns: 0,
                title: None,
                first_prompt: None,
                handoff_to: None,
                handoff_from: None,
                handoff_from_provider: None,
                handoff_title: None,
                display_texts: HashMap::new(),
            },
        );
        assert_eq!(
            reveal_head_row(&entries, &overrides, &records, None, &rows, &grouping, "s-lane"),
            Some(project_head_ix(&grouping, &rows, &ids[1])),
            "the provider record files the rowless session"
        );
        // Filed by the current project: a new session starts in it.
        assert_eq!(
            reveal_head_row(&entries, &overrides, &records, Some(ids[0].as_str()), &rows, &grouping, "s-unknown"),
            Some(project_head_ix(&grouping, &rows, &ids[0])),
            "the current project files an otherwise unknown session"
        );
        // Past all three: nowhere to go.
        assert_eq!(reveal_head_row(&entries, &overrides, &records, None, &rows, &grouping, "s-unknown"), None);
    }

    /// An open view on `session_id`, the way a draft opens before its
    /// first send: no child, no wire, offline throughout.
    fn active_view(vc: &mut gpui::VisualTestContext, workspace: &str, session_id: &str) -> Entity<crate::session::SessionView> {
        let workspace = workspace.to_owned();
        let session_id = session_id.to_owned();
        vc.update(|window, cx| {
            cx.new(|cx| {
                crate::session::SessionView::new(
                    session_id,
                    None,
                    crate::session::SessionHost {
                        provider_id: "muse".to_owned(),
                        workspace,
                        overlays: cx.new(|_| crate::overlays::Overlays::default()),
                        capture: crate::shot::CaptureToken::default(),
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
            let h = baaz.read(cx);
            let selected = SidebarKey::current(h, cx).selected;
            let grouping = h.sidebar_grouping(cx);
            let crate::sidebar::Grouping::Project(groups) = &*grouping else {
                panic!("two adoptions group by project");
            };
            let current = groups.iter().find(|g| g.current).map(|g| g.id.to_string());
            (selected, current)
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
            baaz.update(cx, |h, _| {
                (0..2)
                    .map(|i| {
                        let root = dir.join(format!("proj-{i}"));
                        std::fs::create_dir_all(&root).expect("temp root");
                        h.projects.add(&root).id.clone()
                    })
                    .collect()
            })
        });
        // One turned row in the first project, open in the window; the
        // list has landed.
        let workspace = dir.to_string_lossy().into_owned();
        let open = active_view(vc, &workspace, "s-row");
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.sessions.push(scroll_entry("s-row", &ids[0]));
                h.sessions_loaded = true;
                h.index_loaded = true;
                h.active = Some(open.clone());
                h.invalidate_list();
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
                h.invalidate_list();
            })
        });
        let (selected, current) = selected_and_current(&baaz, vc);
        assert_eq!(selected.as_deref(), Some("s-new"), "a new session still names its selection");
        assert_eq!(current.as_deref(), Some(ids[1].as_str()), "its project group is current");
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H2: the cached pane re-arms on a mode switch, so the switch
    /// row at the top of the column never goes stale.
    #[gpui::test]
    fn sidebar_key_changes_with_mode(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("mode-key");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        let sessions_key = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert_eq!(sessions_key.mode, crate::layout::Mode::Sessions);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| h.mode = crate::layout::Mode::Browse);
        });
        let browse_key = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(sessions_key != browse_key, "the mode switch re-arms the cached pane");
        assert_eq!(browse_key.mode, crate::layout::Mode::Browse);
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H3: the cached pane re-arms when the Browse sidebar's inputs
    /// change — a select, a pin, a close, an expand and the open tab
    /// menu all move the key.
    #[gpui::test]
    fn sidebar_key_changes_with_browse_tabs(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-key");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                // The Browse fingerprints key only in Browse mode (a
                // Sessions frame never walks the tab model), so the test
                // enters Browse the way the switch does.
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                h.browser.tabs.open(
                    space.clone(),
                    crate::browser::Owner::Person,
                    "https://a.example/",
                    None,
                );
                h.browser.tabs.open(space, crate::browser::Owner::Person, "https://b.example/", None);
            })
        });
        let opened = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        // A select moves the key.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                let space = h.browse_space();
                let id = h.browser.tabs.person_tabs(space.as_deref())[0].id;
                h.browser.tabs.activate(id);
            })
        });
        let selected = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(opened != selected, "a tab select re-arms the cached pane");
        // A pin moves the key (the pinned tile appears).
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                let space = h.browse_space();
                let id = h.browser.tabs.person_tabs(space.as_deref())[0].id;
                h.browser.tabs.set_pinned(id, true);
            })
        });
        let pinned = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(selected != pinned, "a pin re-arms the cached pane");
        // A close moves the key.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                let id = h.browser.tabs.tabs[0].id;
                h.browser.tabs.close(id);
            })
        });
        let closed = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(pinned != closed, "a close re-arms the cached pane");
        // A From-sessions expand moves the key.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.browse_from_expanded.insert("s-1".to_owned());
            })
        });
        let expanded = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(closed != expanded, "an expand re-arms the cached pane");
        // The open tab menu is keyed too.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.tab_menu = h.browser.tabs.tabs.first().map(|tab| tab.id);
            })
        });
        let menu = vc.update(|_, cx| SidebarKey::current(baaz.read(cx), cx));
        assert!(expanded != menu, "the tab menu re-arms the cached pane");
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
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                h.browser.tabs.open(
                    space.clone(),
                    crate::browser::Owner::Person,
                    "https://a.example/",
                    None,
                );
                h.browser.tabs.open(space, crate::browser::Owner::Person, "https://b.example/", None);
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
            let tabs = h.browser.tabs.person_tabs(h.browse_space().as_deref());
            assert_eq!(tabs.len(), 1, "⌘W in Browse closes a tab, not the window");
        });
        // Sessions: the same handler must not touch the tabs — the belt
        // holds, and ⌘W keeps closing the window there.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Sessions;
            })
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.close_browse_shortcut(window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let tabs = h.browser.tabs.person_tabs(h.browse_space().as_deref());
            assert_eq!(tabs.len(), 1, "⌘W in Sessions closes no tab");
        });
        b3c_teardown(&dir, guard, old);
    }

    /// B2a-H3b item 6: while a From-sessions tab shows, ⌘W stops showing
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
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                let mine = h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None);
                let agent = h.browser.tabs.open(space, crate::browser::Owner::Session("s-1".to_owned()), "https://b.example/", None);
                h.browser.tabs.activate(agent);
                h.browser.tabs.activate(mine);
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
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                let blank = h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, crate::browser::tabs::BLANK, None);
                h.browser.tabs.activate(blank);
                assert!(h.browse_centre_tab_blank(), "a fresh tab is blank");
                let going = h.browser.tabs.open(space, crate::browser::Owner::Person, "https://news.example/", Some(blank));
                h.browser.tabs.activate(going);
                assert!(!h.browse_centre_tab_blank(), "a tab opened on a URL is not blank");
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
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                let first = h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://a.example/", None);
                h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://b.example/", None);
                let folded = h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://c.example/", None);
                let folder = h.browser.tabs.add_folder(space.as_deref().unwrap_or_default(), "Reading");
                h.browser.tabs.set_folder(folded, Some(&folder));
                h.browser.tabs.activate(first);
                (first, folded)
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.new_browse_sidebar_tab(window, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let ids: Vec<_> = h.browser.tabs.person_tabs(h.browse_space().as_deref()).iter().map(|tab| tab.id).collect();
            let at = ids.iter().position(|id| *id == first).expect("first");
            let new = ids[at + 1];
            assert!(h.browser.tabs.get(new).is_some_and(|tab| tab.url == crate::browser::tabs::BLANK && tab.folder.is_none()), "a Today tab's new tab sits right after it, in Today");
        });
        vc.update(|_, cx| baaz.update(cx, |h, _| h.browser.tabs.activate(folded)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.new_browse_sidebar_tab(window, cx)));
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            let ids: Vec<_> = h.browser.tabs.person_tabs(h.browse_space().as_deref()).iter().map(|tab| tab.id).collect();
            let at = ids.iter().position(|id| *id == folded).expect("folded");
            let new = h.browser.tabs.get(ids[at + 1]).expect("a tab after the folded one");
            assert_eq!(new.url, crate::browser::tabs::BLANK);
            assert_eq!(new.folder, h.browser.tabs.get(folded).and_then(|tab| tab.folder.clone()), "a folder tab's new tab joins its folder");
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
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                let space = h.browse_space();
                h.browser.tabs.open(
                    space.clone(),
                    crate::browser::Owner::Person,
                    "https://a.example/",
                    None,
                );
                h.browser.tabs.open(space.clone(), crate::browser::Owner::Person, "https://b.example/", None);
                let shown = h.browser.tabs.open(
                    space,
                    crate::browser::Owner::Session("s-1".to_owned()),
                    "https://c.example/",
                    None,
                );
                h.browser.tabs.activate(shown);
                h.browse_session_tab = Some(shown);
            })
        });
        let before = vc.update(|_, cx| baaz.read(cx).browser.tabs.generation);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.move_browse_shortcut(1, window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.browse_session_tab.is_some(), "the session show survives the move");
            assert_eq!(
                h.browser.tabs.generation, before,
                "the move touches no person tab while a session tab shows"
            );
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.close_browse_shortcut(window, cx));
        });
        vc.update(|_, cx| {
            let h = baaz.read(cx);
            assert!(h.browse_session_tab.is_none(), "⌘W stops showing the session tab");
            let tabs = h.browser.tabs.person_tabs(h.browse_space().as_deref());
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
    /// driving `sync_sidebar_list` the way the staged boot loads do. The
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
            terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
        };
        (dir, guard, old, args)
    }

    /// B3c: a throwaway signed-in window on the hermetic state above.
    fn b3c_window(vc: &mut gpui::VisualTestContext, args: crate::Args) -> Entity<Harness> {
        use muse_client::schema::AccountStateKind;
        vc.update(|window, cx| {
            let baaz =
                cx.new(|cx| Harness::new(args, crate::shot::CaptureToken::default(), window, cx));
            baaz.update(cx, |h, _| {
                h.auth = crate::login::Auth::SignedIn(crate::auth::Identity {
                    lane: AccountStateKind::AccountLogin,
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
        // First batch: one date bucket, nothing pinned.
        let g1 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![b3c_summary("u-1", false), b3c_summary("u-2", false), b3c_summary("u-3", false)],
        )]);
        let rows1 = flatten_sidebar(&g1, false);
        assert_eq!(rows1.len(), 4, "header plus three sessions");
        let g1 = Rc::new(g1);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                assert!(h.reveal.is_none(), "no session is active, so nothing arms a reveal");
                h.sync_sidebar_list(&rows1, &g1);
                assert_eq!(h.sidebar_list.item_count(), rows1.len());
                // Park the list the way a painted top leaves it: the splice
                // below only carries a materialised scroll top.
                h.sidebar_list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                assert_eq!(h.sidebar_list_top().item_ix, 0, "the first paint parks at the top");
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
            baaz.update(cx, |h, _| {
                h.sync_sidebar_list(&rows2, &g2);
                assert_eq!(h.sidebar_list.item_count(), rows2.len());
                assert_eq!(
                    h.sidebar_list_top().item_ix,
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
        let g1 = Grouping::Date(vec![DateGroup::new(
            "Older",
            vec![b3c_summary("u-1", false), b3c_summary("u-2", false), b3c_summary("u-3", false)],
        )]);
        let rows1 = flatten_sidebar(&g1, false);
        let g1 = Rc::new(g1);
        // Boot order: the activation arms the reveal before any rows land.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.reveal = Some("s-draft".to_owned());
                h.reveal_unknown = None;
                h.sidebar_user_scrolled = false;
                h.sync_sidebar_list(&rows1, &g1);
                // Park the list the way a painted top leaves it (a parked
                // scroll, not a reveal steer — the draft has no row).
                h.sidebar_list.scroll_to(gpui::ListOffset { item_ix: 0, offset_in_item: px(0.0) });
                assert_eq!(h.sidebar_list_top().item_ix, 0, "the first paint parks at the top");
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
            baaz.update(cx, |h, cx| {
                h.sync_sidebar_list(&rows2, &g2);
                // The wait itself never steers, even called every frame: the
                // draft has no row anywhere yet.
                h.reveal_sidebar_row(&rows2, &g2, "s-draft", cx);
                assert_eq!(
                    h.sidebar_list_top().item_ix,
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
            baaz.update(cx, |h, _| {
                assert!(!h.sessions_loaded, "the wire list has not landed yet");
                h.sync_sidebar_list(&rows1, &g1);
                assert_eq!(h.sidebar_list.item_count(), rows1.len());
            })
        });
        let state = vc.update(|_, cx| baaz.read(cx).sidebar_list.clone());
        draw_b3c_list(vc, &g1, &state);
        // Boot order: the activation arms the reveal while staging.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.reveal = Some("s-2".to_owned());
                h.reveal_unknown = None;
                h.sidebar_user_scrolled = false;
                h.reveal_sidebar_row(&rows1, &g1, "s-2", cx);
            })
        });
        draw_b3c_list(vc, &g1, &state);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.reveal_sidebar_row(&rows1, &g1, "s-2", cx);
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
            baaz.update(cx, |h, _| {
                h.sync_sidebar_list(&rows2, &g2);
                assert_eq!(h.sidebar_list.item_count(), rows2.len());
            })
        });
        // Frame pump, the way the renderer drives the armed reveal: each
        // frame steers by id again (measuring further each time) until the
        // row reports visible. A consumed reveal steers no more frames.
        let target = row_index_for_session(&rows2, &g2, &"s-2".into()).expect("the boot row has an index");
        for _ in 0..30 {
            vc.update(|_, cx| {
                baaz.update(cx, |h, cx| {
                    if let Some(id) = h.reveal.clone() {
                        h.reveal_sidebar_row(&rows2, &g2, &id, cx);
                    }
                })
            });
            draw_b3c_list(vc, &g2, &state);
            let settled = vc.update(|_, cx| {
                baaz.read(cx).sidebar_list.item_is_above_viewport(target) == Some(false)
                    && baaz.read(cx).sidebar_list.item_is_below_viewport(target) == Some(false)
            });
            if settled {
                break;
            }
        }
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                assert_eq!(
                    h.reveal.as_deref(),
                    Some("s-2"),
                    "the staged reveal stays armed until the rows settle"
                );
                assert_eq!(
                    h.sidebar_list.item_is_above_viewport(target),
                    Some(false),
                    "the active row is not above the viewport after the loads settle"
                );
                assert_eq!(
                    h.sidebar_list.item_is_below_viewport(target),
                    Some(false),
                    "the active row is visible after the loads settle"
                );
                assert!(
                    h.sidebar_list_top().item_ix > 0,
                    "the list steered to the active row, not row 0"
                );
            })
        });
        // The wire list lands: rows settle, the same reveal completes.
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.sessions_loaded = true;
                h.reveal_sidebar_row(&rows2, &g2, "s-2", cx);
                assert_eq!(h.reveal, None, "the settled reveal completes once its row is visible");
            })
        });
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
                provider: None,
                named: false,
                needs_title: false,
                side_marker: false,
                title_pending: false,
                last_ask: None,
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
                terminals_running: 0,
            }
        }
        // The open session, with no turns yet: the muse 1.3.0 shape, where
        // `session/list` already lists the draft as a zero-turn wire row.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.sessions.push(empty_entry("s-fresh"));
                // A new session is a draft until a turn starts: what keeps
                // the open session's own exemption off it.
                h.drafts.insert("p-fresh".into(), "s-fresh".into());
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::shot::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("s-fresh".into(), None, host, window, cx));
                h.active = Some(view);
                h.invalidate_list();
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
            baaz.update(cx, |h, _cx| {
                let row = h.sessions.iter_mut().find(|entry| entry.id == "s-fresh").expect("fresh row");
                assert!(
                    crate::sidebar::first_send_update(row, Some("Fix the header"), chrono::Local::now(), false),
                    "the first send touches a zero-turn row"
                );
                // The turn started: the session is no draft any more.
                h.drafts.retain(|_, id| id != "s-fresh");
                h.invalidate_list();
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
                h.sessions.push(empty_entry("s-root"));
                h.unsent.insert("s-root".into());
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::shot::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("s-root".into(), None, host, window, cx));
                h.active = Some(view);
                h.invalidate_list();
            });
        });
        let visible = vc.update(|_, cx| {
            baaz.read(cx).visible_sessions(cx).iter().map(|entry| entry.id.clone()).collect::<Vec<_>>()
        });
        assert!(!visible.contains(&"s-root".to_owned()), "an unsent root session has no row, got {visible:?}");
        vc.update(|_, cx| {
            baaz.update(cx, |h, _cx| {
                let row = h.sessions.iter_mut().find(|entry| entry.id == "s-root").expect("root row");
                crate::sidebar::first_send_update(row, Some("Map the folder"), chrono::Local::now(), false);
                h.unsent.remove("s-root");
                h.invalidate_list();
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
                h.sessions.push(empty_entry("h-src"));
                h.sessions.push(empty_entry("h-dst"));
                h.set_override("h-src", |meta| meta.handoff_to = Some("h-dst".into()), cx);
                let host = crate::session::SessionHost {
                    provider_id: "echo".into(),
                    workspace: workspace.clone(),
                    overlays: h.overlays.clone(),
                    capture: crate::shot::CaptureToken::default(),
                    terminal_host: None,
                };
                let view =
                    cx.new(|cx| crate::session::SessionView::new("h-dst".into(), None, host, window, cx));
                h.active = Some(view);
                h.invalidate_list();
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
            baaz.update(cx, |h, _| {
                // The open session: zero turns, not running, unnamed, no
                // draft — the turn started and never settled.
                let mut live = scroll_entry("s-live", "p");
                live.turns = 0;
                live.project = None;
                h.sessions.push(live);
                // Another session with a recorded ask and zero turns.
                let mut asked = scroll_entry("s-asked", "p");
                asked.turns = 0;
                asked.project = None;
                asked.last_ask = Some("Explain the schema".into());
                h.sessions.push(asked);
                // A provider session admitted once: its record holds the
                // first prompt, nothing else.
                crate::provider_sessions::upsert_open(
                    &mut h.provider_sessions,
                    "claude-code",
                    "s-admitted",
                    None,
                    None,
                    None,
                );
                crate::provider_sessions::note_first_prompt(&mut h.provider_sessions, "s-admitted", "Draw the db");
                h.merge_provider_rows();
                // And a true noise row, which still goes.
                let mut noise = scroll_entry("s-noise", "p");
                noise.turns = 0;
                noise.project = None;
                h.sessions.push(noise);
                h.sessions_loaded = true;
                h.index_loaded = true;
                h.active = Some(open.clone());
                h.invalidate_list();
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
            provider: None,
            named: true,
            needs_title: false,
            side_marker: false,
            title_pending: false,
            last_ask: None,
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
            terminals_running: 0,
        }
    }

    /// Fill the harness with `n` date-grouped sessions (the last pinned) and
    /// mark the lists loaded.
    fn load_sessions(h: &mut Harness, n: usize) {
        h.layout.group_by = Some(crate::layout::GroupBy::Date);
        h.sessions = (0..n).map(|i| list_entry(&format!("s{i}"), i == n - 1)).collect();
        h.sessions_loaded = true;
        h.index_loaded = true;
        h.invalidate_list();
    }

    fn render_once(vc: &mut gpui::VisualTestContext, baaz: &Entity<Harness>) {
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let _ = h.render_sidebar(window, cx);
            })
        });
    }

    #[gpui::test]
    fn browse_mode_render_leaves_the_session_list_untouched(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("browse-keeps-list");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| baaz.update(cx, |h, _| load_sessions(h, 40)));
        render_once(vc, &baaz);
        let (count, top) = vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.sidebar_list.scroll_to(gpui::ListOffset { item_ix: 9, offset_in_item: px(0.0) });
                (h.sidebar_list.item_count(), h.sidebar_list.logical_scroll_top())
            })
        });
        assert!(count > 20, "the sessions area has rows: {count}");
        assert_eq!(top.item_ix, 9);
        vc.update(|_, cx| baaz.update(cx, |h, _| h.mode = crate::layout::Mode::Browse));
        render_once(vc, &baaz);
        let (browse_count, browse_top) =
            vc.update(|_, cx| baaz.update(cx, |h, _| (h.sidebar_list.item_count(), h.sidebar_list.logical_scroll_top())));
        assert_eq!(browse_count, count, "Browse must not resize the Sessions list state");
        assert_eq!(browse_top.item_ix, top.item_ix);
        assert_eq!(browse_top.offset_in_item, top.offset_in_item);
        vc.update(|_, cx| baaz.update(cx, |h, _| h.mode = crate::layout::Mode::Sessions));
        render_once(vc, &baaz);
        let back = vc.update(|_, cx| baaz.update(cx, |h, _| h.sidebar_list.logical_scroll_top()));
        assert_eq!(back.item_ix, top.item_ix, "switching back lands where the person was");
        b3c_teardown(&dir, guard, old);
    }

    #[gpui::test]
    fn a_sidebar_wheel_disarms_the_reveal_and_marks_user_scrolled(cx: &mut TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (dir, guard, old, args) = b3c_state("wheel-disarm");
        let vc = cx.add_empty_window();
        let baaz = b3c_window(vc, args);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.reveal = Some("s1".to_owned());
                h.reveal_unknown = Some(("s1".to_owned(), 2));
                h.sidebar_user_scrolled = false;
                h.note_sidebar_wheel(cx);
                assert!(h.reveal.is_none());
                assert!(h.reveal_unknown.is_none());
                assert!(h.sidebar_user_scrolled);
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
        vc.update(|_, cx| baaz.update(cx, |h, _| load_sessions(h, 40)));
        render_once(vc, &baaz);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.sidebar_list.scroll_to(gpui::ListOffset { item_ix: 6, offset_in_item: px(0.0) });
                let before = h.sidebar_list.logical_scroll_top();
                assert!(h.sidebar_pinned(cx).iter().any(|row| row.id.as_ref() == "s39"), "s39 is pinned");
                let grouping = h.sidebar_grouping(cx);
                let rows = flatten_sidebar(&grouping, false);
                h.reveal = Some("s39".to_owned());
                h.reveal_sidebar_row(&rows, &grouping, "s39", cx);
                assert!(h.reveal.is_none(), "a pinned row is always in view");
                let after = h.sidebar_list.logical_scroll_top();
                assert_eq!((after.item_ix, after.offset_in_item), (before.item_ix, before.offset_in_item));
            })
        });
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
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
                h.browser.tabs_loaded = true;
                h.browser.tabs.ensure_spaces(|_| None);
                let space = h.browse_space();
                let folder = h.browser.tabs.add_folder(crate::browser::tabs::PERSONAL, "Reading");
                let filed = h.browser.tabs.open(space.clone(), Owner::Person, "https://filed.example/", None);
                let pinned = h.browser.tabs.open(space, Owner::Person, "https://pinned.example/", None);
                let session = h.browser.tabs.open(None, Owner::Session("sess-1".into()), "https://task.example/", None);
                assert!(h.browser.tabs.set_folder(filed, Some(&folder)));
                assert!(h.browser.tabs.set_pinned(pinned, true));
                (filed, pinned, session)
            })
        });
        let browse = vc.update(|_, cx| baaz.update(cx, |h, cx| h.sidebar_model(cx).browse));
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
}
