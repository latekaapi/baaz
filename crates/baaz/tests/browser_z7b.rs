//! Z7b: the agent can drive the browser pane.
//!
//! Baaz is a binary, so this suite cannot call its functions: the
//! round-trips live in the `terminal/service.rs` unit tests (each tool
//! through the socket against the scripted page, session isolation, URL
//! refusals, the timeout path) and in `mcp-bridge/tests/browser_tools.rs`
//! (forwarding, the screenshot image block, the tool list). What
//! remains here is the wiring contract plus pins, below.

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

/// The engine tag carries the agent API: `eval_with_result`,
/// `take_eval_results` and `agent_js` arrive together on v0.3.8, and the
/// service drives pages through `PageAutomation`.
#[test]
fn engine_tag_carries_the_agent_api() {
    let root = workspace_source("Cargo.toml");
    let aui_line = root
        .lines()
        .find(|line| line.starts_with("aui = "))
        .expect("workspace pins aui to a tag");
    // At least v0.3.8 (the eval API); later tags keep it (additive).
    let tag = aui_line.split("tag = \"v").nth(1).and_then(|rest| rest.split('"').next()).expect("a vX.Y.Z tag");
    let parts: Vec<u32> = tag.split('.').map(|part| part.parse().expect("numeric tag")).collect();
    assert!(
        parts.as_slice() >= [0, 3, 8].as_slice(),
        "Z7b builds on a tag with the agent API (>= v0.3.8): {aui_line}"
    );
    let service = source("terminal/service.rs");
    for item in ["automation()", "page.call(", "take_answers", "take_events"] {
        assert!(service.contains(item), "the service drives the page through {item}");
    }
}

/// The service routes every tool on the UI thread, keeps pending evals
/// across drains like `PendingRun`, refuses non-page URLs, and times out
/// with the contract sentence.
#[test]
fn service_routes_every_tool_with_refusals_and_timeout() {
    let service = source("terminal/service.rs");
    for tool in [
        "browser_tabs",
        "browser_navigate",
        "browser_snapshot",
        "browser_act",
        "browser_read",
        "browser_screenshot",
        "browser_console",
        "browser_network",
        "browser_errors",
        "browser_emulate",
        "browser_ask",
    ] {
        assert!(service.contains(tool), "the service routes {tool}");
    }
    assert!(service.contains("BROWSER_TOOL_NAMES"), "the tools are one named list");
    assert!(service.contains("RunningJob"), "running tools survive across drains");
    assert!(service.contains("browser_url_allowed"), "URLs are allow-listed");
    assert!(service.contains("the page did not answer"), "the timeout names itself");
    assert!(
        service.contains("register_browser"),
        "the harness registers each session's webview"
    );
}

/// Opening the pane from a tool updates the session's saved right-pane
/// state like a person's click would — without the toggle, without moving
/// the keyboard.
#[test]
fn agent_open_updates_the_saved_pane_state() {
    let right = source("right.rs");
    assert!(right.contains("show_browser_for_agent"), "the agent has its own open path");
    let app = source("app.rs");
    assert!(
        app.contains("set_browser_open_hook"),
        "the harness wires the service to the browser registry"
    );
    assert!(app.contains("show_browser_for_agent"), "the hook opens the pane deferred");
}

/// The bridge lists its tools next to the seven terminal ones, and
/// the docs name the steering text.
#[test]
fn bridge_lists_the_tools_and_docs_steer() {
    let main = workspace_source("crates/mcp-bridge/src/main.rs");
    assert!(main.contains("register_browser_tools"), "the bridge serves the browser tools");
    assert!(main.contains("register_terminal_tools"), "the terminal tools stay");
    let docs = workspace_source("docs/15-browser-tools.md");
    for tool in [
        "browser_tabs",
        "browser_navigate",
        "browser_snapshot",
        "browser_act",
        "browser_read",
        "browser_screenshot",
        "browser_console",
        "browser_network",
        "browser_errors",
        "browser_emulate",
        "browser_ask",
    ] {
        assert!(docs.contains(tool), "the docs table names {tool}");
    }
    assert!(
        docs.contains("person sees")
            && docs.contains("pages they should watch")
            && docs.contains("local dev servers"),
        "the docs carry the steering text"
    );
}
