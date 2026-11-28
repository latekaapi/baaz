//! The pane relay (docs/85 13.1–13.4, W4): show the person something in
//! Baaz's right pane, and read or act on Changes, as MCP tools forwarded to
//! the app's socket beside the terminal, browser and memory tools — same
//! routing, same token, same absence rule.
//!
//! Each description carries the phrases a person would say (rule 19), so a
//! provider that defers tools finds them by search.

use serde_json::{Value, json};

use crate::terminal::TerminalTarget;
use crate::ToolRegistry;

/// Steering for the model.
pub const INSTRUCTIONS: &str = "To show the person something — a file at a line, a diff, what changed, the terminal, \
    the pull request — use pane_show: it opens in Baaz's right pane beside the chat (\"open the terminal in the right \
    pane\" is pane_show view=terminal). Call Baaz's tools by the names you were given, and say a tool worked only after \
    it returned without an error. changes_read tells what changed (all \
    changes, this session, the last turn, a turn); changes_revert, changes_comment and changes_commit act through the \
    pane so the person sees it; pr_status reads the pull request's state, checks and review comments.";

/// The tools, in contract order.
pub const PANE_TOOL_NAMES: [&str; 6] = ["pane_show", "changes_read", "changes_revert", "changes_comment", "changes_commit", "pr_status"];

const SCOPE: &str = "all (the working tree against HEAD, the default), session (since this session began), last_turn, or turn_<n>.";

/// One tool's advertisement: name, description, JSON Schema.
pub fn tool_defs() -> Vec<(String, String, Value)> {
    let def = |name: &str, description: &str, schema: Value| (name.to_owned(), description.to_owned(), schema);
    vec![
        def(
            "pane_show",
            "Whenever the person says \"show me\", \"open\" or \"switch to\": put it on their screen in Baaz's right pane — a file \
             in the preview (at a line), a file in the tree, one file's diff, what changed (all, this session, the last \
             turn, turn N), the terminal, the browser, Tasks, or the pull request. Use for \"open X in the preview\", \"show \
             me the diff in X\", \"show what you changed last turn\", \"show me X at line N\", \"switch to the terminal\", \
             \"show me the PR\". Reading it yourself is changes_read; showing it is this. Changes nothing.",
            json!({"type": "object", "required": ["view"], "properties": {
                "view": {"type": "string", "enum": ["files", "file", "changes", "diff", "browser", "terminal", "tasks", "pr"],
                    "description": "file: preview one file (path, line); files: the tree (path reveals); diff: one file's diff (path, scope); changes: the list (scope)."},
                "path": {"type": "string", "description": "A file, absolute or project-relative."},
                "line": {"type": "integer", "description": "The line to show (file)."},
                "scope": {"type": "string", "description": SCOPE},
            }}),
        ),
        def(
            "changes_read",
            "What changed, for you to read and answer from (to put it on the person's screen use pane_show) — use this \
             instead of git diff or git status: a diff stat (files, \
             +added −removed) by default, one file's diff when file is given. Scopes include this session, the last turn and \
             turn N, which plain git cannot tell apart. Use for \"what did the last turn change?\", \"what have you changed \
             in this session?\", \"what changed in turn 2?\". Read-only.",
            json!({"type": "object", "properties": {
                "scope": {"type": "string", "description": SCOPE},
                "file": {"type": "string", "description": "One file: its diff instead of the stat."},
            }}),
        ),
        def(
            "changes_revert",
            "Undo a change to one file through the pane's Revert — use this instead of git checkout or git restore: back to \
             HEAD (scope all) or out of one turn (last_turn, turn_<n>). The person confirms first; the answer says reverted, \
             declined, or waiting for the person. Use for \"undo your change to X\", \"revert X\", \"take back what the last \
             turn did to X\".",
            json!({"type": "object", "required": ["path"], "properties": {
                "path": {"type": "string"},
                "scope": {"type": "string", "description": SCOPE},
            }}),
        ),
        def(
            "changes_comment",
            "Leave a review comment on a line of a file; the person sees it in Changes under that file's diff. Use for \"comment \
             on line N of X\", review notes.",
            json!({"type": "object", "required": ["path", "line", "text"], "properties": {
                "path": {"type": "string"},
                "line": {"type": "integer"},
                "text": {"type": "string"},
            }}),
        ),
        def(
            "changes_commit",
            "Commit — use this instead of running git commit or git push yourself, so the person's Changes pane and pull \
             request card stay in step: the files ticked in the commit box (by default what this session changed), with your \
             message, then push when push is true (the person approves a push unless the session has full access). Use for \
             \"commit this\", \"commit and push this\", \"commit with the message X\".",
            json!({"type": "object", "properties": {
                "message": {"type": "string", "description": "The commit message. Default: the one in the box."},
                "push": {"type": "boolean", "description": "Push the branch after committing."},
            }}),
        ),
        def(
            "pr_status",
            "The pull request for the current branch — use this instead of gh: state, checks (failing ones named) and review \
             comments. Use for \"did CI pass?\", \"which checks fail?\", \"any review comments on the PR?\", \"show me the pull \
             request\". Read-only.",
            json!({"type": "object", "properties": {}}),
        ),
    ]
}

/// Register the six tools, each forwarded over the terminal socket.
pub fn register_pane_tools(registry: &mut ToolRegistry, target: &TerminalTarget) {
    for (name, description, schema) in tool_defs() {
        let target = target.clone();
        let tool = name.clone();
        registry
            .register(name, description, schema, move |args| {
                super::terminal::forward_call_with(&target.socket, &target.session, target.token.as_deref(), &tool, &args)
            })
            .expect("fresh registry takes the pane tools");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_tools_are_declared_in_order_with_their_phrases() {
        let defs = tool_defs();
        let names: Vec<&str> = defs.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(names, PANE_TOOL_NAMES);
        let show = &defs[0].1;
        for phrase in ["open X in the preview", "show me the diff in X", "switch to the terminal"] {
            assert!(show.contains(phrase), "pane_show names {phrase:?}");
        }
        assert!(defs[1].1.contains("what did the last turn change"));
        assert!(defs[4].1.contains("commit and push"));
        assert_eq!(defs[3].2["required"], json!(["path", "line", "text"]));
    }
}
