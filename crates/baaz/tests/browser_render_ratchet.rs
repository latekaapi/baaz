//! The browser leaves the root's render (task 4.5b, audit F13/F19): the
//! ratchet.
//!
//! The page reconcile — tab URLs and titles, the tabs write, history and
//! page words, popups, downloads, permissions, the discard, the done task
//! folders — used to run as `sync_browser` from `Harness::render`, so a
//! quiet window (no frame) stalled it and a frame could write the tab file
//! or reach `browser.db`. It now runs from the pages' own changes
//! (`BrowserView::adopt_page` observes each page) and the view's timers.
//! What render still does for the browser is decide what covers the native
//! page (`occlusion.rs`), in memory.
//!
//! These pins read source text: render and everything it calls for the
//! browser must name no reconcile, no tabs write and no `browser.db` call.

use std::path::PathBuf;

fn manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(rel: &str) -> String {
    std::fs::read_to_string(manifest().join("src").join(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `browser/*.rs` file's text (the module is split by seam).
fn browser_source() -> String {
    let dir = manifest().join("src").join("browser");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .expect("src/browser reads")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "mod.rs")
        .collect();
    names.sort();
    std::iter::once("mod.rs".to_owned()).chain(names).map(|name| source(&format!("browser/{name}"))).collect::<Vec<_>>().join("\n")
}

/// The body of the first `marker` in `text`: from it to the next closing
/// brace at the 4-space impl indent.
fn body<'a>(text: &'a str, marker: &str) -> &'a str {
    let start = text.find(marker).unwrap_or_else(|| panic!("`{marker}` exists"));
    let rest = &text[start..];
    let end = rest.find("\n    }\n").unwrap_or_else(|| panic!("`{marker}` ends"));
    &rest[..end]
}

/// What no frame may do for the browser: run the page reconcile, write the
/// tabs, or reach `browser.db` (directly or through the service's writes).
const FORBIDDEN: &[&str] = &[
    "sync_browser",
    "reconcile_pages",
    "reconcile_browser_root",
    "record_history",
    "tabs_changed",
    "flush_tabs_save",
    "flush_at_quit",
    "save_soon",
    "tabs::write",
    "ensure_tabs_loaded",
    "drain_browse_popups",
    "discard_hidden",
    "remember_browser_url",
    "close_done_task_tabs",
    "data.update",
    "set_text",
    "history_changed",
    "BrowserDb",
    "rusqlite",
];

fn assert_clean(name: &str, text: &str) {
    let found: Vec<&str> = FORBIDDEN.iter().copied().filter(|word| text.contains(word)).collect();
    assert!(found.is_empty(), "{name} names {found:?}: the browser's reconcile and writes stay out of the frame");
}

/// `Harness::render` and the window's lifecycle (`app/window_sync.rs`) name
/// no browser reconcile and no write; the reconcile function itself is gone.
#[test]
fn render_runs_no_browser_reconcile_and_writes_nothing() {
    let app = source("app.rs");
    let render = body(&app, "impl Render for Harness {\n    fn render(");
    assert!(render.contains("self.occlude_pages(window, cx);"), "render asks the one occlusion owner");
    assert_clean("Harness::render", render);
    assert_clean("app/window_sync.rs", &source("app/window_sync.rs"));
    let browser = browser_source();
    assert!(!browser.contains("fn sync_browser"), "the per-frame sync is gone");
    assert!(!app.contains("sync_browser"), "and nothing calls it");
}

/// What render calls for the browser — the occlusion and the view's own
/// render — decides and draws only.
#[test]
fn the_frames_browser_work_is_occlusion_only() {
    let browser = browser_source();
    for marker in [
        "fn occlude_pages(",
        "fn page_covers(",
        "fn occlude(",
        "fn apply_page_visibility(",
        "fn cut_holes(",
        "fn peek_split_holes(",
        "fn browser_cover_settling(",
        "fn render_page_progress(",
        "fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {\n        self.render_page_progress",
    ] {
        assert_clean(marker, body(&browser, marker));
    }
}

/// The reconcile runs from the pages' own changes: every page the view
/// holds is observed, and so is the Peek's.
#[test]
fn the_reconcile_follows_the_pages() {
    let browser = browser_source();
    let adopt = body(&browser, "fn adopt_page(");
    assert!(adopt.contains("cx.observe(&state, |this, _, cx| this.reconcile_pages(cx))"), "a page's change runs the reconcile");
    assert!(adopt.contains("self.states.insert(id, state);"), "adopting is how a page joins the view");
    let peek = body(&browser, "fn adopt_peek(");
    assert!(peek.contains("this.reconcile_pages(cx)"), "the Peek's page is followed too");
    // `adopt_page` is the one way in: no other `states.insert`.
    assert_eq!(browser.matches("states.insert(").count(), 1, "pages join the view only through adopt_page");
    for name in ["app.rs", "app/agent_window.rs", "app/peek_split.rs", "overlay.rs", "steps.rs"] {
        assert!(!source(name).contains("states.insert("), "{name} adds no page behind the view's back");
    }
}
