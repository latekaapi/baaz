//! B2a-H1b: wiring contracts the unit tests cannot see.
//!
//! Like `browser_z7b.rs`, this suite cannot boot the app (baaz is a
//! binary), so it pins the wiring in source text: the quit-time flush in
//! both quit hooks, the off-UI-thread debounced write, the unregister
//! calls on close and discard, and the strip's close semantics. Each
//! assertion fails on the pre-H1b code.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// Every file of the `browser` module (task 4.5 split `browser/mod.rs` by
/// seam): `mod.rs` first, then the rest by name, so a pin follows a
/// function into whichever file holds it now.
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

/// A pending tabs write lands synchronously in BOTH quit paths — the menu's
/// `QuitApp` and the `on_app_quit` hook for Dock/signal quits — next to the
/// layout/sessions flushes, and `browser.db`'s queued writes drain there
/// too, bounded (task 4.5b).
#[test]
fn quit_flushes_a_pending_tabs_write_in_both_quit_paths() {
    for name in ["app.rs", "main.rs"] {
        let body = source(name);
        assert!(
            body.contains("browser.flush_at_quit(cx)"),
            "{name} flushes tabs at quit next to the sessions flush"
        );
    }
    let browser = browser_source();
    let start = browser.find("fn flush_at_quit(").expect("the quit flush exists");
    let quit = &browser[start..start + browser[start..].find("\n    }\n").expect("it ends")];
    assert!(quit.contains("self.flush_tabs_save();"), "the quit flush writes the tabs now");
    assert!(quit.contains("drain_at_quit(super::data::BrowserData::QUIT_DRAIN)"), "and drains browser.db, bounded");
}

/// The debounced write leaves the UI thread through the tabs store's one
/// writer (docs/82 1.7): `save_soon` queues a snapshot that waits the
/// store's delay, a newer one replaces it while keeping the first due time
/// (the max-delay cap), and the quit flush writes the current model
/// synchronously, superseding anything queued. The ordering and the cap
/// themselves are tested on the store (`store::tests`).
#[test]
fn the_debounced_tabs_write_leaves_the_ui_thread_with_a_cap() {
    let body = browser_source();
    assert!(body.contains("tabs::save_soon(&self.tabs);"), "the tab change queues a delayed save");
    assert!(body.contains("fn flush_tabs_save"), "a synchronous quit-time flush exists");
    assert!(body.contains("tabs::write(&self.tabs);"), "the quit flush writes the current model now");
    let tabs = source("browser/tabs.rs");
    assert!(tabs.contains("JsonStore::new(\"Browser/tabs.json\")"), "tabs.json has one writer");
}

/// Closing the last tab unregisters the agent tools, and discarding a
/// registered page would too — a closed tab stays dead through them.
#[test]
fn close_and_discard_unregister_the_agent_page() {
    let body = browser_source();
    assert!(
        body.contains("self.terminal_service.unregister_browser(key);"),
        "the last close unregisters the session's page"
    );
    assert!(
        body.contains("if !was_active {"),
        "a background close rebuilds and re-registers nothing"
    );
    assert!(
        body.contains("self.terminal_service.unregister_browser(&key);\n        }"),
        "a discarded registered page unregisters too"
    );
    let service = source("terminal/service.rs");
    assert!(
        service.contains("pub fn unregister_browser(&self, session: &str)"),
        "the terminal service can forget a session's webview"
    );
}

/// A closed session tab never resurrects: the last close clears the stored
/// `browser_url`, and the migration runs only once per session.
#[test]
fn a_closed_last_tab_never_resurrects() {
    let body = browser_source();
    assert!(
        body.contains("self.clear_session_browser_url(&session, cx);"),
        "the last close forgets the session's stored browser_url"
    );
    let tabs = source("browser/tabs.rs");
    assert!(
        tabs.contains("pub migrated: Vec<String>"),
        "migrated session ids persist in tabs.json"
    );
    assert!(
        tabs.contains("tabs.next_id = tabs.next_id.max(max + 1);"),
        "load repairs next_id past every stored tab"
    );
}

/// A visible page never dirties `tabs.json` on its own: the shown stamp
/// stays in memory and rides along with the next real change (or the quit
/// flush), so an idle visible page writes about never instead of about
/// once per second forever.
#[test]
fn a_visible_page_never_dirties_tabs_json() {
    let tabs = source("browser/tabs.rs");
    assert!(
        tabs.contains("pub(crate) fn mark_shown(&mut self, id: TabId, now: u64) {"),
        "the stamp carries no dirty bit out"
    );
    assert!(
        !tabs.contains("saturating_sub(last) >= 1_000"),
        "no re-stamp throttle: a stamp costs no write"
    );
    let body = browser_source();
    assert!(
        !body.contains("mark_shown(id, tabs::now_ms()) {"),
        "the frame's occlusion schedules no write for a merely visible page"
    );
}

/// Closing from another space reads `was_active` off the tab's OWN space
/// (like `Tabs::close` does), never the current project's — and window
/// activation is no cover at all: the page is a child window of ours, so a
/// menu-bar menu or the DevTools window taking the keyboard left the page
/// blank (owner screenshots 2026-10-11).
#[test]
fn close_and_keyboard_follow_the_shown_tab_not_the_project() {
    let body = browser_source();
    assert!(
        body.contains("active_of(&tab.owner, tab.space.as_deref())"),
        "was_active comes from the tab's own owner+space"
    );
    assert!(!body.contains("pub window_active: bool"), "window activation is not a visibility input");
    assert!(!body.contains("shown_window_active("), "no window-active rule remains to hide the page");
}

/// Captures never touch the owner's tabs: the store is skipped in both
/// directions under `BAAZ_DETERMINISTIC=1`, like `layout::read`.
#[test]
fn deterministic_captures_never_touch_tabs_json() {
    let tabs = source("browser/tabs.rs");
    assert!(
        tabs.contains("if deterministic() {\n        return Tabs::default();"),
        "deterministic loads read nothing"
    );
    assert!(
        tabs.contains("if deterministic() {\n        return None;"),
        "deterministic saves write nothing"
    );
}
