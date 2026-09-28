//! K3b: two Skills-page defects seen on screen, pinned.
//!
//! - The "Add skill ▾" menu floats in the popover layer: an in-flow menu
//!   pushed the meter, the scope bar and the filter row onto the list.
//! - The import preview draws one checkbox row per candidate: the dialog
//!   once built the library preview without any `.row()`, so no skill
//!   listed and there was nothing to toggle.
//! - The bundled detail preview reads `inspect --json`: offline captures
//!   have no CLI, so the scripted page reads the inspect fixture through
//!   the live parse instead of showing "No preview available".
//!
//! The look itself is probe-verified (`skills-page`, `skills-add-menu`,
//! `skills-import-preview`); what is mechanically pinnable lives here.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn read_fixture(name: &str) -> serde_json::Value {
    let path = manifest().join("tests/fixtures/skills").join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("fixture reads: {}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("fixture parses: {}", path.display()))
}

fn page_source() -> String {
    let path = manifest().join("src/skills_page.rs");
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The menu floats above the page: it renders through `popover_layer` and
/// never joins the page column, so opening it cannot move the meter, the
/// scope bar or the filter row.
#[test]
fn add_menu_floats_above_the_page() {
    let src = page_source();
    assert!(
        src.contains("popover_layer("),
        "the Add menu must render through `popover_layer`, not in flow"
    );
    assert!(
        !src.contains("column = column.child(self.render_add_menu(cx))"),
        "the Add menu must not join the page column: it floats, it does not push"
    );
}

/// The import preview wires one checkbox row per dry-run candidate: the
/// rows carry the New / Replaces yours / Already installed chip and the
/// tokens the summary counts.
#[test]
fn import_preview_wires_one_row_per_candidate() {
    let src = page_source();
    assert!(
        src.contains("preview = preview.row(row)"),
        "the import dialog must pass every candidate to the library preview with `.row()`"
    );
}

/// The inspect fixture is what the scripted page previews: its skill is
/// the page fixture's first (selected) row, and it carries the description
/// the live path falls back to when `inspect --json` has no body — the
/// same parse the capture reads.
#[test]
fn inspect_fixture_previews_the_selected_bundled_skill() {
    let page = read_fixture("page.json");
    let first = page
        .get("skills")
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .expect("page fixture has rows");
    assert_eq!(first.get("id").and_then(|v| v.as_str()), Some("bundled:browser-app-delivery"));
    let inspect = read_fixture("inspect-browser-app-delivery.json");
    let skill = inspect.get("skill").unwrap_or(&inspect);
    assert_eq!(
        skill.get("id").and_then(|v| v.as_str()),
        Some("bundled:browser-app-delivery"),
        "the inspect fixture must describe the row the capture selects"
    );
    let description = skill.get("description").and_then(|v| v.as_str()).unwrap_or("");
    assert!(!description.trim().is_empty(), "the live path previews the description when there is no body");
    assert!(
        page_source().contains("parse_inspect_preview"),
        "the scripted page must read the fixture through the live inspect parse"
    );
}

/// Y3a: the header never overlaps the list, dock open or not.
///
/// Two layout rules pin it, both readable off the page source. The page
/// root takes the centre column's REMAINDER (`flex_1` + `min_h(0)`, never
/// `size_full`, which stacked a full page plus the dock and pushed the
/// dock below the fold), and the body row stretches — never cross-centres
/// — the scroll box to the body's full height (cross sizes never
/// flex-shrink, so a centred scroll box started inside the header).
/// The look itself is probe-verified (`skills-dock`).
#[test]
fn skills_page_fills_the_remainder_and_stretches_its_list() {
    let src = page_source();
    let root = src.find("let mut root = div()").expect("the page builds its root off `div()`");
    let root_tail = &src[root..];
    assert!(
        root_tail.starts_with("let mut root = div().flex_1().min_h(px(0.0))"),
        "the page root must take the centre's remainder, never `size_full`"
    );
    assert!(
        !root_tail.starts_with("let mut root = div().size_full()"),
        "a full-height root plus the dock overflows the centre cell"
    );
    let body = src.find("let mut body = h_flex()").expect("the page builds its body off `h_flex()`");
    let body_tail = &src[body..src[body..].find(".child(").map(|i| body + i).unwrap_or(src.len())];
    assert!(
        body_tail.contains(".items_stretch()"),
        "the body row must stretch the scroll box: `h_flex` centres on the cross axis"
    );
}

/// Y3a's Tier V cover: the `skills-dock` probe entry opens the page with
/// the terminal dock up — the condition that overlapped header and list —
/// offline, on the fixture catalog.
#[test]
fn uiprobe_covers_the_skills_page_with_the_dock_open() {
    let path = manifest().join("../../scripts/uiprobe.py");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("probe reads: {}", path.display()));
    assert!(text.contains("\"skills-dock\""), "uiprobe must keep the dock-open Skills entry");
    assert!(
        text.contains("project:.;skills:page;terminal-dock:terminal"),
        "the entry must adopt a project (the dock needs a root), open the page, then the dock"
    );
}
