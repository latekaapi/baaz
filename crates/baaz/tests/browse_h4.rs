//! B2a-H4: the wiring contracts the unit tests cannot see.
//!
//! Like `modes_h2.rs`, this suite cannot boot the app (baaz is a binary),
//! so it pins the wiring in source text. The pure decisions — address
//! states, Enter-to-URL-or-search, omnibox ranking, pick-up rows,
//! back/forward stacks, the omnibox covering rule — are pinned by unit
//! tests in `browser/toolbar.rs` and `browser/mod.rs`; this file pins the
//! hand-over the gate cannot run: the ↩ chip round trip moves the same
//! tab (no reload, the same `WebviewState` entity renders in the centre),
//! ⌘T in Browse opens a blank tab with the field focused, the five probe
//! entries and their steps, and the toolbar's roles and labels.

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

/// The body of `fn name` inside `text`: from its declaration to the next
/// closing brace at the same 4-space impl indent.
fn fn_body<'a>(text: &'a str, name: &str) -> &'a str {
    let marker = format!("fn {name}(");
    let start = text.find(&marker).unwrap_or_else(|| panic!("{name} exists"));
    let rest = &text[start..];
    let end = rest.find("\n    }\n").unwrap_or_else(|| panic!("{name} ends"));
    &rest[..end]
}

/// The ↩ chip round trip keeps the same entity and owner: carrying parks
/// the session tab's id in `centre_override` without touching any page
/// state, and the chip takes it back — no reload, no rebuild, so the same
/// `WebviewState` entity the pane showed renders in the centre and back.
#[test]
fn chip_round_trip_keeps_the_same_entity_and_owner() {
    let browser = source("browser/mod.rs");
    let carry = fn_body(&browser, "carry_session_tab_to_browse");
    assert!(carry.contains("centre_override = Some(id)"), "carrying parks the session tab's id");
    assert!(carry.contains("active_of"), "carrying parks the session's active tab");
    for banned in ["discard", "navigate", "reload", "WebviewState::new", "begin_editing"] {
        assert!(!carry.contains(banned), "carrying touches no page state ({banned})");
    }
    let back = fn_body(&browser, "browse_chip_back");
    assert!(back.contains("centre_override.take()"), "the chip takes the parked id back");
    assert!(back.contains("Owner::Session"), "the chip only answers for session-owned tabs");
    assert!(back.contains("self.resume(session"), "the chip hands the tab to its session's pane");
    for banned in ["discard", "navigate", "reload", "WebviewState::new"] {
        assert!(!back.contains(banned), "the chip never reloads ({banned})");
    }
    // The centre resolves the parked id to the live state by id — the same
    // entity the pane showed, never a rebuild.
    let shown = fn_body(&browser, "browse_centre_shown");
    assert!(shown.contains("centre_override"), "the centre shows the parked tab");
    let state = fn_body(&browser, "browse_centre_state");
    assert!(state.contains("browse_centre_shown"), "the centre state follows the shown tab");
    assert!(state.contains("states.get"), "the centre state is looked up, never built");
}

/// ⌘T in Browse lands on the new-tab page with the field focused: the
/// NewSession action opens a blank person tab there instead of starting a
/// chat session; Sessions mode is untouched.
#[test]
fn cmd_t_in_browse_opens_a_blank_tab_with_the_field_focused() {
    let app = source("app.rs");
    assert!(
        app.contains("if this.mode == layout::Mode::Browse {"),
        "NewSession branches in Browse"
    );
    assert!(app.contains("this.new_browse_sidebar_tab(window, cx);"), "⌘T in Browse opens a tab");
    let browser = source("browser/mod.rs");
    let new_tab = fn_body(&browser, "new_browse_sidebar_tab");
    assert!(new_tab.contains("Owner::Person"), "the new tab is a person tab");
    assert!(new_tab.contains("BLANK"), "the new tab opens blank");
    assert!(
        new_tab.contains("focus_browse_address_with(\"\", window, cx)"),
        "the new-tab field is focused"
    );
}

/// The five probe entries with their exact steps.
#[test]
fn probe_entries_cover_toolbar_omnibox_newtab_and_session_chip() {
    let probe = repo_file("scripts/uiprobe.py");
    for entry in ["browse-toolbar", "browse-toolbar-dev", "browse-omnibox", "browse-newtab", "browse-from-session"] {
        assert!(probe.contains(&format!("\"{entry}\":"),), "uiprobe has a {entry} entry");
    }
    assert!(
        probe.contains("\"--steps\", \"mode:browse;tab-new:https://example.com\""),
        "browse-toolbar shows a secure page"
    );
    assert!(
        probe.contains("\"--steps\", \"mode:browse;tab-new:http://localhost:5173/overview\""),
        "browse-toolbar-dev shows a local page"
    );
    assert!(
        probe.contains("omnibox:example"),
        "browse-omnibox focuses the address and types"
    );
    assert!(
        probe.contains("tab-new:about:blank"),
        "browse-newtab renders the Baaz page"
    );
    assert!(
        probe.contains("\"--steps\", \"fixture-session:Fix ledger chart\""),
        "browse-from-session carries a pane tab into Browse"
    );
}

/// `omnibox:<text>` and `fixture-session:<name>` are window verbs and are
/// documented — the doc test enforces the table, this pins the acceptance
/// the captures rely on.
#[test]
fn omnibox_and_fixture_steps_are_window_verbs_and_documented() {
    let steps = source("steps.rs");
    assert!(
        steps.contains("WindowVerb { verb: \"omnibox\", run: |this, rest, window, cx| this.step_omnibox(rest, window, cx) }"),
        "omnibox: is a window verb"
    );
    assert!(steps.contains("verb: \"fixture-session\""), "fixture-session: is a window verb");
    assert!(steps.contains("| `omnibox:<text>` |"), "omnibox: is documented");
    assert!(steps.contains("| `fixture-session:<name>` |"), "fixture-session: is documented");
}

/// Every toolbar button names itself, and the suggestions expose their text
/// as list-box options.
#[test]
fn toolbar_roles_and_labels() {
    let app = source("app.rs");
    for label in [
        "Back",
        "Forward",
        "Reload",
        "Stop loading",
        "Address and search",
        "Address suggestions",
    ] {
        assert!(app.contains(label), "the toolbar names {label}");
    }
    assert!(app.contains("Back to {name}"), "the ↩ chip names its session");
    assert!(app.contains("Zoom {pill}, reset"), "the zoom pill names its reset");
    assert!(app.contains("gpui::Role::ListBoxOption"), "suggestions are list-box options");
    let right = source("right.rs");
    assert!(right.contains("\"Browse full width\""), "the pane's Browse button is labelled");
}

/// The covering rule includes the omnibox: the visibility struct carries
/// the input, the per-frame sync feeds it from the open dropdown, and the
/// unit tests pin both halves obscured.
#[test]
fn omnibox_covering_is_pinned_in_source() {
    let browser = source("browser/mod.rs");
    assert!(browser.contains("omnibox_open: bool"), "visibility carries the omnibox input");
    assert!(browser.contains("omnibox_open: self.browse_omnibox_open()"), "the sync feeds the input");
    assert!(
        browser.contains("fn open_omnibox_obscures_pane_and_centre()"),
        "a unit test pins the omnibox covering both halves"
    );
}
