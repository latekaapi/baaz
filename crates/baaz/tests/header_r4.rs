//! R4: the header project picker is gone everywhere it rendered.
//!
//! baaz is a binary, so like `browse_h4.rs` this suite pins the wiring in
//! source text: neither centre header renders an `hd-project` element or an
//! `Open project menu` label, while the project menu itself stays reachable
//! from the sidebar group trays, the Skills header and the scripted steps,
//! the palette's `Add or switch project` command still opens the Projects
//! palette, and the empty-state hero still names the project.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
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

/// Neither centre header renders the picker: no `hd-project` element, no
/// `Open project menu` label, no crumb builder and no crumb trigger rect.
#[test]
fn no_project_picker_in_either_centre_header() {
    let app = source("app.rs");
    for banned in ["hd-project", "Open project menu", "project_crumb", "crumb_bounds"] {
        assert!(!app.contains(banned), "app.rs carries no {banned}");
    }
    for site in ["render_browse_toolbar", "render_browse_minimal_toolbar", "render_centre_header"] {
        let body = fn_body(&app, site);
        for banned in ["hd-project", "Open project menu", "project_crumb", "crumb_bounds"] {
            assert!(!body.contains(banned), "{site} renders no picker ({banned})");
        }
    }
}

/// The session title leads the Sessions header, and a project rename from
/// the menu still has its dense field there.
#[test]
fn the_session_title_leads_the_centre_header() {
    let app = source("app.rs");
    let header = fn_body(&app, "render_centre_header");
    assert!(header.contains("The session title leads the row"), "the title leads the row");
    assert!(header.contains("renaming_project_here"), "a project rename still swaps the lead");
    assert!(header.contains("self.rename_field(window, cx)"), "the rename field still renders");
    assert!(header.contains(".truncate().child(label)"), "the session label still renders");
}

/// The Browse toolbar keeps its chip, nav and address with no crumb ahead
/// of them.
#[test]
fn the_browse_toolbar_opens_with_no_crumb() {
    let app = source("app.rs");
    let toolbar = fn_body(&app, "render_browse_toolbar");
    assert!(toolbar.contains("header_cell(\"hd-browse\")"), "the toolbar cell stays");
    assert!(toolbar.contains("browse_address_field"), "the address field stays");
    assert!(toolbar.contains("browse_back_button"), "the nav buttons stay");
}

/// The menu itself survives: still defined, seated at the group row, and
/// still opened from the sidebar trays, the Skills header and the
/// `project-menu` step.
#[test]
fn project_menu_openers_survive_without_the_crumb() {
    let menu = source("project_menu.rs");
    assert!(menu.contains("pub(crate) fn open_project_menu("), "open_project_menu stays");
    assert!(menu.contains("self.group_menu_bounds"), "the menu seats at the group row");
    assert!(!menu.contains("crumb_bounds"), "no crumb seat remains");
    let sidebar = source("sidebar_view.rs");
    assert!(
        sidebar.contains("self.open_project_menu(None, false, cx)"),
        "the unfiled tray still opens the menu"
    );
    assert!(
        sidebar.contains("self.open_project_menu(Some(id.to_string()), false, cx)"),
        "a group tray still opens its menu"
    );
    let skills = source("skills_page.rs");
    assert!(
        skills.contains("this.open_project_menu(this.current_project.clone(), true, cx)"),
        "the Skills header still opens the menu"
    );
    let steps = source("steps.rs");
    assert!(steps.contains("this.step_project_menu(rest, cx)"), "the project-menu step still opens it");
}

/// Switching projects still goes through the palette: the
/// `Add or switch project` command opens the Projects palette.
#[test]
fn switch_project_palette_route_stays() {
    let overlays = source("overlays.rs");
    assert!(
        overlays.contains("Command::Project => \"Add or switch project\""),
        "the palette command keeps its name"
    );
    let composer = source("session/composer.rs");
    assert!(
        composer.contains("Command::Project => cx.emit(SessionEvent::Projects)"),
        "the command still emits its event"
    );
    let lifecycle = source("app/lifecycle.rs");
    assert!(lifecycle.contains("SessionEvent::Projects"), "the event is still handled");
    assert!(
        lifecycle.contains("this.open_projects(false, window, cx)"),
        "the event still opens the Projects palette"
    );
    let dialogs = source("dialogs.rs");
    assert!(dialogs.contains("fn open_projects("), "the Projects palette stays");
}

/// The empty-state hero still shows the project name with its
/// `New session` button.
#[test]
fn empty_state_hero_still_names_the_project() {
    let app = source("app.rs");
    let hero = fn_body(&app, "render_no_session");
    assert!(hero.contains("self.workspace_name()"), "the hero still names the project");
    assert!(hero.contains("button(\"new-session\", \"New session\")"), "the hero button stays");
}
