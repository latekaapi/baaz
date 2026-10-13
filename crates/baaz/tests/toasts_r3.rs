//! Toasts in the corner stack (R3, moved by D4): the wiring contracts the
//! unit tests cannot see.
//!
//! baaz is a binary, so this suite cannot drive its internals (the
//! `corner_in_overlay` predicate is pinned by a `#[gpui::test]` beside it
//! in `dialogs.rs`). What this
//! suite pins is the shipped wiring, by *executing* the artifacts rather
//! than reading their text where it matters: the probe module loads at
//! runtime and its entry table — the data structure a probe run actually
//! dispatches on — drives a `toast:` window step onto a Browse page. The
//! placements (above the composer, in the corner window over the page)
//! are pinned in source text like `browse_h5.rs`, since no headless run
//! can show the corner window over a real Chromium page.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("crates/").parent().expect("root/").to_path_buf()
}

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

/// The probe module's `ENTRIES` table, loaded at runtime and printed as
/// JSON: the table `boot()` dispatches on, not the file's text.
fn probe_entries() -> serde_json::Value {
    let probe = workspace_root().join("scripts").join("uiprobe.py");
    let loader = r#"
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("uiprobe", sys.argv[1])
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
print(json.dumps({"header": module.ENTRIES["toast-header"], "all": module.ENTRIES}))
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(loader)
        .arg(&probe)
        .output()
        .unwrap_or_else(|error| panic!("loading {}: {error}", probe.display()));
    assert!(
        output.status.success(),
        "uiprobe.py loads: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_str(&String::from_utf8_lossy(&output.stdout)).expect("entries parse as JSON")
}

fn shot_steps(entry: &serde_json::Value) -> Vec<String> {
    entry
        .get("shot")
        .and_then(|shot| shot.as_array())
        .unwrap_or_else(|| panic!("entry has a shot list: {entry}"))
        .iter()
        .map(|arg| arg.as_str().unwrap_or_default().to_owned())
        .collect()
}

/// The exact `--steps` payload an entry passes to baaz.
fn steps_arg(entry: &serde_json::Value) -> String {
    let args = shot_steps(entry);
    let mut steps = String::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if arg == "--steps" {
            steps = args.next().cloned().unwrap_or_default();
        }
    }
    assert!(!steps.is_empty(), "entry carries a --steps payload: {args:?}");
    steps
}

/// The `toast-header` entry shows a toast over a live Browse page: it
/// enters Browse, lands the scripted page (captures always run the fake
/// page, which the predicate counts like a real one), then raises the
/// toast through the `toast:` window verb — exact `--steps` payload, read
/// off the runtime table.
#[test]
fn probe_entry_shows_a_toast_over_the_browse_page() {
    let entries = probe_entries();
    let steps = steps_arg(&entries["header"]);
    let items: Vec<&str> = steps.split(';').collect();
    assert!(items.contains(&"mode:browse"), "the toast lands in Browse: {steps}");
    assert!(
        items.iter().any(|step| step.starts_with("tab-new:https://")),
        "a scripted page is on screen first: {steps}"
    );
    let toast = items.iter().find(|step| step.starts_with("toast:")).expect("a toast: step raises it");
    let (title, body) = toast
        .strip_prefix("toast:")
        .and_then(|rest| rest.split_once('|'))
        .unwrap_or_else(|| panic!("toast: carries <title>|<body>: {toast}"));
    assert!(!title.trim().is_empty(), "the toast has a title: {toast}");
    assert!(!body.trim().is_empty(), "the toast has a body: {toast}");
}

/// `toast:` is a window verb with a doc-table row, so the probe entry's
/// step dispatches (and the steps unit test's doc-sync covers the row
/// from the other side).
#[test]
fn toast_is_a_documented_window_verb() {
    let steps = source("steps.rs");
    assert!(steps.contains(r#"verb: "toast""#), "toast: dispatches as a window verb");
    assert!(steps.contains("`toast:<title>"), "toast: has its doc-table row");
}

/// D4 (docs/41 §2.11): the corner stack is the toasts' one home — the
/// newest toast above the feedback slot, right-aligned, 8 apart. The
/// top-right stack and the header chip are gone.
#[test]
fn the_corner_stack_holds_the_toast_above_the_slot() {
    let dialogs = source("dialogs.rs");
    let stack = fn_body(&dialogs, "render_corner_stack");
    let toast = stack.find("render_toast_pill").expect("the stack draws the toast");
    let slot = stack.find("render_feedback_header_slot").expect("the stack draws the feedback slot");
    assert!(toast < slot, "the toast sits above the slot");
    assert!(stack.contains("items_end()"), "right-aligned");
    assert!(stack.contains("gap(px(8.0))"), "8 apart");
    assert!(!dialogs.contains("fn render_toasts("), "the top-right stack is gone");
    assert!(!dialogs.contains("fn render_toast_chip("), "the header chip is gone");
    let app = source("app.rs");
    let bar = fn_body(&app, "render_bar_chrome");
    assert!(!bar.contains("render_feedback_header_slot"), "the header no longer carries the slot");
}

/// Sessions: the stack rides the composer band, its bottom 8 above the
/// composer and 12 from the sheet's right; Browse: its own window over the
/// page, 12 above the sheet's bottom.
#[test]
fn the_stack_sits_above_the_composer_and_over_the_page() {
    let dialogs = source("dialogs.rs");
    let band = fn_body(&dialogs, "corner_above_band");
    assert!(band.contains("right(px(12.0))") && band.contains("pb(px(8.0))"), "8 above the composer, 12 in");
    let sheet = fn_body(&dialogs, "corner_at_sheet_bottom");
    assert!(sheet.contains("bottom(px(12.0))"), "12 above the sheet's bottom without a composer");
    let overlay = source("overlay.rs");
    let wanted = fn_body(&overlay, "wanted_corner");
    assert!(wanted.contains("corner_in_overlay"), "Browse hosts it in the corner window");
    assert!(wanted.contains("centre_page_rect"), "anchored on the sheet");
    let sync = fn_body(&overlay, "sync_corner");
    assert!(sync.contains("focus: false"), "the corner window never takes the keyboard");
}

/// The toast's contract: newest only with `+N`, Undo as its one action,
/// a close, settled under a deterministic capture.
#[test]
fn the_toast_is_the_newest_with_its_queue() {
    let dialogs = source("dialogs.rs");
    let pill = fn_body(&dialogs, "render_toast_pill");
    assert!(pill.contains("toasts.last()"), "newest toast only");
    assert!(pill.contains(".queued(queued)"), "+N while several queue");
    assert!(pill.contains("undo_newest"), "the one action is Undo");
    assert!(pill.contains(".on_close("), "dismissable");
    assert!(pill.contains("at_rest(crate::clock::deterministic())"), "captures draw it settled");
}
