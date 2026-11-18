//! The Browse overlay window hosts the palette and dialogs over the live page.
//!
//! baaz is a binary, so the render and sync wiring is pinned in source text.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}
/// `name` under `src/`; a module the core crates took over (docs/82 4.11)
/// is read where it lives now: `browser/x.rs` in `baaz-browser-core`,
/// anything else in `baaz-core`.
fn resolve(name: &str) -> PathBuf {
    let app = manifest().join("src").join(name);
    if app.exists() {
        return app;
    }
    match name.strip_prefix("browser/") {
        Some(rest) => manifest().join("../baaz-browser-core/src").join(rest),
        None => manifest().join("../baaz-core/src").join(name),
    }
}


fn source(name: &str) -> String {
    let path = resolve(name);
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
        .chain(core_browser_files())
        .collect();
    names.sort();
    std::iter::once("mod.rs".to_owned()).chain(names).map(|name| source(&format!("browser/{name}"))).collect::<Vec<_>>().join("\n")
}

/// The browser module's files that live in `baaz-browser-core` (docs/82
/// 4.11), as paths from `src/browser/` (a name both crates use, like
/// `adblock.rs`, is read from each).
fn core_browser_files() -> Vec<String> {
    std::fs::read_dir(manifest().join("../baaz-browser-core/src"))
        .expect("baaz-browser-core/src reads")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".rs") && name != "lib.rs")
        .map(|name| format!("../../../baaz-browser-core/src/{name}"))
        .collect()
}

/// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
#[test]
fn a_hosted_modal_does_not_hide_the_page() {
    // Plan 4.6: the page's overlay cover is the stack's answer, and only an
    // entry drawn in the main window covers; in Browse the modals' host
    // resolves to the overlay window.
    let browser = browser_source();
    assert!(browser.contains("self.overlays.read(cx).stack.covers_page(cx)"), "the page's overlay cover is the stack's");
    let stack = source("overlay_stack.rs");
    assert!(stack.contains("open.covers_page && open.host.resolve(browse) == Host::Main"), "an entry in the overlay window covers nothing");
    assert!(stack.contains("Layer::Dialog(_) | Layer::Palette(_) => Host::Modal"), "the dialog and the palette follow the mode");
    assert!(stack.contains("Host::Modal if browse => Host::Panel"), "Browse hosts a modal in the overlay window");
}

/// Owner recording 2026-10-11 5.23 PM: the ⌘K scrim dimmed the sidebar before the page; in Browse the overlay window hosts the modal over the live page.
#[test]
fn the_main_window_skips_a_hosted_palette() {
    let app = source("app.rs");
    // Plan 4.6: the palette and the dialog are their own views; the root
    // composes them (facts pushed, then the view) only where it hosts them.
    assert!(
        app.contains("if modal_hosted { None } else { self.compose_palette(cx) }"),
        "the main window does not also draw the hosted palette"
    );
    assert!(
        app.contains("if modal_hosted { None } else { self.compose_dialog(cx) }"),
        "the main window does not also draw the hosted dialog"
    );
}
