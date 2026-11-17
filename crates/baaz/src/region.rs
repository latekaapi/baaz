//! The region contract (task 4.3, audit D Q1 rows 1–2, Q5 #1): what a screen
//! region of the window is to the root that seats it.
//!
//! The window is a fixed set of slots (`aui::shell::app_shell`: sidebar,
//! centre, right column, the dock under the centre). A region is a gpui
//! entity that fills one of them and owns everything only it reads: its
//! caches, its background work, its focus handle, its menus, the actions
//! only it handles. The root composes the region's view and talks to it
//! through explicit calls and the region's own typed events — never by
//! reaching into its fields.
//!
//! The shape follows Zed's dock panels (`workspace/src/dock.rs`, read for the
//! shape only): a region is `Focusable + Render`, and three questions are all
//! the root asks of every region alike:
//!
//! * [`Region::title`] — what the region is called now, for its labels;
//! * [`Region::actions`] — the controls it contributes to its slot's 44 px
//!   header row (the right pane's tab row; a dock's tabs and buttons; a
//!   page's crumb and close), built in the region's own context so every
//!   control's handler runs on the region;
//! * [`Region::covers_page`] — whether it draws its own pixels where the
//!   native Browse page sits this frame. The CEF page is a child window
//!   drawn above everything gpui paints, so a region that covers it makes
//!   the root hide the page (or show its still picture) instead.
//!
//! The right pane ([`crate::right_pane::RightPane`]) is the first region;
//! the terminal dock ([`crate::terminal::TerminalDock`]), the Skills page
//! ([`crate::skills_page::SkillsPage`]), the Settings page
//! ([`crate::settings::SettingsPage`]) and the connect screen
//! ([`crate::connect::ConnectScreen`]) followed (plan 4.4).

use gpui::{AnyElement, App, Context, Focusable, Render, SharedString, Window};

/// One region of the window: a view entity the root seats in a shell slot.
pub(crate) trait Region: Focusable + Render + Sized {
    /// The region's name as it stands now — "Changes", "Files", "Page" for
    /// the right pane — for the labels the region and its slot carry.
    fn title(&self, cx: &App) -> SharedString;

    /// The controls the region contributes to its slot's header row, built
    /// in its own context (their handlers run on the region). `None` when
    /// the region puts nothing there.
    fn actions(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement>;

    /// Whether the region draws over the native Browse page this frame, so
    /// the root must hide the live page under it.
    fn covers_page(&self, cx: &App) -> bool;
}
