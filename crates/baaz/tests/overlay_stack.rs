//! Plan 4.6a (audit A R5 / F18): one overlay stack.
//!
//! baaz is a binary, so the shape is pinned in source text: Escape's
//! `cancel` names no overlay (it pops the stack, then walks what stands),
//! the overlay windows keep no `*_pending` twin beside their slot, the
//! loose open flags stay gone from the views that used to own them, and the
//! root's focus edge detector stays deleted. The behaviour (Escape pops the
//! newest entry and gives the keyboard back, a covering entry covers the
//! page, a menu's button toggles it) is pinned by gpui tests in
//! `src/overlay_stack.rs`, `src/overlays.rs` and `src/dialogs.rs`.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The body of `fn <name>(` in `text`, from its opening brace to the brace
/// that closes it.
fn body<'a>(text: &'a str, signature: &str) -> &'a str {
    let at = text.find(signature).unwrap_or_else(|| panic!("`{signature}` is in the source"));
    let open = at + text[at..].find('{').expect("a body");
    let mut depth = 0usize;
    for (i, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open..open + i + 1];
                }
            }
            _ => {}
        }
    }
    panic!("`{signature}`'s body closes");
}

/// Every `.name(` call in `code`, comments stripped.
fn calls(code: &str) -> Vec<String> {
    let code: String = code
        .lines()
        .map(|line| match line.find("//") {
            Some(at) => &line[..at],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    for (i, _) in code.match_indices('.') {
        let rest = &code[i + 1..];
        let name: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
        if !name.is_empty() && bytes.get(i + 1 + name.len()) == Some(&b'(') {
            out.push(name);
        }
    }
    out
}

/// Escape's `cancel` pops the overlay stack first, then names only what is
/// no overlay: the Settings page's own Back, the Skills page, the right
/// pane's view stack (a file preview, a pushed view), the transcript's
/// selection, a card's inline field, and the turn. A new popover, menu or
/// sheet goes on the stack — never into this list. Adding a call here needs
/// a reason it is no overlay.
#[test]
fn escape_has_no_overlay_rung() {
    let app = source("app.rs");
    let cancel = body(&app, "    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {");
    const ALLOWED: [&str; 26] = [
        // The stack.
        "pop_overlay",
        // The Settings page's own Back (recording → search → sub-page → close).
        "settings_open",
        "settings_escape",
        // The Skills page (a full-page route, like Settings).
        "skills_open",
        "close_skills",
        // The right pane's view stack.
        "right_project",
        "with_right_pane",
        "close_file_preview_for",
        "read",
        "focus_handle",
        "focus",
        "get",
        "screen_index",
        "right_kind",
        "update",
        "pop_right_view",
        // The transcript's selection and a card's inline field.
        "clone",
        "active",
        "clear_selection",
        "close_card_field",
        "map",
        "unwrap_or",
        "notify",
        // The turn.
        "as_ref",
        "is_some_and",
        "draft_is_empty",
    ];
    let mut unknown: Vec<String> = calls(cancel).into_iter().filter(|call| !ALLOWED.contains(&call.as_str()) && call != "interrupt").collect();
    unknown.dedup();
    assert!(unknown.is_empty(), "`cancel` grew a call the ratchet does not know: {unknown:?} — an overlay belongs on the stack");
    assert!(cancel.trim_start_matches('{').trim_start().starts_with("if self.pop_overlay(window, cx) {"), "Escape pops the stack first");
    for word in ["menu", "sheet", "popover", "palette", "dialog", "rename", "plus", "picker", "_open ="] {
        let code: String = cancel.lines().filter(|line| !line.trim_start().starts_with("//")).collect::<Vec<_>>().join("\n");
        assert!(!code.contains(word), "`cancel` names `{word}`: an overlay rung came back");
    }
}

/// The overlay windows keep one slot each: a slot whose window is still
/// opening has no handle yet, so no `*_pending` twin stands beside it.
#[test]
fn no_overlay_window_has_a_pending_twin() {
    let mut text = String::new();
    for name in ["overlay.rs", "overlay/card.rs", "overlay/bookmark.rs", "browser/view.rs", "app/agent_window.rs"] {
        text.push_str(&source(name));
    }
    for twin in ["overlay_pending", "corner_pending", "page_float_pending", "card_float_pending"] {
        assert!(!text.contains(twin), "`{twin}` came back");
    }
    // No new one either: no field holding an overlay kind or a window's
    // bounds under a `_pending` name.
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("pub") && line.contains("_pending:") {
            assert!(
                !line.contains("OverlayKind") && !line.contains("Bounds"),
                "a pending twin for an overlay window: `{line}`"
            );
        }
    }
}

/// What used to be a loose open flag (or an `Option` that meant "open") on
/// a view is an entry on the overlay stack now; each owner keeps at most
/// the content of its own widgets.
#[test]
fn the_loose_open_flags_are_gone() {
    let gone: [(&str, &[&str]); 11] = [
        (
            "app.rs",
            &[
                "muse_sheet: bool",
                "renaming: Option<String>",
                "renaming_project: Option<String>",
                "project_colour_open",
                "chrome_import: Option<",
                "menu_dismissed",
            ],
        ),
        ("session.rs", &["plus_open: bool"]),
        (
            "browser/view.rs",
            &[
                "downloads_open: bool",
                "extensions_open: bool",
                "more_open: bool",
                "site_info_open: bool",
                "bookmark_edit: Option<",
                "tab_menu: Option<",
                "browse_menu: Option<",
                "page_menu_closed",
            ],
        ),
        ("right_pane.rs", &["files_menu: Option<"]),
        ("right.rs", &["pub page_menu: bool"]),
        ("settings.rs", &["assistant_menu_open: bool", "default_menu_open: bool"]),
        ("connect.rs", &["menu_open: bool"]),
        ("setup.rs", &["sheet: Option<SetupSheet>"]),
        ("import_ui.rs", &["sheet: Option<ImportSheet>"]),
        ("app/agent_window.rs", &["handoff_picker_open"]),
        ("overlays.rs", &["dialog: Option<Dialog>", "menu: Option<Menu>", "palette: Option<Palette>"]),
    ];
    for (file, fields) in gone {
        let text = source(file);
        for field in fields {
            assert!(!text.contains(field), "`{field}` is back in {file}: put it on the overlay stack");
        }
    }
}

/// The root's focus edge detector is gone: the stack gives back the focus
/// it took (`OverlayStack::take_restore`, applied in the root's frame).
#[test]
fn the_focus_edge_detector_is_gone() {
    let app = source("app.rs");
    for field in ["overlay_was_open", "composer_menu_was_open", "composer_menu_focus", "overlay_just_closed"] {
        assert!(!app.contains(field), "`{field}` came back");
    }
    assert!(app.contains("overlays.stack.take_restore()"), "the root applies the stack's restore");
    assert!(app.contains("overlays.stack.note_focus(focused)"), "the root tells the stack what holds the keyboard");
}

/// The page's overlay cover is the stack's one answer: no overlay kind is
/// a hand-built input of its own any more.
#[test]
fn the_page_cover_is_the_stacks_answer() {
    let occlusion = source("browser/occlusion.rs");
    for input in ["palette_open", "menu_open", "plus_open", "dialog_open"] {
        assert!(!occlusion.contains(input), "`{input}` is a cover input again");
    }
    assert!(occlusion.contains("pub overlay_open: bool"), "one overlay input");
    let covers = body(&occlusion, "    pub(crate) fn page_covers(&self, window: &gpui::Window, cx: &gpui::App) -> Covers {");
    assert!(covers.contains("let overlay_open = self.overlays.read(cx).stack.covers_page(cx);"), "the stack answers for every overlay");
}
