//! R3 toasts under a native page: the wiring contracts the unit tests
//! cannot see.
//!
//! baaz is a binary, so this suite cannot drive its internals (the
//! `toasts_in_header` truth table over the five layout cases is pinned by
//! `#[gpui::test]` tests beside the predicate in `dialogs.rs`). What this
//! suite pins is the shipped wiring, by *executing* the artifacts rather
//! than reading their text where it matters: the probe module loads at
//! runtime and its entry table — the data structure a probe run actually
//! dispatches on — drives a `toast:` window step onto a Browse page. The
//! render hand-overs (the stack stands down, all three header variants
//! seat the chip left of the feedback slot, the chip's contract) are
//! pinned in source text like `browse_h5.rs`, since no headless run can
//! show a real Chromium page covering the stack.

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

/// While a native page is on screen the top-right stack stands down: the
/// header chip carries the toasts instead, so nothing hides under the
/// page. Anywhere else the stack reads exactly as before.
#[test]
fn the_stack_stands_down_while_the_header_carries_toasts() {
    let dialogs = source("dialogs.rs");
    let render = fn_body(&dialogs, "render_toasts");
    assert!(
        render.contains("toasts_in_header"),
        "render_toasts consults the predicate"
    );
    let gate = render.find("toasts_in_header").expect("the predicate gates the stack");
    let empty = render.find("toasts.is_empty()").expect("the empty check stays");
    assert!(gate < empty, "the header gate runs before the empty check");
}

/// All three header variants seat the chip immediately left of the
/// feedback slot — the one header region native pages never cover.
#[test]
fn every_header_seats_the_chip_left_of_the_feedback_slot() {
    let app = source("app.rs");
    for site in ["render_browse_toolbar", "render_browse_pill_parts", "render_centre_header"] {
        let body = fn_body(&app, site);
        let chip = body.find("render_toast_chip").unwrap_or_else(|| panic!("{site} renders the chip"));
        let slot =
            body.find("render_feedback_header_slot").unwrap_or_else(|| panic!("{site} keeps the slot"));
        assert!(chip < slot, "{site} seats the chip left of the feedback slot");
    }
}

/// The chip's contract: a status named `<title>: <body>`, the full body
/// on hover, a labelled action and close, newest only with `+N` for the
/// queue — on the header-button-height surface-2 chip the spec draws.
#[test]
fn the_chip_names_itself_and_stays_one_line() {
    let dialogs = source("dialogs.rs");
    let chip = fn_body(&dialogs, "render_toast_chip");
    assert!(chip.contains("Role::Status"), "the chip is a status");
    assert!(chip.contains("aria_label(tip"), "the status is labelled <title>: <body>");
    assert!(chip.contains("ToastChipTip"), "hover shows the full body as a tooltip");
    assert!(chip.contains("Dismiss notification"), "the close button is labelled");
    assert!(chip.contains("toasts.last()"), "newest toast only");
    assert!(chip.contains("\"+{queued}\""), "a muted +N while several queue");
    assert!(chip.contains("max_w(px(480.0))"), "the chip caps at 480 px");
    assert!(chip.contains("max_w(px(320.0))"), "the body truncates at 320 px");
    assert!(chip.contains("h(px(24.0))"), "the chip stands at header button height");
    assert!(chip.contains("bg(p.surface_2)"), "the chip grounds on surface-2");
    assert!(chip.contains("rounded(px(8.0))"), "the chip rounds at 8 px");
}
