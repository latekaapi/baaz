//! The sidebar divider's drag: the six fields that only a drag reads, and the
//! four handlers the resize strip calls.
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

use aui::shell::SIDEBAR_WIDTH;
use gpui::Context;

use crate::app::Harness;
use crate::layout;

/// Two taps with no travel count as a double-click: the handle reports
/// positions only, never the click count, so recency is the reset signal.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// The sidebar divider's geometry and whatever drag is in flight over it.
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
    /// release outside the window can never end it: `on_frame` leaves it
    /// alone and only `end_resize` settles it. Never set by the strip.
    pub(crate) scripted: bool,
    /// A frame-paced width sweep (`resize-sweep` step, scripting only): the
    /// divider marches toward the target width one step per rendered frame —
    /// the display link's pace on a real display — so per-tick resize cost
    /// is measurable headlessly. `on_frame` owns it; it never persists.
    pub(crate) sweep: Option<ResizeSweep>,
}

/// One frame-paced width sweep: march [`ResizeDrag::width`] toward `target`
/// by at most `step` per rendered frame.
#[derive(Debug)]
pub(crate) struct ResizeSweep {
    pub(crate) target: f32,
    step: f32,
    pub(crate) ticks: u32,
}

impl ResizeSweep {
    pub(crate) fn new(target: f32, step: f32) -> Self {
        Self { target, step: step.abs().max(0.5), ticks: 0 }
    }

    /// The next width toward the target, clamped to the divider's range.
    pub(crate) fn advance(&mut self, from: f32) -> f32 {
        self.ticks += 1;
        let delta = self.target - from;
        let next = if delta.abs() <= self.step { self.target } else { from + self.step * delta.signum() };
        aui::shell::clamp_sidebar_width(next)
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

    /// The divider's settled x, for `layout.json`. Small and synchronous like
    /// the sessions store: one pretty object, best-effort. A read-modify-write
    /// rather than a fresh object, so a drag never drops the grouping, the
    /// closed groups or the search scope the person chose.
    pub(crate) fn persist(&self) {
        let mut layout = layout::read();
        layout.sidebar_width = Some(self.width);
        layout::write(&layout);
    }
}

impl Harness {
    /// The press on the resize strip: arm the drag from the grab point.
    ///
    /// A resize drag owns the list exactly like a wheel gesture does (owner
    /// round 5 §A1, owner round 6): it disarms any armed reveal — a drag
    /// starting within a few frames of an outside open must not scroll the
    /// sessions list itself — and no reveal installs while it is in flight.
    pub(crate) fn begin_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        self.reveal = None;
        self.sidebar_user_scrolled = true;
        let drag = &mut self.resize;
        drag.active = true;
        drag.grab_x = x;
        drag.start_w = drag.width;
        drag.moved = 0.0;
        cx.notify();
    }

    /// A move with the button held: the divider follows from where the drag
    /// started, clamped, with no spring between it and the pointer.
    pub(crate) fn drag_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        let drag = &mut self.resize;
        if !drag.active {
            return;
        }
        let width = layout::drag_width(drag.start_w, drag.grab_x, x);
        drag.moved = drag.moved.max((width - drag.start_w).abs());
        drag.width = width;
        cx.notify();
    }

    /// The release, wherever it lands: disarm, settle, persist. A release
    /// with no travel shortly after the previous one is the handle's
    /// double-click, which resets to the default width instead of keeping a
    /// tap that moved nothing.
    pub(crate) fn end_resize(&mut self, cx: &mut Context<Self>) {
        let drag = &mut self.resize;
        if !drag.active {
            return;
        }
        drag.active = false;
        drag.scripted = false;
        let now = std::time::Instant::now();
        if drag.moved < 2.0
            && drag.last_release.is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK_WINDOW)
        {
            drag.width = SIDEBAR_WIDTH;
            drag.last_release = None;
        } else {
            drag.last_release = Some(now);
        }
        drag.persist();
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sweep marches one step per tick, snaps when the step would
    /// overshoot, and never leaves the divider's range (owner round 6).
    #[test]
    fn a_sweep_marches_by_the_step_and_snaps_at_the_target() {
        let mut sweep = ResizeSweep::new(412.0, 4.0);
        assert_eq!(sweep.advance(252.0), 256.0);
        assert_eq!(sweep.advance(410.0), 412.0);
        assert_eq!(sweep.advance(412.0), 412.0);
    }

    #[test]
    fn a_sweep_marches_down_as_well_as_up() {
        let mut sweep = ResizeSweep::new(252.0, 4.0);
        assert_eq!(sweep.advance(412.0), 408.0);
        assert_eq!(sweep.advance(254.0), 252.0);
    }

    #[test]
    fn a_sweep_stops_at_the_clamp() {
        // Past the maximum the clamp holds every tick, which is what ends
        // the sweep instead of marching forever.
        let mut sweep = ResizeSweep::new(10_000.0, 4.0);
        assert_eq!(sweep.advance(418.0), aui::shell::SIDEBAR_MAX_WIDTH);
        assert_eq!(sweep.advance(420.0), aui::shell::SIDEBAR_MAX_WIDTH);
    }
}
