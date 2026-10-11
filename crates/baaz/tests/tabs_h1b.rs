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

/// A pending tabs write lands synchronously in BOTH quit paths — the menu's
/// `QuitApp` and the `on_app_quit` hook for Dock/signal quits — next to the
/// layout/sessions flushes.
#[test]
fn quit_flushes_a_pending_tabs_write_in_both_quit_paths() {
    for name in ["app.rs", "main.rs"] {
        let body = source(name);
        assert!(
            body.contains("this.flush_tabs_save()"),
            "{name} flushes tabs at quit next to the sessions flush"
        );
    }
}

/// The debounced write leaves the UI thread (like the sessions write) and
/// arms only one window per dirty spell, so a constantly-changing title
/// cannot postpone the write forever. Every snapshot carries a monotonic
/// generation and the writer drops stale ones, so two saves finishing out
/// of order can never let the older state win — and the quit flush lands
/// the current model synchronously under a newer generation, never a
/// no-op while a save is in flight.
#[test]
fn the_debounced_tabs_write_leaves_the_ui_thread_with_a_cap() {
    let body = source("browser/mod.rs");
    assert!(
        body.contains(
            "cx.background_executor()\n                .spawn(async move {\n                    tabs::write_tabs_snapshot_generation(generation, &snapshot);"
        ),
        "the tabs snapshot saves off the UI thread, generation-ordered"
    );
    assert!(
        body.contains("if self.browser.tabs_save_pending {\n            return;"),
        "one armed window per dirty spell: the max-delay cap"
    );
    assert!(
        body.contains("fn flush_tabs_save"),
        "a synchronous quit-time flush exists"
    );
    assert!(
        body.contains("write_tabs_snapshot_generation(generation, &self.browser.tabs);"),
        "the quit flush writes the current model synchronously under a newer generation"
    );
    assert!(
        !body.contains("if !self.browser.tabs_save_pending {\n            return;"),
        "the quit flush is never a no-op while a save is in flight"
    );
    let tabs = source("browser/tabs.rs");
    assert!(
        tabs.contains("static TABS_LAST_WRITTEN: Mutex<u64>"),
        "one lock serialises racing snapshots"
    );
    assert!(
        tabs.contains("if generation <= *last {\n        return false;"),
        "a snapshot older than the last written is skipped"
    );
}

/// Closing the last tab unregisters the agent tools, and discarding a
/// registered page would too — a closed tab stays dead through them.
#[test]
fn close_and_discard_unregister_the_agent_page() {
    let body = source("browser/mod.rs");
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
    let body = source("browser/mod.rs");
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

/// The strip's × never also selects, middle-click closes only when the
/// press and the release land on the same tab, and both aria labels read
/// the same visible (ellipsized) text the row shows.
#[test]
fn the_strip_close_never_selects_and_labels_what_it_shows() {
    let body = source("right.rs");
    assert!(
        body.contains(".on_mouse_down(gpui::MouseButton::Middle,"),
        "the middle press records its tab"
    );
    assert!(
        body.contains(".on_mouse_up(gpui::MouseButton::Middle,"),
        "middle-click closes on mouse up over the tab"
    );
    assert!(
        body.contains("middle_press_action(&press, id_tab, cx);"),
        "the middle press records its tab on the harness"
    );
    assert!(
        body.contains("middle_release_action(&middle, id_tab, window, cx);"),
        "the release asks the harness, which closes only its own press"
    );
    let browser = source("browser/mod.rs");
    assert!(
        browser.contains("middle_release_closes(pressed, id)"),
        "the release closes only its own press"
    );
    assert!(
        body.contains("cx.stop_propagation();\n                    close_tab_action(&close, id_tab, window, cx);"),
        "the × stops propagation so it never also selects"
    );
    assert!(
        body.contains(".aria_label(format!(\"{} tab\", title))"),
        "the tab aria label uses the row's visible ellipsized text"
    );
    assert!(
        body.contains("format!(\"Close {}\", title)"),
        "the close aria label uses the row's visible ellipsized text"
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
    let body = source("browser/mod.rs");
    assert!(
        !body.contains("mark_shown(id, tabs::now_ms()) {"),
        "the per-frame sync schedules no write for a merely visible page"
    );
}

/// Closing from another space reads `was_active` off the tab's OWN space
/// (like `Tabs::close` does), never the current project's — and window
/// activation is no cover at all: the page is a child window of ours, so a
/// menu-bar menu or the DevTools window taking the keyboard left the page
/// blank (owner screenshots 2026-10-11).
#[test]
fn close_and_keyboard_follow_the_shown_tab_not_the_project() {
    let body = source("browser/mod.rs");
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
        tabs.contains("if deterministic() {\n        return false;"),
        "deterministic saves write nothing"
    );
}
