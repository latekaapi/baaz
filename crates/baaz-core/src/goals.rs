//! Goals (docs/85 Stage 15, D-Q8): "keep going until the tests pass" on
//! every provider, with one pill. The pure half: what a message asks for,
//! the goal's record, its limits, the usage-limit wait, its progress note.
//!
//! * **Detection is a fixed phrase list** ([`detect`]): a clear end
//!   ("until …", "keep going …", "don't stop until …", "all N …") sets the
//!   goal; a weaker one ("make … pass", "get … working") only offers a chip
//!   and sets nothing by itself.
//! * **The goal names its check** ([`Goal::native_text`]): "<condition>
//!   (verified by `<command>` exiting 0)" — the command found by 14.5b, which
//!   Baaz runs itself when the provider says the goal is met (Codex), or
//!   after each goal turn (Claude: nothing reaches stream-json; Muse and ACP:
//!   Baaz's own evaluator, then the check).
//! * **Limits scale to the plan** ([`limits_for`]): 3 hours or 5M tokens on a
//!   subscription, 30 minutes or 500k on per-token billing; reaching one
//!   pauses and asks in one line. A usage-limit wait resumes by itself at the
//!   reset ([`resume_at`]).
//! * **One record per session** in baaz.db's `goal` table
//!   ([`crate::db::goals`]): what the pill shows, what a relaunch resends
//!   (15.6, once: [`Goal::resent`]), and what the progress note says (15.7).
//!
//! Keyed by session id: `Goal::session`.

use serde::{Deserialize, Serialize};

/// What a message asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Detected {
    /// A clear end: set the goal.
    Set(String),
    /// A possible end: offer "Keep going until …?" and set nothing.
    Offer(String),
    None,
}

/// The phrases that set a goal, longest first (each is matched at a word
/// boundary, case-insensitive). The condition is what follows.
const SET_PHRASES: [&str; 6] = ["don't stop until", "do not stop until", "keep going until", "keep working until", "until", "keep going"];
/// Phrases that only offer a chip.
const OFFER_PHRASES: [&str; 3] = ["make ", "get ", "fix "];
const OFFER_ENDS: [&str; 4] = [" pass", " passing", " working", " green"];

fn trim_condition(text: &str) -> String {
    text.trim().trim_start_matches([',', ':', '-']).trim().trim_end_matches(['.', '!', '?']).trim().to_owned()
}

/// What `message` asks for (see the module doc).
pub fn detect(message: &str) -> Detected {
    let lower = message.to_lowercase();
    for phrase in SET_PHRASES {
        if let Some(at) = find_word(&lower, phrase) {
            let rest = trim_condition(&message[at + phrase.len()..]);
            // "keep going" alone carries no condition: it still needs one.
            if phrase == "keep going" && rest.is_empty() {
                continue;
            }
            if !rest.is_empty() {
                return Detected::Set(rest);
            }
        }
    }
    // "all 15 imported", "all N questions"
    if let Some(at) = find_word(&lower, "all ") {
        let rest = &lower[at + 4..];
        if rest.split_whitespace().next().is_some_and(|w| w.chars().all(|c| c.is_ascii_digit())) {
            return Detected::Set(trim_condition(&message[at..]));
        }
    }
    for start in OFFER_PHRASES {
        if let Some(at) = find_word(&lower, start.trim_end()) {
            let rest = &lower[at..];
            if OFFER_ENDS.iter().any(|end| rest.contains(end)) {
                return Detected::Offer(trim_condition(&message[at..]));
            }
        }
    }
    Detected::None
}

/// `phrase` in `text` at a word boundary.
fn find_word(text: &str, phrase: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find(phrase) {
        let at = from + i;
        let before = text[..at].chars().next_back();
        let after = text[at + phrase.len()..].chars().next();
        let boundary_before = before.is_none_or(|c| !c.is_alphanumeric());
        let boundary_after = phrase.ends_with(' ') || after.is_none_or(|c| !c.is_alphanumeric());
        if boundary_before && boundary_after {
            return Some(at);
        }
        from = at + phrase.len();
    }
    None
}

/// Where a goal stands: the pill's states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalState {
    Running,
    Met,
    Paused,
    Blocked,
    /// Waiting for a usage limit to reset.
    UsageWait,
    /// A time or token ceiling was reached.
    BudgetReached,
    Stopped,
}

impl GoalState {
    pub fn word(self) -> &'static str {
        match self {
            Self::Running => "Working",
            Self::Met => "Met",
            Self::Paused => "Paused",
            Self::Blocked => "Blocked",
            Self::UsageWait => "Waiting for the usage limit",
            Self::BudgetReached => "Budget reached",
            Self::Stopped => "Stopped",
        }
    }

    /// Whether the goal is still being pursued (keep-awake holds).
    pub fn active(self) -> bool {
        matches!(self, Self::Running | Self::UsageWait)
    }

    /// The table's state column.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Met => "met",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageWait => "usage_wait",
            Self::BudgetReached => "budget_reached",
            Self::Stopped => "stopped",
        }
    }
}

/// A goal's ceilings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub minutes: u32,
    pub tokens: u64,
}

/// The default ceilings for a plan (docs/85 15.4).
pub fn limits_for(billing: provider::BillingMode) -> Limits {
    match billing {
        provider::BillingMode::PerToken => Limits { minutes: 30, tokens: 500_000 },
        _ => Limits { minutes: 180, tokens: 5_000_000 },
    }
}

/// One goal: a session's, at most one at a time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub session: String,
    pub provider: String,
    pub condition: String,
    /// The check found for it (14.5b), as a command; `None`: changed files parse.
    pub check: Option<String>,
    pub state: GoalState,
    /// Whether the provider holds the goal natively (Claude `/goal`, Codex
    /// `thread/goal/set`) or Baaz's evaluator judges it (Muse, ACP).
    pub native: bool,
    pub started_at: i64,
    pub turns: u32,
    pub tokens: u64,
    /// The last check's one line: `cargo test: 3 failing`, `passed`.
    pub last_check: Option<String>,
    pub limits: Limits,
    /// Set while waiting for a usage limit: when to resume (ms).
    pub resume_at: Option<i64>,
    /// Why it paused or blocked, in one line.
    pub reason: Option<String>,
    /// Turns since the last progress note (15.7).
    pub turns_since_note: u32,
    /// The stall rules' last step and when it was taken (14.7 applies to
    /// goal sessions, 15.4).
    #[serde(default)]
    pub stall: Option<(u8, i64)>,
    /// The last turn counted, so a finish heard twice counts once.
    #[serde(default)]
    pub last_turn: Option<String>,
    /// A relaunch already resent the interrupted turn (15.6: once).
    #[serde(default)]
    pub resent: bool,
    /// When the record last changed (ms).
    #[serde(default)]
    pub updated_at: i64,
}

impl Goal {
    pub fn new(session: &str, provider: &str, condition: &str, check: Option<String>, native: bool, limits: Limits, now: i64) -> Self {
        Self {
            session: session.to_owned(),
            provider: provider.to_owned(),
            condition: condition.to_owned(),
            check,
            state: GoalState::Running,
            native,
            started_at: now,
            turns: 0,
            tokens: 0,
            last_check: None,
            limits,
            resume_at: None,
            reason: None,
            turns_since_note: 0,
            stall: None,
            last_turn: None,
            resent: false,
            updated_at: now,
        }
    }

    /// What the provider's native goal is set to: the condition and how
    /// Baaz will verify it.
    pub fn native_text(&self) -> String {
        match &self.check {
            Some(command) => format!("{} (verified by `{command}` exiting 0)", self.condition),
            None => self.condition.clone(),
        }
    }

    /// The message that keeps a goal going after a turn ends short of it:
    /// the failing check's tail when there is one.
    pub fn continue_text(&self, failing: Option<(&str, &str)>) -> String {
        match failing {
            Some((command, tail)) => format!(
                "Baaz ran `{command}` and it fails:\n```\n{tail}\n```\nKeep going until the goal is met: {}",
                self.condition
            ),
            None => format!("Keep going until the goal is met: {}", self.condition),
        }
    }

    /// The stall rules' nudge for a goal session (14.7, 15.4).
    pub fn nudge_text(&self) -> String {
        format!("From Baaz (not the user): you have been quiet for a while. Carry on toward the goal: {}", self.condition)
    }

    /// A turn of the goal finished with `tokens` spent.
    pub fn count_turn(&mut self, tokens: u64) {
        self.turns += 1;
        self.turns_since_note += 1;
        self.tokens += tokens;
    }

    /// The ceiling reached at `now`, if any, in one line.
    pub fn over_limit(&self, now: i64) -> Option<String> {
        let minutes = ((now - self.started_at).max(0) / 60_000) as u32;
        if minutes >= self.limits.minutes {
            return Some(format!("{minutes} minutes of the goal's {}", self.limits.minutes));
        }
        (self.tokens >= self.limits.tokens).then(|| format!("{} tokens of the goal's {}", self.tokens, self.limits.tokens))
    }

    /// Time on the goal as the pill shows it: `12m`, `1h 4m` — up to now
    /// while it is pursued, up to its last change once it settled.
    pub fn elapsed(&self, now: i64) -> String {
        let end = if self.state.active() { now } else { self.updated_at };
        let m = (end - self.started_at).max(0) / 60_000;
        if m < 60 { format!("{m}m") } else { format!("{}h {}m", m / 60, m % 60) }
    }

    /// Whether a progress note is due (every 10 turns, and on a pause).
    pub fn note_due(&self) -> bool {
        self.turns_since_note >= PROGRESS_EVERY || matches!(self.state, GoalState::Paused | GoalState::BudgetReached | GoalState::Blocked)
    }
}

/// Turns between progress notes (docs/85 15.7).
pub const PROGRESS_EVERY: u32 = 10;

/// The progress note's title: one per goal, replaced each time.
pub fn progress_title(condition: &str) -> String {
    let short: String = condition.chars().take(60).collect();
    format!("Goal progress · {short}")
}

/// The progress note's body: done, left, how to check.
pub fn progress_body(goal: &Goal, done: &str, left: &str) -> String {
    let check = goal.check.as_deref().map(|c| format!("`{c}` exits 0")).unwrap_or_else(|| "the changed files parse".into());
    format!(
        "Goal: {}\nDone so far: {done}\nLeft: {left}\nHow to check: {check}\nTurns: {} · tokens: {} · last check: {}",
        goal.condition,
        goal.turns,
        goal.tokens,
        goal.last_check.as_deref().unwrap_or("not run yet")
    )
}

/// When a usage-limit wait ends: the reset the provider reported (ms), a
/// minute after it so the window has rolled; with no reset known, in an hour.
pub fn resume_at(reset_ms: Option<i64>, now: i64) -> i64 {
    match reset_ms {
        Some(reset) if reset > now => reset + 60_000,
        _ => now + 60 * 60_000,
    }
}

/// Whether a failed turn's message means a usage limit (15.4): the goal
/// then waits for the reset rather than failing.
pub fn is_usage_limit(message: &str) -> bool {
    let m = message.to_lowercase();
    ["usage limit", "rate limit", "limit reached", "quota", "too many requests", "hit your limit"].iter().any(|w| m.contains(w))
}

/// When the exhausted usage window resets (ms), from a reading's windows
/// as `(used fraction, resets at in Unix seconds)`: the fullest window's.
pub fn usage_reset_ms(windows: &[(f64, Option<i64>)]) -> Option<i64> {
    windows
        .iter()
        .filter_map(|(used, at)| at.map(|at| (*used, at)))
        .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(_, at)| at * 1000)
}

/// What Baaz's evaluator asks a cheap model after a goal turn on a provider
/// with no native goal (Muse, ACP): yes or no, from the turn's last words.
pub fn evaluator_prompt(condition: &str, last_reply: &str) -> String {
    let reply: String = last_reply.chars().rev().take(4000).collect::<Vec<_>>().into_iter().rev().collect();
    format!(
        "An agent is working toward this goal:\n<goal>{condition}</goal>\nIts latest reply ends:\n<reply>{reply}</reply>\n\
         Answer with one word: MET if the reply shows the goal is met, otherwise CONTINUE."
    )
}

/// The evaluator's answer read: met or not.
pub fn evaluator_says_met(answer: &str) -> bool {
    answer.trim().to_uppercase().starts_with("MET")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phrases_set_a_goal_and_weaker_ones_offer_a_chip() {
        assert_eq!(detect("Keep going until the tests pass."), Detected::Set("the tests pass".into()));
        assert_eq!(detect("Fix the parser and don't stop until cargo test is green"), Detected::Set("cargo test is green".into()));
        assert_eq!(detect("Import them, all 15 imported please"), Detected::Set("all 15 imported please".into()));
        assert_eq!(detect("keep going"), Detected::None, "no condition, nothing to set");
        assert_eq!(detect("Make the login test pass"), Detected::Offer("Make the login test pass".into()));
        assert_eq!(detect("What does untilDone do?"), Detected::None, "a word inside another is not a phrase");
        assert_eq!(detect("Tell me about the release"), Detected::None);
    }

    #[test]
    fn limits_scale_to_the_plan_and_pause_at_the_ceiling() {
        assert_eq!(limits_for(provider::BillingMode::Subscription), Limits { minutes: 180, tokens: 5_000_000 });
        assert_eq!(limits_for(provider::BillingMode::PerToken), Limits { minutes: 30, tokens: 500_000 });
        let mut g = Goal::new("s", "codex", "tests pass", Some("cargo test".into()), true, limits_for(provider::BillingMode::PerToken), 0);
        assert!(g.over_limit(29 * 60_000).is_none());
        assert!(g.over_limit(30 * 60_000).unwrap().contains("30 minutes"));
        g.count_turn(500_000);
        assert!(g.over_limit(0).unwrap().contains("tokens"));
        assert_eq!(g.native_text(), "tests pass (verified by `cargo test` exiting 0)");
        assert_eq!(g.elapsed(65 * 60_000), "1h 5m");
        g.state = GoalState::Met;
        g.updated_at = 5 * 60_000;
        assert_eq!(g.elapsed(65 * 60_000), "5m", "a settled goal's time stops");
    }

    #[test]
    fn a_usage_wait_resumes_after_the_reset() {
        assert_eq!(resume_at(Some(10_000_000), 1_000), 10_060_000);
        assert_eq!(resume_at(Some(500), 1_000), 1_000 + 3_600_000, "a reset in the past: an hour");
        assert_eq!(resume_at(None, 0), 3_600_000);
    }

    #[test]
    fn a_progress_note_every_ten_turns_and_on_a_pause() {
        let mut g = Goal::new("s", "muse", "all 15 imported", None, false, limits_for(provider::BillingMode::Subscription), 0);
        for _ in 0..9 {
            g.count_turn(1);
        }
        assert!(!g.note_due());
        g.count_turn(1);
        assert!(g.note_due());
        g.turns_since_note = 0;
        g.state = GoalState::Paused;
        assert!(g.note_due());
        assert_eq!(progress_title(&g.condition), "Goal progress · all 15 imported");
        assert!(progress_body(&g, "12 imported", "3 left", ).contains("How to check: the changed files parse"));
    }

    #[test]
    fn a_usage_limit_is_read_from_the_failure_and_waits_for_the_fullest_window() {
        assert!(is_usage_limit("You've hit your usage limit. Try again at 9pm"));
        assert!(!is_usage_limit("the build failed"));
        assert_eq!(usage_reset_ms(&[(0.4, Some(100)), (1.0, Some(50)), (0.9, None)]), Some(50_000));
        assert_eq!(usage_reset_ms(&[]), None);
    }

    #[test]
    fn the_evaluator_reads_met_or_continue() {
        assert!(evaluator_says_met(" met"));
        assert!(!evaluator_says_met("CONTINUE"));
        assert!(evaluator_prompt("x", "y").contains("<goal>x</goal>"));
    }
}
