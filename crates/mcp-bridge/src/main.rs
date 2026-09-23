//! Diagnostic stdio bridge: one built-in `ping` tool on real stdin/stdout.
//!
//! A smoke-test handle, not a feature: pipe NDJSON requests in, read NDJSON
//! replies out. No Baaz tool is wired here.

use std::io::{BufReader, stdin, stdout};

use mcp_bridge::{ToolOutcome, ToolRegistry, serve_loop};
use serde_json::json;

fn main() {
    let mut registry = ToolRegistry::new();
    registry
        .register(
            "ping",
            "Diagnostic ping: replies PONG so the bridge can be smoke-tested by hand.",
            json!({"type": "object", "properties": {}}),
            |_| Ok(ToolOutcome::text("PONG")),
        )
        .expect("fresh registry takes ping");
    let stdin = stdin();
    let mut out = stdout();
    if let Err(e) = serve_loop(BufReader::new(stdin.lock()), &mut out, &registry) {
        eprintln!("mcp-bridge: {e}");
        std::process::exit(1);
    }
}
