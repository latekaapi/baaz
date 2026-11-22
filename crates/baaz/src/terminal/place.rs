//! Where the terminal sits, and the right pane's lower pane (docs/82 8.1):
//! the root's side of both.
//!
//! The terminal is one [`TerminalDock`](super::TerminalDock) drawn in one of
//! two places ([`TerminalPlace`]): the dock under the composer, or the right
//! pane's Terminal view. The place persists in `layout.json`; whether the
//! pane shows the terminal is the pane's own state, which persists per
//! session. The rules — which gesture opens what, where a moved terminal
//! lands, what the pane shows when it leaves — are the layout model's
//! ([`crate::layout::Layout`]'s `seat_terminal_in_pane`,
//! `dock_terminal_bottom`, `toggle_terminal`, `set_lower`); these methods
//! apply them and do what only the root can: persist the session's pane,
//! re-read its data, and hand the terminal the keyboard.

use gpui::{Context, Window};

use crate::app::Harness;
use crate::layout::{LayoutGlobal as _, RightKind, TerminalPlace};

impl Harness {
    /// The pane changed: the session's record takes it, and the pane
    /// re-reads what it now shows.
    fn pane_moved(&mut self, cx: &mut Context<Self>) {
        crate::layout::Layout::save(cx);
        self.save_right_for_active(cx);
        self.refresh_right_now(cx);
    }

    /// ⌃` (and the centre header's terminal button, the palette's
    /// Terminal): the terminal goes or comes in whichever place it sits
    /// ([`crate::layout::Layout::toggle_terminal`]). Shown, it takes the
    /// keyboard and its first tab opens (`dock.opened`). While the Settings
    /// page stands open this is ignored with no state change (B11b): the
    /// page hides the dock and the pane without writing their state, and a
    /// toggle would otherwise flip them invisibly.
    pub(crate) fn toggle_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            return;
        }
        let place = crate::layout::Layout::get(cx).terminal_place;
        let shown = crate::layout::Layout::stage(cx, |layout| layout.toggle_terminal());
        if shown {
            self.with_terminal_dock(cx, |dock, cx| dock.opened(window, cx));
        }
        if place == TerminalPlace::Pane {
            self.pane_moved(cx);
        } else {
            crate::layout::Layout::save(cx);
        }
    }

    /// Show the terminal where it sits, for a person's gesture that needs it
    /// (a new terminal, a Tasks row's tab, the scripted tab): the dock opens,
    /// or the pane seats it ([`crate::layout::Layout::seat_terminal_in_pane`]).
    /// The caller focuses the tab it opens.
    pub(crate) fn show_terminal(&mut self, cx: &mut Context<Self>) {
        match crate::layout::Layout::get(cx).terminal_place {
            TerminalPlace::Bottom => {
                crate::layout::Layout::stage(cx, |layout| layout.terminal_open = true);
                crate::layout::Layout::save(cx);
            }
            TerminalPlace::Pane => {
                if crate::layout::Layout::get(cx).terminal_in_pane() {
                    return;
                }
                crate::layout::Layout::stage(cx, |layout| layout.seat_terminal_in_pane());
                self.pane_moved(cx);
            }
        }
    }

    /// The agent ran something in the terminal: show it, never focused, so
    /// the person's keyboard stays where it was while they watch. At the
    /// bottom the dock opens; in the pane it opens below what the person is
    /// looking at ([`crate::layout::Layout::reveal_terminal_in_pane`]).
    pub(crate) fn reveal_terminal_for_agent(&mut self, cx: &mut Context<Self>) {
        match crate::layout::Layout::get(cx).terminal_place {
            TerminalPlace::Bottom => crate::layout::Layout::update(cx, |layout| layout.terminal_open = true),
            TerminalPlace::Pane => {
                if crate::layout::Layout::get(cx).terminal_in_pane() || self.settings_open(cx) {
                    return;
                }
                crate::layout::Layout::stage(cx, |layout| layout.reveal_terminal_in_pane());
                self.pane_moved(cx);
            }
        }
    }

    /// Move the terminal to `place` (the dock's "Move terminal to the right
    /// pane", the pane's "Dock terminal at the bottom", `terminal-place:`):
    /// the target opens, the source closes, and the same tabs and ptys
    /// carry over — nothing restarts. The terminal takes the keyboard.
    /// Ignored while the Settings page stands open (B11b).
    pub(crate) fn move_terminal(&mut self, place: TerminalPlace, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_open(cx) {
            return;
        }
        crate::layout::Layout::stage(cx, |layout| match place {
            TerminalPlace::Pane => layout.seat_terminal_in_pane(),
            TerminalPlace::Bottom => layout.dock_terminal_bottom(),
        });
        crate::baaz_log!("terminal moved: {place:?}");
        self.with_terminal_dock(cx, |dock, cx| dock.opened(window, cx));
        self.pane_moved(cx);
    }

    /// Stack `kind` under the pane's top view, opening the pane, or close
    /// the lower pane (`None`): the views menu's "below" rows, the lower
    /// pane's ✕, `right-lower:`. A refusal (Browse, the Page, the kind on
    /// top, the Settings page) changes nothing and says why. The terminal
    /// below takes the keyboard like it does anywhere it opens.
    pub(crate) fn set_right_lower(&mut self, kind: Option<RightKind>, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.settings_open(cx) {
            return Err("the Settings page is open".into());
        }
        crate::layout::Layout::stage(cx, |layout| {
            let was_open = layout.right_open;
            if kind.is_some() {
                layout.right_open = true;
            }
            let result = layout.set_lower(kind);
            if result.is_err() {
                layout.right_open = was_open;
            }
            result
        })?;
        if kind == Some(RightKind::Terminal) {
            self.with_terminal_dock(cx, |dock, cx| dock.opened(window, cx));
        }
        self.pane_moved(cx);
        Ok(())
    }

    /// Settle the lower pane's divider at `share` of the pane's height
    /// (held to its range), as a released drag does: written, and the
    /// session's record takes it.
    #[cfg(feature = "probe")]
    pub(crate) fn set_right_split(&mut self, share: f32, cx: &mut Context<Self>) {
        let share = crate::layout::clamp_split(share);
        crate::layout::Layout::update(cx, |layout| layout.right_split = Some(share));
        self.save_right_for_active(cx);
    }
}
