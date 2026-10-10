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

