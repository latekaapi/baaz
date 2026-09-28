//! Z7b: the agent can drive the browser pane.
//!
//! Baaz is a binary, so this suite cannot call its functions: the
//! round-trips live in the `terminal/service.rs` unit tests (each tool
//! through the socket against the scripted page, session isolation, URL
//! refusals, the timeout path) and in `mcp-bridge/tests/browser_tools.rs`
//! (forwarding, the screenshot image block, the thirteen-tool list). What
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
/// service builds every script from `agent_js`.
#[test]
fn engine_tag_carries_the_agent_api() {
    let root = workspace_source("Cargo.toml");
    let aui_line = root
        .lines()
        .find(|line| line.starts_with("aui = "))
        .expect("workspace pins aui to a tag");
    assert!(
        aui_line.contains("tag = \"v0.3.8\""),
        "Z7b builds on the tag with the eval API: {aui_line}"
    );
    let service = source("terminal/service.rs");
    for item in [
        "agent_js::page_text",
        "agent_js::list_links",
        "agent_js::click",
        "agent_js::type_text",
        "eval_with_result",
        "take_eval_results",
    ] {
        assert!(service.contains(item), "the service drives the page through {item}");
    }
}

/// The service routes all six tools on the UI thread, keeps pending evals
/// across drains like `PendingRun`, refuses non-page URLs, and times out
/// with the contract sentence.
#[test]
fn service_routes_six_tools_with_refusals_and_timeout() {
    let service = source("terminal/service.rs");
    for tool in [
        "browser_open",
        "browser_read",
        "browser_links",
        "browser_click",
        "browser_type",
        "browser_screenshot",
    ] {
        assert!(service.contains(tool), "the service routes {tool}");
    }
    assert!(service.contains("BROWSER_TOOL_NAMES"), "the six tools are one named list");
    assert!(service.contains("PendingBrowser"), "pending evals survive across drains");
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

/// The bridge lists thirteen tools next to the seven terminal ones, and
/// the docs name the steering text.
#[test]
fn bridge_lists_thirteen_and_docs_steer() {
    let main = workspace_source("crates/mcp-bridge/src/main.rs");
    assert!(main.contains("register_browser_tools"), "the bridge serves the browser tools");
    assert!(main.contains("register_terminal_tools"), "the terminal tools stay");
    let docs = workspace_source("docs/15-browser-tools.md");
    for tool in [
        "browser_open",
        "browser_read",
        "browser_links",
        "browser_click",
        "browser_type",
        "browser_screenshot",
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
