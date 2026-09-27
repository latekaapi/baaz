//! The terminal service's no-logging pin (D53).
//!
//! Terminal output must never reach a `baaz:` log line: a program's bytes
//! are untrusted, and a log line is an exfiltration-shaped hole (a prompt
//! injection that reads the log back through another tool). So the service
//! — the code that carries those bytes — has no logging at all, by
//! construction. A comment is not a check, so this test reads the source:
//! if a log call appears in `terminal/service.rs`, it fails.
//!
//! The behavioural contract (the seven tools over a real socket) is pinned
//! by the unit tests inside `terminal/service.rs` itself, which drive the
//! socket with scripted ptys.

use std::fs;
use std::path::{Path, PathBuf};

/// Logging-shaped tokens that may not appear in the service source — each
/// with the reason it would be wrong there.
const FORBIDDEN: &[(&str, &str)] = &[
    ("baaz_log!", "terminal bytes must never reach the app log"),
    ("eprint!", "stderr is a log line by another name"),
    ("eprintln!", "stderr is a log line by another name"),
    ("println!", "stdout belongs to the relay protocol, not prose"),
    ("print!", "stdout belongs to the relay protocol, not prose"),
    ("tracing::", "a tracing event is a log line"),
    ("log::", "a log macro is a log line"),
    ("dbg!", "debug output leaks bytes to stderr"),
];

/// Every tool the §4 contract names must be dispatched by the service: a
/// match arm each, so removing one fails here as well as in the unit tests.
const TOOLS: &[&str] = &[
    "terminal_list",
    "terminal_open",
    "terminal_run",
    "terminal_read",
    "terminal_screen",
    "terminal_send",
    "terminal_close",
];

fn service_source() -> String {
    let path: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join("terminal").join("service.rs");
    fs::read_to_string(&path).unwrap_or_else(|_| panic!("service source reads: {}", path.display()))
}

#[test]
fn the_service_carries_no_logging() {
    let source = service_source();
    for (token, why) in FORBIDDEN {
        assert!(
            !source.contains(token),
            "terminal/service.rs mentions `{token}`: {why}"
        );
    }
}

#[test]
fn the_service_dispatches_all_seven_tools() {
    let source = service_source();
    for tool in TOOLS {
        assert!(
            source.contains(&format!("\"{tool}\"")),
            "terminal/service.rs no longer dispatches `{tool}`"
        );
    }
}
