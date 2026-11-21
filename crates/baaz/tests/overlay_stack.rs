//! Plan 4.6 (audit A R5 / F18): one overlay stack, its views, one picker.
//!
//! baaz is a binary, so the shape is pinned in source text: Escape's
//! `cancel` names no overlay (it pops the stack, then walks what stands),
//! the overlay windows keep no `*_pending` twin beside their slot, the
//! loose open flags stay gone from the views that used to own them, the
//! root's focus edge detector stays deleted (4.6a); each dialog, sheet and
//! palette is its own view dismissed through the stack, and every list is
//! picked through the one `Picker` (4.6b). The behaviour (Escape pops the
//! newest entry and gives the keyboard back, a covering entry covers the
//! page, a menu's button toggles it, a sheet's own Escape closes the menu
//! over it first, a picker redraws without the root) is pinned by gpui tests
//! in `src/overlay_stack.rs`, `src/overlays.rs` and `src/dialogs.rs`.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
/// `name` under `src/`; a module the core crates took over (docs/82 4.11)
/// is read where it lives now: `browser/x.rs` in `baaz-browser-core`,
/// anything else in `baaz-core`.
fn resolve(name: &str) -> PathBuf {
    let app = manifest().join("src").join(name);
    if app.exists() {
        return app;
    }
    match name.strip_prefix("browser/") {
        Some(rest) => manifest().join("../baaz-browser-core/src").join(rest),
        None => manifest().join("../baaz-core/src").join(name),
    }
}


fn source(name: &str) -> String {
    let path = resolve(name);
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
    const ALLOWED: [&str; 29] = [
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
        "arm_composer_focus",
        "map",
        "unwrap_or",
        "notify",
        // The first run's regions strip (docs/82 8.7): in the session
        // column's flow, like the selection and the card field — no overlay.
        "is_some",
        "dismiss_regions_strip",
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
    let gone: [(&str, &[&str]); 13] = [
        (
            "app.rs",
            &[
                // 4.6b: the dialog and the palette are views with their own
                // focus and query fields.
                "focus_dialog: FocusHandle",
                "focus_palette: FocusHandle",
                "projects_query: Entity<",
                "commands_query: Entity<",
                "muse_sheet: bool",
                "renaming: Option<String>",
                "renaming_project: Option<String>",
                "project_colour_open",
                "chrome_import: Option<",
                "menu_dismissed",
            ],
        ),
        ("session.rs", &["plus_open: bool", "meter_open: bool"]),
        // 4.6b: the leftovers.
        ("skills_page.rs", &["overflow_open: bool", "add_menu_open: bool"]),
        ("right_changes.rs", &["pub base_menu: bool"]),
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
                "bookmark_edit_sub",
            ],
        ),
        ("right_pane.rs", &["files_menu: Option<"]),
        ("right.rs", &["pub page_menu: bool"]),
        ("settings.rs", &["assistant_menu_open: bool", "default_menu_open: bool"]),
        ("connect.rs", &["menu_open: bool"]),
        ("setup.rs", &["sheet: Option<SetupSheet>"]),
        ("import_ui.rs", &["sheet: Option<ImportSheet>"]),
        ("app/agent_window.rs", &["handoff_picker_open", "handoff_query:", "handoff_selected:"]),
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
/// it took (`OverlayStack::take_restore`, applied by the root's settle and
/// when a frame loses the focus, task 4.9 — never from render).
#[test]
fn the_focus_edge_detector_is_gone() {
    let app = source("app.rs");
    for field in ["overlay_was_open", "composer_menu_was_open", "composer_menu_focus", "overlay_just_closed"] {
        assert!(!app.contains(field), "`{field}` came back");
    }
    let sync = source("app/window_sync.rs");
    assert!(sync.contains("overlays.stack.take_restore()"), "the root applies the stack's restore");
    assert!(!app.contains("take_restore"), "render applies no restore");
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

/// Plan 4.6b: one picker. ⌘K and its lists, the Projects palette, the model
/// picker, the hand-off picker and the `@` file mention each construct a
/// `Picker<…>` over their own delegate, and no second picker type stands
/// beside it (the selection, the arrows, ↩ and the dismissal are the one
/// view's).
#[test]
fn one_picker_type_serves_every_list() {
    let picker = source("picker.rs");
    assert!(picker.contains("pub(crate) struct Picker<D: PickerDelegate> {"), "the one picker");
    assert!(picker.contains("pub(crate) trait PickerDelegate"), "the one delegate contract");
    let palette = source("palette.rs");
    assert!(palette.contains("pub(crate) type PaletteView = Picker<PaletteDelegate>;"), "the palette is a picker");
    let show = body(&palette, "    pub(crate) fn show_palette(");
    for (list, constructor) in [("⌘K (Commands) and Projects", "Picker::with_own_field("), ("search", "Picker::with_field("), ("resume and fork", "Picker::new(")] {
        assert!(show.contains(constructor), "{list} constructs a Picker through `{constructor}`");
    }
    let projects = body(&palette, "    pub(crate) fn open_projects(");
    assert!(projects.contains("self.show_palette(PaletteKind::Projects, cx)"), "the Projects palette is the palette picker");
    let pickers = source("session/pickers.rs");
    assert!(pickers.contains("pub(crate) type ModelPicker = Picker<ModelDelegate>;"), "the model picker is a picker");
    assert!(pickers.contains("pub(crate) type MentionPicker = Picker<MentionDelegate>;"), "the @ mention is a picker");
    assert!(body(&pickers, "    pub(super) fn open_model_picker(").contains("Picker::new(delegate, cx)"), "the model chip constructs a Picker");
    assert!(body(&pickers, "    pub(super) fn open_mention_picker(").contains("Picker::new(delegate, cx)"), "an @ constructs a Picker");
    let handoff = source("app/browser_handoff.rs");
    assert!(handoff.contains("pub(crate) type HandoffPicker = crate::picker::Picker<HandoffDelegate>;"), "the hand-off picker is a picker");
    assert!(body(&handoff, "    pub(crate) fn toggle_handoff_picker(").contains("crate::picker::Picker::with_own_field("), "the card constructs a Picker");
    // Every delegate implements the one contract.
    for (file, delegate) in [
        ("palette.rs", "impl PickerDelegate for PaletteDelegate"),
        ("session/pickers.rs", "impl PickerDelegate for ModelDelegate"),
        ("session/pickers.rs", "impl PickerDelegate for MentionDelegate"),
        ("app/browser_handoff.rs", "impl crate::picker::PickerDelegate for HandoffDelegate"),
    ] {
        assert!(source(file).contains(delegate), "{file}: `{delegate}`");
    }
}

/// Plan 4.6b: each dialog, sheet and palette is its own view that emits
/// `DismissEvent`, and the stack entry holds that view: the one close path
/// (Escape, ✕, a click away) is the view's dismissal, which the stack pops
/// newest-first.
#[test]
fn each_overlay_view_dismisses_through_the_stack() {
    for (file, view) in [
        ("dialogs.rs", "DialogView"),
        ("login.rs", "MuseSheetView"),
        ("setup.rs", "SetupHost"),
        ("import_ui.rs", "ImportSheetView"),
        ("chrome_import_ui.rs", "ChromeImportView"),
        ("overlay/bookmark.rs", "BookmarkView"),
    ] {
        let text = source(file);
        assert!(text.contains(&format!("impl gpui::EventEmitter<gpui::DismissEvent> for {view} {{}}")), "{file}: {view} emits DismissEvent");
        assert!(text.contains(&format!("impl Render for {view} {{")), "{file}: {view} renders itself");
    }
    let picker = source("picker.rs");
    assert!(picker.contains("impl<D: PickerDelegate> EventEmitter<DismissEvent> for Picker<D> {}"), "the picker emits DismissEvent");
    let app = source("app.rs");
    let push = body(&app, "    pub(crate) fn push_overlay_view<V: gpui::ManagedView>(");
    assert!(push.contains("this.dismiss_overlay_view(view.entity_id(), cx);"), "a view's DismissEvent pops its entry");
    let dismiss = body(&app, "    pub(crate) fn dismiss_overlay_view(");
    assert!(dismiss.contains("self.pop_overlay_entry(cx)"), "newest-first: the dismissal pops the top");
    // The sheets' own Escape and the panels' close go through it.
    for (file, sheet) in [("login.rs", "MuseSheetView"), ("setup.rs", "SetupHost"), ("import_ui.rs", "ImportSheetView"), ("chrome_import_ui.rs", "ChromeImportView")] {
        let text = source(file);
        let at = text.find("event.keystroke.key == \"escape\"").unwrap_or_else(|| panic!("{file}: {sheet} handles its own Escape"));
        let handler: String = text[at..].lines().take(3).collect();
        assert!(handler.contains("cx.emit(gpui::DismissEvent);"), "{file}: {sheet}'s Escape dismisses through the stack");
        assert!(!handler.contains("this."), "{file}: {sheet}'s Escape closes nothing itself");
    }
    let overlay = source("overlay.rs");
    let close = body(&overlay, "    pub(crate) fn close_overlay_content(");
    assert!(close.contains("view.update(cx, |_, cx| cx.emit(gpui::DismissEvent))"), "the bookmark panel's close is its view's dismissal");
    assert!(close.contains("self.dismiss_overlay_view(view.entity_id(), cx)"), "the hosted modal's close is its view's dismissal");
    // The dialog body left the root.
    let dialogs = source("dialogs.rs");
    assert!(!dialogs.contains("pub(crate) fn render_dialog("), "the dialog renders itself");
    assert!(!dialogs.contains("pub(crate) fn render_palette("), "the palette renders itself");
}
