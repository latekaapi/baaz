//! Automatic compaction (docs/85 12.1): one pure function decides.
//!
//! A long session is compacted by Baaz, without the person asking, when
//! all of these hold:
//!
//! * its context is over [`THRESHOLD_TOKENS`];
//! * no turn is running and nothing is pending — an approval, a question,
//!   a queued message, a running task;
//! * no key has been pressed in its composer since its last turn ended;
//! * the account's prompt cache lasts an hour (a subscription), and the
//!   moment is [`LEAD`] before it expires — a compaction sent then rewrites
//!   the context while the cache still pays for reading it. A 5-minute
//!   cache (per-token billing) is left to the vendor's own
//!   auto-compaction, and so is a provider that compacts idle sessions
//!   itself (claude ≥ 2.1.290's `idleCompaction`).
//!
//! It runs once per idle stretch. The person typing before it is sent
//! cancels it; once sent, their next message queues behind it like any
//! message sent while a turn runs. The row it leaves is 10.4's "Context
//! compacted · 412k → 38k": no notification, no unread mark.
//!
//! A topic switch above the threshold is decided by a utility call with
//! [`TOPIC_CHECK_LIMIT`]; on a timeout the message goes as it is
//! ([`topic_switch`]).

use std::time::Duration;

use crate::billing::BillingMode;

/// The context, in tokens, above which a session is worth compacting.
pub const THRESHOLD_TOKENS: u64 = 150_000;

/// How long before the prompt cache expires the compaction is sent.
pub const LEAD: Duration = Duration::from_secs(120);

/// The prompt cache's life on a subscription.
pub const SUBSCRIPTION_CACHE: Duration = Duration::from_secs(3600);

/// The topic check's time limit: past it, the message goes as it is.
pub const TOPIC_CHECK_LIMIT: Duration = Duration::from_secs(2);

/// What the decision reads, all of it plain data.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Inputs {
    /// The session's context, in tokens, as the provider last reported it.
    pub context_tokens: Option<u64>,
    /// A turn is running (or a send is on its way).
    pub turn_running: bool,
    /// An approval, a question, a queued message or a running task waits.
    pub pending: bool,
    /// A key was pressed in this session's composer since its last turn.
    pub typed_since_turn: bool,
    /// When the last turn ended (ms since the epoch); `None` before any.
    pub last_turn_ended_ms: Option<i64>,
    /// This idle stretch was already compacted.
    pub compacted_since_turn: bool,
    /// How the account bills: only a subscription's hour-long cache gets
    /// Baaz's timer.
    pub billing: BillingMode,
    /// The provider compacts idle sessions itself.
    pub vendor_compacts_idle: bool,
    /// The provider can compact at all.
    pub can_compact: bool,
}

/// What to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Not this idle stretch, and why.
    Never(Why),
    /// Not yet: look again at this time (ms since the epoch).
    WaitUntil(i64),
    /// Send the compaction now.
    CompactNow,
}

/// Why a session is not compacted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    /// The provider cannot compact.
    CannotCompact,
    /// The provider does it itself.
    VendorCompacts,
    /// The cache is not an hour long (per-token or unknown billing).
    ShortCache,
    /// The context is under the threshold, or unreported.
    SmallContext,
    /// A turn is running.
    Running,
    /// Something waits on the person or in the queue.
    Pending,
    /// The person typed since the last turn.
    Typing,
    /// No turn has ended yet.
    NoTurnYet,
    /// Already compacted since the last turn.
    AlreadyCompacted,
    /// The cache has already expired: compacting now reads the whole
    /// context uncached, which is what the timer exists to avoid.
    CacheGone,
}

/// The decision, at `now_ms`.
pub fn decide(inputs: &Inputs, now_ms: i64) -> Decision {
    if !inputs.can_compact {
        return Decision::Never(Why::CannotCompact);
    }
    if inputs.vendor_compacts_idle {
        return Decision::Never(Why::VendorCompacts);
    }
    if inputs.billing != BillingMode::Subscription {
        return Decision::Never(Why::ShortCache);
    }
    if inputs.context_tokens.is_none_or(|tokens| tokens <= THRESHOLD_TOKENS) {
        return Decision::Never(Why::SmallContext);
    }
    if inputs.turn_running {
        return Decision::Never(Why::Running);
    }
    if inputs.pending {
        return Decision::Never(Why::Pending);
    }
    if inputs.typed_since_turn {
        return Decision::Never(Why::Typing);
    }
    if inputs.compacted_since_turn {
        return Decision::Never(Why::AlreadyCompacted);
    }
    let Some(ended) = inputs.last_turn_ended_ms else { return Decision::Never(Why::NoTurnYet) };
    let expires = ended + SUBSCRIPTION_CACHE.as_millis() as i64;
    let due = expires - LEAD.as_millis() as i64;
    if now_ms < due {
        Decision::WaitUntil(due)
    } else if now_ms < expires {
        Decision::CompactNow
    } else {
        Decision::Never(Why::CacheGone)
    }
}

/// The focus line the compaction carries: keep the person's instructions
/// word for word, and name the task that just finished.
pub fn focus_line(finished_task: Option<&str>) -> String {
    let mut line = "Keep every instruction the user gave, word for word.".to_owned();
    if let Some(task) = finished_task.map(str::trim).filter(|task| !task.is_empty()) {
        let task: String = task.chars().take(200).collect();
        line.push_str(&format!(" The task just finished: {task}"));
        if !line.ends_with('.') {
            line.push('.');
        }
    }
    line
}

/// What a topic check answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TopicCheck {
    /// The new message starts another topic.
    NewTopic,
    /// It goes on with the same one.
    SameTopic,
    /// The check did not answer within [`TOPIC_CHECK_LIMIT`].
    TimedOut,
}

/// Whether a message sent above the threshold compacts first (a new
/// topic) or goes as it is (the same topic, a timeout, or a session under
/// the threshold — which never asks).
pub fn topic_switch(context_tokens: Option<u64>, check: impl FnOnce() -> TopicCheck) -> bool {
    if context_tokens.is_none_or(|tokens| tokens <= THRESHOLD_TOKENS) {
        return false;
    }
    check() == TopicCheck::NewTopic
}

/// The topic check's prompt: the last finished task and the new message,
/// each capped (rule 27: a capped input).
pub fn topic_prompt(finished_task: &str, message: &str) -> String {
    let cap = |text: &str| text.chars().take(600).collect::<String>();
    format!(
        "A coding session just finished this task:\n{}\n\nThe user's next message is:\n{}\n\n\
         Does the next message start a different, unrelated task? Answer with one word: NEW or SAME.",
        cap(finished_task),
        cap(message)
    )
}

/// Read the topic check's answer: `NEW` means a new topic.
pub fn read_topic_answer(answer: &str) -> TopicCheck {
    if answer.trim().to_ascii_uppercase().starts_with("NEW") {
        TopicCheck::NewTopic
    } else {
        TopicCheck::SameTopic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;
    const MIN: i64 = 60_000;

    fn ready() -> Inputs {
        Inputs {
            context_tokens: Some(412_000),
            last_turn_ended_ms: Some(1_000_000),
            billing: BillingMode::Subscription,
            can_compact: true,
            ..Inputs::default()
        }
    }

    #[test]
    fn it_waits_for_two_minutes_before_the_cache_expires_then_compacts() {
        let inputs = ready();
        let ended = 1_000_000;
        assert_eq!(decide(&inputs, ended + MIN), Decision::WaitUntil(ended + HOUR - 2 * MIN));
        assert_eq!(decide(&inputs, ended + HOUR - 2 * MIN - 1), Decision::WaitUntil(ended + HOUR - 2 * MIN));
        assert_eq!(decide(&inputs, ended + HOUR - 2 * MIN), Decision::CompactNow);
        assert_eq!(decide(&inputs, ended + HOUR - 1), Decision::CompactNow);
        assert_eq!(decide(&inputs, ended + HOUR), Decision::Never(Why::CacheGone));
    }

    #[test]
    fn each_condition_holds_it_back() {
        let at = 1_000_000 + HOUR - MIN;
        let cases: [(&str, Inputs, Why); 9] = [
            ("cannot compact", Inputs { can_compact: false, ..ready() }, Why::CannotCompact),
            ("vendor does it", Inputs { vendor_compacts_idle: true, ..ready() }, Why::VendorCompacts),
            ("per-token: 5-minute cache", Inputs { billing: BillingMode::PerToken, ..ready() }, Why::ShortCache),
            ("unknown billing", Inputs { billing: BillingMode::Unknown, ..ready() }, Why::ShortCache),
            ("small context", Inputs { context_tokens: Some(THRESHOLD_TOKENS), ..ready() }, Why::SmallContext),
            ("unreported context", Inputs { context_tokens: None, ..ready() }, Why::SmallContext),
            ("a turn runs", Inputs { turn_running: true, ..ready() }, Why::Running),
            ("something pending", Inputs { pending: true, ..ready() }, Why::Pending),
            ("no turn yet", Inputs { last_turn_ended_ms: None, ..ready() }, Why::NoTurnYet),
        ];
        for (name, inputs, why) in cases {
            assert_eq!(decide(&inputs, at), Decision::Never(why), "{name}");
        }
        assert_eq!(decide(&ready(), at), Decision::CompactNow, "and with none of them it goes");
    }

    /// The cancel: a key pressed before it is sent; and once per stretch.
    #[test]
    fn typing_cancels_it_and_it_runs_once() {
        let at = 1_000_000 + HOUR - MIN;
        assert_eq!(decide(&Inputs { typed_since_turn: true, ..ready() }, at), Decision::Never(Why::Typing));
        assert_eq!(decide(&Inputs { compacted_since_turn: true, ..ready() }, at), Decision::Never(Why::AlreadyCompacted));
    }

    /// The queue: once sent, the compaction is a running turn, so the
    /// person's next message is held back as any message sent mid-turn —
    /// and nothing compacts while that runs.
    #[test]
    fn a_sent_compaction_is_a_running_turn() {
        let at = 1_000_000 + HOUR - MIN;
        let sent = Inputs { turn_running: true, compacted_since_turn: true, ..ready() };
        assert_eq!(decide(&sent, at), Decision::Never(Why::Running));
    }

    #[test]
    fn the_topic_switch_asks_only_above_the_threshold_and_a_timeout_sends_as_it_is() {
        let mut asked = false;
        assert!(!topic_switch(Some(THRESHOLD_TOKENS), || {
            asked = true;
            TopicCheck::NewTopic
        }));
        assert!(!asked, "under the threshold nobody is asked");
        assert!(topic_switch(Some(412_000), || TopicCheck::NewTopic));
        assert!(!topic_switch(Some(412_000), || TopicCheck::SameTopic));
        assert!(!topic_switch(Some(412_000), || TopicCheck::TimedOut), "a slow check never holds the message");
        assert_eq!(read_topic_answer(" new\n"), TopicCheck::NewTopic);
        assert_eq!(read_topic_answer("SAME"), TopicCheck::SameTopic);
        assert_eq!(read_topic_answer(""), TopicCheck::SameTopic);
        assert!(topic_prompt(&"x".repeat(5000), "y").len() < 1000, "a capped input");
    }

    #[test]
    fn the_focus_line_keeps_instructions_and_names_the_task() {
        assert_eq!(focus_line(None), "Keep every instruction the user gave, word for word.");
        let line = focus_line(Some("Fix the GB postcode test"));
        assert!(line.ends_with("The task just finished: Fix the GB postcode test."), "{line}");
    }
}
