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

/// Every file of the `browser` module (task 4.5 split `browser/mod.rs` by
/// seam): `mod.rs` first, then the rest by name, so a pin follows a
/// function into whichever file holds it now.
fn browser_source() -> String {
    let dir = manifest().join("src").join("browser");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .expect("src/browser reads")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "mod.rs")
        .collect();
    names.sort();
    std::iter::once("mod.rs".to_owned()).chain(names).map(|name| source(&format!("browser/{name}"))).collect::<Vec<_>>().join("\n")
}

/// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
#[test]
fn a_hosted_modal_does_not_hide_the_page() {
    let browser = browser_source();
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
