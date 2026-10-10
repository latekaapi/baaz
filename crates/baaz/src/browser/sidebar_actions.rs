//! What the Browse sidebar's rows, menus and name fields do (the approved
//! canvas "Baaz sidebar v2", Browse round, v22): spaces, your folders,
//! task folders, the tab / folder / task-folder / space menus, Ask, close
//! all. The screen reports intents; [`Harness::browse_intent`] in
//! `sidebar_view.rs` routes each here.

use aui::screens::sidebar::{BrowseMenu, BrowseNaming, FolderAction, SpaceAction, TabAction, TaskFolderAction};
use gpui::{ClipboardItem, Context, Focusable, Window};

use super::tabs::{self, TabId};
use super::{Owner, HOME_KEY};
use crate::app::Harness;

/// A folder's name until the person types one.
const NEW_FOLDER: &str = "New folder";

impl Harness {
    /// The person's tab ids in the current space, Today and folders alike,
    /// pinned ones excluded unless `pinned`.
    fn space_person_tabs(&self, pinned: bool) -> Vec<TabId> {
        let space = self.browse_space().unwrap_or_default();
        self.browser
            .tabs
            .ordered_tabs(&Owner::Person, Some(&space))
            .iter()
            .filter(|tab| pinned || !tab.pinned)
            .map(|tab| tab.id)
            .collect()
    }

    /// Close `ids`, each through its owner's path (a person tab's close
    /// picks the neighbour; closing the space's last one leaves a fresh
    /// blank tab, as the × always has).
    fn close_tabs(&mut self, ids: Vec<TabId>, window: &mut Window, cx: &mut Context<Self>) {
        for id in ids {
            self.browse_sidebar_tab(id, true, window, cx);
        }
        self.schedule_tabs_write(cx);
        cx.notify();
    }

    /// Open the right-click menu (one at a time) and keep the page from
    /// taking the next click.
    pub(crate) fn open_browse_menu(&mut self, menu: BrowseMenu, cx: &mut Context<Self>) {
        self.browse_naming = None;
        self.browse_menu = Some(menu);
        cx.notify();
    }

    /// Close the right-click menu, if one is open.
    pub(crate) fn close_browse_menu(&mut self, cx: &mut Context<Self>) {
        if self.browse_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Put a name field on `target`, seeded with `draft` and focused: the
    /// sidebar's one rename editor serves folders and spaces too.
    pub(crate) fn start_browse_naming(&mut self, target: BrowseNaming, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_menu = None;
        self.renaming = None;
        self.rename.update(cx, |state, cx| state.set_value(draft.to_owned(), window, cx));
        self.browse_naming = Some(target);
        window.focus(&self.rename.focus_handle(cx), cx);
        cx.notify();
    }

    /// Commit a name field. A blank folder name keeps "New folder"; a blank
    /// new space is "Space N"; a blank rename keeps the old name.
    pub(crate) fn commit_browse_name(&mut self, target: BrowseNaming, text: &str, cx: &mut Context<Self>) {
        self.browse_naming = None;
        let text = text.trim();
        let changed = match target {
            BrowseNaming::Folder(id) => self.browser.tabs.rename_folder(&id, text),
            BrowseNaming::Space(Some(id)) => self.browser.tabs.rename_space(&id, text),
            BrowseNaming::Space(None) => {
                self.browser.tabs.add_space(text);
                true
            }
        };
        if changed {
            self.schedule_tabs_write(cx);
        }
        self.focus_composer = true;
        cx.notify();
    }

    /// Abandon a name field: the folder keeps whatever name it had.
    pub(crate) fn cancel_browse_name(&mut self, cx: &mut Context<Self>) {
        if self.browse_naming.take().is_some() {
            cx.notify();
        }
    }

    /// Today's "new folder" (and Move to folder › New folder): a folder
    /// at the top of the current space's list, named in place, optionally
    /// taking `tab` with it.
    pub(crate) fn browse_new_folder(&mut self, tab: Option<TabId>, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let space = self.browse_space().unwrap_or_default();
        let id = self.browser.tabs.add_folder(&space, NEW_FOLDER);
        if let Some(tab) = tab {
            self.browser.tabs.set_folder(tab, Some(&id));
        }
        self.schedule_tabs_write(cx);
        self.start_browse_naming(BrowseNaming::Folder(id.into()), NEW_FOLDER, window, cx);
    }

    /// Today's "close all": every unpinned tab of yours in this space.
    pub(crate) fn browse_close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.space_person_tabs(false);
        self.close_tabs(ids, window, cx);
    }

    /// A space chip: make it current and show its active tab (or a new one).
    pub(crate) fn browse_switch_space(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.tabs.spaces.get(index).map(|space| space.id.clone()) else {
            return;
        };
        if !self.browser.tabs.switch_space(&id) {
            return;
        }
        self.schedule_tabs_write(cx);
        self.browse_session_tab = None;
        self.browser.centre_override = None;
        match self.browser.tabs.active_of(&Owner::Person, Some(&id)) {
            Some(tab) => {
                self.ensure_tab_page(HOME_KEY, tab, window, cx);
            }
            None => self.ensure_browse_tab(window, cx),
        }
        cx.notify();
    }

    /// The Ask button: a new session with the page as its unsent draft
    /// (the omnibox Ask's path), until the agent line lands (docs/27 R3).
    pub(crate) fn browse_ask_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.browse_centre_shown().and_then(|id| self.browser.tabs.get(id)).map(|tab| (tab.title.clone(), tab.url.clone()));
        let query = match shown {
            Some((title, url)) if url != tabs::BLANK && !url.is_empty() => {
                let title = tabs::tab_label(&title, &url);
                format!("About this page, {title} ({url}): ")
            }
            _ => "About this page: ".to_owned(),
        };
        self.browse_ask(&query, window, cx);
    }

    /// A tab menu row.
    pub(crate) fn browse_tab_action(&mut self, tab: TabId, action: TabAction, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_menu = None;
        self.ensure_tabs_loaded(cx);
        let Some(found) = self.browser.tabs.get(tab).cloned() else {
            cx.notify();
            return;
        };
        match action {
            TabAction::Duplicate => {
                let copy = self.browser.tabs.open(found.space.clone(), found.owner.clone(), &found.url, Some(tab));
                if let Some(folder) = found.folder.as_deref() {
                    self.browser.tabs.set_folder(copy, Some(folder));
                }
                self.schedule_tabs_write(cx);
                self.browse_sidebar_tab(copy, false, window, cx);
            }
            TabAction::CopyLink => {
                cx.write_to_clipboard(ClipboardItem::new_string(found.url.clone()));
                self.overlays.update(cx, |overlays, _| overlays.toast("Link copied", &found.url));
            }
            TabAction::TogglePin => {
                self.browser.tabs.set_pinned(tab, !found.pinned);
                self.schedule_tabs_write(cx);
            }
            TabAction::MoveToFolder(folder) => {
                self.browser.tabs.set_folder(tab, Some(folder.as_ref()));
                self.schedule_tabs_write(cx);
            }
            TabAction::MoveToNewFolder => self.browse_new_folder(Some(tab), window, cx),
            TabAction::RemoveFromFolder => {
                self.browser.tabs.set_folder(tab, None);
                self.schedule_tabs_write(cx);
            }
            TabAction::Close => self.browse_sidebar_tab(tab, true, window, cx),
            TabAction::CloseOthers => {
                let others: Vec<TabId> = self.space_person_tabs(false).into_iter().filter(|id| *id != tab).collect();
                self.close_tabs(others, window, cx);
            }
        }
        cx.notify();
    }

    /// A folder menu row.
    pub(crate) fn browse_folder_action(&mut self, folder: &str, action: FolderAction, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_menu = None;
        match action {
            FolderAction::Rename => {
                let name = self.browser.tabs.folders.iter().find(|f| f.id == folder).map(|f| f.name.clone()).unwrap_or_default();
                self.start_browse_naming(BrowseNaming::Folder(folder.to_owned().into()), &name, window, cx);
            }
            FolderAction::CloseTabs => {
                let space = self.browse_space().unwrap_or_default();
                let ids = self.browser.tabs.person_ids_in(&space, Some(folder));
                self.close_tabs(ids, window, cx);
            }
            FolderAction::Delete => {
                if self.browser.tabs.delete_folder(folder) {
                    self.schedule_tabs_write(cx);
                }
            }
        }
        cx.notify();
    }

    /// A task-folder menu row (and the head's click: Open chat).
    pub(crate) fn browse_task_folder_action(&mut self, session: &str, action: TaskFolderAction, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_menu = None;
        match action {
            TaskFolderAction::OpenChat => {
                self.set_mode(crate::layout::Mode::Sessions, window, cx);
                self.resume_quiet(session.to_owned(), window, cx);
            }
            TaskFolderAction::Stop => {
                if let Some(view) = self.find_view(session, cx) {
                    view.update(cx, |view, cx| view.interrupt(cx));
                }
            }
            TaskFolderAction::CloseTabs => {
                let owner = Owner::Session(session.to_owned());
                let ids: Vec<TabId> = self.browser.tabs.tabs_of(&owner).iter().map(|tab| tab.id).collect();
                self.close_tabs(ids, window, cx);
            }
        }
        cx.notify();
    }

    /// A space-chip menu row. Deleting a space closes the pages it held;
    /// the last space is never deleted (the row is disabled).
    pub(crate) fn browse_space_action(&mut self, space: &str, action: SpaceAction, window: &mut Window, cx: &mut Context<Self>) {
        self.browse_menu = None;
        match action {
            SpaceAction::Rename => {
                let name = self.browser.tabs.spaces.iter().find(|s| s.id == space).map(|s| s.name.clone()).unwrap_or_default();
                self.start_browse_naming(BrowseNaming::Space(Some(space.to_owned().into())), &name, window, cx);
            }
            SpaceAction::Delete => {
                let was_current = self.browser.tabs.current_space == space;
                if let Some(held) = self.browser.tabs.delete_space(space) {
                    for id in held {
                        self.close_tab(HOME_KEY, id, window, cx);
                    }
                    self.schedule_tabs_write(cx);
                    if was_current {
                        self.ensure_browse_tab(window, cx);
                    }
                }
            }
        }
        cx.notify();
    }
}
