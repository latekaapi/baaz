//! F2 feedback mode shell: the setting-driven capture state, the capture
//! layer, and the bar + pill chrome around the F1 model.
//!
//! The F1 [`crate::feedback`] module owns the data ([`Draft`](crate::feedback::Draft),
//! [`ElementProbe`](crate::feedback::ElementProbe), [`write_report`](crate::feedback::write_report));
//! this module owns the UI state machine on top of it. One [`FeedbackUi`]
//! lives on [`Harness`](crate::app::Harness) as `feedback`.
//!
//! Phases: `Off` (nothing shown) → `Active` (the capture layer blocks the
//! app and the bar floats on top) → `Paused` (the app is usable, the pill
//! floats bottom-right). ⌃⌥⌘F toggles Off→Active→Paused→Active; Escape
//! pauses; Done writes the report and goes Off.
//!
//! F3 hook: hover/click/drag note-taking attaches to the capture layer
//! built by [`FeedbackUi::layer_base`] — add handlers there, next to the
//! swallowing ones, so every pointer trap stays in one place.

use std::time::{Duration, Instant};

use aui::data::button;
use aui::overlay::popover_layer;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, FocusHandle, Focusable, Point, Pixels, Window};
use gpui_kit::base::input::TextareaState;
use gpui_kit::base::{h_flex, v_flex};
use gpui_kit::component::input::Textarea;
use image::RgbaImage;

use crate::feedback::{
    crop_with_outline, human_role, render_window, Draft, ElementHit, ElementProbe, MacProbe, Rect, ScreenContext,
    Target,
};
use crate::wire::WireCall as _;

/// The key context the capture layer carries while Active, so Escape
/// reaches [`crate::app::FeedbackEscape`] instead of the composer and
/// typing never lands in a field behind the layer.
pub const FEEDBACK_CONTEXT: &str = "BaazFeedback";

/// The key context the note popover carries: a sibling of the layer (never
/// inside it), so typing shortcuts and Escape reach the note instead of
/// pausing the mode.
pub const FEEDBACK_NOTE_CONTEXT: &str = "BaazFeedbackNote";

/// A press travels this far before it becomes a drag, and a hover must move
/// this far before it re-hit-tests.
pub(crate) const FEEDBACK_DRAG_PX: f32 = 6.0;
/// A released box smaller than this in either axis counts as a click.
pub(crate) const FEEDBACK_MIN_BOX: f32 = 6.0;
/// A click with no hit falls back to this centred box.
pub(crate) const FEEDBACK_CLICK_W: f32 = 60.0;
/// A click with no hit falls back to this centred box.
pub(crate) const FEEDBACK_CLICK_H: f32 = 40.0;
/// The capture hides the overlay, repaints, then reads the window after two
/// painted frames; the timer below bounds the wait when frames never come.
pub(crate) const FEEDBACK_CAPTURE_TIMEOUT: Duration = Duration::from_millis(120);
/// A cold accessibility tree gets this long to warm before hover and click
/// fall back.
pub(crate) const FEEDBACK_PROBE_WARM: Duration = Duration::from_secs(1);

/// How long a first Discard click stays armed waiting for its confirm.
const DISCARD_ARM: Duration = Duration::from_secs(4);

/// What the feedback shell shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FeedbackPhase {
    /// Nothing shown; the app is untouched.
    #[default]
    Off,
    /// The capture layer blocks the app; the bar floats on top.
    Active,
    /// The app is usable; the pill floats bottom-right.
    Paused,
}

/// What `toggle` did, so the caller knows which side effects to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToggleOutcome {
    /// The setting is off: nothing changed, toast instead.
    NeedsSetting,
    /// Now Active: activate the probe, focus the layer, refresh.
    EnteredActive,
    /// Now Paused.
    Paused,
    /// Back to Active.
    Resumed,
}

/// What a Discard click did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiscardOutcome {
    /// No notes: the button is disabled, nothing happened.
    Disabled,
    /// First click: armed, waiting up to 4 s for the confirm.
    Armed,
    /// Second click inside the window: the draft is cleared.
    Cleared,
}

/// The note being written: its target and images plus the field holding
/// the owner's words. Open from a click or a drag once the capture lands.
pub struct PendingNote {
    /// What the note points at.
    pub target: Target,
    /// The outlined crop around the target, at device pixels, when the
    /// capture produced one.
    pub crop: Option<RgbaImage>,
    /// The full window shot, at device pixels, when the capture produced one.
    pub full: Option<RgbaImage>,
    /// The field holding the owner's words.
    pub field: Entity<TextareaState>,
}

/// A click that hit-tested nothing: the tree may still be cold, so the
/// shell retries on later frames before falling back to an area.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PendingClick {
    /// Where the owner clicked, in window-logical pixels.
    pub point: Point<Pixels>,
    /// When the click landed, bounding the warm retry.
    pub since: Instant,
}

/// The feedback shell's state: one per window, on `Harness`.
pub struct FeedbackUi {
    /// What the shell shows.
    pub phase: FeedbackPhase,
    /// The notes taken so far this session.
    pub draft: Draft,
    /// Hit-tests the window; [`MacProbe`] everywhere a real window exists,
    /// [`FakeProbe`] only under unit tests (explicit injection).
    pub probe: Box<dyn ElementProbe>,
    /// The element under the pointer while Active, for the hover outline.
    pub hover: Option<ElementHit>,
    /// Where the pointer was on the last hit-test, in window-logical
    /// pixels; moves under 2 px skip the next one.
    pub hover_point: Option<Point<Pixels>>,
    /// When the current hover wait started, bounding the warm retry while
    /// the accessibility tree is still cold.
    pub hover_since: Option<Instant>,
    /// Where the left button went down, while it is down.
    pub press: Option<Point<Pixels>>,
    /// Where the pointer is now, while a drag is live.
    pub drag: Option<Point<Pixels>>,
    /// While set, the layer draws nothing (no frame, outline, box, pins or
    /// chrome) but still occludes input, so the capture reads a clean frame.
    pub capturing: bool,
    /// The capture's generation: a stale frame callback never opens a
    /// popover for a note that has since been cancelled.
    pub capture_seq: u64,
    /// The target waiting for its capture to land, with the field already
    /// built so opening the popover is synchronous when it does.
    pub capture_pending: Option<(Target, Entity<TextareaState>)>,
    /// The open note popover, if the owner is writing one.
    pub popover: Option<PendingNote>,
    /// A click that hit-tested nothing, waiting for the tree to warm.
    pub pending_click: Option<PendingClick>,
    /// A first Discard click is waiting for its confirm.
    pub discard_armed: bool,
    /// When the arm started, for the 4 s window.
    pub discard_armed_at: Option<Instant>,
    /// The pending disarm task, if the arm is live. One slot, never a
    /// queue: re-arming replaces the previous task (dropping cancels it),
    /// so the arm can never stack timers on `wire_tasks`.
    pub discard_task: Option<gpui::Task<()>>,
    /// A Done write is in flight. While set, `done_enabled()` is false
    /// and toggle/pause/resume/discard/steps are no-ops, so a second
    /// Done cannot start a second report.
    pub saving: bool,
    /// The `feedback-mode:on` step verb parked the shell here for this
    /// run. Runtime-only, never `layout.feedback_mode`: [`Self::toggle`]
    /// treats it as the setting being on, and no layout save persists it.
    pub forced_on: bool,
    /// The capture layer's focus, taken on entering Active.
    pub focus: FocusHandle,
}

impl FeedbackUi {
    /// Blank shell in `Off` with `probe`. The focus handle needs the app.
    pub fn new(probe: Box<dyn ElementProbe>, cx: &mut gpui::App) -> Self {
        Self {
            phase: FeedbackPhase::Off,
            draft: Draft::new(),
            probe,
            hover: None,
            hover_point: None,
            hover_since: None,
            press: None,
            drag: None,
            capturing: false,
            capture_seq: 0,
            capture_pending: None,
            popover: None,
            pending_click: None,
            discard_armed: false,
            discard_armed_at: None,
            discard_task: None,
            saving: false,
            forced_on: false,
            focus: cx.focus_handle(),
        }
    }

    /// The probe for this run: the real macOS probe wherever a real window
    /// exists — the app and `--shot`/`--steps` captures alike, whose
    /// NSWindow carries a real accessibility tree. Unit tests inject a
    /// [`FakeProbe`] explicitly instead.
    pub fn probe_for() -> Box<dyn ElementProbe> {
        Box::new(MacProbe)
    }

    /// Close the popover without saving, dropping any in-flight capture so
    /// a stale frame callback never reopens it. Stays in phase.
    pub fn cancel_note(&mut self) {
        self.popover = None;
        self.capture_pending = None;
        self.capture_seq = self.capture_seq.wrapping_add(1);
        self.capturing = false;
        self.pending_click = None;
    }

    /// Clear the hover outline, for the bar's mouse-move catcher.
    pub fn clear_hover(&mut self) {
        self.hover = None;
        self.hover_point = None;
        self.hover_since = None;
    }

    /// Whether anything is shown.
    pub fn is_open(&self) -> bool {
        self.phase != FeedbackPhase::Off
    }

    /// Whether the capture layer covers the window (Active only — a
    /// paused shell leaves pages live).
    pub fn covers_app(&self) -> bool {
        self.phase == FeedbackPhase::Active
    }

    /// How many notes the draft holds.
    pub fn note_count(&self) -> usize {
        self.draft.len()
    }

    /// `N notes`, or `1 note`.
    pub fn notes_label(&self) -> String {
        let n = self.note_count();
        if n == 1 {
            "1 note".to_owned()
        } else {
            format!("{n} notes")
        }
    }

    /// Drop a stale arm: at or past [`DISCARD_ARM`].
    pub fn expire_discard(&mut self) {
        if self.discard_armed
            && self.discard_armed_at.is_some_and(|at| at.elapsed() >= DISCARD_ARM)
        {
            self.disarm_discard();
        }
    }

    /// Clear the arm without touching the draft.
    pub fn disarm_discard(&mut self) {
        self.discard_armed = false;
        self.discard_armed_at = None;
    }

    /// Whether the arm is live right now (stale arms read as off even
    /// before [`Self::expire_discard`] runs).
    pub fn discard_armed_live(&self) -> bool {
        self.discard_armed
            && self.discard_armed_at.is_some_and(|at| at.elapsed() <= DISCARD_ARM)
    }

    /// The Discard button's label: `Discard`, or `Discard N notes?` while
    /// the confirm is armed.
    pub fn discard_label(&self) -> String {
        if self.discard_armed_live() {
            format!("Discard {}?", self.notes_label())
        } else {
            "Discard".to_owned()
        }
    }

    /// Discard is clickable only with notes to drop.
    pub fn discard_enabled(&self) -> bool {
        !self.draft.is_empty()
    }

    /// Done is clickable in Active or Paused with notes to save — never
    /// while a write is in flight.
    pub fn done_enabled(&self) -> bool {
        !self.saving && self.phase != FeedbackPhase::Off && !self.draft.is_empty()
    }

    /// ⌃⌥⌘F / the menu item: setting off stays Off; Off → Active (a fresh
    /// draft when empty, the kept one otherwise); Active → Paused;
    /// Paused → Active. Pure — the caller runs the side effects the
    /// outcome names (toasts, probe activation, focus). The run-only
    /// [`Self::forced_on`] counts as the setting being on. While
    /// [`Self::saving`] the Harness guards the toggle outright (there is
    /// no stay-put outcome to return here).
    pub fn toggle(&mut self, setting_on: bool) -> ToggleOutcome {
        if !(setting_on || self.forced_on) {
            return ToggleOutcome::NeedsSetting;
        }
        match self.phase {
            FeedbackPhase::Off => {
                if self.draft.is_empty() {
                    self.draft = Draft::new();
                }
                self.cancel_note();
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::EnteredActive
            }
            FeedbackPhase::Active => {
                self.cancel_note();
                self.disarm_discard();
                self.phase = FeedbackPhase::Paused;
                ToggleOutcome::Paused
            }
            FeedbackPhase::Paused => {
                self.cancel_note();
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::Resumed
            }
        }
    }

    /// Escape, the Pause button: Active → Paused, anywhere else a no-op —
    /// and always a no-op while [`Self::saving`].
    pub fn pause(&mut self) {
        if self.phase == FeedbackPhase::Active && !self.saving {
            self.cancel_note();
            self.disarm_discard();
            self.phase = FeedbackPhase::Paused;
        }
    }

    /// The Resume button / `resume` verb: Paused → Active, else a no-op —
    /// and always a no-op while [`Self::saving`].
    pub fn resume(&mut self) {
        if self.phase == FeedbackPhase::Paused && !self.saving {
            self.disarm_discard();
            self.phase = FeedbackPhase::Active;
        }
    }

    /// Turning the setting off mid-session: Off, but the draft survives
    /// so the next toggle resumes it. Clears the run-only
    /// [`Self::forced_on`]: the shell is fully off, not parked for a run.
    pub fn setting_off(&mut self) {
        self.forced_on = false;
        if self.phase != FeedbackPhase::Off {
            self.cancel_note();
            self.disarm_discard();
            self.phase = FeedbackPhase::Off;
        }
    }

    /// A Discard click: disabled with 0 notes (or while
    /// [`Self::saving`]); the first click arms (the button reads
    /// `Discard N notes?` for up to 4 s); the second click inside the
    /// window clears the draft, staying in the current phase.
    /// A stale arm reads as a first click again.
    pub fn discard_press(&mut self) -> DiscardOutcome {
        if self.saving || self.draft.is_empty() {
            return DiscardOutcome::Disabled;
        }
        if self.discard_armed_live() {
            self.draft.discard();
            self.disarm_discard();
            DiscardOutcome::Cleared
        } else {
            self.discard_armed = true;
            self.discard_armed_at = Some(Instant::now());
            DiscardOutcome::Armed
        }
    }

    /// Discard arm + confirm in one go, for the `discard` step verb. A
    /// no-op while [`Self::saving`].
    pub fn discard_all(&mut self) {
        if self.saving {
            return;
        }
        self.draft.discard();
        self.disarm_discard();
    }

    /// A finished Done write: Off with a cleared draft, and the in-flight
    /// guard lifted.
    pub fn done_applied(&mut self) {
        self.cancel_note();
        self.disarm_discard();
        self.saving = false;
        self.phase = FeedbackPhase::Off;
        self.draft = Draft::new();
    }

    /// The target for a press-release at `point`: the hit element, or a
    /// 60×40 box centred on the point (clamped into `win`) when nothing
    /// labelled is there.
    pub(crate) fn click_target(
        hit: Option<ElementHit>,
        point: Point<Pixels>,
        win: gpui::Size<Pixels>,
    ) -> Target {
        match hit {
            Some(hit) => Target::Element(hit),
            None => {
                let w = FEEDBACK_CLICK_W.min(f32::from(win.width).max(1.0));
                let h = FEEDBACK_CLICK_H.min(f32::from(win.height).max(1.0));
                let x = (f32::from(point.x) - w / 2.0).clamp(0.0, (f32::from(win.width) - w).max(0.0));
                let y = (f32::from(point.y) - h / 2.0).clamp(0.0, (f32::from(win.height) - h).max(0.0));
                Target::Area { rect: Rect { x, y, w, h }, region: None }
            }
        }
    }

    /// The dragged box from `start` to `current`, normalised so dragging
    /// up-left works. `None` when either axis runs under 6 px: the release
    /// counts as a click instead.
    pub(crate) fn drag_rect(start: Point<Pixels>, current: Point<Pixels>) -> Option<Rect> {
        let (sx, sy) = (f32::from(start.x), f32::from(start.y));
        let (cx, cy) = (f32::from(current.x), f32::from(current.y));
        let (x0, x1) = if sx <= cx { (sx, cx) } else { (cx, sx) };
        let (y0, y1) = if sy <= cy { (sy, cy) } else { (cy, sy) };
        if x1 - x0 < FEEDBACK_MIN_BOX || y1 - y0 < FEEDBACK_MIN_BOX {
            return None;
        }
        Some(Rect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
    }

    /// Whether the pointer moved far enough from `press` to be dragging.
    pub(crate) fn is_drag(press: Point<Pixels>, current: Point<Pixels>) -> bool {
        let dx = (f32::from(current.x) - f32::from(press.x)).abs();
        let dy = (f32::from(current.y) - f32::from(press.y)).abs();
        dx.max(dy) >= FEEDBACK_DRAG_PX
    }

    /// Whether a hover moved far enough to re-hit-test.
    pub(crate) fn hover_moved(last: Point<Pixels>, point: Point<Pixels>) -> bool {
        let dx = (f32::from(point.x) - f32::from(last.x)).abs();
        let dy = (f32::from(point.y) - f32::from(last.y)).abs();
        dx.max(dy) >= 2.0
    }

    /// The rect a note points at: the element's frame or the area's box.
    pub(crate) fn target_rect(target: &Target) -> Rect {
        match target {
            Target::Element(hit) => hit.rect,
            Target::Area { rect, .. } => *rect,
        }
    }
}

/// The feedback colour: amber `#E8A64B` on dark, `#C9821F` on light. The
/// one place the hex lives — call sites read this, never a literal.
pub fn feedback_accent(cx: &gpui::App) -> gpui::Hsla {
    if cx.aui().kind.is_dark() {
        gpui::Hsla { h: 0.0966, s: 0.7734, l: 0.6020, a: 1.0 }
    } else {
        gpui::Hsla { h: 0.0971, s: 0.7328, l: 0.4549, a: 1.0 }
    }
}

/// Text on the feedback colour: near-black `#1A1206` on dark's amber,
/// white on light's. F3's hover chip, pins and note popover paint on the
/// feedback colour.
pub fn feedback_on_accent(cx: &gpui::App) -> gpui::Hsla {
    if cx.aui().kind.is_dark() {
        gpui::Hsla { h: 0.1000, s: 0.6250, l: 0.0627, a: 1.0 }
    } else {
        gpui::Hsla { h: 0.0, s: 0.0, l: 1.0, a: 1.0 }
    }
}

/// `baaz <version> (<debug|release>)`: the build string Done stamps on
/// the report.
pub fn feedback_build_string() -> String {
    format!(
        "baaz {} ({})",
        env!("CARGO_PKG_VERSION"),
        if cfg!(debug_assertions) { "debug" } else { "release" }
    )
}

/// Parse an `x,y` window-logical point for the `hover:`/`click:` step
/// subs. `None` on anything unparseable (or non-finite).
fn parse_feedback_point(payload: &str) -> Option<Point<Pixels>> {
    let (x, y) = payload.split_once(',')?;
    let (x, y): (f32, f32) = (x.trim().parse().ok()?, y.trim().parse().ok()?);
    if x.is_finite() && y.is_finite() {
        Some(gpui::point(gpui::px(x), gpui::px(y)))
    } else {
        None
    }
}

/// Parse an `x1,y1,x2,y2` drag for the `drag:` step sub: corner to corner
/// in window-logical pixels. `None` on anything unparseable (or
/// non-finite).
fn parse_feedback_drag(payload: &str) -> Option<(Point<Pixels>, Point<Pixels>)> {
    let mut parts = payload.split(',');
    let nums: Option<Vec<f32>> = parts
        .by_ref()
        .map(|part| part.trim().parse::<f32>().ok().filter(|n| n.is_finite()))
        .collect();
    let nums = nums?;
    if nums.len() != 4 || parts.next().is_some() {
        return None;
    }
    Some((
        gpui::point(gpui::px(nums[0]), gpui::px(nums[1])),
        gpui::point(gpui::px(nums[2]), gpui::px(nums[3])),
    ))
}

impl crate::app::Harness {
    /// ⌃⌥⌘F / Help → Feedback Mode: setting off toasts and stays Off;
    /// otherwise the toggle outcome, focusing the layer and priming the
    /// probe on the way into Active.
    pub(crate) fn toggle_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.saving {
            return;
        }
        match self.feedback.toggle(self.layout.feedback_mode) {
            ToggleOutcome::NeedsSetting => {
                self.overlays.update(cx, |o, _| {
                    o.toast("Feedback mode is off", "Turn it on in Settings → General.")
                });
                cx.notify();
            }
            ToggleOutcome::EnteredActive | ToggleOutcome::Resumed => {
                self.enter_feedback_active(window, cx);
            }
            ToggleOutcome::Paused => {
                window.focus(&self.focus_root, cx);
                cx.notify();
            }
        }
    }

    /// On the way into Active: close the palette, menu and dialog (they
    /// would otherwise sit frozen under the capture layer — Settings may
    /// stay open beneath it), touch the a11y tree so F3 can hit-test on
    /// a later frame, take focus so typing never reaches the composer,
    /// and repaint.
    pub(crate) fn enter_feedback_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = None;
            overlays.menu = None;
        });
        self.close_dialog(cx);
        self.feedback.probe.activate(window);
        window.focus(&self.feedback.focus, cx);
        window.refresh();
        cx.notify();
    }

    /// Escape while Active → Paused, parking focus on the root. Anywhere
    /// else a no-op (the binding only exists on the Active layer).
    pub(crate) fn escape_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.phase == FeedbackPhase::Active {
            self.feedback.pause();
            window.focus(&self.focus_root, cx);
            cx.notify();
        }
    }

    /// The bar's Pause button. A no-op while saving.
    pub(crate) fn pause_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.saving {
            return;
        }
        self.feedback.pause();
        window.focus(&self.focus_root, cx);
        cx.notify();
    }

    /// The pill's Resume button. A no-op while saving.
    pub(crate) fn resume_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.saving {
            return;
        }
        if self.feedback.phase == FeedbackPhase::Paused {
            self.feedback.resume();
            self.enter_feedback_active(window, cx);
        }
    }

    /// The bar's Discard button: arm, then clear on the confirm. A stale
    /// arm disarms itself on a timer so the label falls back. At most one
    /// disarm task is ever pending: re-arming replaces the previous one.
    /// A no-op while saving. Afterwards the layer keeps the keyboard when
    /// still Active, so Escape keeps working after the click.
    pub(crate) fn discard_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.saving {
            return;
        }
        match self.feedback.discard_press() {
            DiscardOutcome::Disabled => {}
            DiscardOutcome::Armed => {
                let task = cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(DISCARD_ARM).await;
                    let _ = this.update(cx, |this: &mut Self, cx| {
                        this.feedback.expire_discard();
                        cx.notify();
                    });
                });
                self.feedback.discard_task = Some(task);
                cx.notify();
            }
            DiscardOutcome::Cleared => {
                cx.notify();
            }
        }
        if self.feedback.phase == FeedbackPhase::Active {
            window.focus(&self.feedback.focus, cx);
        }
    }

    /// The Done button (Active or Paused, needs notes): write the report
    /// off the UI thread, then back on it copy the report dir path to the
    /// clipboard, toast it, and go Off with a cleared draft. A write
    /// error toasts instead and keeps the draft and the phase.
    ///
    /// The in-flight guard: `saving` is set before the spawn, so a second
    /// Done (button or step) is a no-op until this one lands — exactly one
    /// report folder per save. The note count is captured before the
    /// write, so the toast never says `0 notes`; the draft moves out with
    /// `mem::take` (never cloned on the UI thread) and moves back on
    /// error. Afterwards the layer keeps the keyboard when still Active,
    /// so Escape keeps working after the click.
    pub(crate) fn finish_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.feedback.done_enabled() {
            return;
        }
        self.feedback.saving = true;
        let note_count = self.feedback.note_count();
        let draft = std::mem::take(&mut self.feedback.draft);
        let root = crate::feedback::feedback_root();
        let build = feedback_build_string();
        let task = cx.spawn(async move |this, cx| {
            let (written, draft) = cx
                .background_executor()
                .spawn(async move {
                    let result =
                        crate::feedback::write_report(&root, &draft, chrono::Local::now(), &build);
                    (result, draft)
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                match written {
                    Ok(dir) => {
                        let path = dir.display().to_string();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                        let unit = if note_count == 1 { "note" } else { "notes" };
                        this.feedback.done_applied();
                        this.overlays.update(cx, |o, _| {
                            o.toast(format!("Feedback saved · {note_count} {unit}"), path)
                        });
                    }
                    Err(err) => {
                        this.feedback.draft = draft;
                        this.feedback.saving = false;
                        this.overlays.update(cx, |o, _| {
                            o.toast("Couldn't save feedback", format!("{err:#}"))
                        });
                    }
                }
                cx.notify();
            });
        });
        self.wire_tasks().push(task);
        if self.feedback.phase == FeedbackPhase::Active {
            window.focus(&self.feedback.focus, cx);
        }
        cx.notify();
    }

    /// The setting flipped off mid-session: the shell goes Off but keeps
    /// the draft, so the next toggle resumes it.
    pub(crate) fn feedback_setting_off(&mut self, cx: &mut Context<Self>) {
        self.feedback.setting_off();
        cx.notify();
    }

    /// `feedback-mode:<sub>`: `on` parks the run-only [`FeedbackUi::forced_on`]
    /// flag (never `layout.feedback_mode`) and enters Active; `pause` /
    /// `resume` move between Active and Paused; `off` clears the run's
    /// flag and parks Off keeping the draft; `discard` clears the draft
    /// at once; `done` saves like the Done button. Idempotent where it
    /// can be (`on` while Active stays Active). A no-op while saving. An
    /// unknown sub records a step failure — there is no forwarding: the
    /// session verb `feedback:<text>` (the open approval/clarify field)
    /// is a different verb again and dispatches on its own.
    ///
    /// The F3 note-taking subs run through the same handlers as the
    /// mouse: `hover:x,y` hovers, `click:x,y` presses and releases at one
    /// point, `drag:x1,y1,x2,y2` drags corner to corner, `note:<text>`
    /// fills the open popover and saves (completing an in-flight capture
    /// first), `cancel` closes the popover unsaved. Coordinates are
    /// window-logical pixels. A note sub with no popover and no pending
    /// capture, an empty text, an unparseable coordinate, or a pointing
    /// sub while not Active records a step failure.
    pub(crate) fn step_feedback(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.saving {
            return;
        }
        let rest = rest.trim();
        if let Some(payload) = rest.strip_prefix("hover:") {
            match parse_feedback_point(payload) {
                Some(point) if self.feedback.phase == FeedbackPhase::Active => {
                    self.feedback_hover_at(point, window, cx);
                }
                _ => crate::steps::record_step_failure(&format!("feedback-mode:{rest}")),
            }
            cx.notify();
            return;
        }
        if let Some(payload) = rest.strip_prefix("click:") {
            match parse_feedback_point(payload) {
                Some(point) if self.feedback.phase == FeedbackPhase::Active => {
                    self.feedback_press_at(point, window, cx);
                    self.feedback_release_at(point, window, cx);
                }
                _ => crate::steps::record_step_failure(&format!("feedback-mode:{rest}")),
            }
            cx.notify();
            return;
        }
        if let Some(payload) = rest.strip_prefix("drag:") {
            match parse_feedback_drag(payload) {
                Some((start, end)) if self.feedback.phase == FeedbackPhase::Active => {
                    self.feedback_press_at(start, window, cx);
                    self.feedback_drag_at(end, cx);
                    self.feedback_release_at(end, window, cx);
                }
                _ => crate::steps::record_step_failure(&format!("feedback-mode:{rest}")),
            }
            cx.notify();
            return;
        }
        if let Some(text) = rest.strip_prefix("note:") {
            if self.feedback.phase != FeedbackPhase::Active || text.trim().is_empty() {
                crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
            } else if self.feedback.popover.is_some() {
                self.fill_feedback_note(text, window, cx);
                self.save_feedback_note(window, cx);
            } else if self.feedback.capture_pending.is_some() {
                // The capture task (two painted frames) has not landed
                // yet: steps run back-to-back with no frame between them,
                // so finish it synchronously here instead of scripting a
                // `wait:` around capture internals.
                let seq = self.feedback.capture_seq;
                self.finish_note_capture(window, cx, seq);
                if self.feedback.popover.is_some() {
                    self.fill_feedback_note(text, window, cx);
                    self.save_feedback_note(window, cx);
                } else {
                    crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
                }
            } else {
                crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
            }
            cx.notify();
            return;
        }
        if rest == "cancel" {
            self.cancel_feedback_note(window, cx);
            cx.notify();
            return;
        }
        match rest {
            "on" => {
                self.feedback.forced_on = true;
                if self.feedback.phase == FeedbackPhase::Off {
                    self.feedback.toggle(true);
                    self.enter_feedback_active(window, cx);
                } else {
                    cx.notify();
                }
            }
            "pause" => {
                self.feedback.pause();
                cx.notify();
            }
            "resume" => {
                if self.feedback.phase == FeedbackPhase::Paused {
                    self.feedback.resume();
                    self.enter_feedback_active(window, cx);
                } else {
                    cx.notify();
                }
            }
            "off" => {
                self.feedback.forced_on = false;
                self.feedback.setting_off();
                cx.notify();
            }
            "discard" => {
                self.feedback.discard_all();
                cx.notify();
            }
            "done" => self.finish_feedback(window, cx),
            _ => {
                crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
                cx.notify();
            }
        }
    }

    /// Where the app was, for a note's [`ScreenContext`]: the mode, the
    /// current project, the open session and its title, the right pane
    /// (only while open), the shown page's URL (only while a page shows),
    /// the settings section (only while Settings is open), the window
    /// size and scale, and the theme. F3 calls this per note.
    pub(crate) fn feedback_context(&self, window: &mut Window, cx: &gpui::App) -> ScreenContext {
        let mode = match self.mode {
            crate::layout::Mode::Sessions => "sessions".to_owned(),
            crate::layout::Mode::Browse => "browse".to_owned(),
        };
        let project = self.current_project().map(|p| p.name.clone());
        let session_id = self.active.as_ref().map(|view| view.read(cx).session_id.clone());
        let session_title = session_id
            .as_ref()
            .and_then(|id| self.collapsed_head_label(&self.chain_head(id), cx));
        let right_pane = self.layout.right_open.then(|| {
            crate::layout::right_kind(&self.layout).label().to_owned()
        });
        let page_url = self
            .existing_browser(cx)
            .map(|state| state.read(cx).url().to_string())
            .filter(|url| !url.is_empty() && url != "about:blank");
        let settings_section = self
            .settings_page
            .open
            .then(|| self.settings_page.section.clone())
            .filter(|section| !section.is_empty());
        let size = window.viewport_size();
        ScreenContext {
            mode,
            project,
            session_id,
            session_title,
            right_pane,
            page_url,
            settings_section,
            window_w: f32::from(size.width),
            window_h: f32::from(size.height),
            scale: window.scale_factor(),
            theme: if cx.aui().kind.is_dark() { "dark".to_owned() } else { "light".to_owned() },
        }
    }

    /// The pointer moved while Active with nothing pressed: refresh the
    /// hover outline. The mouse-move handler and the `hover:` step verb
    /// both land here. Moves under 2 px skip the hit-test; while a popover
    /// is open, while capturing, or with the button down, hovers stay off.
    pub(crate) fn feedback_hover_at(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.feedback.phase != FeedbackPhase::Active
            || self.feedback.capturing
            || self.feedback.popover.is_some()
            || self.feedback.capture_pending.is_some()
            || self.feedback.press.is_some()
        {
            return;
        }
        if let Some(last) = self.feedback.hover_point {
            if !FeedbackUi::hover_moved(last, point) {
                return;
            }
        }
        let hit = self.feedback.probe.hit(window, point);
        self.feedback.hover_point = Some(point);
        if hit.is_some() {
            self.feedback.hover_since = None;
            self.feedback.hover = hit;
        } else {
            // The tree may still be cold (it warms on a later frame after
            // `activate`): remember when the wait started so the layer's
            // per-frame poll can retry for up to a second.
            if self.feedback.hover_since.is_none() {
                self.feedback.hover_since = Some(Instant::now());
            }
            self.feedback.hover = None;
        }
        cx.notify();
    }

    /// The left button went down while Active: arm a press. With a popover
    /// open (or a capture in flight) the press lands outside it — the
    /// popover consumes its own events — so it cancels the note instead of
    /// starting a new one, and stays Active.
    pub(crate) fn feedback_press_at(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.feedback.phase != FeedbackPhase::Active || self.feedback.capturing {
            return;
        }
        if self.feedback.popover.is_some() || self.feedback.capture_pending.is_some() {
            self.feedback.cancel_note();
            window.focus(&self.feedback.focus, cx);
            cx.notify();
            return;
        }
        self.feedback.press = Some(point);
        self.feedback.drag = None;
        self.feedback.pending_click = None;
        cx.notify();
    }

    /// The pointer moved with the left button down: past 6 px it is a
    /// drag, and the layer draws the live box until release.
    pub(crate) fn feedback_drag_at(&mut self, point: Point<Pixels>, cx: &mut Context<Self>) {
        if self.feedback.phase != FeedbackPhase::Active
            || self.feedback.capturing
            || self.feedback.popover.is_some()
            || self.feedback.capture_pending.is_some()
        {
            return;
        }
        let Some(press) = self.feedback.press else {
            return;
        };
        if FeedbackUi::is_drag(press, point) {
            self.feedback.drag = Some(point);
            self.feedback.hover = None;
            cx.notify();
        }
    }

    /// The left button came up while Active: a ≥ 6×6 box becomes an area
    /// note (named after the centre hit when there is one), anything else
    /// a click — an element note on a hit, a parked retry on a miss while
    /// the tree may still be cold. Both capture first, then open the
    /// popover; the 60×40 area fallback only runs when the retry expires.
    pub(crate) fn feedback_release_at(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.feedback.phase != FeedbackPhase::Active || self.feedback.capturing {
            self.feedback.press = None;
            self.feedback.drag = None;
            return;
        }
        if self.feedback.popover.is_some() || self.feedback.capture_pending.is_some() {
            self.feedback.press = None;
            self.feedback.drag = None;
            return;
        }
        let press = self.feedback.press.take();
        self.feedback.drag.take();
        let Some(start) = press else {
            return;
        };
        if FeedbackUi::is_drag(start, point) {
            if let Some(rect) = FeedbackUi::drag_rect(start, point) {
                let centre = gpui::point(
                    gpui::px(rect.x + rect.w / 2.0),
                    gpui::px(rect.y + rect.h / 2.0),
                );
                let region = self
                    .feedback
                    .probe
                    .hit(window, centre)
                    .and_then(|hit| {
                        if hit.label.is_empty() { None } else { Some(format!("near {}", hit.label)) }
                    });
                self.begin_note_capture(Target::Area { rect, region }, window, cx);
                return;
            }
        }
        match self.feedback.probe.hit(window, point) {
            Some(hit) => self.begin_note_capture(Target::Element(hit), window, cx),
            None => {
                self.feedback.pending_click =
                    Some(PendingClick { point, since: Instant::now() });
                cx.notify();
            }
        }
    }

    /// The layer's per-frame poll (it renders every frame while Active):
    /// retry a parked click's hit-test while the tree may still be cold,
    /// then fall back to the 60×40 area; and retry a cleared hover the
    /// same way so the outline appears when the tree warms.
    pub(crate) fn feedback_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.phase != FeedbackPhase::Active
            || self.feedback.capturing
            || self.feedback.popover.is_some()
            || self.feedback.capture_pending.is_some()
        {
            return;
        }
        if let Some(pending) = self.feedback.pending_click {
            if let Some(hit) = self.feedback.probe.hit(window, pending.point) {
                self.feedback.pending_click = None;
                self.begin_note_capture(Target::Element(hit), window, cx);
                return;
            }
            if pending.since.elapsed() >= FEEDBACK_PROBE_WARM {
                self.feedback.pending_click = None;
                let win = window.viewport_size();
                let target = FeedbackUi::click_target(None, pending.point, win);
                self.begin_note_capture(target, window, cx);
                return;
            }
        }
        if self.feedback.hover.is_none() {
            if let (Some(point), Some(since)) =
                (self.feedback.hover_point, self.feedback.hover_since)
            {
                if since.elapsed() < FEEDBACK_PROBE_WARM {
                    if let Some(hit) = self.feedback.probe.hit(window, point) {
                        self.feedback.hover = Some(hit);
                        self.feedback.hover_since = None;
                        cx.notify();
                    }
                } else {
                    self.feedback.hover_since = None;
                }
            }
        }
    }

    /// Start the capture for `target`: hide every overlay pixel, repaint,
    /// then read the window after two painted frames (a 120 ms timer bounds
    /// the wait). The note field is built now, so opening the popover is
    /// synchronous when the capture lands — or when it fails, in which
    /// case the popover still opens, with no images.
    pub(crate) fn begin_note_capture(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.feedback.hover = None;
        self.feedback.hover_point = None;
        self.feedback.hover_since = None;
        self.feedback.press = None;
        self.feedback.drag = None;
        self.feedback.pending_click = None;
        let field = cx.new(|cx| {
            aui::composer::composer_state_rows("What should change?", 3, 8, window, cx)
        });
        self.feedback.capturing = true;
        self.feedback.capture_seq = self.feedback.capture_seq.wrapping_add(1);
        let seq = self.feedback.capture_seq;
        self.feedback.capture_pending = Some((target, field));
        window.refresh();
        cx.notify();
        let weak = cx.entity().downgrade();
        window.on_next_frame(move |window, _cx| {
            let weak = weak.clone();
            window.on_next_frame(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.finish_note_capture(window, cx, seq));
            });
        });
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FEEDBACK_CAPTURE_TIMEOUT).await;
            let _ = this.update_in(cx, |this, window, cx| this.finish_note_capture(window, cx, seq));
        });
        self.wire_tasks().push(timer);
    }

    /// Finish the capture `seq` started: read the window, cut the outlined
    /// crop, and open the popover (imageless when the render fails). A
    /// no-op for a stale generation or an already-finished capture, so the
    /// frame path and the timer bound never open the popover twice.
    pub(crate) fn finish_note_capture(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        seq: u64,
    ) {
        if seq != self.feedback.capture_seq || self.feedback.capture_pending.is_none() {
            return;
        }
        self.feedback.capture_seq = self.feedback.capture_seq.wrapping_add(1);
        let (target, field) = self.feedback.capture_pending.take().expect("pending checked");
        self.feedback.capturing = false;
        let rect = FeedbackUi::target_rect(&target);
        let scale = window.scale_factor();
        let (crop, full) = match render_window(window) {
            Ok(full) => (Some(crop_with_outline(&full, scale, rect, 24.0)), Some(full)),
            Err(err) => {
                crate::baaz_log!("feedback capture failed: {err:#}");
                (None, None)
            }
        };
        self.feedback.popover = Some(PendingNote { target, crop, full, field });
        if let Some(note) = &self.feedback.popover {
            let focus = note.field.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Save the open popover's text as a note. Empty text keeps the
    /// popover open; afterwards focus returns to the layer.
    pub(crate) fn save_feedback_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.feedback.popover.as_ref() else {
            return;
        };
        let text = note.field.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let note = self.feedback.popover.take().expect("popover checked");
        let context = self.feedback_context(window, cx);
        self.feedback.draft.add(note.target, text, context, note.crop, note.full);
        self.feedback.hover = None;
        window.focus(&self.feedback.focus, cx);
        cx.notify();
    }

    /// Fill the open popover's field without saving, for the `note:` step.
    pub(crate) fn fill_feedback_note(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.feedback.popover.as_ref() else {
            return;
        };
        note.field.update(cx, |field, cx| field.set_value(text.to_owned(), window, cx));
        cx.notify();
    }

    /// Close the open popover (or drop its in-flight capture) without
    /// saving, staying Active. Escape and Cancel land here.
    pub(crate) fn cancel_feedback_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.popover.is_none() && self.feedback.capture_pending.is_none() {
            return;
        }
        self.feedback.cancel_note();
        window.focus(&self.feedback.focus, cx);
        cx.notify();
    }

    /// The popover's title: the element's label (its role when macOS named
    /// nothing) or `Area W × H` for a dragged box.
    pub(crate) fn feedback_note_title(target: &Target) -> String {
        match target {
            Target::Element(hit) if !hit.label.is_empty() => hit.label.clone(),
            Target::Element(hit) => human_role(&hit.role).to_owned(),
            Target::Area { rect, .. } => {
                format!("Area {} × {}", rect.w.round() as i64, rect.h.round() as i64)
            }
        }
    }

    /// The popover's muted subtitle: `<human role> · in <nearest
    /// ancestor> · <right pane or mode>` for an element, the region or the
    /// pane for an area.
    pub(crate) fn feedback_note_subtitle(&self, target: &Target) -> String {
        let pane = if self.layout.right_open {
            crate::layout::right_kind(&self.layout).label().to_owned()
        } else {
            match self.mode {
                crate::layout::Mode::Sessions => "sessions".to_owned(),
                crate::layout::Mode::Browse => "browse".to_owned(),
            }
        };
        match target {
            Target::Element(hit) => {
                let role = human_role(&hit.role);
                match hit.ancestors.first() {
                    Some((_, ancestor)) => format!("{role} · in {ancestor} · {pane}"),
                    None => format!("{role} · {pane}"),
                }
            }
            Target::Area { region, .. } => region.clone().unwrap_or(pane),
        }
    }

    /// The capture layer while Active: nothing when Off or Paused. One
    /// element, and every pointer trap stays on it: hover, press, drag and
    /// release drive the F3 note flow through the shared handlers (the
    /// `hover:`/`click:`/`drag:` step verbs land on those same functions),
    /// while right/middle/scroll stay plain swallows. The hover outline,
    /// the live drag box and the pins render as its decorative children;
    /// while capturing it draws nothing but still occludes input.
    pub(crate) fn render_feedback_layer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.feedback.phase != FeedbackPhase::Active {
            return None;
        }
        self.feedback.expire_discard();
        self.feedback_poll(window, cx);
        let accent = feedback_accent(cx);
        let win = window.viewport_size();
        let win_w = f32::from(win.width);
        let win_h = f32::from(win.height);
        let layer = div()
            .id("feedback-layer")
            .absolute()
            .inset_0()
            .cursor(gpui::CursorStyle::Crosshair)
            .occlude()
            .key_context(FEEDBACK_CONTEXT)
            .track_focus(&self.feedback.focus)
            .when(!self.feedback.capturing, |this| this.border_2().border_color(accent))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.feedback_press_at(event.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseUpEvent, window, cx| {
                    this.feedback_release_at(event.position, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_mouse_up(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(
                cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                    if event.pressed_button == Some(gpui::MouseButton::Left) {
                        this.feedback_drag_at(event.position, cx);
                    } else {
                        this.feedback_hover_at(event.position, window, cx);
                    }
                    cx.stop_propagation();
                }),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .children(self.render_feedback_hover(cx, win_w))
            .children(self.render_feedback_dragbox(cx))
            .children(self.render_feedback_pins(cx, win_w, win_h));
        Some(layer.into_any_element())
    }

    /// The hover outline (3 px outside the hit rect, accent border plus a
    /// 14 % accent fill) and its name chip (`<human role> · <label>`,
    /// accent background, on-accent mono text), above the rect or below it
    /// near the top. Decorative: no role, pointer-transparent.
    fn render_feedback_hover(&self, cx: &mut Context<Self>, win_w: f32) -> Option<AnyElement> {
        if self.feedback.capturing {
            return None;
        }
        let hit = self.feedback.hover.as_ref()?;
        if self.feedback.press.is_some()
            || self.feedback.popover.is_some()
            || self.feedback.capture_pending.is_some()
        {
            return None;
        }
        let accent = feedback_accent(cx);
        let on_accent = feedback_on_accent(cx);
        let fill = gpui::Hsla { a: 0.14, ..accent };
        let chip = if hit.label.is_empty() {
            human_role(&hit.role).to_owned()
        } else {
            format!("{} · {}", human_role(&hit.role), hit.label)
        };
        // Roughly 6.9 px per mono glyph at 11.5 px plus the 7 px side
        // padding each side; clamped into the window.
        let est_w = chip.chars().count() as f32 * 6.9 + 14.0;
        let centre = hit.rect.x + hit.rect.w / 2.0;
        let chip_x = (centre - est_w / 2.0).clamp(4.0, (win_w - est_w - 4.0).max(4.0));
        let chip_y = if hit.rect.y <= 30.0 {
            hit.rect.y + hit.rect.h + 4.0
        } else {
            hit.rect.y - 28.0
        };
        Some(
            div()
                .child(
                    div()
                        .absolute()
                        .left(px(hit.rect.x - 3.0))
                        .top(px(hit.rect.y - 3.0))
                        .w(px(hit.rect.w + 6.0))
                        .h(px(hit.rect.h + 6.0))
                        .rounded(px(6.0))
                        .border_2()
                        .border_color(accent)
                        .bg(fill),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(chip_x))
                        .top(px(chip_y))
                        .rounded(px(6.0))
                        .bg(accent)
                        .px(px(7.0))
                        .py(px(3.0))
                        .mono(11.5)
                        .text_color(on_accent)
                        .whitespace_nowrap()
                        .child(chip),
                )
                .into_any_element(),
        )
    }

    /// The live drag box: a 2 px accent border over the 14 % fill.
    /// Decorative: no role, pointer-transparent.
    fn render_feedback_dragbox(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.feedback.capturing {
            return None;
        }
        let (start, current) = (self.feedback.press?, self.feedback.drag?);
        if self.feedback.popover.is_some() || self.feedback.capture_pending.is_some() {
            return None;
        }
        let (sx, sy) = (f32::from(start.x), f32::from(start.y));
        let (cx_, cy) = (f32::from(current.x), f32::from(current.y));
        let (x0, x1) = if sx <= cx_ { (sx, cx_) } else { (cx_, sx) };
        let (y0, y1) = if sy <= cy { (sy, cy) } else { (cy, sy) };
        let accent = feedback_accent(cx);
        let fill = gpui::Hsla { a: 0.14, ..accent };
        Some(
            div()
                .absolute()
                .left(px(x0))
                .top(px(y0))
                .w(px((x1 - x0).max(1.0)))
                .h(px((y1 - y0).max(1.0)))
                .border_2()
                .border_color(accent)
                .bg(fill)
                .into_any_element(),
        )
    }

    /// One pin per draft note: a 1.5 px accent outline at ~55 % opacity
    /// around its rect and a 22 px numbered badge at the rect's top-right.
    /// Active only, hidden while capturing. Decorative: no role,
    /// pointer-transparent.
    fn render_feedback_pins(
        &self,
        cx: &mut Context<Self>,
        win_w: f32,
        win_h: f32,
    ) -> Option<AnyElement> {
        if self.feedback.capturing || self.feedback.draft.is_empty() {
            return None;
        }
        let accent = feedback_accent(cx);
        let on_accent = feedback_on_accent(cx);
        let line = gpui::Hsla { a: 0.55, ..accent };
        let pins = self
            .feedback
            .draft
            .notes
            .iter()
            .map(|note| {
                let rect = FeedbackUi::target_rect(&note.target);
                let badge_x = (rect.x + rect.w - 11.0).clamp(0.0, (win_w - 22.0).max(0.0));
                let badge_y = (rect.y - 11.0).clamp(0.0, (win_h - 22.0).max(0.0));
                div()
                    .child(
                        div()
                            .absolute()
                            .left(px(rect.x))
                            .top(px(rect.y))
                            .w(px(rect.w.max(1.0)))
                            .h(px(rect.h.max(1.0)))
                            .rounded(px(6.0))
                            .border(px(1.5))
                            .border_color(line),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(px(badge_x))
                            .top(px(badge_y))
                            .size(px(22.0))
                            .rounded_full()
                            .bg(accent)
                            .flex()
                            .items_center()
                            .justify_center()
                            .mono(11.5)
                            .semibold()
                            .text_color(on_accent)
                            .child(note.index.to_string()),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        Some(div().children(pins).into_any_element())
    }

    /// The floating chrome: the bar while Active, the pill while Paused,
    /// nothing while Off — and nothing while capturing, when the window
    /// must read clean.
    pub(crate) fn render_feedback_chrome(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.feedback.capturing {
            return None;
        }
        match self.feedback.phase {
            FeedbackPhase::Off => None,
            FeedbackPhase::Active => Some(self.render_feedback_bar(window, cx)),
            FeedbackPhase::Paused => Some(self.render_feedback_pill(cx)),
        }
    }

    /// The accent Done button both the bar and the pill share: feedback
    /// accent fill with on-accent text (never Baaz's purple primary), at
    /// ~40 % opacity while disabled. A styled div with the aui md metrics
    /// (28 px height, 11 px side padding, small radius), a Button role and
    /// label, hover and disabled states.
    fn render_feedback_done(
        &self,
        id: &'static str,
        label: &str,
        two_line: bool,
        cx: &mut Context<Self>,
        on_click: impl Fn(&mut Window, &mut gpui::App) + 'static,
    ) -> AnyElement {
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let on_accent = feedback_on_accent(cx);
        let hover = gpui::Hsla { l: (accent.l - 0.06).max(0.0), ..accent };
        let sub = gpui::Hsla { a: 0.75, ..on_accent };
        let enabled = self.feedback.done_enabled();
        div()
            .id(id)
            .flex_none()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .px(px(11.0))
            .py(if two_line { px(4.0) } else { px(0.0) })
            .h(if two_line { px(44.0) } else { px(28.0) })
            .rounded(px(scale::R_SM))
            .border_1()
            .border_color(gpui::transparent_black())
            .bg(accent)
            .when(!enabled, |this| this.opacity(0.4))
            .when(enabled, |this| {
                this.hover(|this| this.bg(hover)).cursor_pointer()
            })
            .text_color(on_accent)
            .role(gpui::Role::Button)
            .aria_label("Done — save feedback report")
            .on_click(move |_, window, cx| {
                if enabled {
                    on_click(window, cx);
                }
            })
            .child(
                div()
                    .ui(scale::FS_13)
                    .semibold()
                    .line_height(gpui::relative(1.0))
                    .whitespace_nowrap()
                    .child(label.to_owned()),
            )
            .when(two_line, |this| {
                this.child(
                    div()
                        .ui(scale::FS_11)
                        .line_height(gpui::relative(1.0))
                        .text_color(sub)
                        .whitespace_nowrap()
                        .child("Saves the report and copies its path"),
                )
            })
            .shadow(p.shadow(1))
            .into_any_element()
    }

    /// The note popover: a sibling of the capture layer (never inside its
    /// key context), floating above it and consuming its own mouse events.
    /// 300 px wide, 12 px right of the target (left when there is no room,
    /// clamped below the bar): header, field, Cancel/Save, hints.
    pub(crate) fn render_feedback_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.feedback.phase != FeedbackPhase::Active || self.feedback.capturing {
            return None;
        }
        let (target, field) = match &self.feedback.popover {
            Some(note) => (note.target.clone(), note.field.clone()),
            None => return None,
        };
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let on_accent = feedback_on_accent(cx);
        let hover = gpui::Hsla { l: (accent.l - 0.06).max(0.0), ..accent };
        let win = window.viewport_size();
        let (win_w, win_h) = (f32::from(win.width), f32::from(win.height));
        let rect = FeedbackUi::target_rect(&target);
        let x = if rect.x + rect.w + 12.0 + 300.0 <= win_w - 8.0 {
            rect.x + rect.w + 12.0
        } else {
            rect.x - 300.0 - 12.0
        }
        .clamp(8.0, (win_w - 308.0).max(8.0));
        let y = rect.y.clamp(64.0, (win_h - 280.0).max(64.0));
        let title = Self::feedback_note_title(&target);
        let subtitle = self.feedback_note_subtitle(&target);
        let save = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.save_feedback_note(window, cx);
        });
        let cancel = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.cancel_feedback_note(window, cx);
        });
        let on_save_action = cx.listener(
            |this: &mut Self, _: &crate::app::FeedbackNoteSave, window, cx| {
                this.save_feedback_note(window, cx);
            },
        );
        let on_cancel_action = cx.listener(
            |this: &mut Self, _: &crate::app::FeedbackNoteCancel, window, cx| {
                this.cancel_feedback_note(window, cx);
            },
        );
        let popover = v_flex()
            .id("feedback-note")
            .w(px(300.0))
            .role(gpui::Role::Group)
            .aria_label("Feedback note")
            .bg(p.surface_1)
            .border_1()
            .border_color(p.line_strong)
            .rounded(px(12.0))
            .shadow(p.shadow(2))
            .p(px(12.0))
            .gap(px(9.0))
            .occlude()
            .key_context(FEEDBACK_NOTE_CONTEXT)
            .on_action(move |event, window, cx| on_save_action(event, window, cx))
            .on_action(move |event, window, cx| on_cancel_action(event, window, cx))
            .child(
                v_flex()
                    .gap(px(2.0))
                    .child(div().ui(scale::FS_13).semibold().text_color(p.ink).child(title))
                    .child(
                        div()
                            .mono(scale::FS_12)
                            .text_color(p.ink_3)
                            .child(subtitle),
                    ),
            )
            .child(
                Textarea::new(&field)
                    .aria_label("Feedback note text")
                    .into_any_element(),
            )
            .child(
                h_flex()
                    .gap(px(8.0))
                    .justify_end()
                    .child(
                        button("feedback-note-cancel", "Cancel")
                            .ghost()
                            .accessibility_label("Cancel note")
                            .on_click(move |_, window, cx| cancel(&(), window, cx)),
                    )
                    .child(
                        div()
                            .id("feedback-note-save")
                            .h(px(28.0))
                            .px(px(11.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(scale::R_SM))
                            .border_1()
                            .border_color(gpui::transparent_black())
                            .bg(accent)
                            .hover(|this| this.bg(hover))
                            .cursor_pointer()
                            .ui(scale::FS_13)
                            .medium()
                            .text_color(on_accent)
                            .whitespace_nowrap()
                            .role(gpui::Role::Button)
                            .aria_label("Save note")
                            .on_click(move |_, window, cx| save(&(), window, cx))
                            .child("Save note"),
                    ),
            )
            .child(
                div()
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child("↵ save · ⇧↵ new line · Esc cancel"),
            );
        Some(
            popover_layer(
                div()
                    .absolute()
                    .left(px(x))
                    .top(px(y))
                    .child(popover),
            )
            .into_any_element(),
        )
    }

    /// The Active bar, top-centre: dot · Feedback · N notes · help ·
    /// Pause · Discard · Done (one two-line button: the caption lives
    /// inside it). Above the capture layer, below toasts. Moving over the
    /// bar clears the hover outline beneath it.
    fn render_feedback_bar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.feedback.expire_discard();
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let notes = self.feedback.notes_label();
        let discard_label = self.feedback.discard_label();
        let discard_enabled = self.feedback.discard_enabled();
        let pause = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.pause_feedback(window, cx);
        });
        let discard = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.discard_feedback(window, cx);
        });
        let done = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.finish_feedback(window, cx);
        });
        let done_label = if self.feedback.saving { "Saving…" } else { "Done" };
        let done_button =
            self.render_feedback_done("feedback-done", done_label, true, cx, move |window, cx| {
                done(&(), window, cx);
            });
        let bar = h_flex()
            .id("feedback-bar")
            .occlude()
            .role(gpui::Role::Group)
            .aria_label("Feedback mode")
            .on_mouse_move(cx.listener(|this: &mut Self, _: &gpui::MouseMoveEvent, _, cx| {
                if this.feedback.hover.is_some() {
                    this.feedback.clear_hover();
                    cx.notify();
                }
            }))
            .bg(p.surface_1)
            .border_1()
            .border_color(p.line_strong)
            .rounded(px(10.0))
            .shadow(p.shadow(2))
            .pl(px(12.0))
            .pr(px(6.0))
            .py(px(6.0))
            .gap(px(10.0))
            .items_center()
            .child(div().flex_none().size(px(8.0)).rounded_full().bg(accent))
            .child(
                div()
                    .flex_none()
                    .ui(scale::FS_13)
                    .semibold()
                    .text_color(p.ink)
                    .child("Feedback"),
            )
            .child(
                div()
                    .flex_none()
                    .mono(scale::FS_12)
                    .text_color(p.ink_3)
                    .child(notes),
            )
            .child(
                div()
                    .flex_none()
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child("Click an element or drag an area"),
            )
            .child(
                button("feedback-pause", "Pause")
                    .accessibility_label("Pause feedback")
                    .on_click(move |_, window, cx| pause(&(), window, cx)),
            )
            .child(
                button("feedback-discard", discard_label)
                    .ghost()
                    .disabled(!discard_enabled)
                    .accessibility_label("Discard feedback notes")
                    .on_click(move |_, window, cx| discard(&(), window, cx)),
            )
            .child(done_button);
        popover_layer(
            div()
                .absolute()
                .top(px(8.0))
                .left(px(0.0))
                .right(px(0.0))
                .flex()
                .justify_center()
                .child(bar),
        )
        .into_any_element()
    }

    /// The Paused pill, bottom-right: dot · N notes · Resume · Done.
    /// It never blocks the app. Shown even with 0 notes (Resume live,
    /// Done disabled). Done is the same accent button as the bar's, one
    /// line.
    fn render_feedback_pill(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let notes = self.feedback.notes_label();
        let resume = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.resume_feedback(window, cx);
        });
        let done = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.finish_feedback(window, cx);
        });
        let done_label = if self.feedback.saving { "Saving…" } else { "Done" };
        let done_button =
            self.render_feedback_done("feedback-done", done_label, false, cx, move |window, cx| {
                done(&(), window, cx);
            });
        let pill = h_flex()
            .id("feedback-pill")
            .occlude()
            .role(gpui::Role::Group)
            .aria_label("Feedback paused")
            .bg(p.surface_1)
            .border_1()
            .border_color(p.line_strong)
            .rounded(px(10.0))
            .shadow(p.shadow(2))
            .px(px(12.0))
            .py(px(6.0))
            .gap(px(10.0))
            .items_center()
            .child(div().flex_none().size(px(8.0)).rounded_full().bg(accent))
            .child(
                div()
                    .flex_none()
                    .mono(scale::FS_12)
                    .text_color(p.ink_3)
                    .child(notes),
            )
            .child(
                button("feedback-resume", "Resume")
                    .accessibility_label("Resume feedback")
                    .on_click(move |_, window, cx| resume(&(), window, cx)),
            )
            .child(done_button);
        popover_layer(
            div()
                .absolute()
                .bottom(px(12.0))
                .right(px(12.0))
                .child(pill),
        )
        .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FakeProbe, Rect, Target};

    /// A live shell on the test app: the transition tests need a real
    /// focus handle, which needs the app.
    fn live_shell(cx: &mut gpui::App) -> FeedbackUi {
        FeedbackUi::new(Box::new(FakeProbe { hits: Vec::new() }), cx)
    }

    fn note_on(shell: &mut FeedbackUi, text: &str) {
        shell.draft.add(
            Target::Area { rect: Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 }, region: None },
            text.to_owned(),
            crate::feedback::test_context(),
            None,
            None,
        );
    }

    /// The toggle table: setting off stays Off; Off → Active (fresh draft
    /// when empty, kept draft otherwise); Active → Paused; Paused → Active.
    #[gpui::test]
    fn toggle_moves_through_off_active_paused(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            assert_eq!(shell.toggle(false), ToggleOutcome::NeedsSetting);
            assert_eq!(shell.phase, FeedbackPhase::Off);

            assert_eq!(shell.toggle(true), ToggleOutcome::EnteredActive);
            assert_eq!(shell.phase, FeedbackPhase::Active);
            assert_eq!(shell.toggle(true), ToggleOutcome::Paused);
            assert_eq!(shell.phase, FeedbackPhase::Paused);
            assert_eq!(shell.toggle(true), ToggleOutcome::Resumed);
            assert_eq!(shell.phase, FeedbackPhase::Active);

            // A kept draft survives the Off round-trip: entering Active
            // with notes keeps them instead of starting over.
            note_on(&mut shell, "kept");
            shell.pause();
            shell.setting_off();
            assert_eq!(shell.phase, FeedbackPhase::Off);
            assert_eq!(shell.note_count(), 1);
            assert_eq!(shell.toggle(true), ToggleOutcome::EnteredActive);
            assert_eq!(shell.note_count(), 1, "the kept draft resumes");

            // An empty draft starts fresh.
            shell.discard_all();
            shell.setting_off();
            let before = shell.draft.started;
            assert_eq!(shell.toggle(true), ToggleOutcome::EnteredActive);
            assert!(shell.draft.is_empty());
            assert!(shell.draft.started >= before);
        });
    }

    /// Escape and Pause only ever leave Active for Paused.
    #[gpui::test]
    fn escape_and_pause_only_leave_active(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            shell.pause();
            assert_eq!(shell.phase, FeedbackPhase::Off, "pause while Off is a no-op");
            shell.toggle(true);
            shell.pause();
            assert_eq!(shell.phase, FeedbackPhase::Paused);
            shell.pause();
            assert_eq!(shell.phase, FeedbackPhase::Paused, "pause while Paused is a no-op");
            shell.resume();
            assert_eq!(shell.phase, FeedbackPhase::Active);
            shell.resume();
            assert_eq!(shell.phase, FeedbackPhase::Active, "resume while Active is a no-op");
        });
    }

    /// Discard: disabled with 0 notes; first click arms, second clears
    /// staying in phase; a stale arm re-arms instead of clearing.
    #[gpui::test]
    fn discard_arms_then_clears(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            shell.toggle(true);
            assert_eq!(shell.discard_press(), DiscardOutcome::Disabled);
            assert!(!shell.discard_enabled());
            assert_eq!(shell.discard_label(), "Discard");

            note_on(&mut shell, "one");
            note_on(&mut shell, "two");
            assert!(shell.discard_enabled());
            assert_eq!(shell.discard_press(), DiscardOutcome::Armed);
            assert_eq!(shell.discard_label(), "Discard 2 notes?");
            assert_eq!(shell.discard_press(), DiscardOutcome::Cleared);
            assert_eq!(shell.note_count(), 0);
            assert_eq!(shell.phase, FeedbackPhase::Active, "clearing stays in phase");
            assert_eq!(shell.discard_label(), "Discard");

            // A stale arm reads as a first click again.
            note_on(&mut shell, "three");
            assert_eq!(shell.discard_press(), DiscardOutcome::Armed);
            shell.discard_armed_at = Some(Instant::now() - DISCARD_ARM - Duration::from_secs(1));
            assert!(!shell.discard_armed_live());
            assert_eq!(shell.discard_label(), "Discard", "a stale arm falls back");
            assert_eq!(shell.discard_press(), DiscardOutcome::Armed, "stale arms re-arm");
            assert_eq!(shell.note_count(), 1, "nothing cleared on the re-arm");
        });
    }

    /// Done gates on phase and notes; applying it goes Off cleared.
    #[gpui::test]
    fn done_gates_on_phase_and_notes(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            assert!(!shell.done_enabled(), "Off never saves");
            shell.toggle(true);
            assert!(!shell.done_enabled(), "Active with 0 notes never saves");
            note_on(&mut shell, "one");
            assert!(shell.done_enabled());
            shell.pause();
            assert!(shell.done_enabled(), "Paused saves too");
            shell.done_applied();
            assert_eq!(shell.phase, FeedbackPhase::Off);
            assert!(shell.draft.is_empty());
            assert!(!shell.done_enabled());
        });
    }

    /// Turning the setting off parks Off but keeps the draft.
    #[gpui::test]
    fn setting_off_parks_but_keeps_the_draft(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            shell.setting_off();
            assert_eq!(shell.phase, FeedbackPhase::Off);
            shell.toggle(true);
            note_on(&mut shell, "kept");
            shell.setting_off();
            assert_eq!(shell.phase, FeedbackPhase::Off);
            assert_eq!(shell.note_count(), 1);
        });
    }

    /// Holds [`crate::store::test_env_lock`] while the test points
    /// `BAAZ_STATE_DIR` at a fresh temp dir, restoring after: the
    /// variable is process-global, so two such tests at once would read
    /// each other's state.
    fn hermetic_state(
        name: &str,
    ) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-feedback-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("feedback test state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    fn restore_state(state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf)) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    fn test_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
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
            terminal_socket_dir: None,
        }
    }

    fn hermetic_harness(
        vc: &mut gpui::VisualTestContext,
        dir: &std::path::Path,
    ) -> gpui::Entity<crate::app::Harness> {
        vc.update(|window, cx| {
            use gpui::AppContext as _;
            cx.new(|cx| {
                crate::app::Harness::new(
                    test_args(dir),
                    crate::shot::CaptureToken::default(),
                    window,
                    cx,
                )
            })
        })
    }

    /// Done on a hermetic window: the setting on, toggle Active, one
    /// Area note added directly (no images), Done writes the report dir
    /// plus `latest` under `BAAZ_STATE_DIR`, and the shell parks Off
    /// with a cleared draft.
    #[gpui::test]
    fn done_writes_a_report_and_parks_off(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("done");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = hermetic_harness(vc, &state.2);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.layout.feedback_mode = true;
                h.toggle_feedback(window, cx);
            })
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Active),
            "the toggle enters Active with the setting on"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 8.0, y: 8.0, w: 64.0, h: 32.0 },
                        region: None,
                    },
                    "the bar overlaps the header".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.finish_feedback(window, cx)));
        vc.run_until_parked();
        let report_root = crate::feedback::feedback_root();
        let latest_path = report_root.join("latest");
        let latest = std::fs::read_to_string(&latest_path).expect("Done writes latest");
        let dir = std::path::PathBuf::from(latest.trim());
        assert!(dir.is_dir(), "the latest path is a report dir: {dir:?}");
        assert!(dir.join("report.md").is_file(), "the report renders Markdown");
        assert!(dir.join("report.json").is_file(), "the report renders JSON");
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Off),
            "Done parks Off"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.draft.is_empty()),
            "Done clears the draft"
        );
        restore_state(state);
    }

    /// The context builder on a test window: every field filled from the
    /// live sources, optionals empty when nothing is open.
    #[gpui::test]
    fn context_builder_reads_the_window(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("context");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = hermetic_harness(vc, &state.2);
        let ctx = vc.update(|window, cx| baaz.read(cx).feedback_context(window, cx));
        assert_eq!(ctx.mode, "sessions");
        assert_eq!(ctx.theme, "dark");
        assert!(ctx.window_w > 0.0 && ctx.window_h > 0.0, "the window has a size");
        assert!(ctx.scale > 0.0, "the window has a scale");
        assert!(ctx.right_pane.is_none(), "the pane starts closed");
        assert!(ctx.page_url.is_none(), "no page shows");
        assert!(ctx.settings_section.is_none(), "settings starts closed");
        assert!(ctx.session_id.is_none(), "no session is open");
        assert!(ctx.project.is_some(), "the explicit workspace is current");
        restore_state(state);
    }

    /// A rendered window harness with keys bound, for dispatch tests: an
    /// entity that is never rendered has no element tree, so keystrokes
    /// never dispatch through it.
    fn window_harness<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (gpui::Entity<crate::app::Harness>, &'a mut gpui::VisualTestContext) {
        cx.update(crate::app::bind_keys);
        cx.add_window_view(|window, cx| {
            crate::app::Harness::new(
                test_args(dir),
                crate::shot::CaptureToken::default(),
                window,
                cx,
            )
        })
    }

    fn enter_active(
        baaz: &gpui::Entity<crate::app::Harness>,
        vc: &mut gpui::VisualTestContext,
    ) {
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.layout.feedback_mode = true;
                h.toggle_feedback(window, cx);
            })
        });
    }

    /// The cover predicate follows the phase: Active covers, Paused leaves
    /// pages live (so `browser_visible` stays true under the pill).
    #[gpui::test]
    fn covers_app_is_active_only(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            assert!(!shell.covers_app(), "Off covers nothing");
            shell.toggle(true);
            assert!(shell.covers_app(), "Active covers the app");
            shell.pause();
            assert!(!shell.covers_app(), "Paused leaves pages live");
        });
    }

    /// The run-only flag counts as the setting being on, without writing
    /// it: `toggle(false)` still enters Active once forced.
    #[gpui::test]
    fn forced_on_counts_as_the_setting(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            assert_eq!(shell.toggle(false), ToggleOutcome::NeedsSetting);
            shell.forced_on = true;
            assert_eq!(shell.toggle(false), ToggleOutcome::EnteredActive);
            assert_eq!(shell.phase, FeedbackPhase::Active);
        });
    }

    /// While saving, Done is disabled and pause/resume/discard are no-ops.
    #[gpui::test]
    fn saving_freezes_the_shell(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            shell.toggle(true);
            note_on(&mut shell, "one");
            shell.saving = true;
            assert!(!shell.done_enabled(), "no second Done while saving");
            shell.pause();
            assert_eq!(shell.phase, FeedbackPhase::Active, "pause waits out the save");
            shell.resume();
            assert_eq!(shell.phase, FeedbackPhase::Active, "resume waits out the save");
            assert_eq!(shell.discard_press(), DiscardOutcome::Disabled);
            shell.discard_all();
            assert_eq!(shell.note_count(), 1, "the in-flight draft survives");
        });
    }

    /// The layer element exists only while Active; the chrome exists while
    /// Active (bar) or Paused (pill), never while Off.
    #[gpui::test]
    fn layer_and_chrome_follow_the_phase(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("layer-phase");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        let layer = |vc: &mut gpui::VisualTestContext| {
            vc.update(|window, cx| baaz.update(cx, |h, cx| h.render_feedback_layer(window, cx).is_some()))
        };
        let chrome = |vc: &mut gpui::VisualTestContext| {
            vc.update(|window, cx| {
                baaz.update(cx, |h, cx| h.render_feedback_chrome(window, cx).is_some())
            })
        };
        assert!(!layer(vc), "Off renders no layer");
        assert!(!chrome(vc), "Off renders no chrome");
        enter_active(&baaz, vc);
        assert!(layer(vc), "Active renders the layer");
        assert!(chrome(vc), "Active renders the bar");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.pause_feedback(window, cx)));
        assert!(!layer(vc), "Paused renders no layer");
        assert!(chrome(vc), "Paused renders the pill");
        restore_state(state);
    }

    /// While Active, app keys die on the layer: ⌘N opens no session and
    /// Tab never leaves the layer's focus — but ⌃⌥⌘F still pauses.
    #[gpui::test]
    fn active_layer_swallows_app_keys_but_keeps_the_toggle(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("keys-swallow");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Active));
        let sessions_before = vc.update(|_, cx| baaz.read(cx).sessions.len());
        vc.simulate_keystrokes("cmd-n");
        vc.simulate_keystrokes("tab");
        vc.update(|window, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.sessions.len(), sessions_before, "⌘N opened a session under the layer");
            assert!(harness.active.is_none(), "⌘N opened a draft under the layer");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
            assert!(
                harness.feedback.focus.is_focused(window),
                "Tab moved focus off the layer"
            );
        });
        vc.simulate_keystrokes("ctrl-alt-cmd-f");
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Paused),
            "⌃⌥⌘F still pauses from the layer"
        );
        restore_state(state);
    }

    /// Entering Active closes the palette and the dialog: they would
    /// otherwise sit frozen under the capture layer.
    #[gpui::test]
    fn entering_active_closes_palette_and_dialog(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("enter-closes");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.open_palette(crate::overlays::PaletteKind::Commands, cx);
                h.set_dialog(cx, crate::app::Harness::quit_terminal_dialog("sleep 10", 0));
            })
        });
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette.is_some()));
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).dialog.is_some()));
        enter_active(&baaz, vc);
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette.is_none()));
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).dialog.is_none()));
        restore_state(state);
    }

    /// Two Done calls in a row write exactly one report folder; the toast
    /// carries the pre-write note count and the clipboard the report dir.
    #[gpui::test]
    fn double_done_writes_one_report_with_count_and_clipboard(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("double-done");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 1.0, y: 1.0, w: 8.0, h: 8.0 },
                        region: None,
                    },
                    "first".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 2.0, y: 2.0, w: 8.0, h: 8.0 },
                        region: None,
                    },
                    "second".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.finish_feedback(window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.saving),
            "the first Done raises the guard"
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.finish_feedback(window, cx)));
        vc.run_until_parked();
        let root = crate::feedback::feedback_root();
        let dirs = std::fs::read_dir(&root)
            .expect("report root reads")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .count();
        assert_eq!(dirs, 1, "two Done calls wrote {dirs} report folders");
        let latest = std::fs::read_to_string(root.join("latest")).expect("Done writes latest");
        let (toast, clip) = vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let toast = harness.overlays.read(cx).toasts.last().cloned().expect("a toast lands");
            let clip = cx.read_from_clipboard().and_then(|item| item.text());
            (toast, clip)
        });
        assert_eq!(toast.title.to_string(), "Feedback saved · 2 notes");
        assert_eq!(clip.as_deref(), Some(latest.trim()), "the clipboard holds the report dir");
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Off));
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.draft.is_empty()));
        assert!(!vc.update(|_, cx| baaz.read(cx).feedback.saving));
        restore_state(state);
    }

    /// A failed write keeps the draft and the phase, lifts the guard, and
    /// toasts the error. The report root itself is a FILE (planted where
    /// `feedback_root()` lands), so `create_dir_all` cannot succeed — while
    /// the state dir stays a real directory, so parallel tests reading
    /// their own roots never see this one's failure.
    #[gpui::test]
    fn failed_done_keeps_draft_and_phase(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("done-error");
        let blocker = state.2.join("feedback");
        std::fs::write(&blocker, b"not a dir").expect("blocker file writes");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 3.0, y: 3.0, w: 8.0, h: 8.0 },
                        region: None,
                    },
                    "kept".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.finish_feedback(window, cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active, "the phase survives");
            assert_eq!(harness.feedback.note_count(), 1, "the draft moves back");
            assert!(!harness.feedback.saving, "the guard lifts");
            let toast = harness.overlays.read(cx).toasts.last().cloned().expect("an error toasts");
            assert_eq!(toast.title.to_string(), "Couldn't save feedback");
        });
        restore_state(state);
    }

    /// The click path keeps Escape alive: Discard (button handler) leaves
    /// the layer focused, so `escape` still pauses.
    #[gpui::test]
    fn discard_click_keeps_escape_working(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("discard-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 4.0, y: 4.0, w: 8.0, h: 8.0 },
                        region: None,
                    },
                    "arming".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
            })
        });
        // The bar button's own handler, exactly as the click calls it.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.discard_feedback(window, cx)));
        vc.simulate_keystrokes("escape");
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Paused),
            "escape pauses after the Discard click"
        );
        restore_state(state);
    }

    /// The window verb never touches the persisted setting: `on` parks the
    /// run-only flag, `off` clears it, `layout.feedback_mode` never moves —
    /// and an unknown sub records a step failure instead of forwarding.
    #[gpui::test]
    fn feedback_mode_steps_never_touch_the_layout_setting(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("mode-steps");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        assert!(!vc.update(|_, cx| baaz.read(cx).layout.feedback_mode));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("on", window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
            assert!(harness.feedback.forced_on, "the run-only flag parks");
            assert!(!harness.layout.feedback_mode, "the persisted setting never moves");
        });
        let failures_before = crate::steps::step_failures();
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("bogus", window, cx)));
        assert_eq!(crate::steps::step_failures(), failures_before + 1);
        assert!(
            crate::steps::step_failure_names().iter().any(|name| name == "feedback-mode:bogus"),
            "the unknown sub fails under its own verb"
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("off", window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Off);
            assert!(!harness.feedback.forced_on, "off clears the run-only flag");
            assert!(!harness.layout.feedback_mode, "off leaves the persisted setting alone");
        });
        restore_state(state);
    }

    /// Arming Discard twice leaves a single pending disarm task: the slot
    /// replaces, never queues.
    #[gpui::test]
    fn rearming_discard_keeps_a_single_task(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("discard-task");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 5.0, y: 5.0, w: 8.0, h: 8.0 },
                        region: None,
                    },
                    "arming".to_owned(),
                    crate::feedback::test_context(),
                    None,
                    None,
                );
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.discard_feedback(window, cx)));
        // A stale arm reads as a first click again, so the second press
        // re-arms instead of clearing.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.discard_armed_at =
                    Some(Instant::now() - DISCARD_ARM - Duration::from_secs(1));
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.discard_feedback(window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.discard_task.is_some()),
            "one disarm task is pending after two arms"
        );
        restore_state(state);
    }
}
