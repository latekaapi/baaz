//! The dividers' drags: the fields that only a drag reads ([`ResizeDrag`],
//! held by the sidebar pane; [`RightResizeDrag`], held by the right pane),
//! and the handlers the resize strips call.
//!
//! The strip reports positions and nothing else — no click count, no press
//! state — so everything a drag needs to be reconstructed lives in
//! [`ResizeDrag`]: where the pointer grabbed, what the width was at that
//! moment, how far it has travelled since, and when the last release was. Two
//! rules come out of that:
//!
//! * **Every move measures from the press**, never from the previous frame, so
//!   a stalled frame can never compound an error.
//! * **A release with no travel shortly after the previous one is a
//!   double-click**, which resets to the default width; recency is the only
//!   signal the strip gives for it.
//!
//! The width is local state while the drag is in flight and `layout.json`
//! (see [`crate::layout`]) once it settles. Nothing here touches sessions,
//! login or menus.

use aui::shell::{RIGHT_WIDTH, SIDEBAR_WIDTH};
use gpui::Context;

use crate::app::Harness;
use crate::layout;
use crate::layout::LayoutGlobal as _;

/// Two taps with no travel count as a double-click: the handle reports
/// positions only, never the click count, so recency is the reset signal.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// The sidebar divider's geometry and whatever drag is in flight over it,
/// held by the sidebar pane ([`crate::sidebar_view::SidebarPane::resize`]).
pub(crate) struct ResizeDrag {
    /// The sidebar divider's current x, in window pixels. Local state until
    /// the drag settles, then `layout.json`.
    pub(crate) width: f32,
    /// A resize drag is in flight: the shell skips its layout spring so the
    /// divider tracks the pointer, and the capture overlay owns every move.
    pub(crate) active: bool,
    /// The pointer x where the drag started, in window pixels.
    grab_x: f32,
    /// [`Self::width`] when the drag started: every move measures from here,
    /// so a stalled frame can never compound an error.
    start_w: f32,
    /// How far the width has travelled this drag, in pixels.
    moved: f32,
    /// When the last drag ended, for the double-click reset above.
    last_release: Option<std::time::Instant>,
    /// A scripted drag (`resize-begin/move/end` steps) has no pointer, so a
    /// release outside the window can never end it: the deactivation rule
    /// (`Harness::on_window_activation`) leaves it alone and only
    /// `end_resize` settles it. Never set by the strip.
    pub(crate) scripted: bool,
    // Set only by the probe's `resize-sweep:` step.
    #[cfg_attr(not(feature = "probe"), allow(dead_code))]
    /// A frame-paced width sweep (`resize-sweep` step, scripting only): the
    /// divider marches toward the target width one step per rendered frame —
    /// the display link's pace on a real display — so per-tick resize cost
    /// is measurable headlessly. Its ticks run one per frame from the step
    /// (`Harness::resize_sweep_tick`); it never persists.
    pub(crate) sweep: Option<ResizeSweep>,
}

// Built only by the probe's `resize-sweep:` step.
#[cfg_attr(not(feature = "probe"), allow(dead_code))]
/// One frame-paced width sweep: march [`ResizeDrag::width`] toward `target`
/// by at most `step` per rendered frame.
#[derive(Debug)]
pub(crate) struct ResizeSweep {
    pub(crate) target: f32,
    step: f32,
    pub(crate) ticks: u32,
}

impl ResizeSweep {
    #[cfg(feature = "probe")]
    pub(crate) fn new(target: f32, step: f32) -> Self {
        Self { target, step: step.abs().max(0.5), ticks: 0 }
    }

    #[cfg(feature = "probe")]
    /// The next width toward the target, clamped to the divider's range.
    pub(crate) fn advance(&mut self, from: f32) -> f32 {
        self.ticks += 1;
        let delta = self.target - from;
        let next = if delta.abs() <= self.step { self.target } else { from + self.step * delta.signum() };
        aui::shell::clamp_sidebar_width(next)
    }
}

/// One frame-paced scroll sweep: applies a wheel
/// delta once per rendered frame — the display link's pace on a real
/// display — standing in for a posted trackpad gesture when the
/// environment cannot deliver real `CGEvent`s to the window (this
/// machine's screen session refused `NSRunningApplication.activate()` and
/// `screencapture` returned solid black, so a posted event's destination
/// window could not be confirmed — see `docs/02-app.md`). A finger phase
/// of `finger` ticks at a constant `dy`, then a tail of `tail` ticks
/// decaying exponentially to 5% of `dy` — the same shape `postscroll.swift`
/// (the real-`CGEvent` tool in the scratch `rwtools/` used for this
/// investigation) posts, so a sweep and a real gesture are comparable.
#[derive(Debug)]
pub(crate) struct ScrollSweep {
    dy: f32,
    finger_left: u32,
    tail_left: u32,
    tail_total: u32,
    pub(crate) ticks: u32,
}

impl ScrollSweep {
    #[cfg(feature = "probe")]
    pub(crate) fn new(dy: f32, finger: u32, tail: u32) -> Self {
        Self { dy, finger_left: finger, tail_left: tail, tail_total: tail.max(1), ticks: 0 }
    }

    /// The delta to apply this tick, or `None` once the sweep is done —
    /// the caller drops it then.
    pub(crate) fn advance(&mut self) -> Option<f32> {
        self.ticks += 1;
        if self.finger_left > 0 {
            self.finger_left -= 1;
            Some(self.dy)
        } else if self.tail_left > 0 {
            // `progress` walks 0..1 across the tail (0 on the first tail
            // tick, 1 on the last); 0.05^progress decays from 1.0 (full
            // `dy`) to 0.05 (5% of `dy`) — an exponential curve, matching
            // the Swift tool's own momentum decay. A one-tick tail has no
            // span to walk, so it decays straight to the tail's end value.
            let progress = if self.tail_total > 1 {
                (self.tail_total - self.tail_left) as f32 / (self.tail_total - 1) as f32
            } else {
                1.0
            };
            self.tail_left -= 1;
            Some(self.dy * 0.05f32.powf(progress))
        } else {
            None
        }
    }
}

impl ResizeDrag {
    /// The settled state at boot: whatever `layout.json` restored.
    pub(crate) fn restored(width: f32) -> Self {
        Self {
            width,
            active: false,
            grab_x: 0.0,
            start_w: width,
            moved: 0.0,
            last_release: None,
            scripted: false,
            sweep: None,
        }
    }

    /// The divider's settled x, for `layout.json`: one field of the layout
    /// global, written with the rest. It used to be a read-modify-write of
    /// the file beside the window's own copy, which the next preference
    /// write then overwrote with the width the window booted at.
    pub(crate) fn persist(&self, cx: &mut gpui::App) {
        let width = self.width;
        layout::Layout::update(cx, |layout| layout.sidebar_width = Some(width));
    }

    /// Arm the drag from the grab point `x`.
    pub(crate) fn begin(&mut self, x: f32) {
        self.active = true;
        self.grab_x = x;
        self.start_w = self.width;
        self.moved = 0.0;
    }

    /// One move to `x`: the divider follows from where the drag started,
    /// clamped. False while no drag is armed.
    pub(crate) fn drag_to(&mut self, x: f32) -> bool {
        if !self.active {
            return false;
        }
        let width = layout::drag_width(self.start_w, self.grab_x, x);
        self.moved = self.moved.max((width - self.start_w).abs());
        self.width = width;
        true
    }

    /// The release: disarm and settle — a release with no travel shortly
    /// after the previous one is the handle's double-click, which resets to
    /// the default width instead of keeping a tap that moved nothing. False
    /// while no drag is armed; the caller persists.
    pub(crate) fn end(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        self.scripted = false;
        let now = std::time::Instant::now();
        if self.moved < 2.0 && self.last_release.is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK_WINDOW) {
            self.width = SIDEBAR_WIDTH;
            self.last_release = None;
        } else {
            self.last_release = Some(now);
        }
        true
    }
}

/// The right-pane divider's geometry and whatever drag is in flight over
/// it: a separate struct rather than a `which` discriminant on
/// [`ResizeDrag`], so every existing sidebar-drag path — the handlers, the
/// scripted `resize-*` steps, the frame-paced sweep — keeps working
/// untouched. The two drags persist to different `layout.json` keys, reset
/// to different defaults, and run the drag math with opposite signs, so
/// sharing one struct would thread a discriminant through all of that for
/// no gain. The two are never active at once: each `begin_*` refuses while
/// the other runs, which is also why one capture overlay covers both. The
/// right pane entity owns this one ([`crate::right_pane::RightPane::resize`]);
/// the root's handlers below drive it there.
pub(crate) struct RightResizeDrag {
    /// The right divider's current width, in window pixels. Local state
    /// until the drag settles, then `layout.json`'s `rightWidth`.
    pub(crate) width: f32,
    /// A right-pane resize drag is in flight: the shell skips its layout
    /// spring so the divider tracks the pointer, and the capture overlay
    /// owns every move.
    pub(crate) active: bool,
    /// The pointer x where the drag started, in window pixels.
    grab_x: f32,
    /// [`Self::width`] when the drag started: every move measures from here,
    /// so a stalled frame can never compound an error.
    start_w: f32,
    /// How far the width has travelled this drag, in pixels.
    moved: f32,
    /// When the last drag ended, for the double-click reset.
    last_release: Option<std::time::Instant>,
    /// A scripted drag has no pointer, so a release outside the window can
    /// never end it. Never set by the strip.
    pub(crate) scripted: bool,
}

impl RightResizeDrag {
    /// The settled state at boot: whatever `layout.json` restored.
    pub(crate) fn restored(width: f32) -> Self {
        Self {
            width,
            active: false,
            grab_x: 0.0,
            start_w: width,
            moved: 0.0,
            last_release: None,
            scripted: false,
        }
    }

    /// The divider's settled width, for `layout.json`'s `rightWidth`: one
    /// field of the layout global, written with the rest (see
    /// [`ResizeDrag::persist`]).
    pub(crate) fn persist(&self, cx: &mut gpui::App) {
        let width = self.width;
        layout::Layout::update(cx, |layout| layout.right_width = Some(width));
    }

    /// Arm the drag from the grab point `x`.
    fn begin(&mut self, x: f32) {
        self.active = true;
        self.grab_x = x;
        self.start_w = self.width;
        self.moved = 0.0;
    }

    /// One move to `x`: the divider follows from where the drag started —
    /// narrowing as the pointer moves right — clamped. False while no drag
    /// is armed.
    fn drag_to(&mut self, x: f32) -> bool {
        if !self.active {
            return false;
        }
        let width = layout::right_drag_width(self.start_w, self.grab_x, x);
        self.moved = self.moved.max((width - self.start_w).abs());
        self.width = width;
        true
    }

    /// The release: disarm and settle — a release with no travel shortly
    /// after the previous one is the double-click, which resets to the
    /// default width. False while no drag is armed; the caller persists.
    fn end(&mut self) -> bool {
        if !self.active {
            return false;
        }
        self.active = false;
        self.scripted = false;
        let now = std::time::Instant::now();
        if self.moved < 2.0 && self.last_release.is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK_WINDOW) {
            self.width = RIGHT_WIDTH;
            self.last_release = None;
        } else {
            self.last_release = Some(now);
        }
        true
    }
}

impl Harness {
    /// The press on the sidebar strip: the pane arms its drag
    /// ([`crate::sidebar_view::SidebarPane::begin_resize`]). Refuses while
    /// the right pane's drag runs: both dividers can never be under the
    /// pointer at once.
    pub(crate) fn begin_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if self.right_pane.read(cx).resize.active {
            return;
        }
        self.sidebar_pane.update(cx, |pane, cx| pane.begin_resize(x, cx));
    }

    /// A move with the button held on the sidebar strip.
    pub(crate) fn drag_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        self.sidebar_pane.update(cx, |pane, cx| pane.drag_resize(x, cx));
    }

    /// The release on the sidebar strip, wherever it lands.
    pub(crate) fn end_resize(&mut self, cx: &mut Context<Self>) {
        self.sidebar_pane.update(cx, |pane, cx| pane.end_resize(cx));
    }

    /// The window went inactive: a sidebar drag still armed is over,
    /// settled where it stands (the pane's own rule, which spares a
    /// scripted drag).
    pub(crate) fn settle_resize_on_deactivate(&mut self, cx: &mut Context<Self>) {
        self.sidebar_pane.update(cx, |pane, cx| pane.settle_resize_on_deactivate(cx));
    }

    #[cfg(feature = "probe")]
    /// One tick of a `resize-sweep:` (scripting only): the divider marches
    /// one step, and the tick logs its width plus the renders and re-hint
    /// the frame before it cost, so per-tick resize cost is readable off a
    /// scripted run. Like a real drag it owns the list (no reveal installs
    /// while active); unlike one it never persists — a measurement, not a
    /// choice. The next tick waits for the frame this one asks for.
    pub(crate) fn resize_sweep_tick(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        // The pane marches its own divider and redraws (the shell, its
        // ancestor, with it); done at the target, when the clamp stops all
        // progress, or past any reasonable drag, it settles without
        // persisting.
        let Some((next, done)) = self.sidebar_pane.update(cx, |pane, cx| pane.tick_resize_sweep(cx)) else {
            return;
        };
        let pane = crate::sidebar_view::take_sidebar_pane_renders();
        let root = crate::sidebar_view::take_baaz_root_renders();
        let rehint = self.active.clone().map(|view| view.update(cx, |view, _| view.take_trace_rehint())).unwrap_or(false);
        crate::baaz_log!("rssweep w={next:.1} pane={pane} root={root} rehint={}", rehint as u8);
        if done {
            crate::baaz_log!("rssweep done w={next:.1}");
        } else {
            cx.on_next_frame(window, |this, window, cx| this.resize_sweep_tick(window, cx));
        }
    }

    /// A sidebar drag is in flight.
    pub(crate) fn sidebar_resizing(&self, cx: &gpui::App) -> bool {
        self.sidebar_pane.read(cx).resize.active
    }

    /// The sidebar divider's x now.
    pub(crate) fn sidebar_width(&self, cx: &gpui::App) -> f32 {
        self.sidebar_pane.read(cx).resize.width
    }

    /// The press on the right pane's strip: arm the pane's drag from the
    /// grab point, exactly like [`Harness::begin_resize`]. Refuses while the
    /// sidebar drag runs: both dividers can never be under the pointer at
    /// once, so the second press is a no-op rather than a second drag.
    pub(crate) fn begin_right_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if self.sidebar_resizing(cx) {
            return;
        }
        self.sidebar_pane.update(cx, |pane, _| pane.user_owns_list());
        self.right_pane.update(cx, |pane, _| pane.resize.begin(x));
        cx.notify();
    }

    /// A move with the button held on the right strip: the divider follows
    /// from where the drag started — narrowing as the pointer moves right —
    /// clamped, with no spring between it and the pointer.
    pub(crate) fn drag_right_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if self.right_pane.update(cx, |pane, _| pane.resize.drag_to(x)) {
            cx.notify();
        }
    }

    /// The release, wherever it lands: disarm, settle, persist to
    /// `rightWidth`. A release with no travel shortly after the previous
    /// one is the handle's double-click, which resets to the default right
    /// width instead of keeping a tap that moved nothing. The persist is a
    /// layout-global update, whose observers (the root) redraw.
    pub(crate) fn end_right_resize(&mut self, cx: &mut Context<Self>) {
        self.right_pane.update(cx, |pane, cx| {
            if pane.resize.end() {
                pane.resize.persist(cx);
            }
        });
    }
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep marches one step per tick, snaps when the step would
    /// overshoot, and never leaves the divider's range.
    #[test]
    fn a_sweep_marches_by_the_step_and_snaps_at_the_target() {
        // Inside the divider's range (docs/83 §2.5: 220–360).
        let mut sweep = ResizeSweep::new(352.0, 4.0);
        assert_eq!(sweep.advance(252.0), 256.0);
        assert_eq!(sweep.advance(350.0), 352.0);
        assert_eq!(sweep.advance(352.0), 352.0);
    }

    #[test]
    fn a_sweep_marches_down_as_well_as_up() {
        let mut sweep = ResizeSweep::new(252.0, 4.0);
        assert_eq!(sweep.advance(352.0), 348.0);
        assert_eq!(sweep.advance(254.0), 252.0);
    }

    #[test]
    fn a_sweep_stops_at_the_clamp() {
        // Past the maximum the clamp holds every tick, which is what ends
        // the sweep instead of marching forever.
        let mut sweep = ResizeSweep::new(10_000.0, 4.0);
        assert_eq!(sweep.advance(aui::shell::SIDEBAR_MAX_WIDTH - 2.0), aui::shell::SIDEBAR_MAX_WIDTH);
        assert_eq!(sweep.advance(aui::shell::SIDEBAR_MAX_WIDTH), aui::shell::SIDEBAR_MAX_WIDTH);
    }

    /// A scroll sweep holds a constant delta for the finger phase, then
    /// decays to 5% of it over the tail, then stops.
    #[test]
    fn a_scroll_sweep_holds_then_decays_then_stops() {
        let mut sweep = ScrollSweep::new(-20.0, 3, 2);
        assert_eq!(sweep.advance(), Some(-20.0));
        assert_eq!(sweep.advance(), Some(-20.0));
        assert_eq!(sweep.advance(), Some(-20.0));
        assert_eq!(sweep.advance(), Some(-20.0)); // tail tick 1: progress 0 -> full dy
        assert_eq!(sweep.advance(), Some(-1.0)); // tail tick 2: progress 1 -> 5% of dy
        assert_eq!(sweep.advance(), None);
        assert_eq!(sweep.advance(), None);
    }

    #[test]
    fn a_scroll_sweep_with_no_tail_stops_right_after_the_finger_phase() {
        let mut sweep = ScrollSweep::new(10.0, 2, 0);
        assert_eq!(sweep.advance(), Some(10.0));
        assert_eq!(sweep.advance(), Some(10.0));
        assert_eq!(sweep.advance(), None);
    }
}
