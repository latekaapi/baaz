//! Z7a: the browser pane runs a real engine (`aui-webview`) per session.
//!
//! Baaz is a binary, so this suite cannot call its functions: the behaviour
//! lives in `browser.rs` unit tests (`browser_visible` truth table over all
//! eleven inputs, `SendAnnotations` draft text, URL persistence round-trip,
//! per-session isolation — all real calls, no window) and in `right.rs`
//! `gpui::test`s (the pane's state drives the nav row on the scripted
//! backend). What remains here is the source contract plus pins, below.

use std::path::{Path, PathBuf};

fn manifest() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

fn source(name: &str) -> String {
    let path = manifest().join("src").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

fn workspace_source(name: &str) -> String {
    let path = manifest().join("../../").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("source reads: {}", path.display()))
}

/// The engine dependency: `aui-webview` at the same git source/tag as `aui`,
/// with the `wry` (WKWebView) feature.
#[test]
fn webview_dependency_rides_with_aui() {
    let root = workspace_source("Cargo.toml");
    let aui_line = root
        .lines()
        .find(|line| line.starts_with("aui = "))
        .expect("workspace pins aui to a tag");
    let tag = aui_line
        .split("tag = \"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .expect("aui names its tag");
    let webview = root
        .lines()
        .find(|line| line.starts_with("aui-webview = "))
        .expect("workspace depends on aui-webview");
    assert!(
        webview.contains(&format!("tag = \"{tag}\"")),
        "aui-webview must track aui's tag ({tag}): {webview}"
    );
    assert!(
        webview.contains("wry"),
        "aui-webview needs the wry feature for WKWebView: {webview}"
    );
    let baaz = manifest().join("Cargo.toml");
    let baaz = std::fs::read_to_string(&baaz).expect("baaz manifest reads");
    assert!(
        baaz.contains("aui-webview.workspace = true"),
        "baaz uses the workspace aui-webview"
    );
}

/// The pane renders the library's `webview_pane` for the session's state —
/// never the old chrome-only placeholder — and keeps role Group + "Browser".
#[test]
fn browser_pane_renders_the_session_webview() {
    let right = source("right.rs");
    assert!(right.contains("aui_webview::webview_pane(\"right-browser\""), "pane renders webview_pane");
    assert!(right.contains(".aria_label(\"Browser\")"), "pane keeps its accessible label");
    assert!(right.contains("handle_browser_intent"), "intents forward to the harness");
    assert!(
        !right.contains("no web engine"),
        "the chrome-only placeholder is gone"
    );
}

/// The fake backend never reaches production render: the registry picks it
/// only behind the boot flag (captures) or in tests. Grep-level pin; the
/// real proof is `browser.rs` (flag-gated construction) plus the capture
/// entries below.
#[test]
fn fake_backend_only_behind_the_boot_flag() {
    let browser = source("browser.rs");
    assert!(browser.contains("self.browser.fake || cfg!(test)"), "fake is flag- or test-gated");
    assert!(browser.contains("WryBackend::new_at"), "production path builds WKWebView");
    assert!(browser.contains("set_obscured"), "the host hides the native view under overlays");
    assert!(browser.contains("browser_visible"), "visibility runs through the pure predicate");
}

/// Per-session memory: the registry is keyed by session id (plus a home
/// key), the URL persists on `RightState`, and `browse:` drives navigation.
#[test]
fn per_session_state_and_verbs_are_wired() {
    let browser = source("browser.rs");
    assert!(browser.contains("browser.states"), "one live state per session");
    assert!(browser.contains("browser.home"), "a home webview with no session open");
    let sessions = source("sessions.rs");
    assert!(sessions.contains("pub browser_url: Option<String>"), "RightState remembers the URL");
    let steps = source("steps.rs");
    assert!(steps.contains("verb: \"browse\""), "`browse:<url>` is a window verb");
    assert!(steps.contains("step_browse"), "the verb navigates the active/home browser");
}

/// Intents land in the draft, never auto-send: screenshots attach as images,
/// annotations append text, Console toasts.
#[test]
fn intents_land_in_the_draft() {
    let browser = source("browser.rs");
    assert!(browser.contains("attach_screenshot"), "Screenshot attaches the PNG to the draft");
    assert!(browser.contains("append_draft_block"), "SendAnnotations appends pin text to the draft");
    assert!(browser.contains("Console is not available yet"), "Console toasts");
    assert!(browser.contains("annotations_draft_block"), "pin text is built by the pure helper");
    let composer = source("session/composer.rs");
    assert!(composer.contains("fn attach_screenshot"), "image-attach entry point exists");
    assert!(composer.contains("fn append_draft_block"), "text-append entry point exists");
}

/// The visual surface: `right-browser` still captures deterministically and
/// `right-browser-page` drives the fake backend through `browse:`.
#[test]
fn probe_entries_cover_blank_and_loaded_browser() {
    let probe = workspace_source("scripts/uiprobe.py");
    assert!(probe.contains("\"right-browser\""), "blank browser entry stays");
    assert!(
        probe.contains("\"right-browser-page\""),
        "loaded-page browser entry exists"
    );
    assert!(
        probe.contains("browse:https://example.com"),
        "the loaded entry navigates the fake backend to example.com"
    );
}
