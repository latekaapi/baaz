//! Plan mode: client-side, and flagged as such (spec §3.1).
//!
//! MSP has no plan mode. What Muse has is a **skill** called `plan`, and the
//! Phase 3 probe settled how it is reached: sending the literal text
//! `/plan <prompt>` as an ordinary `turn/start` text part makes the server read
//! the skill and answer in its shape. The evidence, from one real `meta` turn
//! (`fixtures/msp/transcript-plan-probe.jsonl`):
//!
//! ```text
//! item/started   toolCall read_skill {"name":"bundled:plan"}
//! item/completed agentMessage "**Plan:** Print `hello` to stdout … Reply
//!                              Approve, Request changes, or Cancel."
//! ```
//!
//! So plan mode sends [`prefix`]ed text with `displayText` set to what the
//! person typed, and puts the session in `denyUnmatched` for the duration.
//! [`PREAMBLE`] is the fallback the spec named for the case where the skill did
//! **not** fire; it stays because it is the only thing that would still work if
//! a later Muse build drops the skill, and [`PREAMBLE_ENV`] is how to reach it
//! without a rebuild.

/// What plan mode prefixes the model-visible text with.
///
/// The `/plan` slash form, because the probe proved the skill fires on it.
pub const PLAN_COMMAND: &str = "/plan ";

/// The fallback preamble, used only if the skill stops firing.
pub const PREAMBLE: &str = "Create a grounded, decision-complete plan for the request below, then \
stop and wait for approval. Do not edit files or run commands that change state.\n\n";

/// The environment escape hatch: set it to use [`PREAMBLE`] instead of the
/// slash form, for the day a Muse build stops shipping the skill.
pub const PREAMBLE_ENV: &str = "HARNESS_PLAN_PREAMBLE";

/// The model-visible text for a prompt sent in plan mode.
///
/// The slash form, because the probe proved the skill fires on it; the preamble
/// form under [`PREAMBLE_ENV`], because it is the only thing that would still
/// work without the skill and a fallback nobody can reach is not a fallback.
pub fn prefix(text: &str) -> String {
    match std::env::var_os(PREAMBLE_ENV) {
        Some(_) => format!("{PREAMBLE}{text}"),
        None => format!("{PLAN_COMMAND}{text}"),
    }
}

/// What Accept sends once the person approves the plan.
pub const ACCEPT_PROMPT: &str = "Implement the plan above.";

/// Turn a plan reply into the steps a `Block::Plan` renders.
///
/// The skill answers in markdown, and the shape it uses is headings and list
/// items, so those are the steps. A reply with neither — the one-line plan the
/// probe got back is exactly that — degrades to its paragraphs, because an
/// empty plan card would be worse than a one-item one.
pub fn steps(reply: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        if let Some(rest) = heading(line) {
            out.push(rest.to_owned());
        } else if let Some(rest) = list_item(line) {
            out.push(rest.to_owned());
        }
    }
    if out.is_empty() {
        out = reply
            .split("\n\n")
            .map(|p| p.trim().replace('\n', " "))
            .filter(|p| !p.is_empty())
            .collect();
    }
    out.retain(|s| !s.is_empty());
    out
}

/// `## Step one` → `Step one`.
fn heading(line: &str) -> Option<&str> {
    let rest = line.trim_start_matches('#');
    if rest.len() < line.len() && rest.starts_with(' ') {
        Some(rest.trim())
    } else {
        None
    }
}

/// `- do it`, `* do it`, `1. do it`, `1) do it` → `do it`.
fn list_item(line: &str) -> Option<&str> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }
    let digits: String = line.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &line[digits.len()..];
    for marker in [". ", ") "] {
        if let Some(rest) = rest.strip_prefix(marker) {
            return Some(rest.trim());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slash_form_is_what_the_probe_proved() {
        assert_eq!(prefix("do the thing"), "/plan do the thing");
    }

    #[test]
    fn the_preamble_is_a_full_sentence_before_the_prompt() {
        assert!(PREAMBLE.ends_with("\n\n"));
        assert!(PREAMBLE.starts_with("Create a grounded"));
    }

    #[test]
    fn headings_and_list_items_become_steps() {
        let reply = "## Read the code\nsome prose\n\n1. Change the validator\n2) Run the tests\n- Update the docs";
        assert_eq!(
            steps(reply),
            vec!["Read the code", "Change the validator", "Run the tests", "Update the docs"]
        );
    }

    #[test]
    fn a_plan_with_no_structure_falls_back_to_paragraphs() {
        let reply = "**Plan:** Print `hello` to stdout.\n\nNo file was created.";
        assert_eq!(steps(reply), vec!["**Plan:** Print `hello` to stdout.", "No file was created."]);
    }

    #[test]
    fn a_hash_with_no_space_is_not_a_heading() {
        assert_eq!(steps("#tag only"), vec!["#tag only"]);
    }
}
