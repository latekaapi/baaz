//! What floats over the window: the modal, the palette, the toast stack and
//! the header's overflow menu.
//!
//! [`crate::overlays`] owns the state — which modal is up, which menu is open,
//! where its selection sits, what the menus are built from. This module owns
//! the elements for the ones the **window** anchors, and the intents that put
//! them up and take them down. The two the sidebar anchors live in
//! [`crate::sidebar_view`], and the composer's own chip menus in
//! [`crate::session`], because a picker is anchored to the affordance that
//! opened it and only its owner knows where that is.
//!
//! Everything here goes through `aui::overlay::popover_layer`, so the paint
//! order stays the single one the library defines, and a deterministic capture
//! draws the dialog and the toast stack settled rather than rising in: an
//! enter presence never lands on the same frame twice.

use aui::data::{button, ButtonSize};
use aui::keys::{Cancel, Confirm, SelectNext, SelectPrev};
use aui::nav::{folder_drop_card, view_menu, MenuRow};
use aui::overlay::{
    anchored_menu, command_palette, dialog, popover_layer, DialogKind, MenuAlign, MenuSide, PaletteIcon,
    PaletteItem, PaletteSection,
};
use aui_icons::IconName;
use aui_motion::{presence, EnterExit, PresenceStyle};
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{
    black, div, prelude::*, px, relative, AnyElement, App, Context, Focusable, SharedString, Window,
};
use gpui_kit::base::h_flex;
use gpui_kit::component::input::Textarea;

use crate::app::{Harness, Wire, PALETTE_ROWS, PALETTE_SCRIM, PALETTE_TOP, TOAST_STACK_H, TOAST_TOP, TOAST_W};
use crate::layout::RightKind;
use crate::login::Auth;
use crate::overlays::{Command, Dialog, DialogAction, Menu, MenuKind, Palette, PaletteKind};
use crate::wire::WireCall;
use crate::{files, skills};

/// Actions of the header's overflow menu, in row order.
#[derive(Clone, Copy)]
enum OverflowAction {
    Rename,
    Fork,
    Archive,
}

/// One palette section's rows under one icon.
fn palette_items(
    rows: &[(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
) -> Vec<PaletteItem> {
    rows.iter().map(|(id, label, detail)| PaletteItem::new(id.clone(), icon.clone(), label.clone()).context(detail.clone())).collect()
}

/// The Commands palette's filter: [`Command::ALL`] narrowed to the query,
/// matching the slash and the description case-insensitively. The empty
/// query lists every command. The drawn rows come from
/// [`Harness::palette_rows`], which reads the palette's own query field,
/// so the keyboard and the click walk the same filtered list.
fn filter_commands(query: &str) -> Vec<Command> {
    let needle = query.trim().to_lowercase();
    Command::ALL
        .into_iter()
        .filter(|command| {
            needle.is_empty()
                || command.slash().to_lowercase().contains(&needle)
                || command.description().to_lowercase().contains(&needle)
        })
        .collect()
}

/// One row of the Projects palette: its stable id, whether it belongs to
/// the Add section, and the drawn item.
struct ProjectsRow {
    /// `p:<project id>`, `choose`, or `a:<workspace root>`.
    id: SharedString,
    /// Section "Add" rather than section "Projects".
    add: bool,
    /// The drawn row: mark or folder glyph, root context, session-count
    /// badge, and the `⌘⇧O` hint on "Choose folder…".
    item: PaletteItem,
}

/// A root with `~` for home, for the Projects section's context line.
fn tilde_root(root: &str) -> String {
    if let Some(home) = std::env::var_os("HOME") {
        let home = home.to_string_lossy().into_owned();
        if let Some(rest) = root.strip_prefix(home.as_str()) {
            if rest.is_empty() {
                return "~".to_owned();
            }
            if rest.starts_with('/') {
                return format!("~{rest}");
            }
        }
    }
    root.to_owned()
}

impl Harness {
    /// Put a modal up. Only one at a time, which is what makes Escape's order
    /// (menu, then modal) a single rule.
    pub(crate) fn set_dialog(&mut self, cx: &mut Context<Self>, dialog: Dialog) {
        self.overlays.update(cx, |overlays, _| {
            overlays.dialog = Some(dialog);
            overlays.settings = None;
        });
        cx.notify();
    }

    pub(crate) fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.dialog = None);
        cx.notify();
    }

    /// The quit confirmation (D52): names the running command. `others`
    /// counts the running commands past the named one, so a second tab is
    /// not a surprise after the first is confirmed. Pure, so the naming
    /// has a test without a window.
    pub(crate) fn quit_terminal_dialog(command: &str, others: usize) -> Dialog {
        let detail = if others == 0 {
            format!("`{command}` is still running. Quitting stops it.")
        } else {
            format!(
                "{total} terminal commands are still running, including `{command}`. Quitting stops them.",
                total = others + 1
            )
        };
        Dialog {
            title: "Quit with a terminal command running?".into(),
            detail,
            kind: DialogKind::Warning,
            primary: "Quit",
            action: DialogAction::QuitWithRunningTerminal,
            archive_target: None,
        }
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

    /// ⌘Q with a terminal command running asks first through
    /// [`Self::quit_terminal_dialog`], naming the command. Returns `true`
    /// when it asked — the caller must not quit.
    pub(crate) fn maybe_confirm_quit(&mut self, cx: &mut Context<Self>) -> bool {
        let running = self.terminal_host.read(cx).running_terminals(cx);
        let Some((_, command)) = running.first() else { return false };
        let dialog = Self::quit_terminal_dialog(command, running.len() - 1);
        self.set_dialog(cx, dialog);
        true
    }

    /// The close-tab dialog's primary button: the tab id rode the dialog,
    /// so dismissing it any other way already dropped the target.
    pub(crate) fn confirm_close_terminal_tab(&mut self, cx: &mut Context<Self>) {
        let target = self.overlays.read(cx).dialog.as_ref().and_then(|d| {
            (d.action == DialogAction::CloseTerminalTab).then(|| d.archive_target.clone()).flatten()
        });
        self.close_dialog(cx);
        if let Some(id) = target {
            self.terminal_host.update(cx, |host, _| host.close(&id));
        }
        cx.notify();
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
            let skills = skills::list_in(&program, Some(&root));
            let files = files::walk(&root);
            (skills, files, root)
        };
        self.wire_call(cx, work, |this, (skills, files, root), cx| {
            let key = root.to_string_lossy().into_owned();
            if files.truncated {
                // The toast names the project the walk ran in, so a person
                // with eight roots knows whose picker is short.
                let name = this
                    .projects
                    .find_by_root(&root)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| key.clone());
                crate::baaz_log!(
                    "@ mention index stopped at {} files; some workspace files are not mentionable",
                    files::CAP
                );
                this.overlays.update(cx, |overlays, _| {
                    overlays.toast(
                        "@ mentions shortened",
                        format!("{name} holds more than {} files; some are not mentionable.", files::CAP),
                    );
                });
            }
            this.overlays.update(cx, |overlays, _| overlays.insert_root(key, files, skills));
            cx.notify();
        });
    }

    /// Open the palette on one list.
    pub(crate) fn open_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = if already { None } else { Some(Palette { kind, selected: 0 }) };
        });
        cx.notify();
    }

    /// Open the Projects palette: adopted projects to start a session in,
    /// then "Choose folder…" and recent Muse workspaces to adopt. Picking a
    /// project starts a session there (and makes it current); adopting only
    /// adopts. `focus_add` starts the selection on "Choose folder…", which
    /// is where the hero's "Recent workspaces" button points.
    pub(crate) fn open_projects(&mut self, focus_add: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.projects_query.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        let rows = self.projects_rows(cx);
        let selected = if focus_add { rows.iter().position(|row| row.add).unwrap_or(0) } else { 0 };
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = Some(Palette { kind: PaletteKind::Projects, selected });
        });
        window.focus(&self.projects_query.focus_handle(cx), cx);
        cx.notify();
    }

    /// The Projects palette's query editor, drawn inside the card's own
    /// query row like the search palette's: the query filters both sections
    /// by name and path, synchronously.
    fn projects_query_editor(&self) -> AnyElement {
        Textarea::new(&self.projects_query)
            .appearance(false)
            .bordered(false)
            .text_size(aui_tokens::scaled(scale::FS_14))
            .h_auto()
            .whitespace_nowrap()
            .into_any_element()
    }

    /// Keep the Projects selection inside the filtered rows after a
    /// keystroke: the rows are rebuilt at render, so a selection past the
    /// new end wraps to the head.
    pub(crate) fn clamp_projects_selection(&mut self, cx: &mut Context<Self>) {
        let count = self.projects_rows(cx).len();
        self.overlays.update(cx, |overlays, _| {
            if let Some(palette) = overlays.palette.as_mut() {
                if palette.kind == PaletteKind::Projects && palette.selected >= count {
                    palette.selected = 0;
                }
            }
        });
    }

    /// Keep the Commands selection inside the filtered rows after a
    /// keystroke: the rows are rebuilt at render, so a selection past the
    /// new end wraps to the head.
    pub(crate) fn clamp_commands_selection(&mut self, cx: &mut Context<Self>) {
        let query = self.commands_query.read(cx).value().trim().to_owned();
        let count = filter_commands(&query).len();
        self.overlays.update(cx, |overlays, _| {
            if let Some(palette) = overlays.palette.as_mut() {
                if palette.kind == PaletteKind::Commands && palette.selected >= count {
                    palette.selected = 0;
                }
            }
        });
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


    /// One ordered vec feeds the Projects palette's keyboard, click and
    /// drawn sections, so the three agree about what row 3 is. Section
    /// "Projects" holds every adoption in sidebar order (mark, root with `~`
    /// for home, visible session count); section "Add" holds recent Muse
    /// workspaces from the index — minus adopted roots, only paths that
    /// still exist as directories, newest first, at most twelve, badged
    /// with their session count. The "Choose folder…" row is gone: the Add
    /// section's lead is the library's `folder_drop_card`, drawn inside the
    /// card under the section title (it is not a `PaletteItem` and takes no
    /// keyboard selection). The query filters both sections by name and path.
    fn projects_rows(&self, cx: &gpui::App) -> Vec<ProjectsRow> {
        let query = self.projects_query.read(cx).value().trim().to_owned();
        let needle = query.to_lowercase();
        let matches = |name: &str, path: &str| {
            needle.is_empty() || name.to_lowercase().contains(&needle) || path.to_lowercase().contains(&needle)
        };
        let emphasise = |item: PaletteItem| if query.is_empty() { item } else { item.matching(&query) };
        let mut rows = Vec::new();
        let visible = self.visible_sessions(cx);
        for project in self.ordered_projects() {
            let root = project.root.to_string_lossy().into_owned();
            if !matches(&project.name, &root) {
                continue;
            }
            let count = visible.iter().filter(|e| e.project.as_deref() == Some(project.id.as_str())).count();
            // No coloured tiles anywhere: adopted
            // projects wear the folder glyph, like the recent workspaces
            // below.
            let item = emphasise(
                PaletteItem::new(
                    SharedString::from(format!("p:{}", project.id)),
                    PaletteIcon::Glyph(IconName::Folder),
                    project.name.clone(),
                )
                .context(tilde_root(&root))
                .badge(count.to_string()),
            );
            rows.push(ProjectsRow { id: SharedString::from(format!("p:{}", project.id)), add: false, item });
        }
        // Recent Muse workspaces, newest first: what the index saw sessions
        // in, minus the roots this window already holds. ("Choose folder…"
        // lives in the Add section as the `folder_drop_card` lead, not as
        // a row.)
        let adopted: std::collections::HashSet<String> = self
            .projects
            .projects
            .iter()
            .map(|p| crate::projects::canonical_path(&p.root).to_string_lossy().into_owned())
            .collect();
        // Baaz's own state directory holds the tier probe's throwaway
        // workspace; it is never a project anyone means to adopt.
        let own_state = [crate::store::support_dir(), crate::store::default_support_dir()];
        let mut recents = 0;
        for (root, count, _) in crate::index::workspaces(&self.index) {
            if recents >= 12 {
                break;
            }
            if adopted.contains(&crate::projects::canonical_str(&root)) {
                continue;
            }
            if own_state.iter().any(|dir| std::path::Path::new(&root).starts_with(dir)) {
                continue;
            }
            if !std::path::Path::new(&root).is_dir() {
                continue;
            }
            let name = std::path::Path::new(&root)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.clone());
            if !matches(&name, &root) {
                continue;
            }
            let item = emphasise(
                PaletteItem::new(
                    SharedString::from(format!("a:{root}")),
                    PaletteIcon::Glyph(IconName::Folder),
                    name,
                )
                .context(root.clone())
                .badge(count.to_string()),
            );
            rows.push(ProjectsRow { id: SharedString::from(format!("a:{root}")), add: true, item });
            recents += 1;
        }
        rows
    }

    /// The palette's rows, in the order it draws them, so the keyboard and the
    /// click agree about what row 3 is.
    fn palette_rows(&self, kind: PaletteKind, cx: &gpui::App) -> Vec<(SharedString, SharedString, SharedString)> {
        match kind {
            PaletteKind::Commands => {
                let query = self.commands_query.read(cx).value().trim().to_owned();
                filter_commands(&query)
                    .into_iter()
                    .map(|c| (c.slash().into(), c.slash().into(), c.description().into()))
                    .collect()
            }
            PaletteKind::Resume => self
                .visible_sessions(cx)
                .iter()
                // Newest first, and only as many as the palette can show: a
                // list taller than the window is a list with a hidden bottom.
                .take(PALETTE_ROWS)
                .map(|entry| {
                    let meta: SharedString =
                        if entry.turns > 0 { format!("{} turns", entry.turns).into() } else { "no turns".into() };
                    (entry.id.clone().into(), entry.label.clone().into(), meta)
                })
                .collect(),
            PaletteKind::Search => self.search_rows(cx),
            // The keyboard walks the same ordered rows the sections draw,
            // so an index is a row in both.
            PaletteKind::Projects => self
                .projects_rows(cx)
                .into_iter()
                .map(|row| {
                    let detail = row.item.context.clone().unwrap_or_default();
                    (row.id, row.item.label, detail)
                })
                .collect(),
            // The active session's completed turns, newest first; the rows
            // come from the view because the window does not keep a transcript.
            PaletteKind::Fork => self
                .active
                .as_ref()
                .map(|view| view.read(cx).fork_turns())
                .unwrap_or_default()
                .into_iter()
                .take(PALETTE_ROWS)
                .map(|(id, label, detail)| (id.into(), label.into(), detail.into()))
                .collect(),
        }
    }

    /// Run the palette's selected row: the keyboard's path, which is the one
    /// that still has to resolve an index into a row.
    fn confirm_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected)) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected)) else {
            return;
        };
        let rows = self.palette_rows(kind, cx);
        let Some((id, _, _)) = rows.get(selected).cloned() else {
            crate::baaz_log!("palette confirm: nothing at {selected} of {} rows", rows.len());
            // The Add section's lead is a card, not a row, so an empty
            // Projects palette still offers the panel on ↩.
            if kind == PaletteKind::Projects {
                self.choose_project_folder(cx);
            }
            return;
        };
        crate::baaz_log!("palette select (keyboard): {kind:?} {id}");
        self.run_palette_row(kind, id, window, cx);
    }

    /// Run one palette row by its id.
    ///
    /// A click already knows which row it hit, so it comes straight here
    /// rather than rebuilding every row to turn the id back into an index and
    /// the index back into the same id (finding `performance-7`).
    fn run_palette_row(&mut self, kind: PaletteKind, id: SharedString, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.palette = None);
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
                self.with_session(cx, |view, vc| view.fork(Some(id.to_string()), vc));
            }
            PaletteKind::Commands => {
                if let Some(command) = Command::parse(&id) {
                    if !self.run_window_command(command, window, cx) {
                        self.with_session(cx, |view, cx| view.run_command(command, window, cx));
                    }
                }
            }
        }
        cx.notify();
    }

    /// Run one window-level ⌘K command, returning whether it was one.
    ///
    /// Window commands act on the window itself — the right pane, the
    /// terminal dock — so they run here on [`Harness`], before the session
    /// delegation in [`Self::run_palette_row`], which does nothing when no
    /// session is open. Session commands return `false` and keep their old
    /// path. Both sides list their variants explicitly, with no `_` arm:
    /// adding command 24 must force a decision about which side it is on.
    pub(crate) fn run_window_command(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) -> bool {
        match command {
            Command::RightBrowser => {
                self.show_right(RightKind::Browser, cx);
                true
            }
            Command::RightDiff => {
                self.show_right(RightKind::Diff, cx);
                true
            }
            Command::RightGit => {
                self.show_right(RightKind::Git, cx);
                true
            }
            Command::RightFiles => {
                self.show_right(RightKind::Files, cx);
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
    /// own button again closes it.
    pub(crate) fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).menu.as_ref().is_some_and(|m| m.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = if already { None } else { Some(Menu::picker(kind, 0)) };
        });
        cx.notify();
    }

    /// The header's overflow menu, anchored under the "…" button: Rename swaps
    /// the title for the dense inline field, Fork opens the fork picker, and
    /// Archive asks first through the archive dialog.
    pub(crate) fn render_overflow_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Overflow) {
            return None;
        }
        let rows = vec![
            MenuRow::Toggle { label: "Rename".into(), checked: false },
            MenuRow::Toggle { label: "Fork".into(), checked: false },
            MenuRow::Toggle { label: "Archive".into(), checked: false },
        ];
        let actions = [OverflowAction::Rename, OverflowAction::Fork, OverflowAction::Archive];
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            let action = actions.get(*index).copied();
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            match action {
                Some(OverflowAction::Rename) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.sidebar_open = true;
                        this.start_rename(session_id, window, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to rename", "No session is open.");
                        });
                    }
                    cx.notify();
                }
                Some(OverflowAction::Fork) => this.open_palette(PaletteKind::Fork, cx),
                Some(OverflowAction::Archive) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.open_archive_dialog(session_id, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to archive", "No session is open.");
                        });
                        cx.notify();
                    }
                }
                None => {}
            }
        });
        // Below-end of the `…` button that opened it — `anchored_menu` flips
        // above and slides inside the window on overflow. No button bounds
        // yet: no menu this frame, never the old `top(48) right(8)` corner
        // (the button is always rendered, so the notify lands next frame).
        let Some(trigger) = self.overflow_bounds else {
            cx.notify();
            return None;
        };
        // A click anywhere outside closes it: the catcher is a sibling of
        // the menu inside the same draw.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            cx.notify();
        });
        let catcher = div()
            .id("overflow-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .on_click(move |_, w, cx| dismiss(&(), w, cx));
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

    /// ↑/↓ in an open menu, wrapping over the rows the menu actually has.
    pub(crate) fn move_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let rows = view.read(cx).menu_rows(cx);
        self.overlays.update(cx, |overlays, _| overlays.move_selection(delta, rows));
        cx.notify();
    }

    /// The search palette's query editor, drawn inside the card's own query
    /// row through the library's slot: one surface, the field where the
    /// placeholder would be, no chrome of its own. Clearing it returns to
    /// the empty state (recents).
    fn search_query_editor(&self) -> AnyElement {
        Textarea::new(&self.search_query)
            .appearance(false)
            .bordered(false)
            .text_size(aui_tokens::scaled(scale::FS_14))
            .h_auto()
            .whitespace_nowrap()
            .into_any_element()
    }

    /// The Commands palette's query editor, drawn inside the card's own
    /// query row through the library's slot, like the search palette's:
    /// one surface, the field where the placeholder would be, no chrome
    /// of its own. The query filters [`Command::ALL`] on slash and
    /// description, synchronously.
    fn commands_query_editor(&self) -> AnyElement {
        Textarea::new(&self.commands_query)
            .appearance(false)
            .bordered(false)
            .text_size(aui_tokens::scaled(scale::FS_14))
            .h_auto()
            .whitespace_nowrap()
            .into_any_element()
    }

    /// ⌘K and `/resume`: the command palette, over everything.
    ///
    /// The same primitive for both lists, because they are the same gesture —
    /// a list, an arrow key and a return — and a second picker would be a
    /// second set of keys to learn.
    pub(crate) fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, selected) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected))?;
        let rows = self.palette_rows(kind, cx);
        // The card's own query row mirrors the query for the picking lists;
        // the search palette edits through its own field above the card, so
        // the card's row carries the result count instead.
        let (query, placeholder, sections) = match kind {
            PaletteKind::Commands => {
                // The rows already carry the filter; the match emphasis
                // covers the label, like the search palette's.
                let needle = self.commands_query.read(cx).value().trim().to_owned();
                let items = palette_items(&rows, PaletteIcon::Glyph(IconName::Slash));
                let items = if needle.is_empty() {
                    items
                } else {
                    items.into_iter().map(|item| item.matching(&needle)).collect()
                };
                (
                    SharedString::from(""),
                    SharedString::from("Every command in this build"),
                    vec![PaletteSection::new("Commands", items)],
                )
            }
            PaletteKind::Resume => (
                SharedString::from(""),
                SharedString::from("Resume a session in this workspace"),
                vec![PaletteSection::new(
                    "Sessions",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Clock)),
                )],
            ),
            PaletteKind::Fork => (
                SharedString::from(""),
                SharedString::from("Pick a completed turn to branch from"),
                vec![PaletteSection::new(
                    "Fork from",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Git)),
                )],
            ),
            PaletteKind::Projects => {
                // The rows already carry their icons, contexts and badges;
                // the sections only partition them. The Add section always
                // shows: its lead is the library's folder card, the first
                // thing under "ADD" above the recent workspaces, so a click
                // chooses through P1's panel and a drop adopts every dropped
                // directory, the first becoming current. It is not a row —
                // the keyboard walks past it.
                let entity = cx.entity().downgrade();
                let choose = move |_: &mut Window, cx: &mut App| {
                    entity.update(cx, |this, cx| this.choose_project_folder(cx)).ok();
                };
                let entity = cx.entity().downgrade();
                let drop = move |paths: Vec<std::path::PathBuf>, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |this, cx| this.adopt_dropped(paths, cx)).ok();
                };
                let lead = folder_drop_card("projects-drop").key("⌘⇧O").on_click(choose).on_drop(drop);
                let mut adopted = Vec::new();
                let mut adding = Vec::new();
                for row in self.projects_rows(cx) {
                    if row.add {
                        adding.push(row.item);
                    } else {
                        adopted.push(row.item);
                    }
                }
                let mut sections = Vec::new();
                if !adopted.is_empty() {
                    sections.push(PaletteSection::new("Projects", adopted));
                }
                sections.push(PaletteSection::new("Add", adding).lead(lead));
                (SharedString::from(""), SharedString::from("Add or switch project"), sections)
            }
            PaletteKind::Search => {
                let (sessions, files): (Vec<_>, Vec<_>) =
                    rows.iter().partition(|(id, _, _)| id.starts_with("s:"));
                // The row's own match emphasis covers the label only — the
                // library paints `matched` ranges on the label and the
                // context (the snippet) stays muted mono. Primary text is
                // the sidebar label either way; the snippet is display-only.
                let query = self.search_query.read(cx).value().to_string();
                // Every row wears its project's display name (the folder
                // name for sessions no project holds), so hits from several
                // projects tell themselves apart.
                let badged = |part: Vec<&(SharedString, SharedString, SharedString)>, icon: PaletteIcon| {
                    part.into_iter()
                        .map(|(id, label, detail)| {
                            let session_id = id
                                .strip_prefix("s:")
                                .or_else(|| id.strip_prefix("f:").and_then(|r| r.split_once(':').map(|(s, _)| s)));
                            let mut item =
                                PaletteItem::new(id.clone(), icon.clone(), label.clone()).context(detail.clone());
                            if let Some(badge) =
                                session_id.and_then(|sid| self.search_badge(sid).map(SharedString::from))
                            {
                                item = item.badge(badge);
                            }
                            if query.trim().is_empty() { item } else { item.matching(&query) }
                        })
                        .collect::<Vec<_>>()
                };
                let mut sections = Vec::new();
                if !sessions.is_empty() {
                    sections.push(PaletteSection::new(
                        "Sessions",
                        badged(sessions, PaletteIcon::Glyph(IconName::Clock)),
                    ));
                }
                if !files.is_empty() {
                    sections.push(PaletteSection::new("Files", badged(files, PaletteIcon::Glyph(IconName::File))));
                }
                (SharedString::from(""), self.search_status(cx), sections)
            }
        };
        // The click carries the row id, so it runs that row directly instead
        // of rebuilding every row to map the id back to a position and the
        // position back to the same id (finding `performance-7`).
        let select = cx.listener(move |this: &mut Self, id: &SharedString, window, cx| {
            crate::baaz_log!("palette select (click): {kind:?} {id}");
            this.run_palette_row(kind, id.clone(), window, cx);
        });
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.palette = None);
            cx.notify();
        });
        let count = rows.len();
        // A scripted screenshot is a static composition, not an opening: the
        // card's enter presence (fade + rise) never settles inside a capture,
        // so screenshots draw the palette at rest — opaque, one surface.
        // Live opens keep the rise.
        let mut card = command_palette("palette", query, sections, selected)
            .placeholder(placeholder)
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        if kind == PaletteKind::Search {
            card = card.query_slot(self.search_query_editor());
        }
        if kind == PaletteKind::Projects {
            card = card.query_slot(self.projects_query_editor());
        }
        if kind == PaletteKind::Commands {
            card = card.query_slot(self.commands_query_editor());
        }
        if self.still() {
            card = card.at_rest();
        }
        // The Projects palette's drop card rides inside the card as the Add
        // section's lead now, not as a floating box above it.
        let centred: AnyElement = card.into_any_element();
        Some(
            popover_layer(
                div()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_palette)
                    .on_action(cx.listener(move |this, _: &SelectNext, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(move |this, _: &SelectPrev, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(-1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm_palette(window, cx)))
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .id("palette-scrim")
                    .absolute()
                    .inset_0()
                    // The open palette owns the wheel: occluding the scrim
                    // makes the transcript's `wheel_capture` yield everywhere
                    // outside the card (the library card already occludes its
                    // own rect and stops the wheel after its list scrolls),
                    // so a wheel over the dimmed ground never reaches the
                    // transcript behind this modal. Clicks already dismiss
                    // through this same scrim, so blocking click-through
                    // changes nothing.
                    .occlude()
                    .bg(gpui::black().opacity(PALETTE_SCRIM))
                    // A click on the dimmed ground closes it, which is the
                    // gesture every overlay in this window already answers to.
                    // On the *release*, not the press: gpui fires a row's
                    // `on_click` on mouse-up against the frame the release
                    // sees, so dismissing on mouse-down closed the palette
                    // under the press and the release found no row — every
                    // palette row's click silently did nothing but dismiss.
                    // The row's own click runs first on
                    // the way up and this runs after it, idempotently.
                    .on_click(cx.listener(|this, _: &gpui::ClickEvent, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .child(
                        gpui_kit::base::h_flex()
                            .w_full()
                            .justify_center()
                            .pt(px(PALETTE_TOP))
                            .child(centred),
                    ),
            )
            .into_any_element(),
        )
    }

    pub(crate) fn render_dialog(&self, window: &mut Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read the modal out whole before anything asks `cx` for a listener:
        // the entity's borrow and `cx.listener` cannot be alive at once.
        let (title, detail, kind, primary_label, action, danger) = {
            let modal = self.overlays.read(cx).dialog.as_ref()?;
            let danger = modal.action == DialogAction::Archive
                || modal.action == DialogAction::RemoveProject
                || modal.action == DialogAction::QuitWithRunningTerminal
                || modal.action == DialogAction::CloseTerminalTab;
            (modal.title.clone(), modal.detail.clone(), modal.kind, modal.primary, modal.action, danger)
        };
        // The archive target stays on the dialog until its own button runs:
        // closing it any other way drops the target with it.
        // A dialog whose primary already dismisses it carries no second
        // button: "Dismiss  Dismiss" was what a screenshot showed. The danger
        // dialog keeps Cancel beside Archive; the others keep Dismiss beside
        // Reconnect / Sign in / Done.
        let secondary = match (danger, action) {
            (true, _) => Some("Cancel"),
            (false, DialogAction::Dismiss) => None,
            (false, _) => Some("Dismiss"),
        };
        let primary = cx.listener(move |this: &mut Self, _: &(), window, cx| {
            if action == DialogAction::Archive {
                this.confirm_archive_dialog(window, cx);
                return;
            }
            if action == DialogAction::RemoveProject {
                this.confirm_remove_project(cx);
                return;
            }
            // Confirmed through its own path, like Archive: the tab id rode
            // the dialog, so closing any other way already dropped it.
            if action == DialogAction::CloseTerminalTab {
                this.confirm_close_terminal_tab(cx);
                return;
            }
            this.close_dialog(cx);
            match action {
                DialogAction::Dismiss => {}
                DialogAction::Reconnect => {
                    this.wire = Wire::Reconnecting;
                    this.reconnect(cx);
                }
                DialogAction::SignIn => {
                    this.auth = Auth::SignedOut;
                    this.active = None;
                    this.login.reset_to_choose();
                }
                DialogAction::Archive => {}
                // Confirmed through the early return above, like Archive.
                DialogAction::RemoveProject => {}
                DialogAction::QuitWithRunningTerminal => {
                    crate::tier::cleanup_probes();
                    cx.quit();
                }
                // Confirmed through the early return above, like Archive.
                DialogAction::CloseTerminalTab => {}
            }
            cx.notify();
        });
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // Critical errors (`DialogKind::Error`) draw a Baaz-owned card that
        // mirrors the library dialog's shell — same 420 pt width, 16 pt
        // padding, 12 pt gap, scrim, motion and action row — with the error
        // mascot leading the header in place of the 26 px kind tile: 56 pt,
        // centred on the title row, the title to its right. Only critical
        // dialogs take this path; confirmations keep the library card below
        // untouched, and the toast stack stays text-only.
        let card: AnyElement = if kind == DialogKind::Error {
            let sprite = crate::mascot::ErrorMascot::classify(&title, &detail);
            let p = cx.aui().colors;
            // The shared modal motion (6 px drop at 98.5 % scale), settled
            // for a scripted capture — the same stillness the palette uses.
            let style = if self.still() {
                PresenceStyle { opacity: 1.0, offset_y: px(0.0), scale: 1.0 }
            } else {
                PresenceStyle::fade_rise_scale(
                    presence("baaz-error-dialog", true, EnterExit::DEFAULT, window, cx),
                    6.0,
                    0.985,
                )
            };
            let head = h_flex()
                .w_full()
                .items_center()
                .gap(px(scale::SP_3))
                .child(crate::mascot::error_mascot(sprite))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_role(TextRole::Title)
                        .text_color(p.ink)
                        .child(SharedString::from(title)),
                );
            let mut shell = div()
                .relative()
                .top(-style.offset_y)
                .flex_none()
                .w(px(420.0 * style.scale))
                .rounded(px(scale::R_LG))
                .border_1()
                .border_color(p.line_strong)
                .bg(p.overlay)
                .shadow(p.shadow(3))
                .text_color(p.ink)
                .gap(px(scale::SP_4))
                .p(px(scale::SP_5))
                .child(head)
                .child(
                    div()
                        .w_full()
                        .ui(scale::FS_12)
                        .line_height(relative(scale::LH_UI))
                        .text_color(p.ink_2)
                        .child(SharedString::from(detail)),
                );
            // The one action row, as the library draws it: spacer, secondary,
            // primary at the far right.
            let mut actions =
                h_flex().w_full().items_center().gap(px(scale::SP_3)).child(div().flex_1().min_w(px(0.0)));
            if let Some(label) = secondary {
                actions = actions.child(
                    button("dialog-secondary", label)
                        .size(ButtonSize::Md)
                        .on_click(move |_, w, cx| close(&(), w, cx)),
                );
            }
            let mut prime = button("dialog-primary", primary_label).size(ButtonSize::Md);
            prime = if danger { prime.danger() } else { prime.primary() };
            shell = shell.child(actions.child(prime.on_click(move |_, w, cx| primary(&(), w, cx))));
            let mut scrim = div()
                .id("dialog")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(black().opacity(0.25 * style.opacity));
            scrim = scrim.on_click(move |_, w, cx| dismiss(&(), w, cx));
            scrim
                .child(div().id("dialog-card").flex_none().opacity(style.opacity).occlude().child(shell))
                .into_any_element()
        } else {
            // A deterministic capture draws the dialog settled rather than rising
            // in: the enter presence never lands on the same frame twice.
            let mut card = dialog("dialog", title)
                .kind(kind)
                .body(detail)
                .danger(danger)
                .primary(primary_label);
            if let Some(secondary) = secondary {
                card = card.secondary(secondary);
            }
            let card = card
                .on_primary(move |w, cx| primary(&(), w, cx))
                .on_secondary(move |w, cx| close(&(), w, cx))
                .on_dismiss(move |w, cx| dismiss(&(), w, cx));
            let card = if crate::clock::deterministic() { card.at_rest() } else { card };
            card.into_any_element()
        };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .inset_0()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_dialog)
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| this.close_dialog(cx)))
                    .child(card),
            )
            .into_any_element(),
        )
    }

    /// `error-dialog:<offline|blocked|sorry>`: capture aid that raises a
    /// critical error dialog through the same [`Self::set_dialog`] path a
    /// real failure takes, so the screenshot shows the production render —
    /// never a hardcoded render branch. Scripting only, like the other
    /// capture aids (`title-land:`, `row-detail:`); free, it reaches the
    /// wire nowhere. The payload picks the sprite family (default `sorry`).
    pub(crate) fn step_error_dialog(&mut self, rest: &str, cx: &mut Context<Self>) {
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
            _ => Dialog {
                title: "Muse hit an internal error".into(),
                detail: "The turn failed inside `muse serve` before producing anything. \
                    Nothing was billed for the attempt; retry the turn or start a new session."
                    .into(),
                kind: DialogKind::Error,
                primary: "Dismiss",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
        };
        self.set_dialog(cx, dialog);
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

    /// The toast stack, bottom right. Toasts here are informational — a
    /// compaction that did nothing, a command a later phase brings — so they
    /// carry no action, only a close.
    pub(crate) fn render_toasts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toasts = self.overlays.read(cx).toasts.clone();
        if toasts.is_empty() {
            return None;
        }
        let newest = toasts.last().map(|t| t.id.to_string()).unwrap_or_default();
        let dismissed = newest.clone();
        let close = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&dismissed));
            cx.notify();
        });
        // One action exists, and it is Undo — one hidden row, one "Clear
        // empty" batch, or one archived session, whichever the newest toast
        // was for.
        let act = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.undo_newest(cx);
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&newest));
            cx.notify();
        });
        // A deterministic capture draws the stack settled: toasts slide in,
        // which never lands on the same frame twice.
        let stack = aui::feedback::toast_stack("toasts", toasts);
        let stack = if crate::clock::deterministic() { stack.at_rest() } else { stack };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .right(px(scale::SP_5))
                    .top(px(TOAST_TOP))
                    .w(px(TOAST_W))
                    .h(px(TOAST_STACK_H))
                    .child(
                        stack
                            .on_action(move |_, window, cx| act(&(), window, cx))
                            .on_close(move |window, cx| close(&(), window, cx)),
                    ),
            )
            .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_commands, tilde_root};
    use crate::app::Harness;
    use crate::layout::RightKind;
    use crate::overlays::{Command, DialogAction, PaletteKind};
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
            (Command::RightDiff, RightKind::Diff),
            (Command::RightGit, RightKind::Git),
            (Command::RightFiles, RightKind::Files),
        ] {
            let baaz = vc.update(|window, cx| {
                cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
            });
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "{command:?} runs with no session open");
            let handled = vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| harness.run_window_command(command, window, cx))
            });
            assert!(handled, "{command:?} is a window command");
            assert!(vc.update(|_, cx| baaz.read(cx).layout.right_open), "{command:?} opens the right pane");
            assert_eq!(
                vc.update(|_, cx| baaz.read(cx).layout.right_kind),
                Some(kind),
                "{command:?} opens on {kind:?}"
            );
        }
        for command in [Command::Terminal, Command::NewTerminalCmd] {
            let baaz = vc.update(|window, cx| {
                cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
            });
            assert!(vc.update(|_, cx| baaz.read(cx).active.is_none()), "{command:?} runs with no session open");
            let handled = vc.update(|window, cx| {
                baaz.update(cx, |harness, cx| harness.run_window_command(command, window, cx))
            });
            assert!(handled, "{command:?} is a window command");
            assert!(
                vc.update(|_, cx| baaz.read(cx).layout.terminal_open),
                "{command:?} opens the dock from closed"
            );
        }
        restore_state(state);
    }

    /// The two dispatch paths are exhaustive and disjoint: every variant of
    /// [`Command::ALL`] is either a window command (`run_window_command`
    /// returns `true`) or a session command (`false`), the six new ones are
    /// in the window set, and the original 17 are not. A 24th variant that
    /// lands in neither path fails here.
    #[gpui::test]
    fn every_command_is_on_exactly_one_dispatch_side(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("sides");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
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
                    | Command::NewTerminalCmd
            );
            assert_eq!(handled, is_window, "{command:?} is classified on the wrong side");
            window_count += usize::from(handled);
        }
        assert_eq!(window_count, 6, "exactly the six new commands are window-level");
        restore_state(state);
    }

    /// The composer's route reaches the window, not just the palette's.
    ///
    /// These six also appear in the composer's `/` menu, which is built from
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
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        // The event the composer emits for a window command, delivered the way
        // the session would deliver it.
        for (command, kind) in [
            (Command::RightFiles, crate::layout::RightKind::Files),
            (Command::RightGit, crate::layout::RightKind::Git),
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
                vc.update(|_, cx| (baaz.read(cx).layout.right_open, baaz.read(cx).layout.right_kind));
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
            Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx)
        });
        let is_open = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette.is_some())
        };

        let sidebar = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).sidebar_open);

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
            Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx)
        });
        let selected = |vc: &mut gpui::VisualTestContext| {
            vc.update(|_, cx| baaz.read(cx).overlays.read(cx).palette.as_ref().map(|p| p.selected))
        };
        // Rows for the session-backed palettes: three visible sessions feed
        // both Search (empty query lists recents) and Resume.
        vc.update(|_, cx| {
            baaz.update(cx, |harness, _| {
                for (n, id) in ["s-arrow-1", "s-arrow-2", "s-arrow-3"].iter().enumerate() {
                    harness.sessions.push(SessionEntry {
                        id: id.to_string(),
                        label: format!("Arrow session {n}"),
                        updated: chrono::Local::now(),
                        running: false,
                        turns: 3,
                        hidden: false,
                        pinned: false,
                        archived: false,
                        description: "the ask".into(),
                        replayed: false,
                        named: true,
                        needs_title: false,
                        title_pending: false,
                        last_ask: Some("the ask".into()),
                        local: false,
                        provisional: false,
                        workspace: None,
                        project: None,
                        project_name: None,
                        attention: Vec::new(),
                        approval_command: None,
                        pending_question: None,
                        turn_started: None,
                        last_error: None,
                        branch: None,
                        terminals_running: 0,
                    });
                }
                harness.invalidate_list();
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
        let query = vc.update(|_, cx| baaz.read(cx).commands_query.read(cx).value().to_string());
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
                harness.commands_query.update(cx, |field, cx| field.set_value(String::new(), window, cx));
            })
        });
        vc.simulate_input("brow");
        assert_eq!(selected(vc), Some(0));
        vc.simulate_keystrokes("enter");
        let (open, kind) =
            vc.update(|_, cx| (baaz.read(cx).layout.right_open, baaz.read(cx).layout.right_kind));
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

    /// Quitting names the running command, and counts the ones past it.
    #[test]
    fn the_quit_confirmation_names_the_running_command() {
        let dialog = Harness::quit_terminal_dialog("pnpm vitest", 0);
        assert_eq!(dialog.action, DialogAction::QuitWithRunningTerminal);
        assert_eq!(dialog.primary, "Quit");
        assert!(dialog.detail.contains("pnpm vitest"), "it names the command");
        let crowded = Harness::quit_terminal_dialog("pnpm vitest", 2);
        assert!(crowded.detail.contains("pnpm vitest"), "still names one command");
        assert!(crowded.detail.contains('3'), "and counts all three");
    }

    /// The Commands filter reads the slash and the description,
    /// case-insensitively: `brow` is `/browser`, `reasoning` is a
    /// description word only, nonsense is nothing, empty is all 23.
    #[test]
    fn the_command_filter_matches_slash_and_description() {
        assert_eq!(filter_commands("brow"), vec![Command::RightBrowser]);
        assert_eq!(filter_commands("BROW"), vec![Command::RightBrowser]);
        assert_eq!(filter_commands("reasoning"), vec![Command::Effort]);
        assert!(filter_commands("zzz-no-such-command").is_empty());
        assert_eq!(filter_commands("").len(), 23, "an empty query lists every command");
        assert_eq!(filter_commands("   ").len(), 23, "whitespace is an empty query");
    }

    /// `Commands` owns a query editor, and the drawn rows follow it: the
    /// field opens empty with all 23 rows, and setting it to `brow`
    /// leaves `/browser` alone — the same rows the keyboard walks.
    #[gpui::test]
    fn the_command_palette_has_a_query_field_and_filters(cx: &mut gpui::TestAppContext) {
        let state = hermetic_state("commands-query");
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let baaz = vc.update(|window, cx| {
            cx.new(|cx| Harness::new(test_args(&state.2), crate::shot::CaptureToken::default(), window, cx))
        });
        assert!(vc.update(|_, cx| baaz.read(cx).commands_query.read(cx).value().is_empty()));
        let empty = vc.update(|_, cx| {
            let app: &gpui::App = cx;
            baaz.read(app).palette_rows(PaletteKind::Commands, app).len()
        });
        assert_eq!(empty, 23, "an empty query lists every command");
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.commands_query.update(cx, |field, cx| field.set_value("brow", window, cx));
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
}
