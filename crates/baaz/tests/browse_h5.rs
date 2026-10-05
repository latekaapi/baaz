//! B2a-H5c: the wiring contracts the unit tests cannot see.
//!
//! Like `browse_h4.rs`, this suite cannot boot the app (baaz is a binary),
//! so it pins the wiring in source text. The pure decisions — the zoom
//! ladder, the downloads badge, per-tab debounce, `(origin, kind)` grants,
//! origin normalisation, popup foreground, the honest popover-covering
//! rule — are pinned by unit tests in `browser/chrome.rs` and
//! `browser/toolbar.rs`; this file pins the two hand-overs the gate cannot
//! run: the shared find input drops when no strip stands open (a closed
//! tab cannot strand it), and a crashed tab's strip closes before the
//! stored page rect counts strip rows (no dead row under the error page).

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

/// The shared find input lives while any strip stands open: the per-frame
/// sync drops it (and its subscription) once the closed-tab retain leaves
/// none — the same hand-over `close_browse_find` does for Done/Esc, so a
/// closed tab never strands a reader-less input.
#[test]
fn shared_find_input_drops_when_no_strip_stands_open() {
    let browser = source("browser/mod.rs");
    let sync = fn_body(&browser, "sync_browser");
    let retain = sync.find("find_strips.retain(&live)").expect("closed tabs drop their strips");
    let drop = sync.find("browse_find = None").expect("the input drops with the last strip");
    assert!(drop > retain, "the drop follows the retain");
    let guard = sync.find("find_strips.any_open()").expect("the input lives while any strip is open");
    assert!(guard > retain && guard < drop, "aliveness is checked between retain and drop");
    let unsub = sync.find("browse_find_sub = None").expect("the subscription drops too");
    assert!(unsub > drop, "the subscription drops with the input");
}

/// A crashed page hides the find strip and owns the rect alone: the
/// crash-close (`stop_find`, strip close) runs before the strip-stack
/// rect computation, so the stored rect never reserves a row for a strip
/// that just closed.
#[test]
fn crashed_strip_closes_before_the_rect_stores() {
    let browser = source("browser/mod.rs");
    let sync = fn_body(&browser, "sync_browser");
    let crash = sync.find("crashed().is_some()").expect("a crash closes the strip");
    let stack = sync.find("strip_stack_px").expect("the rect counts open strip rows");
    let stored = sync.find("centre_page_rect = in_browse.then_some").expect("the rect stores once");
    assert!(crash < stack && stack < stored, "crash-close, then stack, then store");
    assert!(sync[crash..stack].contains("stop_find"), "the crash drops the highlight");
}
