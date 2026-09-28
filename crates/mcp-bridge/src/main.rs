//! The stdio bridge: `ping` alone, or the terminal relay beside it.
//!
//! A smoke-test handle and a route, selected by flags:
//!
//! ```bash
//! mcp-bridge # one built-in `ping` tool on real stdin/stdout
//! mcp-bridge --terminal --socket <path> --session <id> # + the seven terminal and six browser tools
//! ```
//!
//! Pipe NDJSON requests in, read NDJSON replies out. The terminal and
//! browser tools forward to Baaz's socket; when the socket is gone they
//! answer that the terminal is unavailable rather than failing.

use std::io::{BufReader, stdin, stdout};
use std::path::PathBuf;

use mcp_bridge::{ToolOutcome, ToolRegistry, browser, serve_loop, terminal};
use mcp_bridge::terminal::TerminalTarget;
use serde_json::json;

fn usage(error: &str) -> ! {
    if !error.is_empty() {
        eprintln!("error: {error}");
    }
    eprintln!("usage: mcp-bridge [--terminal --socket <path> --session <id>]");
    std::process::exit(if error.is_empty() { 0 } else { 2 });
}

fn main() {
    let mut terminal = false;
    let mut socket: Option<PathBuf> = None;
    let mut session: Option<String> = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--terminal" => terminal = true,
            "--socket" => {
                socket = Some(PathBuf::from(args.next().unwrap_or_else(|| usage("--socket needs <path>"))));
            }
            "--session" => {
                session = Some(args.next().unwrap_or_else(|| usage("--session needs an id")));
            }
            "-h" | "--help" => usage(""),
            other => usage(&format!("unknown argument `{other}`")),
        }
    }
    let mut registry = ToolRegistry::new();
    if terminal {
        let (Some(socket), Some(session)) = (socket, session) else {
            usage("--terminal needs --socket <path> and --session <id>");
        };
        registry.set_instructions(format!("{}\n{}", terminal::INSTRUCTIONS, browser::INSTRUCTIONS));
        let target = TerminalTarget { socket, session };
        terminal::register_terminal_tools(&mut registry, &target);
        browser::register_browser_tools(&mut registry, &target);
    }
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
