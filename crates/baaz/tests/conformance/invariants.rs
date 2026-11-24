//! The seam's invariants, written once, as a checker over what an adapter
//! said: the ordered [`ProviderEvent`] stream, interleaved with the commands
//! the driver sent and the acks they returned (docs/82 5.2; docs/81 B
//! "tests" item 2). Every adapter's every fixture runs through
//! [`check`]; the seeded-fault tests (`faults.rs`) prove each invariant
//! fails on its violation with a message that names it.
//!
//! The invariants are the contract `provider/src/event.rs` and
//! `provider/src/traits.rs` document in prose:
//!
//! * [`ACK_BEFORE_OR_WITH_EVENTS`] — an ack is admission, never outcome: it
//!   names a turn no event had named before the command left, a
//!   `TurnIdentified` bridges only an id an ack returned (once, and the old
//!   id is never used after it), and an admitted turn streams.
//! * [`ONE_TERMINAL_PER_TURN`] — a turn finishes (`TurnFinished`) at most
//!   once and settles (`TurnSettled`) at most once, a terminal names a turn
//!   that opened, and every opened turn ends once the wire ended it.
//! * [`APPROVAL_CARD_BEFORE_TAP`] — every `ApprovalRequested` follows its
//!   card's delta.
//! * [`APPROVAL_ANSWERED_ONCE`] — one tap per approval, at most one
//!   decision accepted, every later decision refused typed as settled, and
//!   a card that left `Pending` never returns to it.
//! * [`LOST_LEAVES_NOTHING_PENDING`] — by `ConnectionLost` every opened
//!   turn has ended and every raised approval's card has settled; a
//!   `ListPending` after it lists none of them, and a decision for one is
//!   refused.
//! * [`BLOCK_INDEX_IN_RANGE`] — every positional delta names a turn the
//!   stream opened and a block that exists, of the kind the delta appends
//!   to (a late delta for a removed turn is ignored, never an error).
//! * [`USAGE_PER_TURN`] — a turn's usage rides its own `TurnFinished`, and
//!   the per-turn figures add up to the total the wire itself reported (no
//!   cumulative figure leaks into a turn, no turn's usage is lost).
//! * [`RUNNING_BEFORE_CONTENT`] — a `TurnRunning` is sent before the turn
//!   says anything.
//! * [`QUESTION_CARD_WITH_TAP`] — every `QuestionRaised` has its prompt
//!   card in the stream.
//! * [`RESTORED_FOLLOWS_LOST`] — `ConnectionRestored` only after a
//!   `ConnectionLost`.
//! * [`EVENTS_NAME_THEIR_SESSION`] — every session event names the session
//!   the driver opened.

use std::collections::{HashMap, HashSet};

use aui_protocol::{ApprovalState, Block, Delta, Turn};
use provider::{Ack, ProviderError, ProviderEvent, RefusalHint};

pub const ACK_BEFORE_OR_WITH_EVENTS: &str = "ack-before-or-with-events";
pub const ONE_TERMINAL_PER_TURN: &str = "one-terminal-per-turn";
pub const APPROVAL_CARD_BEFORE_TAP: &str = "approval-card-before-tap";
pub const APPROVAL_ANSWERED_ONCE: &str = "approval-answered-once";
pub const LOST_LEAVES_NOTHING_PENDING: &str = "lost-leaves-nothing-pending";
pub const BLOCK_INDEX_IN_RANGE: &str = "block-index-in-range";
pub const USAGE_PER_TURN: &str = "usage-per-turn";
pub const RUNNING_BEFORE_CONTENT: &str = "running-before-content";
pub const QUESTION_CARD_WITH_TAP: &str = "question-card-with-tap";
pub const RESTORED_FOLLOWS_LOST: &str = "restored-follows-lost";
pub const EVENTS_NAME_THEIR_SESSION: &str = "events-name-their-session";
pub const TAP_OFFERS_THE_CARDS_ANSWERS: &str = "tap-offers-the-cards-answers";

/// Every invariant [`check`] asserts, by name.
pub const INVARIANTS: [&str; 12] = [
    ACK_BEFORE_OR_WITH_EVENTS,
    ONE_TERMINAL_PER_TURN,
    APPROVAL_CARD_BEFORE_TAP,
    APPROVAL_ANSWERED_ONCE,
    LOST_LEAVES_NOTHING_PENDING,
    BLOCK_INDEX_IN_RANGE,
    USAGE_PER_TURN,
    RUNNING_BEFORE_CONTENT,
    QUESTION_CARD_WITH_TAP,
    RESTORED_FOLLOWS_LOST,
    EVENTS_NAME_THEIR_SESSION,
    TAP_OFFERS_THE_CARDS_ANSWERS,
];

/// A command the driver sent, as much of it as the invariants read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Sent {
    Open,
    Resume(String),
    Submit,
    Decide(String),
    Answer(String),
    Interrupt,
    ListPending,
    /// A compact-now (docs/85 10.4): acked `Accepted`, its turn the
    /// provider's own — no submission admits it.
    Compact,
}

/// One thing that happened, in the order the driver saw it. Events drained
/// between a `Sent` and its `Acked` arrived while the command was in flight
/// — "with" its ack.
#[derive(Clone, Debug)]
pub enum Entry {
    Sent(Sent),
    Acked(Sent, Result<Ack, ProviderError>),
    Event(ProviderEvent),
}

/// What the driver saw, start to end.
#[derive(Clone, Debug, Default)]
pub struct Trace {
    pub entries: Vec<Entry>,
}

impl Trace {
    pub fn sent(&mut self, sent: Sent) {
        self.entries.push(Entry::Sent(sent));
    }

    pub fn acked(&mut self, sent: Sent, result: Result<Ack, ProviderError>) {
        self.entries.push(Entry::Acked(sent, result));
    }

    pub fn event(&mut self, event: ProviderEvent) {
        self.entries.push(Entry::Event(event));
    }

    /// The events alone, in order.
    pub fn events(&self) -> impl Iterator<Item = &ProviderEvent> {
        self.entries.iter().filter_map(|entry| match entry {
            Entry::Event(event) => Some(event),
            _ => None,
        })
    }
}

/// Usage figures summed over a trace's finished turns, or reported by the
/// wire as its own running total.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub tokens_in: Option<u64>,
    pub tokens_out: Option<u64>,
    pub cost_usd: Option<f64>,
}

/// What the fixture promises about the trace it produced.
#[derive(Clone, Debug, Default)]
pub struct Expect {
    /// The wire ended every turn it opened (the recording did not stop
    /// mid-turn): every opened turn must have its terminal.
    pub closed: bool,
    /// The session total the wire itself reported, where it reports one:
    /// the per-turn figures must add up to it.
    pub wire_usage: Option<Usage>,
}

/// One broken invariant: which, where in the trace, and what.
#[derive(Clone, Debug, PartialEq)]
pub struct Violation {
    pub invariant: &'static str,
    pub at: usize,
    pub message: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] entry {}: {}", self.invariant, self.at, self.message)
    }
}

/// A refusal of something no longer pending, in any of the typed shapes
/// the seam has for it — never `Ok`, never an untyped failure.
pub fn refused_as_gone(error: &ProviderError) -> bool {
    error.is_unknown_approval()
        || matches!(error.hint(), RefusalHint::AlreadySettled | RefusalHint::ItemGone)
        || matches!(error, ProviderError::Unavailable { .. })
}

#[derive(Default)]
struct TurnLife {
    opened_at: Option<usize>,
    /// The provider ran it: an ack admitted it or a `TurnRunning` announced
    /// it. A turn that only appeared in the transcript (history a resume
    /// replayed, a host turn for a session-level card) never ran, so
    /// nothing has to end it.
    ran: bool,
    /// It opened after the driver's first submit (or compaction): its usage
    /// is the live child's, which the wire's own total covers.
    live: bool,
    running_at: Option<usize>,
    content_at: Option<usize>,
    finished: Vec<usize>,
    settled: Vec<usize>,
}

#[derive(Default)]
struct ApprovalLife {
    card_at: Option<usize>,
    /// The card's choices as last carried: what the tap's typed options
    /// must name (docs/82 5.5).
    choices: Vec<aui_protocol::ApprovalChoice>,
    pending: bool,
    settled_at: Option<usize>,
    taps: Vec<usize>,
    /// Decisions in order: where, and whether accepted.
    decisions: Vec<(usize, Result<(), ProviderError>)>,
    tapped_before_loss: bool,
}

struct Checker<'a> {
    expect: &'a Expect,
    out: Vec<Violation>,
    /// accepted id → the id the turn streams under.
    alias: HashMap<String, String>,
    turns: HashMap<String, TurnLife>,
    /// Turn ids in the order they opened.
    order: Vec<String>,
    removed: HashSet<String>,
    /// The transcript as the deltas build it: per turn id, its blocks.
    blocks: HashMap<String, Vec<Block>>,
    approvals: HashMap<String, ApprovalLife>,
    approval_order: Vec<String>,
    question_cards: HashSet<String>,
    questions_tapped: Vec<(usize, String)>,
    session: Option<String>,
    lost: bool,
    lost_ever: bool,
    /// Acked turns: (sent_at, acked_at, id, queued).
    acked: Vec<(usize, usize, String, bool)>,
    identified: HashMap<String, usize>,
    /// Canonical ids of the turns an ack admitted.
    admitted: HashSet<String>,
    /// Ids named by any event before index i (for the freshness check).
    named_before: HashMap<String, usize>,
    usage: Usage,
    last_sent: Option<usize>,
    first_submit: Option<usize>,
}

impl<'a> Checker<'a> {
    fn fail(&mut self, invariant: &'static str, at: usize, message: String) {
        self.out.push(Violation { invariant, at, message });
    }

    fn canon(&self, id: &str) -> String {
        self.alias.get(id).cloned().unwrap_or_else(|| id.to_owned())
    }

    fn name(&mut self, id: &str, at: usize) {
        self.named_before.entry(id.to_owned()).or_insert(at);
    }

    fn open(&mut self, raw: &str, at: usize) {
        let id = self.canon(raw);
        self.name(raw, at);
        let live = self.first_submit.is_some_and(|first| first < at);
        let life = self.turns.entry(id.clone()).or_default();
        if life.opened_at.is_none() {
            life.opened_at = Some(at);
            life.live = live;
            self.order.push(id);
        }
    }

    fn ran(&mut self, raw: &str, at: usize) {
        let id = self.canon(raw);
        self.open(raw, at);
        if let Some(life) = self.turns.get_mut(&id) {
            life.ran = true;
        }
    }

    fn content(&mut self, raw: &str, at: usize) {
        let id = self.canon(raw);
        self.name(raw, at);
        if let Some(life) = self.turns.get_mut(&id) {
            life.content_at.get_or_insert(at);
        }
    }

    fn after_identified(&mut self, raw: &str, at: usize, what: &str) {
        if let Some(&identified_at) = self.identified.get(raw) {
            if identified_at < at {
                let streams = self.canon(raw);
                self.fail(
                    ACK_BEFORE_OR_WITH_EVENTS,
                    at,
                    format!("{what} names turn {raw:?} after TurnIdentified renamed it {streams:?} (entry {identified_at})"),
                );
            }
        }
    }

    fn ran_turn(&self, id: &str, life: &TurnLife) -> bool {
        life.ran || self.admitted.contains(id)
    }

    fn card(&mut self, block: &Block, at: usize) {
        match block {
            Block::Approval { id, state, choices, .. } => {
                let life = self.approvals.entry(id.clone()).or_default();
                life.choices = choices.clone();
                if life.card_at.is_none() {
                    life.card_at = Some(at);
                    self.approval_order.push(id.clone());
                }
                let pending = *state == ApprovalState::Pending;
                if pending && life.settled_at.is_some() {
                    let settled = life.settled_at.unwrap_or_default();
                    self.fail(
                        APPROVAL_ANSWERED_ONCE,
                        at,
                        format!("approval {id:?} went back to Pending after it settled (entry {settled})"),
                    );
                } else if !pending && life.settled_at.is_none() {
                    let life = self.approvals.get_mut(id).expect("just inserted");
                    life.settled_at = Some(at);
                }
                if let Some(life) = self.approvals.get_mut(id) {
                    life.pending = pending;
                }
            }
            Block::Question { id, .. } => {
                // One request may carry several questions: a card's id is
                // the request's, or the request's and the question's
                // joined by ':' (Muse).
                self.question_cards.insert(id.split(':').next().unwrap_or(id).to_owned());
                self.question_cards.insert(id.clone());
            }
            Block::ToolGroup { .. } => {}
            _ => {}
        }
    }

    fn positional(&mut self, raw: &str, index: usize, want: Option<&'static str>, what: &str, at: usize) {
        self.after_identified(raw, at, what);
        let id = self.canon(raw);
        if self.removed.contains(&id) || self.removed.contains(raw) {
            return;
        }
        let Some(blocks) = self.blocks.get(&id).or_else(|| self.blocks.get(raw)) else {
            self.fail(BLOCK_INDEX_IN_RANGE, at, format!("{what} names turn {raw:?}, which the stream never opened"));
            return;
        };
        let Some(block) = blocks.get(index) else {
            let len = blocks.len();
            self.fail(
                BLOCK_INDEX_IN_RANGE,
                at,
                format!("{what} names block {index} of turn {raw:?}, which has {len} block(s)"),
            );
            return;
        };
        let kind = block_kind(block);
        if let Some(want) = want {
            if kind != want {
                self.fail(
                    BLOCK_INDEX_IN_RANGE,
                    at,
                    format!("{what} wants a {want} block at index {index} of turn {raw:?}, which holds a block of kind {kind}"),
                );
            }
        }
        self.content(raw, at);
    }

    fn delta(&mut self, delta: &Delta, at: usize) {
        match delta {
            Delta::TurnStarted { turn } => {
                let id = turn.id().to_owned();
                if matches!(turn, Turn::Assistant { .. }) {
                    self.after_identified(&id, at, "TurnStarted");
                    self.open(&id, at);
                } else {
                    self.name(&id, at);
                }
                for block in turn.blocks() {
                    self.card(block, at);
                }
                if !turn.blocks().is_empty() {
                    self.content(&id, at);
                }
                self.removed.remove(&id);
                self.blocks.insert(self.canon(&id), turn.blocks().to_vec());
            }
            Delta::TurnReplaced { turn } => {
                let id = turn.id().to_owned();
                for block in turn.blocks() {
                    self.card(block, at);
                }
                let key = self.canon(&id);
                if self.blocks.contains_key(&key) {
                    self.blocks.insert(key, turn.blocks().to_vec());
                }
                self.content(&id, at);
            }
            Delta::BlockAdded { turn_id, block } => {
                self.after_identified(turn_id, at, "BlockAdded");
                let key = self.canon(turn_id);
                self.card(block, at);
                if self.removed.contains(&key) {
                    return;
                }
                match self.blocks.get_mut(&key) {
                    Some(blocks) => blocks.push(block.clone()),
                    None => self.fail(
                        BLOCK_INDEX_IN_RANGE,
                        at,
                        format!("BlockAdded names turn {turn_id:?}, which the stream never opened"),
                    ),
                }
                self.content(turn_id, at);
            }
            Delta::BlockUpdated { turn_id, block_index, block } => {
                self.card(block, at);
                self.positional(turn_id, *block_index, None, "BlockUpdated", at);
                let key = self.canon(turn_id);
                if let Some(slot) = self.blocks.get_mut(&key).and_then(|blocks| blocks.get_mut(*block_index)) {
                    *slot = block.clone();
                }
            }
            Delta::TextDelta { turn_id, block_index, .. } => {
                self.positional(turn_id, *block_index, Some("text"), "TextDelta", at);
            }
            Delta::ThinkingDelta { turn_id, block_index, .. } => {
                self.positional(turn_id, *block_index, Some("thinking"), "ThinkingDelta", at);
            }
            Delta::ToolOutputDelta { turn_id, block_index, .. } => {
                self.positional(turn_id, *block_index, Some("tool"), "ToolOutputDelta", at);
            }
            Delta::BlockRemoved { turn_id, block_index } => {
                self.positional(turn_id, *block_index, None, "BlockRemoved", at);
                let key = self.canon(turn_id);
                if let Some(blocks) = self.blocks.get_mut(&key) {
                    if *block_index < blocks.len() {
                        blocks.remove(*block_index);
                    }
                }
            }
            Delta::TurnRemoved { turn_id } => {
                let key = self.canon(turn_id);
                self.removed.insert(key.clone());
                self.removed.insert(turn_id.clone());
                self.blocks.remove(&key);
            }
            Delta::TurnFinished { turn_id, meta } => {
                self.after_identified(turn_id, at, "TurnFinished");
                let key = self.canon(turn_id);
                self.name(turn_id, at);
                if self.removed.contains(&key) {
                    return;
                }
                let Some(life) = self.turns.get_mut(&key) else {
                    self.fail(
                        ONE_TERMINAL_PER_TURN,
                        at,
                        format!("TurnFinished names turn {turn_id:?}, which never opened"),
                    );
                    return;
                };
                life.finished.push(at);
                if life.finished.len() == 2 {
                    let first = life.finished[0];
                    self.fail(
                        ONE_TERMINAL_PER_TURN,
                        at,
                        format!("turn {turn_id:?} finished twice (TurnFinished at entries {first} and {at})"),
                    );
                    return;
                }
                if !life.live {
                    return;
                }
                self.usage.tokens_in = Some(self.usage.tokens_in.unwrap_or(0) + meta.tokens_in);
                self.usage.tokens_out = Some(self.usage.tokens_out.unwrap_or(0) + meta.tokens_out);
                self.usage.cost_usd = Some(self.usage.cost_usd.unwrap_or(0.0) + meta.cost_usd);
            }
        }
    }

    fn session_named(&mut self, id: &str, what: &str, at: usize) {
        if let Some(session) = &self.session {
            if session != id {
                let session = session.clone();
                self.fail(
                    EVENTS_NAME_THEIR_SESSION,
                    at,
                    format!("{what} names session {id:?}, but the driver's session is {session:?}"),
                );
            }
        }
    }

    fn event(&mut self, event: &ProviderEvent, at: usize) {
        match event {
            ProviderEvent::Deltas { session_id, deltas } => {
                if let Some(id) = session_id {
                    self.session_named(id, "Deltas", at);
                }
                for delta in deltas {
                    self.delta(delta, at);
                }
            }
            ProviderEvent::ApprovalRequested { session_id, approval_id, options, .. } => {
                self.session_named(session_id, "ApprovalRequested", at);
                let lost = self.lost;
                let life = self.approvals.entry(approval_id.clone()).or_default();
                life.taps.push(at);
                life.tapped_before_loss = !lost;
                let card = life.card_at;
                let taps = life.taps.clone();
                let choices = life.choices.clone();
                // The tap's typed answers are the card's own buttons, in the
                // one vocabulary, each answer once (docs/82 5.5): the app
                // decides by them, never by the id's shape.
                if card.is_some() {
                    let mut seen = HashSet::new();
                    for option in options {
                        let on_card = choices
                            .iter()
                            .any(|choice| choice.id == option.choice && choice.label == option.answer.label());
                        if !on_card || !seen.insert(option.answer) {
                            self.fail(
                                TAP_OFFERS_THE_CARDS_ANSWERS,
                                at,
                                format!(
                                    "ApprovalRequested {approval_id:?} offers {:?} as {:?}, which its card does not carry once",
                                    option.answer, option.choice
                                ),
                            );
                        }
                    }
                }
                if card.is_none() {
                    self.approval_order.push(approval_id.clone());
                    self.fail(
                        APPROVAL_CARD_BEFORE_TAP,
                        at,
                        format!("ApprovalRequested {approval_id:?} arrived before any delta carried its card"),
                    );
                }
                if taps.len() == 2 {
                    self.fail(
                        APPROVAL_ANSWERED_ONCE,
                        at,
                        format!("approval {approval_id:?} was asked twice (taps at entries {} and {at})", taps[0]),
                    );
                }
            }
            ProviderEvent::QuestionRaised { session_id, question_id, .. } => {
                self.session_named(session_id, "QuestionRaised", at);
                self.questions_tapped.push((at, question_id.clone()));
            }
            ProviderEvent::TurnIdentified { session_id, accepted, .. } => {
                self.session_named(session_id, "TurnIdentified", at);
                if let Some(first) = self.identified.get(accepted) {
                    let first = *first;
                    self.fail(
                        ACK_BEFORE_OR_WITH_EVENTS,
                        at,
                        format!("turn {accepted:?} was identified twice (entries {first} and {at})"),
                    );
                }
                self.identified.insert(accepted.clone(), at);
            }
            ProviderEvent::TurnRunning { session_id, turn_id } => {
                self.session_named(session_id, "TurnRunning", at);
                let key = self.canon(turn_id);
                if let Some(content) = self.turns.get(&key).and_then(|life| life.content_at) {
                    self.fail(
                        RUNNING_BEFORE_CONTENT,
                        at,
                        format!("TurnRunning for {turn_id:?} came after the turn spoke (entry {content})"),
                    );
                }
                self.ran(turn_id, at);
                if let Some(life) = self.turns.get_mut(&key) {
                    life.running_at.get_or_insert(at);
                }
            }
            ProviderEvent::TurnSettled { session_id, turn_id, .. } => {
                self.session_named(session_id, "TurnSettled", at);
                self.after_identified(turn_id, at, "TurnSettled");
                let key = self.canon(turn_id);
                self.name(turn_id, at);
                if self.removed.contains(&key) {
                    return;
                }
                let Some(life) = self.turns.get_mut(&key) else {
                    self.fail(ONE_TERMINAL_PER_TURN, at, format!("TurnSettled names turn {turn_id:?}, which never opened"));
                    return;
                };
                life.settled.push(at);
                if life.settled.len() == 2 {
                    let first = life.settled[0];
                    self.fail(
                        ONE_TERMINAL_PER_TURN,
                        at,
                        format!("turn {turn_id:?} settled twice (TurnSettled at entries {first} and {at})"),
                    );
                }
            }
            ProviderEvent::ApprovalModeChanged { session_id, .. }
            | ProviderEvent::PlanModeChanged { session_id, .. }
            | ProviderEvent::SkillsListed { session_id, .. }
            | ProviderEvent::EffortInForce { session_id, .. }
            | ProviderEvent::ModelCatalogChanged { session_id }
            | ProviderEvent::SessionFacts { session_id, .. }
            | ProviderEvent::HeaderChanged { session_id, .. }
            | ProviderEvent::PromptRestored { session_id, .. }
            | ProviderEvent::Notice { session_id, .. }
            | ProviderEvent::TranscriptPaged { session_id, .. } => {
                self.session_named(session_id, "a session event", at);
            }
            ProviderEvent::StatusChanged { .. }
            | ProviderEvent::AccountChanged { .. }
            | ProviderEvent::LoginCompleted { .. }
            | ProviderEvent::UsageChanged { .. } => {}
            ProviderEvent::ConnectionRestored => {
                if !self.lost {
                    self.fail(RESTORED_FOLLOWS_LOST, at, "ConnectionRestored with no ConnectionLost before it".into());
                }
                self.lost = false;
            }
            ProviderEvent::ConnectionLost { reason } => {
                self.lost = true;
                self.lost_ever = true;
                let open: Vec<String> = self
                    .order
                    .iter()
                    .filter(|id| !self.removed.contains(*id))
                    .filter(|id| self.turns.get(*id).is_some_and(|life| self.ran_turn(id, life) && life.finished.is_empty() && life.settled.is_empty()))
                    .cloned()
                    .collect();
                for id in open {
                    self.fail(
                        LOST_LEAVES_NOTHING_PENDING,
                        at,
                        format!("ConnectionLost ({reason}) left turn {id:?} open: no TurnFinished or TurnSettled before it"),
                    );
                }
                let pending: Vec<String> = self
                    .approval_order
                    .iter()
                    .filter(|id| self.approvals.get(*id).is_some_and(|life| !life.taps.is_empty() && life.pending))
                    .cloned()
                    .collect();
                for id in pending {
                    self.fail(
                        LOST_LEAVES_NOTHING_PENDING,
                        at,
                        format!("ConnectionLost ({reason}) left approval {id:?} Pending: its card never settled"),
                    );
                }
            }
        }
    }

    fn acked(&mut self, sent: &Sent, result: &Result<Ack, ProviderError>, at: usize) {
        let sent_at = self.last_sent.unwrap_or(at);
        match (sent, result) {
            (Sent::Open | Sent::Resume(_), Ok(Ack::Session { session_id, .. })) => {
                self.session = Some(session_id.clone());
            }
            (Sent::Submit, Ok(Ack::TurnAccepted { turn_id, queued })) => {
                if let Some(&named) = self.named_before.get(turn_id) {
                    if named < sent_at {
                        self.fail(
                            ACK_BEFORE_OR_WITH_EVENTS,
                            at,
                            format!(
                                "TurnAccepted names turn {turn_id:?}, which events named before the submit left (entry {named}): an ack names the turn it admits, not an old one"
                            ),
                        );
                    }
                }
                self.acked.push((sent_at, at, turn_id.clone(), *queued));
                if !queued {
                    self.admitted.insert(self.canon(turn_id));
                }
            }
            (Sent::Decide(id), result) => {
                let lost = self.lost;
                let life = self.approvals.entry(id.clone()).or_default();
                let decided = result.as_ref().map(|_| ()).map_err(Clone::clone);
                let accepted_before = life.decisions.iter().filter(|(_, result)| result.is_ok()).count();
                life.decisions.push((at, decided.clone()));
                let tapped_before_loss = life.tapped_before_loss;
                match decided {
                    Ok(()) if accepted_before > 0 => self.fail(
                        APPROVAL_ANSWERED_ONCE,
                        at,
                        format!("approval {id:?} was decided again and the adapter answered Ok: answered twice"),
                    ),
                    Ok(()) if lost && tapped_before_loss => self.fail(
                        LOST_LEAVES_NOTHING_PENDING,
                        at,
                        format!("approval {id:?}, raised before the connection was lost, accepted a decision after it"),
                    ),
                    Err(error) if accepted_before > 0 && !refused_as_gone(&error) => self.fail(
                        APPROVAL_ANSWERED_ONCE,
                        at,
                        format!("a second decision for approval {id:?} was refused untyped: {error:?}"),
                    ),
                    _ => {}
                }
            }
            (Sent::ListPending, Ok(Ack::PendingWork { approvals, .. })) if self.lost_ever => {
                for approval in approvals {
                    if self.approvals.get(&approval.id).is_some_and(|life| life.tapped_before_loss) {
                        self.fail(
                            LOST_LEAVES_NOTHING_PENDING,
                            at,
                            format!("ListPending after ConnectionLost still lists approval {:?}", approval.id),
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(&mut self, end: usize) {
        // Every TurnIdentified bridges an id an ack returned.
        let acked: HashSet<String> = self.acked.iter().map(|(_, _, id, _)| id.clone()).collect();
        let mut identified: Vec<(&String, &usize)> = self.identified.iter().collect();
        identified.sort_by_key(|(_, at)| **at);
        let stray: Vec<(String, usize)> = identified
            .into_iter()
            .filter(|(accepted, _)| !acked.contains(*accepted))
            .map(|(accepted, at)| (accepted.clone(), *at))
            .collect();
        for (accepted, at) in stray {
            self.fail(
                ACK_BEFORE_OR_WITH_EVENTS,
                at,
                format!("TurnIdentified bridges {accepted:?}, an id no ack returned"),
            );
        }
        if !self.expect.closed {
            return;
        }
        // An admitted turn streams.
        let admitted: Vec<(usize, String)> =
            self.acked.iter().filter(|(_, _, _, queued)| !queued).map(|(_, at, id, _)| (*at, id.clone())).collect();
        for (at, id) in admitted {
            let key = self.canon(&id);
            let opened = self.turns.get(&key).is_some_and(|life| life.opened_at.is_some());
            if !opened && !self.removed.contains(&key) && !self.lost_ever {
                self.fail(
                    ACK_BEFORE_OR_WITH_EVENTS,
                    at,
                    format!("TurnAccepted admitted turn {id:?}, and no event ever opened it"),
                );
            }
        }
        // Every opened turn ended once the wire ended it.
        let open: Vec<(usize, String)> = self
            .order
            .iter()
            .filter(|id| !self.removed.contains(*id))
            .filter_map(|id| {
                let life = self.turns.get(id)?;
                (self.ran_turn(id, life) && life.finished.is_empty() && life.settled.is_empty())
                    .then(|| (life.opened_at.unwrap_or(end), id.clone()))
            })
            .collect();
        for (at, id) in open {
            self.fail(
                ONE_TERMINAL_PER_TURN,
                at,
                format!("turn {id:?} opened and never ended: no TurnFinished or TurnSettled, though the wire ended it"),
            );
        }
        // Questions: every tap has its card somewhere in the stream.
        let missing: Vec<(usize, String)> =
            self.questions_tapped.iter().filter(|(_, id)| !self.question_cards.contains(id)).cloned().collect();
        for (at, id) in missing {
            self.fail(QUESTION_CARD_WITH_TAP, at, format!("QuestionRaised {id:?} has no Question card in the stream"));
        }
        // Usage: the per-turn figures add up to the wire's own total.
        if let Some(wire) = self.expect.wire_usage {
            let summed = self.usage;
            let tokens = |name: &str, wire: Option<u64>, summed: Option<u64>, out: &mut Vec<String>| {
                if let Some(wire) = wire {
                    let summed = summed.unwrap_or(0);
                    if summed != wire {
                        out.push(format!("{name}: turns sum to {summed}, the wire reported {wire}"));
                    }
                }
            };
            let mut wrong = Vec::new();
            tokens("tokens_in", wire.tokens_in, summed.tokens_in, &mut wrong);
            tokens("tokens_out", wire.tokens_out, summed.tokens_out, &mut wrong);
            if let Some(cost) = wire.cost_usd {
                let summed = summed.cost_usd.unwrap_or(0.0);
                if (summed - cost).abs() > 1e-9 {
                    wrong.push(format!("cost_usd: turns sum to {summed}, the wire reported {cost}"));
                }
            }
            for message in wrong {
                self.fail(USAGE_PER_TURN, end, message);
            }
        }
    }
}

fn block_kind(block: &Block) -> &'static str {
    match block {
        Block::Text { .. } => "text",
        Block::Thinking { .. } => "thinking",
        Block::ToolCall { .. } => "tool",
        Block::ToolGroup { .. } => "tool group",
        Block::Approval { .. } => "approval",
        Block::Question { .. } => "question",
        _ => "other",
    }
}

/// Every invariant [`INVARIANTS`] names, over `trace`. Empty when the trace
/// keeps the contract.
pub fn check(trace: &Trace, expect: &Expect) -> Vec<Violation> {
    let mut checker = Checker {
        expect,
        out: Vec::new(),
        alias: HashMap::new(),
        turns: HashMap::new(),
        order: Vec::new(),
        removed: HashSet::new(),
        blocks: HashMap::new(),
        approvals: HashMap::new(),
        approval_order: Vec::new(),
        question_cards: HashSet::new(),
        questions_tapped: Vec::new(),
        session: None,
        lost: false,
        lost_ever: false,
        acked: Vec::new(),
        identified: HashMap::new(),
        admitted: HashSet::new(),
        named_before: HashMap::new(),
        usage: Usage::default(),
        last_sent: None,
        first_submit: None,
    };
    // First pass: the renames, so a turn is one turn under both its names.
    for event in trace.events() {
        if let ProviderEvent::TurnIdentified { accepted, turn_id, .. } = event {
            checker.alias.insert(accepted.clone(), turn_id.clone());
        }
    }
    for (at, entry) in trace.entries.iter().enumerate() {
        match entry {
            Entry::Sent(sent) => {
                checker.last_sent = Some(at);
                // A compaction is live work on the child too (docs/85
                // 10.4): its turn's usage is in the wire's total.
                if matches!(sent, Sent::Submit | Sent::Compact) {
                    checker.first_submit.get_or_insert(at);
                }
            }
            Entry::Acked(sent, result) => checker.acked(sent, result, at),
            Entry::Event(event) => checker.event(event, at),
        }
    }
    checker.finish(trace.entries.len());
    checker.out
}

/// One line per entry, for a failure's dump (`CONFORMANCE_DUMP=<case>`).
pub fn describe(trace: &Trace) -> String {
    let delta = |delta: &Delta| match delta {
        Delta::TurnStarted { turn } => format!("TurnStarted({} {:?}, {} blocks)", if matches!(turn, Turn::Assistant { .. }) { "assistant" } else { "user" }, turn.id(), turn.blocks().len()),
        Delta::BlockAdded { turn_id, block } => format!("BlockAdded({turn_id:?}, {})", block_kind(block)),
        Delta::BlockUpdated { turn_id, block_index, block } => format!("BlockUpdated({turn_id:?}#{block_index}, {})", block_kind(block)),
        Delta::TurnFinished { turn_id, meta } => format!("TurnFinished({turn_id:?}, in {} out {} cost {})", meta.tokens_in, meta.tokens_out, meta.cost_usd),
        other => format!("{other:?}").chars().take(100).collect(),
    };
    trace
        .entries
        .iter()
        .enumerate()
        .map(|(at, entry)| match entry {
            Entry::Sent(sent) => format!("{at:>3} sent {sent:?}"),
            Entry::Acked(sent, result) => format!("{at:>3} acked {sent:?}: {}", format!("{result:?}").chars().take(160).collect::<String>()),
            Entry::Event(ProviderEvent::Deltas { deltas, .. }) => {
                format!("{at:>3} deltas [{}]", deltas.iter().map(delta).collect::<Vec<_>>().join(", "))
            }
            Entry::Event(event) => format!("{at:>3} {}", format!("{event:?}").chars().take(160).collect::<String>()),
        })
        .collect::<Vec<_>>()
        .join("\n")
}
