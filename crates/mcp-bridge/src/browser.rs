//! The browser relay: Baaz's six browser tools as MCP, forwarded to
//! the app's unix socket beside the seven terminal tools.
//!
//! Same socket, same routing, same [`UNAVAILABLE`][super::terminal::UNAVAILABLE]
//! rule as [`super::terminal`]: each `tools/call` sends one
//! `{id, session, tool, params}` line naming the calling session, and the
//! service answers from that session's own webview — never another
//! session's. The one shaping this module adds is `browser_screenshot`:
//! the service replies with `{"image_base64": …}` and the handler renders
//! it as an MCP image content block rather than text.

use serde_json::{Value, json};

use crate::terminal::TerminalTarget;
use crate::{ToolOutcome, ToolRegistry};

/// Steering for the model: the person sees this browser, so it is for
/// pages they should watch or for local dev servers — not for bulk
/// fetching. Shared with the tool descriptions below.
pub const INSTRUCTIONS: &str = "Baaz's browser tools drive the person's visible browser: \
    every page the agent opens there the person can watch. Use the browser for pages they \
    should watch or for local dev servers; use the shell tool for quick captured checks. \
    If a browser tool reports that Baaz isn't running, the browser is unavailable — say \
    so and carry on without it.";

/// The six tools, in contract order (`docs/15-browser-tools.md`).
pub const BROWSER_TOOL_NAMES: [&str; 6] = [
    "browser_open",
    "browser_read",
    "browser_links",
    "browser_click",
    "browser_type",
    "browser_screenshot",
];

/// One tool's advertisement: name, description, JSON Schema.
pub fn tool_defs() -> Vec<(String, String, Value)> {
    vec![
        (
            "browser_open".to_owned(),
            "Open a URL in the person's visible browser and wait for it to load. \
            The person sees this browser; use it for pages they should watch or for \
            local dev servers. Opens the Browser pane when it is closed."
                .to_owned(),
            json!({"type": "object", "required": ["url"], "properties": {
                "url": {"type": "string", "description": "http, https, file, or about:blank URL to open."},
            }}),
        ),
        (
            "browser_read".to_owned(),
            "Read the visible page as text: its title, URL, and body text. \
            The person sees this browser; use it for pages they should watch or for \
            local dev servers. Read-only."
                .to_owned(),
            json!({"type": "object", "properties": {
                "max_chars": {"type": "integer", "description": "Body text cap (default 8000)."},
            }}),
        ),
        (
            "browser_links".to_owned(),
            "List the visible page's links as href + text pairs. \
            The person sees this browser; use it for pages they should watch or for \
            local dev servers. Read-only."
                .to_owned(),
            json!({"type": "object", "properties": {
                "max": {"type": "integer", "description": "Link cap (default 100)."},
            }}),
        ),
        (
            "browser_click".to_owned(),
            "Click the first element matching a CSS selector on the visible page. \
            The person sees this browser; use it for pages they should watch or for \
            local dev servers."
                .to_owned(),
            json!({"type": "object", "required": ["selector"], "properties": {
                "selector": {"type": "string", "description": "CSS selector of the element to click."},
            }}),
        ),
        (
            "browser_type".to_owned(),
            "Type text into the first element matching a CSS selector on the \
            visible page. The person sees this browser; use it for pages they \
            should watch or for local dev servers."
                .to_owned(),
            json!({"type": "object", "required": ["selector", "text"], "properties": {
                "selector": {"type": "string", "description": "CSS selector of the field to type into."},
                "text": {"type": "string", "description": "Text to type."},
            }}),
        ),
        (
            "browser_screenshot".to_owned(),
            "Capture the visible page as a PNG image. \
            The person sees this browser; use it for pages they should watch or for \
            local dev servers. Read-only."
                .to_owned(),
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

/// Register the six tools, each forwarding to `target` — the same socket
/// and session the terminal tools use.
pub fn register_browser_tools(registry: &mut ToolRegistry, target: &TerminalTarget) {
    for (name, description, schema) in tool_defs() {
        let target = target.clone();
        let tool = name.clone();
        registry
            .register(name, description, schema, move |args| {
                forward_browser_call(&target, &tool, &args)
            })
            .expect("fresh registry takes the browser tools");
    }
}

/// Forward one call over the terminal socket and render the reply.
///
/// Routing and absence behave exactly like
/// [`super::terminal::forward_call`]; the only shaping here is the
/// screenshot: a result carrying `image_base64` becomes an MCP image
/// content block, everything else stays text.
pub fn forward_browser_call(
    target: &TerminalTarget,
    tool: &str,
    args: &Value,
) -> Result<ToolOutcome, String> {
    let outcome = super::terminal::forward_call(&target.socket, &target.session, tool, args)?;
    if tool != "browser_screenshot" {
        return Ok(outcome);
    }
    for block in &outcome.content {
        if let Some(text) = block.text.as_deref() {
            if let Ok(result) = serde_json::from_str::<Value>(text) {
                if let Some(png) = result.get("image_base64").and_then(Value::as_str) {
                    return Ok(ToolOutcome {
                        content: vec![crate::ContentBlock::image(png, "image/png")],
                        is_error: false,
                    });
                }
            }
        }
    }
    Ok(outcome)
}
