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

use aui::keys::{Cancel, Confirm, SelectNext, SelectPrev};
use aui::nav::{folder_drop_card, view_menu, MenuRow};
use aui::overlay::{
    anchored_menu, command_palette, dialog, popover_layer, DialogKind, MenuAlign, MenuSide, PaletteIcon,
    PaletteItem, PaletteSection,
};
use aui_icons::IconName;
use aui_tokens::scale;
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Focusable, SharedString, Window,
};
use gpui_kit::component::input::Textarea;

use crate::app::{Harness, Wire, PALETTE_ROWS, PALETTE_SCRIM, PALETTE_TOP, TOAST_STACK_H, TOAST_TOP, TOAST_W};
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

    /// The two lists the `/` and `@` menus are built from, walked per root
    /// on the background executor: at boot, when a session view for a root
    /// opens and the cache lacks that root, and when a new session starts.
    ///
    /// Neither is on the wire: skills reach MSP only as `toolCall` items, and
    /// a mention is plain text inside the prompt (research §1.5). Skills are
    /// listed with the root as the working directory.
    pub(crate) fn load_menu_sources(&mut self, root: std::path::PathBuf, cx: &mut Context<Self>) {
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
                crate::harness_log!(
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

    /// The native folder panel, directories only: a chosen folder is adopted
    /// exactly like a recent workspace. Cancel does nothing.
    ///
    /// The three `harness:` lines are the diagnosis trail for "Choose
    /// folder… does nothing": the select line says the row fired, the entry
    /// line says the panel was asked for, and the resolution line says what
    /// the panel answered. They stay because this will regress.
    pub(crate) fn choose_project_folder(&mut self, cx: &mut Context<Self>) {
        crate::harness_log!("choose_project_folder: opening the folder panel");
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
                    crate::harness_log!(
                        "choose_project_folder: panel chose {}",
                        paths.first().map(|p| p.display().to_string()).unwrap_or_default()
                    );
                    let _ = this.update(cx, |this, cx| {
                        if let Some(root) = paths.first() {
                            this.adopt_root(root, cx);
                        }
                    });
                }
                Ok(Ok(None)) => crate::harness_log!("choose_project_folder: panel cancelled"),
                Ok(Err(error)) => crate::harness_log!("choose_project_folder: panel errored: {error:#}"),
                Err(_) => crate::harness_log!("choose_project_folder: panel future dropped"),
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
        // The harness's own state directory holds the tier probe's throwaway
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
            PaletteKind::Commands => Command::ALL
                .into_iter()
                .map(|c| (c.slash().into(), c.slash().into(), c.description().into()))
                .collect(),
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
            crate::harness_log!("palette confirm: nothing at {selected} of {} rows", rows.len());
            // The Add section's lead is a card, not a row, so an empty
            // Projects palette still offers the panel on ↩.
            if kind == PaletteKind::Projects {
                self.choose_project_folder(cx);
            }
            return;
        };
        crate::harness_log!("palette select (keyboard): {kind:?} {id}");
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
                    self.with_session(cx, |view, cx| view.run_command(command, window, cx));
                }
            }
        }
        cx.notify();
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
            PaletteKind::Commands => (
                SharedString::from(""),
                SharedString::from("Every command in this build"),
                vec![PaletteSection::new(
                    "Commands",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Slash)),
                )],
            ),
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
            crate::harness_log!("palette select (click): {kind:?} {id}");
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

    pub(crate) fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read the modal out whole before anything asks `cx` for a listener:
        // the entity's borrow and `cx.listener` cannot be alive at once.
        let (title, detail, kind, primary_label, action, danger) = {
            let modal = self.overlays.read(cx).dialog.as_ref()?;
            let danger = modal.action == DialogAction::Archive || modal.action == DialogAction::RemoveProject;
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
            }
            cx.notify();
        });
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
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

    /// Harness → About Harness.
    pub(crate) fn show_about(&mut self, cx: &mut Context<Self>) {
        self.set_dialog(
            cx,
            Dialog {
                title: "About Harness".into(),
                detail: format!(
                    "Harness {} \u{2014} a macOS chat interface to Muse Code.\n\nKeys: docs/08-keymap.md. App: docs/02-app.md.",
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
    use super::tilde_root;

    #[test]
    fn home_folds_to_a_tilde_and_other_roots_stand() {
        // HOME is read, never written: safe beside parallel tests.
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            assert_eq!(tilde_root(&format!("{home}/Projects/harness")), "~/Projects/harness");
            assert_eq!(tilde_root(&home), "~");
        }
        assert_eq!(tilde_root("/private/tmp/h4ws"), "/private/tmp/h4ws");
    }
}
