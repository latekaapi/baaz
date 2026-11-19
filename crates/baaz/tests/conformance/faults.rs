//! Seeded faults: the checker in [`super::invariants`] fails on every
//! violation it exists to catch, naming the invariant and saying what broke.
//!
//! Two layers. Each invariant gets a planted trace — the seam events of a
//! session that breaks exactly that rule and keeps every other — so a
//! checker that went quiet (or loud) on any one of them fails here. Then
//! planted wire fixtures (`tests/fixtures/conformance/planted/`, hand-
//! written, never captured) travel a real adapter on its stand-in child and
//! come out the other end flagged, so the whole path — stand-in, adapter,
//! driver, checker — is shown to catch a fault, not only the checker.

use aui_protocol::{ApprovalScope, ApprovalState, Block, Delta, Turn, TurnMeta};
use provider::{Ack, ProviderError, ProviderEvent};

use super::invariants::{self, check, Expect, Sent, Trace, Usage};
use super::replay;
use super::Agent;

const SESSION: &str = "s-1";

fn deltas(list: Vec<Delta>) -> ProviderEvent {
    ProviderEvent::Deltas { session_id: Some(SESSION.into()), deltas: list }
}

fn assistant(id: &str) -> Delta {
    Delta::TurnStarted {
        turn: Turn::Assistant { id: id.into(), blocks: Vec::new(), meta: TurnMeta::default(), timestamp: None },
    }
}

fn text(turn: &str) -> Delta {
    Delta::BlockAdded { turn_id: turn.into(), block: Block::Text { text: "hi".into(), streaming: false } }
}

fn finished(turn: &str, tokens_out: u64) -> Delta {
    Delta::TurnFinished { turn_id: turn.into(), meta: TurnMeta { tokens_out, ..TurnMeta::default() } }
}

fn settled(turn: &str) -> ProviderEvent {
    ProviderEvent::TurnSettled {
        session_id: SESSION.into(),
        turn_id: turn.into(),
        outcome: provider::facts::TurnOutcome::Completed,
    }
}

fn approval(id: &str, state: ApprovalState) -> Block {
    Block::Approval {
        id: id.into(),
        tool: "shell".into(),
        command: "ls".into(),
        reason: String::new(),
        cwd: String::new(),
        capabilities: Vec::new(),
        scope: ApprovalScope::ThisCommand,
        body_kind: Default::default(),
        state,
        rule: None,
        choices: Vec::new(),
        stages: Vec::new(),
        current_stage: None,
        badges: Default::default(),
        feedback: None,
        resolved_by: None,
    }
}

fn tap(id: &str) -> ProviderEvent {
    ProviderEvent::ApprovalRequested {
        session_id: SESSION.into(),
        approval_id: id.into(),
        headline: "ls".into(),
        options: Vec::new(),
    }
}

/// A session opened, one submit admitted as `t-1`.
fn opened() -> Trace {
    let mut trace = Trace::default();
    trace.sent(Sent::Open);
    trace.acked(Sent::Open, Ok(Ack::Session { session_id: SESSION.into(), title: None }));
    trace.sent(Sent::Submit);
    trace.acked(Sent::Submit, Ok(Ack::TurnAccepted { turn_id: "t-1".into(), queued: false }));
    trace
}

/// One clean turn: opened, said something, finished, settled.
fn clean() -> Trace {
    let mut trace = opened();
    trace.event(ProviderEvent::TurnRunning { session_id: SESSION.into(), turn_id: "t-1".into() });
    trace.event(deltas(vec![assistant("t-1"), text("t-1"), finished("t-1", 5)]));
    trace.event(settled("t-1"));
    trace
}

/// A clean turn that raised an approval, decided once, refused the second.
fn clean_with_approval() -> Trace {
    let mut trace = opened();
    trace.event(deltas(vec![assistant("t-1"), Delta::BlockAdded {
        turn_id: "t-1".into(),
        block: approval("a-1", ApprovalState::Pending),
    }]));
    trace.event(tap("a-1"));
    trace.sent(Sent::Decide("a-1".into()));
    trace.acked(Sent::Decide("a-1".into()), Ok(Ack::Accepted));
    trace.sent(Sent::Decide("a-1".into()));
    trace.acked(Sent::Decide("a-1".into()), Err(ProviderError::Unavailable { reason: "already settled".into() }));
    trace.event(deltas(vec![
        Delta::BlockUpdated { turn_id: "t-1".into(), block_index: 0, block: approval("a-1", ApprovalState::Approving) },
        finished("t-1", 0),
    ]));
    trace
}

fn closed() -> Expect {
    Expect { closed: true, wire_usage: None }
}

/// One planted fault: the invariant it breaks, its trace, and words its
/// message must carry.
struct Fault {
    invariant: &'static str,
    what: &'static str,
    trace: fn() -> (Trace, Expect),
    says: &'static str,
}

const FAULTS: &[Fault] = &[
    Fault {
        invariant: invariants::TAP_OFFERS_THE_CARDS_ANSWERS,
        what: "a tap offering an answer its card does not carry",
        trace: || {
            let mut trace = opened();
            let mut card = approval("a-1", ApprovalState::Pending);
            if let Block::Approval { choices, .. } = &mut card {
                *choices = vec![provider::ApprovalAnswer::Once.choice("allow")];
            }
            trace.event(ProviderEvent::TurnRunning { session_id: SESSION.into(), turn_id: "t-1".into() });
            trace.event(deltas(vec![assistant("t-1"), Delta::BlockAdded { turn_id: "t-1".into(), block: card }]));
            trace.event(ProviderEvent::ApprovalRequested {
                session_id: SESSION.into(),
                approval_id: "a-1".into(),
                headline: "ls".into(),
                options: vec![
                    provider::ApprovalOption { answer: provider::ApprovalAnswer::Once, choice: "allow".into() },
                    provider::ApprovalOption { answer: provider::ApprovalAnswer::ThisSession, choice: "always".into() },
                ],
            });
            trace.event(deltas(vec![finished("t-1", 0)]));
            trace.event(settled("t-1"));
            (trace, closed())
        },
        says: "which its card does not carry",
    },
    Fault {
        invariant: invariants::ACK_BEFORE_OR_WITH_EVENTS,
        what: "an ack naming a turn that streamed before the submit left",
        trace: || {
            let mut trace = Trace::default();
            trace.sent(Sent::Open);
            trace.acked(Sent::Open, Ok(Ack::Session { session_id: SESSION.into(), title: None }));
            trace.event(deltas(vec![assistant("t-old"), text("t-old"), finished("t-old", 0)]));
            trace.sent(Sent::Submit);
            trace.acked(Sent::Submit, Ok(Ack::TurnAccepted { turn_id: "t-old".into(), queued: false }));
            (trace, closed())
        },
        says: "before the submit left",
    },
    Fault {
        invariant: invariants::ACK_BEFORE_OR_WITH_EVENTS,
        what: "a TurnIdentified bridging an id no ack returned",
        trace: || {
            let mut trace = clean();
            trace.event(ProviderEvent::TurnIdentified {
                session_id: SESSION.into(),
                accepted: "t-never-acked".into(),
                turn_id: "t-9".into(),
            });
            (trace, closed())
        },
        says: "no ack returned",
    },
    Fault {
        invariant: invariants::ACK_BEFORE_OR_WITH_EVENTS,
        what: "an admitted turn that never streams",
        trace: || (opened(), closed()),
        says: "no event ever opened it",
    },
    Fault {
        invariant: invariants::ONE_TERMINAL_PER_TURN,
        what: "a duplicate TurnFinished",
        trace: || {
            let mut trace = clean();
            trace.event(deltas(vec![finished("t-1", 0)]));
            (trace, closed())
        },
        says: "finished twice",
    },
    Fault {
        invariant: invariants::ONE_TERMINAL_PER_TURN,
        what: "a duplicate TurnSettled",
        trace: || {
            let mut trace = clean();
            trace.event(settled("t-1"));
            (trace, closed())
        },
        says: "settled twice",
    },
    Fault {
        invariant: invariants::ONE_TERMINAL_PER_TURN,
        what: "a turn that ran and never ended",
        trace: || {
            let mut trace = opened();
            trace.event(ProviderEvent::TurnRunning { session_id: SESSION.into(), turn_id: "t-1".into() });
            trace.event(deltas(vec![assistant("t-1"), text("t-1")]));
            (trace, closed())
        },
        says: "never ended",
    },
    Fault {
        invariant: invariants::APPROVAL_CARD_BEFORE_TAP,
        what: "an approval with no card",
        trace: || {
            let mut trace = clean();
            trace.event(tap("a-ghost"));
            (trace, closed())
        },
        says: "before any delta carried its card",
    },
    Fault {
        invariant: invariants::APPROVAL_ANSWERED_ONCE,
        what: "a second decision answered Ok",
        trace: || {
            let mut trace = clean_with_approval();
            trace.sent(Sent::Decide("a-1".into()));
            trace.acked(Sent::Decide("a-1".into()), Ok(Ack::Accepted));
            (trace, closed())
        },
        says: "answered twice",
    },
    Fault {
        invariant: invariants::APPROVAL_ANSWERED_ONCE,
        what: "an approval asked twice",
        trace: || {
            let mut trace = clean_with_approval();
            trace.event(tap("a-1"));
            (trace, closed())
        },
        says: "asked twice",
    },
    Fault {
        invariant: invariants::APPROVAL_ANSWERED_ONCE,
        what: "a settled card going back to Pending",
        trace: || {
            let mut trace = clean_with_approval();
            trace.event(deltas(vec![Delta::BlockUpdated {
                turn_id: "t-1".into(),
                block_index: 0,
                block: approval("a-1", ApprovalState::Pending),
            }]));
            (trace, closed())
        },
        says: "back to Pending",
    },
    Fault {
        invariant: invariants::LOST_LEAVES_NOTHING_PENDING,
        what: "a loss with the approval still Pending",
        trace: || {
            let mut trace = opened();
            trace.event(deltas(vec![assistant("t-1"), Delta::BlockAdded {
                turn_id: "t-1".into(),
                block: approval("a-1", ApprovalState::Pending),
            }]));
            trace.event(tap("a-1"));
            trace.event(deltas(vec![finished("t-1", 0)]));
            trace.event(ProviderEvent::ConnectionLost { reason: "the child exited".into() });
            (trace, closed())
        },
        says: "left approval \"a-1\" Pending",
    },
    Fault {
        invariant: invariants::LOST_LEAVES_NOTHING_PENDING,
        what: "a loss with the turn still open",
        trace: || {
            let mut trace = opened();
            trace.event(deltas(vec![assistant("t-1"), text("t-1")]));
            trace.event(ProviderEvent::ConnectionLost { reason: "the child exited".into() });
            trace.event(ProviderEvent::ConnectionRestored);
            trace.event(deltas(vec![finished("t-1", 0)]));
            (trace, closed())
        },
        says: "left turn \"t-1\" open",
    },
    Fault {
        invariant: invariants::LOST_LEAVES_NOTHING_PENDING,
        what: "a ListPending after the loss still listing the dead child's approval",
        trace: || {
            let mut trace = opened();
            trace.event(deltas(vec![assistant("t-1"), Delta::BlockAdded {
                turn_id: "t-1".into(),
                block: approval("a-1", ApprovalState::Pending),
            }]));
            trace.event(tap("a-1"));
            trace.event(deltas(vec![
                Delta::BlockUpdated {
                    turn_id: "t-1".into(),
                    block_index: 0,
                    block: approval("a-1", ApprovalState::Cancelled { reason: None }),
                },
                finished("t-1", 0),
            ]));
            trace.event(ProviderEvent::ConnectionLost { reason: "the child exited".into() });
            trace.sent(Sent::ListPending);
            trace.acked(
                Sent::ListPending,
                Ok(Ack::PendingWork {
                    approvals: vec![provider::PendingApproval {
                        id: "a-1".into(),
                        session_id: SESSION.into(),
                        headline: "ls".into(),
                        stage_token: None,
                    }],
                    questions: Vec::new(),
                }),
            );
            (trace, closed())
        },
        says: "still lists approval",
    },
    Fault {
        invariant: invariants::BLOCK_INDEX_IN_RANGE,
        what: "a delta naming a block past the end",
        trace: || {
            let mut trace = clean();
            trace.event(deltas(vec![Delta::TextDelta { turn_id: "t-1".into(), block_index: 3, text: "x".into() }]));
            (trace, closed())
        },
        says: "names block 3 of turn \"t-1\", which has 1 block(s)",
    },
    Fault {
        invariant: invariants::BLOCK_INDEX_IN_RANGE,
        what: "a text delta aimed at an approval card",
        trace: || {
            let mut trace = opened();
            trace.event(deltas(vec![
                assistant("t-1"),
                Delta::BlockAdded { turn_id: "t-1".into(), block: approval("a-1", ApprovalState::AutoAllowed { rule: "r".into() }) },
                Delta::TextDelta { turn_id: "t-1".into(), block_index: 0, text: "x".into() },
                finished("t-1", 0),
            ]));
            (trace, closed())
        },
        says: "wants a text block at index 0 of turn \"t-1\", which holds a block of kind approval",
    },
    Fault {
        invariant: invariants::BLOCK_INDEX_IN_RANGE,
        what: "a delta naming a turn that never opened",
        trace: || {
            let mut trace = clean();
            trace.event(deltas(vec![Delta::BlockUpdated {
                turn_id: "t-nowhere".into(),
                block_index: 0,
                block: Block::Text { text: "x".into(), streaming: false },
            }]));
            (trace, closed())
        },
        says: "never opened",
    },
    Fault {
        invariant: invariants::USAGE_PER_TURN,
        what: "a cumulative figure leaking into a turn",
        trace: || {
            let mut trace = clean();
            trace.sent(Sent::Submit);
            trace.acked(Sent::Submit, Ok(Ack::TurnAccepted { turn_id: "t-2".into(), queued: false }));
            // The wire said 5 then 7; the adapter reported the running
            // total (12) as the second turn's own.
            trace.event(deltas(vec![assistant("t-2"), text("t-2"), finished("t-2", 12)]));
            let wire = Usage { tokens_in: None, tokens_out: Some(12), cost_usd: None };
            (trace, Expect { closed: true, wire_usage: Some(wire) })
        },
        says: "tokens_out: turns sum to 17, the wire reported 12",
    },
    Fault {
        invariant: invariants::RUNNING_BEFORE_CONTENT,
        what: "a TurnRunning after the turn already spoke",
        trace: || {
            let mut trace = opened();
            trace.event(deltas(vec![assistant("t-1"), text("t-1")]));
            trace.event(ProviderEvent::TurnRunning { session_id: SESSION.into(), turn_id: "t-1".into() });
            trace.event(deltas(vec![finished("t-1", 0)]));
            (trace, closed())
        },
        says: "came after the turn spoke",
    },
    Fault {
        invariant: invariants::QUESTION_CARD_WITH_TAP,
        what: "a question with no card",
        trace: || {
            let mut trace = clean();
            trace.event(ProviderEvent::QuestionRaised {
                session_id: SESSION.into(),
                question_id: "q-ghost".into(),
                headline: "which?".into(),
            });
            (trace, closed())
        },
        says: "has no Question card",
    },
    Fault {
        invariant: invariants::RESTORED_FOLLOWS_LOST,
        what: "a ConnectionRestored with nothing lost",
        trace: || {
            let mut trace = clean();
            trace.event(ProviderEvent::ConnectionRestored);
            (trace, closed())
        },
        says: "no ConnectionLost before it",
    },
    Fault {
        invariant: invariants::EVENTS_NAME_THEIR_SESSION,
        what: "an event for another session on this session's stream",
        trace: || {
            let mut trace = clean();
            trace.event(ProviderEvent::Notice { session_id: "s-other".into(), text: "hello".into() });
            (trace, closed())
        },
        says: "names session \"s-other\"",
    },
];

#[test]
fn the_clean_traces_keep_every_invariant() {
    for trace in [clean(), clean_with_approval()] {
        let violations = check(&trace, &closed());
        assert!(violations.is_empty(), "{violations:#?}\n{}", invariants::describe(&trace));
    }
}

/// Every planted fault fails exactly its own invariant, with words that say
/// what broke.
#[test]
fn every_seeded_fault_fails_its_invariant_with_a_clear_message() {
    for fault in FAULTS {
        let (trace, expect) = (fault.trace)();
        let violations = check(&trace, &expect);
        let listed: Vec<String> = violations.iter().map(ToString::to_string).collect();
        assert!(
            violations.iter().any(|violation| violation.invariant == fault.invariant && violation.message.contains(fault.says)),
            "{} ({}): expected [{}] saying {:?}, got:\n{}",
            fault.what,
            fault.invariant,
            fault.invariant,
            fault.says,
            listed.join("\n")
        );
        assert!(
            violations.iter().all(|violation| violation.invariant == fault.invariant),
            "{}: only [{}] fails, got:\n{}",
            fault.what,
            fault.invariant,
            listed.join("\n")
        );
    }
}

/// No invariant goes unseeded: a new one needs a fault that proves it bites.
#[test]
fn every_invariant_has_a_seeded_fault() {
    for invariant in invariants::INVARIANTS {
        assert!(FAULTS.iter().any(|fault| fault.invariant == invariant), "[{invariant}] has no seeded fault in FAULTS");
    }
}

fn planted(name: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance/planted").join(name)
}

/// A planted wire fixture with the turn's `turn/completed` sent twice
/// travels the real Muse hub and comes out flagged: the suite sees the
/// duplicate terminal end to end.
#[test]
fn a_planted_duplicate_terminal_is_caught_through_the_muse_hub() {
    let case = replay::muse_case(&planted("msp-duplicate-terminal.jsonl"));
    let (trace, violations) = replay::run(Agent::Muse, &case);
    assert!(
        violations.iter().any(|violation| violation.invariant == invariants::ONE_TERMINAL_PER_TURN
            && violation.message.contains("twice")),
        "expected the duplicate terminal flagged, got {violations:#?}\n{}",
        invariants::describe(&trace)
    );
}


/// The same fault on Codex's wire: `basic.jsonl` with its `turn/completed`
/// sent twice, played by the stand-in app-server through the real adapter.
#[test]
fn a_planted_duplicate_terminal_is_caught_through_the_codex_adapter() {
    let case = replay::codex_cases_in(&planted("codex-duplicate-terminal.jsonl")).remove(0);
    let (trace, violations) = replay::run(Agent::Codex, &case);
    assert!(
        violations.iter().any(|violation| violation.invariant == invariants::ONE_TERMINAL_PER_TURN
            && violation.message.contains("finished twice")),
        "expected the duplicate terminal flagged, got {violations:#?}\n{}",
        invariants::describe(&trace)
    );
}
