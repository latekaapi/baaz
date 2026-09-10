//! `Overlays`: everything that floats above the window (spec §2.3).
//!
//! One entity, and it holds **state only**: which modal is up, which menu is
//! open and where its selection is, the toasts waiting to be shown, and the two
//! lists the menus are built from — the workspace's files and the CLI's skills,
//! both walked once at boot and both window-wide rather than per-session. The
//! elements themselves are rendered by whoever anchors them — the dialog and the
//! toast stack by [`crate::app::Harness`], the composer's chip menus and caret
//! popovers by [`crate::session::SessionView`], because the design rule is that
//! a picker is anchored to the chip that opened it and only the composer knows
//! where its chips are. Every one of them goes through
//! `aui::overlay::popover_layer`, so the paint order is still the single one the
//! library defines.
//!
//! Keeping the state here rather than in the two views is what makes "Escape
//! closes whatever is open, in order" one function instead of a negotiation.

use aui::feedback::{ToastAction, ToastData, ToastKind};
use aui::overlay::DialogKind;
use aui_protocol::{PermissionMode, ReasoningEffort};

/// One modal on screen. Only one at a time.
pub struct Dialog {
    /// Heading.
    pub title: String,
    /// Body paragraph.
    pub detail: String,
    /// Which tint the dialog wears.
    pub kind: DialogKind,
    /// The primary button's label.
    pub primary: &'static str,
    /// What the primary button does.
    pub action: DialogAction,
    /// When the action archives, the session it archives. The target lives on
    /// the dialog so dismissing it — Escape, the scrim, Cancel — drops the
    /// target with it and nothing can confirm afterwards.
    pub archive_target: Option<String>,
}

/// What a dialog's primary button does.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DialogAction {
    /// Close it and carry on.
    Dismiss,
    /// Respawn `muse serve` and resume.
    Reconnect,
    /// Go to the login screen.
    SignIn,
    /// Archive the dialog's `archive_target` out of the sidebar.
    Archive,
}

/// Which popover is open over the composer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuKind {
    /// The model picker, anchored to the model chip.
    Model,
    /// The reasoning-effort picker, anchored to the effort chip.
    Effort,
    /// The approval-mode picker, anchored to the mode chip.
    Mode,
    /// The `/` command menu, above the composer.
    Command,
    /// The `@` mention picker, above the composer.
    Mention,
    /// The header's overflow menu: Rename, Fork, Archive.
    Overflow,
    /// The Sessions caption's view menu: empty and archived filters.
    ViewOptions,
    /// The footer's account menu: Sign out.
    Account,
}

/// An open menu: which one, where the keyboard is, and what has been typed.
pub struct Menu {
    /// Which popover.
    pub kind: MenuKind,
    /// The highlighted row, across all sections in order.
    pub selected: usize,
    /// What has been typed after the `/` or the `@`. The chip pickers do not
    /// filter, so it stays empty for them.
    pub filter: String,
    /// Byte offset in the draft where the `/` or `@` sits, so the caret popovers
    /// know exactly what to replace when a row is picked.
    pub at: usize,
}

impl Menu {
    /// A chip picker with the selection on `selected`.
    pub fn picker(kind: MenuKind, selected: usize) -> Self {
        Self { kind, selected, filter: String::new(), at: 0 }
    }

    /// A caret popover that started at byte `at` in the draft.
    pub fn caret(kind: MenuKind, at: usize) -> Self {
        Self { kind, selected: 0, filter: String::new(), at }
    }
}

/// Which list the command palette is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PaletteKind {
    /// ⌘K: every `/` command and every session operation.
    Commands,
    /// `/resume`: the workspace's sessions, under the titles the sidebar shows.
    Resume,
    /// `/fork` with nothing named: the session's completed assistant turns,
    /// newest first, under the user prompt that started each one.
    Fork,
    /// `/search`, the sidebar search icon and Cmd+Shift+F: full-text matches
    /// over past sessions plus the files this workspace's turns created.
    Search,
}

/// The open command palette: which list, and where the keyboard is in it.
pub struct Palette {
    /// Which list.
    pub kind: PaletteKind,
    /// The highlighted row, across all sections in order.
    pub selected: usize,
}

/// The floating state of the window.
#[derive(Default)]
pub struct Overlays {
    /// The modal, if any.
    pub dialog: Option<Dialog>,
    /// The open popover, if any.
    pub menu: Option<Menu>,
    /// The open command palette, if any.
    pub palette: Option<Palette>,
    /// Toasts, oldest first.
    pub toasts: Vec<ToastData>,
    /// The `/` menu's **Skills** section, from `muse skills list --json`.
    pub skills: Vec<crate::skills::Skill>,
    /// The `@` picker's candidates: the workspace's files, relative to it,
    /// lowercased once at walk time (see [`crate::files::FileEntry`]).
    pub files: Vec<crate::files::FileEntry>,
    /// Monotonic id source, so two identical toasts are still two toasts.
    next_toast: u64,
}

impl Overlays {
    /// Close whatever Escape should close, innermost first. Returns `true` when
    /// something was closed, which is what tells the caller not to interrupt the
    /// turn as well.
    pub fn close_topmost(&mut self) -> bool {
        if self.menu.take().is_some() {
            return true;
        }
        if self.palette.take().is_some() {
            return true;
        }
        self.dialog.take().is_some()
    }

    /// Move the palette's selection by `delta` within `count`, wrapping.
    pub fn move_palette(&mut self, delta: isize, count: usize) {
        let Some(palette) = self.palette.as_mut() else { return };
        if count == 0 {
            palette.selected = 0;
            return;
        }
        let count = count as isize;
        palette.selected = (((palette.selected as isize + delta) % count + count) % count) as usize;
    }

    /// Open a menu, replacing whatever was open.
    pub fn open(&mut self, menu: Menu) {
        self.menu = Some(menu);
    }

    /// Whether `kind` is the open menu.
    pub fn is_open(&self, kind: MenuKind) -> bool {
        self.menu.as_ref().is_some_and(|m| m.kind == kind)
    }

    /// Move the selection by `delta` rows within `count`, wrapping.
    pub fn move_selection(&mut self, delta: isize, count: usize) {
        let Some(menu) = self.menu.as_mut() else { return };
        if count == 0 {
            menu.selected = 0;
            return;
        }
        let count = count as isize;
        menu.selected = (((menu.selected as isize + delta) % count + count) % count) as usize;
    }

    /// Show a note. Toasts are informational here — a compaction that did
    /// nothing, a command this build does not have — so they carry no action.
    pub fn toast(&mut self, title: impl Into<String>, body: impl Into<String>) {
        self.next_toast += 1;
        let id = format!("toast-{}", self.next_toast);
        self.toasts.push(ToastData::new(id, title.into(), body.into()).kind(ToastKind::Neutral));
    }

    /// Show a note carrying one action, and return the toast's id.
    ///
    /// The one action that exists is Undo, and it exists because hiding a
    /// session is the only thing in this window that takes something away from
    /// the list without asking first.
    pub fn toast_with_action(
        &mut self,
        title: impl Into<String>,
        body: impl Into<String>,
        action: impl Into<String>,
    ) -> String {
        self.next_toast += 1;
        let id = format!("toast-{}", self.next_toast);
        let label = action.into();
        self.toasts.push(
            ToastData::new(id.clone(), title.into(), body.into())
                .kind(ToastKind::Neutral)
                .action(ToastAction::new(label.to_lowercase(), label).primary()),
        );
        id
    }

    /// Drop the toast with this id.
    pub fn dismiss_toast(&mut self, id: &str) {
        self.toasts.retain(|t| t.id != id);
    }
}

// ---------------------------------------------------------------------------
// The client-side slash commands (spec §3.10)
// ---------------------------------------------------------------------------

/// One entry of the `/` menu's **Commands** section.
///
/// Every one of these is client-side: MSP has no command plane for them, and
/// the TUI's own `/` palette is a TUI feature. Three of them are named by the
/// spec but belong to later phases; they stay in the list — a command that
/// vanished would read as a command that does not exist — and say so when
/// picked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    /// Open the model picker.
    Model,
    /// Open the effort picker.
    Effort,
    /// Open the approval-mode picker.
    Mode,
    /// Toggle plan mode.
    Plan,
    /// `session/compact`.
    Compact,
    /// Fork the session — Phase 4.
    Fork,
    /// Rename the session (`sessions.json`, not the wire).
    Name,
    /// Resume another session — the command palette over `session/list`.
    Resume,
    /// Search past sessions and created files.
    Search,
    /// Hide this session from the sidebar.
    Hide,
    /// Show sessions with no turns in the sidebar again.
    Empty,
    /// The session status dialog.
    Status,
    /// The session usage dialog (the same dialog as `/status`).
    Usage,
    /// Start a new session in this workspace.
    Clear,
    /// `muse logout`.
    Logout,
    /// Show this menu, unfiltered.
    Help,
}

impl Command {
    /// Every command, in the order the menu lists them.
    pub const ALL: [Command; 16] = [
        Command::Model,
        Command::Effort,
        Command::Mode,
        Command::Plan,
        Command::Compact,
        Command::Status,
        Command::Usage,
        Command::Clear,
        Command::Fork,
        Command::Name,
        Command::Resume,
        Command::Search,
        Command::Hide,
        Command::Empty,
        Command::Logout,
        Command::Help,
    ];

    /// The command as typed, leading slash included.
    pub fn slash(&self) -> &'static str {
        match self {
            Command::Model => "/model",
            Command::Effort => "/effort",
            Command::Mode => "/mode",
            Command::Plan => "/plan",
            Command::Compact => "/compact",
            Command::Fork => "/fork",
            Command::Name => "/name",
            Command::Resume => "/resume",
            Command::Search => "/search",
            Command::Hide => "/hide",
            Command::Empty => "/empty",
            Command::Status => "/status",
            Command::Usage => "/usage",
            Command::Clear => "/clear",
            Command::Logout => "/logout",
            Command::Help => "/help",
        }
    }

    /// What the row says it does.
    pub fn description(&self) -> &'static str {
        match self {
            Command::Model => "Choose the model",
            Command::Effort => "Set the reasoning effort",
            Command::Mode => "Set the approval mode",
            Command::Plan => "Plan first, then approve",
            Command::Compact => "Summarize the conversation to free up context",
            Command::Fork => "Branch this session from an earlier turn",
            Command::Name => "Show or rename this session",
            Command::Resume => "Resume an earlier session",
            Command::Search => "Search sessions and created files",
            Command::Hide => "Hide this session from the sidebar",
            Command::Empty => "Show sessions with no turns in the sidebar",
            Command::Status => "Show current session status",
            Command::Usage => "Show session usage",
            Command::Clear => "Start a new session in this workspace",
            Command::Logout => "Log out and forget the saved login",
            Command::Help => "Show every command",
        }
    }

    /// Whether this build actually does it. The three that do not still appear.
    pub fn available(&self) -> bool {
        true
    }

    /// The phase that would bring a command this build does not have.
    ///
    /// Nothing is unavailable any more — `/fork` landed in Phase 4, `/name`
    /// and `/resume` in Phase 5 — so this is the empty sentence the toast
    /// would have carried. The pair is kept because a build that grows a
    /// command before it grows the code should say so rather than do nothing.
    pub fn coming_in(&self) -> &'static str {
        ""
    }

    /// Parse a typed slash command.
    pub fn parse(text: &str) -> Option<Command> {
        Command::ALL.into_iter().find(|c| c.slash() == text)
    }

    /// Parse a whole typed line into a command and whatever followed it.
    ///
    /// `"/name Fix the parser"` is `(Name, "Fix the parser")`, `"/status"` is
    /// `(Status, "")`, and anything that is not a command is `None` — which is
    /// what keeps a prompt beginning with a slash a prompt.
    pub fn parse_line(text: &str) -> Option<(Command, &str)> {
        let line = text.trim_end();
        if !line.starts_with('/') || line.lines().count() > 1 {
            return None;
        }
        let (head, rest) = match line.split_once(' ') {
            Some((head, rest)) => (head, rest.trim_start()),
            None => (line, ""),
        };
        Command::parse(head).map(|command| (command, rest))
    }
}

/// The effort tiers the picker offers: MSP's whole closed enum, including the
/// `max` tier muse 1.1.1 added between `xhigh` and `ultra`.
pub const EFFORTS: [Option<ReasoningEffort>; 9] = [
    None,
    Some(ReasoningEffort::None),
    Some(ReasoningEffort::Minimal),
    Some(ReasoningEffort::Low),
    Some(ReasoningEffort::Medium),
    Some(ReasoningEffort::High),
    Some(ReasoningEffort::Xhigh),
    Some(ReasoningEffort::Max),
    Some(ReasoningEffort::Ultra),
];

/// The label for an effort slot; `None` is "Default", which omits the field.
pub fn effort_label(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "Default",
        Some(effort) => effort.label(),
    }
}

/// The one-line description under an effort row.
pub fn effort_detail(effort: Option<ReasoningEffort>) -> &'static str {
    match effort {
        None => "Leave it to the provider; the field is omitted.",
        Some(ReasoningEffort::None) => "No reasoning at all.",
        Some(ReasoningEffort::Minimal) => "The smallest budget the provider offers.",
        Some(ReasoningEffort::Low) => "A short budget.",
        Some(ReasoningEffort::Medium) => "The middle budget, and the usual default.",
        Some(ReasoningEffort::High) => "A long budget.",
        Some(ReasoningEffort::Xhigh) => "Longer than high.",
        Some(ReasoningEffort::Max) => "Longer than extra high, below ultra.",
        Some(ReasoningEffort::Ultra) => "The largest budget MSP accepts.",
    }
}

/// The four approval modes, in the order the picker lists them (spec §3.6).
pub const MODES: [PermissionMode; 4] = [
    PermissionMode::AllowAll,
    PermissionMode::OnRequest,
    PermissionMode::PromptUnmatched,
    PermissionMode::DenyUnmatched,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_closes_the_menu_before_the_dialog() {
        let mut overlays = Overlays {
            dialog: Some(Dialog {
                title: "x".into(),
                detail: "y".into(),
                kind: DialogKind::Error,
                primary: "Dismiss",
                action: DialogAction::Dismiss,
                archive_target: None,
            }),
            ..Default::default()
        };
        overlays.open(Menu::picker(MenuKind::Model, 0));
        assert!(overlays.close_topmost());
        assert!(overlays.menu.is_none() && overlays.dialog.is_some());
        assert!(overlays.close_topmost());
        assert!(!overlays.close_topmost());
    }

    #[test]
    fn the_selection_wraps_both_ways() {
        let mut overlays = Overlays::default();
        overlays.open(Menu::picker(MenuKind::Effort, 0));
        overlays.move_selection(-1, 3);
        assert_eq!(overlays.menu.as_ref().unwrap().selected, 2);
        overlays.move_selection(1, 3);
        assert_eq!(overlays.menu.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn an_empty_menu_keeps_the_selection_at_zero() {
        let mut overlays = Overlays::default();
        overlays.open(Menu::caret(MenuKind::Command, 0));
        overlays.move_selection(1, 0);
        assert_eq!(overlays.menu.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn every_command_parses_back_from_its_slash() {
        for command in Command::ALL {
            assert_eq!(Command::parse(command.slash()), Some(command));
        }
        assert_eq!(Command::parse("/nope"), None);
    }
}
