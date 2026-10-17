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

/// The suggestions expose their text as list-box options, and the pane's
/// Open in Browse is wired (the toolbar's own buttons moved to the
/// sidebar's header in the library, 2026-10-13).
#[test]
fn toolbar_roles_and_labels() {
    let app = source("app.rs");
    assert!(app.contains("Address suggestions"), "the suggestions name themselves");
    assert!(app.contains("gpui::Role::ListBoxOption"), "suggestions are list-box options");
    // D6: the pane's 'Browse full width' button became the nav row's
    // labelled 'Open in Browse' (agentic-ui page.rs), routed here.
    let browser = source("browser/mod.rs");
    assert!(
        browser.contains("WebviewIntent::OpenInBrowse => self.carry_session_tab_to_browse(window, cx)"),
        "the page's Open in Browse carries the session's page into Browse"
    );
}

/// The covering rule includes the omnibox: the visibility struct carries
/// the input, the per-frame sync feeds it from the open dropdown, and the
/// unit tests pin both halves obscured.
#[test]
fn omnibox_covering_is_pinned_in_source() {
    let browser = source("browser/mod.rs");
    assert!(browser.contains("omnibox_open: bool"), "visibility carries the omnibox input");
    assert!(browser.contains("omnibox_open: self.browse_omnibox_open(cx)"), "the sync feeds the input");
    assert!(
        browser.contains("fn open_omnibox_obscures_pane_and_centre()"),
        "a unit test pins the omnibox covering both halves"
    );
}

/// The page covers only while the dropdown renders rows — merely editing
/// with no rows obscures nothing — and leaving Browse (or switching
/// project) leaves the field and resets the page-holds edge.
#[test]
fn omnibox_covers_only_while_rows_render() {
    let browser = source("browser/mod.rs");
    let open = fn_body(&browser, "browse_omnibox_open");
    assert!(open.contains("browse_suggestions"), "open means rows actually rank");
    let app = source("app.rs");
    let switch = fn_body(&app, "set_mode");
    assert!(switch.contains("centre_override = None"), "a mode switch unparks the carry");
    assert!(switch.contains("leave_browse_address(window, cx)"), "a mode switch leaves the field");
    assert!(switch.contains("browse_last_holds = false"), "a mode switch resets the holds edge");
}


/// The Browse centre draws no library nav row: the pane leaves it out
/// with `without_nav`, so only the page shows under Browse's own toolbar
/// — no shift, no clip.
#[test]
fn browse_centre_hides_the_library_nav_row() {
    let app = source("app.rs");
    let centre = fn_body(&app, "render_browse_centre");
    assert!(centre.contains(".without_nav()"), "the centre pane leaves the nav row out");
    assert!(!centre.contains("top(px(-38.0))"), "no shift hack for the nav row");
}


/// The new-tab field carries the placeholder, and each field's dropdown
/// anchors under it: the toolbar's under the toolbar, the new-tab's under
/// the centred field.
#[test]
fn newtab_placeholder_and_dropdown_anchors() {
    let browser = source("browser/mod.rs");
    let focus = fn_body(&browser, "focus_browse_address_with");
    assert!(
        focus.contains("set_placeholder(\"Search, open a page, or ask\""),
        "the field carries the placeholder"
    );
    let app = source("app.rs");
    assert!(app.contains("OmniAnchor::Toolbar"), "the toolbar list anchors under the toolbar");
    assert!(app.contains("OmniAnchor::Newtab"), "the new-tab list anchors under the centred field");
}




/// Suggestions stay in this project and out of the hidden list, and closed
/// pick-up rows are this project's person tabs that reopen the tab they
/// name. Rows press on mouse-down so the click lands before blur.
#[test]
fn suggestions_pickup_and_row_presses() {
    let browser = source("browser/mod.rs");
    let suggestions = fn_body(&browser, "browse_suggestions");
    assert!(suggestions.contains("!entry.hidden"), "hidden sessions never suggest");
    assert!(suggestions.contains("entry.project == current"), "suggestions stay in this project");
    let pickup = fn_body(&browser, "browse_pickup_full");
    assert!(pickup.contains("tab.owner == Owner::Person"), "closed rows are person tabs");
    assert!(pickup.contains("PickupTarget::Closed(*id)"), "each closed row reopens its own tab");
    let app = source("app.rs");
    assert!(app.contains("reopen_browse_closed_tab"), "closed rows reopen on press");
    assert!(app.contains("\"browse-pickup-closed\""), "closed rows name their own chain");
    assert!(
        app.contains("on_mouse_down(gpui::MouseButton::Left, press)"),
        "omnibox rows press on mouse-down"
    );
    assert!(app.contains(".on_click(activate)"), "omnibox rows also answer clicks");
    let leave = fn_body(&browser, "leave_browse_address");
    assert!(leave.contains("browse_address_page_focus"), "leaving prefers the page when it had focus");
    assert!(leave.contains("focus_root"), "leaving falls back to the root, never a dead handle");
}

/// Ask drafts into the session that lands: `browse_ask` arms the question
/// and starts the session without touching `active`; `new_session_in`
/// keys it by the created (or reused) session id and drops it on every
/// early return; `activate` writes it into the keyed view only (appending
/// over reused text) — never sent, never navigated. ⌘↵ rides the input's
/// own binding, so the commit never also fires.
#[test]
fn ask_draft_lands_on_arrival_never_sent() {
    let browser = source("browser/mod.rs");
    let ask = fn_body(&browser, "browse_ask");
    assert!(ask.contains("PendingAsk { target: None, query }"), "Ask arms the unkeyed question");
    assert!(!ask.contains("self.active"), "Ask never touches whatever is active right now");
    assert!(!ask.contains("set_draft"), "Ask never writes a draft immediately");
    for banned in ["submit", ".send(", "navigate", "start_turn"] {
        assert!(!ask.contains(banned), "Ask never sends and never navigates ({banned})");
    }
    let focus = fn_body(&browser, "focus_browse_address_with");
    assert!(
        focus.contains("PressEnter { secondary: true"),
        "⌘↵ arrives through the input context"
    );
    assert!(focus.contains("ask_from_browse_address"), "⌘↵ asks instead of committing");
    let app = source("app.rs");
    let input = fn_body(&app, "browse_address_input");
    assert!(!input.contains("platform"), "no parent ⌘↵ arm doubles the Ask");
    let lifecycle = source("app/lifecycle.rs");
    let new = fn_body(&lifecycle, "new_session_in");
    assert!(new.contains("self.drop_pending_ask()"), "an early return drops the draft");
    assert!(
        lifecycle.contains("self.key_pending_ask("),
        "every open path keys the draft to its session"
    );
    let activate = fn_body(&lifecycle, "activate");
    assert!(
        activate.contains("ask.target.as_deref() == Some(session_id.as_str())"),
        "only the keyed session's landing takes the question"
    );
    assert!(activate.contains("ask_draft_text"), "reused text appends on a new line");
    assert!(activate.contains("view.set_draft(next, window, cx)"), "the draft lands, never sent");
}

/// The carried tab unparks everywhere it must: sidebar and new-tab
/// selection, pick-up and omnibox rows, project switch, and every mode
/// switch. An omnibox session-tab row opens its session (updating the
/// sidebar selection there) instead of carrying.
#[test]
fn centre_override_clears_everywhere() {
    let browser = source("browser/mod.rs");
    for name in ["select_browse_sidebar_tab", "new_browse_sidebar_tab"] {
        assert!(
            fn_body(&browser, name).contains("centre_override = None"),
            "{name} unparks the carry"
        );
    }
    let row = fn_body(&browser, "activate_omnibox_target");
    assert!(row.contains("select_browse_sidebar_tab"), "a person-tab row selects (and unparks)");
    assert!(row.contains("self.resume(session"), "a session-tab row opens its session");
    let lifecycle = source("app/lifecycle.rs");
    assert!(
        fn_body(&lifecycle, "new_session_in").contains("centre_override = None"),
        "a new session unparks even without a mode switch"
    );
    let projects = source("project_menu.rs");
    assert!(
        projects.contains("note_project_changed"),
        "project adoptions unpark through the shared hook"
    );
}
