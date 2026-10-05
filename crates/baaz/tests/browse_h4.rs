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

/// The address field shows the shown tab's URL, falling back to the tab
/// model when the page has no live state (a discarded tab) — never empty
/// on a loaded page.
#[test]
fn address_shows_the_page_with_tab_fallback() {
    let app = source("app.rs");
    let toolbar = fn_body(&app, "render_browse_toolbar");
    assert!(toolbar.contains("tab.as_ref().map(|tab| tab.url.clone())"), "the tab URL is the fallback");
    assert!(toolbar.contains("state.read(cx).loading()"), "the stop button reads the engine loading state");
    assert!(!toolbar.contains("loading_tabs"), "no Baaz-tracked load set feeds the button");
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

/// The extensions button is a monochrome toolbar icon (the sprite has no
/// puzzle or grid glyph), and the address reads in the UI font — only the
/// dev label stays mono.
#[test]
fn toolbar_icons_and_fonts() {
    let app = source("app.rs");
    assert!(!app.contains("🧩"), "no coloured emoji on the toolbar");
    assert!(app.contains("IconName::Layout"), "the extensions button is a monochrome icon");
    let field = fn_body(&app, "browse_address_field");
    assert!(field.contains(".ui(scale::FS_12)"), "the address reads in the UI font");
    assert!(field.contains("mono(scale::FS_12)"), "only the dev label stays mono");
    assert!(field.contains("\"Site information\""), "the site button is labelled");
    assert!(field.contains("\"hd-browse-site\""), "the site button names its own chain");
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

/// The back-history menu never opens while back is disabled; an empty
/// stack draws nothing (the render never mutates state — the opener
/// decides); picking a row steps back through the history so the stacks
/// stay right.
#[test]
fn back_history_menu_guards_clears_and_steps() {
    let app = source("app.rs");
    let toolbar = fn_body(&app, "render_browse_toolbar");
    assert!(toolbar.contains("history.can_back()"), "the opener checks back first");
    assert!(toolbar.contains("open_menu(MenuKind::BackHistory, cx)"), "the opener opens the menu");
    let dialogs = source("dialogs.rs");
    let menu = fn_body(&dialogs, "render_back_history_menu");
    assert!(menu.contains("overlays.menu = None"), "an empty render clears the menu");
    assert!(menu.contains("go_back_steps(steps)"), "picking steps back through the history");
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
    assert!(
        app.contains("on_mouse_down(gpui::MouseButton::Left, press)"),
        "the zoom pill presses on mouse-down"
    );
    assert!(app.contains(".on_click(reset)"), "the zoom pill also answers clicks");
    let leave = fn_body(&browser, "leave_browse_address");
    assert!(leave.contains("browse_address_page_focus"), "leaving prefers the page when it had focus");
    assert!(leave.contains("focus_root"), "leaving falls back to the root, never a dead handle");
}

/// The stop/reload control follows the engine: `browse_reload_or_stop`
/// stops exactly while `WebviewState::loading()` reports a load and
/// reloads otherwise — no Baaz-tracked set anywhere, in the registry or
/// the sync.
#[test]
fn loading_tracks_until_done_or_stop() {
    let browser = source("browser/mod.rs");
    assert!(!browser.contains("loading_tabs"), "no Baaz-tracked load set survives");
    assert!(
        fn_body(&browser, "browse_reload_or_stop").contains("state.read(cx).loading()"),
        "the control reads the engine loading state"
    );
}
