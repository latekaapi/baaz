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
use gpui::{div, prelude::*, px, AnyElement, Context, FocusHandle, Window};
use gpui_kit::base::{h_flex, v_flex};

use crate::feedback::{Draft, ElementProbe, FakeProbe, MacProbe, ScreenContext};
use crate::wire::WireCall as _;

/// The key context the capture layer carries while Active, so Escape
/// reaches [`crate::app::FeedbackEscape`] instead of the composer and
/// typing never lands in a field behind the layer.
pub const FEEDBACK_CONTEXT: &str = "BaazFeedback";

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

/// The feedback shell's state: one per window, on `Harness`.
pub struct FeedbackUi {
    /// What the shell shows.
    pub phase: FeedbackPhase,
    /// The notes taken so far this session.
    pub draft: Draft,
    /// Hit-tests the window; [`MacProbe`] in the app, [`FakeProbe`] for
    /// captures and tests.
    pub probe: Box<dyn ElementProbe>,
    /// A first Discard click is waiting for its confirm.
    pub discard_armed: bool,
    /// When the arm started, for the 4 s window.
    pub discard_armed_at: Option<Instant>,
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
            discard_armed: false,
            discard_armed_at: None,
            focus: cx.focus_handle(),
        }
    }

    /// The probe for this run: scripted hits under captures/tests, the
    /// real macOS probe in the app.
    pub fn probe_for(fake: bool) -> Box<dyn ElementProbe> {
        if fake {
            Box::new(FakeProbe { hits: Vec::new() })
        } else {
            Box::new(MacProbe)
        }
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

    /// Drop a stale arm: older than [`DISCARD_ARM`].
    pub fn expire_discard(&mut self) {
        if self.discard_armed
            && self.discard_armed_at.is_some_and(|at| at.elapsed() > DISCARD_ARM)
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

    /// Done is clickable in Active or Paused with notes to save.
    pub fn done_enabled(&self) -> bool {
        self.phase != FeedbackPhase::Off && !self.draft.is_empty()
    }

    /// ⌃⌥⌘F / the menu item: setting off stays Off; Off → Active (a fresh
    /// draft when empty, the kept one otherwise); Active → Paused;
    /// Paused → Active. Pure — the caller runs the side effects the
    /// outcome names (toasts, probe activation, focus).
    pub fn toggle(&mut self, setting_on: bool) -> ToggleOutcome {
        if !setting_on {
            return ToggleOutcome::NeedsSetting;
        }
        match self.phase {
            FeedbackPhase::Off => {
                if self.draft.is_empty() {
                    self.draft = Draft::new();
                }
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::EnteredActive
            }
            FeedbackPhase::Active => {
                self.disarm_discard();
                self.phase = FeedbackPhase::Paused;
                ToggleOutcome::Paused
            }
            FeedbackPhase::Paused => {
                self.disarm_discard();
                self.phase = FeedbackPhase::Active;
                ToggleOutcome::Resumed
            }
        }
    }

    /// Escape, the Pause button: Active → Paused, anywhere else a no-op.
    pub fn pause(&mut self) {
        if self.phase == FeedbackPhase::Active {
            self.disarm_discard();
            self.phase = FeedbackPhase::Paused;
        }
    }

    /// The Resume button / `resume` verb: Paused → Active, else a no-op.
    pub fn resume(&mut self) {
        if self.phase == FeedbackPhase::Paused {
            self.disarm_discard();
            self.phase = FeedbackPhase::Active;
        }
    }

    /// Turning the setting off mid-session: Off, but the draft survives
    /// so the next toggle resumes it.
    pub fn setting_off(&mut self) {
        if self.phase != FeedbackPhase::Off {
            self.disarm_discard();
            self.phase = FeedbackPhase::Off;
        }
    }

    /// A Discard click: disabled with 0 notes; the first click arms (the
    /// button reads `Discard N notes?` for up to 4 s); the second click
    /// inside the window clears the draft, staying in the current phase.
    /// A stale arm reads as a first click again.
    pub fn discard_press(&mut self) -> DiscardOutcome {
        if self.draft.is_empty() {
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

    /// Discard arm + confirm in one go, for the `discard` step verb.
    pub fn discard_all(&mut self) {
        self.draft.discard();
        self.disarm_discard();
    }

    /// A finished Done write: Off with a cleared draft.
    pub fn done_applied(&mut self) {
        self.disarm_discard();
        self.phase = FeedbackPhase::Off;
        self.draft = Draft::new();
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
/// white on light's.
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

impl crate::app::Harness {
    /// ⌃⌥⌘F / Help → Feedback Mode: setting off toasts and stays Off;
    /// otherwise the toggle outcome, focusing the layer and priming the
    /// probe on the way into Active.
    pub(crate) fn toggle_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// On the way into Active: touch the a11y tree so F3 can hit-test on
    /// a later frame, take focus so typing never reaches the composer,
    /// and repaint.
    pub(crate) fn enter_feedback_active(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

    /// The bar's Pause button.
    pub(crate) fn pause_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.feedback.pause();
        window.focus(&self.focus_root, cx);
        cx.notify();
    }

    /// The pill's Resume button.
    pub(crate) fn resume_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.feedback.phase == FeedbackPhase::Paused {
            self.feedback.resume();
            self.enter_feedback_active(window, cx);
        }
    }

    /// The bar's Discard button: arm, then clear on the confirm. A stale
    /// arm disarms itself on a timer so the label falls back.
    pub(crate) fn discard_feedback(&mut self, cx: &mut Context<Self>) {
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
                self.wire_tasks().push(task);
                cx.notify();
            }
            DiscardOutcome::Cleared => {
                cx.notify();
            }
        }
    }

    /// The Done button (Active or Paused, needs notes): write the report
    /// off the UI thread, then back on it copy the report dir path to the
    /// clipboard, toast it, and go Off with a cleared draft. A write
    /// error toasts instead and keeps the draft and the phase.
    pub(crate) fn finish_feedback(&mut self, cx: &mut Context<Self>) {
        if !self.feedback.done_enabled() {
            return;
        }
        let draft = self.feedback.draft.clone();
        let root = crate::feedback::feedback_root();
        let build = feedback_build_string();
        let task = cx.spawn(async move |this, cx| {
            let written = cx
                .background_executor()
                .spawn(async move {
                    crate::feedback::write_report(&root, &draft, chrono::Local::now(), &build)
                })
                .await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                match written {
                    Ok(dir) => {
                        let path = dir.display().to_string();
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(path.clone()));
                        let count = this.feedback.note_count();
                        let unit = if count == 1 { "note" } else { "notes" };
                        this.feedback.done_applied();
                        this.overlays.update(cx, |o, _| {
                            o.toast(format!("Feedback saved · {count} {unit}"), path)
                        });
                    }
                    Err(err) => {
                        this.overlays.update(cx, |o, _| {
                            o.toast("Couldn't save feedback", format!("{err:#}"))
                        });
                    }
                }
                cx.notify();
            });
        });
        self.wire_tasks().push(task);
    }

    /// The setting flipped off mid-session: the shell goes Off but keeps
    /// the draft, so the next toggle resumes it.
    pub(crate) fn feedback_setting_off(&mut self, cx: &mut Context<Self>) {
        self.feedback.setting_off();
        cx.notify();
    }

    /// `feedback:<sub>`: `on` turns the setting on for this run (in
    /// memory, never `layout.json` — a capture aid) and enters Active;
    /// `pause` / `resume` move between Active and Paused; `off` clears the
    /// run's setting and parks Off keeping the draft; `discard` clears the
    /// draft at once; `done` saves like the Done button. Idempotent where
    /// it can be (`on` while Active stays Active). Any other payload is
    /// the session verb's `feedback:<text>`, forwarded to the open
    /// session's field; with no session open it records a step failure.
    pub(crate) fn step_feedback(&mut self, rest: &str, window: &mut Window, cx: &mut Context<Self>) {
        match rest.trim() {
            "on" => {
                self.layout.feedback_mode = true;
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
                self.layout.feedback_mode = false;
                self.feedback.setting_off();
                cx.notify();
            }
            "discard" => {
                self.feedback.discard_all();
                cx.notify();
            }
            "done" => self.finish_feedback(cx),
            _ => {
                if let Some(view) = self.active.clone() {
                    view.update(cx, |v, cx| v.step_feedback(rest, window, cx));
                } else {
                    crate::steps::record_step_failure(&format!("feedback:{rest}"));
                }
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
            scale: f32::from(window.scale_factor()),
            theme: if cx.aui().kind.is_dark() { "dark".to_owned() } else { "light".to_owned() },
        }
    }

    /// The capture layer while Active: nothing when Off or Paused. One
    /// element, built here so F3 finds every pointer trap in one place.
    pub(crate) fn render_feedback_layer(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.feedback.phase != FeedbackPhase::Active {
            return None;
        }
        self.feedback.expire_discard();
        let accent = feedback_accent(cx);
        // F3 hook: hover/click/drag note-taking handlers attach to this
        // layer, beside the swallowing ones below — never scattered
        // across the app.
        let layer = div()
            .id("feedback-layer")
            .absolute()
            .inset_0()
            .border_2()
            .border_color(accent)
            .cursor(gpui::CursorStyle::Crosshair)
            .occlude()
            .key_context(FEEDBACK_CONTEXT)
            .track_focus(&self.feedback.focus)
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(gpui::MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .on_mouse_up(gpui::MouseButton::Middle, |_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation());
        Some(layer.into_any_element())
    }

    /// The floating chrome: the bar while Active, the pill while Paused,
    /// nothing while Off.
    pub(crate) fn render_feedback_chrome(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match self.feedback.phase {
            FeedbackPhase::Off => None,
            FeedbackPhase::Active => Some(self.render_feedback_bar(cx)),
            FeedbackPhase::Paused => Some(self.render_feedback_pill(cx)),
        }
    }

    /// The Active bar, top-centre: dot · Feedback · N notes · help ·
    /// Pause · Discard · Done (two lines: the primary button over its
    /// caption). Above the capture layer, below toasts.
    fn render_feedback_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.feedback.expire_discard();
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let notes = self.feedback.notes_label();
        let discard_label = self.feedback.discard_label();
        let discard_enabled = self.feedback.discard_enabled();
        let done_enabled = self.feedback.done_enabled();
        let pause = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.pause_feedback(window, cx);
        });
        let discard = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.discard_feedback(cx);
        });
        let done = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.finish_feedback(cx);
        });
        let bar = h_flex()
            .id("feedback-bar")
            .role(gpui::Role::Group)
            .aria_label("Feedback mode")
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
            .child(
                v_flex()
                    .flex_none()
                    .gap(px(2.0))
                    .items_center()
                    .child(
                        button("feedback-done", "Done")
                            .primary()
                            .disabled(!done_enabled)
                            .accessibility_label("Done — save feedback report")
                            .on_click(move |_, window, cx| done(&(), window, cx)),
                    )
                    .child(
                        div()
                            .ui(scale::FS_12)
                            .text_color(p.ink_3)
                            .child("Saves the report and copies its path"),
                    ),
            );
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
    /// Done disabled).
    fn render_feedback_pill(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let accent = feedback_accent(cx);
        let notes = self.feedback.notes_label();
        let done_enabled = self.feedback.done_enabled();
        let resume = cx.listener(|this: &mut Self, _: &(), window, cx| {
            this.resume_feedback(window, cx);
        });
        let done = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.finish_feedback(cx);
        });
        let pill = h_flex()
            .id("feedback-pill")
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
            .child(
                button("feedback-done", "Done")
                    .primary()
                    .disabled(!done_enabled)
                    .accessibility_label("Done — save feedback report")
                    .on_click(move |_, window, cx| done(&(), window, cx)),
            );
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
    use crate::feedback::{Rect, Target};

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
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.finish_feedback(cx)));
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
}
