//! The todo status vocabulary, one mapping for every wire spelling.

use aui_protocol::TodoState;

/// A wire's task status onto the transcript's: Codex's plan steps
/// (`inProgress`), Claude Code's tasks (`in_progress`) and Muse's todo
/// arguments all say the same three things.
pub fn todo_state(status: &str) -> TodoState {
    match status {
        "inProgress" | "in_progress" => TodoState::Running,
        "completed" => TodoState::Done,
        _ => TodoState::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_maps_to_the_same_three_states() {
        assert_eq!(todo_state("inProgress"), TodoState::Running);
        assert_eq!(todo_state("in_progress"), TodoState::Running);
        assert_eq!(todo_state("completed"), TodoState::Done);
        assert_eq!(todo_state("pending"), TodoState::Pending);
    }
}
