//! D6's Files screen in Baaz (docs/43 §2.4–2.5, §4): the project tree with
//! git letters and faint ignored paths, its filter, a capped folder's line,
//! the keyboard, and the file preview as a pushed view — code (banded on a
//! link's lines), Markdown that reads like an answer, an image, a file Baaz
//! does not show, a file that is gone.

use aui::util::ScrollAxisLock as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aui::workbench::{
    code_skeleton, code_view, file_tree, filter_bar, image_facts, image_missing, image_view, markdown_view, pane_action, pane_row, pushed_row,
    secondary_action, skeleton_rows, state_block, FileNode, FileTreeAction,
};
use aui_icons::{FileType, IconName};
use aui_tokens::{scale, ActiveAui};
use gpui::{div, prelude::*, px, AnyElement, Context, Entity, IntoElement, SharedString, Window};
use gpui_kit::base::input::InputState;
use gpui_kit::base::v_flex;

use crate::app::Harness;
use crate::right::{FilePreview, PreviewContent, RightCache};

/// A text preview's split lines: (path, text length, lines).
type LinesMemo = (String, usize, Arc<Vec<String>>);

/// Files' own UI state: the filter and the keyboard cursor.
#[derive(Default)]
pub(crate) struct FilesUi {
    /// The filter field while it stands open.
    pub filter: Option<Entity<InputState>>,
    /// Every file path in the project (for the filter), read once per open.
    pub all_paths: Option<(PathBuf, Arc<Vec<String>>)>,
    pub all_paths_reading: bool,
    /// The row the keyboard is on.
    pub cursor: Option<String>,
    /// The code preview's lines, split once per loaded file.
    pub lines_memo: std::cell::RefCell<Option<LinesMemo>>,
    /// The reveal token the code view last banded with.
    pub band_token: u64,
}

/// The lines of a text preview, split once per file and kept.
fn preview_lines(ui: &FilesUi, preview: &FilePreview, code: &str) -> Arc<Vec<String>> {
    let mut memo = ui.lines_memo.borrow_mut();
    if let Some((path, len, lines)) = memo.as_ref() {
        if *path == preview.path && *len == code.len() {
            return lines.clone();
        }
    }
    let lines: Arc<Vec<String>> = Arc::new(code.lines().map(str::to_string).collect());
    *memo = Some((preview.path.clone(), code.len(), lines.clone()));
    lines
}

/// What a file Baaz does not show is, by its extension.
fn kind_of(name: &str) -> String {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "db" | "sqlite" | "sqlite3" => "SQLite database".into(),
        "zip" => "ZIP archive".into(),
        "gz" | "tgz" => "gzip archive".into(),
        "pdf" => "PDF document".into(),
        "wasm" => "WebAssembly module".into(),
        "" => "Binary file".into(),
        other => format!("{} file", other.to_uppercase()),
    }
}

/// Bytes as the canvas writes them: `4.2 MB`, `312 KB`.
fn size_label(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// The filter's matches over `paths`: every file whose path contains the
/// query (case-insensitive), with its folders open above it — nothing else.
pub(crate) fn filtered_nodes(paths: &[String], query: &str) -> (Vec<FileNode>, usize) {
    let q = query.trim().to_lowercase();
    let mut matches: Vec<&String> = paths.iter().filter(|p| p.to_lowercase().contains(&q)).collect();
    matches.sort();
    let count = matches.len();
    let mut nodes = Vec::new();
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    for path in matches.into_iter().take(300) {
        let parts: Vec<&str> = path.split('/').collect();
        for depth in 0..parts.len() {
            let id = parts[..=depth].join("/");
            if depth + 1 < parts.len() {
                if emitted.insert(id.clone()) {
                    nodes.push(FileNode::dir(id, parts[depth].to_string(), true).depth(depth));
                }
            } else {
                nodes.push(FileNode::file(id, parts[depth].to_string(), file_type(parts[depth])).depth(depth));
            }
        }
    }
    (nodes, count)
}

fn file_type(name: &str) -> FileType {
    let l = name.to_ascii_lowercase();
    if l.ends_with(".json") {
        FileType::Json
    } else if l.ends_with(".lock") {
        FileType::Lock
    } else {
        FileType::File
    }
}

/// Every file in the project for the filter: `git ls-files` (tracked and
/// untracked, ignores honoured) in a repo, else a walk that skips the
/// vendored folders. Blocking.
fn list_all_paths(root: &Path) -> Vec<String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    if let Some(out) = crate::git_ops::spawn("git", root, &["ls-files", "--cached", "--others", "--exclude-standard", "-z"], None, false).and_then(|j| j.finish(deadline)) {
        if out.ok {
            return out.text.split('\0').filter(|p| !p.is_empty()).map(str::to_string).collect();
        }
    }
    let mut paths = Vec::new();
    for entry in ignore::WalkBuilder::new(root).hidden(false).build().flatten().take(50_000) {
        if entry.file_type().is_some_and(|t| t.is_file()) {
            if let Ok(rel) = entry.path().strip_prefix(root) {
                paths.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    paths
}

impl Harness {
    /// Open (or focus) Files' filter (⌘F, /, the row's search button).
    pub(crate) fn open_files_filter(&mut self, root: &Path, window: &mut Window, cx: &mut Context<Self>) {
        if self.right_cache.files_ui.filter.is_none() {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder("Filter files"));
            cx.observe(&state, |_, _, cx| cx.notify()).detach();
            self.right_cache.files_ui.filter = Some(state);
        }
        if let Some(state) = &self.right_cache.files_ui.filter {
            let focus = gpui::Focusable::focus_handle(state.read(cx), cx);
            window.focus(&focus, cx);
        }
        let stale = self.right_cache.files_ui.all_paths.as_ref().is_none_or(|(r, _)| r != root);
        if stale && !self.right_cache.files_ui.all_paths_reading {
            self.right_cache.files_ui.all_paths_reading = true;
            let root = root.to_path_buf();
            cx.spawn(async move |this, cx| {
                let r = root.clone();
                let paths = cx.background_executor().spawn(async move { list_all_paths(&r) }).await;
                let _ = this.update(cx, |this: &mut Self, cx| {
                    this.right_cache.files_ui.all_paths = Some((root, Arc::new(paths)));
                    this.right_cache.files_ui.all_paths_reading = false;
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    /// Esc in the filter: clear it and close it.
    pub(crate) fn close_files_filter(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.right_cache.files_ui.filter.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    /// A key in the Files screen (docs/43 §4): ↑ ↓ move, → opens a folder or
    /// enters it, ← closes or goes to the parent, ⏎ previews, ⌘F or /
    /// opens the filter, Esc closes the filter or pops the preview.
    pub(crate) fn files_key(&mut self, key: &str, cmd: bool, nodes: &[FileNode], root: &Path, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if (key == "f" && cmd) || (key == "/" && !cmd) {
            self.open_files_filter(root, window, cx);
            return true;
        }
        if key == "escape" {
            return self.close_files_menu(cx) || self.close_files_filter(cx) || self.close_file_preview_for(root, cx);
        }
        let rows: Vec<&FileNode> = nodes.iter().filter(|n| !n.note).collect();
        if rows.is_empty() {
            return false;
        }
        let at = self.right_cache.files_ui.cursor.as_ref().and_then(|c| rows.iter().position(|n| n.id.as_ref() == c));
        let set = |this: &mut Self, ix: usize| {
            this.right_cache.files_ui.cursor = Some(rows[ix].id.to_string());
            this.right_cache.note_reveal(root, rows[ix].id.as_ref());
        };
        match key {
            "down" => set(self, at.map_or(0, |i| (i + 1).min(rows.len() - 1))),
            "up" => set(self, at.map_or(0, |i| i.saturating_sub(1))),
            "right" => {
                let Some(i) = at else { return false };
                match rows[i].open {
                    Some(false) => self.toggle_files_dir(root, rows[i].id.as_ref(), cx),
                    Some(true) if i + 1 < rows.len() && rows[i + 1].depth > rows[i].depth => set(self, i + 1),
                    _ => {}
                }
            }
            "left" => {
                let Some(i) = at else { return false };
                if rows[i].open == Some(true) {
                    self.toggle_files_dir(root, rows[i].id.as_ref(), cx);
                } else if let Some(p) = (0..i).rev().find(|&j| rows[j].depth + 1 == rows[i].depth && rows[i].depth > 0) {
                    set(self, p);
                }
            }
            "enter" => {
                let Some(i) = at else { return false };
                let id = rows[i].id.to_string();
                if rows[i].is_dir() {
                    self.toggle_files_dir(root, &id, cx);
                } else {
                    self.open_file_entry(root, &id, cx);
                }
            }
            _ => return false,
        }
        aui::keys::set_keyboard_nav(true, cx);
        cx.notify();
        true
    }
}

/// The Files screen: its row and body. A preview is a pushed view.
#[allow(clippy::too_many_arguments)]
pub(crate) fn files_screen(
    cache: &RightCache,
    project: Option<(PathBuf, String)>,
    width: f32,
    focus: &gpui::FocusHandle,
    window: &mut Window,
    cx: &mut Context<Harness>,
) -> (AnyElement, AnyElement) {
    let Some((root, _name)) = project else {
        return (
            pane_row("right-row-files", IconName::Folder, "Files").on_close(cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| this.toggle_right(cx))).into_any_element(),
            state_block("right-files-empty", IconName::Folder, "No project").line("Open or add a project to see its files.").into_any_element(),
        );
    };
    if let Some(preview) = cache.preview_for(&root) {
        return preview_screen(cache, &root, &preview, width, window, cx);
    }
    let ui = &cache.files_ui;
    let filter_open = ui.filter.is_some();
    let search_root = root.clone();
    let mut row = pane_row("right-row-files", IconName::Folder, "Files")
        .action(pane_action("right-files-filter", IconName::Search, "Filter files").toggled(filter_open).on_click(cx.listener(move |this: &mut Harness, _, window, cx| {
            if this.right_cache.files_ui.filter.is_some() {
                this.close_files_filter(cx);
            } else {
                this.open_files_filter(&search_root, window, cx);
            }
        })))
        .on_close(cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| this.toggle_right(cx)))
        .into_any_element();
    let tree = cache.git_for(&root).flatten();
    let query = ui.filter.as_ref().map(|f| f.read(cx).value().to_string()).unwrap_or_default();
    let (nodes, count): (Vec<FileNode>, Option<String>) = if filter_open && !query.trim().is_empty() {
        match &ui.all_paths {
            Some((r, paths)) if *r == root => {
                let (nodes, n) = filtered_nodes(paths, &query);
                let total = paths.len();
                (crate::right::decorate_nodes(nodes, tree.as_ref()), Some(format!("{} of {}", aui::workbench::group_thousands(n).replace(' ', ","), aui::workbench::group_thousands(total).replace(' ', ","))))
            }
            _ => (Vec::new(), None),
        }
    } else {
        match cache.files_for(&root) {
            Some((nodes, _)) => {
                let selected = cache.selected_for(&root);
                (crate::right::decorate_nodes(crate::right::mark_selected_pub(nodes, selected.as_deref()), tree.as_ref()), None)
            }
            None => {
                let body = skeleton_rows(&[(0, 64.0), (0, 88.0), (0, 56.0), (0, 112.0), (1, 72.0), (1, 96.0), (0, 60.0)], cx).into_any_element();
                return (row, body);
            }
        }
    };
    let handler_root = root.clone();
    let weak = cx.weak_entity();
    let mut t = file_tree("right-files", nodes.clone())
        .flush()
        .track_scroll(&cache.files_scroll_for(&root))
        .focused(ui.cursor.clone().map(SharedString::from))
        .on_action(move |action, window, cx| {
            let root = handler_root.clone();
            let _ = weak.update(cx, |this, cx| {
                match action {
                    FileTreeAction::Toggle(id) => {
                        this.right_cache.files_ui.cursor = Some(id.to_string());
                        this.toggle_files_dir(&root, id, cx);
                    }
                    FileTreeAction::Select(id) => {
                        this.right_cache.files_ui.cursor = Some(id.to_string());
                        this.open_file_entry(&root, id, cx);
                    }
                }
                window.focus(&this.right_files_focus.clone(), cx);
            });
        })
        .on_context_menu({
            let (root, weak) = (root.clone(), cx.weak_entity());
            move |menu, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.files_menu = Some(FilesMenu { root: root.clone(), id: menu.id.to_string(), is_dir: menu.is_dir, at: menu.at });
                    cx.notify();
                });
            }
        });
    if let Some((reveal, token)) = cache.reveal.get(&root) {
        t = t.scroll_to(reveal.clone()).scroll_token(*token);
    }
    let key_root = root.clone();
    let key_nodes = nodes;
    let (bar_root, bar_nodes) = (key_root.clone(), key_nodes.clone());
    let body = v_flex()
        .id("right-files-body")
        .size_full()
        .track_focus(focus)
        .key_context("RightFiles")
        .on_key_down(cx.listener(move |this: &mut Harness, e: &gpui::KeyDownEvent, window, cx| {
            let key = e.keystroke.key.as_str();
            if this.files_key(key, e.keystroke.modifiers.platform, &key_nodes, &key_root, window, cx) {
                cx.stop_propagation();
            }
        }));
    // One bar (owner 2026-10-21, canvas D6OneBar): while filtering, the
    // row itself is the field — no second row under it.
    if let Some(filter) = &ui.filter {
        // The field's keys (Esc, ↑ ↓, ⏎) reach the tree's handler as they
        // did when the field sat inside the body.
        row = filter_bar("right-files-filter-field", filter, count.map(SharedString::from), cx.listener(|this: &mut Harness, _: &gpui::ClickEvent, _, cx| {
            this.close_files_filter(cx);
        }), window, cx)
        .key_context("RightFiles")
        .on_key_down(cx.listener(move |this: &mut Harness, e: &gpui::KeyDownEvent, window, cx| {
            let key = e.keystroke.key.as_str();
            if this.files_key(key, e.keystroke.modifiers.platform, &bar_nodes, &bar_root, window, cx) {
                cx.stop_propagation();
            }
        }))
        .into_any_element();
    }
    let body = body.child(div().flex_1().min_h(px(0.0)).child(t)).into_any_element();
    (row, body)
}

/// The preview: a view pushed on Files (docs/43 §2.5).
fn preview_screen(cache: &RightCache, root: &Path, preview: &FilePreview, width: f32, window: &mut Window, cx: &mut Context<Harness>) -> (AnyElement, AnyElement) {
    let p = cx.aui().colors;
    let abs = root.join(&preview.path);
    let folder = preview.path.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
    let back_root = root.to_path_buf();
    let open_abs = abs.clone();
    let gone = matches!(&preview.content, PreviewContent::Card { meta } if meta.contains("gone"));
    let mut row = pushed_row("right-file-row", preview.name.clone())
        .back_label("Back to files")
        .width(px(width))
        .on_back(cx.listener(move |this: &mut Harness, _, window, cx| {
            this.close_file_preview_for(&back_root, cx);
            // The tree takes the keyboard back, so ↑ ↓ go on from the row
            // that was open (live 2026-10-19: the keys went nowhere).
            window.focus(&this.right_files_focus.clone(), cx);
        }))
        .on_close(cx.listener(|this: &mut Harness, _, _, cx| this.toggle_right(cx)));
    if !folder.is_empty() {
        row = row.context(folder);
    }
    if !gone {
        row = row.action(pane_action("right-file-open", IconName::External, "Open in default app").on_click(move |_, _, cx| {
            if open_abs.exists() {
                cx.open_with_system(&open_abs);
            }
        }));
    }
    let image = crate::right::is_image_path(&preview.name);
    let body: AnyElement = match &preview.content {
        PreviewContent::Loading => code_skeleton(&[118.0, 92.0, 152.0, 132.0, 66.0, 112.0, 142.0, 74.0, 126.0, 100.0], cx).into_any_element(),
        _ if gone => state_block("right-file-gone", IconName::File, "This file is gone")
            .line_with(div().child(div().font_family(scale::FONT_MONO).text_color(p.fg_muted).child(preview.path.clone())).child("was moved or deleted since you opened it."))
            .into_any_element(),
        _ if image => match crate::right_views::image_size(&abs) {
            Some((w, h)) => {
                let kind = preview.name.rsplit_once('.').map(|(_, e)| e.to_string()).unwrap_or_default();
                let actual = cache.files_image_actual;
                image_view("right-file-image", abs.clone(), image_facts(w, h, &kind, preview.size), Some((w as f32, h as f32)), actual, window, cx)
                    .on_click(cx.listener(|this: &mut Harness, _, _, cx| {
                        this.right_cache.files_image_actual = !this.right_cache.files_image_actual;
                        cx.notify();
                    }))
                    .into_any_element()
            }
            None => image_missing("right-file-image-missing", &preview.name, cx).into_any_element(),
        },
        PreviewContent::Text { language, code } => {
            let lines = preview_lines(&cache.files_ui, preview, code);
            let mut view = code_view("right-file-code", lines).language(language.clone());
            if let Some(band) = preview.highlight.clone() {
                view = view.band(band, cache.files_ui.band_token);
            }
            view.into_any_element()
        }
        PreviewContent::Markdown { text } => div().id("right-file-md").size_full().overflow_y_scroll().scroll_axis_locked().child(markdown_view("right-file-md-body", text.clone(), cx)).into_any_element(),
        PreviewContent::Card { .. } => {
            let open_abs = abs.clone();
            state_block("right-file-notshown", IconName::File, preview.name.clone())
                .line(format!("{} · {}. Baaz shows text, Markdown and images here.", kind_of(&preview.name), size_label(preview.size)))
                .action(secondary_action("right-file-notshown-open", "Open in default app", Some(IconName::External), cx).on_click(move |_, _, cx| {
                    if open_abs.exists() {
                        cx.open_with_system(&open_abs);
                    }
                }))
                .into_any_element()
        }
    };
    (row.into_any_element(), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_filter_keeps_matches_inside_their_open_folders() {
        let paths = vec!["crates/baaz/src/right.rs".to_string(), "crates/baaz/src/app.rs".into(), "docs/right-pane.md".into(), "README.md".into()];
        let (nodes, n) = filtered_nodes(&paths, "RIGHT");
        assert_eq!(n, 2);
        let ids: Vec<(String, usize, bool)> = nodes.iter().map(|n| (n.id.to_string(), n.depth, n.is_dir())).collect();
        assert_eq!(
            ids,
            vec![
                ("crates".into(), 0, true),
                ("crates/baaz".into(), 1, true),
                ("crates/baaz/src".into(), 2, true),
                ("crates/baaz/src/right.rs".into(), 3, false),
                ("docs".into(), 0, true),
                ("docs/right-pane.md".into(), 1, false),
            ]
        );
    }

    #[test]
    fn a_file_baaz_does_not_show_names_its_kind() {
        assert_eq!(kind_of("app.db"), "SQLite database");
        assert_eq!(kind_of("x.bin"), "BIN file");
        assert_eq!(size_label(4_404_019), "4.2 MB");
    }
}

/// The open Files context menu (owner 2026-10-21): the row it belongs to and
/// where the secondary click landed.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct FilesMenu {
    pub root: PathBuf,
    pub id: String,
    pub is_dir: bool,
    pub at: gpui::Point<gpui::Pixels>,
}

/// One row of the Files context menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FilesMenuAction {
    OpenDefault,
    Reveal,
    ViewSource,
}

/// The menu's rows for a row: every file gets Open in default app and Reveal
/// in Finder, a file that otherwise renders (HTML, SVG, Markdown) also View
/// source; a folder gets Reveal in Finder only.
pub(crate) fn files_menu_rows(id: &str, is_dir: bool) -> Vec<(&'static str, FilesMenuAction)> {
    if is_dir {
        return vec![("Reveal in Finder", FilesMenuAction::Reveal)];
    }
    let mut rows = vec![("Open in default app", FilesMenuAction::OpenDefault), ("Reveal in Finder", FilesMenuAction::Reveal)];
    if crate::right::has_source_view(id) {
        rows.push(("View source", FilesMenuAction::ViewSource));
    }
    rows
}

impl Harness {
    /// Open a file row (click or ⏎): HTML, SVG and PDF render in the Page;
    /// everything else previews here (owner 2026-10-21).
    pub(crate) fn open_file_entry(&mut self, root: &Path, id: &str, cx: &mut Context<Self>) {
        if crate::right::opens_in_page(id) {
            self.right_cache.selected.insert(root.to_path_buf(), id.to_string());
            self.open_url_in_browser(crate::right::file_url(&root.join(id)), cx);
        } else {
            self.begin_file_preview_for(root, id, cx);
        }
    }

    /// Close the Files context menu; whether one was open (Escape's first
    /// layer, from the tree or from anywhere the window's Cancel reaches).
    pub(crate) fn close_files_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.files_menu.take().is_some();
        if open {
            cx.notify();
        }
        open
    }

    fn run_files_menu(&mut self, menu: &FilesMenu, action: FilesMenuAction, cx: &mut Context<Self>) {
        let abs = menu.root.join(&menu.id);
        match action {
            FilesMenuAction::OpenDefault => cx.open_with_system(&abs),
            FilesMenuAction::Reveal => cx.reveal_path(&abs),
            FilesMenuAction::ViewSource => {
                self.right_cache.files_ui.cursor = Some(menu.id.clone());
                self.begin_file_source_for(&menu.root, &menu.id, cx);
            }
        }
    }

    /// The Files context menu, seated at the secondary click: the app's one
    /// menu over a window-wide catcher, as the project menu draws.
    pub(crate) fn render_files_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.files_menu.clone()?;
        let rows = files_menu_rows(&menu.id, menu.is_dir);
        let actions: Vec<FilesMenuAction> = rows.iter().map(|(_, a)| *a).collect();
        let view_rows: Vec<aui::nav::MenuRow> = rows.iter().map(|(label, _)| aui::nav::MenuRow::Toggle { label: (*label).into(), checked: false }).collect();
        let held = menu.clone();
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            this.files_menu = None;
            if let Some(action) = actions.get(*index) {
                this.run_files_menu(&held, *action, cx);
            }
            cx.notify();
        });
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.files_menu = None;
            cx.notify();
        });
        let catcher = div()
            .id("files-menu-scrim")
            .occlude()
            .absolute()
            .inset_0()
            .on_mouse_down(gpui::MouseButton::Right, move |_, w, cx| dismiss(&(), w, cx))
            .on_click({
                let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
                    this.files_menu = None;
                    cx.notify();
                });
                move |_, w, cx| dismiss(&(), w, cx)
            });
        let trigger = gpui::Bounds { origin: menu.at, size: gpui::size(px(0.0), px(0.0)) };
        let list = aui::nav::view_menu("files-menu", view_rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx));
        Some(
            div()
                .absolute()
                .inset_0()
                .child(catcher)
                .child(aui::overlay::anchored_menu(trigger, aui::overlay::MenuSide::Below, aui::overlay::MenuAlign::Start, list))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod menu_tests {
    use super::{files_menu_rows, FilesMenuAction};

    #[test]
    fn the_menu_offers_view_source_only_where_a_file_renders() {
        let actions = |id: &str, dir: bool| files_menu_rows(id, dir).into_iter().map(|(_, a)| a).collect::<Vec<_>>();
        use FilesMenuAction::*;
        assert_eq!(actions("site/index.html", false), [OpenDefault, Reveal, ViewSource]);
        assert_eq!(actions("README.md", false), [OpenDefault, Reveal, ViewSource]);
        assert_eq!(actions("logo.svg", false), [OpenDefault, Reveal, ViewSource]);
        assert_eq!(actions("src/main.rs", false), [OpenDefault, Reveal]);
        assert_eq!(actions("report.pdf", false), [OpenDefault, Reveal]);
        assert_eq!(actions("docs", true), [Reveal]);
    }
}
