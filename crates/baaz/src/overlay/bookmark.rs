//! The bookmark popover (R4 board R4Daily 1a): ⌘D, or the address line's
//! filled ★, opens it under the address line, over the live page. Name,
//! folder, Remove, Done. ↩ or a click away keeps what is typed; Remove
//! deletes the bookmark. It types, so like the Field its float takes the
//! keyboard and holds its name input focused.

use aui_icons::{icon, IconName};
use aui_tokens::{scale, ActiveAui, AuiStyled, Density, TextRole};
use gpui::{div, prelude::*, px, Context, Entity, SharedString, Window};
use gpui_kit::base::input::InputState;
use gpui_kit::base::{h_flex, v_flex};

use super::{menu_surface, SHADOW_PAD};
use crate::app::Harness;
use crate::browser::bookmarks;

/// The popover's width (the board's 300).
pub(crate) const BOOKMARK_W: f32 = 300.0;

/// The bookmark being edited.
pub(crate) struct BookmarkEdit {
    pub id: i64,
    pub name: Entity<InputState>,
    pub folder: i64,
    /// The folder list stands open under the folder row.
    pub folders_open: bool,
}

impl Harness {
    /// ⌘D: bookmark the shown page into the Bookmarks bar (if it is not
    /// yet) and open the popover on it. Again while open: Done.
    pub(crate) fn bookmark_shown_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.bookmark_edit.is_some() {
            self.close_bookmark_popover(cx);
            return;
        }
        let Some(id) = self.browse_centre_shown() else { return };
        let Some(tab) = self.browser.tabs.get(id).cloned() else { return };
        if !crate::browser::history::recordable(&tab.url) {
            return;
        }
        let conn = self.browser.db().conn();
        let title = crate::browser::tabs::tab_label(&tab.title, &tab.url);
        let Some(mark) = bookmarks::add(conn, &tab.url, &title, bookmarks::BAR, crate::browser::tabs::now_ms()) else { return };
        self.browser.bookmarks_changed();
        self.open_bookmark_popover(mark, window, cx);
    }

    /// The ★ in the address line: the popover on the page's bookmark.
    pub(crate) fn edit_shown_bookmark(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.browse_centre_shown() else { return };
        let Some(url) = self.browser.tabs.get(id).map(|tab| tab.url.clone()) else { return };
        if let Some(mark) = bookmarks::find(self.browser.db().conn(), &url) {
            self.open_bookmark_popover(mark, window, cx);
        }
    }

    fn open_bookmark_popover(&mut self, mark: bookmarks::Bookmark, window: &mut Window, cx: &mut Context<Self>) {
        let name = cx.new(|cx| InputState::new(window, cx));
        name.update(cx, |input, cx| {
            input.set_value(mark.title.clone(), window, cx);
            input.select_all(window, cx);
        });
        let subscription = cx.subscribe(&name, |this: &mut Self, _, event: &gpui_kit::base::input::InputEvent, cx| {
            if matches!(event, gpui_kit::base::input::InputEvent::PressEnter { .. }) {
                this.close_bookmark_popover(cx);
            }
        });
        self.bookmark_edit_sub = Some(subscription);
        self.bookmark_edit = Some(BookmarkEdit { id: mark.id, name, folder: mark.folder, folders_open: false });
        cx.notify();
    }

    /// Keep what is typed and close (Done, ↩, Escape, a click away).
    pub(crate) fn close_bookmark_popover(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.bookmark_edit.take() else { return };
        self.bookmark_edit_sub = None;
        let name = edit.name.read(cx).value().trim().to_owned();
        let conn = self.browser.db().conn();
        if !name.is_empty() {
            bookmarks::update(conn, edit.id, &name, edit.folder);
        } else if let Ok(old) = conn.query_row("SELECT title FROM bookmarks WHERE id = ?1", [edit.id], |r| r.get::<_, String>(0)) {
            bookmarks::update(conn, edit.id, &old, edit.folder);
        }
        self.browser.bookmarks_changed();
        cx.notify();
    }

    /// Remove: the bookmark goes and the popover closes.
    pub(crate) fn remove_bookmark_from_popover(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.bookmark_edit.take() else { return };
        self.bookmark_edit_sub = None;
        bookmarks::remove(self.browser.db().conn(), edit.id);
        self.browser.bookmarks_changed();
        cx.notify();
    }

    /// The popover's content inside its float.
    pub(crate) fn render_bookmark_popover(&mut self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let p = cx.aui().colors;
        let metrics = cx.aui().metrics;
        let edit = self.bookmark_edit.as_ref()?;
        let name = edit.name.clone();
        let folder = edit.folder;
        let folders_open = edit.folders_open;
        let all = bookmarks::folders(self.browser.db().conn());
        let folder_name = bookmarks::folder_path(&all, folder);
        let label = |text: &'static str, cx: &gpui::App| {
            div().px(metrics.item_px).pt(px(scale::SP_2)).pb(px(scale::SP_1)).role_in(TextRole::Caption, cx).text_color(p.fg_muted).child(text)
        };
        let field_box = |inner: gpui::AnyElement| {
            h_flex()
                .mx(metrics.item_px)
                .h(px(30.0))
                .px(px(9.0))
                .gap(px(scale::SP_2))
                .items_center()
                .rounded(px(scale::R_SM))
                .bg(p.surface_1)
                .border_1()
                .border_color(p.line)
                .child(inner)
        };
        let toggle = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            if let Some(edit) = this.bookmark_edit.as_mut() {
                edit.folders_open = !edit.folders_open;
                cx.notify();
            }
        });
        let mut panel = v_flex()
            .id("bookmark-popover")
            .w_full()
            .pb(px(scale::SP_2))
            .role(gpui::Role::Dialog)
            .aria_label("Bookmark")
            .child(
                h_flex()
                    .px(metrics.item_px)
                    .pt(metrics.item_px)
                    .gap(px(scale::SP_2))
                    .items_center()
                    .child(icon(IconName::StarFill).size(metrics.icon).color(p.fg))
                    .child(div().flex_1().text_role_in(TextRole::Body, Density::Standard).semibold().text_color(p.fg).child("Bookmarked"))
                    .child(div().role_in(TextRole::Micro, cx).text_color(p.fg_muted).child("⌘D")),
            )
            .child(label("Name", cx))
            .child(field_box(aui::a11y_text::a11y_text_input("bookmark-name", "Bookmark name", &name, cx).flex_1().min_w(px(0.0)).text_color(p.fg).into_any_element()))
            .child(label("Folder", cx))
            .child(
                div()
                    .id("bookmark-folder")
                    .cursor_pointer()
                    .role(gpui::Role::Button)
                    .aria_label(SharedString::from(format!("Folder, {folder_name}")))
                    .on_click(toggle)
                    .child(field_box(
                        h_flex()
                            .flex_1()
                            .gap(px(scale::SP_2))
                            .items_center()
                            .child(icon(IconName::Folder).size(px(13.0)).color(p.fg_muted))
                            .child(div().flex_1().truncate().text_role_in(TextRole::Body, Density::Standard).text_color(p.fg).child(folder_name))
                            .child(icon(IconName::ChevronDown).size(px(11.0)).color(p.fg_muted))
                            .into_any_element(),
                    )),
            );
        if folders_open {
            let mut list = v_flex().mx(metrics.item_px).mt(px(scale::SP_1)).rounded(px(scale::R_SM)).border_1().border_color(p.line).py(px(2.0));
            for f in all.iter() {
                let id = f.id;
                let path = bookmarks::folder_path(&all, id);
                let pick = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    if let Some(edit) = this.bookmark_edit.as_mut() {
                        edit.folder = id;
                        edit.folders_open = false;
                        cx.notify();
                    }
                });
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("bookmark-folder-{id}")))
                        .h(px(28.0))
                        .px(px(9.0))
                        .gap(px(scale::SP_2))
                        .items_center()
                        .cursor_pointer()
                        .hover(|s| s.bg(p.surface_2))
                        .role(gpui::Role::Button)
                        .aria_label(SharedString::from(format!("Move to {path}")))
                        .on_click(pick)
                        .child(div().flex_1().truncate().text_role_in(TextRole::Body, Density::Standard).text_color(p.fg).child(path))
                        .when(id == folder, |d| d.child(icon(IconName::Check).size(px(12.0)).color(p.fg))),
                );
            }
            panel = panel.child(list);
        }
        let remove = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.remove_bookmark_from_popover(cx));
        let done = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.close_bookmark_popover(cx));
        panel = panel.child(
            h_flex()
                .px(metrics.item_px)
                .pt(px(scale::SP_3))
                .gap(px(scale::SP_2))
                .child(aui::data::button("bookmark-remove", "Remove").accessibility_label("Remove bookmark").on_click(move |e, w, cx| remove(e, w, cx)))
                .child(div().flex_1())
                .child(aui::data::button("bookmark-done", "Done").primary().accessibility_label("Done").on_click(move |e, w, cx| done(e, w, cx))),
        );
        let leave = cx.listener(|this: &mut Self, _: &gpui::MouseDownEvent, _, cx| this.close_bookmark_popover(cx));
        Some(
            div()
                .size_full()
                .flex()
                .flex_col()
                .p(px(SHADOW_PAD))
                .on_mouse_down(gpui::MouseButton::Left, leave)
                .child(div().w_full().occlude().on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(menu_surface(panel)))
                .into_any_element(),
        )
    }
}
