//! The orchestration relay (docs/85 Stage 14, D-Q6): a lead session hands
//! work to worker sessions on any provider, and workers report back, as MCP
//! tools forwarded to the app's socket — same routing, same token, same
//! absence rule. Who calls is the bridge's session, never the agent's word.

use serde_json::{Value, json};

use crate::terminal::TerminalTarget;
use crate::ToolRegistry;

/// Steering for the model.
pub const INSTRUCTIONS: &str = "To split work across agents — \"write with Sonnet, review with Codex\", \"have \
    another agent do X\", \"run these in parallel\" — use delegate (agents_available says who can take a task); \
    give each task 2-5 acceptance criteria drawn from the person's words and show them in your message. You are \
    woken when your tasks settle; task_status shows them meanwhile. If you were given a task, finish by \
    calling report, or escalate when blocked. To start a session the person will drive themselves (\"open a new \
    session that…\"), use session_new: it waits for them, unsent.";

/// The tools, in contract order.
pub const ORCHESTRA_TOOL_NAMES: [&str; 12] = [
    "agents_available",
    "delegate",
    "task_status",
    "task_send",
    "task_stop",
    "task_wait",
    "task_land",
    "session_send",
    "session_watch",
    "session_new",
    "report",
    "escalate",
];

/// One tool's advertisement: name, description, JSON Schema.
pub fn tool_defs() -> Vec<(String, String, Value)> {
    let def = |name: &str, description: &str, schema: Value| (name.to_owned(), description.to_owned(), schema);
    let task = json!({"type": "object", "required": ["task"], "properties": {"task": {"type": "string", "description": "A task id from delegate."}}});
    vec![
        def(
            "agents_available",
            "Which agents can take a task now: each provider (Claude Code, Codex, Muse, …), whether it is signed in, its models, how it bills and whether it can be steered mid-turn; and the limits. Read-only.",
            json!({"type": "object", "properties": {}}),
        ),
        def(
            "delegate",
            "Hand a task to another agent, which works in its own session the person can watch: \"write this with Sonnet\", \"have Codex review it\", \"use Muse to import them\", \"do these in parallel\". A writer gets its own worktree; a reviewer reads, changing nothing and writing no file: its findings come back in its report, which a task chained after it receives. Give 2-5 acceptance criteria from the person's words; Baaz checks the work before it counts as done. Use after to chain tasks (the next starts when the previous passes, with its report and branch). You are woken once when your tasks settle.",
            json!({"type": "object", "required": ["task", "provider"], "properties": {
                "task": {"type": "string", "description": "What the worker should do, complete enough to act on alone."},
                "title": {"type": "string", "description": "A few words for the sidebar."},
                "provider": {"type": "string", "description": "claude-code, codex, muse, … (agents_available)."},
                "model": {"type": "string", "description": "A model the provider knows; omit for its default."},
                "role": {"type": "string", "enum": ["writer", "reviewer", "helper"], "description": "Default writer."},
                "criteria": {"type": "array", "items": {"type": "string"}, "minItems": 1, "maxItems": 5, "description": "2-5 acceptance criteria, frozen once given."},
                "check": {"type": "string", "description": "A command that must exit 0 (default: the project's tests, found by Baaz)."},
                "after": {"type": "array", "items": {"type": "string"}, "description": "Task ids that must pass first."},
                "inputs": {"type": "array", "items": {"type": "string"}, "description": "Files, notes or facts the worker needs."},
                "scope": {"type": "array", "items": {"type": "string"}, "description": "Paths a writer's change may touch (none for a reviewer)."},
                "budget": {"type": "object", "properties": {"tokens": {"type": "integer"}, "minutes": {"type": "integer"}}},
            }}),
        ),
        def(
            "task_status",
            "Your tasks (or one): state, worker, minutes, what it is doing now, its report and Baaz's verdict. Read-only.",
            json!({"type": "object", "properties": {"task": {"type": "string"}}}),
        ),
        def(
            "task_send",
            "Send a task's worker a message: steered into its running turn where the provider allows, else queued for its next. It reads as from you (an agent), not from the person.",
            json!({"type": "object", "required": ["task", "text"], "properties": {"task": {"type": "string"}, "text": {"type": "string"}}}),
        ),
        def("task_stop", "Stop a task: its worker's turn ends and the task is stopped.", task.clone()),
        def(
            "task_wait",
            "Wait briefly (at most 60 seconds) for a task to settle; prefer ending your turn — you are woken when your tasks settle. Read-only.",
            json!({"type": "object", "required": ["task"], "properties": {"task": {"type": "string"}, "seconds": {"type": "integer"}}}),
        ),
        def(
            "task_land",
            "Merge passed tasks' branches into this branch one at a time, running the check after each; stops at a conflict with the files named (never resolves it). Each worktree is saved to a ref before it is removed.",
            json!({"type": "object", "required": ["tasks"], "properties": {"tasks": {"type": "array", "items": {"type": "string"}}}}),
        ),
        def(
            "session_send",
            "Send another Baaz session a message (by session id from session_search or task_status): steered where its provider allows, else queued; an idle session starts a turn. It reads as from you (an agent), not the person.",
            json!({"type": "object", "required": ["session", "text"], "properties": {"session": {"type": "string"}, "text": {"type": "string"}}}),
        ),
        def(
            "session_watch",
            "Treat existing sessions as your tasks (\"these three sessions, then land and push\"): Baaz checks and lands them like delegated work.",
            json!({"type": "object", "required": ["sessions"], "properties": {"sessions": {"type": "array", "items": {"type": "string"}}, "check": {"type": "string"}}}),
        ),
        def(
            "session_new",
            "Open a new session for the person with your prompt waiting in its composer, unsent: \"start a new session that…\", \"open a session to do X\", \"make a new session asking the agent to…\". It gets its own row in the sidebar, in this project; the person stays where they are, opens it, reviews the prompt and sends it. Not for work you need back (that is delegate). Answers the new session's id.",
            json!({"type": "object", "required": ["prompt"], "properties": {
                "prompt": {"type": "string", "description": "The prompt, complete enough for the new session to act on alone."},
                "title": {"type": "string", "description": "A few words for the sidebar."},
                "provider": {"type": "string", "description": "claude-code, codex, muse, … (agents_available); omit for your own."},
                "model": {"type": "string", "description": "A model the provider knows; omit for its default."},
            }}),
        ),
        def(
            "report",
            "Finish the task you were given: your result and, for each acceptance criterion, how you met it. Baaz then checks the work. Only for a session that was given a task.",
            json!({"type": "object", "required": ["result"], "properties": {
                "result": {"type": "string"},
                "criteria": {"type": "array", "items": {"type": "object", "properties": {"criterion": {"type": "string"}, "met": {"type": "boolean"}, "evidence": {"type": "string"}}}},
            }}),
        ),
        def(
            "escalate",
            "Ask for a decision you cannot make on the task you were given: it shows as a question to the person and your lead, and your task pauses until answered.",
            json!({"type": "object", "required": ["question"], "properties": {"question": {"type": "string"}, "options": {"type": "array", "items": {"type": "string"}}}}),
        ),
    ]
}

/// Register the tools, each forwarded over the terminal socket.
pub fn register_orchestra_tools(registry: &mut ToolRegistry, target: &TerminalTarget) {
    for (name, description, schema) in tool_defs() {
        let target = target.clone();
        let tool = name.clone();
        registry
            .register(name, description, schema, move |args| {
                super::terminal::forward_call_with(&target.socket, &target.session, target.token.as_deref(), &tool, &args)
            })
            .expect("fresh registry takes the orchestration tools");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tools_are_declared_in_order_with_their_phrases() {
        let defs = tool_defs();
        let names: Vec<&str> = defs.iter().map(|(name, _, _)| name.as_str()).collect();
        assert_eq!(names, ORCHESTRA_TOOL_NAMES);
        assert!(defs[1].1.contains("have Codex review it") && defs[1].1.contains("in parallel"));
        assert_eq!(defs[1].2["properties"]["criteria"]["maxItems"], 5);
    }
}
