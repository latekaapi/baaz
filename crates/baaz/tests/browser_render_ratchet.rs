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


fn source(rel: &str) -> String {
    std::fs::read_to_string(resolve(rel)).unwrap_or_else(|e| panic!("read {rel}: {e}"))
}

/// Every `browser/*.rs` file's text (the module is split by seam).
fn browser_source() -> String {
    let dir = manifest().join("src").join("browser");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .expect("src/browser reads")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "mod.rs")
        .chain(core_browser_files())
        .collect();
    names.sort();
    std::iter::once("mod.rs".to_owned()).chain(names).map(|name| source(&format!("browser/{name}"))).collect::<Vec<_>>().join("\n")
}

/// The browser module's files that live in `baaz-browser-core` (docs/82
/// 4.11), as paths from `src/browser/` (a name both crates use, like
/// `adblock.rs`, is read from each).
fn core_browser_files() -> Vec<String> {
    std::fs::read_dir(manifest().join("../baaz-browser-core/src"))
        .expect("baaz-browser-core/src reads")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "lib.rs")
        .map(|name| format!("../../../baaz-browser-core/src/{name}"))
        .collect()
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

/// The argument text of every `marker(` call in `text`, up to its matching
/// `)` (string literals in these files hold no parentheses that matter).
fn call_args<'a>(text: &'a str, marker: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(marker) {
        let open = from + at + marker.len();
        let mut depth = 1usize;
        let mut end = open;
        for (i, c) in text[open..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = open + i;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(&text[open..end]);
        from = open;
    }
    out
}

/// Gate 4 (19 Nov): gpui logged "handling resize … RefCell already
/// borrowed". A float's native chrome came off inside `open_window`'s build
/// closure, itself inside the open task's `cx.update`; the style-mask swap
/// resizes the panel's view and AppKit calls gpui's resize callback from
/// inside it, which cannot borrow the App while an update holds it, so the
/// resize was dropped. The AppKit setters now run from the open task
/// between updates. Pinned: no float's `open_window` or `cx.update` names
/// them, every float drops its chrome, and the helper takes the task's
/// `AsyncApp` (no `&Window`, which only an update hands out).
#[test]
fn a_floats_native_chrome_comes_off_outside_any_update() {
    let files = ["overlay.rs", "overlay/card.rs"];
    let mut opens = 0;
    let mut drops = 0;
    for name in files {
        let text = source(name);
        for marker in ["open_window(", "cx.update("] {
            for args in call_args(&text, marker) {
                for platform in ["drop_native_chrome", "msg_send"] {
                    assert!(!args.contains(platform), "{name}: `{platform}` inside `{marker}…)` — AppKit calls that resize re-enter gpui's resize callback while the App is borrowed");
                }
            }
        }
        opens += text.matches("cx.open_window(").count();
        drops += text.matches("drop_native_chrome((*opened).into(),").count();
    }
    assert_eq!(opens, 5, "the five floats (overlay, corner, page float, card, agent cursor) open here");
    assert_eq!(drops, opens, "every float drops its native chrome once it has opened");
    let overlay = source("overlay.rs");
    for signature in ["fn drop_native_chrome(handle: AnyWindowHandle, kind: OverlayKind, cx: &mut gpui::AsyncApp)", "fn drop_native_chrome(_: AnyWindowHandle, _: OverlayKind, _: &mut gpui::AsyncApp)"] {
        assert!(overlay.contains(signature), "the chrome drop takes the open task's AsyncApp: {signature}");
    }
}
