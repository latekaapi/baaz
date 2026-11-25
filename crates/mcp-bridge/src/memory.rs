//! The memory relay: past sessions and shared memory notes as MCP tools
//! (docs/85 12.3), forwarded to the app's socket beside the terminal and
//! browser tools — same routing, same token, same absence rule.

use serde_json::{Value, json};

use crate::terminal::TerminalTarget;
use crate::ToolRegistry;

/// Steering for the model.
pub const INSTRUCTIONS: &str = "Baaz keeps every past session and a shared memory. Before redoing \
    work or asking the person something they may have settled before, look: session_search with 3-5 \
    different phrasings, then session_read on a hit. Save what you learn that a later session in this \
    project would need (a decision, a convention, a fact about the person) with memory_save, one note \
    at a time; if the person says to forget something, remove it with memory_edit delete.";

/// The tools, in contract order.
pub const MEMORY_TOOL_NAMES: [&str; 4] = ["session_search", "session_read", "memory_save", "memory_edit"];

/// One tool's advertisement: name, description, JSON Schema.
pub fn tool_defs() -> Vec<(String, String, Value)> {
    let def = |name: &str, description: &str, schema: Value| (name.to_owned(), description.to_owned(), schema);
    vec![
        def(
            "session_search",
            "Find past sessions: each result names its title, date, provider, project, digest and the text around the match. Pass 3-5 different phrasings of what you are looking for. Searches this project unless scope is all. Read-only.",
            json!({"type": "object", "required": ["queries"], "properties": {
                "queries": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 5, "description": "3-5 phrasings: different words for the same thing."},
                "scope": {"type": "string", "enum": ["project", "all"], "description": "Default project."},
            }}),
        ),
        def(
            "session_read",
            "Read a past session's turns around a query (or its last turns), secrets removed. Omit session for this session's own earlier turns, including those from before a compaction. Read-only.",
            json!({"type": "object", "properties": {
                "session": {"type": "string", "description": "A session id from session_search. Default: this session."},
                "query": {"type": "string", "description": "Words to read around."},
            }}),
        ),
        def(
            "memory_save",
            "Save one memory note that later sessions will see: a decision, a convention, a fact about the person. Project notes are shared by every agent in this project; personal notes go with the person across projects. Saving a title again replaces that note.",
            json!({"type": "object", "required": ["title", "description", "type", "body"], "properties": {
                "title": {"type": "string", "description": "A short name for the note."},
                "description": {"type": "string", "description": "One line: what the note is about."},
                "type": {"type": "string", "enum": ["user", "feedback", "project", "reference"]},
                "body": {"type": "string", "description": "The note. For feedback and project notes, follow with Why: and How to apply: lines."},
                "scope": {"type": "string", "enum": ["project", "personal"], "description": "Default project."},
            }}),
        ),
        def(
            "memory_edit",
            "Change or delete one memory note by its title. Use delete when the person asks to forget something.",
            json!({"type": "object", "required": ["title"], "properties": {
                "title": {"type": "string"},
                "body": {"type": "string", "description": "The new body."},
                "description": {"type": "string", "description": "The new one-line description."},
                "delete": {"type": "boolean"},
                "scope": {"type": "string", "enum": ["project", "personal"], "description": "Default project."},
            }}),
        ),
    ]
}

/// Register the four tools, each forwarded over the terminal socket.
pub fn register_memory_tools(registry: &mut ToolRegistry, target: &TerminalTarget) {
    for (name, description, schema) in tool_defs() {
        let target = target.clone();
        let tool = name.clone();
        registry
            .register(name, description, schema, move |args| {
                super::terminal::forward_call_with(&target.socket, &target.session, target.token.as_deref(), &tool, &args)
            })
            .expect("fresh registry takes the memory tools");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_four_tools_are_declared_in_order_with_their_required_fields() {
        let defs = tool_defs();
        let names: Vec<&str> = defs.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(names, MEMORY_TOOL_NAMES);
        let save = &defs[2].2;
        assert_eq!(save["required"], json!(["title", "description", "type", "body"]));
        assert_eq!(defs[0].2["properties"]["queries"]["maxItems"], 5);
    }
}
