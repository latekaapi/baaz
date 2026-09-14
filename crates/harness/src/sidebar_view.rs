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
    dense_field, group_row, nav_item, rail, sidebar_footer, sidebar_view, view_menu, GroupAction, MenuRow, RailItem,
    RowAction, SidebarView,
};
use aui::overlay::{anchored_menu, popover_layer, MenuAlign, MenuSide};
use aui_icons::{IconName, Provider};
use aui_tokens::{scale, ActiveAui, AgentState, AuiStyled};
use gpui::{
    div, point, prelude::*, px, AnyElement, Bounds, Context, Focusable, Pixels, Point, Render, ScrollHandle,
    SharedString, WeakEntity, Window,
};
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
/// (hover, its scroll container, the rename editor, the reveal prepaint
/// intents) dirty it directly through the view tree.
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
    reveal_stable: bool,
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
            reveal_stable: harness.reveal_stable,
            auth,
            tier_args: harness.args.tier.is_some(),
            tier,
            current_project: harness.current_project.clone(),
        }
    }
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

    /// The sessions list's current scroll offset in pixels (owner round 5
    /// §A1.6): what `sidebar-wheel:` samples. Negative downward, like the
    /// handle's own offset — the sidebar analogue of
    /// `SessionView::bench_list_px`.
    pub(crate) fn sidebar_px(&self) -> f32 {
        f32::from(self.sessions_scroll.offset().y)
    }

    /// Whether a sidebar wheel gesture is in flight (owner round 5 §A1):
    /// an event landed within the horizon. While this holds the pane
    /// presents every tick and no reveal installs.
    pub(crate) fn sidebar_gesture_active(&self) -> bool {
        self.sidebar_wheel.borrow().gesture_until.is_some_and(|until| std::time::Instant::now() < until)
    }

    /// Apply the capture handler's input (owner round 5 §A1): take the
    /// accumulated travel into exactly one clamped offset write, disarm
    /// any armed reveal (the user's scroll wins), and mark scrolled-since-
    /// armed so none reinstalls until the next activation. Called once per
    /// pane render from `render_sidebar`, before anything reads the offset
    /// — the sidebar twin of `SessionView::drain_pending_wheel`.
    pub(crate) fn drain_sidebar_wheel(&mut self) {
        let (pending, scrolled) = {
            let mut state = self.sidebar_wheel.borrow_mut();
            (std::mem::replace(&mut state.pending, px(0.0)), std::mem::replace(&mut state.scrolled, false))
        };
        if scrolled {
            self.reveal = None;
            self.reveal_stable = false;
            self.sidebar_user_scrolled = true;
        }
        if pending == px(0.0) {
            return;
        }
        let offset = self.sessions_scroll.offset();
        let max = self.sessions_scroll.max_offset();
        let next_y = crate::sidebar::clamp_sidebar_offset(f32::from(offset.y), f32::from(pending), f32::from(max.y));
        self.sessions_scroll.set_offset(point(offset.x, px(next_y)));
        SIDEBAR_WHEEL_DRAINS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
        // Owner round 5 §A1: one offset per frame. The capture handler only
        // accumulates; this drain is the frame's single offset write.
        self.drain_sidebar_wheel();
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
        // The click's target first: the row highlights on the click's own
        // frame, before the new view (or any page) exists.
        let selected =
            self.pending_id.clone().or_else(|| self.active.as_ref().map(|a| a.read(cx).session_id.clone()));
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
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
        // scroll div (see `render_sessions_caption`), so it never scrolls
        // away and the list always starts below it.
        let mut view = sidebar_view("sessions", Rc::clone(&grouping))
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
            view = view.editing(renaming, self.rename_field(window, cx));
        }
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        // The one-shot reveal (owner round 4, O6; owner round 5 §A1): armed
        // in `activate` for every activation path, consumed on the first
        // prepaint after it. Never from a list refresh, a regroup, during
        // a wheel gesture, or after the user has scrolled — the flag only
        // exists between an activation and its prepaint, and a wheel
        // disarms it outright (`push_sidebar_wheel`).
        if !self.sidebar_user_scrolled && !self.sidebar_gesture_active() {
            if let Some(reveal_id) = self.reveal.clone() {
                view = self.install_reveal(view, &grouping, &reveal_id, cx);
            }
        }
        // No quick-filter field: ⌘⇧F and the sidebar search icon open the
        // full-text search palette instead, so the two can never share the
        // sidebar.
        v_flex()
            .size_full()
            .child(self.render_nav_block(cx))
            .child(self.render_sessions_caption(cx))
            // The list's positioned wrapper (owner round 5 §A1): `relative`
            // only establishes the containing block — the capture canvas
            // below resolves against this instead of the window (the same
            // load-bearing `relative` the transcript wrapper wears for its
            // own `wheel_capture`). A plain box otherwise: layout and paint
            // are unchanged.
            .child(
                div()
                    .flex_1()
                    .min_h(px(0.0))
                    .relative()
                    .child(sidebar_wheel_capture(self))
                    .child(
                        div()
                            .id("sessions-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            // Tracked so the reveal can read the viewport and move
                            // the offset; observing changes nothing about the
                            // scrolling itself. The fixed caption above is outside
                            // this div, so the list clips at the div's own top edge
                            // and nothing from it ever reaches the nav rows.
                            .track_scroll(&self.sessions_scroll)
                            .child(view)
                            .children(empty),
                    ),
            )
            .child(self.render_footer(cx))
            .into_any_element()
    }

    /// Fold one reveal prepaint's [`RevealProgress`] into the flag.
    ///
    /// A scroll that landed whole consumes the flag at once. An inside
    /// reading consumes it only when the previous prepaint read inside too:
    /// a single inside reading can come from a frame whose layout has not
    /// settled, and consuming on it strands the scroll wherever that frame
    /// left it. Anything else leaves the flag (and the confirmation bit)
    /// for the next prepaint.
    fn settle_reveal(&mut self, progress: RevealProgress, cx: &mut Context<Self>) {
        match progress {
            // The flag is kept, so the intent must be re-installed: poke the
            // pane directly, because a cached pane would otherwise never
            // re-run the prepaint whose layout has not settled (owner
            // round 4 §3). The terminal arms consume the flag, which the key
            // picks up on the next frame.
            RevealProgress::NotReady => {
                self.sidebar_pane.update(cx, |_, cx| cx.notify());
            }
            RevealProgress::Inside => {
                if self.reveal_stable {
                    self.reveal = None;
                    self.reveal_stable = false;
                } else {
                    self.reveal_stable = true;
                }
                cx.notify();
            }
            RevealProgress::Landed => {
                self.reveal = None;
                self.reveal_stable = false;
                cx.notify();
            }
        }
    }

    /// Install the one-shot reveal for `reveal_id` on the sidebar view.
    ///
    /// When the row is rendered this frame, the selected-row intent scrolls
    /// the minimum distance that brings it into view and consumes the flag.
    /// When it is not rendered — its group is closed or folded past the cut
    /// — the current-group intent does the same for the group row and never
    /// auto-expands. Both intents settle through [`Self::settle_reveal`],
    /// which is what finally consumes the flag, so a later user scroll
    /// never fights a stale reveal.
    fn install_reveal(
        &mut self,
        view: SidebarView,
        grouping: &Grouping,
        reveal_id: &str,
        cx: &mut Context<Self>,
    ) -> SidebarView {
        let rendered = match grouping {
            Grouping::Project(groups) => {
                groups.iter().any(|g| g.sessions.iter().any(|s| s.id.as_ref() == reveal_id))
            }
            Grouping::Date(groups) => {
                groups.iter().any(|g| g.sessions.iter().any(|s| s.id.as_ref() == reveal_id))
            }
            Grouping::Status(groups) => {
                groups.iter().any(|g| g.sessions.iter().any(|s| s.id.as_ref() == reveal_id))
            }
        };
        let scroll = self.sessions_scroll.clone();
        let harness = cx.entity();
        if rendered {
            let reveal_id = reveal_id.to_owned();
            view.on_selected_prepainted(move |id, bounds, _window, app| {
                if id.as_ref() != reveal_id {
                    return;
                }
                let (progress, applied) = reveal_scroll(&scroll, bounds);
                if applied.is_some() {
                    // The write above lands too late for this frame's paint
                    // (the scroll div already threaded the old offset through
                    // the prepaint walk) and invalidating from inside draw
                    // schedules nothing — so notify from a task outside the
                    // draw, which schedules the frame that paints it. The
                    // flag is already consumed and the clamp keeps the point,
                    // so that frame is a plain repaint.
                    let notify = harness.clone();
                    app.spawn(async move |cx| {
                        cx.update(|cx| notify.update(cx, |_, cx| cx.notify()));
                    })
                    .detach();
                }
                harness.update(app, |this: &mut Harness, cx| this.settle_reveal(progress, cx));
            })
        } else {
            // The row is not on screen: reveal its group instead. The
            // session's own project when it names one still adopted, else
            // the current project — with neither, there is no group to
            // reveal and the flag clears instead of waiting (owner round 5
            // §A1: a waiting flag outlives its activation and can move a
            // list the user scrolled meanwhile).
            let group_id = self
                .sessions
                .iter()
                .find(|e| e.id == reveal_id)
                .and_then(|e| e.project.clone())
                .filter(|id| self.projects.find(id).is_some())
                .or_else(|| self.current_project.clone());
            let Some(group_id) = group_id else {
                self.reveal = None;
                self.reveal_stable = false;
                return view;
            };
            view.on_current_prepainted(move |id, bounds, _window, app| {
                if id.as_ref() != group_id {
                    return;
                }
                let (progress, applied) = reveal_scroll(&scroll, bounds);
                if applied.is_some() {
                    // Same late-write as the selected-row intent above: wake
                    // the painting frame from outside the draw.
                    let notify = harness.clone();
                    app.spawn(async move |cx| {
                        cx.update(|cx| notify.update(cx, |_, cx| cx.notify()));
                    })
                    .detach();
                }
                harness.update(app, |this: &mut Harness, cx| this.settle_reveal(progress, cx));
            })
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
    /// commit path is unchanged. The same element serves the sidebar row and
    /// the header title.
    pub(crate) fn rename_field(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let focused = self.rename.focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(cx.listener(|this, _: &ConfirmRename, window, cx| this.commit_rename(window, cx)))
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
                    .child(dense_field(&self.rename).h_auto().whitespace_nowrap().overflow_x_hidden()),
            )
            .into_any_element()
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
        if let Auth::SignedIn(identity) = &self.auth {
            rail = rail.avatar(identity.initial());
        }
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
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

/// What one reveal prepaint found: the scroll div not yet measured
/// ([`RevealProgress::NotReady`]), the row already inside
/// ([`RevealProgress::Inside`]), or a scroll that landed whole
/// ([`RevealProgress::Landed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevealProgress {
    NotReady,
    Inside,
    Landed,
}

/// Move the scroll offset by the minimum that brings `row` into the scroll
/// viewport, and nothing when it is already inside (owner round 4, O6).
///
/// The offset is ≤ 0 and grows negative as the list scrolls down, so the
/// answer is clamped into `[−max_offset.y, 0]`. Anything less than a whole
/// landing — a viewport or row with no height yet, a zero maximum offset,
/// or a clamp that cut the target short — reads [`RevealProgress::NotReady`]:
/// the scroll div has not measured yet and the flag must survive for the
/// next prepaint instead of dying on a stale maximum.
///
/// Returns the point written on a landing, so the caller can re-apply the
/// same point on the next frame: a write from inside a prepaint callback
/// lands too late for that frame's paint (the scroll div already threaded
/// the old offset through the walk), and only a pre-draw write paints.
fn reveal_scroll(scroll: &ScrollHandle, row: Bounds<Pixels>) -> (RevealProgress, Option<Point<Pixels>>) {
    let viewport = scroll.bounds();
    let offset = scroll.offset();
    let max = scroll.max_offset();
    let target = crate::sidebar::reveal_offset(viewport, row, offset.y);
    if f32::from(viewport.size.height) <= 0.0 {
        return (RevealProgress::NotReady, None);
    }
    if f32::from(row.size.height) <= 0.0 {
        return (RevealProgress::NotReady, None);
    }
    let Some(target) = target else { return (RevealProgress::Inside, None) };
    let max = f32::from(max.y);
    if max <= 0.0 {
        return (RevealProgress::NotReady, None);
    }
    let target = f32::from(target);
    let clamped = target.clamp(-max, 0.0);
    let applied = point(offset.x, px(clamped));
    scroll.set_offset(applied);
    if clamped == target {
        (RevealProgress::Landed, Some(applied))
    } else {
        (RevealProgress::NotReady, None)
    }
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

    #[test]
    fn view_menu_seat_for_the_default_sidebar() {
        // The standard 252 px sidebar with the caption at y 146: the menu
        // opens 4 px under it with its left clamped at 8 px (252 - 12 is
        // narrower than the 250 px menu). The seat takes no scroll offset,
        // so it stays under the sliders icon at any scroll position.
        assert_eq!(view_menu_seat(caption(0.0, 146.0, 252.0, 28.0)), (178.0, 8.0));
    }
}
