//! B2a-H2: the wiring contracts the unit tests cannot see.
//!
//! Like `tabs_h1b.rs`, this suite cannot boot the app (baaz is a
//! binary), so it pins the wiring in source text: the `ToggleBrowseMode`
//! action and its ⌥⌘B row, the claimed-key path that carries it while a
//! page holds the keyboard, the `mode:` step verb and its doc row, the
//! three probe entries, the switch row and slide in the sidebar, and the
//! Browse centre visibility. Each assertion fails on the pre-H2 code.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

fn repo_file(name: &str) -> String {
    let path = manifest().join("../../").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The action exists, is handled at the window root (so it fires while a
/// native page holds the keyboard), and sits in the View menu with ⌥⌘B
/// reading off the keymap.
#[test]
fn toggle_browse_mode_is_an_action_handled_and_menued() {
    let app = source("app.rs");
    assert!(app.contains("ToggleBrowseMode,"), "the action is declared");
    assert!(
        app.contains("this.toggle_browse_mode(window, cx)"),
        "the window root handles it"
    );
    assert!(
        app.contains("gpui::MenuItem::action(\"Browse\", ToggleBrowseMode)"),
        "the View menu carries it"
    );
}

/// The keymap row binds ⌥⌘B at the root in the pane category with the
/// approved label — and the claimed-key path picks up exactly such rows,
/// which is what lets it fire with a page focused.
#[test]
fn browse_toggle_row_is_bound_claimed_and_categorised() {
    let keymap = source("keymap.rs");
    assert!(
        keymap.contains(
            "KeymapEntry { action: \"ToggleBrowseMode\", keystroke: \"cmd-alt-b\", \
             context: Some(aui::keys::ROOT_CONTEXT), category: \"pane\", \
             label: \"Switch between Sessions and Browse\" }"
        ),
        "one row binds ⌥⌘B at the root"
    );
    assert!(
        keymap.contains("\"ToggleBrowseMode\" => Some(gpui::KeyBinding::new("),
        "the row builds a real binding"
    );
    assert!(
        keymap.contains("predicate.to_string() == aui::keys::ROOT_CONTEXT"),
        "the claimed set picks up AuiRoot rows automatically"
    );
}

/// `mode:browse` / `mode:sessions` set idempotently (never toggle), are
/// window verbs, and are documented — the doc test enforces the table,
/// this pins the acceptance the captures rely on.
#[test]
fn mode_steps_set_idempotently_and_are_window_steps() {
    let steps = source("steps.rs");
    assert!(
        steps.contains("WindowVerb { verb: \"mode\", run: |this, rest, window, cx| this.step_mode(rest, window, cx) }"),
        "mode: is a window verb"
    );
    assert!(
        steps.contains("Some(mode) => self.set_mode(mode, window, cx),"),
        "the verb sets the parsed mode"
    );
    let app = source("app.rs");
    assert!(
        app.contains("if self.mode == mode {"),
        "asking for the current mode sets nothing and never toggles"
    );
    assert!(
        app.contains("pub(crate) fn toggle_browse_mode("),
        "⌥⌘B toggles through the same setter"
    );
}

/// The three probe entries with their exact steps.
#[test]
fn probe_entries_cover_both_modes() {
    let probe = repo_file("scripts/uiprobe.py");
    for entry in ["browse-empty", "browse-tabs", "sessions-switch"] {
        assert!(probe.contains(&format!("\"{entry}\":"),), "uiprobe has a {entry} entry");
    }
    assert!(
        probe.contains("\"--steps\", \"mode:browse\""),
        "browse-empty enters Browse"
    );
    assert!(
        probe.contains("\"--steps\", \"mode:browse;tab-new:https://www.wikipedia.org/;tab-new:https://docs.rs/\""),
        "browse-tabs enters Browse over two person tabs"
    );
}

/// The switch row sits at the top of the sidebar with counts, roles and
/// labels; the rail carries the labelled toggle; the body slides on the
/// Layout spring inside a clip; the Browse list is marked for B2a-H3.
#[test]
fn sidebar_carries_the_switch_and_the_slide() {
    let sidebar = source("sidebar_view.rs");
    assert!(sidebar.contains(".child(self.render_mode_switch(cx))"), "the switch tops the column");
    assert!(sidebar.contains("\"mode-switch\""), "the switch has an id");
    assert!(sidebar.contains("gpui::Role::TabList"), "the switch is a tab list");
    assert!(sidebar.contains("gpui::Role::Tab"), "each segment is a tab");
    assert!(sidebar.contains("mode.label(), count"), "each segment names its count");
    assert!(sidebar.contains("aria_selected(selected)"), "segments carry selected state");
    assert!(
        sidebar.contains("RailItem::nav(\"Browse\", IconName::Globe)"),
        "the rail toggles to Browse"
    );
    assert!(
        sidebar.contains("RailItem::nav(\"Sessions\", IconName::Globe)"),
        "the rail toggles back to Sessions"
    );
    assert!(
        sidebar.contains("SpringKind::Layout, window, cx"),
        "the slide rides the Layout spring"
    );
    assert!(
        sidebar.contains("overflow_hidden()"),
        "the slide is clipped to the column"
    );
    assert!(
        sidebar.contains("B2a-H3 replaces this with the real Browse sidebar"),
        "the placeholder list is marked for B2a-H3"
    );
}

/// The centre page is visible under the same covering rules in Browse,
/// with its own bounds; Sessions-mode visibility is unchanged.
#[test]
fn browse_centre_has_its_own_visibility_and_bounds() {
    let browser = source("browser/mod.rs");
    assert!(browser.contains("pub centre_browse: bool"), "the visibility input exists");
    assert!(
        browser.contains("(inputs.pane_open && inputs.kind_browser || inputs.centre_browse)"),
        "the centre shows without an open pane"
    );
    assert!(
        browser.contains("pub(crate) fn browser_centre_bounds("),
        "the centre page has its own rect"
    );
    assert!(
        browser.contains("let centre_shown = self.browse_centre_shown();"),
        "sync resolves the centre tab every frame"
    );
    assert!(
        browser.contains("self.apply_page_visibility(shown, frame_live, centre_shown, centre_live, cx);"),
        "both pages apply every frame"
    );
    let app = source("app.rs");
    assert!(
        app.contains("aui_webview::webview_pane(\"browse-centre-page\""),
        "the centre renders the tab's page"
    );
}

/// The mode and its per-mode memory persist in `layout.json`, defaulting
/// to sessions.
#[test]
fn mode_and_memory_persist_with_sessions_default() {
    let layout = source("layout.rs");
    assert!(layout.contains("#[serde(rename = \"mode\", default)]"), "the mode persists");
    assert!(
        layout.contains("#[serde(rename = \"modeMemory\", default"),
        "the per-mode memory persists"
    );
    assert!(layout.contains("pub fn switch(&mut self, from: Mode,"), "switching saves then restores");
}
