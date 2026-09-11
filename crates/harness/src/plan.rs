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

use aui_protocol::PlanSection;

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

/// Whether [`PREAMBLE_ENV`] is set. Read once (finding `support-17`): the
/// variable cannot change mid-run, and the old code paid an environment
/// lookup on every prompt sent in plan mode.
fn preamble_env_set() -> bool {
    static SET: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *SET.get_or_init(|| std::env::var_os(PREAMBLE_ENV).is_some())
}

/// The model-visible text for a prompt sent in plan mode.
///
/// The slash form, because the probe proved the skill fires on it; the preamble
/// form under [`PREAMBLE_ENV`], because it is the only thing that would still
/// work without the skill and a fallback nobody can reach is not a fallback.
pub fn prefix(text: &str) -> String {
    if preamble_env_set() {
        format!("{PREAMBLE}{text}")
    } else {
        format!("{PLAN_COMMAND}{text}")
    }
}

/// What Accept sends once the person approves the plan.
pub const ACCEPT_PROMPT: &str = "Implement the plan above.";

/// Turn a plan reply into the steps and section labels a `Block::Plan` renders.
///
/// The skill answers in markdown, and the shape it uses has **two** levels:
/// headings that group the work, and list items that are the work. Folding both
/// into one numbered list, as this used to, numbered the headings as if they
/// were steps — "3. Prove it" is not something anybody does (finding F6). So
/// headings become unnumbered section labels and only list items are counted.
///
/// A reply with neither — the one-line plan the probe got back is exactly that
/// — degrades to its paragraphs, because an empty plan card would be worse than
/// a one-item one. A heading with no items under it is dropped: a label with
/// nothing to label is a heading the model wrote for itself.
pub fn steps(reply: &str) -> (Vec<String>, Vec<PlanSection>) {
    let mut items: Vec<String> = Vec::new();
    let mut sections: Vec<PlanSection> = Vec::new();
    // Every heading label seen, itemless or not — kept alongside `sections`
    // (which drops an itemless one) so the all-headings fallback below has a
    // clean label to show instead of the heading's raw markdown line
    // (finding `support-18`).
    let mut headings: Vec<String> = Vec::new();
    for line in reply.lines() {
        let line = line.trim();
        if let Some(rest) = heading(line) {
            // A heading replaces an earlier one that gathered no items rather
            // than stacking on it: the later heading is the one in force.
            sections.retain(|s: &PlanSection| s.first_item < items.len());
            sections.push(PlanSection { label: rest.to_owned(), first_item: items.len() });
            headings.push(rest.to_owned());
        } else if let Some(rest) = list_item(line) {
            items.push(rest.to_owned());
        }
    }
    // A trailing heading covers nothing at all.
    sections.retain(|s| s.first_item < items.len());
    if items.is_empty() {
        sections.clear();
        items = if headings.is_empty() {
            reply
                .split("\n\n")
                .map(|p| p.trim().replace('\n', " "))
                .filter(|p| !p.is_empty())
                .collect()
        } else {
            // A headings-only reply: the section labels are the only
            // structure it has, so they become the steps instead of each
            // heading's raw `## ` line being rendered as its own paragraph.
            headings
        };
    }
    items.retain(|s| !s.is_empty());
    (items, sections)
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
    fn headings_become_sections_and_list_items_become_the_numbered_steps() {
        let reply = "## Read the code\nsome prose\n\n1. Change the validator\n2) Run the tests\n\n## Prove it\n- Update the docs";
        let (items, sections) = steps(reply);
        assert_eq!(items, vec!["Change the validator", "Run the tests", "Update the docs"]);
        assert_eq!(
            sections,
            vec![
                PlanSection { label: "Read the code".into(), first_item: 0 },
                PlanSection { label: "Prove it".into(), first_item: 2 },
            ]
        );
    }

    #[test]
    fn a_heading_with_nothing_under_it_is_dropped() {
        let (items, sections) = steps("## Read the code\n## Change it\n- do the thing\n## Later");
        assert_eq!(items, vec!["do the thing"]);
        assert_eq!(sections, vec![PlanSection { label: "Change it".into(), first_item: 0 }]);
    }

    #[test]
    fn a_plan_with_no_structure_falls_back_to_paragraphs() {
        let reply = "**Plan:** Print `hello` to stdout.\n\nNo file was created.";
        let (items, sections) = steps(reply);
        assert_eq!(items, vec!["**Plan:** Print `hello` to stdout.", "No file was created."]);
        assert!(sections.is_empty());
    }

    #[test]
    fn a_hash_with_no_space_is_not_a_heading() {
        assert_eq!(steps("#tag only").0, vec!["#tag only"]);
    }

    /// **support-18 / A-MECH-22.** A headings-only reply has no list items at
    /// all, so every heading is itemless and `sections` ends up empty too —
    /// the old paragraph-split fallback then rendered each heading's raw
    /// `## ` line as a numbered step. The clean labels are used instead.
    #[test]
    fn an_all_headings_reply_uses_the_labels_not_the_raw_markdown_lines() {
        let reply = "## Read the code\n\n## Prove it\n\n## Ship it";
        let (items, sections) = steps(reply);
        assert_eq!(items, vec!["Read the code", "Prove it", "Ship it"]);
        assert!(sections.is_empty(), "an itemless heading is not a section");
    }
}
