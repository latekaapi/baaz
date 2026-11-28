//! What the Browse sidebar's rows, menus and name fields do (the approved
//! canvas "Baaz sidebar v2", Browse round, v22): spaces, your folders,
//! task folders, the tab / folder / task-folder / space menus, Ask, close
//! all. The screen reports intents; [`Harness::browse_intent`] in
//! `sidebar_view.rs` routes each here.

use aui::screens::sidebar::{BrowseMenu, BrowseNaming, FolderAction, SpaceAction, TabAction, TaskFolderAction};
use gpui::{ClipboardItem, Context, Focusable, Window};

use super::tabs::TabId;
use super::{Owner, HOME_KEY};
use crate::app::Harness;
use super::BrowserView;

/// A folder's name until the person types one.
const NEW_FOLDER: &str = "New folder";

/// What a click on ⋯ does.
#[derive(Debug, PartialEq, Eq)]
enum PageMenuPress {
    /// The menu is open: close it.
    Close,
    /// The press already closed it (its overlay lost the keyboard): stay closed.
    AlreadyClosed,
    /// Open it.
    Open,
}

/// The ⋯ toggle as a pure rule: `open` now, and how long ago a press last
/// closed the page menu (the overlay stack's guard, [`crate::overlay_stack::REPRESS`]).
fn page_menu_press(open: bool, closed_ago: Option<std::time::Duration>) -> PageMenuPress {
    if open {
        PageMenuPress::Close
    } else if closed_ago.is_some_and(|ago| ago < crate::overlay_stack::REPRESS) {
        PageMenuPress::AlreadyClosed
    } else {
        PageMenuPress::Open
    }
}

/// The page menu's top-left for a `width`-wide menu under the ⋯ at
/// `anchor` (window coordinates): FF's dropdown, 6 px below its trigger,
/// right edges aligned, never past the window's left edge.
fn page_menu_origin(anchor: gpui::Bounds<gpui::Pixels>, width: gpui::Pixels) -> gpui::Point<gpui::Pixels> {
    let gap = gpui::px(aui_tokens::scale::SP_2 + aui_tokens::scale::SP_1);
    gpui::point((anchor.right() - width).max(gpui::px(0.0)), anchor.bottom() + gap)
}

impl Harness {

    /// Close `ids`, each through its owner's path (a person tab's close
    /// picks the neighbour; closing the space's last one leaves a fresh
    /// blank tab, as the × always has).
    pub(crate) fn close_tabs(&mut self, ids: Vec<TabId>, window: &mut Window, cx: &mut Context<Self>) {
        for id in ids {
            self.browse_sidebar_tab(id, true, window, cx);
        }
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
    }

    /// Open the right-click menu (one at a time) and keep the page from
    /// taking the next click.
    pub(crate) fn open_browse_menu(&mut self, menu: BrowseMenu, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.set_naming(None, cx));
        // Its page is asked for a selection now: "Send selection to
        // session…" shows once the answer lands (`browse_menu_now`).
        let asked = match &menu.target {
            aui::screens::sidebar::BrowseMenuTarget::Tab { id, .. } => crate::sidebar_view::browse_tab_id(id),
            aui::screens::sidebar::BrowseMenuTarget::Page { .. } => self.browser.read(cx).browse_centre_shown(cx),
            _ => None,
        };
        if let Some(tab) = asked {
            self.browser.update(cx, |browser, cx| browser.ask_selection(tab, cx));
        }
        self.overlays.update(cx, |o, cx| o.push(crate::overlay_stack::Entry::new(crate::overlay_stack::Layer::BrowseMenu(menu)), cx));
    }

    /// The page controls' ⋯: opens the page menu under the button (FF's
    /// dropdown: 6 px below, right edges aligned, so it stays over the
    /// sidebar), or closes it when it is open. The menu lives in an overlay
    /// window that holds the keyboard, so the press on ⋯ has usually closed
    /// it already (resign-key) by the time this click lands: a close within
    /// the guard window counts as this click's own (owner screenshot
    /// 2026-10-12 1:21 AM: ⋯ never toggled, the menu jumped to the pointer).
    pub(crate) fn toggle_browse_page_menu(&mut self, anchor: Option<gpui::Bounds<gpui::Pixels>>, window: &mut Window, cx: &mut Context<Self>) {
        use aui::screens::sidebar::BrowseMenuTarget;
        let open = self.overlays.read(cx).stack.browse_menu().is_some_and(|m| matches!(m.target, BrowseMenuTarget::Page { .. }));
        let closed_ago = self.overlays.update(cx, |o, _| o.stack.pressed_shut(crate::overlay_stack::Trigger::BrowsePageMenu));
        match page_menu_press(open, closed_ago) {
            PageMenuPress::Close => return self.close_browse_menu(cx),
            PageMenuPress::AlreadyClosed => return,
            PageMenuPress::Open => {}
        }
        let Some(state) = self.browse_centre_state(cx) else {
            return;
        };
        let page = state.read(cx);
        let target = BrowseMenuTarget::Page {
            safari: self.browse_safari_url(cx).is_some(),
            devtools: page.supports(aui_webview::BackendFeature::DevTools),
            annotating: page.annotating(),
            offers: self.shown_page_offers(cx),
            zoom: self.shown_page_zoom(cx),
        };
        let mut menu = BrowseMenu { target, at: window.mouse_position() };
        if let Some(anchor) = anchor {
            menu.at = page_menu_origin(anchor, aui::screens::sidebar::browse_menu_size(&menu, 0, cx).width);
        }
        self.open_browse_menu(menu, cx);
    }

    /// A page menu row: the B2a page menu's actions, on the shown page.
    pub(crate) fn browse_page_action(&mut self, action: aui::screens::sidebar::PageAction, window: &mut Window, cx: &mut Context<Self>) {
        use aui::screens::sidebar::PageAction;
        // The Zoom row's controls walk the level with the menu open.
        let zoom = match action {
            PageAction::ZoomIn => Some(1),
            PageAction::ZoomOut => Some(-1),
            PageAction::ZoomReset => Some(0),
            _ => None,
        };
        if let Some(direction) = zoom {
            self.zoom_shown_page(direction, cx);
            return;
        }
        self.close_browse_menu(cx);
        match action {
            PageAction::ZoomIn | PageAction::ZoomOut | PageAction::ZoomReset => {}
            PageAction::SiteSettings => self.open_shown_site_settings(cx),
            PageAction::Bookmarks => self.open_library(crate::browser::library::Shelf::Bookmarks, window, cx),
            PageAction::History => self.open_library(crate::browser::library::Shelf::History, window, cx),
            PageAction::NewTab => self.new_browse_sidebar_tab(window, cx),
            PageAction::Find => self.open_browse_find(window, cx),
            PageAction::CopyLink => self.copy_shown_browse_link(cx),
            PageAction::OpenInSafari => self.open_browse_in_safari(cx),
            PageAction::Annotate => self.toggle_browse_annotate(cx),
            PageAction::Screenshot => self.capture_browse_screenshot(window, cx),
            PageAction::DevTools => self.open_browse_devtools(cx),
            PageAction::Settings => self.open_browse_settings(cx),
            PageAction::SendPage | PageAction::SendSelection => {
                if let Some(tab) = self.browser.read(cx).browse_centre_shown(cx) {
                    self.send_tab_page(tab, action == PageAction::SendSelection, window, cx);
                }
            }
            PageAction::Keep => {
                if let Some(tab) = self.browser.read(cx).browse_centre_shown(cx) {
                    self.keep_tab(tab, cx);
                }
            }
            PageAction::Collapse => {
                self.collapse_to_session_page(window, cx);
            }
        }
    }

    /// Close the right-click menu, if one is open.
    pub(crate) fn close_browse_menu(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| {
            let entry = o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx)?;
            if let crate::overlay_stack::Layer::BrowseMenu(menu) = &entry.layer {
                if matches!(menu.target, aui::screens::sidebar::BrowseMenuTarget::Page { .. }) {
                    o.stack.note_pressed_shut(Some(crate::overlay_stack::Trigger::BrowsePageMenu));
                }
            }
            Some(())
        });
    }

    /// Put a name field on `target`, seeded with `draft` and focused: the
    /// sidebar's one rename editor serves folders and spaces too.
    pub(crate) fn start_browse_naming(&mut self, target: BrowseNaming, draft: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx); });
        self.overlays.update(cx, |o, cx| {
            o.close(crate::overlay_stack::LayerKind::RenameSession, cx);
        });
        self.rename.update(cx, |state, cx| state.set_value(draft.to_owned(), window, cx));
        self.browser.update(cx, |browser, cx| browser.set_naming(Some(target), cx));
        window.focus(&self.rename.focus_handle(cx), cx);
    }

    /// Commit a name field. A blank folder name keeps "New folder"; a blank
    /// new space is "Space N"; a blank rename keeps the old name.
    pub(crate) fn commit_browse_name(&mut self, target: BrowseNaming, text: &str, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.set_naming(None, cx));
        let text = text.trim();
        let changed = match target {
            BrowseNaming::Folder(id) => self.browser.update(cx, |browser, _| browser.tabs.rename_folder(&id, text)),
            BrowseNaming::Space(Some(id)) => self.browser.update(cx, |browser, _| browser.tabs.rename_space(&id, text)),
            BrowseNaming::Space(None) => {
                self.browser.update(cx, |browser, _| browser.tabs.add_space(text));
                true
            }
        };
        if changed {
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        }
        self.arm_composer_focus(cx);
    }

    /// Abandon a name field: the folder keeps whatever name it had.
    pub(crate) fn cancel_browse_name(&mut self, cx: &mut Context<Self>) {
        self.browser.update(cx, |browser, cx| browser.set_naming(None, cx));
    }

    /// Today's "new folder" (and Move to folder › New folder): a folder
    /// at the top of the current space's list, named in place, optionally
    /// taking `tab` with it.
    pub(crate) fn browse_new_folder(&mut self, tab: Option<TabId>, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let space = self.browser.read(cx).browse_space().unwrap_or_default();
        let id = self.browser.update(cx, |browser, _| browser.tabs.add_folder(&space, NEW_FOLDER));
        if let Some(tab) = tab {
            self.browser.update(cx, |browser, _| browser.tabs.set_folder(tab, Some(&id)));
        }
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        self.start_browse_naming(BrowseNaming::Folder(id.into()), NEW_FOLDER, window, cx);
    }

    /// Today's "close all": every unpinned tab of yours in this space.
    pub(crate) fn browse_close_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.browser.read(cx).space_person_tabs(false);
        self.close_tabs(ids, window, cx);
    }

    /// A space chip: make it current and show its active tab (or a new one).
    pub(crate) fn browse_switch_space(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_tabs_loaded(cx);
        let Some(id) = self.browser.read(cx).tabs.spaces.get(index).map(|space| space.id.clone()) else {
            return;
        };
        if !self.browser.update(cx, |browser, _| browser.tabs.switch_space(&id)) {
            return;
        }
        self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
        self.browser.update(cx, |browser, cx| browser.set_session_tab(None, cx));
        match self.browser.read(cx).tabs.active_of(&Owner::Person, Some(&id)) {
            Some(tab) => {
                self.ensure_tab_page(HOME_KEY, tab, window, cx);
            }
            None => self.ensure_browse_tab(window, cx),
        }
    }

    /// The ✦ Ask button: the one Ask (docs/82 8.2) — the agent card opens
    /// on the page shown, ready to type. Never a session, never a draft.
    pub(crate) fn browse_ask_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_agent_card(window, cx);
    }

    /// A tab menu row.
    pub(crate) fn browse_tab_action(&mut self, tab: TabId, action: TabAction, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx); });
        self.ensure_tabs_loaded(cx);
        let Some(found) = self.browser.read(cx).tabs.get(tab).cloned() else {
            return;
        };
        match action {
            TabAction::Duplicate => {
                let copy = self.browser.update(cx, |browser, _| browser.tabs.open(found.space.clone(), found.owner.clone(), &found.url, Some(tab)));
                if let Some(folder) = found.folder.as_deref() {
                    self.browser.update(cx, |browser, _| browser.tabs.set_folder(copy, Some(folder)));
                }
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                self.browse_sidebar_tab(copy, false, window, cx);
            }
            TabAction::CopyLink => {
                cx.write_to_clipboard(ClipboardItem::new_string(found.url.clone()));
                self.overlays.update(cx, |overlays, cx| overlays.toast("Link copied", &found.url, cx));
            }
            TabAction::TogglePin => {
                self.browser.update(cx, |browser, _| browser.tabs.set_pinned(tab, !found.pinned));
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
            }
            TabAction::MoveToFolder(folder) => {
                self.browser.update(cx, |browser, _| browser.tabs.set_folder(tab, Some(folder.as_ref())));
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
            }
            TabAction::MoveToNewFolder => self.browse_new_folder(Some(tab), window, cx),
            TabAction::RemoveFromFolder => {
                self.browser.update(cx, |browser, _| browser.tabs.set_folder(tab, None));
                self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
            }
            TabAction::Close => self.browse_sidebar_tab(tab, true, window, cx),
            TabAction::SendPage | TabAction::SendSelection => {
                self.send_tab_page(tab, action == TabAction::SendSelection, window, cx);
            }
            TabAction::Keep => {
                self.keep_tab(tab, cx);
            }
            TabAction::CloseOthers => {
                let others: Vec<TabId> = self.browser.read(cx).space_person_tabs(false).into_iter().filter(|id| *id != tab).collect();
                self.close_tabs(others, window, cx);
            }
        }
    }

    /// A folder menu row.
    pub(crate) fn browse_folder_action(&mut self, folder: &str, action: FolderAction, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx); });
        match action {
            FolderAction::Rename => {
                let name = self.browser.read(cx).tabs.folders.iter().find(|f| f.id == folder).map(|f| f.name.clone()).unwrap_or_default();
                self.start_browse_naming(BrowseNaming::Folder(folder.to_owned().into()), &name, window, cx);
            }
            FolderAction::CloseTabs => {
                let space = self.browser.read(cx).browse_space().unwrap_or_default();
                let ids = self.browser.read(cx).tabs.person_ids_in(&space, Some(folder));
                self.close_tabs(ids, window, cx);
            }
            FolderAction::Delete => {
                if self.browser.update(cx, |browser, _| browser.tabs.delete_folder(folder)) {
                    self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
                }
            }
        }
    }

    /// A task-folder menu row (and the head's click: Open chat).
    pub(crate) fn browse_task_folder_action(&mut self, session: &str, action: TaskFolderAction, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx); });
        match action {
            // A browser chat opens in its card, over the page (R3); a
            // project session opens in Sessions.
            TaskFolderAction::OpenChat if self.is_browser_chat(session, cx) && self.show_browser_chat(session, cx) => {
                if self.browser.read(cx).agent_window.card_ui == crate::app::agent_window::CardUi::Closed {
                    self.browser.update(cx, |browser, _| browser.agent_window.card_ui = crate::app::agent_window::CardUi::Card);
                }
            }
            TaskFolderAction::OpenChat => {
                self.set_mode(crate::layout::Mode::Sessions, window, cx);
                self.resume_quiet(session.to_owned(), window, cx);
            }
            TaskFolderAction::Stop => {
                if let Some(view) = self.find_view(session, cx) {
                    view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.interrupt(cx)));
                }
            }
            TaskFolderAction::CloseTabs => {
                let owner = Owner::Session(session.to_owned());
                let ids: Vec<TabId> = self.browser.read(cx).tabs.tabs_of(&owner).iter().map(|tab| tab.id).collect();
                self.close_tabs(ids, window, cx);
            }
        }
        cx.notify();
    }

    /// A space-chip menu row. Deleting a space closes the pages it held;
    /// the last space is never deleted (the row is disabled).
    pub(crate) fn browse_space_action(&mut self, space: &str, action: SpaceAction, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |o, cx| { o.close(crate::overlay_stack::LayerKind::BrowseMenu, cx); });
        match action {
            SpaceAction::Rename => {
                let name = self.browser.read(cx).tabs.spaces.iter().find(|s| s.id == space).map(|s| s.name.clone()).unwrap_or_default();
                self.start_browse_naming(BrowseNaming::Space(Some(space.to_owned().into())), &name, window, cx);
            }
            SpaceAction::SignIns => {
                let Some(found) = self.browser.read(cx).tabs.spaces.iter().find(|s| s.id == space).cloned() else { return };
                let (title, detail, primary) = if found.own_signins {
                    (
                        format!("Share sign-ins in \"{}\" again?", found.name),
                        "Its tabs reload with the sign-ins your other spaces use. Its own sign-ins stay on this Mac, so switching back finds them again.".to_owned(),
                        "Share sign-ins",
                    )
                } else {
                    (
                        format!("Give \"{}\" its own sign-ins?", found.name),
                        "Its tabs reload signed out of every site: sign in here with a second account. Cookies and site data stay apart from your other spaces; saved passwords stay shared.".to_owned(),
                        "Use its own sign-ins",
                    )
                };
                self.set_dialog(
                    cx,
                    crate::overlays::Dialog {
                        title,
                        detail,
                        kind: aui::overlay::DialogKind::Info,
                        primary,
                        action: crate::overlays::DialogAction::SpaceSignIns,
                        archive_target: Some(space.to_owned()),
                    },
                );
            }
            SpaceAction::Delete => {
                let space_tabs = self.browser.read(cx).tabs.person_ids_in(space, None).len();
                if space_tabs == 0 {
                    self.delete_space_now(space, window, cx);
                } else {
                    let name = self.browser.read(cx).tabs.spaces.iter().find(|s| s.id == space).map(|s| s.name.clone()).unwrap_or_default();
                    self.set_dialog(
                        cx,
                        crate::overlays::Dialog {
                            title: format!("Delete \"{name}\"?"),
                            detail: format!("Its {space_tabs} tab{} close and its folders go with it.", if space_tabs == 1 { "" } else { "s" }),
                            kind: aui::overlay::DialogKind::Warning,
                            primary: "Delete space",
                            action: crate::overlays::DialogAction::DeleteSpace,
                            archive_target: Some(space.to_owned()),
                        },
                    );
                }
            }
        }
    }

    /// Delete a space now (confirmed, or empty): close the pages it held and
    /// show the space that becomes current. The last space never goes.
    pub(crate) fn delete_space_now(&mut self, space: &str, window: &mut Window, cx: &mut Context<Self>) {
        let was_current = self.browser.read(cx).tabs.current_space == space;
        if let Some(held) = self.browser.update(cx, |browser, _| browser.tabs.delete_space(space)) {
            for id in held {
                self.close_tab(HOME_KEY, id, window, cx);
            }
            self.browser.update(cx, |browser, cx| browser.tabs_changed(cx));
            if was_current {
                self.ensure_browse_tab(window, cx);
            }
        }
    }
}

impl BrowserView {
    /// The person's tab ids in the current space, Today and folders alike,
    /// pinned ones excluded unless `pinned`.
    pub(super) fn space_person_tabs(&self, pinned: bool) -> Vec<TabId> {
        let space = self.browse_space().unwrap_or_default();
        self.tabs
            .ordered_tabs(&Owner::Person, Some(&space))
            .iter()
            .filter(|tab| pinned || !tab.pinned)
            .map(|tab| tab.id)
            .collect()
    }
}

#[cfg(feature = "probe")]
#[cfg(test)]
mod page_menu_tests {
    use super::*;
    use std::time::Duration;

    /// Owner screenshot 2026-10-12 1:21 AM: a second click on ⋯ left the
    /// menu open (its press closed it, its release reopened it).
    #[test]
    fn a_second_click_on_the_dots_closes_the_menu() {
        assert_eq!(page_menu_press(true, None), PageMenuPress::Close, "open: the click closes it");
        assert_eq!(page_menu_press(false, Some(Duration::from_millis(30))), PageMenuPress::AlreadyClosed, "the press just closed it");
        assert_eq!(page_menu_press(false, Some(Duration::from_millis(900))), PageMenuPress::Open, "a later click opens it");
        assert_eq!(page_menu_press(false, None), PageMenuPress::Open);
    }

    /// The menu hung off the pointer and followed it on every click.
    #[test]
    fn the_page_menu_hangs_under_its_button_right_aligned() {
        use gpui::{point, px, size, Bounds};
        let dots = Bounds::new(point(px(212.0), px(56.0)), size(px(28.0), px(28.0)));
        let at = page_menu_origin(dots, px(240.0));
        assert_eq!(at.x, px(0.0), "clamped at the window's left edge (240 > 240 − 0)");
        let wide = Bounds::new(point(px(400.0), px(56.0)), size(px(28.0), px(28.0)));
        let at = page_menu_origin(wide, px(240.0));
        assert_eq!(at.x + px(240.0), wide.right(), "right edges aligned");
        assert_eq!(at.y, wide.bottom() + px(6.0), "6 px below the button");
    }
}
