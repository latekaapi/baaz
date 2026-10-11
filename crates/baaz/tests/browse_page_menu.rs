//! Owner screenshot 2026-10-12: the Browse page controls' ⋯ opened the
//! Sessions header's overflow menu (Rename / Fork / Archive) at the window's
//! top right, over the page. It opens the Browse page menu, an overlay menu.

fn source(rel: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(rel)).expect("source")
}

#[test]
fn the_page_controls_menu_is_the_browse_page_menu() {
    let view = source("sidebar_view.rs");
    assert!(view.contains("BrowseIntent::PageMenu => self.open_browse_page_menu(window, cx)"), "⋯ opens the page menu");
    assert!(!view.contains("BrowseIntent::PageMenu => self.open_menu(MenuKind::Overflow"), "never the Sessions header menu");
    let actions = source("browser/sidebar_actions.rs");
    assert!(actions.contains("BrowseMenuTarget::Page {"), "a Browse menu target, drawn in the overlay window");
}
