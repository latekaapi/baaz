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

use std::collections::VecDeque;
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
/// Hover hit-tests run at most this often: every ≥ 2 px move past this
/// interval re-tests, anything sooner keeps the last outline.
pub(crate) const FEEDBACK_HOVER_THROTTLE: Duration = Duration::from_millis(30);
/// A queued `note:`/`click:`/`drag:` step waits this long for the popover
/// (or the idle shell) before it fails loudly instead of racing the
/// async capture.
pub(crate) const FEEDBACK_STEP_WAIT: Duration = Duration::from_secs(2);
/// The top strip the Active bar floats in: the hover chip never parks its
/// top edge inside it.
pub(crate) const FEEDBACK_BAR_ZONE_H: f32 = 64.0;
/// The note popover's fixed width, matching its render.
pub(crate) const FEEDBACK_POPOVER_W: f32 = 300.0;
/// The note field's inner width: the 300 px popover minus its 12 px side
/// padding each side, the textarea's own insets and border — what one
/// visual row of text actually fills before it wraps.
pub(crate) const FEEDBACK_FIELD_INNER_W: f32 = 252.0;
/// The average glyph width the wrap estimate divides by: between the chip's
/// 6.9 px mono advance and a proportional UI face at this size.
pub(crate) const FEEDBACK_GLYPH_W: f32 = 7.0;
/// How long the Done confirmation lingers in the header slot.
pub(crate) const FEEDBACK_SAVED_FLASH: Duration = Duration::from_secs(4);

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
    /// Where the app was when the note was captured (taken in
    /// [`crate::app::Harness::begin_note_capture`], never refreshed): a
    /// Pause or mode switch before Save must not rewrite it.
    pub context: ScreenContext,
}

/// A finished Done write's brief header confirmation, while fresh.
pub struct SavedFlash {
    /// When the write landed, bounding the ~4 s show.
    pub at: Instant,
    /// How many notes the report held, for `Saved · N notes`.
    pub notes: usize,
    /// The report dir path the toast also carries (and the clipboard holds).
    pub path: String,
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

/// A step-driven pointing action waiting behind an in-flight capture: a
/// `click:`/`drag:` issued while a capture is pending queues here instead
/// of cancelling it, and a `note:` waits here for its popover. Each item
/// carries its full step name so the pump can fail it loudly on expiry.
#[derive(Clone, Debug)]
pub(crate) enum QueuedStep {
    /// A queued `click:x,y`.
    Click {
        /// The full `feedback-mode:click:…` step name, for the failure log.
        step: String,
        /// Where the owner clicked, in window-logical pixels.
        point: Point<Pixels>,
    },
    /// A queued `drag:x1,y1,x2,y2`.
    Drag {
        /// The full `feedback-mode:drag:…` step name, for the failure log.
        step: String,
        /// The drag's start corner, in window-logical pixels.
        start: Point<Pixels>,
        /// The drag's end corner, in window-logical pixels.
        end: Point<Pixels>,
    },
    /// A queued `note:<text>`.
    Note {
        /// The full `feedback-mode:note:…` step name, for the failure log.
        step: String,
        /// The owner's words, filled into the popover once it opens.
        text: String,
    },
}

impl QueuedStep {
    /// The full step name, for the failure log on expiry.
    fn step(&self) -> &str {
        match self {
            QueuedStep::Click { step, .. }
            | QueuedStep::Drag { step, .. }
            | QueuedStep::Note { step, .. } => step,
        }
    }
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
    /// When the last hover hit-test ran: moves inside
    /// [`FEEDBACK_HOVER_THROTTLE`] of it skip the next one, so hover costs
    /// at most one hit-test per ~30 ms.
    pub hover_tested_at: Option<Instant>,
    /// The latest pointer position seen while throttled: the trailing edge
    /// the per-frame poll hit-tests once the window above has passed, so
    /// the outline always settles on the last position after the pointer
    /// stops.
    pub hover_dirty: Option<Point<Pixels>>,
    /// The "Feedback mode is off" toast's id, while it is up: entering
    /// Active dismisses it so it never goes stale.
    pub off_toast: Option<String>,
    /// A finished Done write's brief header confirmation, while fresh.
    pub saved_flash: Option<SavedFlash>,
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
    /// built so opening the popover is synchronous when it does — plus
    /// the screen context taken the moment the note began, so saving it
    /// later never re-reads a screen the owner has since left.
    pub capture_pending: Option<(Target, Entity<TextareaState>, ScreenContext)>,
    /// The capture's 120 ms timer bound. One slot, never a queue:
    /// starting a capture replaces the previous timer (dropping cancels
    /// it), so timers can never stack on `wire_tasks`.
    pub capture_task: Option<gpui::Task<()>>,
    /// Step-driven pointing actions waiting behind an in-flight capture,
    /// oldest first, each with the moment it queued (bounding the
    /// [`FEEDBACK_STEP_WAIT`] wait). Drained by the frame loop — see
    /// [`FeedbackUi::step_pump_tick`] — so no task is ever spawned for
    /// them.
    pub step_queue: VecDeque<(QueuedStep, Instant)>,
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
            hover_tested_at: None,
            hover_dirty: None,
            off_toast: None,
            saved_flash: None,
            press: None,
            drag: None,
            capturing: false,
            capture_seq: 0,
            capture_pending: None,
            capture_task: None,
            step_queue: VecDeque::new(),
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
        self.capture_task = None;
        self.capture_seq = self.capture_seq.wrapping_add(1);
        self.capturing = false;
        self.pending_click = None;
    }

    /// Drop everything transient except the open popover: hover, press,
    /// drag, the parked click and any in-flight capture (its frame
    /// callbacks go stale). Pausing and toggling park through here so a
    /// note-in-progress survives them; cancelling goes through
    /// [`Self::cancel_note`] instead.
    pub(crate) fn clear_transient(&mut self) {
        self.hover = None;
        self.hover_point = None;
        self.hover_since = None;
        self.hover_dirty = None;
        self.press = None;
        self.drag = None;
        self.pending_click = None;
        self.capture_pending = None;
        self.capture_task = None;
        self.capture_seq = self.capture_seq.wrapping_add(1);
        self.capturing = false;
    }

    /// Clear the hover outline, for the bar's mouse-move catcher.
    pub fn clear_hover(&mut self) {
        self.hover = None;
        self.hover_point = None;
        self.hover_since = None;
        self.hover_dirty = None;
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
    /// while a write is in flight. An open popover holding non-empty text
    /// counts: Done files it first, so the button the bar and the pill
    /// render must already read enabled for it.
    pub fn done_enabled(&self, cx: &gpui::App) -> bool {
        if self.saving || self.phase == FeedbackPhase::Off {
            return false;
        }
        if !self.draft.is_empty() {
            return true;
        }
        self.popover.as_ref().is_some_and(|note| !note.field.read(cx).value().trim().is_empty())
    }

    /// While Active, menu-bar dispatches must not run app actions: the F2b
    /// `NoAction` rows stop gpui keybindings, but a native menu equivalent
    /// dispatches its action straight into the tree. Every menu-bar action
    /// handler early-returns on this except `ToggleFeedback`, `QuitApp`
    /// and the feedback/note actions.
    pub(crate) fn blocks_chrome_action(&self) -> bool {
        self.phase == FeedbackPhase::Active
    }

    /// ⌃⌥⌘F / the menu item: setting off stays Off; Off → Active (a fresh
    /// draft when empty, the kept one otherwise); Active → Paused;
    /// Paused → Active. Pure — the caller runs the side effects the
    /// outcome names (toasts, probe activation, focus). The run-only
    /// [`Self::forced_on`] counts as the setting being on. While
    /// [`Self::saving`] the Harness guards the toggle outright (there is
    /// no stay-put outcome to return here). Moving between Active and
    /// Paused keeps the open popover (a note-in-progress survives the
    /// round-trip); only the transient pointer and capture state parks.
    pub fn toggle(&mut self, setting_on: bool) -> ToggleOutcome {
        if !(setting_on || self.forced_on) {
            return ToggleOutcome::NeedsSetting;
        }
        match self.phase {
            FeedbackPhase::Off => {
                if self.draft.is_empty() {
                    self.draft = Draft::new();
                }
                self.clear_transient();
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::EnteredActive
            }
            FeedbackPhase::Active => {
                self.clear_transient();
                self.disarm_discard();
                self.phase = FeedbackPhase::Paused;
                ToggleOutcome::Paused
            }
            FeedbackPhase::Paused => {
                self.clear_transient();
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::Resumed
            }
        }
    }

    /// Escape, the Pause button: Active → Paused, anywhere else a no-op —
    /// and always a no-op while [`Self::saving`]. The open popover (with
    /// its typed text) survives: Resume brings it back. Queued steps die
    /// with it — their popovers will never open — failing loudly under
    /// their own step names like the pump's expiry does.
    pub fn pause(&mut self) {
        if self.phase == FeedbackPhase::Active && !self.saving {
            self.clear_transient();
            while let Some((queued, _)) = self.step_queue.pop_front() {
                crate::steps::record_step_failure(queued.step());
            }
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
    /// Queued steps die with it — their popovers will never open.
    pub fn setting_off(&mut self) {
        self.forced_on = false;
        if self.phase != FeedbackPhase::Off {
            self.cancel_note();
            self.step_queue.clear();
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
    /// guard lifted. Queued steps die with it.
    pub fn done_applied(&mut self) {
        self.cancel_note();
        self.step_queue.clear();
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

    /// The target for a hit at `point`: the element itself — unless its
    /// rect covers more than 40 % of the window, when naming the whole
    /// area (a click on a web page hit-tested as `Browse (Group)`,
    /// 1188×856) is useless. Then it becomes a 60×40 area around the
    /// point with `region = near <label>` — or `"web page"` when the hit
    /// is the page stand-in itself (an `AXGroup` or an unlabelled hit).
    /// Pure, so tests drive the edges directly.
    pub(crate) fn note_target_for_hit(
        hit: ElementHit,
        point: Point<Pixels>,
        win: gpui::Size<Pixels>,
    ) -> Target {
        let area = f32::from(win.width) * f32::from(win.height);
        if area > 0.0 && hit.rect.w * hit.rect.h > 0.4 * area {
            let Target::Area { rect, .. } = FeedbackUi::click_target(None, point, win) else {
                unreachable!("click_target(None, ..) is always an area")
            };
            let region = if hit.role == "AXGroup" || hit.label.is_empty() {
                "web page".to_owned()
            } else {
                format!("near {}", hit.label)
            };
            Target::Area { rect, region: Some(region) }
        } else {
            Target::Element(hit)
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

    /// Where the hover name chip sits: centred on the target, above it by
    /// default, below it near the top — and never with its top edge inside
    /// the bar zone (then below the element too). Clamped horizontally and
    /// vertically into the window. When the element is taller than ~60 % of
    /// the window (a page-sized hit whose bottom edge is unreachable), the
    /// chip pins inside its top-left instead. Pure, so tests drive the
    /// edges directly.
    pub(crate) fn feedback_chip_pos(
        rect: Rect,
        est_w: f32,
        chip_h: f32,
        win_w: f32,
        win_h: f32,
    ) -> (f32, f32) {
        if rect.h >= win_h * 0.6 {
            let x = (rect.x + 8.0).clamp(8.0, (win_w - est_w - 8.0).max(8.0));
            return (x, rect.y + 8.0);
        }
        let centre = rect.x + rect.w / 2.0;
        let x = (centre - est_w / 2.0).clamp(4.0, (win_w - est_w - 4.0).max(4.0));
        let mut y = if rect.y <= 30.0 { rect.y + rect.h + 4.0 } else { rect.y - 28.0 };
        if y < FEEDBACK_BAR_ZONE_H {
            y = rect.y + rect.h + 4.0;
        }
        (x, y.clamp(4.0, (win_h - chip_h - 4.0).max(4.0)))
    }

    /// The note popover's estimated full height: padding plus the header,
    /// the textarea at its current row count (the field grows 3–8 rows),
    /// the buttons and the hint. An estimate — the render cannot measure
    /// before layout — but conservative enough that Save stays on screen.
    pub(crate) fn feedback_popover_height(rows: usize) -> f32 {
        let rows = rows.clamp(3, 8) as f32;
        12.0 + 20.0 + 2.0 + 16.0 + 9.0 + rows * 20.0 + 9.0 + 28.0 + 9.0 + 16.0 + 12.0
    }

    /// The textarea's real visual row count for `text`: logical lines add
    /// their wrapped rows (a long single line grows the field), an empty
    /// line still takes one row, and a trailing newline counts — so
    /// `"a\n"` is two rows, not one. `TextareaState` exposes no row or line
    /// layout, so wrapping is estimated from the field's inner width
    /// ([`FEEDBACK_FIELD_INNER_W`]) over an average glyph width
    /// ([`FEEDBACK_GLYPH_W`]). Pure, so tests drive the edges directly.
    pub(crate) fn feedback_visual_rows(text: &str) -> usize {
        let per_row = (FEEDBACK_FIELD_INNER_W / FEEDBACK_GLYPH_W).floor().max(1.0) as usize;
        text.split('\n')
            .map(|line| {
                let chars = line.chars().count();
                ((chars + per_row - 1) / per_row).max(1)
            })
            .sum::<usize>()
            .max(1)
    }

    /// Where the note popover sits: right of the target (left when there
    /// is no room), top-aligned with it — flipped above the target when
    /// there is no room below, so the whole popover including Save stays
    /// on screen and below the bar. Pure, so tests drive the edges
    /// directly.
    pub(crate) fn feedback_popover_pos(
        rect: Rect,
        pop_h: f32,
        win_w: f32,
        win_h: f32,
    ) -> (f32, f32) {
        let x = if rect.x + rect.w + 12.0 + FEEDBACK_POPOVER_W <= win_w - 8.0 {
            rect.x + rect.w + 12.0
        } else {
            rect.x - FEEDBACK_POPOVER_W - 12.0
        }
        .clamp(8.0, (win_w - FEEDBACK_POPOVER_W - 8.0).max(8.0));
        let mut y = rect.y;
        if y + pop_h > win_h - 8.0 {
            y = rect.y - pop_h - 8.0;
        }
        let hi = (win_h - 8.0 - pop_h).max(FEEDBACK_BAR_ZONE_H);
        (x, y.clamp(FEEDBACK_BAR_ZONE_H, hi))
    }

    /// Whether a note's stored context names the current screen: same
    /// mode, same session id (Sessions) or same page URL (Browse), same
    /// right-pane kind, same Settings section. Pure, so tests drive the
    /// matrix directly.
    pub(crate) fn pin_context_matches(current: &ScreenContext, note: &ScreenContext) -> bool {
        current.mode == note.mode
            && current.session_id == note.session_id
            && current.page_url == note.page_url
            && current.right_pane == note.right_pane
            && current.settings_section == note.settings_section
    }

    /// One `(rect, 1-based index)` per draft note on `current`'s screen, in
    /// order: pins from other screens (another mode, session, page, pane
    /// or Settings section) never draw. The layer renders this.
    pub(crate) fn visible_pin_rects(&self, current: &ScreenContext) -> Vec<(Rect, usize)> {
        self.draft
            .notes
            .iter()
            .filter(|note| FeedbackUi::pin_context_matches(current, &note.context))
            .map(|note| (Self::target_rect(&note.target), note.index))
            .collect()
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
                let id = self.overlays.update(cx, |o, _| {
                    o.toast("Feedback mode is off", "Turn it on in Settings → General.")
                });
                self.feedback.off_toast = Some(id);
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
        // The "mode is off" toast goes stale the moment the mode is on:
        // dismiss it by id on the way in.
        if let Some(id) = self.feedback.off_toast.take() {
            self.overlays.update(cx, |overlays, _| {
                overlays.dismiss_toast(&id);
            });
        }
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = None;
            overlays.menu = None;
        });
        self.close_dialog(cx);
        self.feedback.probe.activate(window);
        // A note-in-progress survived the pause: hand the keyboard back to
        // its field, otherwise to the layer.
        if let Some(note) = self.feedback.popover.as_ref() {
            let focus = note.field.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        } else {
            window.focus(&self.feedback.focus, cx);
        }
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
        // Done first files the open note when it holds text: a
        // note-in-progress is a note, not chrome. An in-flight capture has
        // no popover to file yet, so it is dropped instead.
        if self.feedback.popover.is_some() {
            self.save_feedback_note(window, cx);
        }
        if self.feedback.capture_pending.is_some() {
            self.feedback.cancel_note();
        }
        if !self.feedback.done_enabled(cx) {
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
                        this.feedback.saved_flash =
                            Some(SavedFlash { at: Instant::now(), notes: note_count, path: path.clone() });
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
    /// fills the open popover and saves (waiting behind an in-flight
    /// capture across frames, bounded ~2 s), `cancel` closes the popover
    /// unsaved. Coordinates are window-logical pixels. A `click:`/`drag:`
    /// issued while a capture is pending queues behind it instead of
    /// cancelling it. A note sub with no popover and nothing pending, an
    /// empty text, an unparseable coordinate, or a pointing sub while not
    /// Active records a step failure.
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
                    let step = QueuedStep::Click { step: format!("feedback-mode:{rest}"), point };
                    self.run_or_queue_step(step, window, cx);
                }
                _ => crate::steps::record_step_failure(&format!("feedback-mode:{rest}")),
            }
            cx.notify();
            return;
        }
        if let Some(payload) = rest.strip_prefix("drag:") {
            match parse_feedback_drag(payload) {
                Some((start, end)) if self.feedback.phase == FeedbackPhase::Active => {
                    let step =
                        QueuedStep::Drag { step: format!("feedback-mode:{rest}"), start, end };
                    self.run_or_queue_step(step, window, cx);
                }
                _ => crate::steps::record_step_failure(&format!("feedback-mode:{rest}")),
            }
            cx.notify();
            return;
        }
        if let Some(text) = rest.strip_prefix("note:") {
            if self.feedback.phase != FeedbackPhase::Active || text.trim().is_empty() {
                crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
            } else if self.feedback.popover.is_some() && self.feedback.step_queue.is_empty() {
                self.fill_feedback_note(text, window, cx);
                self.save_feedback_note(window, cx);
            } else if self.feedback.popover.is_some()
                || self.feedback.capture_pending.is_some()
                || self.feedback.pending_click.is_some()
                || !self.feedback.step_queue.is_empty()
            {
                // The popover is not open yet (or an earlier step is still
                // queued): wait behind the capture across frames instead of
                // racing it — the pump fills and saves once it opens, and
                // only then reports failure.
                let step = QueuedStep::Note { step: format!("feedback-mode:{rest}"), text: text.to_owned() };
                self.queue_step(step, cx);
            } else {
                crate::steps::record_step_failure(&format!("feedback-mode:{rest}"));
            }
            cx.notify();
            return;
        }
        if rest == "cancel" {
            // A cancel supersedes queued steps: their popovers will never
            // open, so drop them rather than failing each on expiry.
            self.feedback.step_queue.clear();
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

    /// Run a step `click:`/`drag:` now when the shell is idle, or queue it
    /// behind the in-flight capture (or the already-queued steps) instead
    /// of cancelling them the way an interactive press would.
    pub(crate) fn run_or_queue_step(
        &mut self,
        step: QueuedStep,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        debug_assert!(matches!(step, QueuedStep::Click { .. } | QueuedStep::Drag { .. }));
        if self.feedback.capture_pending.is_some()
            || self.feedback.pending_click.is_some()
            || !self.feedback.step_queue.is_empty()
        {
            self.queue_step(step, cx);
            return;
        }
        match step {
            QueuedStep::Click { point, .. } => {
                self.feedback_press_at(point, window, cx);
                self.feedback_release_at(point, window, cx);
            }
            QueuedStep::Drag { start, end, .. } => {
                self.feedback_press_at(start, window, cx);
                self.feedback_drag_at(end, cx);
                self.feedback_release_at(end, window, cx);
            }
            QueuedStep::Note { .. } => {}
        }
    }

    /// Push a step onto the queue: the frame loop's poll drains it from
    /// there, so no task is ever spawned for it.
    pub(crate) fn queue_step(&mut self, step: QueuedStep, cx: &mut Context<Self>) {
        self.feedback.step_queue.push_back((step, Instant::now()));
        cx.notify();
    }

    /// One pump tick, driven by the layer's per-frame poll (never a task,
    /// so queued steps resolve wherever frames do — the real window, a
    /// headless `--steps` run, or a test draw). A `note:` fills and saves
    /// once its popover opens; a `click:`/`drag:` runs once the shell is
    /// idle; anything still waiting past [`FEEDBACK_STEP_WAIT`] fails
    /// loudly under its own step name.
    pub(crate) fn step_pump_tick(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.phase != FeedbackPhase::Active {
            while let Some((queued, _)) = self.feedback.step_queue.pop_front() {
                crate::steps::record_step_failure(queued.step());
            }
            cx.notify();
            return;
        }
        let Some((_, since)) = self.feedback.step_queue.front() else {
            return;
        };
        if since.elapsed() >= FEEDBACK_STEP_WAIT {
            let (queued, _) = self.feedback.step_queue.pop_front().expect("front checked");
            crate::steps::record_step_failure(queued.step());
            cx.notify();
            return;
        }
        if self.feedback.popover.is_some() {
            // A click or drag behind an open popover waits for the popover
            // to close: cancelling it here would eat the note the script is
            // about to file. It stays at the front with its timestamp, so
            // the expiry above still bounds the wait.
            let front_is_note =
                matches!(self.feedback.step_queue.front(), Some((QueuedStep::Note { .. }, _)));
            if !front_is_note {
                return;
            }
            let (queued, _) = self.feedback.step_queue.pop_front().expect("front checked");
            let QueuedStep::Note { text, .. } = queued else { unreachable!("front checked") };
            self.fill_feedback_note(&text, window, cx);
            self.save_feedback_note(window, cx);
            cx.notify();
            return;
        }
        if self.feedback.capture_pending.is_some() || self.feedback.pending_click.is_some() {
            // Still resolving: wait for the capture (or the cold retry the
            // poll runs right after this) to land.
            return;
        }
        // Idle with no popover: a note has nothing to fill — fail it — and
        // a click or drag runs through the real handlers now.
        let (queued, _) = self.feedback.step_queue.pop_front().expect("front checked");
        match queued {
            QueuedStep::Note { step, .. } => {
                crate::steps::record_step_failure(&step);
            }
            QueuedStep::Click { point, .. } => {
                self.feedback_press_at(point, window, cx);
                self.feedback_release_at(point, window, cx);
            }
            QueuedStep::Drag { start, end, .. } => {
                self.feedback_press_at(start, window, cx);
                self.feedback_drag_at(end, cx);
                self.feedback_release_at(end, window, cx);
            }
        }
        cx.notify();
    }

    /// The mode name the report prints verbatim: `Sessions` / `Browse`.
    pub(crate) fn feedback_mode_name(&self) -> String {
        match self.mode {
            crate::layout::Mode::Sessions => "Sessions".to_owned(),
            crate::layout::Mode::Browse => "Browse".to_owned(),
        }
    }

    /// The open session's raw id, when a session is open.
    pub(crate) fn feedback_session_id(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|view| view.read(cx).session_id.clone())
    }

    /// The human right-pane name (`Browser`, `Files`, …), only while open.
    pub(crate) fn feedback_right_pane(&self) -> Option<String> {
        self.layout
            .right_open
            .then(|| crate::layout::right_kind(&self.layout).label().to_owned())
    }

    /// The shown page's URL (only while a page shows): in Browse the centre
    /// tab's page, otherwise the pane's page. Empty and `about:blank`
    /// never count as a page.
    pub(crate) fn feedback_page_url(&self, cx: &gpui::App) -> Option<String> {
        let state = if self.mode == crate::layout::Mode::Browse {
            self.browse_centre_state()
        } else {
            self.existing_browser(cx)
        };
        state
            .map(|state| state.read(cx).url().to_string())
            .filter(|url| !url.is_empty() && url != "about:blank")
    }

    /// The settings section in view, only while Settings is open.
    pub(crate) fn feedback_settings_section(&self) -> Option<String> {
        self.settings_page
            .open
            .then(|| self.settings_page.section.clone())
            .filter(|section| !section.is_empty())
    }

    /// Where the app was, for a note's [`ScreenContext`]: the mode, the
    /// current project, the open session and its title, the right pane
    /// (only while open), the shown page's URL (only while a page shows),
    /// the settings section (only while Settings is open), the window
    /// size and scale, and the theme. Taken when the note begins, never
    /// refreshed at save time. The mode and pane read exactly as the
    /// report prints them (`Sessions` / `Browse`, `Browser`, `Files`, …);
    /// a session with no title reads `New session`, never a raw id.
    pub(crate) fn feedback_context(&self, window: &mut Window, cx: &gpui::App) -> ScreenContext {
        let project = self.current_project().map(|p| p.name.clone());
        let session_id = self.feedback_session_id(cx);
        let session_title = session_id.as_ref().map(|id| {
            self.browse_chip_session_name(&self.chain_head(id), cx)
        });
        let size = window.viewport_size();
        ScreenContext {
            mode: self.feedback_mode_name(),
            project,
            session_id,
            session_title,
            right_pane: self.feedback_right_pane(),
            page_url: self.feedback_page_url(cx),
            settings_section: self.feedback_settings_section(),
            window_w: f32::from(size.width),
            window_h: f32::from(size.height),
            scale: window.scale_factor(),
            theme: if cx.aui().kind.is_dark() { "dark".to_owned() } else { "light".to_owned() },
        }
    }

    /// The current screen as a [`ScreenContext`] for pin matching: the same
    /// five fields [`Self::feedback_context`] stores per note (mode,
    /// session, page URL, right pane, settings section). Window-only
    /// fields stay empty — pins compare screens, not sizes.
    pub(crate) fn current_pin_context(&self, cx: &gpui::App) -> ScreenContext {
        ScreenContext {
            mode: self.feedback_mode_name(),
            project: None,
            session_id: self.feedback_session_id(cx),
            session_title: None,
            right_pane: self.feedback_right_pane(),
            page_url: self.feedback_page_url(cx),
            settings_section: self.feedback_settings_section(),
            window_w: 0.0,
            window_h: 0.0,
            scale: 1.0,
            theme: String::new(),
        }
    }

    /// The pointer moved while Active with the left button up: refresh the
    /// hover outline. The mouse-move handler and the `hover:` step verb
    /// both land here. Moves under 2 px skip the hit-test, and moves
    /// inside ~30 ms of the last one park the position as the trailing
    /// edge instead of hit-testing: the per-frame poll runs it once the
    /// window has passed, so the outline settles on the last position
    /// after the pointer stops. While a popover is open, while capturing,
    /// or with the button down, hovers stay off. The hit-test never
    /// enriches (that serialises the whole debug tree) — enrichment waits
    /// for [`Self::begin_note_capture`].
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
        if let Some(tested) = self.feedback.hover_tested_at {
            if tested.elapsed() < FEEDBACK_HOVER_THROTTLE {
                self.feedback.hover_dirty = Some(point);
                window.request_animation_frame();
                cx.notify();
                return;
            }
        }
        self.hover_hit_test(point, window, cx);
    }

    /// Run the hover hit-test at `point` now: probe (never enriching),
    /// record the position and the time, and settle the outline — or park
    /// the cold-tree retry when nothing hit. The move handler calls this
    /// past the throttle; the per-frame poll calls it for the trailing
    /// edge once the window has passed.
    pub(crate) fn hover_hit_test(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let hit = self.feedback.probe.hit(window, point);
        self.feedback.hover_point = Some(point);
        self.feedback.hover_tested_at = Some(Instant::now());
        self.feedback.hover_dirty = None;
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

    /// The pointer moved while Active: with the left button down it is a
    /// drag, otherwise a hover. When a press is armed but the button is no
    /// longer down, the mouse-up landed on the bar, the popover or outside
    /// the window and never reached the layer: clear the stale press and
    /// drag without filing a note, then hover on so the outline revives.
    /// The layer's mouse-move handler lands here.
    pub(crate) fn feedback_move_at(
        &mut self,
        point: Point<Pixels>,
        pressed_button: Option<gpui::MouseButton>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.feedback.phase != FeedbackPhase::Active {
            return;
        }
        if pressed_button == Some(gpui::MouseButton::Left) {
            self.feedback_drag_at(point, cx);
            return;
        }
        if self.feedback.press.is_some() {
            self.feedback.press = None;
            self.feedback.drag = None;
            self.feedback_hover_at(point, window, cx);
            cx.notify();
            return;
        }
        self.feedback_hover_at(point, window, cx);
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
            Some(hit) => {
                let win = window.viewport_size();
                let target = FeedbackUi::note_target_for_hit(hit, point, win);
                self.begin_note_capture(target, window, cx);
            }
            None => {
                self.feedback.pending_click =
                    Some(PendingClick { point, since: Instant::now() });
                cx.notify();
            }
        }
    }

    /// The layer's per-frame poll (it renders every frame while Active):
    /// first drain queued steps (a `note:` waiting for its popover, a
    /// `click:`/`drag:` waiting for the idle shell), then run the hover
    /// trailing edge once its throttle window has passed, then retry a
    /// parked click's hit-test while the tree may still be cold, falling
    /// back to the 60×40 area; and retry a cleared hover the same way so
    /// the outline appears when the tree warms.
    pub(crate) fn feedback_poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.phase != FeedbackPhase::Active {
            return;
        }
        if !self.feedback.step_queue.is_empty() {
            self.step_pump_tick(window, cx);
        }
        if self.feedback.capturing
            || self.feedback.popover.is_some()
            || self.feedback.capture_pending.is_some()
        {
            return;
        }
        if let Some(point) = self.feedback.hover_dirty {
            if self.feedback.press.is_none() {
                match self.feedback.hover_tested_at {
                    Some(tested) if tested.elapsed() < FEEDBACK_HOVER_THROTTLE => {
                        window.request_animation_frame();
                    }
                    _ => {
                        self.hover_hit_test(point, window, cx);
                        return;
                    }
                }
            }
        }
        if let Some(pending) = self.feedback.pending_click {
            if let Some(hit) = self.feedback.probe.hit(window, pending.point) {
                self.feedback.pending_click = None;
                let win = window.viewport_size();
                let target = FeedbackUi::note_target_for_hit(hit, pending.point, win);
                self.begin_note_capture(target, window, cx);
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
    /// case the popover still opens, with no images. A no-op while a Done
    /// write is in flight. An element target is enriched once here (its
    /// gpui provenance), never on the hover path.
    pub(crate) fn begin_note_capture(
        &mut self,
        target: Target,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.feedback.saving {
            return;
        }
        self.feedback.hover = None;
        self.feedback.hover_point = None;
        self.feedback.hover_since = None;
        self.feedback.hover_dirty = None;
        self.feedback.press = None;
        self.feedback.drag = None;
        self.feedback.pending_click = None;
        let mut target = target;
        if let Target::Element(hit) = &mut target {
            self.feedback.probe.enrich(window, hit);
        }
        let field = cx.new(|cx| {
            aui::composer::composer_state_rows("What should change?", 3, 8, window, cx)
        });
        // The note carries the screen it was captured on: anything the
        // owner does before Save (Pause, a mode switch, a session switch)
        // must not rewrite it.
        let context = self.feedback_context(window, cx);
        self.feedback.capturing = true;
        self.feedback.capture_seq = self.feedback.capture_seq.wrapping_add(1);
        let seq = self.feedback.capture_seq;
        self.feedback.capture_pending = Some((target, field, context));
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
        self.feedback.capture_task = Some(timer);
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
        self.feedback.capture_task = None;
        let (target, field, context) =
            self.feedback.capture_pending.take().expect("pending checked");
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
        self.feedback.popover = Some(PendingNote { target, crop, full, field, context });
        if let Some(note) = &self.feedback.popover {
            let focus = note.field.read(cx).focus_handle(cx);
            window.focus(&focus, cx);
        }
        cx.notify();
    }

    /// Save the open popover's text as a note. Empty text keeps the
    /// popover open; afterwards focus returns to the layer. The note keeps
    /// the context captured when it began — never a fresh one, which would
    /// name whatever screen the owner has since moved to.
    pub(crate) fn save_feedback_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(note) = self.feedback.popover.as_ref() else {
            return;
        };
        let text = note.field.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let note = self.feedback.popover.take().expect("popover checked");
        self.feedback.draft.add(note.target, text, note.context, note.crop, note.full);
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
    /// saving, staying Active. Escape and Cancel land here. Queued steps
    /// die with it — their popovers will never open.
    pub(crate) fn cancel_feedback_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback.step_queue.clear();
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

    /// Where the note was taken, as a person reads it: `<pane> pane`
    /// while the right pane is open (`Browser pane`, `Files pane`, …),
    /// else the capitalised mode (`Sessions` / `Browse`). Display only —
    /// [`ScreenContext`] keeps the stored lowercase values and the report
    /// is untouched.
    pub(crate) fn feedback_pane_label(&self) -> String {
        if self.layout.right_open {
            format!("{} pane", crate::layout::right_kind(&self.layout).label())
        } else {
            match self.mode {
                crate::layout::Mode::Sessions => "Sessions".to_owned(),
                crate::layout::Mode::Browse => "Browse".to_owned(),
            }
        }
    }

    /// The popover's muted subtitle: `<human role> · in <nearest
    /// ancestor> · <right pane or mode>` for an element, the region or the
    /// pane for an area.
    pub(crate) fn feedback_note_subtitle(&self, target: &Target) -> String {
        let pane = self.feedback_pane_label();
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
        // Keep frames coming while anything is waiting on one: a parked
        // cold-tree click, a cold hover retry, a throttled hover trailing
        // edge, an in-flight capture, or a queued step all stall forever
        // in an idle window otherwise.
        if self.feedback.pending_click.is_some()
            || self.feedback.hover_since.is_some()
            || self.feedback.hover_dirty.is_some()
            || self.feedback.capture_pending.is_some()
            || !self.feedback.step_queue.is_empty()
        {
            window.request_animation_frame();
        }
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
                    this.feedback_move_at(event.position, event.pressed_button, window, cx);
                    cx.stop_propagation();
                }),
            )
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .children(self.render_feedback_hover(cx, win_w, win_h))
            .children(self.render_feedback_dragbox(cx))
            .children(self.render_feedback_pins(cx, win_w, win_h));
        Some(layer.into_any_element())
    }

    /// The hover outline (3 px outside the hit rect, accent border plus a
    /// 14 % accent fill) and its name chip (`<human role> · <label>`,
    /// accent background, on-accent mono text), above the rect or below it
    /// near the top — clamped into the window and out from under the bar.
    /// Decorative: no role, pointer-transparent.
    fn render_feedback_hover(
        &self,
        cx: &mut Context<Self>,
        win_w: f32,
        win_h: f32,
    ) -> Option<AnyElement> {
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
        // padding each side; the position clamps into the window.
        let est_w = chip.chars().count() as f32 * 6.9 + 14.0;
        let (chip_x, chip_y) = FeedbackUi::feedback_chip_pos(hit.rect, est_w, 24.0, win_w, win_h);
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

    /// One pin per draft note on this screen: a 1.5 px accent outline at
    /// ~55 % opacity around its rect and a 22 px numbered badge at the
    /// rect's top-right. Active only, hidden while capturing. Notes from
    /// other screens (another mode, session, page, pane or Settings
    /// section) draw no pin. Decorative: no role, pointer-transparent.
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
        let current = self.current_pin_context(cx);
        let pins = self
            .feedback
            .visible_pin_rects(&current)
            .into_iter()
            .map(|(rect, index)| {
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
                            .child(index.to_string()),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        Some(div().children(pins).into_any_element())
    }

    /// The floating chrome: the bar while Active, nothing otherwise — and
    /// nothing while capturing, when the window must read clean. While
    /// Paused the shell lives in the window's header row (see
    /// [`Self::render_feedback_header_slot`]): native pages paint above
    /// gpui, so a floating pill sits under the page and its clicks hit the
    /// page instead.
    pub(crate) fn render_feedback_chrome(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.feedback.capturing {
            return None;
        }
        match self.feedback.phase {
            FeedbackPhase::Off | FeedbackPhase::Paused => None,
            FeedbackPhase::Active => Some(self.render_feedback_bar(window, cx)),
        }
    }

    /// The Paused shell (and the Done confirmation) in the window's top
    /// header row, just left of its icon buttons — a region native pages
    /// never cover. While Paused: dot · N notes · Resume · Done, compact
    /// (the height of a header button). After Done lands: `Saved · N
    /// notes — path copied` for ~4 s, so the confirmation is visible even
    /// when a page covers the toast area. Anywhere else: nothing.
    pub(crate) fn render_feedback_header_slot(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.feedback.capturing {
            return None;
        }
        if self.feedback.phase == FeedbackPhase::Paused {
            return Some(self.render_feedback_head_pill(cx));
        }
        if self.feedback.phase == FeedbackPhase::Off {
            let fresh = self
                .feedback
                .saved_flash
                .as_ref()
                .is_some_and(|flash| flash.at.elapsed() < FEEDBACK_SAVED_FLASH);
            if !fresh {
                return None;
            }
            let flash = self.feedback.saved_flash.as_ref().expect("fresh checked");
            let unit = if flash.notes == 1 { "note" } else { "notes" };
            let p = cx.aui().colors;
            let accent = feedback_accent(cx);
            return Some(
                h_flex()
                    .id("feedback-saved")
                    .occlude()
                    .flex_none()
                    .items_center()
                    .gap(px(6.0))
                    .h(px(28.0))
                    .px(px(10.0))
                    .rounded(px(scale::R_SM))
                    .bg(p.surface_1)
                    .border_1()
                    .border_color(p.line_strong)
                    .role(gpui::Role::Group)
                    .aria_label(format!("Feedback saved to {}", flash.path))
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(accent))
                    .child(
                        div()
                            .flex_none()
                            .mono(scale::FS_12)
                            .text_color(p.ink_2)
                            .whitespace_nowrap()
                            .child(format!("Saved · {} {unit} — path copied", flash.notes)),
                    )
                    .into_any_element(),
            );
        }
        None
    }

    /// The Paused pill for the header slot: dot · N notes · Resume · Done,
    /// compact at header-button height. Done is the same accent button as
    /// the bar's, one line.
    fn render_feedback_head_pill(&self, cx: &mut Context<Self>) -> AnyElement {
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
        h_flex()
            .id("feedback-pill")
            .occlude()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(28.0))
            .px(px(10.0))
            .rounded(px(scale::R_SM))
            .bg(p.surface_1)
            .border_1()
            .border_color(p.line_strong)
            .shadow(p.shadow(1))
            .role(gpui::Role::Group)
            .aria_label("Feedback paused")
            .child(div().flex_none().size(px(8.0)).rounded_full().bg(accent))
            .child(
                div()
                    .flex_none()
                    .mono(scale::FS_12)
                    .text_color(p.ink_3)
                    .whitespace_nowrap()
                    .child(notes),
            )
            .child(
                button("feedback-resume", "Resume")
                    .accessibility_label("Resume feedback")
                    .on_click(move |_, window, cx| resume(&(), window, cx)),
            )
            .child(done_button)
            .into_any_element()
    }

    /// The accent Done button both the bar and the pill share: feedback
    /// accent fill with on-accent text (never Baaz's purple primary) while
    /// enabled; while disabled a transparent background with a 1 px accent
    /// border at ~45 % and accent text at ~55 % (both lines in the bar).
    /// A styled div with the aui md metrics (28 px height, 11 px side
    /// padding, small radius), a Button role and label, hover and disabled
    /// states.
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
        let faint_border = gpui::Hsla { a: 0.45, ..accent };
        let faint_text = gpui::Hsla { a: 0.55, ..accent };
        let enabled = self.feedback.done_enabled(cx);
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
            .border_color(if enabled { gpui::transparent_black() } else { faint_border })
            .bg(if enabled { accent } else { gpui::transparent_black() })
            .when(enabled, |this| {
                this.hover(|this| this.bg(hover)).cursor_pointer()
            })
            .text_color(if enabled { on_accent } else { faint_text })
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
                        .text_color(if enabled { sub } else { faint_text })
                        .whitespace_nowrap()
                        .child("Saves the report and copies its path"),
                )
            })
            .shadow(p.shadow(1))
            .into_any_element()
    }

    /// The note popover: a sibling of the capture layer (never inside its
    /// key context), floating above it and consuming its own mouse events.
    /// 300 px wide, 12 px right of the target (left when there is no room),
    /// top-aligned and flipped above the target when there is no room
    /// below, so the whole popover including Save stays on screen below
    /// the bar: header, field, Cancel/Save, hints. Save reads disabled
    /// while the text is empty.
    pub(crate) fn render_feedback_popover(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.feedback.phase != FeedbackPhase::Active || self.feedback.capturing {
            return None;
        }
        let note = self.feedback.popover.as_ref()?;
        let (target, field) = (note.target.clone(), note.field.clone());
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let on_accent = feedback_on_accent(cx);
        let hover = gpui::Hsla { l: (accent.l - 0.06).max(0.0), ..accent };
        let win = window.viewport_size();
        let (win_w, win_h) = (f32::from(win.width), f32::from(win.height));
        let rect = FeedbackUi::target_rect(&target);
        // The popover's full height at the field's current visual row
        // count (wrapped lines, so a long single line grows it), so the
        // position keeps all of it — including Save — on screen, flipping
        // above the target when there is no room below.
        let rows = FeedbackUi::feedback_visual_rows(&note.field.read(cx).value());
        let pop_h = FeedbackUi::feedback_popover_height(rows);
        let (x, y) = FeedbackUi::feedback_popover_pos(rect, pop_h, win_w, win_h);
        let title = Self::feedback_note_title(&target);
        let subtitle = self.feedback_note_subtitle(&target);
        // Empty text saves nothing (the save keeps focus as a no-op), so
        // the button reads disabled until there is something to file.
        let save_enabled = !note.field.read(cx).value().trim().is_empty();
        let save_border = gpui::Hsla { a: 0.45, ..accent };
        let save_faint = gpui::Hsla { a: 0.55, ..accent };
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
                            .border_color(if save_enabled {
                                gpui::transparent_black()
                            } else {
                                save_border
                            })
                            .bg(if save_enabled { accent } else { gpui::transparent_black() })
                            .when(save_enabled, |this| {
                                this.hover(|this| this.bg(hover)).cursor_pointer()
                            })
                            .ui(scale::FS_13)
                            .medium()
                            .text_color(if save_enabled { on_accent } else { save_faint })
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

}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feedback::{FakeProbe, Rect, Target};

    /// A live shell on the test app: the transition tests need a real
    /// focus handle, which needs the app.
    fn live_shell(cx: &mut gpui::App) -> FeedbackUi {
        FeedbackUi::new(
            Box::new(FakeProbe {
                hits: Vec::new(),
                enrich_calls: std::rc::Rc::new(std::cell::Cell::new(0)),
            }),
            cx,
        )
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
            assert!(!shell.done_enabled(cx), "Off never saves");
            shell.toggle(true);
            assert!(!shell.done_enabled(cx), "Active with 0 notes never saves");
            note_on(&mut shell, "one");
            assert!(shell.done_enabled(cx));
            shell.pause();
            assert!(shell.done_enabled(cx), "Paused saves too");
            shell.done_applied();
            assert_eq!(shell.phase, FeedbackPhase::Off);
            assert!(shell.draft.is_empty());
            assert!(!shell.done_enabled(cx));
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
        assert_eq!(ctx.mode, "Sessions");
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
            assert!(!shell.done_enabled(cx), "no second Done while saving");
            shell.pause();
            assert_eq!(shell.phase, FeedbackPhase::Active, "pause waits out the save");
            shell.resume();
            assert_eq!(shell.phase, FeedbackPhase::Active, "resume waits out the save");
            assert_eq!(shell.discard_press(), DiscardOutcome::Disabled);
            shell.discard_all();
            assert_eq!(shell.note_count(), 1, "the in-flight draft survives");
        });
    }

    /// The layer element exists only while Active; the floating chrome
    /// exists only while Active (the bar). While Paused the shell lives
    /// in the header slot instead — native pages paint above a floating
    /// pill — and while Off nothing renders unless a Done flash is fresh.
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
        let slot = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.render_feedback_header_slot(cx).is_some()))
        };
        assert!(!layer(vc), "Off renders no layer");
        assert!(!chrome(vc), "Off renders no chrome");
        assert!(!slot(vc), "Off renders no header slot");
        enter_active(&baaz, vc);
        assert!(layer(vc), "Active renders the layer");
        assert!(chrome(vc), "Active renders the bar");
        assert!(!slot(vc), "Active needs no header slot");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.pause_feedback(window, cx)));
        assert!(!layer(vc), "Paused renders no layer");
        assert!(!chrome(vc), "Paused renders no floating chrome");
        assert!(slot(vc), "Paused renders the header pill");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.resume_feedback(window, cx)));
        assert!(!slot(vc), "back Active clears the header slot");
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

    /// F3 test helpers: scripted hits, window-logical points, and the
    /// synchronous click path (the real press/release handlers plus the
    /// real capture finish — the async frame/timer paths call that same
    /// finish).
    fn f3_hit(x: f32, y: f32, w: f32, h: f32, label: &str) -> ElementHit {
        ElementHit {
            label: label.to_owned(),
            role: "AXButton".to_owned(),
            ancestors: vec![("AXGroup".to_owned(), "Header".to_owned())],
            rect: Rect { x, y, w, h },
            element_id: None,
            view: None,
        }
    }

    fn f3_point(x: f32, y: f32) -> gpui::Point<gpui::Pixels> {
        gpui::point(gpui::px(x), gpui::px(y))
    }

    /// Swap the shell's probe for scripted hits, handing back the shared
    /// enrich counter so tests assert hover never enriches and a click
    /// enriches exactly once.
    fn inject_hits(
        baaz: &gpui::Entity<crate::app::Harness>,
        vc: &mut gpui::VisualTestContext,
        hits: Vec<ElementHit>,
    ) -> std::rc::Rc<std::cell::Cell<usize>> {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let probe_calls = calls.clone();
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.probe = Box::new(FakeProbe { hits, enrich_calls: probe_calls });
            })
        });
        calls
    }

    /// Drive the real frame path: run parked tasks (firing the capture's
    /// 120 ms bound) and deliver two painted frames for the capture's
    /// nested `on_next_frame` pair.
    fn drive_frames(vc: &mut gpui::VisualTestContext) {
        vc.run_until_parked();
        vc.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.simulate_next_frame(cx);
        });
        vc.run_until_parked();
    }

    /// Draw a real frame: the full render runs the layer (its poll ticks
    /// the queued steps and retries cold clicks) inside the paint phase,
    /// where requesting the next frame is legal.
    fn draw_frame(vc: &mut gpui::VisualTestContext) {
        vc.update(|window, cx| {
            window.draw(cx).clear(cx);
        });
    }

    /// Drive the queued-step flow the way frames do: each pass draws (the
    /// layer's poll ticks the queue), lands captures, and fires timers.
    fn pump_frames(vc: &mut gpui::VisualTestContext, passes: usize) {
        for _ in 0..passes {
            draw_frame(vc);
            drive_frames(vc);
        }
    }

    /// Press and release at one point, then complete the capture the way
    /// the frame/timer paths do once they land.
    fn f3_click(
        baaz: &gpui::Entity<crate::app::Harness>,
        vc: &mut gpui::VisualTestContext,
        x: f32,
        y: f32,
    ) {
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let point = f3_point(x, y);
                h.feedback_press_at(point, window, cx);
                h.feedback_release_at(point, window, cx);
                let seq = h.feedback.capture_seq;
                h.finish_note_capture(window, cx, seq);
            })
        });
    }

    fn f3_popover_target(
        baaz: &gpui::Entity<crate::app::Harness>,
        vc: &mut gpui::VisualTestContext,
    ) -> Option<Target> {
        vc.update(|_, cx| baaz.read(cx).feedback.popover.as_ref().map(|note| note.target.clone()))
    }

    /// Hover hit-tests on a ≥ 2 px move and clears off every hit; a
    /// sub-2 px move skips the hit-test and keeps the outline.
    #[gpui::test]
    fn hover_sets_and_clears(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-hover");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.feedback_hover_at(f3_point(20.0, 20.0), window, cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.as_ref().map(|hit| hit.label.clone())),
            Some("Headers".to_owned()),
            "hovering the hit outlines it"
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.feedback_hover_at(f3_point(20.5, 20.0), window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.is_some()),
            "a sub-2 px move keeps the outline without re-hit-testing"
        );
        // The throttle would skip an immediate re-test: age the last one
        // past ~30 ms so the empty-space hover really hit-tests.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.hover_tested_at =
                    Some(Instant::now() - FEEDBACK_HOVER_THROTTLE - Duration::from_millis(1));
            })
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.feedback_hover_at(f3_point(500.0, 500.0), window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.is_none()),
            "hovering empty space clears the outline"
        );
        restore_state(state);
    }

    /// Hover never enriches (the debug-tree parse waits for the note's
    /// target) and hit-tests at most once per ~30 ms — but the outline
    /// still settles on the LAST position: a move inside the window parks
    /// the trailing edge, and the frame poll hit-tests it once the window
    /// has passed (driven here by a real draw plus the clock, never by
    /// calling the poll by hand).
    #[gpui::test]
    fn hover_throttles_and_never_enriches(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-hover-throttle");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        let calls = inject_hits(
            &baaz,
            vc,
            vec![
                f3_hit(10.0, 10.0, 100.0, 30.0, "Headers"),
                f3_hit(300.0, 300.0, 100.0, 30.0, "Footer"),
            ],
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.feedback_hover_at(f3_point(20.0, 20.0), window, cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.as_ref().map(|hit| hit.label.clone())),
            Some("Headers".to_owned())
        );
        assert_eq!(calls.get(), 0, "hover never enriches");
        // An immediate move onto the other hit is throttled: no re-test,
        // the old outline stays, the position parks as the trailing edge,
        // and still no enrichment.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.feedback_hover_at(f3_point(310.0, 310.0), window, cx)));
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.as_ref().map(|hit| hit.label.clone())),
            Some("Headers".to_owned()),
            "the throttled move keeps the last outline"
        );
        assert_eq!(calls.get(), 0, "the throttled move enriches nothing either");
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover_dirty.is_some()),
            "the throttled move parks the trailing edge"
        );
        // The pointer stops. Once the window elapses, a real frame's poll
        // runs the parked hit-test and the outline reaches the last
        // position.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.hover_tested_at =
                    Some(Instant::now() - FEEDBACK_HOVER_THROTTLE - Duration::from_millis(1));
            })
        });
        draw_frame(vc);
        assert_eq!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover.as_ref().map(|hit| hit.label.clone())),
            Some("Footer".to_owned()),
            "after the window the outline reaches the last position"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.hover_dirty.is_none()),
            "the poll consumes the trailing edge"
        );
        assert_eq!(calls.get(), 0, "even the trailing re-test never enriches");
        restore_state(state);
    }

    /// A click on a hit opens the popover with that element as its target,
    /// staying Active — driven through the real frame path (the step
    /// handler, then parked tasks plus painted frames), never by calling
    /// the capture finish by hand. The click enriches exactly once.
    #[gpui::test]
    fn click_on_a_hit_opens_an_element_popover(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-click-hit");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        let calls =
            inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.popover.is_none()),
            "no popover yet: the capture has not landed"
        );
        drive_frames(vc);
        match f3_popover_target(&baaz, vc) {
            Some(Target::Element(hit)) => assert_eq!(hit.label, "Headers"),
            other => panic!("the popover points at the clicked element, got {other:?}"),
        }
        assert_eq!(calls.get(), 1, "the click enriches exactly once");
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.phase == FeedbackPhase::Active),
            "note-taking stays Active"
        );
        restore_state(state);
    }

    /// A click on no hit parks a warm retry; when the tree stays cold past
    /// a second, a real frame's poll falls back to a 60×40 area centred on
    /// the point — driven through the real frame path (the step handler,
    /// a draw, then frames), never by calling the poll or the capture
    /// finish by hand.
    #[gpui::test]
    fn click_off_any_hit_falls_back_to_a_centred_area(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-click-miss");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, Vec::new());
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:200,200", window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.popover.is_none()),
            "no popover yet: the click waits for the tree to warm"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.pending_click.is_some()),
            "the miss parks a retry"
        );
        // Let the tree stay cold past the warm window (wall-clock: the
        // retry bounds on `Instant`, not the test clock), then draw a real
        // frame: its poll falls back to the area and starts the capture,
        // which the frames below land.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                if let Some(pending) = h.feedback.pending_click.as_mut() {
                    pending.since = Instant::now() - FEEDBACK_PROBE_WARM - Duration::from_secs(1);
                }
            })
        });
        draw_frame(vc);
        drive_frames(vc);
        match f3_popover_target(&baaz, vc) {
            Some(Target::Area { rect, region }) => {
                assert_eq!(rect, Rect { x: 170.0, y: 180.0, w: 60.0, h: 40.0 });
                assert_eq!(region, None);
            }
            other => panic!("the cold click falls back to a centred 60×40 area, got {other:?}"),
        }
        restore_state(state);
    }

    /// Dragging up-left normalises the box, and the region names the hit
    /// at the box centre.
    #[gpui::test]
    fn drag_up_left_normalises_and_names_the_region(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-drag");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(190.0, 190.0, 40.0, 40.0, "Canvas")]);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.feedback_press_at(f3_point(300.0, 300.0), window, cx);
                h.feedback_drag_at(f3_point(100.0, 100.0), cx);
                h.feedback_release_at(f3_point(100.0, 100.0), window, cx);
                let seq = h.feedback.capture_seq;
                h.finish_note_capture(window, cx, seq);
            })
        });
        match f3_popover_target(&baaz, vc) {
            Some(Target::Area { rect, region }) => {
                assert_eq!(rect, Rect { x: 100.0, y: 100.0, w: 200.0, h: 200.0 });
                assert_eq!(region, Some("near Canvas".to_owned()));
            }
            other => panic!("the drag opens a normalised named area, got {other:?}"),
        }
        restore_state(state);
    }

    /// A 4×4 drag is a click: on a hit it opens that element's popover.
    #[gpui::test]
    fn tiny_drag_counts_as_a_click(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-tiny-drag");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.feedback_press_at(f3_point(20.0, 20.0), window, cx);
                h.feedback_drag_at(f3_point(24.0, 24.0), cx);
                h.feedback_release_at(f3_point(24.0, 24.0), window, cx);
                let seq = h.feedback.capture_seq;
                h.finish_note_capture(window, cx, seq);
            })
        });
        match f3_popover_target(&baaz, vc) {
            Some(Target::Element(hit)) => assert_eq!(hit.label, "Headers"),
            other => panic!("the 4×4 drag clicks through to the element, got {other:?}"),
        }
        restore_state(state);
    }

    /// Saving files the note with its context, assigns index 1, closes
    /// the popover and stays Active; empty text keeps it open and saves
    /// nothing.
    #[gpui::test]
    fn save_files_the_note_and_empty_text_keeps_it_open(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-save");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        f3_click(&baaz, vc, 20.0, 20.0);
        // Empty first: nothing saves, the popover stays open.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.save_feedback_note(window, cx)));
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.draft.is_empty()),
            "empty text saves nothing"
        );
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()),
            "empty text keeps the popover open"
        );
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.fill_feedback_note("quieter header", window, cx);
                h.save_feedback_note(window, cx);
            })
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.note_count(), 1);
            let note = &harness.feedback.draft.notes[0];
            assert_eq!(note.index, 1);
            assert_eq!(note.text, "quieter header");
            assert_eq!(note.context.mode, "Sessions", "the context is filled per note");
            assert!(note.context.window_w > 0.0, "the context carries the window");
            assert!(harness.feedback.popover.is_none(), "saving closes the popover");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
        });
        restore_state(state);
    }

    /// Cancel (and a press outside the popover) closes without saving and
    /// stays Active — and no new note starts from that press.
    #[gpui::test]
    fn cancel_and_outside_press_close_without_saving(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-cancel");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        f3_click(&baaz, vc, 20.0, 20.0);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.cancel_feedback_note(window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.feedback.popover.is_none());
            assert!(harness.feedback.draft.is_empty(), "cancelling saves nothing");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active, "cancelling stays Active");
        });
        // Again, this time cancelled by a press outside the popover: the
        // release that follows must not start a new note.
        f3_click(&baaz, vc, 20.0, 20.0);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.feedback_press_at(f3_point(400.0, 400.0), window, cx);
                h.feedback_release_at(f3_point(400.0, 400.0), window, cx);
            })
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.feedback.popover.is_none(), "the outside press cancels");
            assert!(harness.feedback.capture_pending.is_none(), "no capture starts");
            assert!(harness.feedback.pending_click.is_none(), "no retry parks");
            assert!(harness.feedback.draft.is_empty());
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
        });
        restore_state(state);
    }

    /// One pin per note on this screen: the pin rects match the note
    /// targets in order, and notes from other screens draw nothing — a
    /// different mode, session, page URL, right pane or Settings section
    /// each hides its pin.
    #[gpui::test]
    fn pins_match_the_notes_on_this_screen(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| {
            let mut shell = live_shell(cx);
            shell.toggle(true);
            let current = crate::feedback::test_context();
            assert!(shell.visible_pin_rects(&current).is_empty(), "no notes, no pins");
            note_on(&mut shell, "one");
            shell.draft.add(
                Target::Element(f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")),
                "two".to_owned(),
                crate::feedback::test_context(),
                None,
                None,
            );
            let pins = shell.visible_pin_rects(&current);
            assert_eq!(pins.len(), 2, "one pin per note");
            assert_eq!(pins[0].0, Rect { x: 1.0, y: 2.0, w: 3.0, h: 4.0 });
            assert_eq!(pins[0].1, 1);
            assert_eq!(pins[1].0, Rect { x: 10.0, y: 10.0, w: 100.0, h: 30.0 });
            assert_eq!(pins[1].1, 2);

            // The matrix: each screen field on its own hides the pin.
            let mut other = current.clone();
            other.mode = "Browse".to_owned();
            assert!(shell.visible_pin_rects(&other).is_empty(), "another mode hides every pin");
            let mut other = current.clone();
            other.session_id = Some("other-session".to_owned());
            assert!(shell.visible_pin_rects(&other).is_empty(), "another session hides every pin");
            let mut other = current.clone();
            other.page_url = Some("https://example.com/other".to_owned());
            assert!(shell.visible_pin_rects(&other).is_empty(), "another page hides every pin");
            let mut other = current.clone();
            other.right_pane = Some("Browser".to_owned());
            assert!(shell.visible_pin_rects(&other).is_empty(), "another pane hides every pin");
            let mut other = current.clone();
            other.settings_section = Some("General".to_owned());
            assert!(shell.visible_pin_rects(&other).is_empty(), "another section hides every pin");
            assert!(
                FeedbackUi::pin_context_matches(&current, &current),
                "the same screen matches"
            );
        });
    }

    /// The pure geometry: up-left drags normalise, sub-6 px boxes are
    /// clicks, clicks centre the 60×40 fallback inside the window.
    #[gpui::test]
    fn drag_and_click_geometry() {
        let win = gpui::size(gpui::px(1000.0), gpui::px(800.0));
        assert_eq!(
            FeedbackUi::drag_rect(f3_point(300.0, 300.0), f3_point(100.0, 100.0)),
            Some(Rect { x: 100.0, y: 100.0, w: 200.0, h: 200.0 })
        );
        assert_eq!(
            FeedbackUi::drag_rect(f3_point(10.0, 10.0), f3_point(14.0, 14.0)),
            None,
            "a 4×4 release is a click"
        );
        assert!(FeedbackUi::is_drag(f3_point(0.0, 0.0), f3_point(6.0, 0.0)));
        assert!(!FeedbackUi::is_drag(f3_point(0.0, 0.0), f3_point(5.9, 5.9)));
        assert!(FeedbackUi::hover_moved(f3_point(0.0, 0.0), f3_point(2.0, 0.0)));
        assert!(!FeedbackUi::hover_moved(f3_point(0.0, 0.0), f3_point(1.9, 1.9)));
        match FeedbackUi::click_target(None, f3_point(200.0, 200.0), win) {
            Target::Area { rect, region } => {
                assert_eq!(rect, Rect { x: 170.0, y: 180.0, w: 60.0, h: 40.0 });
                assert_eq!(region, None);
            }
            other => panic!("a miss centres the fallback, got {other:?}"),
        }
        // Near the corner the fallback clamps into the window.
        match FeedbackUi::click_target(None, f3_point(5.0, 5.0), win) {
            Target::Area { rect, .. } => assert_eq!(rect, Rect { x: 0.0, y: 0.0, w: 60.0, h: 40.0 }),
            other => panic!("the fallback clamps, got {other:?}"),
        }
    }

    /// Typing into the open popover: `h i`, a `cmd-v` paste, `shift-enter`
    /// for a newline, then `enter` saves the exact text. `escape` closes a
    /// popover unsaved while the phase stays Active.
    #[gpui::test]
    fn typing_paste_and_enter_save_while_escape_cancels(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-typing");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        f3_click(&baaz, vc, 20.0, 20.0);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()));
        vc.update(|_, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string("XY".to_owned()));
        });
        vc.simulate_keystrokes("h i");
        vc.simulate_keystrokes("cmd-v");
        vc.simulate_keystrokes("shift-enter");
        let field = vc.update(|_, cx| {
            baaz.read(cx).feedback.popover.as_ref().expect("popover open").field.read(cx).value().to_string()
        });
        assert_eq!(field, "hiXY\n", "typing, paste and shift-enter land in the field");
        vc.simulate_keystrokes("enter");
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.note_count(), 1);
            assert_eq!(harness.feedback.draft.notes[0].text, "hiXY");
            assert!(harness.feedback.popover.is_none(), "enter saves and closes");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
        });
        // Escape closes the next popover unsaved and never pauses.
        f3_click(&baaz, vc, 20.0, 20.0);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()));
        vc.simulate_keystrokes("escape");
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.feedback.popover.is_none(), "escape closes the popover");
            assert_eq!(harness.feedback.note_count(), 1, "escape saves nothing");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active, "escape never pauses");
        });
        restore_state(state);
    }

    /// The scripted `click → note → drag → note` sequence files two notes:
    /// `note:` waits behind the in-flight capture across frames instead of
    /// racing it, and the `drag:` queues behind the first capture instead
    /// of cancelling it. Then `cancel` really runs: it closes a fresh
    /// popover unsaved. All through `step_feedback` plus parked tasks —
    /// never by calling the capture finish by hand. The drag's centre has
    /// a scripted hit so the area names its region without waiting out
    /// the cold-tree window (wall-clock, not the test clock).
    #[gpui::test]
    fn note_and_cancel_steps_fill_and_close(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-note-step");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(
            &baaz,
            vc,
            vec![
                f3_hit(10.0, 10.0, 100.0, 30.0, "Headers"),
                f3_hit(700.0, 330.0, 100.0, 80.0, "Canvas"),
            ],
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_feedback("note:Make this quieter", window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_feedback("drag:560,300,900,420", window, cx))
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_feedback("note:Too much space here", window, cx))
        });
        pump_frames(vc, 8);
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.note_count(), 2, "click → note → drag → note files 2 notes");
            assert_eq!(harness.feedback.draft.notes[0].text, "Make this quieter");
            assert_eq!(harness.feedback.draft.notes[1].text, "Too much space here");
            match &harness.feedback.draft.notes[1].target {
                Target::Area { rect, region } => {
                    assert_eq!(*rect, Rect { x: 560.0, y: 300.0, w: 340.0, h: 120.0 });
                    assert_eq!(*region, Some("near Canvas".to_owned()));
                }
                other => panic!("the drag saves an area, got {other:?}"),
            }
            assert!(harness.feedback.popover.is_none());
            assert!(harness.feedback.step_queue.is_empty(), "the pump drained the queue");
        });
        // `cancel` really runs: a fresh popover opens, then closes unsaved.
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        drive_frames(vc);
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()),
            "the fresh click opens a popover"
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("cancel", window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.feedback.popover.is_none(), "cancel closes the popover");
            assert_eq!(harness.feedback.note_count(), 2, "cancel saves nothing");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active, "cancel stays Active");
        });
        restore_state(state);
    }

    /// A press whose mouse-up landed on the bar, the popover or outside
    /// the window never reaches the layer: the next button-up move clears
    /// the stale press and drag without filing a note, and hover revives.
    #[gpui::test]
    fn stale_press_clears_on_move_without_filing(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-stale-press");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.feedback_press_at(f3_point(20.0, 20.0), window, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.press.is_some()));
        // The button is up but the press is still armed: the release went
        // elsewhere. A move clears it — no note, no retry, no capture.
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.feedback_move_at(f3_point(30.0, 30.0), None, window, cx))
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(harness.feedback.press.is_none(), "the stale press clears");
            assert!(harness.feedback.drag.is_none(), "the stale drag clears");
            assert!(harness.feedback.pending_click.is_none(), "no retry parks");
            assert!(harness.feedback.capture_pending.is_none(), "no capture starts");
            assert!(harness.feedback.popover.is_none(), "no note is filed");
            assert!(harness.feedback.draft.is_empty());
            assert!(
                harness.feedback.hover.as_ref().is_some_and(|hit| hit.label == "Headers"),
                "hover revives on the same move"
            );
        });
        restore_state(state);
    }

    /// The pure clamp geometry: the chip sits below a top-edge target,
    /// above a mid-window one, on screen at the bottom edge, and pinned
    /// inside an element taller than the window; the popover drops below
    /// the bar at the top edge and flips above a bottom-edge target so
    /// Save stays on screen, growing with the field's row count.
    #[gpui::test]
    fn chip_and_popover_clamps() {
        let (win_w, win_h) = (1000.0, 800.0);
        let (_, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 100.0, y: 8.0, w: 200.0, h: 24.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!(y, 36.0, "a top-edge chip sits below the element");
        let (_, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 100.0, y: 400.0, w: 200.0, h: 30.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!(y, 372.0, "a mid-window chip sits above the element");
        let (_, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 100.0, y: 780.0, w: 200.0, h: 30.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!(y, 752.0, "a bottom-edge chip stays on screen");
        assert!(y + 24.0 <= win_h);
        let (x, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 50.0, y: 0.0, w: 900.0, h: 900.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!((x, y), (58.0, 8.0), "a taller-than-window chip pins inside its top-left");
        // Past ~60 % of the window height the bottom edge is unreachable,
        // so the chip moves inside even when the element fits on screen.
        let (x, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 100.0, y: 100.0, w: 800.0, h: 500.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!((x, y), (108.0, 108.0), "a taller-than-60 % chip pins inside its top-left");
        let (_, y) = FeedbackUi::feedback_chip_pos(
            Rect { x: 100.0, y: 100.0, w: 800.0, h: 400.0 },
            150.0,
            24.0,
            win_w,
            win_h,
        );
        assert_eq!(y, 72.0, "below 60 % the chip still sits above the element");

        assert!(
            FeedbackUi::feedback_popover_height(8) > FeedbackUi::feedback_popover_height(3),
            "the estimate grows with the row count"
        );
        assert_eq!(
            FeedbackUi::feedback_popover_height(1),
            FeedbackUi::feedback_popover_height(3),
            "rows clamp to the field minimum"
        );
        assert_eq!(
            FeedbackUi::feedback_popover_height(20),
            FeedbackUi::feedback_popover_height(8),
            "rows clamp to the field maximum"
        );
        let (_, y) = FeedbackUi::feedback_popover_pos(
            Rect { x: 100.0, y: 8.0, w: 200.0, h: 24.0 },
            FeedbackUi::feedback_popover_height(3),
            win_w,
            win_h,
        );
        assert_eq!(y, FEEDBACK_BAR_ZONE_H, "a top-edge popover drops below the bar");
        let (_, y) = FeedbackUi::feedback_popover_pos(
            Rect { x: 100.0, y: 740.0, w: 200.0, h: 40.0 },
            250.0,
            win_w,
            win_h,
        );
        assert_eq!(y, 482.0, "a bottom-edge popover flips above the target");
        assert!(y + 250.0 <= win_h - 8.0, "the whole popover including Save stays on screen");
        assert!(y >= FEEDBACK_BAR_ZONE_H);
    }

    /// Pause keeps a note-in-progress (popover plus typed text) and Resume
    /// restores it with the keyboard back in its field; Done files the open
    /// note first and then writes.
    #[gpui::test]
    fn pause_keeps_the_note_and_resume_restores_it(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-pause-note");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        drive_frames(vc);
        assert!(vc.update(|_, cx| baaz.read(cx).feedback.popover.is_some()));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.fill_feedback_note("half-typed words", window, cx))
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.pause_feedback(window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Paused);
            assert!(harness.feedback.popover.is_some(), "pause keeps the popover");
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.resume_feedback(window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                assert_eq!(h.feedback.phase, FeedbackPhase::Active);
                let note = h.feedback.popover.as_ref().expect("resume restores the popover");
                assert_eq!(
                    note.field.read(cx).value().to_string(),
                    "half-typed words",
                    "the typed text survives the round-trip"
                );
                assert!(
                    note.field.read(cx).focus_handle(cx).is_focused(window),
                    "resume hands the keyboard back to the field"
                );
            })
        });
        restore_state(state);
    }

    /// Done with an unsaved open note files it first, then writes: the
    /// toast counts it and the shell parks Off with a cleared draft.
    #[gpui::test]
    fn done_saves_the_open_note_first(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-done-note");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        drive_frames(vc);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.fill_feedback_note("file me on Done", window, cx))
        });
        // Done reads enabled through the same predicate the button
        // renders with: the open popover holds text, so Done will file it
        // first even though the draft is still empty.
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.done_enabled(cx)),
            "an open note with text enables Done with an empty draft"
        );
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.finish_feedback(window, cx)));
        vc.run_until_parked();
        let latest =
            std::fs::read_to_string(crate::feedback::feedback_root().join("latest")).expect("Done writes latest");
        assert!(std::path::PathBuf::from(latest.trim()).is_dir(), "the report dir lands");
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Off, "Done parks Off");
            assert!(harness.feedback.draft.is_empty(), "Done clears the draft");
            let toast = harness.overlays.read(cx).toasts.last().cloned().expect("a toast lands");
            assert_eq!(toast.title.to_string(), "Feedback saved · 1 note");
        });
        restore_state(state);
    }

    /// Starting a capture while a Done write is in flight is a no-op: the
    /// write owns the window until it lands.
    #[gpui::test]
    fn begin_capture_refuses_while_saving(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-capture-saving");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.feedback.saving = true;
                h.begin_note_capture(
                    Target::Area { rect: Rect { x: 1.0, y: 1.0, w: 8.0, h: 8.0 }, region: None },
                    window,
                    cx,
                );
            })
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert!(!harness.feedback.capturing, "no capture starts while saving");
            assert!(harness.feedback.capture_pending.is_none());
            assert!(harness.feedback.popover.is_none());
        });
        restore_state(state);
    }

    /// The popover subtitle reads as a person reads it: the capitalised
    /// mode, or `<pane> pane` while the right pane is open. The stored
    /// [`ScreenContext`] values stay lowercase — display only.
    #[gpui::test]
    fn subtitle_reads_as_a_person_reads_it(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-subtitle");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        let target = Target::Element(f3_hit(10.0, 10.0, 100.0, 30.0, "Headers"));
        let subtitle = vc.update(|_, cx| baaz.read(cx).feedback_note_subtitle(&target));
        assert_eq!(subtitle, "Button · in Header · Sessions");
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.layout.right_open = true;
                h.layout.right_kind = Some(crate::layout::RightKind::Browser);
            })
        });
        let subtitle = vc.update(|_, cx| baaz.read(cx).feedback_note_subtitle(&target));
        assert_eq!(subtitle, "Button · in Header · Browser pane");
        restore_state(state);
    }

    /// Step coordinates parse strictly: garbage and off-phase pointing
    /// fail loudly instead of capturing a window where nothing happened.
    #[gpui::test]
    fn bad_step_coordinates_fail_loudly(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3-bad-steps");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        let failures_before = crate::steps::step_failures();
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("hover:nowhere", window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("drag:1,2,3", window, cx)));
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("note:", window, cx)));
        assert_eq!(
            crate::steps::step_failures(),
            failures_before + 4,
            "off-phase, unparseable and empty pointing subs all fail"
        );
        restore_state(state);
    }

    /// Pausing with steps queued fails them loudly under their own names
    /// instead of leaving them behind: their popovers will never open.
    #[gpui::test]
    fn pause_fails_its_queued_steps(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-pause-queue");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:20,21", window, cx)));
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| h.step_feedback("note:queued behind the capture", window, cx))
        });
        assert!(
            vc.update(|_, cx| !baaz.read(cx).feedback.step_queue.is_empty()),
            "the note waits behind the in-flight capture"
        );
        let failures_before = crate::steps::step_failures();
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.pause_feedback(window, cx)));
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Paused);
            assert!(harness.feedback.step_queue.is_empty(), "pause clears the queue");
        });
        assert_eq!(
            crate::steps::step_failures(),
            failures_before + 1,
            "the queued note fails loudly on pause"
        );
        assert!(
            crate::steps::step_failure_names().iter().any(|name| name.contains("queued behind the capture")),
            "the failure carries the queued step's own name"
        );
        restore_state(state);
    }

    /// A note keeps the screen it began on: begin Active, move to another
    /// mode before Save, and the filed note still names the original one.
    #[gpui::test]
    fn note_keeps_the_screen_it_began_on(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-stale-context");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        inject_hits(&baaz, vc, vec![f3_hit(10.0, 10.0, 100.0, 30.0, "Headers")]);
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                h.feedback_press_at(f3_point(20.0, 20.0), window, cx);
                h.feedback_release_at(f3_point(20.0, 20.0), window, cx);
            })
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).feedback.capture_pending.is_some()),
            "the click starts a capture"
        );
        // The owner switches screens before the capture lands.
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.mode = crate::layout::Mode::Browse;
            })
        });
        vc.update(|window, cx| {
            baaz.update(cx, |h, cx| {
                let seq = h.feedback.capture_seq;
                h.finish_note_capture(window, cx, seq);
                h.fill_feedback_note("kept words", window, cx);
                h.save_feedback_note(window, cx);
            })
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.note_count(), 1);
            assert_eq!(
                harness.feedback.draft.notes[0].context.mode, "Sessions",
                "the note keeps the screen it began on, not the switched-to one"
            );
        });
        restore_state(state);
    }

    /// The visual row count: a long single line wraps into several rows, a
    /// trailing newline counts, and empty text is one row — so the height
    /// estimate keeps Save on screen for wrapped text.
    #[gpui::test]
    fn visual_rows_count_wrapped_lines_and_trailing_newlines() {
        assert_eq!(FeedbackUi::feedback_visual_rows("hi"), 1);
        assert_eq!(FeedbackUi::feedback_visual_rows(""), 1, "empty text is one row");
        assert_eq!(FeedbackUi::feedback_visual_rows("a\n"), 2, "a trailing newline counts");
        assert_eq!(FeedbackUi::feedback_visual_rows("a\nb\n"), 3);
        let long = "x".repeat(100);
        assert_eq!(
            FeedbackUi::feedback_visual_rows(&long),
            3,
            "a 100-char single line wraps into 3 rows at ~36 chars each"
        );
        assert!(
            FeedbackUi::feedback_popover_height(FeedbackUi::feedback_visual_rows(&long))
                > FeedbackUi::feedback_popover_height(FeedbackUi::feedback_visual_rows("hi")),
            "wrapped text grows the popover"
        );
        assert_eq!(
            FeedbackUi::feedback_popover_height(FeedbackUi::feedback_visual_rows("a\n")),
            FeedbackUi::feedback_popover_height(3),
            "two visual rows still clamp to the field minimum"
        );
    }

    /// A hit covering more than 40 % of the window becomes a 60×40 area
    /// around the point: `near <label>` for a widget, `"web page"` for
    /// the page stand-in. Smaller hits stay elements.
    #[gpui::test]
    fn big_hits_become_nearby_areas() {
        let win = gpui::size(gpui::px(1000.0), gpui::px(800.0));
        let mut page = f3_hit(0.0, 0.0, 1000.0, 800.0, "Browse");
        page.role = "AXGroup".to_owned();
        match FeedbackUi::note_target_for_hit(page, f3_point(500.0, 400.0), win) {
            Target::Area { rect, region } => {
                assert_eq!(rect, Rect { x: 470.0, y: 380.0, w: 60.0, h: 40.0 });
                assert_eq!(region, Some("web page".to_owned()));
            }
            other => panic!("the page-sized group becomes a web-page area, got {other:?}"),
        }
        let big = f3_hit(0.0, 0.0, 1000.0, 800.0, "Canvas");
        match FeedbackUi::note_target_for_hit(big, f3_point(500.0, 400.0), win) {
            Target::Area { region, .. } => {
                assert_eq!(region, Some("near Canvas".to_owned()));
            }
            other => panic!("a page-sized widget becomes a nearby area, got {other:?}"),
        }
        let small = f3_hit(10.0, 10.0, 100.0, 30.0, "Headers");
        match FeedbackUi::note_target_for_hit(small, f3_point(20.0, 20.0), win) {
            Target::Element(hit) => assert_eq!(hit.label, "Headers"),
            other => panic!("a small hit stays an element, got {other:?}"),
        }
    }

    /// A click on a page-sized hit files a web-page area note end to end:
    /// the step handler, parked tasks plus painted frames, then the
    /// popover points at a 60×40 area with `region = "web page"`.
    #[gpui::test]
    fn click_on_a_page_sized_hit_files_a_web_page_area(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-page-hit");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        let mut page = f3_hit(0.0, 0.0, 2000.0, 2000.0, "Browse");
        page.role = "AXGroup".to_owned();
        inject_hits(&baaz, vc, vec![page]);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.step_feedback("click:200,200", window, cx)));
        drive_frames(vc);
        match f3_popover_target(&baaz, vc) {
            Some(Target::Area { rect, region }) => {
                assert_eq!(rect, Rect { x: 170.0, y: 180.0, w: 60.0, h: 40.0 });
                assert_eq!(region, Some("web page".to_owned()));
            }
            other => panic!("the page click files a web-page area, got {other:?}"),
        }
        restore_state(state);
    }

    /// Menu-bar dispatches die while Active and work again when Paused:
    /// ⌘N (and Settings…, the palette) dispatched the way the native menu
    /// does — straight into the tree, past the `NoAction` key rows.
    #[gpui::test]
    fn menu_actions_die_while_active_and_return_when_paused(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-menu-guard");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        let sessions_before = vc.update(|_, cx| baaz.read(cx).sessions.len());
        vc.update(|window, cx| {
            window.dispatch_action(Box::new(crate::app::NewSession), cx);
        });
        vc.update(|window, cx| {
            window.dispatch_action(Box::new(crate::app::OpenSettings), cx);
        });
        vc.update(|window, cx| {
            window.dispatch_action(Box::new(aui::keys::TogglePalette), cx);
        });
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.sessions.len(), sessions_before, "⌘N opens no session while Active");
            assert!(harness.active.is_none(), "⌘N opens no draft while Active");
            assert!(!harness.settings_page.open, "Settings… stays closed while Active");
            assert!(harness.overlays.read(cx).palette.is_none(), "the palette stays closed while Active");
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
        });
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.pause_feedback(window, cx)));
        vc.update(|window, cx| {
            window.dispatch_action(Box::new(crate::app::OpenSettings), cx);
        });
        assert!(
            vc.update(|_, cx| baaz.read(cx).settings_page.open),
            "Settings… works again when Paused"
        );
        restore_state(state);
    }

    /// The "Feedback mode is off" toast never goes stale: entering Active
    /// dismisses it by id.
    #[gpui::test]
    fn stale_off_toast_dismisses_on_entering_active(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-off-toast");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.toggle_feedback(window, cx)));
        assert!(
            vc.update(|_, cx| {
                baaz.read(cx)
                    .overlays
                    .read(cx)
                    .toasts
                    .iter()
                    .any(|t| t.title.to_string() == "Feedback mode is off")
            }),
            "the toast shows while the setting is off"
        );
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.layout.feedback_mode = true;
            })
        });
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            assert_eq!(harness.feedback.phase, FeedbackPhase::Active);
            assert!(
                !harness
                    .overlays
                    .read(cx)
                    .toasts
                    .iter()
                    .any(|t| t.title.to_string() == "Feedback mode is off"),
                "entering Active dismisses the stale toast"
            );
        });
        restore_state(state);
    }

    /// Session titles never show a raw id: a session with no title reads
    /// `New session`, through the same ↩ chip rule the Browse header
    /// wears — which is also what notes store.
    #[gpui::test]
    fn chip_session_name_never_shows_a_raw_id(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-chip-name");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let name = harness.browse_chip_session_name("local-draft-b0d3f00d", cx);
            assert_eq!(name, "New session", "a title-less session never reads as a raw id");
            assert!(!name.contains("local-draft"), "no raw id leaks into the name");
        });
        restore_state(state);
    }

    /// Done leaves a brief confirmation in the header slot: Off with a
    /// fresh flash renders it, so the save is visible even when a page
    /// covers the toast area.
    #[gpui::test]
    fn done_flash_lingers_in_the_header_slot(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("f3c-done-flash");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = window_harness(cx, &state.2);
        enter_active(&baaz, vc);
        vc.update(|_, cx| {
            baaz.update(cx, |h, _| {
                h.feedback.draft.add(
                    Target::Area {
                        rect: Rect { x: 8.0, y: 8.0, w: 64.0, h: 32.0 },
                        region: None,
                    },
                    "flash me".to_owned(),
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
            assert_eq!(harness.feedback.phase, FeedbackPhase::Off);
            assert!(harness.feedback.saved_flash.is_some(), "Done arms the header flash");
        });
        assert!(
            vc.update(|_, cx| baaz.update(cx, |h, cx| h.render_feedback_header_slot(cx).is_some())),
            "the fresh flash renders in the header slot"
        );
        restore_state(state);
    }
}
