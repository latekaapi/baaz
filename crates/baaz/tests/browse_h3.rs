//! B2a-H3: the wiring contracts the unit tests cannot see.
//!
//! Like `modes_h2.rs`, this suite cannot boot the app (baaz is a
//! binary), so it pins the wiring in source text: the Browse tab
//! actions and their root handlers, the `BaazBrowse` context the window
//! wears in Browse mode, the `tab-pin:`, `tab-menu:` and
//! `from-sessions:` step verbs with their doc rows, the two probe
//! entries, and the tab menu's rows (with the later rows absent). Each
//! assertion fails on the pre-H3 code.

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

/// Every Browse tab action is declared and handled at the window root,
/// so it fires while a native page holds the keyboard (the claimed set
/// carries the chord there).
#[test]
fn browse_tab_actions_are_declared_and_handled() {
    let app = source("app.rs");
    for action in [
        "CloseBrowseTab",
        "ReopenBrowseTab",
        "BrowseTab1",
        "BrowseTab2",
        "BrowseTab3",
        "BrowseTab4",
        "BrowseTab5",
        "BrowseTab6",
        "BrowseTab7",
        "BrowseTab8",
        "BrowseTabLast",
        "MoveBrowseTabDown",
        "MoveBrowseTabUp",
    ] {
        assert!(app.contains(&format!("{action},")), "the {action} action is declared");
        assert!(
            app.contains(&format!("_: &{action},")),
            "the window root handles {action}"
        );
    }
}

/// The `BaazBrowse` context exists and wraps the shell in Browse mode
/// only — Sessions never wears it, so ⌘W keeps closing the window
/// there and ⌘N is untouched in both modes.
#[test]
fn browse_context_wraps_the_shell_in_browse_only() {
    let app = source("app.rs");
    assert!(
        app.contains("pub(crate) const BROWSE_CONTEXT: &str = \"BaazBrowse\";"),
        "the context is declared"
    );
    assert!(
        app.contains(".key_context(BROWSE_CONTEXT)"),
        "a wrapper wears it"
    );
    assert!(
        app.contains("if self.mode == layout::Mode::Browse {"),
        "the wrapper mounts in Browse mode"
    );
    assert!(
        app.contains("this.new_browser_tab(window, cx);"),
        "⌘T still routes through the new-tab handler"
    );
    assert!(
        app.contains("if self.mode != layout::Mode::Browse {"),
        "⌘T switches to Browse first from Sessions"
    );
}

/// B2a-H3b item 1: `BaazBrowse` sits on an ancestor of the focus-root
/// div (which owns `track_focus` and `AuiRoot`), never on a child of it
/// — focus resting on the root carries both contexts, so ⌘W closes a
/// tab in Browse and the window in Sessions.
#[test]
fn browse_context_sits_above_the_focus_root() {
    let app = source("app.rs");
    assert!(
        app.contains(".key_context(BROWSE_CONTEXT).child(root)"),
        "the context wrapper is an ancestor of the focus-root div"
    );
    assert!(
        !app.contains(".key_context(BROWSE_CONTEXT).child(body)"),
        "no child wrapper sits below the focused node"
    );
    assert!(
        app.contains(".track_focus(&self.focus_root)"),
        "the root still owns the focus"
    );
}

/// `tab-pin:`, `tab-menu:` and `from-sessions:` are window verbs and
/// are documented — the doc test enforces the table, this pins the
/// registration the captures rely on.
#[test]
fn browse_steps_are_window_verbs_and_documented() {
    let steps = source("steps.rs");
    for verb in ["tab-pin", "tab-menu", "from-sessions"] {
        assert!(
            steps.contains(&format!("verb: \"{verb}\"")),
            "{verb}: is a window verb"
        );
        assert!(
            steps.contains(&format!("`{verb}:<")),
            "{verb}: is documented"
        );
    }
}

/// The two probe entries with their seed steps, and no regenerated
/// baselines hiding behind them.
#[test]
fn probe_entries_cover_the_browse_sidebar_and_its_menu() {
    let probe = repo_file("scripts/uiprobe.py");
    assert!(probe.contains("\"browse-sidebar\":"), "uiprobe has a browse-sidebar entry");
    assert!(probe.contains("\"browse-tab-menu\":"), "uiprobe has a browse-tab-menu entry");
    assert!(
        probe.contains("mode:browse;tab-new:https://example.com/;tab-new:https://rust-lang.org/;tab-pin:2;from-sessions:1"),
        "browse-sidebar seeds person tabs, a pin and an expanded session group"
    );
    assert!(
        probe.contains("tab-menu:2"),
        "browse-tab-menu opens the menu"
    );
}

/// The tab menu carries exactly the H3 rows — Reload, Duplicate,
/// Pin/Unpin, Copy link, Move to project, Close tab (⌘W), Close other
/// tabs — and none of the later rows.
#[test]
fn tab_menu_carries_the_h3_rows_and_no_later_ones() {
    let sidebar = source("sidebar_view.rs");
    for row in ["\"Reload\"", "\"Duplicate\"", "\"Copy link\"", "\"Move to project\"", "\"Close tab\"", "\"Close other tabs\""] {
        assert!(sidebar.contains(row), "the menu has a {row} row");
    }
    assert!(
        sidebar.contains("\"Pin tab\"") || sidebar.contains("pin_label"),
        "the menu pins and unpins"
    );
    for later in ["Send to session", "Open beside a session", "Mute site"] {
        assert!(!sidebar.contains(later), "the menu has no {later} row yet");
    }
}

/// The sidebar's interactive elements name themselves: tab rows are
/// Tabs, the menu is a Menu of MenuItems, groups are Groups, and the
/// close buttons say what they close.
#[test]
fn browse_rows_carry_ids_roles_and_labels() {
    let sidebar = source("sidebar_view.rs");
    assert!(sidebar.contains("\"browse-new-tab\""), "New tab has an id");
    assert!(sidebar.contains("\"browse-pinned\""), "pinned tiles have ids");
    assert!(sidebar.contains("\"browse-tab\""), "tab rows have ids");
    assert!(sidebar.contains("\"browse-agent\""), "agent rows have ids");
    assert!(sidebar.contains("gpui::Role::MenuItem"), "menu rows are menu items");
    assert!(sidebar.contains("gpui::Role::Group"), "session groups are groups");
    assert!(
        sidebar.contains("format!(\"Close {title}\")"),
        "the × names the tab it closes"
    );
    assert!(
        sidebar.contains("format!(\"Pinned: {title}\")"),
        "pinned tiles name themselves"
    );
}
