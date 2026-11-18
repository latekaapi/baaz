//! The project menu: a group row's `…` tray, the Skills header, and the
//! scripted steps.
//!
//! Part of the [`Harness`] entity; see [`crate::app`]
//! for what it owns. The menu wears the Sessions view menu's shape (`view_menu`,
//! 250 wide) and seats through `aui::overlay::anchored_menu` at the group
//! row's tray `…` button (where the pointer stood when the `…` was pressed,
//! recorded by the sidebar pane in `SidebarPane::group_menu_bounds`). The
//! Colour submenu
//! hangs off the menu's right edge at the Colour row's height through a
//! second `anchored_menu`, so it flips left near the window edge.
//!
//! One [`MenuRow::Toggle`] per project in sidebar order, checked for the
//! menu's project; picking one moves an unsent draft's content into that
//! project's draft session (started there if needed) and otherwise starts a
//! sibling session there. Then "New session here", "Rename project",
//! a "Colour" submenu of eight swatches, pin, reveal, and removal.

use aui::nav::{view_menu, view_submenu_rows, MenuRow};
use aui::overlay::{anchored_menu, MenuAlign, MenuSide};
use aui_tokens::ActiveAui;
use gpui::{div, prelude::*, AnyElement, Context, Focusable, Window};

use crate::app::Harness;
use crate::overlays::{Menu, MenuKind};
use crate::projects;
use crate::steps::StepResult;

/// The eight label-ramp slots as the Colour submenu names them, in token
/// order (`label-1` … `label-8`, red through pink).
pub(crate) const COLOUR_NAMES: [&str; 8] = ["Red", "Orange", "Yellow", "Green", "Teal", "Blue", "Indigo", "Pink"];

/// One row of the project menu, in row order.
#[derive(Clone)]
enum ProjectMenuAction {
    /// Switch to this project, replacing an empty unnamed session or
    /// starting a sibling there.
    Pick(String),
    /// Start a session in the menu's project.
    NewHere,
    /// Rename the menu's project through the header's rename field.
    Rename,
    /// Open or close the Colour submenu (the swatches hang off the menu
    /// with their own handler).
    Colour,
    /// Pin or unpin the menu's project.
    Pin,
    /// Reveal the menu project's root in Finder.
    Reveal,
    /// Ask before removing the menu's project from the sidebar.
    Remove,
    /// The unfiled row: adopt through the Projects palette.
    AddAsProject,
}

impl Harness {
    /// Open a project menu: a header or palette surface (`from_header`),
    /// or a group row's `…` tray (`project` is `None` when the session is
    /// unfiled). `from_header` no longer carries its own trigger rect —
    /// the menu seats at the group row either way — but the toggle still
    /// tells a header open apart from a group tray open.
    /// Clicking the same affordance again closes it, like every other menu.
    pub(crate) fn open_project_menu(&mut self, project: Option<String>, from_header: bool, cx: &mut Context<Self>) {
        let already = self
            .overlays
            .read(cx)
            .menu()
            .is_some_and(|m| m.kind == MenuKind::Project && m.project == project && m.project_header == from_header);
        // One menu at a time, as `open_menu`: an open `+` menu closes in the
        // session view that draws it.
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.close_plus_menu(cx));
        }
        self.overlays.update(cx, |overlays, _| {
            if already {
                overlays.close_menu();
            } else {
                overlays.open(Menu::project(project, from_header));
            }
        });
        cx.notify();
    }

    /// Adopt every dropped directory, the first becoming current: the
    /// folder card and the hero's drop land here. Files never arrive (the
    /// card filters them) and missing paths are skipped, so a sloppy drop
    /// adopts what it can.
    pub(crate) fn adopt_dropped(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        let dirs: Vec<std::path::PathBuf> = paths.into_iter().filter(|p| p.is_dir()).collect();
        if dirs.is_empty() {
            return;
        }
        crate::baaz_log!("adopting {} dropped folder{}", dirs.len(), if dirs.len() == 1 { "" } else { "s" });
        let mut first: Option<String> = None;
        for root in &dirs {
            let id = self.adopt_root(root, cx);
            if first.is_none() {
                first = Some(id);
            }
        }
        // `adopt_root` leaves the last adoption current; the drop's first
        // folder is the one the person aimed at.
        if let Some(id) = first {
            self.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
            self.current_project = Some(id);
            projects::write(&self.session_list.read(cx).projects);
            self.note_project_changed(cx);
        }
    }

    /// Adopt `root` into the sidebar and make it current, without starting
    /// a session there: adopting is not opening. Returns the project id.
    pub(crate) fn adopt_root(&mut self, root: &std::path::Path, cx: &mut Context<Self>) -> String {
        let id = self.session_list.update(cx, |list, _| list.projects.add(root).id.clone());
        self.session_list.update(cx, |list, _| list.projects.touch(&id));
        self.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
        self.current_project = Some(id.clone());
        projects::write(&self.session_list.read(cx).projects);
        self.note_project_changed(cx);
        self.rejoin(cx);
        self.invalidate_list(cx);
        cx.notify();
        id
    }

    /// The adoptions in sidebar order: pinned first, then name, never
    /// recency — minus the adoptions whose root is gone.
    /// What the project menu and the Projects palette both list.
    pub(crate) fn ordered_projects(&self, cx: &gpui::App) -> Vec<crate::projects::Project> {
        self.session_list.read(cx).projects.sorted_available().into_iter().cloned().collect()
    }

    /// The current project just changed: drop the session tab the Browse
    /// centre held and the address field (its tabs, sessions and rows belong
    /// to the old project). Window-free, for the adoption paths that own no
    /// window.
    pub(crate) fn note_project_changed(&mut self, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, _| browser.browse_session_tab = None);
        self.browser.update(cx, |browser, cx| browser.drop_browse_address(cx));
        cx.notify();
    }

    /// Pick a project from its menu row: when the active session is an
    /// unsent draft and another project is picked, the draft's content moves
    /// into the picked project's draft session (started there if needed) —
    /// the server prunes zero-turn sessions on relaunch, so the old id
    /// simply leaves the draft map. A session with turns instead gets a
    /// sibling there, as before.
    fn pick_project(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(active) = self.active_id(cx) {
            // Only a registered draft moves: a session with a turn in
            // flight already left, and a replayed capture is not ours.
            let registered = self.open_state.names_draft(&active);
            let sending =
                self.active.as_ref().is_some_and(|view| view.read(cx).rt(cx).session_id == active && view.read(cx).rt(cx).is_sending());
            let project_of = self
                .session_list.read(cx).sessions
                .iter()
                .find(|e| e.id == active)
                .and_then(|e| e.project.clone())
                .or_else(|| self.current_project_id());
            if registered && self.is_live_draft(&active, cx) && !sending && project_of.as_deref() != Some(id.as_str()) {
                let moving = self
                    .active
                    .clone()
                    .filter(|view| view.read(cx).rt(cx).session_id == active)
                    .map(|view| view.update(cx, |view, vc| view.take_draft(window, vc)));
                self.open_state.forget_draft(&active);
                // A rowless draft has no row to hide; a rowed one keeps the
                // old rule so it never strands a visible empty row.
                if self.session_list.read(cx).sessions.iter().any(|e| e.id == active) {
                    self.set_override(&active, |meta| meta.hidden = true, cx);
                }
                self.open_state.pending_draft = moving;
            }
        }
        self.new_session_in(Some(id), window, cx);
    }

    /// Rename through the header's dense field: the menu's project becomes
    /// current (its name is what the field shows), the field is seeded with
    /// it, and `ConfirmRename` commits through [`Self::commit_project_rename`].
    /// Renaming never touches the project: opening the field must not
    /// reorder the groups.
    fn start_project_rename(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(project) = self.session_list.read(cx).projects.find(&id).cloned() else { return };
        self.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
        self.current_project = Some(id.clone());
        projects::write(&self.session_list.read(cx).projects);
        self.note_project_changed(cx);
        self.rename.update(cx, |state, cx| state.set_value(project.name, window, cx));
        let entry = Self::rename_entry(crate::overlay_stack::Layer::RenameProject(id));
        self.overlays.update(cx, |o, _| o.stack.push(entry));
        window.focus(&self.rename.focus_handle(cx), cx);
        cx.notify();
    }

    /// Ask before removing a project from the sidebar: a danger dialog in
    /// the archive dialog's shape, carrying the project id so only its own
    /// Remove button can confirm it. Undo is not offered — re-adding is one
    /// click in the Projects palette.
    pub(crate) fn open_remove_project_dialog(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(project) = self.session_list.read(cx).projects.find(&id).cloned() else { return };
        let n = self.session_list.read(cx).sessions.iter().filter(|e| e.project.as_deref() == Some(id.as_str())).count();
        let detail = if n == 1 {
            "Its 1 session stays on disk and moves to Unfiled. Nothing in the folder changes.".to_owned()
        } else {
            format!("Its {n} sessions stay on disk and move to Unfiled. Nothing in the folder changes.")
        };
        self.set_dialog(
            cx,
            crate::overlays::Dialog {
                title: format!("Remove {} from the sidebar?", project.name),
                detail,
                kind: aui::overlay::DialogKind::Warning,
                primary: "Remove",
                action: crate::overlays::DialogAction::RemoveProject,
                archive_target: Some(id),
            },
        );
    }

    /// The removal dialog's Remove button, or the `remove-confirm` step:
    /// forget the adoption, clear the project off its sessions (so a later
    /// re-add resolves them by root), hand current to the most recently
    /// opened remaining adoption whose root is on disk, and regroup.
    pub(crate) fn confirm_remove_project(&mut self, cx: &mut Context<Self>) {
        let target = self.overlays.read(cx).dialog().and_then(|d| {
            (d.action == crate::overlays::DialogAction::RemoveProject)
                .then(|| d.archive_target.clone())
                .flatten()
        });
        let Some(id) = target else { return };
        self.close_dialog(cx);
        if !self.session_list.update(cx, |list, _| list.projects.remove(&id)) {
            return;
        }
        self.session_list.update(cx, |list, _| list.branches.remove(&id));
        // A removed project takes its unsent draft with it: the parked view
        // stays cached but becomes evictable, and ⌘N can never reopen it.
        self.open_state.drafts.remove(&id);
        // The current project is gone: the most recently opened remaining
        // adoption takes it, or nothing does.
        if self.current_project.as_deref() == Some(id.as_str()) {
            let next = self.session_list.read(cx).projects.most_recent_available().map(|p| p.id.clone());
            self.session_list.update(cx, |list, _| list.projects.current = next.clone());
            self.current_project = next;
            self.note_project_changed(cx);
        }
        projects::write(&self.session_list.read(cx).projects);
        // The rows re-resolve by root now, settled once for the whole batch.
        self.clear_session_projects(&id, cx);
        self.invalidate_list(cx);
        cx.notify();
    }

    /// Commit whatever is in the rename field as the project rename: an
    /// empty name reverts to the folder name (a project is always called
    /// something), and a rename never touches the folder.
    pub(crate) fn commit_project_rename(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.renaming_project(cx) else { return };
        self.overlays.update(cx, |o, _| {
            o.stack.close(crate::overlay_stack::LayerKind::RenameProject);
        });
        let text = self.rename.read(cx).value().to_string();
        let text = text.trim().to_owned();
        self.session_list.update(cx, |list, _| {
            if let Some(project) = list.projects.projects.iter_mut().find(|p| p.id == id) {
                project.name = if text.is_empty() {
                    project
                        .root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| project.root.to_string_lossy().into_owned())
                } else {
                    text
                };
            }
        });
        projects::write(&self.session_list.read(cx).projects);
        self.invalidate_list(cx);
        self.focus_composer = true;
        cx.notify();
    }

    /// `projects`: the Projects palette, over whatever is open.
    pub(crate) fn step_projects(&mut self, window: &mut Window, cx: &mut Context<Self>) -> StepResult {
        self.open_projects(false, window, cx);
        Ok(())
    }

    /// `project:<path>`: adopt the path and make it current — no panel, no
    /// session. Relative paths resolve against the launch directory, like
    /// the fixture's roots. An empty path, or one that is not a directory
    /// on disk, fails the step.
    pub(crate) fn step_adopt(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let rest = rest.trim();
        if rest.is_empty() {
            return Err("no project path".into());
        }
        let path = std::path::PathBuf::from(rest);
        let path =
            if path.is_absolute() { path } else { std::env::current_dir().unwrap_or_default().join(path) };
        if !path.is_dir() {
            return Err(format!("`{}` is not a directory", path.display()));
        }
        self.adopt_root(&path, cx);
        Ok(())
    }

    /// `project-menu`: the current project's menu, seated at its group row.
    /// `project-menu:<name>`: the group row menu for the project named.
    /// No current project, or no project by that name, fails the step.
    pub(crate) fn step_project_menu(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let name = rest.trim();
        if name.is_empty() {
            let id = self.current_project_id().ok_or("no current project")?;
            self.open_project_menu(Some(id), true, cx);
        } else {
            let id = self
                .session_list.read(cx).projects
                .projects
                .iter()
                .find(|p| p.name == name)
                .map(|p| p.id.clone())
                .ok_or_else(|| format!("no project named `{name}`"))?;
            self.open_project_menu(Some(id), false, cx);
        }
        Ok(())
    }

    /// `project-close:<name>`: close the group of the project named, so its
    /// header shows the strongest state of the sessions it hides (task
    /// 3.2). A setter, never a toggle: a capture's second run finds it
    /// closed and changes nothing. No project by that name fails the step.
    pub(crate) fn step_project_close(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let name = rest.trim();
        let id = self
            .session_list.read(cx).projects
            .projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("no project named `{name}`"))?;
        if !crate::layout::Layout::get(cx).closed_groups.iter().any(|g| g == &id) {
            self.toggle_group(id, cx);
        }
        Ok(())
    }

    /// `project-colour:<n>`: the current project's colour slot, 1–8.
    /// Anything else is not a slot and fails the step, as does no current
    /// project. With no payload, open the Colour submenu instead (opening
    /// the current project's menu first when no project menu is open), so
    /// the submenu has a scripted entry for its screenshots; with no
    /// project menu open and no current project to open one on, that fails
    /// the step too.
    pub(crate) fn step_project_colour(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        if rest.trim().is_empty() {
            if !self.overlays.read(cx).is_open(MenuKind::Project) {
                let id = self.current_project_id().ok_or("no project menu is open and no current project")?;
                self.open_project_menu(Some(id), true, cx);
            }
            self.overlays.update(cx, |overlays, _| {
                if let Some(menu) = overlays.menu_mut().filter(|menu| menu.kind == MenuKind::Project) {
                    menu.sub_open = true;
                }
            });
            cx.notify();
            return Ok(());
        }
        let slot = rest.trim().parse::<u8>().ok().filter(|slot| (1..=8).contains(slot));
        let slot = slot.ok_or_else(|| format!("`{rest}` is not a colour slot 1–8"))?;
        let id = self.current_project_id().ok_or("no current project")?;
        self.session_list.update(cx, |list, _| {
            let project = list
                .projects
                .projects
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or_else(|| format!("current project `{id}` is not adopted"))?;
            project.colour = slot;
            projects::write(&list.projects);
            Ok::<(), String>(())
        })?;
        self.invalidate_list(cx);
        cx.notify();
        Ok(())
    }

    /// `group-by:<date|project>`: persist the sidebar grouping and regroup.
    /// Any other payload fails the step.
    pub(crate) fn step_group_by(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let mode = match rest.trim() {
            "date" => crate::layout::GroupBy::Date,
            "project" => crate::layout::GroupBy::Project,
            other => return Err(format!("unknown grouping `{other}`; known groupings are date, project")),
        };
        crate::layout::Layout::update(cx, |layout| layout.group_by = Some(mode));
        self.invalidate_list(cx);
        Ok(())
    }

    /// `group-bar`: flip the current-project accent bar flag and persist it.
    /// The flag no longer draws anything (see `Layout::group_bar`).
    pub(crate) fn step_group_bar(&mut self, cx: &mut Context<Self>) -> StepResult {
        crate::layout::Layout::update(cx, |layout| layout.group_bar = !layout.group_bar);
        self.invalidate_list(cx);
        Ok(())
    }

    /// `group-branch`: flip the trailing-branch flag and persist it, as the
    /// Settings › Sidebar switch does.
    pub(crate) fn step_group_branch(&mut self, cx: &mut Context<Self>) -> StepResult {
        crate::layout::Layout::update(cx, |layout| layout.group_branch = !layout.group_branch);
        self.invalidate_list(cx);
        Ok(())
    }

    /// `remove-project:<name>`: raise the removal dialog for the project
    /// named. No project by that name fails the step.
    pub(crate) fn step_remove_project(&mut self, rest: &str, cx: &mut Context<Self>) -> StepResult {
        let name = rest.trim();
        let id = self
            .session_list.read(cx).projects
            .projects
            .iter()
            .find(|p| p.name == name)
            .map(|p| p.id.clone())
            .ok_or_else(|| format!("no project named `{name}`"))?;
        self.open_remove_project_dialog(id, cx);
        Ok(())
    }

    /// `remove-confirm`: confirm the open removal dialog. No removal
    /// dialog open, or one whose project is no longer adopted, fails the
    /// step.
    pub(crate) fn step_remove_confirm(&mut self, cx: &mut Context<Self>) -> StepResult {
        let target = self.overlays.read(cx).dialog().and_then(|d| {
            (d.action == crate::overlays::DialogAction::RemoveProject)
                .then(|| d.archive_target.clone())
                .flatten()
        });
        let id = target.ok_or("no removal dialog is open")?;
        if self.session_list.read(cx).projects.find(&id).is_none() {
            return Err(format!("project `{id}` is not adopted"));
        }
        self.confirm_remove_project(cx);
        Ok(())
    }

    /// The project menu, anchored below-start of the group row's tray `…`
    /// for its project (below-start of the unfiled row for `None`).
    /// Escape and click-outside close it like the other menus: Escape
    /// through the overlay stack, and there is no scrim, exactly as the
    /// view menu does it.
    pub(crate) fn render_project_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (target, from_header) = {
            let menu = self.overlays.read(cx).menu()?;
            if menu.kind != MenuKind::Project {
                return None;
            }
            (menu.project.clone(), menu.project_header)
        };
        let colour_open = self.overlays.read(cx).menu().is_some_and(|menu| menu.kind == MenuKind::Project && menu.sub_open);
        let mut rows: Vec<MenuRow> = Vec::new();
        let mut actions: Vec<Option<ProjectMenuAction>> = Vec::new();
        let mut push = |row: MenuRow, action: Option<ProjectMenuAction>| {
            rows.push(row);
            actions.push(action);
        };
        // The project rows in sidebar order, hoisted: the Colour submenu's
        // height inside the menu counts them.
        let listed = self.ordered_projects(cx);
        let listed_len = listed.len();
        match target.clone() {
            // The unfiled group carries one row: adopting starts in the
            // Projects palette.
            None => push(
                MenuRow::Toggle { label: "Add as project…".into(), checked: false },
                Some(ProjectMenuAction::AddAsProject),
            ),
            Some(id) => {
                for project in listed {
                    let checked = project.id == id;
                    let pid = project.id.clone();
                    push(
                        MenuRow::Toggle { label: project.name.into(), checked },
                        Some(ProjectMenuAction::Pick(pid)),
                    );
                }
                let pinned = self.session_list.read(cx).projects.find(&id).is_some_and(|p| p.pinned);
                let colour = self.session_list.read(cx).projects.find(&id).map(|p| p.colour).unwrap_or(1);
                let colour_name = COLOUR_NAMES.get(colour.saturating_sub(1) as usize).unwrap_or(&"?");
                push(MenuRow::Separator, None);
                push(MenuRow::Toggle { label: "New session here".into(), checked: false }, Some(ProjectMenuAction::NewHere));
                push(MenuRow::Toggle { label: "Rename project".into(), checked: false }, Some(ProjectMenuAction::Rename));
                push(
                    MenuRow::Submenu { label: "Colour".into(), value: (*colour_name).into(), highlighted: colour_open },
                    Some(ProjectMenuAction::Colour),
                );
                push(
                    MenuRow::Toggle {
                        label: (if pinned { "Unpin project" } else { "Pin project" }).into(),
                        checked: false,
                    },
                    Some(ProjectMenuAction::Pin),
                );
                push(MenuRow::Toggle { label: "Reveal in Finder".into(), checked: false }, Some(ProjectMenuAction::Reveal));
                push(MenuRow::Separator, None);
                push(
                    MenuRow::Toggle { label: "Remove from sidebar".into(), checked: false },
                    Some(ProjectMenuAction::Remove),
                );
            }
        }
        let target_for = target.clone();
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            let action = actions.get(*index).cloned().flatten();
            // The Colour row only opens its submenu; every other row
            // dismisses the menu before acting.
            let keep = matches!(action, Some(ProjectMenuAction::Colour));
            if !keep {
                this.overlays.update(cx, |overlays, _| overlays.close_menu());
            }
            match action {
                Some(ProjectMenuAction::Pick(id)) => this.pick_project(id, window, cx),
                Some(ProjectMenuAction::NewHere) => {
                    if let Some(id) = target_for.clone() {
                        this.new_session_in(Some(id), window, cx);
                    }
                }
                Some(ProjectMenuAction::Rename) => {
                    if let Some(id) = target_for.clone() {
                        this.start_project_rename(id, window, cx);
                    }
                }
                Some(ProjectMenuAction::Colour) => {
                    this.overlays.update(cx, |overlays, _| {
                        if let Some(menu) = overlays.menu_mut().filter(|menu| menu.kind == MenuKind::Project) {
                            menu.sub_open = !menu.sub_open;
                        }
                    });
                    cx.notify();
                }
                Some(ProjectMenuAction::Pin) => {
                    if let Some(id) = target_for.clone() {
                        this.session_list.update(cx, |list, _| {
                            if let Some(project) = list.projects.projects.iter_mut().find(|p| p.id == id) {
                                project.pinned = !project.pinned;
                            }
                        });
                        projects::write(&this.session_list.read(cx).projects);
                        this.invalidate_list(cx);
                        cx.notify();
                    }
                }
                Some(ProjectMenuAction::Reveal) => {
                    if let Some(root) =
                        target_for.clone().and_then(|id| this.session_list.read(cx).projects.find(&id).map(|p| p.root.clone()))
                    {
                        cx.reveal_path(&root);
                    }
                }
                Some(ProjectMenuAction::Remove) => {
                    if let Some(id) = target_for.clone() {
                        this.open_remove_project_dialog(id, cx);
                    }
                }
                Some(ProjectMenuAction::AddAsProject) => {
                    this.tasks.push(cx.spawn(async move |this, cx| {
                        let _ = this.update_in(cx, |this, window, cx| this.open_projects(false, window, cx));
                    }));
                }
                None => {}
            }
        });
        // The seat: below-start of the group row's tray `…` for the menu's
        // project (`None` is the unfiled row, keyed under its group id).
        // `anchored_menu` flips the side and slides inside the window on
        // overflow. No bounds yet: no menu this frame, never the old
        // `top(52)` seat; the open notifies so the menu appears once its
        // row reports.
        let key = target.clone().unwrap_or_else(|| crate::sidebar::OTHER_GROUP.to_owned());
        let trigger = self.sidebar_pane.read(cx).group_menu_bounds.get(&key).copied();
        let Some(trigger) = trigger else {
            crate::baaz_log!("project menu: no trigger bounds for {key}");
            if from_header || crate::layout::Layout::get(cx).sidebar_open {
                cx.notify();
            }
            return None;
        };
        // A click anywhere outside closes the menu and its colour submenu:
        // the catcher is a sibling of the menu inside the same deferred
        // draw, so it covers the window without covering the menu — the same
        // shape the composer's chip pickers use.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.close_menu());
            cx.notify();
        });
        let catcher = div()
            .id("project-menu-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .on_click(move |_, w, cx| dismiss(&(), w, cx));
        // The menu reports its own rect (its only child): the Colour submenu
        // anchors off the menu's right edge at the Colour row's height.
        let menu_report = cx.entity().downgrade();
        let menu = div()
            .on_children_prepainted(move |bounds, _, cx| {
                if let Some(first) = bounds.first() {
                    let bounds = *first;
                    let _ = menu_report.update(cx, |this, cx| {
                        Harness::note_trigger_bounds(&mut this.project_menu_bounds, bounds, cx);
                    });
                }
            })
            .child(view_menu("project-menu", rows).at_rest().on_activate(move |i, w, cx| {
                activate(&i, w, cx)
            }));
        let mut overlay = div()
            .absolute()
            .inset_0()
            .child(catcher)
            .child(anchored_menu(trigger, MenuSide::Below, MenuAlign::Start, menu));
        // The Colour submenu hangs off the menu's right edge at the Colour
        // row's height: a zero-size trigger there seats the submenu's
        // top-left corner one gap right and down, and `SwitchAnchor` flips it
        // left near the window edge. The row offset is the menu's top padding
        // plus every row above the Colour row — project toggles at the theme
        // row height, the separator at its 1 px + 2 × 6 px margins, then New
        // and Rename (the menu padding and separator metrics are
        // `aui::nav::view_menu` privates, read off
        // `crates/aui/src/nav/view_menu.rs`: `MENU_PAD`, `SEP_H`,
        // `SEP_MARGIN_Y`).
        if colour_open {
            if let (Some(id), Some(menu_bounds)) = (target, self.project_menu_bounds) {
                if let Some(current) = self.session_list.read(cx).projects.find(&id).map(|p| p.colour) {
                    let row_h = f32::from(cx.aui().metrics.row);
                    let row_top = f32::from(menu_bounds.origin.y)
                        + 6.0
                        + listed_len as f32 * row_h
                        + 13.0
                        + 2.0 * row_h;
                    let right =
                        f32::from(menu_bounds.origin.x) + f32::from(menu_bounds.size.width);
                    let sub_trigger = gpui::Bounds::new(
                        gpui::point(gpui::px(right), gpui::px(row_top)),
                        gpui::size(gpui::px(0.0), gpui::px(0.0)),
                    );
                    let p = cx.aui().colors;
                    let swatches: Vec<MenuRow> = (1u8..=8)
                        .map(|slot| MenuRow::Swatch {
                            label: COLOUR_NAMES[(slot - 1) as usize].into(),
                            colour: p.label(slot - 1),
                            checked: slot == current,
                        })
                        .collect();
                    let pick = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
                        if let Some(slot) = (1u8..=8).nth(*index) {
                            this.session_list.update(cx, |list, _| {
                                if let Some(project) = list.projects.projects.iter_mut().find(|p| p.id == id) {
                                    project.colour = slot;
                                }
                            });
                            projects::write(&this.session_list.read(cx).projects);
                            this.invalidate_list(cx);
                            cx.notify();
                        }
                    });
                    overlay = overlay.child(anchored_menu(
                        sub_trigger,
                        MenuSide::Below,
                        MenuAlign::Start,
                        view_submenu_rows("project-colour", swatches)
                            .at_rest()
                            .on_activate(move |i, w, cx| pick(&i, w, cx)),
                    ));
                }
            }
        }
        Some(overlay.into_any_element())
    }
}

#[cfg(test)]
mod tests {
    use super::COLOUR_NAMES;

    #[test]
    fn the_colour_submenu_names_all_eight_slots() {
        assert_eq!(COLOUR_NAMES.len(), 8);
        let mut names = COLOUR_NAMES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 8);
    }
}
