//! Bookmarks and History (docs/82 8.6): a page in Browse, like Settings is
//! one — it fills the centre (the native page hides under it), a list on
//! its left picks the shelf (Bookmarks and its folders, History), and
//! Escape or ✕ closes it. Reached from the page menu, the Field's
//! commands, ⇧⌘Y (Bookmarks) and ⌘Y (History).
//!
//! - **Bookmarks**: a folder's bookmarks (or every match for the search),
//!   each opened by a click, renamed in place, moved to another folder or
//!   deleted.
//! - **History**: the newest pages (or the search's matches), grouped by
//!   day; a click opens one, Delete forgets one, "Clear history…" asks
//!   first (the same dialog as Settings › Browser).
//!
//! Every read is [`BrowserData`]'s front — history rows come back from its
//! worker, never from SQLite on this thread — and every edit is its write.
//! The page is an entity the Browse view owns
//! ([`super::BrowserView::library`]); the root composes it and answers its
//! [`LibraryEvent`]s.

use aui::data::{button, field_frame, icon_button, ButtonSize};
use aui::interact::{list_row, LIST_INSET, ROW_GAP};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled, TextRole};
use gpui::{
    div, prelude::*, px, AnyElement, App, Context, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Subscription,
    Window,
};
use gpui_kit::base::input::{Input, InputEvent, InputState};
use gpui_kit::base::{h_flex, v_flex};

use super::data::BrowserData;

/// Which shelf the page shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Shelf {
    Bookmarks,
    History,
}

/// What the page asks of the root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LibraryEvent {
    /// ✕ or Escape.
    Close,
    /// A row was clicked: open its page in a new tab and leave the page.
    Open(String),
    /// "Clear history…": the root's confirm dialog.
    ClearHistory,
}

/// The left list's width.
const LIST_W: f32 = scale::SP_7 * 7.0;
/// How wide the page's rows grow, like Settings' column.
const COLUMN_MAX: f32 = crate::settings::SETTINGS_COLUMN_MAX;

/// The Bookmarks and History page (see the module doc).
pub(crate) struct LibraryPage {
    /// Whether it stands open over the centre.
    open: bool,
    shelf: Shelf,
    /// The bookmark folder shown (the Bookmarks bar until another is picked).
    folder: i64,
    /// The search over the shelf shown.
    search: Entity<InputState>,
    /// The bookmark being renamed, with its field.
    rename: Option<(i64, Entity<InputState>)>,
    /// The bookmark whose "Move to" folders are listed under it.
    moving: Option<i64>,
    focus: FocusHandle,
    /// Set by an open: the next render puts the keyboard on the page.
    focus_armed: bool,
    data: Entity<BrowserData>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LibraryEvent> for LibraryPage {}

impl Focusable for LibraryPage {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl LibraryPage {
    pub(crate) fn new(data: Entity<BrowserData>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        // What the page reads in render (the store's front, the search's
        // text) redraws it when it changes: gpui follows what a frame read.
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search"));
        Self {
            open: false,
            shelf: Shelf::Bookmarks,
            folder: super::bookmarks::BAR,
            search,
            rename: None,
            moving: None,
            focus: cx.focus_handle(),
            focus_armed: false,
            data,
            _subscriptions: Vec::new(),
        }
    }

    /// Whether the page stands open.
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    /// The shelf shown.
    pub(crate) fn shelf(&self) -> Shelf {
        self.shelf
    }

    /// Every change to what the page shows goes through here: `change`
    /// runs, any rename or "Move to" in progress ends unless it says
    /// otherwise, and the page redraws once.
    fn set(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Self)) {
        self.rename = None;
        self.moving = None;
        change(self);
        cx.notify();
    }

    /// Open on `shelf`, the search cleared. Idempotent.
    pub(crate) fn show(&mut self, shelf: Shelf, window: &mut Window, cx: &mut Context<Self>) {
        if !self.open || self.shelf != shelf {
            self.search.update(cx, |search, cx| search.set_value("", window, cx));
        }
        self.set(cx, |page| {
            page.open = true;
            page.shelf = shelf;
            page.focus_armed = true;
        });
    }

    /// Shut, dropping any rename in progress.
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.set(cx, |page| page.open = false);
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_owned()
    }

    fn pick(&mut self, shelf: Shelf, folder: Option<i64>, window: &mut Window, cx: &mut Context<Self>) {
        self.search.update(cx, |search, cx| search.set_value("", window, cx));
        self.set(cx, |page| {
            page.shelf = shelf;
            if let Some(folder) = folder {
                page.folder = folder;
            }
        });
    }

    fn start_rename(&mut self, id: i64, title: &str, window: &mut Window, cx: &mut Context<Self>) {
        let field = cx.new(|cx| InputState::new(window, cx).placeholder("Name"));
        field.update(cx, |field, cx| field.set_value(title.to_owned(), window, cx));
        let commit = cx.subscribe(&field, move |this: &mut Self, field, event: &InputEvent, cx| {
            if matches!(event, InputEvent::PressEnter { .. }) {
                let title = field.read(cx).value().trim().to_owned();
                this.finish_rename(id, Some(title), cx);
            }
        });
        self._subscriptions.push(commit);
        window.focus(&field.focus_handle(cx), cx);
        self.set(cx, |page| page.rename = Some((id, field)));
    }

    /// End the rename: `Some(title)` keeps it (an empty one changes nothing).
    fn finish_rename(&mut self, id: i64, title: Option<String>, cx: &mut Context<Self>) {
        if self.rename.as_ref().map(|(at, _)| *at) != Some(id) {
            return;
        }
        if let Some(title) = title.filter(|t| !t.is_empty()) {
            if let Some(mark) = self.data.read(cx).bookmark_by_id(id) {
                self.data.update(cx, |data, cx| data.update_bookmark(id, &title, mark.folder, cx));
            }
        }
        self.set(cx, |_| {});
    }

    fn move_to(&mut self, id: i64, folder: i64, cx: &mut Context<Self>) {
        if let Some(mark) = self.data.read(cx).bookmark_by_id(id) {
            self.data.update(cx, |data, cx| data.update_bookmark(id, &mark.title, folder, cx));
        }
        self.set(cx, |_| {});
    }

    /// The left list: Bookmarks with its folders under it, then History.
    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let folders = self.data.read(cx).folders();
        let mut list = v_flex()
            .id("library-list")
            .flex_none()
            .w(px(LIST_W))
            .h_full()
            .px(px(LIST_INSET))
            .pt(px(scale::SP_6))
            .gap(px(ROW_GAP))
            .border_r_1()
            .border_color(p.line)
            .role(gpui::Role::TabList)
            .aria_label("Bookmarks and history");
        let top = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.pick(Shelf::Bookmarks, Some(super::bookmarks::BAR), window, cx));
        list = list.child(
            list_row("library-bookmarks", "Bookmarks")
                .leading(aui_icons::icon(IconName::Star).size(px(scale::ICON_DEFAULT)))
                .selected(self.shelf == Shelf::Bookmarks && self.folder == super::bookmarks::BAR)
                .on_click(top),
        );
        for folder in folders.iter().filter(|f| f.id != super::bookmarks::BAR) {
            let depth = folder_depth(&folders, folder.id);
            let id = folder.id;
            let open = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.pick(Shelf::Bookmarks, Some(id), window, cx));
            list = list.child(
                list_row(SharedString::from(format!("library-folder-{id}")), folder.name.clone())
                    .depth(depth.min(3))
                    .selected(self.shelf == Shelf::Bookmarks && self.folder == id)
                    .on_click(open),
            );
        }
        let history = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| this.pick(Shelf::History, None, window, cx));
        list.child(div().h_px().w_full().flex_none().my(px(scale::SP_2)).bg(p.line))
            .child(
                list_row("library-history", "History")
                    .leading(aui_icons::icon(IconName::Clock).size(px(scale::ICON_DEFAULT)))
                    .selected(self.shelf == Shelf::History)
                    .on_click(history),
            )
            .into_any_element()
    }

    /// One bookmark: its title (or the rename field) over its address,
    /// opened by a click; Rename, Move to and Delete at its right, and the
    /// folders to move it to under it while Move to is open.
    fn bookmark_row(&self, mark: &super::bookmarks::Bookmark, folders: &[super::bookmarks::Folder], window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let id = mark.id;
        let title = if mark.title.is_empty() { mark.url.clone() } else { mark.title.clone() };
        let renaming = self.rename.as_ref().filter(|(at, _)| *at == id).map(|(_, field)| field.clone());
        let main: AnyElement = match renaming {
            Some(field) => {
                let keys = cx.listener(move |this: &mut Self, e: &gpui::KeyDownEvent, _, cx| {
                    if e.keystroke.key == "escape" {
                        cx.stop_propagation();
                        this.finish_rename(id, None, cx);
                    }
                });
                field_frame(SharedString::from(format!("library-rename-{id}")), &field, window, cx)
                    .flex_1()
                    .min_w(px(0.0))
                    .role(gpui::Role::TextInput)
                    .aria_label(format!("Rename {title}"))
                    .capture_key_down(keys)
                    .child(Input::new(&field))
                    .into_any_element()
            }
            None => {
                let url = mark.url.clone();
                let open = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(LibraryEvent::Open(url.clone())));
                aui::interact::interactive(
                    SharedString::from(format!("library-bookmark-{id}")),
                    v_flex()
                        .flex_1()
                        .min_w(px(0.0))
                        .px(px(scale::SP_2))
                        .py(px(scale::SP_1))
                        .child(div().truncate().ui(TextRole::Body).text_color(p.ink).child(title.clone()))
                        .child(div().truncate().ui(TextRole::Caption).text_color(p.fg_muted).child(crate::browser::toolbar::display_url(&mark.url))),
                )
                .role("link")
                .label(format!("Open {title}"))
                .on_click(open)
                .into_any_element()
            }
        };
        let (rename_title, mv) = (title.clone(), self.moving == Some(id));
        let rename = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.start_rename(id, &rename_title, window, cx));
        let toggle_move = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            let open = this.moving != Some(id);
            this.set(cx, |page| page.moving = open.then_some(id));
        });
        let delete = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.data.update(cx, |data, cx| data.remove_bookmark(id, cx));
        });
        let actions = h_flex()
            .flex_none()
            .gap(px(scale::SP_1))
            .child(button(SharedString::from(format!("library-rename-btn-{id}")), "Rename").ghost().sm().accessibility_label(format!("Rename {title}")).on_click(rename))
            .child(button(SharedString::from(format!("library-move-btn-{id}")), "Move to…").ghost().sm().accessibility_label(format!("Move {title} to another folder")).on_click(toggle_move))
            .child(button(SharedString::from(format!("library-delete-btn-{id}")), "Delete").ghost().sm().accessibility_label(format!("Delete the bookmark {title}")).on_click(delete));
        let mut row = v_flex()
            .w_full()
            .border_b_1()
            .border_color(p.line)
            .child(h_flex().w_full().min_h(px(scale::ROW_2_DEFAULT)).items_center().gap(px(scale::SP_3)).child(main).child(actions));
        if mv {
            let mut targets = h_flex()
                .id(SharedString::from(format!("library-move-targets-{id}")))
                .flex_wrap()
                .gap(px(scale::SP_2))
                .px(px(scale::SP_2))
                .pb(px(scale::SP_3))
                .role(gpui::Role::Group)
                .aria_label(format!("Move {title} to"));
            for folder in folders.iter().filter(|f| f.id != mark.folder) {
                let to = folder.id;
                let pick = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| this.move_to(id, to, cx));
                targets = targets.child(
                    button(SharedString::from(format!("library-move-{id}-{to}")), folder.name.clone())
                        .sm()
                        .accessibility_label(format!("Move to {}", folder.name))
                        .on_click(pick),
                );
            }
            row = row.child(targets);
        }
        row.into_any_element()
    }

    fn render_bookmarks(&self, window: &mut Window, cx: &mut Context<Self>) -> (String, AnyElement) {
        let query = self.query(cx);
        let data = self.data.read(cx);
        let folders = data.folders();
        let (heading, marks) = if query.is_empty() {
            let name = folders.iter().find(|f| f.id == self.folder).map(|f| f.name.clone()).unwrap_or_else(|| "Bookmarks".to_owned());
            (name, data.in_folder(self.folder))
        } else {
            (format!("Bookmarks matching \u{201C}{query}\u{201D}"), data.search_bookmarks(&query, 200))
        };
        let mut body = v_flex().w_full();
        if marks.is_empty() {
            body = body.child(empty_line(if query.is_empty() { "No bookmarks in this folder. \u{2318}D bookmarks the page you\u{2019}re on." } else { "No bookmarks match." }, cx));
        }
        for mark in &marks {
            body = body.child(self.bookmark_row(mark, &folders, window, cx));
        }
        (heading, body.into_any_element())
    }

    fn render_history(&self, cx: &mut Context<Self>) -> (String, AnyElement) {
        let p = cx.aui().colors;
        let query = self.query(cx);
        let rows = self.data.read(cx).history_page(&query);
        let heading = if query.is_empty() { "History".to_owned() } else { format!("History matching \u{201C}{query}\u{201D}") };
        let Some(rows) = rows else {
            return (heading, empty_line("Reading history\u{2026}", cx));
        };
        if rows.is_empty() {
            return (heading, empty_line(if query.is_empty() { "No history yet." } else { "No pages match." }, cx));
        }
        let now = crate::browser::tabs::now_ms();
        let offset = chrono::Local::now().offset().local_minus_utc() as i64 * 1000;
        let days = crate::browser::history::by_day(rows, now, offset, |ms| local_time(ms, "%A, %-d %B"));
        let mut body = v_flex().w_full().gap(px(scale::SP_5));
        for (day_index, (day, visits)) in days.into_iter().enumerate() {
            let mut group = v_flex()
                .id(SharedString::from(format!("library-day-{day_index}")))
                .w_full()
                .role(gpui::Role::Group)
                .aria_label(day.clone())
                .child(div().pb(px(scale::SP_2)).ui(TextRole::Caption).text_color(p.fg_muted).child(day));
            for (index, visit) in visits.into_iter().enumerate() {
                let title = if visit.title.is_empty() { visit.url.clone() } else { visit.title.clone() };
                let url = visit.url.clone();
                let open = cx.listener(move |_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(LibraryEvent::Open(url.clone())));
                let gone = visit.url.clone();
                let delete = cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, _, cx| {
                    let url = gone.clone();
                    this.data.update(cx, |data, cx| data.remove_visit(&url, cx));
                });
                let key = format!("{day_index}-{index}");
                group = group.child(
                    h_flex()
                        .w_full()
                        .min_h(px(scale::ROW_2_DEFAULT))
                        .items_center()
                        .gap(px(scale::SP_3))
                        .border_b_1()
                        .border_color(p.line)
                        .child(div().flex_none().w(px(scale::SP_7 * 1.5)).ui(TextRole::Caption).text_color(p.fg_muted).child(local_time(visit.last_ms, "%H:%M")))
                        .child(
                            aui::interact::interactive(
                                SharedString::from(format!("library-visit-{key}")),
                                v_flex()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .px(px(scale::SP_2))
                                    .py(px(scale::SP_1))
                                    .child(div().truncate().ui(TextRole::Body).text_color(p.ink).child(title.clone()))
                                    .child(div().truncate().ui(TextRole::Caption).text_color(p.fg_muted).child(crate::browser::toolbar::display_url(&visit.url))),
                            )
                            .role("link")
                            .label(format!("Open {title}"))
                            .on_click(open),
                        )
                        .child(
                            button(SharedString::from(format!("library-visit-delete-{key}")), "Delete")
                                .ghost()
                                .sm()
                                .accessibility_label(format!("Delete {title} from history"))
                                .on_click(delete),
                        ),
                );
            }
            body = body.child(group);
        }
        (heading, body.into_any_element())
    }
}

/// How deep `id` sits in the folder tree (top-level folders are 0).
fn folder_depth(folders: &[super::bookmarks::Folder], id: i64) -> u8 {
    let mut depth = 0u8;
    let mut at = folders.iter().find(|f| f.id == id).and_then(|f| f.parent);
    while let Some(parent) = at {
        depth = depth.saturating_add(1);
        if depth > 8 {
            break;
        }
        at = folders.iter().find(|f| f.id == parent).and_then(|f| f.parent);
    }
    depth
}

/// `ms` as local time in `format` (a capture's frozen clock reads the same).
fn local_time(ms: u64, format: &str) -> String {
    chrono::DateTime::from_timestamp_millis(ms as i64).map(|t| t.with_timezone(&chrono::Local).format(format).to_string()).unwrap_or_default()
}

fn empty_line(text: &'static str, cx: &App) -> AnyElement {
    let p = cx.aui().colors;
    div().py(px(scale::SP_5)).ui(TextRole::Body).text_color(p.fg_muted).child(text).into_any_element()
}

impl Render for LibraryPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if std::mem::take(&mut self.focus_armed) && self.open && !crate::runtime::captures() {
            window.focus(&self.focus, cx);
        }
        let p = cx.aui().colors;
        let (heading, body) = match self.shelf {
            Shelf::Bookmarks => self.render_bookmarks(window, cx),
            Shelf::History => self.render_history(cx),
        };
        let close = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(LibraryEvent::Close));
        let search = field_frame("library-search", &self.search, window, cx)
            .w(px(scale::SP_7 * 8.0))
            .flex_none()
            .role(gpui::Role::TextInput)
            .aria_label(match self.shelf {
                Shelf::Bookmarks => "Search bookmarks",
                Shelf::History => "Search history",
            })
            .child(Input::new(&self.search));
        let mut tools = h_flex().flex_none().gap(px(scale::SP_2)).items_center().child(search);
        if self.shelf == Shelf::History {
            let clear = cx.listener(|_: &mut Self, _: &gpui::ClickEvent, _, cx| cx.emit(LibraryEvent::ClearHistory));
            tools = tools.child(button("library-clear-history", "Clear history\u{2026}").accessibility_label("Clear browsing history").on_click(clear));
        }
        tools = tools.child(
            icon_button("library-close", IconName::X)
                .ghost()
                .size(ButtonSize::Sm)
                .accessibility_label(match self.shelf {
                    Shelf::Bookmarks => "Close bookmarks",
                    Shelf::History => "Close history",
                })
                .on_click(close),
        );
        let escape = cx.listener(|this: &mut Self, e: &gpui::KeyDownEvent, _, cx| {
            if e.keystroke.key == "escape" && this.rename.is_none() {
                cx.stop_propagation();
                cx.emit(LibraryEvent::Close);
            }
        });
        h_flex()
            .id("library-page")
            .size_full()
            .bg(p.surface_1)
            .track_focus(&self.focus)
            .on_key_down(escape)
            .role(gpui::Role::Group)
            .aria_label("Bookmarks and history")
            .child(self.render_list(cx))
            .child(
                h_flex().flex_1().min_w(px(0.0)).h_full().justify_center().child(
                    v_flex()
                        .id("library-column")
                        .flex_1()
                        .min_w(px(0.0))
                        .max_w(px(COLUMN_MAX))
                        .h_full()
                        .overflow_y_scroll()
                        .pt(px(scale::SP_6))
                        .px(px(scale::SP_7))
                        .pb(px(scale::SP_7))
                        .gap(px(scale::SP_5))
                        .child(
                            h_flex()
                                .w_full()
                                .items_center()
                                .gap(px(scale::SP_4))
                                .child(
                                    div()
                                        .id("library-h1")
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .ui(TextRole::Title)
                                        .text_color(p.ink)
                                        .role(gpui::Role::Heading)
                                        .aria_label(heading.clone())
                                        .child(heading),
                                )
                                .child(tools),
                        )
                        .child(body),
                ),
            )
    }
}
