//! Screen-reader clickability ratchet: labelled clickables may only go up.
//!
//! VoiceOver's AXPress arrives as an a11y Click at the node's recorded
//! bounds, so an interactive element without an accessible name is silent to
//! assistive tech even while the mouse works. gpui has no element-tree query
//! in tests here, so this test works at the source level instead: it counts
//! `.on_click(` occurrences under `crates/baaz/src` whose builder chain (the
//! ~12 preceding lines) carries neither `accessibility_label` nor
//! `aria_label` nor a `button(`/`chip(`/`nav_item(`/`sidebar_footer(` call
//! with a visible text label, and pins the count.
//!
//! **When this test fails because the number went down, lower `CEILING`.**
//! When it fails because the number went up, name the new control instead:
//! `.accessibility_label(...)` on an aui `Button`/`Chip`, or
//! `.role(gpui::Role::Button).aria_label(...)` on a raw clickable div.
//!
//! The remaining hits at the time of writing are all outside this ratchet's
//! reach: `connect.rs`, `dialogs.rs`, `project_menu.rs` and
//! `settings_providers.rs` are not covered by the labelling pass, and the
//! `account-scrim` catcher in `sidebar_view.rs` sits inside
//! `render_account_menu`, which must not be touched. Shrinking that list is
//! welcome — lowering `CEILING` along with it is mandatory.

use std::fs;
use std::path::{Path, PathBuf};

/// The most `.on_click(` sites under `crates/baaz/src` that may lack an
/// accessible name. Measured after the Z9b labelling pass. It may only ever
/// be reduced.
const CEILING: usize = 8;

/// How many lines above an `.on_click(` still count as its builder chain.
const CHAIN_LINES: usize = 12;

fn source_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

/// Builders that take a visible text label, which names the control for
/// assistive tech on its own: `button("id", "label")` and friends.
fn has_text_label(window: &str) -> bool {
    ["button(\"", "chip(\"", "nav_item(\"", "sidebar_footer(\""].iter().any(|needle| window.contains(needle))
}

fn unlabeled_clickables() -> Vec<String> {
    let root = source_root();
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    files.sort();
    let mut hits = Vec::new();
    for path in files {
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let lines: Vec<&str> = text.lines().collect();
        for (index, line) in lines.iter().enumerate() {
            if !line.contains(".on_click(") {
                continue;
            }
            let start = index.saturating_sub(CHAIN_LINES);
            let window = lines[start..=index].join("\n");
            if window.contains("accessibility_label")
                || window.contains("aria_label")
                || has_text_label(&window)
            {
                continue;
            }
            let rel = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
            hits.push(format!("{rel}:{}", index + 1));
        }
    }
    hits
}

#[test]
fn unlabeled_clickables_only_ever_go_down() {
    let hits = unlabeled_clickables();
    assert!(
        hits.len() <= CEILING,
        "{} `.on_click(` sites under crates/baaz/src lack an accessible name, above the ceiling of {}.\n\
         Name the new control (`.accessibility_label` on Button/Chip, `.role(Role::Button).aria_label` on a raw div).\n\
         Raising CEILING is not the fix.\nSites:\n  {}",
        hits.len(),
        CEILING,
        hits.join("\n  "),
    );
}
