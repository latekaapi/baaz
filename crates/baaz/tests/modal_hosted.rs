//! The Browse overlay window hosts the palette and dialogs over the live page.
//!
//! baaz is a binary, so the render and sync wiring is pinned in source text.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
#[test]
fn a_hosted_modal_does_not_hide_the_page() {
    let browser = source("browser/mod.rs");
    assert!(browser.contains("overlays.palette.is_some() && !modal_hosted"), "a hosted palette leaves the page up");
    assert!(browser.contains("overlays.dialog.is_some() && !modal_hosted"), "a hosted dialog leaves the page up");
}

/// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
#[test]
fn the_main_window_skips_a_hosted_palette() {
    let app = source("app.rs");
    assert!(
        app.contains("if modal_hosted { None } else { self.render_palette(cx) }"),
        "the main window does not also draw the hosted palette"
    );
}
