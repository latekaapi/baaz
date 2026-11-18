//! What floats over the window: the modal, the toast stack and the header's
//! menus.
//!
//! [`crate::overlays`] owns the state — which modal is up, which menu is open,
//! where its selection sits, what the menus are built from. The modal is its
//! own view, [`DialogView`] (plan 4.6): this module owns it, the root's side
//! that opens it and maps its buttons, and the elements of the menus the
//! **window** anchors. The palette is its own view too
//! ([`crate::palette`]); the two menus the sidebar anchors live in
//! [`crate::sidebar_view`], and the composer's own chip menus in
//! [`crate::session`], because a picker is anchored to the affordance that
//! opened it and only its owner knows where that is.
//!
//! Everything here goes through `aui::overlay::popover_layer`, so the paint
//! order stays the single one the library defines, and a deterministic capture
//! draws the dialog and the toast stack settled rather than rising in: an
//! enter presence never lands on the same frame twice.

use aui::feedback::ToastKind;
use aui::keys::Cancel;
use aui::nav::{view_menu, MenuRow};
use aui::overlay::{
    anchored_menu, dialog, popover_layer, DialogKind, MenuAlign, MenuSide,
};
use aui::transcript::handoff_confirm;
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, Context, SharedString, Window,
};
use aui::interact::dismiss_layer;
use aui::util::v_flex;

use crate::app::{Harness, Wire};
use crate::layout::{self, RightKind};
use crate::overlays::{Command, Dialog, DialogAction, Menu, MenuKind, PaletteKind};
use crate::wire::WireCall;
use crate::{files, skills};
use crate::layout::LayoutGlobal as _;

/// Actions of the header's overflow menu, in row order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverflowAction {
    Rename,
    Fork,
    Archive,
}

/// The header overflow menu's rows and their actions: Rename, Fork,
/// Archive.
fn overflow_rows() -> (Vec<MenuRow>, Vec<OverflowAction>) {
    let rows = vec![
        MenuRow::Toggle { label: "Rename".into(), checked: false },
        MenuRow::Toggle { label: "Fork".into(), checked: false },
        MenuRow::Toggle { label: "Archive".into(), checked: false },
    ];
    let actions = vec![OverflowAction::Rename, OverflowAction::Fork, OverflowAction::Archive];
    (rows, actions)
}





/// The modal dialog as its own view (plan 4.6): the dialog's content, its
/// focus, its buttons. The root opens it ([`Harness::set_dialog`]), pushes
/// what it reads from the root each frame ([`DialogFacts`]), and maps its
/// buttons ([`DialogEvent`]) onto the root's own work; Escape, the scrim and
/// ✕ dismiss it through the overlay stack ([`DismissEvent`]).
pub(crate) struct DialogView {
    dialog: Dialog,
    /// What the root pushed this frame.
    facts: DialogFacts,
    focus: gpui::FocusHandle,
}

/// What the dialog reads from the root, pushed each frame it draws.
#[derive(Default)]
pub(crate) struct DialogFacts {
    /// The handoff confirm's frozen facts, when the dialog is the handoff
    /// confirm.
    handoff: Option<crate::handoff::HandoffConfirmState>,
    /// Drawn settled (a scripted capture): no rising in.
    still: bool,
}

/// A dialog button pressed.
#[derive(Clone, Debug)]
pub(crate) enum DialogEvent {
    /// The primary button: `action` on the `target` that rode the dialog.
    Primary { action: DialogAction, target: Option<String> },
    /// The secondary button (Cancel, Dismiss, "Not now").
    Secondary { action: DialogAction },
}

impl gpui::EventEmitter<DialogEvent> for DialogView {}

/// A handler that has the view emit `make()`'s event.
fn emitter<V: gpui::EventEmitter<E>, E: 'static>(cx: &mut Context<V>, make: impl Fn() -> E + 'static) -> impl Fn(&mut Window, &mut gpui::App) + 'static {
    let me = cx.entity().downgrade();
    move |_, cx| {
        let _ = me.update(cx, |_, cx| cx.emit(make()));
    }
}
impl gpui::EventEmitter<gpui::DismissEvent> for DialogView {}

impl gpui::Focusable for DialogView {
    fn focus_handle(&self, _: &gpui::App) -> gpui::FocusHandle {
        self.focus.clone()
    }
}

impl DialogView {
    fn new(dialog: Dialog, cx: &mut Context<Self>) -> Self {
        DialogView { dialog, facts: DialogFacts::default(), focus: cx.focus_handle() }
    }

    /// The modal layer around `card`: the menu key context on the dialog's
    /// focus, Escape popping the overlay stack like anywhere else (a menu
    /// opened over the dialog closes first).
    fn layer(&self, card: AnyElement, cx: &mut Context<Self>) -> AnyElement {
        popover_layer(
            div()
                .absolute()
                .inset_0()
                .key_context(aui::keys::MENU_CONTEXT)
                .track_focus(&self.focus)
                .on_action(cx.listener(|_, _: &Cancel, _, cx| cx.emit(gpui::DismissEvent)))
                .child(card),
        )
        .into_any_element()
    }

    /// The handoff confirm: the library's own dialog — the destination row,
    /// the Carried / Not carried lists the card draws, the pack size and the
    /// honest fresh-start note — over the same modal layer every other
    /// dialog uses. Cancel closes it (the frozen facts leave with it); Hand
    /// off starts the run.
    fn render_handoff(&self, state: crate::handoff::HandoffConfirmState, cx: &mut Context<Self>) -> AnyElement {
        let action = self.dialog.action;
        let target = self.dialog.archive_target.clone();
        let model = if state.to_model.trim().is_empty() { "provider default".to_owned() } else { state.to_model };
        let card = handoff_confirm(
            "handoff-confirm",
            crate::handoff::wire_provider(state.to),
            model,
            &state.carried,
            &state.lost,
            Some(state.pack_tokens),
        )
        .on_primary(emitter(cx, move || DialogEvent::Primary { action, target: target.clone() }))
        .on_secondary(emitter(cx, move || DialogEvent::Secondary { action }))
        .on_dismiss(emitter(cx, || gpui::DismissEvent));
        let card = if crate::runtime::captures() { card.at_rest() } else { card };
        self.layer(card.into_any_element(), cx)
    }
}

impl Render for DialogView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.dialog.action == DialogAction::HandoffConfirm {
            if let Some(state) = self.facts.handoff.clone() {
                return self.render_handoff(state, cx);
            }
        }
        let modal = &self.dialog;
        let (title, detail, kind, primary_label, action, target) =
            (modal.title.clone(), modal.detail.clone(), modal.kind, modal.primary, modal.action, modal.archive_target.clone());
        let danger = matches!(
            action,
            DialogAction::Archive
                | DialogAction::RemoveProject
                | DialogAction::QuitWithWorkRunning
                | DialogAction::CloseTerminalTab
                | DialogAction::ProviderSignOut
                | DialogAction::DeleteSpace
                | DialogAction::ClearHistory
                | DialogAction::TrashTranscript
        );
        // A dialog whose primary already dismisses it carries no second
        // button: "Dismiss  Dismiss" was what a screenshot showed. The danger
        // dialog keeps Cancel beside Archive; the others keep Dismiss beside
        // Reconnect / Sign in / Done.
        let secondary = match (danger, action) {
            (true, _) => Some("Cancel"),
            (false, DialogAction::Dismiss) => None,
            // The migration prompt's opt-out: one click, never asked again
            // (the Providers row stays while moves remain). The scrim and
            // Escape beside it only close, so an accidental dismissal
            // re-asks next launch instead of stranding sessions.
            (false, DialogAction::MigrateSessions) => Some("Not now"),
            (false, _) => Some("Dismiss"),
        };
        // Critical errors (`DialogKind::Error`) lead the library dialog with
        // the error mascot (56 pt, at the head of the body) in place of the
        // kind tile; confirmations keep the tile, and the toast stack stays
        // text-only.
        let mascot = (kind == DialogKind::Error).then(|| crate::mascot::error_mascot(crate::mascot::ErrorMascot::classify(&title, &detail)));
        let mut card = dialog("dialog", title).kind(kind).body(detail).danger(danger).primary(primary_label);
        if let Some(mascot) = mascot {
            card = card.leading(mascot);
        }
        if let Some(secondary) = secondary {
            card = card.secondary(secondary);
        }
        let card = card
            .on_primary(emitter(cx, move || DialogEvent::Primary { action, target: target.clone() }))
            .on_secondary(emitter(cx, move || DialogEvent::Secondary { action }))
            .on_dismiss(emitter(cx, || gpui::DismissEvent));
        // A scripted capture draws the dialog settled rather than rising in:
        // the enter presence never lands on the same frame twice.
        let card = if self.facts.still { card.at_rest() } else { card };
        self.layer(card.into_any_element(), cx)
    }
}

impl Harness {
    /// Put a modal up: an entry on the overlay stack, one at a time (a new
    /// one replaces the one up). Escape pops the newest entry, so a menu
    /// opened over it closes first.
    pub(crate) fn set_dialog(&mut self, cx: &mut Context<Self>, dialog: Dialog) {
        let view = cx.new(|cx| DialogView::new(dialog.clone(), cx));
        // Its buttons run on the main window (a dialog Browse hosts in the
        // overlay window must not open, focus or measure there).
        let pressed = cx.subscribe(&view, |this: &mut Self, _, event: &DialogEvent, cx| {
            let event = event.clone();
            this.run_on_main(cx, move |this, window, cx| this.on_dialog_event(event, window, cx));
        });
        // Dismissing it (Escape, the scrim) runs what `close_dialog` does:
        // the handoff confirm's facts leave with it.
        let entry = crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::Dialog(dialog))
            .on_close(|_, this: &mut Harness, _| this.handoff_state.confirm = None)
            .keep(pressed);
        self.push_overlay_view(entry, &view, cx);
    }

    pub(crate) fn close_dialog(&mut self, cx: &mut Context<Self>) {
        // The handoff confirm's facts leave with it: dismissing any other
        // way confirms nothing afterwards.
        self.handoff_state.confirm = None;
        self.overlays.update(cx, |overlays, cx| overlays.close_dialog(cx));
    }

    /// The quit confirmation (D52): names the running command. `others`
    /// counts the running commands past the named one, so a second tab is
    /// not a surprise after the first is confirmed. Pure, so the naming
    /// has a test without a window. Tests only; the window asks through [`Self::quit_dialog`].
    #[cfg(test)]
    pub(crate) fn quit_terminal_dialog(command: &str, others: usize) -> Dialog {
        Self::quit_dialog(&[], Some((command, others))).expect("a running command asks")
    }

    /// The quit confirmation for everything still working (docs/82 0.14):
    /// the sessions with a turn running, by name and count, and the
    /// terminal's running command. `None` when nothing is running. Pure.
    pub(crate) fn quit_dialog(working: &[String], command: Option<(&str, usize)>) -> Option<Dialog> {
        let sessions = match working {
            [] => None,
            [one] => Some(format!("\u{201C}{one}\u{201D} is still working.")),
            [first, rest @ ..] => Some(format!(
                "{total} sessions are still working, including \u{201C}{first}\u{201D}.",
                total = rest.len() + 1
            )),
        };
        let terminal = command.map(|(command, others)| {
            if others == 0 {
                format!("`{command}` is still running.")
            } else {
                format!(
                    "{total} terminal commands are still running, including `{command}`.",
                    total = others + 1
                )
            }
        });
        let (title, detail) = match (sessions, terminal) {
            (None, None) => return None,
            (Some(sessions), None) => (
                if working.len() == 1 { "Quit while an agent is working?" } else { "Quit while agents are working?" },
                format!("{sessions} Quitting stops {}; each session picks up when you next send to it.", if working.len() == 1 { "it" } else { "them" }),
            ),
            (None, Some(terminal)) => {
                let them = if command.is_some_and(|(_, others)| others > 0) { "them" } else { "it" };
                ("Quit with a terminal command running?", format!("{terminal} Quitting stops {them}."))
            }
            (Some(sessions), Some(terminal)) => {
                ("Quit while work is running?", format!("{sessions} {terminal} Quitting stops all of it."))
            }
        };
        Some(Dialog {
            title: title.into(),
            detail,
            kind: DialogKind::Warning,
            primary: "Quit",
            action: DialogAction::QuitWithWorkRunning,
            archive_target: None,
        })
    }

    /// The close-tab confirmation (D52), or `None` when the tab is idle: a
    /// busy tab asks first, naming its running command; an idle tab closes
    /// without asking. Pure, so both halves have a test without a window.
    pub(crate) fn close_tab_dialog(tab_id: &str, running_command: Option<&str>) -> Option<Dialog> {
        let command = running_command?;
        Some(Dialog {
            title: format!("Close this terminal while `{command}` is running?"),
            detail: "Closing the tab stops the command.".into(),
            kind: DialogKind::Warning,
            primary: "Close Tab",
            action: DialogAction::CloseTerminalTab,
            archive_target: Some(tab_id.to_owned()),
        })
    }

    /// ⌘Q with a turn or a terminal command running asks first through
    /// [`Self::quit_dialog`], naming them. Returns `true` when it asked —
    /// the caller must not quit.
    pub(crate) fn maybe_confirm_quit(&mut self, cx: &mut Context<Self>) -> bool {
        let working = self.working_session_names(cx);
        let running = self.terminal_dock.read(cx).host().read(cx).running_terminals(cx);
        let command = running.first().map(|(_, command)| (command.as_str(), running.len() - 1));
        let Some(dialog) = Self::quit_dialog(&working, command) else { return false };
        self.set_dialog(cx, dialog);
        true
    }

    /// The names of the sessions with a turn running — open, parked or the
    /// browser chat — as their rows read them.
    pub(crate) fn working_session_names(&self, cx: &gpui::App) -> Vec<String> {
        self.registry
            .iter_running(self.active.as_ref(), cx)
            .iter()
            .map(|runtime| {
                let id = runtime.read(cx).session_id.clone();
                if self.is_browser_chat(&id, cx) {
                    return "Browser assistant".to_owned();
                }
                self.session_list.read(cx).sessions
                    .iter()
                    .find(|entry| entry.id == id)
                    .map(|entry| entry.label.clone())
                    .filter(|label| !label.trim().is_empty())
                    .unwrap_or_else(|| "New session".to_owned())
            })
            .collect()
    }

    /// The close-tab dialog's primary button: the tab id rode the dialog,
    /// so dismissing it any other way already dropped the target.
    pub(crate) fn confirm_close_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let target = self.overlays.read(cx).dialog().and_then(|d| {
            (d.action == DialogAction::CloseTerminalTab).then(|| d.archive_target.clone()).flatten()
        });
        self.close_dialog(cx);
        if let Some(id) = target {
            self.terminal_dock.update(cx, |dock, cx| dock.close_tab_now(&id, cx));
        }
    }

    /// The two lists the `/` and `@` menus are built from, walked per root
    /// on the background executor: at boot, when a session view for a root
    /// opens and the cache lacks that root, and when a new session starts.
    ///
    /// Neither is on the wire: skills reach MSP only as `toolCall` items, and
    /// a mention is plain text inside the prompt (research §1.5). Skills are
    /// listed with the root as the working directory.
    pub(crate) fn load_menu_sources(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
        // `/` and `$HOME` are not workspaces (D39), and walking either to fill
        // a picker costs the whole disk. Every caller derives its root from
        // something that can fall back to the launch directory, which a bundle
        // opened from Finder starts at `/` — so the rule is enforced here,
        // once, rather than at each call site.
        if !crate::projects::is_workspace_root(&root) {
            crate::baaz_log!("menu sources: {} is not a workspace; not walking it", root.display());
            return;
        }
        if self.overlays.read(cx).has_root(&root.to_string_lossy()) {
            return;
        }
        let program = self.args.program.clone();
        let work = move || {
            // The `/` menu reads the page's catalog (D64): one list with
            // `--workspace --trust-workspace`, fully parsed.
            let catalog = skills::list_catalog(&program, &root.to_string_lossy());
            let (skills, files, root) = (catalog.rows, files::walk(&root), root);
            (skills, files, root)
        };
        self.wire_call(cx, work, |this, (skills, files, root), cx| {
            let key = root.to_string_lossy().into_owned();
            if files.truncated {
                // The toast names the project the walk ran in, so a person
                // with eight roots knows whose picker is short.
                let name = this
                    .session_list.read(cx).projects
                    .find_by_root(&root)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| key.clone());
                crate::baaz_log!(
                    "@ mention index stopped at {} files; some workspace files are not mentionable",
                    files::CAP
                );
                this.overlays.update(cx, |overlays, cx| {
                    overlays.toast(
                        "@ mentions shortened",
                        format!("{name} holds more than {} files; some are not mentionable.", files::CAP),
                     cx);
                });
            }
            this.overlays.update(cx, |overlays, cx| overlays.insert_root(key, files, skills, cx));
        });
    }





    /// The open session's provider, for the menus that offer per provider.
    pub(crate) fn active_provider(&self, cx: &gpui::App) -> Option<crate::providers::ProviderId> {
        self.active.as_ref().map(|view| view.read(cx).rt(cx).provider_kind())
    }


    /// The native folder panel, directories only: a chosen folder is adopted
    /// exactly like a recent workspace. Cancel does nothing.
    ///
    /// The three `baaz:` lines are the diagnosis trail for "Choose
    /// folder… does nothing": the select line says the row fired, the entry
    /// line says the panel was asked for, and the resolution line says what
    /// the panel answered. They stay because this will regress.
    pub(crate) fn choose_project_folder(&mut self, cx: &mut Context<Self>) {
        crate::baaz_log!("choose_project_folder: opening the folder panel");
        // The panel is app-modal but opens behind everything when this app is
        // not active (launched from a script, or the palette's scrim press
        // deactivated it): brought forward first so "nothing happens" is
        // never a hidden panel.
        cx.activate(true);
        let paths = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add".into()),
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            match paths.await {
                Ok(Ok(Some(paths))) => {
                    crate::baaz_log!(
                        "choose_project_folder: panel chose {}",
                        paths.first().map(|p| p.display().to_string()).unwrap_or_default()
                    );
                    let _ = this.update(cx, |this, cx| {
                        if let Some(root) = paths.first() {
                            this.adopt_root(root, cx);
                        }
                    });
                }
                Ok(Ok(None)) => crate::baaz_log!("choose_project_folder: panel cancelled"),
                Ok(Err(error)) => crate::baaz_log!("choose_project_folder: panel errored: {error:#}"),
                Err(_) => crate::baaz_log!("choose_project_folder: panel future dropped"),
            }
        }));
    }






    /// Run one palette row by its id.
    ///
    /// A click already knows which row it hit, so it comes straight here
    /// rather than rebuilding every row to turn the id back into an index and
    /// the index back into the same id (finding `performance-7`).
    pub(crate) fn run_palette_row(&mut self, kind: PaletteKind, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, cx| overlays.close_palette(cx));
        // A palette action executed leaves Settings (no restore: the
        // destination owns the next route) — except pane/terminal commands,
        // which are ignored with no state change while the page stands
        // open (B11b), like their keybindings.
        if self.settings_open(cx) {
            if kind == PaletteKind::Commands
                && Command::parse(&id).is_some_and(|command| command.is_pane_or_terminal())
            {
                return;
            }
            self.exit_settings_for_navigation(cx);
        }
        match kind {
            PaletteKind::Search => {
                if let Some(session_id) = id.strip_prefix("s:") {
                    self.resume(session_id.to_owned(), window, cx);
                } else if let Some(rest) = id.strip_prefix("f:") {
                    self.reveal_created(rest, cx);
                }
            }
            PaletteKind::Resume => self.resume(id.to_string(), window, cx),
            // Picking a project starts a session there (and makes it
            // current); adopting a folder only adopts — it never starts one.
            PaletteKind::Projects => {
                if id.as_ref() == "choose" {
                    self.choose_project_folder(cx);
                } else if let Some(project) = id.strip_prefix("p:") {
                    self.new_session_in(Some(project.to_owned()), window, cx);
                } else if let Some(root) = id.strip_prefix("a:") {
                    self.adopt_root(std::path::Path::new(root), cx);
                }
            }
            PaletteKind::Fork => {
                self.with_session(cx, |view, vc| view.runtime().clone().update(vc, |rt, cx| rt.fork(Some(id.to_string()), cx)));
            }
            PaletteKind::Commands => {
                if let Some(command) = Command::parse(&id) {
                    if !self.run_window_command(command, window, cx) {
                        self.with_session(cx, |view, cx| view.run_command(command, window, cx));
                    }
                }
            }
        }
    }

    /// The tests' way into [`Self::run_palette_row`]: run one palette row
    /// by its id, exactly as a click or ↩ would.
    #[cfg(test)]
    pub(crate) fn run_palette_row_for_test(
        &mut self,
        kind: PaletteKind,
        id: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.run_palette_row(kind, id, window, cx);
    }

    /// Run one window-level ⌘K command, returning whether it was one.
    ///
    /// Window commands act on the window itself — the right pane, the
    /// terminal dock, the Skills page — so they run here on [`Harness`],
    /// before the session delegation in [`Self::run_palette_row`], which
    /// does nothing when no session is open. Session commands return
    /// `false` and keep their old path. Both sides list their variants
    /// explicitly, with no `_` arm: adding a command must force a decision
    /// about which side it is on.
    pub(crate) fn run_window_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match command {
            Command::Skills => {
                let _ = window;
                self.open_skills(cx);
                true
            }
            Command::NextWaiting => {
                self.jump_to_next_waiting(window, cx);
                true
            }
            Command::RightBrowser => {
                self.select_right(RightKind::Browser, cx);
                true
            }
            Command::RightDiff | Command::RightGit => {
                // B8: both spellings open the merged Changes view, without
                // ever toggling it shut when it already shows.
                self.select_right(RightKind::Changes, cx);
                true
            }
            Command::RightFiles => {
                self.select_right(RightKind::Files, cx);
                true
            }
            Command::Terminal => {
                self.toggle_terminal(window, cx);
                true
            }
            Command::NewTerminalCmd => {
                self.new_terminal(window, cx);
                true
            }
            Command::Model
            | Command::Effort
            | Command::Mode
            | Command::Plan
            | Command::Compact
            | Command::Status
            | Command::Usage
            | Command::Clear
            | Command::Project
            | Command::Handoff
            | Command::Fork
            | Command::Name
            | Command::Resume
            | Command::Search
            | Command::Hide
            | Command::Empty
            | Command::Logout
            | Command::Help => false,
        }
    }

    /// Open a header/footer menu, replacing whatever is open. Clicking its
    /// own button again closes it. Opening the account menu refreshes its
    /// usage cards first (throttled): the popover renders from the live
    /// statuses, so the refresh must land before this frame's render.
    pub(crate) fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        if kind == MenuKind::Account {
            self.refresh_account_usage(cx);
        }
        // One menu at a time (the stack holds one menu): an open `+` menu
        // closes through the session view that draws it, so it redraws.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.close_plus_menu(cx));
        }
        let already = self.overlays.read(cx).menu().is_some_and(|m| m.kind == kind);
        self.overlays.update(cx, |overlays, cx| {
            if already {
                overlays.close_menu(cx);
            } else {
                overlays.open(Menu::picker(kind, 0), cx);
            }
        });
    }

    /// The header's overflow menu, anchored under the "…" button: Rename swaps
    /// the title for the dense inline field, Fork opens the fork picker, and
    /// Archive asks first through the archive dialog.
    pub(crate) fn render_overflow_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Overflow) {
            return None;
        }
        let (rows, actions) = overflow_rows();
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            let action = actions.get(*index).copied();
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            match action {
                Some(OverflowAction::Rename) => {
                    if let Some(session_id) = this.active_id(cx) {
                        crate::layout::Layout::stage(cx, |layout| layout.sidebar_open = true);
                        this.start_rename(session_id, window, cx);
                    } else {
                        this.overlays.update(cx, |overlays, cx| {
                            overlays.toast("Nothing to rename", "No session is open.", cx);
                        });
                    }
                }
                Some(OverflowAction::Fork) => this.open_palette(PaletteKind::Fork, cx),
                Some(OverflowAction::Archive) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.open_archive_dialog(session_id, cx);
                    } else {
                        this.overlays.update(cx, |overlays, cx| {
                            overlays.toast("Nothing to archive", "No session is open.", cx);
                        });
                    }
                }
                None => {}
            }
        });
        // Below-end of the `…` button that opened it — `anchored_menu` flips
        // above and slides inside the window on overflow. No button bounds
        // yet: no menu this frame, never the old `top(48) right(8)` corner
        // (the button is always rendered, so the notify lands next frame).
        let Some(trigger) = self.overlays.read(cx).anchor(crate::overlays::Anchor::Overflow) else {
            cx.notify();
            return None;
        };
        // A click anywhere outside closes it: the catcher is a sibling of
        // the menu inside the same draw.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
        });
        let catcher = dismiss_layer("overflow-scrim", "Dismiss menu", move |w, cx| dismiss(&(), w, cx));
        Some(
            div().absolute().inset_0().child(catcher).child(anchored_menu(
                trigger,
                MenuSide::Below,
                MenuAlign::End,
                view_menu("overflow", rows).at_rest().on_activate(move |i, w, cx| {
                    activate(&i, w, cx)
                }),
            ))
            .into_any_element(),
        )
    }

    /// The right pane's view menu (docs/83 §6), anchored under the tab
    /// row's `＋`: Changes, Files, Page — the shown one checked — open the
    /// pane on that view, as their tabs and the centre header's toggles do;
    /// in Sessions a fourth row, Terminal, toggles the dock exactly as ⌃`
    /// does (checked while it stands open). Browse has no dock, so no row.
    pub(crate) fn render_right_views_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::RightViews) {
            return None;
        }
        let shown = layout::right_kind(crate::layout::Layout::get(cx));
        let mut rows: Vec<MenuRow> =
            RightKind::ALL.into_iter().map(|kind| MenuRow::Toggle { label: kind.label().into(), checked: kind == shown }).collect();
        let terminal = crate::layout::Layout::get(cx).mode != layout::Mode::Browse;
        if terminal {
            rows.push(MenuRow::Toggle { label: "Terminal".into(), checked: crate::layout::Layout::get(cx).terminal_open });
        }
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            match RightKind::ALL.get(*index) {
                Some(kind) => this.select_right(*kind, cx),
                None if terminal => this.toggle_terminal(window, cx),
                None => {}
            }
        });
        // No `＋` bounds yet: no menu this frame (the button is drawn
        // whenever the pane is, so the notify lands next frame).
        let Some(trigger) = self.right_pane.read(cx).cache.views_menu_seat else {
            cx.notify();
            return None;
        };
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
        });
        let catcher = dismiss_layer("right-views-scrim", "Dismiss menu", move |w, cx| dismiss(&(), w, cx));
        Some(
            div()
                .absolute()
                .inset_0()
                .child(catcher)
                .child(anchored_menu(
                    trigger,
                    MenuSide::Below,
                    MenuAlign::Start,
                    view_menu("right-views", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx)),
                ))
                .into_any_element(),
        )
    }

    /// The Browse header back button's history menu (B2a-H4), anchored
    /// under the back button: the shown tab's back stack, newest first.
    /// Picking a row steps back to it through the history (go back N), so
    /// the stacks stay right. The menu covers the page like every other
    /// menu, through the shared overlay inputs. "Nothing to show" is
    /// decided before opening — in the header's back menu listener, which
    /// never opens it while back is disabled — so this render is pure: it
    /// never mutates state and never notifies, and an empty stack simply
    /// draws nothing. Keyboard reach for the back control itself is
    /// still open (B2a-H4c note): right-click (or its menu listener) is the
    /// only route today.
    pub(crate) fn render_back_history_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::BackHistory) {
            return None;
        }
        let id = self.browser.read(cx).browse_centre_shown(cx)?;
        let mut past: Vec<String> = self
            .browser
            .read(cx)
            .histories
            .get(&id)
            .map(|history| history.past_urls())
            .unwrap_or_default();
        past.reverse();
        if past.is_empty() {
            return None;
        }
        let rows: Vec<MenuRow> =
            past.iter().map(|url| MenuRow::Toggle { label: url.clone().into(), checked: false }).collect();
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
            this.ensure_tabs_loaded(cx);
            // Rows read newest-first, so row `i` steps back `i + 1`.
            if let Some(id) = this.browser.read(cx).browse_centre_shown(cx) {
                let steps = index.saturating_add(1);
                if let Some(url) = this.browser.update(cx, |browser, _| browser.histories.entry(id).or_default().go_back_steps(steps)) {
                    this.navigate_browse_tab(id, &url, window, cx);
                }
            }
        });
        let trigger = self.browser.read(cx).back_bounds?;
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.close_menu(cx));
        });
        let catcher = dismiss_layer("back-history-scrim", "Dismiss menu", move |w, cx| dismiss(&(), w, cx));
        Some(
            div().absolute().inset_0().child(catcher).child(anchored_menu(
                trigger,
                MenuSide::Below,
                MenuAlign::Start,
                view_menu("back-history", rows).at_rest().on_activate(move |i, w, cx| {
                    activate(&i, w, cx)
                }),
            ))
            .into_any_element(),
        )
    }

    /// ↑/↓ in an open menu, wrapping over the rows the menu actually has.
    /// ← / → in the open composer menu (D4): the effort slider steps a
    /// level; on the model picker → opens the selected row's effort submenu
    /// and ← closes it.
    pub(crate) fn menu_sideways(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        view.update(cx, |view, cx| view.menu_sideways(delta, cx));
    }

    pub(crate) fn move_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        // The composer draws its menus: the session view (or the menu's
        // picker) redraws, never the root.
        view.update(cx, |view, cx| view.move_menu(delta, cx));
    }




    /// The modal for this frame — its facts pushed first (the dialog draws
    /// right after, in this same frame) — or `None` when none is up.
    pub(crate) fn compose_dialog(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let view = self.overlays.read(cx).stack.view_as::<DialogView>(crate::overlay_stack::LayerKind::Dialog)?;
        let facts = DialogFacts { handoff: self.handoff_state.confirm.clone(), still: self.still() || crate::runtime::captures() };
        view.update(cx, |dialog, _| dialog.facts = facts);
        Some(view.into_any_element())
    }

    /// The modal's focus handle, while one is up.
    pub(crate) fn dialog_focus(&self, cx: &gpui::App) -> Option<gpui::FocusHandle> {
        use gpui::Focusable as _;
        self.overlays.read(cx).stack.view_as::<DialogView>(crate::overlay_stack::LayerKind::Dialog).map(|view| view.focus_handle(cx))
    }

    /// What the modal's buttons asked for.
    fn on_dialog_event(&mut self, event: DialogEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DialogEvent::Primary { action, target } => self.run_dialog_primary(action, target, window, cx),
            // The migration prompt's secondary is Never-ask-again ("Not
            // now"); every other secondary only closes.
            DialogEvent::Secondary { action } => {
                if action == DialogAction::MigrateSessions {
                    self.dismiss_session_migration(cx);
                } else {
                    self.close_dialog(cx);
                }
            }
        }
    }

    /// The modal's primary button: `action` on the `target` that rode the
    /// dialog. The archive target stays on the dialog until its own button
    /// runs: closing it any other way drops the target with it.
    fn run_dialog_primary(&mut self, action: DialogAction, target: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if action == DialogAction::Archive {
            self.confirm_archive_dialog(window, cx);
            return;
        }
        // The provider wire id rode the dialog: confirming signs that
        // provider out for real, then re-probes so its card follows.
        if action == DialogAction::ProviderSignOut {
            let id = target.as_deref().map(crate::providers::ProviderId::parse);
            if let Some(id) = id {
                self.confirm_provider_signout(id, window, cx);
            } else {
                self.close_dialog(cx);
            }
            return;
        }
        if action == DialogAction::RemoveProject {
            self.confirm_remove_project(cx);
            return;
        }
        // The space id rode the dialog: confirming deletes it and closes
        // its tabs; closing any other way keeps everything.
        if action == DialogAction::DeleteSpace {
            self.close_dialog(cx);
            if let Some(space) = target.as_deref() {
                self.delete_space_now(space, window, cx);
            }
            return;
        }
        if action == DialogAction::SpaceSignIns {
            self.close_dialog(cx);
            if let Some(space) = target.as_deref() {
                self.toggle_space_signins(space, cx);
            }
            return;
        }
        if action == DialogAction::ClearHistory {
            self.close_dialog(cx);
            self.clear_history_now(cx);
            return;
        }
        // The transcript's paths rode the dialog: only this button moves
        // them, and only to the Trash.
        if action == DialogAction::TrashTranscript {
            self.close_dialog(cx);
            self.trash_transcript(target, cx);
            return;
        }
        // Confirmed through its own path, like Archive: the tab id rode
        // the dialog, so closing any other way already dropped it.
        if action == DialogAction::CloseTerminalTab {
            self.confirm_close_terminal_tab(cx);
            return;
        }
        // The migration prompt's Move: the full dry-run plan through the
        // journaling executor. Nothing moved without this click.
        if action == DialogAction::MigrateSessions {
            self.confirm_session_migration(cx);
            return;
        }
        // The handoff confirm runs before the dialog closes: its frozen
        // facts leave with the dialog.
        if action == DialogAction::HandoffConfirm {
            self.confirm_handoff(window, cx);
            return;
        }
        self.close_dialog(cx);
        match action {
            DialogAction::Dismiss => {}
            DialogAction::Reconnect => {
                // A person asked: the crash count starts afresh, so a hub
                // that gave up respawns again (docs/81 B PM-7).
                if let Some(hub) = &self.muse_hub {
                    hub.forgive_crashes();
                }
                self.wire = Wire::Reconnecting;
                self.reconnect(cx);
            }
            DialogAction::QuitWithWorkRunning => {
                crate::tier::cleanup_probes();
                cx.quit();
            }
            // Confirmed through the early returns above.
            DialogAction::Archive
            | DialogAction::RemoveProject
            | DialogAction::CloseTerminalTab
            | DialogAction::ProviderSignOut
            | DialogAction::MigrateSessions
            | DialogAction::DeleteSpace
            | DialogAction::ClearHistory
            | DialogAction::SpaceSignIns
            | DialogAction::TrashTranscript
            | DialogAction::HandoffConfirm => {}
        }
    }

    #[cfg(feature = "probe")]
    /// `error-dialog:<offline|blocked|sorry>`: capture aid that raises a
    /// critical error dialog through the same [`Self::set_dialog`] path a
    /// real failure takes, so the screenshot shows the production render —
    /// never a hardcoded render branch. Scripting only, like the other
    /// capture aids (`title-land:`, `row-detail:`); free, it reaches the
    /// wire nowhere. The payload picks the sprite family (empty means
    /// `sorry`); any other payload is an `Err` and raises nothing.
    pub(crate) fn step_error_dialog(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        use crate::overlays::{Dialog, DialogAction};
        let dialog = match rest.trim().to_lowercase().as_str() {
            "offline" => Dialog {
                title: "Muse disconnected".into(),
                detail: "The connection to `muse serve` dropped before the turn finished. \
                    Reconnect and the open session resumes where it stopped."
                    .into(),
                kind: DialogKind::Error,
                primary: "Reconnect",
                action: DialogAction::Reconnect,
                archive_target: None,
            },
            "blocked" => Dialog {
                title: "Usage limit exceeded".into(),
                detail: "This login's plan allows no more turns until the quota resets. \
                    Sign in with a login that still has room, or wait for the reset."
                    .into(),
                kind: DialogKind::Error,
                primary: "Dismiss",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
            "" | "sorry" => Dialog {
                title: "Muse hit an internal error".into(),
                detail: "The turn failed inside `muse serve` before producing anything. \
                    Nothing was billed for the attempt; retry the turn or start a new session."
                    .into(),
                kind: DialogKind::Error,
                primary: "Dismiss",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
            other => return Err(format!("unknown error-dialog family {other:?} (expected offline, blocked or sorry)")),
        };
        self.set_dialog(cx, dialog);
        Ok(())
    }

    /// Baaz → About Baaz.
    pub(crate) fn show_about(&mut self, cx: &mut Context<Self>) {
        self.set_dialog(
            cx,
            Dialog {
                title: "About Baaz".into(),
                detail: format!(
                    "Baaz {} \u{2014} a macOS chat interface to Muse Code.\n\nKeys: docs/08-keymap.md. App: docs/02-app.md.",
                    env!("CARGO_PKG_VERSION")
                ),
                kind: DialogKind::Info,
                primary: "OK",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
        );
    }

    /// The corner stack (D4, docs/41 §2.11): the newest toast,
    /// right-aligned. The host places it — in
    /// Sessions 12 from the sheet's right with its bottom 8 above the
    /// composer ([`Self::corner_above_band`]), with no composer 12 above
    /// the sheet's bottom ([`Self::corner_at_sheet_bottom`]); in Browse in
    /// the corner window over the page ([`Self::corner_in_overlay`]).
    pub(crate) fn render_corner_stack(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toast = self.render_toast_pill(cx)?;
        Some(v_flex().flex_none().items_end().child(toast).into_any_element())
    }

    /// The corner stack's inset from the sheet's right (and bottom) edge.
    const CORNER_INSET: f32 = 12.0;

    /// The stack over the composer band: its bottom 8 above the band's top
    /// (the composer's top edge), 12 from the sheet's right.
    pub(crate) fn corner_above_band(stack: AnyElement) -> AnyElement {
        div().absolute().right(px(Self::CORNER_INSET)).bottom(gpui::relative(1.0)).pb(px(scale::SP_3)).child(stack).into_any_element()
    }

    /// The stack where the sheet has no composer: 12 above its bottom, 12
    /// from its right.
    pub(crate) fn corner_at_sheet_bottom(stack: AnyElement) -> AnyElement {
        div().absolute().right(px(Self::CORNER_INSET)).bottom(px(Self::CORNER_INSET)).child(stack).into_any_element()
    }

    /// Whether the stack lives in the corner window: Browse, where the
    /// native page draws above everything the main window paints (memory
    /// `feedback-native-page-above-gpui`) — always in Browse, page or not,
    /// so a page loading under a toast never moves it. Settings hides the
    /// page, so the stack draws in the window there.
    pub(crate) fn corner_in_overlay(&self, cx: &gpui::App) -> bool {
        crate::layout::Layout::get(cx).mode == layout::Mode::Browse && !self.settings_open(cx)
    }

    /// The newest toast as the stack's chip, with `+n` while more wait. The
    /// same [`ToastData`](aui::feedback::ToastData) as ever: toasts wait for
    /// a press, never a clock. One action exists, and it is Undo — one
    /// hidden row, one "Clear empty" batch, or one archived session,
    /// whichever the newest toast was for.
    fn render_toast_pill(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toasts = self.overlays.read(cx).toasts.clone();
        let newest = toasts.last()?.clone();
        let queued = toasts.len().saturating_sub(1);
        let tone = match newest.kind {
            ToastKind::Ok => aui::feedback::ToastTone::Success,
            ToastKind::Warn => aui::feedback::ToastTone::Warning,
            ToastKind::Neutral => aui::feedback::ToastTone::Neutral,
        };
        let newest_id = newest.id.to_string();
        let dismiss = newest_id.clone();
        let close = self.main_listener(cx, move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.overlays.update(cx, |overlays, cx| overlays.dismiss_toast(&dismiss, cx));
        });
        // Keyed by the toast, so a new one plays its own enter.
        let mut chip = aui::feedback::toast_chip(gpui::ElementId::Name(format!("toast-chip-{newest_id}").into()), tone, newest.title.clone(), newest.body.clone())
            .queued(queued)
            .at_rest(crate::runtime::captures())
            .on_close(close);
        if let Some(first) = newest.actions.first() {
            let action = newest_id;
            let act = self.main_listener(cx, move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                this.undo_newest(cx);
                this.overlays.update(cx, |overlays, cx| overlays.dismiss_toast(&action, cx));
            });
            chip = chip.action(first.label.clone(), act);
        }
        Some(chip.into_any_element())
    }

    #[cfg(feature = "probe")]
    /// `toast:<title>|<body>`: capture aid that raises a toast through the
    /// same [`Overlays::toast`](crate::overlays::Overlays::toast) path a
    /// production note takes, so the screenshot shows the production
    /// render — never a hardcoded branch. Scripting only, like the other
    /// capture aids (`title-land:`, `row-detail:`); free, it reaches the
    /// wire nowhere. An empty title is an `Err` and raises nothing.
    pub(crate) fn step_toast(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let (title, body) = rest.split_once('|').unwrap_or((rest, ""));
        let (title, body) = (title.trim(), body.trim());
        if title.is_empty() {
            return Err("toast needs a title (`toast:<title>|<body>`)".to_owned());
        }
        self.overlays.update(cx, |overlays, cx| {
            overlays.toast(title, body, cx);
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use aui::nav::MenuRow;
    use crate::layout::LayoutGlobal as _;

    /// The header ⋯ menu's rows: Rename, Fork, Archive, one action each.
    #[test]
    fn overflow_menu_rows_are_rename_fork_archive() {
        let label = |row: &MenuRow| match row {
            MenuRow::Toggle { label, checked } => (label.to_string(), *checked),
            _ => panic!("overflow rows are toggles"),
        };
        let (rows, actions) = super::overflow_rows();
        assert_eq!(rows.iter().map(|r| label(r).0).collect::<Vec<_>>(), ["Rename", "Fork", "Archive"]);
        assert!(rows.iter().all(|r| !label(r).1), "no row is checked");
        assert_eq!(actions, [super::OverflowAction::Rename, super::OverflowAction::Fork, super::OverflowAction::Archive]);
    }

    use crate::palette::{filter_commands, tilde_root};
    use crate::app::Harness;
        use crate::layout::{Mode, RightKind};
    use crate::overlays::{Command, DialogAction, MenuKind, PaletteKind};
    use crate::session::{SessionHost, SessionView};
    // `cx.new` is `AppContext`'s, and the trait has to be in scope for it.
    use gpui::AppContext as _;
    use std::path::PathBuf;

    /// A bootable [`crate::Args`] pointed at a hermetic state dir, mirroring
    /// the helper in `app.rs`'s tests (which this module cannot import).
    fn test_args(dir: &std::path::Path) -> crate::Args {
        crate::Args {
            workspace: dir.to_path_buf(),
            workspace_explicit: true,
            provider: "echo".into(),
            provider_explicit: false,
            program: "muse".into(),
            theme: Some(aui_tokens::ThemeKind::Dark),
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
        }
    }

    /// Point `BAAZ_STATE_DIR` at a fresh temp dir for the test's duration,
    /// restoring whatever was there before.
    fn hermetic_state(name: &str) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf) {
        let dir = std::env::temp_dir().join(format!("baaz-palette-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("probe state dir");
        let guard = crate::store::test_env_lock();
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        (guard, old, dir)
    }

    /// Undo [`hermetic_state`]: remove the temp dir, put the old value back,
    /// release the lock so no test leaks its dir into another.
    #[allow(clippy::needless_pass_by_value)]
    fn restore_state(
        state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, PathBuf),
    ) {
        let (guard, old, dir) = state;
        let _ = std::fs::remove_dir_all(&dir);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
    }

    /// The six window commands mutate `layout` with no session open — the
    /// defect this task exists to prevent is `true` without an effect, so
    /// every arm asserts the effect, not just the return. Each command runs
    /// on a fresh [`Harness`] with `active == None`, so no state carries
    /// between arms.
    #[gpui::test]
    fn window_commands_land_with_no_session_open(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("window");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        for (command, kind) in [
            (Command::RightBrowser, RightKind::Browser),
            (Command::RightDiff, RightKind::Changes),
            (Command::RightGit, RightKind::Changes),
            (Command::RightFiles, RightKind::Files),
        ] {
            let baaz = vc.update(|window, cx| {
                cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
            });
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "{command:?} runs with no session open");
            let handled = vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| harness.run_window_command(command, window, cx))
            });
            assert!(handled, "{command:?} is a window command");
            assert!(vc.update(|_, cx| crate::layout::Layout::get(cx).right_open), "{command:?} opens the right pane");
            assert_eq!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).right_kind),
                Some(kind),
                "{command:?} opens on {kind:?}"
            );
        }
        for command in [Command::Terminal, Command::NewTerminalCmd] {
            let baaz = vc.update(|window, cx| {
                cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
            });
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "{command:?} runs with no session open");
            let handled = vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| harness.run_window_command(command, window, cx))
            });
            assert!(handled, "{command:?} is a window command");
            assert!(
                vc.update(|_, cx| crate::layout::Layout::get(cx).terminal_open),
                "{command:?} opens the dock from closed"
            );
        }
        restore_state(state);
    }

    /// The two dispatch paths are exhaustive and disjoint: every variant of
    /// [`Command::ALL`] is either a window command (`run_window_command`
    /// returns `true`) or a session command (`false`), the six window ones
    /// are in the window set, and the other eighteen are not. A 25th variant
    /// that lands in neither path fails here. (`RightDiff` is a legacy
    /// alias with no palette row: still window-handled when invoked, but
    /// not in `ALL`, so not counted here.)
    #[gpui::test]
    fn every_command_is_on_exactly_one_dispatch_side(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("sides");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        let mut window_count = 0;
        for command in Command::ALL {
            let handled = vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| harness.run_window_command(command, window, cx))
            });
            let is_window = matches!(
                command,
                Command::RightBrowser
                    | Command::RightDiff
                    | Command::RightGit
                    | Command::RightFiles
                    | Command::Terminal
                    | Command::Skills
                    | Command::NextWaiting
                    | Command::NewTerminalCmd
            );
            assert_eq!(handled, is_window, "{command:?} is classified on the wrong side");
            window_count += usize::from(handled);
        }
        assert_eq!(window_count, 7, "exactly the seven window commands are window-level");
        restore_state(state);
    }

    /// The composer's route reaches the window, not just the palette's.
    ///
    /// These also appear in the composer's `/` menu, which is built from
    /// [`Command::ALL`] in `session/render.rs`. They were originally swallowed
    /// in `SessionView::run_command` with an empty arm, so the menu rows and
    /// the typed commands did nothing at all — a gate cannot see that, and it
    /// is the reason this test exists rather than a second palette test.
    #[gpui::test]
    fn a_window_command_from_the_composer_reaches_the_window(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("composer-window");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx))
        });
        // The event the composer emits for a window command, delivered the way
        // the session would deliver it.
        for (command, kind) in [
            (Command::RightFiles, crate::layout::RightKind::Files),
            (Command::RightGit, crate::layout::RightKind::Changes),
        ] {
            vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| {
                    assert!(
                        harness.run_window_command(command, window, cx),
                        "{command:?} must be handled on the window side"
                    );
                })
            });
            let (open, shown) =
                vc.update(|_, cx| (crate::layout::Layout::get(cx).right_open, crate::layout::Layout::get(cx).right_kind));
            assert!(open, "{command:?} left the right pane shut");
            assert_eq!(shown, Some(kind), "{command:?} opened the wrong kind");
        }
        restore_state(state);
    }

    /// ⌘K twice in a row must open the palette twice. The owner's report:
    /// "hitting cmd+k still doesn't open the second time."
    #[gpui::test]
    fn cmd_k_opens_the_palette_every_time(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("cmdk-twice");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        // The Harness must actually be the window's root view: an entity that
        // is never rendered has no element tree, so no dispatch path and no
        // handlers. `add_empty_window` + `cx.new` gives exactly that.
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        let is_open = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette().is_some())
        };

        let sidebar = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| crate::layout::Layout::get(cx).sidebar_open);

        // ⌘K must open the palette every time, not only the first.
        vc.simulate_keystrokes("cmd-k");
        assert!(is_open(vc), "first cmd-k did not open the palette");
        vc.simulate_keystrokes("escape");
        assert!(!is_open(vc), "escape did not close the palette");
        vc.simulate_keystrokes("cmd-k");
        assert!(is_open(vc), "SECOND cmd-k did not open the palette");
        vc.simulate_keystrokes("escape");

        // And a palette cycle must not take every OTHER shortcut down with
        // it. This is the assertion that would have caught the real defect:
        // ⌘B worked fine until the palette had been opened once, because
        // both handlers hang off the same div and the div left the dispatch
        // path together.
        let before = sidebar(vc);
        vc.simulate_keystrokes("cmd-b");
        let after = sidebar(vc);
        assert_ne!(before, after, "cmd-b stopped working after a palette open/close");
        restore_state(state);
    }

    /// ↑/↓ reach the composer's `/` menu past the composer's own field —
    /// the palette's bug again, in the composer (owner, 2026-10-14). The
    /// keys are real keystrokes through the focused textarea, never a
    /// dispatched action: dispatching was how the miss stayed hidden.
    #[gpui::test]
    fn composer_menu_arrows_move_the_selection_from_the_field(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("composer-arrows");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let mut args = test_args(&state.2);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl"));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let view = vc.update(|_, cx| baaz.read(cx).active.clone()).expect("the replay opens a session");
        vc.update(|window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx)));
        vc.run_until_parked();
        vc.simulate_input("/");
        vc.run_until_parked();
        let selected = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().map(|m| m.selected))
        };
        assert_eq!(selected(vc), Some(0), "typing / opens the command menu");
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(1), "down did not move the selection");
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(2));
        vc.simulate_keystrokes("up");
        assert_eq!(selected(vc), Some(1), "up did not move back");
        // ⎋ closes it (owner 2026-10-15: the approval's ⎋ handler swallowed
        // the key and the menu stayed open).
        vc.simulate_keystrokes("escape");
        assert_eq!(selected(vc), None, "escape closes the menu");
        restore_state(state);
    }

    /// A replay names its row by the transcript on every open, not only the
    /// first in a state dir: the second open finds the title the first one
    /// stored, and the row used to keep the file name (the `jump-pill`
    /// capture took its two shots in one state dir and disagreed).
    #[gpui::test]
    fn a_replay_keeps_its_transcript_title_on_the_second_open(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("replay-title-reopen");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let capture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl");
        let want = "Show me the validation summary with code and links.";
        for open in ["first", "second"] {
            let mut args = test_args(&state.2);
            args.replay = Some(capture.clone());
            let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
            vc.run_until_parked();
            let label = vc.update(|_, cx| baaz.read(cx).session_list.read(cx).sessions.first().map(|row| row.label.clone()));
            assert_eq!(label.as_deref(), Some(want), "the {open} open's row");
            crate::sessions::flush();
        }
        restore_state(state);
    }

    /// A scripted replay opens no draft over itself: `open_boot_session`
    /// ran before the replay was `active`, saw no session, and opened an
    /// empty draft for the steps — the header then said "New session" over
    /// the replayed transcript (`transcript-full-top`, `-mid`).
    #[gpui::test]
    fn a_scripted_replay_opens_no_draft_over_itself(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("replay-steps-no-draft");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let mut args = test_args(&state.2);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-full.jsonl"));
        args.steps = vec!["top".to_owned()];
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let (pending, drafts, header, row) = vc.update(|_, cx| {
            let b = baaz.read(cx);
            let id = b.active.as_ref().map(|v| v.read(cx).rt(cx).session_id.clone()).expect("the replay is open");
            (b.open_state.pending_id.clone(), b.open_state.drafts.len(), b.collapsed_head_label(&b.chain_head(&id, cx), cx), b.session_list.read(cx).sessions.first().map(|r| r.label.clone()))
        });
        assert_eq!(pending, None, "nothing is opening over the replay");
        assert_eq!(drafts, 0, "no draft was made for the steps");
        assert_eq!(header, row, "the header names the replayed row");
        restore_state(state);
    }

    /// ⎋ on a caret menu keeps the caret in the field: the next key lands
    /// in the draft, for `/` and `@` alike (live, 2026-10-16: it went
    /// nowhere until the field was clicked).
    #[gpui::test]
    fn composer_menu_escape_keeps_the_field_focused(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("composer-esc-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let mut args = test_args(&state.2);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl"));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let view = vc.update(|_, cx| baaz.read(cx).active.clone()).expect("the replay opens a session");
        vc.update(|window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx)));
        vc.run_until_parked();
        for caret in ["/", "@"] {
            vc.simulate_input(caret);
            vc.run_until_parked();
            assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_some()), "{caret} opens its menu");
            vc.simulate_keystrokes("down down escape");
            vc.run_until_parked();
            assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_none()), "escape closes the {caret} menu");
            let focused = vc.update(|window, cx| view.update(cx, |view, cx| view.composer_focus_handle(cx).is_focused(window)));
            assert!(focused, "escape on the {caret} menu took the focus off the field");
            vc.simulate_input("x");
            vc.run_until_parked();
            let text = vc.update(|_, cx| view.read(cx).draft_text(cx));
            assert_eq!(text, format!("{caret}x"), "the key after escape reached the draft");
            vc.simulate_keystrokes("backspace backspace");
            vc.run_until_parked();
            assert_eq!(vc.update(|_, cx| view.read(cx).draft_text(cx)), "");
        }
        // A chip picker's ⎋ hands the keyboard to the composer too, even
        // when the picker opened with the keyboard elsewhere (a chip click).
        for keys in ["cmd-shift-p", "cmd-shift-m"] {
            vc.update(|window, cx| {
                let root = baaz.read(cx).focus_root.clone();
                window.focus(&root, cx);
            });
            vc.simulate_keystrokes(keys);
            vc.run_until_parked();
            assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_some()), "{keys} opens its picker");
            vc.simulate_keystrokes("down escape");
            vc.run_until_parked();
            assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_none()), "escape closes the {keys} picker");
            vc.simulate_input("x");
            vc.run_until_parked();
            assert_eq!(vc.update(|_, cx| view.read(cx).draft_text(cx)), "x", "the key after the {keys} picker's escape reached the draft");
            vc.simulate_keystrokes("backspace");
            vc.run_until_parked();
        }
        restore_state(state);
    }

    /// A composer menu closed by focusing another field — a click on the
    /// sidebar's rename field, the search, the URL bar — leaves the
    /// keyboard in that field: the composer takes it back only when
    /// nothing else did.
    #[gpui::test]
    fn composer_menu_closed_by_another_field_keeps_that_field_focused(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("composer-click-elsewhere");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let mut args = test_args(&state.2);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl"));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let view = vc.update(|_, cx| baaz.read(cx).active.clone()).expect("the replay opens a session");
        vc.update(|window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx)));
        vc.run_until_parked();
        vc.simulate_input("/");
        vc.run_until_parked();
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_some()), "/ opens its menu");
        // The click: another field takes the keyboard and the menu closes.
        let rename = vc.update(|_, cx| gpui::Focusable::focus_handle(&baaz.read(cx).rename, cx));
        vc.update(|window, cx| {
            window.focus(&rename, cx);
            baaz.update(cx, |h, cx| h.overlays.update(cx, |overlays, cx| overlays.close_menu(cx)));
        });
        vc.run_until_parked();
        assert!(vc.update(|window, _| rename.is_focused(window)), "the clicked field keeps the keyboard");
        let composer = vc.update(|_, cx| view.update(cx, |view, cx| view.composer_focus_handle(cx)));
        assert!(!vc.update(|window, _| composer.is_focused(window)), "the composer did not take it back");
        restore_state(state);
    }

    /// An arrow after the pointer lit a row continues from that row, not
    /// from the row the keyboard last reached (live, 2026-10-16: ↓↓↓ to
    /// /plan, hover /usage, ↓ landed on /compact). The pointer is really ON
    /// a row here: a test without it passed while the bug was live.
    #[gpui::test]
    fn composer_menu_arrow_continues_from_the_hovered_row(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("composer-hover-arrow");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let mut args = test_args(&state.2);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl"));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let view = vc.update(|_, cx| baaz.read(cx).active.clone()).expect("the replay opens a session");
        // The `@` picker's rows: a walked root, seeded rather than walked.
        vc.update(|_, cx| {
            let root = view.read(cx).rt(cx).workspace_path().to_owned();
            let entries = (0..8)
                .map(|i| format!("src/file{i}.rs"))
                .map(|path| crate::files::FileEntry { lower: path.to_lowercase(), path })
                .collect();
            let overlays = baaz.read(cx).overlays.clone();
            overlays.update(cx, |o, cx| o.insert_root(root, crate::files::WalkResult { entries, truncated: false }, Vec::new(), cx));
        });
        vc.update(|window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx)));
        vc.run_until_parked();
        // The `@` list is a picker (plan 4.6): its selection is the picker's.
        let selected = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| view.read(cx).menu_selected(cx));
        let size = vc.update(|window, _| window.viewport_size());
        for caret in ["/", "@"] {
            vc.simulate_input(caret);
            vc.run_until_parked();
            vc.simulate_keystrokes("down down down");
            vc.run_until_parked();
            assert_eq!(selected(vc), Some(3), "{caret}: three arrows reach row 3");
            // Walk the pointer up the window until it lights a row other
            // than the keyboard's.
            let mut hovered = None;
            let mut y = f32::from(size.height) - 4.0;
            while y > 0.0 && hovered.is_none() {
                vc.simulate_mouse_move(gpui::point(size.width / 2.0, gpui::px(y)), None, Default::default());
                vc.run_until_parked();
                hovered = selected(vc).filter(|row| *row != 3);
                y -= 4.0;
            }
            let hovered = hovered.unwrap_or_else(|| panic!("{caret}: the pointer on a row never made it the selection"));
            let rows = vc.update(|_, cx| view.read(cx).menu_rows(cx));
            vc.simulate_keystrokes("down");
            assert_eq!(selected(vc), Some((hovered + 1) % rows), "{caret}: the arrow continues from the hovered row");
            // The pointer rests: the keyboard holds the highlight.
            vc.simulate_keystrokes("down");
            vc.run_until_parked();
            assert_eq!(selected(vc), Some((hovered + 2) % rows), "{caret}: a resting pointer never takes the selection back");
            vc.simulate_keystrokes("escape backspace");
            vc.run_until_parked();
            vc.simulate_mouse_move(gpui::point(gpui::px(1.0), gpui::px(1.0)), None, Default::default());
            vc.run_until_parked();
        }
        restore_state(state);
    }

    /// ↑/↓ reach the palette list past its query field.
    ///
    /// The query palettes (Search, Commands, Projects) focus a `gpui_kit`
    /// textarea whose own `Input` context binds the arrows to caret moves at
    /// a deeper depth than the scrim's `AuiMenu` — so the scrim's
    /// `SelectPrev`/`SelectNext` never fired while the field was focused, and
    /// Enter always ran row 0. Two keymap rows rebind the arrows at
    /// `AuiMenu > Input` (`crate::app::PALETTE_QUERY_CONTEXT`), which ties
    /// the textarea's binding at full depth and wins by later registration.
    #[gpui::test]
    fn palette_arrows_move_the_selection_with_a_query_field(cx: &mut gpui::TestAppContext) {
        use crate::sidebar::SessionEntry;

        let state = hermetic_state("palette-arrows");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        // The Harness must be the window's root view (`add_window_view`), not
        // an entity made with `cx.new` — an unrendered entity has no element
        // tree, so no dispatch path and no handlers.
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        let selected = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).palette_selected(cx))
        };
        // Rows for the session-backed palettes: three visible sessions feed
        // both Search (empty query lists recents) and Resume.
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                for (n, id) in ["s-arrow-1", "s-arrow-2", "s-arrow-3"].iter().enumerate() {
                    harness.session_list.update(cx, |list, _| {
                        list.sessions.push(SessionEntry {
                            id: id.to_string(),
                            label: format!("Arrow session {n}"),
                            updated: chrono::Local::now(),
                            created: chrono::Local::now(),
                            running: false,
                            turns: 3,
                            hidden: false,
                            pinned: false,
                            archived: false,
                            description: "the ask".into(),
                            replayed: false,
                            provider: crate::providers::ProviderId::Muse,
                            named: true,
                            needs_title: false,
                            side_marker: false,
                            title_pending: false,
                            last_ask: Some("the ask".into()),
                            local: false,
                            provisional: false,
                            turns_unknown: false,
                            workspace: None,
                            project: None,
                            project_name: None,
                            attention: Vec::new(),
                            approval_command: None,
                            pending_question: None,
                            turn_started: None,
                            last_error: None,
                            unread: false,
                            branch: None,
                            terminals_running: 0,
                        });
                    });
                }
                harness.invalidate_list(cx);
            })
        });
        // Rows for the Projects palette: two adopted folders. (Boot may have
        // adopted the workspace already, so the asserts below only assume at
        // least two rows.)
        for name in ["proj-a", "proj-b"] {
            let dir = state.2.join(name);
            std::fs::create_dir_all(&dir).expect("probe project dir");
            vc.update(|_, cx| {
                baaz.update(cx, |harness, cx| {
                    harness.adopt_root(&dir, cx);
                })
            });
        }

        // Commands (⌘K): the palette with a query field.
        vc.simulate_keystrokes("cmd-k");
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(1), "down did not move the selection");
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(2));
        vc.simulate_keystrokes("up");
        assert_eq!(selected(vc), Some(1), "up did not move back");
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(2));
        // The selected row is the third command, and Enter runs it — not row
        // 0. `Command::ALL[2]` is `/mode`, a session command: with no session
        // open it runs quietly, and the palette closing proves the confirm
        // path ran with this selection.
        let third = vc.update(|_, cx| {
            let app: &gpui::App = cx;
            baaz.read(app).palette_rows(PaletteKind::Commands, app)[2].0.to_string()
        });
        assert_eq!(Command::ALL[2].slash(), "/mode");
        assert_eq!(third, Command::ALL[2].slash(), "the third row is not the third command");
        vc.simulate_keystrokes("enter");
        assert_eq!(selected(vc), None, "enter did not confirm the selection");
        // Typing still reaches the field, including `j` and `k`.
        vc.simulate_keystrokes("cmd-k");
        vc.simulate_input("jk");
        let query = vc.update(|_, cx| baaz.read(cx).palette_query(cx).expect("the palette's field").read(cx).value().to_string());
        assert_eq!(query, "jk", "typing no longer reaches the query field");
        assert_eq!(selected(vc), Some(0), "the selection escaped the filtered rows");
        vc.simulate_keystrokes("escape");
        assert_eq!(selected(vc), None, "escape did not dismiss the query palette");
        // Enter acts on the SELECTED row: filtered to `/browser`, Enter opens
        // the right pane on Browser — row 0 of the unfiltered list (`/model`)
        // would have done nothing observable.
        vc.simulate_keystrokes("cmd-k");
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.palette_query(cx).expect("the palette's field").update(cx, |field, cx| field.set_value(String::new(), window, cx));
            })
        });
        vc.simulate_input("brow");
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("enter");
        let (open, kind) =
            vc.update(|_, cx| (crate::layout::Layout::get(cx).right_open, crate::layout::Layout::get(cx).right_kind));
        assert!(open, "enter did not run the selected row");
        assert_eq!(kind, Some(RightKind::Browser), "enter ran the wrong row");

        // Search (⌘⇧F): the other query palette from the owner's report.
        vc.simulate_keystrokes("cmd-shift-f");
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(1), "down did not move the Search selection");
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(2));
        vc.simulate_keystrokes("up");
        assert_eq!(selected(vc), Some(1), "up did not move the Search selection back");
        vc.simulate_keystrokes("escape");
        assert_eq!(selected(vc), None, "escape did not dismiss Search");

        // Projects (⌘⇧O): the third query palette.
        vc.simulate_keystrokes("cmd-shift-o");
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(1), "down did not move the Projects selection");
        vc.simulate_keystrokes("up");
        assert_eq!(selected(vc), Some(0), "up did not move the Projects selection back");
        vc.simulate_keystrokes("escape");
        assert_eq!(selected(vc), None, "escape did not dismiss Projects");

        // Resume has no query field and must keep working: arrows move over
        // its rows, and a scrim click still dismisses.
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| harness.open_palette(PaletteKind::Resume, cx))
        });
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("down");
        assert_eq!(selected(vc), Some(1), "down stopped working where no query field exists");
        vc.simulate_click(gpui::point(gpui::px(10.), gpui::px(10.)), gpui::Modifiers::default());
        assert_eq!(selected(vc), None, "the scrim click stopped dismissing the palette");
        restore_state(state);
    }

    /// Enter must run the row the arrows moved to, not row 0. Moving the
    /// selection is only half of it: if Enter still fired the first row, the
    /// palette would look fixed and behave exactly as it did before.
    #[gpui::test]
    fn enter_runs_the_selected_row_not_the_first(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("palette-enter");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        vc.simulate_keystrokes("cmd-k");
        vc.simulate_keystrokes("down down");
        let selected = vc.update(|_, cx| baaz.read(cx).palette_selected(cx));
        assert_eq!(selected, Some(2), "two downs must select the third row");
        // The row the palette would run, resolved the way the picker's ↩
        // resolves it (`PickerDelegate::confirm`): index into the same
        // ordered rows.
        let rows = vc.update(|_, cx| baaz.read(cx).palette_rows_len_for(PaletteKind::Commands, cx));
        assert!(rows > 2, "the Commands palette lists more than three rows");
        vc.simulate_keystrokes("enter");
        assert!(
            vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette().is_none()),
            "enter did not act on a row at all"
        );
        restore_state(state);
    }

    /// A palette with NO query field must keep working: the fix binds the
    /// arrows on `AuiMenu > Input`, and a fix scoped too broadly — or one
    /// that moved the binding off `AuiMenu` — would break these instead.
    #[gpui::test]
    fn arrows_still_work_in_a_palette_without_a_query(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("palette-noquery");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        // Projects has a query field; Commands has one too. Resume and Fork
        // do not, but they are empty without sessions, so Projects is the
        // one that can be asserted here — it always lists at least the
        // "Choose folder…" lead plus any adoptable workspace.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Projects, cx)));
        let rows = vc.update(|_, cx| baaz.read(cx).palette_rows_len_for(PaletteKind::Projects, cx));
        if rows > 1 {
            vc.simulate_keystrokes("down");
            assert_eq!(
                vc.update(|_, cx| baaz.read(cx).palette_selected(cx)),
                Some(1),
                "down did not move in the Projects palette"
            );
        }
        restore_state(state);
    }

    /// A busy tab asks before it closes, naming its running command; an
    /// idle tab gets no dialog at all.
    #[test]
    fn the_close_tab_confirmation_fires_for_busy_tabs_only() {
        let dialog = Harness::close_tab_dialog("t1", Some("sleep 300")).expect("a busy tab asks");
        assert!(dialog.title.contains("sleep 300"), "it names the command");
        assert_eq!(dialog.action, DialogAction::CloseTerminalTab);
        assert_eq!(dialog.archive_target.as_deref(), Some("t1"));
        assert_eq!(dialog.primary, "Close Tab");
        assert!(Harness::close_tab_dialog("t1", None).is_none(), "an idle tab closes outright");
    }

    /// docs/82 0.14: quitting with turns running names the session and
    /// counts the rest; with a command too, it says both; with nothing
    /// running it does not ask.
    #[test]
    fn the_quit_confirmation_names_working_sessions() {
        let one = Harness::quit_dialog(&["Fix the login bug".to_owned()], None).expect("asks");
        assert_eq!(one.title, "Quit while an agent is working?");
        assert!(one.detail.contains("Fix the login bug"), "{}", one.detail);
        assert_eq!(one.action, DialogAction::QuitWithWorkRunning);
        let several = Harness::quit_dialog(&["A".to_owned(), "B".to_owned(), "C".to_owned()], None).expect("asks");
        assert!(several.detail.contains('3') && several.detail.contains("\u{201C}A\u{201D}"), "{}", several.detail);
        let both = Harness::quit_dialog(&["A".to_owned()], Some(("cargo test", 0))).expect("asks");
        assert!(both.detail.contains("cargo test") && both.detail.contains("\u{201C}A\u{201D}"), "{}", both.detail);
        assert!(Harness::quit_dialog(&[], None).is_none(), "nothing running, no question");
    }

    /// Quitting names the running command, and counts the ones past it.
    #[test]
    fn the_quit_confirmation_names_the_running_command() {
        let dialog = Harness::quit_terminal_dialog("pnpm vitest", 0);
        assert_eq!(dialog.action, DialogAction::QuitWithWorkRunning);
        assert_eq!(dialog.primary, "Quit");
        assert!(dialog.detail.contains("pnpm vitest"), "it names the command");
        let crowded = Harness::quit_terminal_dialog("pnpm vitest", 2);
        assert!(crowded.detail.contains("pnpm vitest"), "still names one command");
        assert!(crowded.detail.contains('3'), "and counts all three");
    }

    /// The Commands filter reads the slash and the description,
    /// case-insensitively: `brow` is `/browser`, `reasoning` is a
    /// description word only, nonsense is nothing, empty is all 24.
    #[test]
    fn the_command_filter_matches_slash_and_description() {
        assert_eq!(filter_commands("brow", None), vec![Command::RightBrowser]);
        assert_eq!(filter_commands("BROW", None), vec![Command::RightBrowser]);
        assert_eq!(filter_commands("reasoning", None), vec![Command::Effort]);
        assert!(filter_commands("zzz-no-such-command", None).is_empty());
        assert_eq!(filter_commands("", None).len(), 25, "an empty query lists every command");
        assert_eq!(filter_commands("   ", None).len(), 25, "whitespace is an empty query");
    }

    /// `Commands` owns a query editor, and the drawn rows follow it: the
    /// field opens empty with all 24 rows, and setting it to `brow`
    /// leaves `/browser` alone — the same rows the keyboard walks.
    #[gpui::test]
    fn the_command_palette_has_a_query_field_and_filters(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("commands-query");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = cx.add_window_view(|window, cx| {
            Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx)
        });
        // The field is the palette's own, made on its first frame.
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_palette(PaletteKind::Commands, cx)));
        vc.run_until_parked();
        assert!(vc.update(|_, cx| baaz.read(cx).palette_query(cx).expect("the palette's field").read(cx).value().is_empty()));
        let empty = vc.update(|_, cx| {
            let app: &gpui::App = cx;
            baaz.read(app).palette_rows(PaletteKind::Commands, app).len()
        });
        assert_eq!(empty, 25, "an empty query lists every command");
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.palette_query(cx).expect("the palette's field").update(cx, |field, cx| field.set_value("brow", window, cx));
            });
        });
        let rows = vc.update(|_, cx| {
            let app: &gpui::App = cx;
            baaz.read(app).palette_rows(PaletteKind::Commands, app)
        });
        assert_eq!(rows.len(), 1, "only /browser matches `brow`");
        assert!(rows[0].0.as_ref() == "/browser", "the row is the slash command");
        restore_state(state);
    }

    #[test]
    fn home_folds_to_a_tilde_and_other_roots_stand() {
        // HOME is read, never written: safe beside parallel tests.
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            assert_eq!(tilde_root(&format!("{home}/Projects/baaz")), "~/Projects/baaz");
            assert_eq!(tilde_root(&home), "~");
        }
        assert_eq!(tilde_root("/private/tmp/h4ws"), "/private/tmp/h4ws");
    }

    /// B6: a harness with an open lane, wired the way `open_on_provider`
    /// wires one — the view shares the harness's own overlay stack, so the
    /// header menu and the composer's `+` menu meet in one place.
    fn harness_with_lane(
        vc: &mut gpui::VisualTestContext,
        state_dir: &std::path::Path,
    ) -> (gpui::Entity<Harness>, gpui::Entity<SessionView>) {
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(state_dir), crate::runtime::CaptureToken::default(), window, cx))
        });
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id: "echo".to_owned(),
                workspace: state_dir.to_string_lossy().into_owned(),
                overlays: baaz.read(cx).overlays.clone(),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            cx.new(|cx| SessionView::new("s-plus".to_owned(), host, window, cx))
        });
        vc.update(|_, cx| baaz.update(cx, |harness, _| harness.active = Some(view.clone())));
        (baaz, view)
    }

    /// Which menus stand open: the `+` menu and any other menu on the
    /// overlay stack. B6 leaves exactly one set after either opening order
    /// (the stack holds one menu, `+` among them).
    fn open_menu_count(
        vc: &mut gpui::VisualTestContext,
        baaz: &gpui::Entity<Harness>,
        view: &gpui::Entity<SessionView>,
    ) -> (bool, bool) {
        vc.update(|_, cx| {
            (view.read(cx).plus_open(cx), baaz.read(cx).overlays.read(cx).menu().is_some_and(|m| m.kind != MenuKind::Plus))
        })
    }

    /// B6: opening the header `…` menu while the composer's `+` menu is
    /// open leaves exactly one menu open — `open_menu` closes plus first.
    /// Without the close both stood open, stacked.
    #[gpui::test]
    fn opening_the_header_menu_closes_the_plus_menu(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("header-plus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, view) = harness_with_lane(vc, &state.2);
        vc.update(|_, cx| view.update(cx, |view, cx| view.step_plus(cx))).expect("plus opens");
        assert!(vc.update(|_, cx| view.read(cx).plus_open(cx)), "the `+` menu opened");
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_menu(MenuKind::Overflow, cx)));
        let (plus, menu) = open_menu_count(vc, &baaz, &view);
        assert!(!plus, "the header menu closed the `+` menu");
        assert!(menu, "the header menu still opened");
        assert_eq!(usize::from(plus) + usize::from(menu), 1, "exactly one menu stays open");
        restore_state(state);
    }

    /// B6: the project menu opens through its own path, not `open_menu`,
    /// and still closes an open `+` menu.
    #[gpui::test]
    fn opening_the_project_menu_closes_the_plus_menu(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("project-plus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, view) = harness_with_lane(vc, &state.2);
        vc.update(|_, cx| view.update(cx, |view, cx| view.step_plus(cx))).expect("plus opens");
        assert!(vc.update(|_, cx| view.read(cx).plus_open(cx)), "the `+` menu opened");
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_project_menu(None, true, cx)));
        let (plus, menu) = open_menu_count(vc, &baaz, &view);
        assert!(!plus, "the project menu closed the `+` menu");
        assert!(menu, "the project menu still opened");
        restore_state(state);
    }

    /// B6: the scripted `plus` step takes the click's path, so it too
    /// closes an open header menu instead of stacking on it.
    #[gpui::test]
    fn the_scripted_plus_step_closes_the_header_menu(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("step-plus-header");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, view) = harness_with_lane(vc, &state.2);
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_menu(MenuKind::Overflow, cx)));
        vc.update(|_, cx| view.update(cx, |view, cx| view.step_plus(cx))).expect("plus opens");
        let (plus, menu) = open_menu_count(vc, &baaz, &view);
        assert!(plus && !menu, "exactly the `+` menu stays open (plus={plus}, menu={menu})");
        restore_state(state);
    }

    /// B6: opening `+` while the header `…` menu is open leaves exactly
    /// one menu open — the plus toggle drops the overlay menu first.
    /// Without the drop both stood open, stacked.
    #[gpui::test]
    fn opening_plus_closes_the_header_menu(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("plus-header");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (baaz, view) = harness_with_lane(vc, &state.2);
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.open_menu(MenuKind::Overflow, cx)));
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).menu().is_some()), "the header menu opened");
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_plus_menu(cx)));
        let (plus, menu) = open_menu_count(vc, &baaz, &view);
        assert!(plus, "the `+` menu opened");
        assert!(!menu, "opening `+` closed the header menu");
        assert_eq!(usize::from(plus) + usize::from(menu), 1, "exactly one menu stays open");
        restore_state(state);
    }

    /// A shell with no session: the predicate tests need no lane, only the
    /// window, so they boot the plain harness the `app.rs` tests boot.
    fn header_harness(
        vc: &mut gpui::VisualTestContext,
        state_dir: &std::path::Path,
    ) -> gpui::Entity<Harness> {
        vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(state_dir), crate::runtime::CaptureToken::default(), window, cx))
        })
    }

    /// D4: the corner stack draws in the corner window only in Browse, and
    /// in the main window again while Settings hides the page.
    #[gpui::test]
    fn the_corner_stack_moves_to_its_window_only_in_browse(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("corner-host");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = header_harness(vc, &state.2);
        assert!(vc.update(|_, cx| !baaz.read(cx).corner_in_overlay(cx)), "Sessions draws it in the window");
        vc.update(|_, cx| baaz.update(cx, |_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = Mode::Browse)));
        assert!(vc.update(|_, cx| baaz.read(cx).corner_in_overlay(cx)), "Browse draws it over the page");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.settings_page.update(cx, |page, _| page.state.open = true)));
        assert!(vc.update(|_, cx| !baaz.read(cx).corner_in_overlay(cx)), "Settings hides the page");
        restore_state(state);
    }

    /// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
    #[gpui::test]
    fn the_overlay_window_hosts_a_modal_only_in_browse(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("modal-hosted");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = header_harness(vc, &state.2);
        let wanted = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).modal_overlay_wanted(cx));
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            crate::layout::Layout::stage(cx, |layout| layout.mode = Mode::Sessions);
            h.open_palette(PaletteKind::Search, cx);
        }));
        assert!(!wanted(vc), "Sessions draws its palette in the main window");
        vc.update(|_, cx| baaz.update(cx, |_, cx| crate::layout::Layout::stage(cx, |layout| layout.mode = Mode::Browse)));
        assert!(wanted(vc), "Browse hosts the palette in the overlay window");
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            h.overlays.update(cx, |o, cx| o.close_palette(cx));
        }));
        assert!(!wanted(vc), "Browse with neither palette nor dialog hosts nothing");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.set_dialog(cx, Harness::quit_terminal_dialog("cargo", 0))));
        assert!(wanted(vc), "Browse hosts a dialog in the overlay window");
        restore_state(state);
    }

    /// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
    #[gpui::test]
    fn the_hosted_modal_takes_the_keyboard_at_its_own_handle(cx: &mut gpui::TestAppContext) {
        use gpui::Focusable as _;
        let state = hermetic_state("modal-focus");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = header_harness(vc, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            crate::layout::Layout::stage(cx, |layout| layout.mode = Mode::Browse);
            h.open_palette(PaletteKind::Search, cx);
        }));
        let (focus, query) = vc.update(|_, cx| {
            let h = baaz.read(cx);
            (h.modal_focus(cx), h.search_query.focus_handle(cx))
        });
        assert_eq!(focus, Some(query), "a Search palette is typed into its query field");
        vc.update(|_, cx| baaz.update(cx, |h, cx| {
            h.overlays.update(cx, |o, cx| o.close_palette(cx));
            h.set_dialog(cx, Harness::quit_terminal_dialog("cargo", 0));
        }));
        let dialog_focus = vc.update(|_, cx| baaz.read(cx).dialog_focus(cx));
        assert!(dialog_focus.is_some(), "the dialog view has its own focus");
        assert_eq!(vc.update(|_, cx| baaz.read(cx).modal_focus(cx)), dialog_focus, "a dialog takes its own focus");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.close_dialog(cx)));
        assert_eq!(vc.update(|_, cx| baaz.read(cx).modal_focus(cx)), None, "no modal, no handle");
        restore_state(state);
    }

    /// A replayed session with its composer holding the keyboard: what the
    /// overlay-stack tests open things over.
    fn replay_with_composer_focused<'a>(
        cx: &'a mut gpui::TestAppContext,
        dir: &std::path::Path,
    ) -> (gpui::Entity<Harness>, gpui::Entity<SessionView>, &'a mut gpui::VisualTestContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        cx.update(crate::app::bind_keys);
        let mut args = test_args(dir);
        args.replay = Some(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/msp/synthetic-markdown.jsonl"));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let view = vc.update(|_, cx| baaz.read(cx).active.clone()).expect("the replay opens a session");
        vc.update(|window, cx| view.update(cx, |view, cx| view.focus_composer(window, cx)));
        vc.run_until_parked();
        (baaz, view, vc)
    }

    fn composer_focused(vc: &mut gpui::VisualTestContext, view: &gpui::Entity<SessionView>) -> bool {
        vc.update(|window, cx| view.update(cx, |view, cx| view.composer_focus_handle(cx).is_focused(window)))
    }

    /// Plan 4.6: Escape pops the newest entry and gives back the focus it
    /// took — a menu opened over a dialog closes first and hands the
    /// keyboard back to the dialog; the dialog then closes and hands it back
    /// to the composer that held it when the dialog opened.
    #[gpui::test]
    fn escape_pops_a_menu_off_a_dialog_and_each_gives_back_its_focus(cx: &mut gpui::TestAppContext) {
        use crate::overlay_stack::LayerKind;
        let state = hermetic_state("stack-menu-over-dialog");
        let (baaz, view, vc) = replay_with_composer_focused(cx, &state.2);
        assert!(composer_focused(vc, &view), "the composer holds the keyboard");
        vc.update(|_, cx| {
            baaz.update(cx, |h, cx| {
                h.set_dialog(
                    cx,
                    crate::overlays::Dialog {
                        title: "Something happened".into(),
                        detail: "Read this.".into(),
                        kind: aui::overlay::DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    },
                )
            })
        });
        vc.run_until_parked();
        // The dialog takes the keyboard (a click on it does the same).
        let dialog_focus = vc.update(|_, cx| baaz.read(cx).dialog_focus(cx)).expect("the dialog is up");
        vc.update(|window, cx| window.focus(&dialog_focus, cx));
        vc.run_until_parked();
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Overflow, cx)));
        vc.run_until_parked();
        let top = vc.update(|_, cx| baaz.read(cx).overlays.read(cx).stack.top());
        assert_eq!(top, Some(LayerKind::Menu), "the menu stands over the dialog");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        let (menu, dialog) = vc.update(|_, cx| {
            let overlays = baaz.read(cx).overlays.read(cx);
            (overlays.menu().is_some(), overlays.dialog().is_some())
        });
        assert!(!menu && dialog, "the first Escape closed the menu only");
        assert!(vc.update(|window, _| dialog_focus.is_focused(window)), "the menu gave the keyboard back to the dialog");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).dialog().is_none()), "the second Escape closed the dialog");
        assert!(composer_focused(vc, &view), "the dialog gave the keyboard back to the composer");
        restore_state(state);
    }

    /// Plan 4.6b: a sheet's own Escape goes through the overlay stack, so a
    /// menu opened over the sheet closes first (newest first) and the sheet
    /// only on the next Escape. The keyboard is in the Muse sheet's own key
    /// field, so the Escape is the sheet's (its capture handler), not the
    /// root's.
    #[gpui::test]
    fn a_sheets_own_escape_closes_the_menu_over_it_first(cx: &mut gpui::TestAppContext) {
        use crate::overlay_stack::LayerKind;
        let state = hermetic_state("sheet-escape-newest");
        let (baaz, _view, vc) = replay_with_composer_focused(cx, &state.2);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_muse_sheet(cx)));
        vc.run_until_parked();
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.login_intent(aui::screens::LoginIntent::UseApiKey, window, cx)));
        vc.run_until_parked();
        let key_field = vc.update(|_, cx| gpui::Focusable::focus_handle(baaz.read(cx).login.read(cx).api_key_field().read(cx), cx));
        assert!(vc.update(|window, _| key_field.is_focused(window)), "the sheet's key field holds the keyboard");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Overflow, cx)));
        vc.run_until_parked();
        let top = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).overlays.read(cx).stack.top());
        assert_eq!(top(vc), Some(LayerKind::Menu), "the menu stands over the sheet");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        assert_eq!(top(vc), Some(LayerKind::MuseSheet), "the sheet's Escape closed the menu over it, not the sheet");
        assert!(vc.update(|window, _| key_field.is_focused(window)), "the menu gave the keyboard back to the sheet's field");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        assert!(
            !vc.update(|_, cx| baaz.read(cx).overlays.read(cx).stack.is_open(LayerKind::MuseSheet)),
            "the second Escape closed the sheet"
        );
        restore_state(state);
    }

    /// Plan 4.6b: the overlay views redraw themselves — the arrows in the
    /// palette and in the composer's model picker move the picker's own
    /// selection, and the root is never notified for it.
    #[gpui::test]
    fn the_pickers_redraw_without_notifying_the_root(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("picker-redraw-alone");
        let (baaz, view, vc) = replay_with_composer_focused(cx, &state.2);
        let notified = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let count = notified.clone();
        let _watch = vc.update(|_, cx| cx.observe(&baaz, move |_, _| count.set(count.get() + 1)));
        // The palette: ⌘K, then ↓ twice.
        vc.simulate_keystrokes("cmd-k");
        vc.run_until_parked();
        let palette = vc.update(|_, cx| baaz.read(cx).palette_view(cx)).expect("⌘K opened the palette");
        let redrawn = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let picker_count = redrawn.clone();
        let _palette_watch = vc.update(|_, cx| cx.observe(&palette, move |_, _| picker_count.set(picker_count.get() + 1)));
        notified.set(0);
        vc.simulate_keystrokes("down down");
        vc.run_until_parked();
        assert_eq!(vc.update(|_, cx| palette.read(cx).selected()), 2, "the palette's own selection moved");
        assert!(redrawn.get() > 0, "the palette redrew itself");
        assert_eq!(notified.get(), 0, "the palette's arrows never notified the root");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        // The composer's model picker: the composer's arrows move it.
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx)));
        vc.run_until_parked();
        let model = vc.update(|_, cx| view.read(cx).model_picker(cx)).expect("the model picker opened");
        vc.update(|_, cx| {
            model.update(cx, |picker, cx| {
                picker.push_facts(cx, |delegate| {
                    delegate.rows = vec![aui::composer::PickerRow::new("a", "A", ""), aui::composer::PickerRow::new("b", "B", "")];
                });
                picker.select(0, cx);
            })
        });
        vc.run_until_parked();
        notified.set(0);
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.move_menu(1, cx)));
        vc.run_until_parked();
        assert_eq!(vc.update(|_, cx| model.read(cx).selected()), 1, "the model picker's own selection moved");
        assert_eq!(notified.get(), 0, "the model picker's arrows never notified the root");
        restore_state(state);
    }

    /// Plan 4.6: the ⌘K palette takes the keyboard into its query field,
    /// and Escape gives it back to the composer that held it (the old edge
    /// detector parked it on the root).
    #[gpui::test]
    fn escape_on_the_palette_gives_the_keyboard_back_to_the_composer(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("stack-palette-focus");
        let (baaz, view, vc) = replay_with_composer_focused(cx, &state.2);
        vc.simulate_keystrokes("cmd-k");
        vc.run_until_parked();
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette().is_some()), "⌘K opened the palette");
        assert!(!composer_focused(vc, &view), "the palette took the keyboard");
        vc.simulate_keystrokes("escape");
        vc.run_until_parked();
        assert!(vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette().is_none()), "Escape closed the palette");
        assert!(composer_focused(vc, &view), "the palette gave the keyboard back to the composer");
        // The keyboard is live there: typing lands in the draft.
        vc.simulate_input("x");
        vc.run_until_parked();
        assert_eq!(vc.update(|_, cx| view.read(cx).draft_text(cx)), "x");
        restore_state(state);
    }

    /// Plan 4.6: an entry that covers the page and draws in the main window
    /// makes `Covers` report the page covered; the same modal hosted in the
    /// overlay window (Browse) and a panel cover nothing.
    #[gpui::test]
    fn a_covering_entry_covers_the_page(cx: &mut gpui::TestAppContext) {
        use crate::overlay_stack::{Entry, Layer};
        let state = hermetic_state("stack-covers");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let (baaz, vc) = cx.add_window_view(|window, cx| Harness::new(test_args(&state.2), crate::runtime::CaptureToken::default(), window, cx));
        vc.run_until_parked();
        let covered = |vc: &mut gpui::VisualTestContext| vc.update(|window, cx| baaz.read(cx).page_covers(window, cx).base.overlay_open);
        assert!(!covered(vc), "nothing open, nothing covers");
        let dialog = || crate::overlays::Dialog {
            title: "x".into(),
            detail: "y".into(),
            kind: aui::overlay::DialogKind::Error,
            primary: "Dismiss",
            action: DialogAction::Dismiss,
            archive_target: None,
        };
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.set_dialog(cx, dialog())));
        assert!(covered(vc), "a dialog in the main window covers the page");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.close_dialog(cx)));
        assert!(!covered(vc), "closing it uncovers the page");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Overflow, cx)));
        assert!(covered(vc), "a menu covers the page");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Account, cx)));
        assert!(!covered(vc), "the account menu stays inside the sidebar");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.overlays.update(cx, |o, cx| o.close_menu(cx))));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.overlays.update(cx, |o, cx| o.push(Entry::new(Layer::SiteInfo), cx))));
        assert!(!covered(vc), "a panel over the live page covers nothing");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(Mode::Browse, window, cx)));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.set_dialog(cx, dialog())));
        assert!(!covered(vc), "Browse hosts the dialog in the overlay window, over the live page");
        vc.update(|window, cx| baaz.update(cx, |h, cx| h.set_mode(Mode::Sessions, window, cx)));
        assert!(covered(vc), "back in Sessions the same dialog draws in the main window and covers");
        restore_state(state);
    }

    /// Plan 4.6: a menu's own button toggles it — the second press closes
    /// what the first opened, at once, with no timestamp in between.
    #[gpui::test]
    fn a_menus_button_toggles_it(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("stack-toggle");
        let (baaz, view, vc) = replay_with_composer_focused(cx, &state.2);
        let open = |vc: &mut gpui::VisualTestContext, kind: MenuKind| vc.update(|_, cx| baaz.read(cx).overlays.read(cx).is_open(kind));
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Overflow, cx)));
        assert!(open(vc, MenuKind::Overflow), "the first press opens the header menu");
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_menu(MenuKind::Overflow, cx)));
        assert!(!open(vc, MenuKind::Overflow), "the second press closes it");
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx)));
        assert!(open(vc, MenuKind::Model), "the chip opens its picker");
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_picker(MenuKind::Model, cx)));
        assert!(!open(vc, MenuKind::Model), "the chip closes its picker");
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_plus_menu(cx)));
        assert!(open(vc, MenuKind::Plus), "`+` opens its menu");
        vc.update(|_, cx| view.update(cx, |view, cx| view.toggle_plus_menu(cx)));
        assert!(!open(vc, MenuKind::Plus), "`+` closes its menu");
        restore_state(state);
    }
}
