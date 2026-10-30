//! The provider conformance suite (docs/71 §4): one scenario list, run
//! against all three adapters, asserting the SEAM's contract — the ordered
//! `ProviderEvent`s a view depends on, approvals settled exactly once, a
//! lost connection that leaves nothing pending and a session that revives,
//! typed refusals where a capability is absent — never an adapter's
//! internals.
//!
//! Every adapter runs against a fake child over a real pipe, never a real
//! CLI and never a model:
//!
//! * Claude Code and Codex spawn `tests/fixtures/conformance/fake_agent.py`,
//!   which replays a recorded fixture (`fixtures/claude-code/*.jsonl`,
//!   `fixtures/codex/*.jsonl`) to whatever the adapter asks, and can die on
//!   cue with stderr (the scripted kill);
//! * Muse spawns the `provider-muse` puppet
//!   (`crates/provider-muse/tests/fixtures/model_list_puppet.py`) through
//!   the hub, with its `--turn-shape` for approvals, failures, interrupts and
//!   the crash.
//!
//! Lives in `baaz`'s tests because baaz is the one crate that already
//! depends on all three adapters: a new dev crate would change the lock
//! file for nothing. Hand-written fixtures (no recorded one existed) sit
//! beside the fake and say so in their first lines; docs/71 §6 lists them.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aui_protocol::{ApprovalState, Block, Delta, Turn};
use provider::{
    Ack, Capability, CapabilityState, Command, ConnectInfo, Provider, ProviderAdapter, ProviderError, ProviderEvent,
    RefusalHint, SubmissionPart,
};

/// How long any one wait may take: a fake answers in milliseconds.
const WAIT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Agent {
    ClaudeCode,
    Codex,
    Muse,
}

const AGENTS: [Agent; 3] = [Agent::ClaudeCode, Agent::Codex, Agent::Muse];

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(kind: &str, name: &str) -> String {
    repo().join("fixtures").join(kind).join(name).to_string_lossy().into_owned()
}

fn hand_written(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/conformance")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

/// A fresh scratch dir under the temp dir, removed when the rig drops.
fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "baaz-conformance-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|at| at.as_nanos()).unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

fn executable(path: &Path, body: &str) {
    std::fs::write(path, body).expect("wrapper writes");
    use std::os::unix::fs::PermissionsExt as _;
    let mut perms = std::fs::metadata(path).expect("wrapper metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).expect("wrapper executable");
}

/// What one spawn of the fake child plays: a fixture spec for Claude Code
/// and Codex (`FIXTURE[@SEGMENT][#MODE]`, see `fake_agent.py`), the puppet's
/// flags for Muse.
type Plan<'a> = &'a [&'a str];

/// One adapter on its fake child, with its session.
struct Rig {
    provider: Provider,
    events: crossbeam_channel::Receiver<ProviderEvent>,
    session: String,
    dir: PathBuf,
    hub: Option<provider_muse::MuseHub>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.provider.shutdown();
        if let Some(hub) = &self.hub {
            hub.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The program the adapter spawns: a shell wrapper around the fake, so
/// each test's plans and state travel in argv, never the shared env.
fn wrapper(agent: Agent, dir: &Path, plans: Plan) -> PathBuf {
    let state = dir.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    let program = dir.join(match agent {
        Agent::ClaudeCode => "claude",
        Agent::Codex => "codex",
        Agent::Muse => "muse",
    });
    let body = match agent {
        Agent::ClaudeCode | Agent::Codex => {
            let fake = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/conformance/fake_agent.py");
            let dialect = if agent == Agent::Codex { "codex" } else { "claude" };
            let plans: String = plans.iter().map(|plan| format!(" --plan '{plan}'")).collect();
            format!(
                "#!/bin/sh\nexec /usr/bin/env python3 '{}' --dialect {dialect} --state '{}'{plans} \"$@\"\n",
                fake.display(),
                state.display()
            )
        }
        Agent::Muse => {
            let puppet =
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../provider-muse/tests/fixtures/model_list_puppet.py");
            let mut cases = String::new();
            for (at, flags) in plans.iter().enumerate() {
                let label = if at + 1 == plans.len() { "*".to_owned() } else { at.to_string() };
                cases.push_str(&format!("  {label}) FLAGS='{flags}' ;;\n"));
            }
            format!(
                "#!/bin/sh\nN=$(cat '{s}/spawns' 2>/dev/null || echo 0)\necho $((N+1)) > '{s}/spawns'\ncase $N in\n{cases}esac\nexec /usr/bin/env python3 '{}' $FLAGS \"$@\"\n",
                puppet.display(),
                s = state.display()
            )
        }
    };
    executable(&program, &body);
    program
}

fn info() -> ConnectInfo {
    ConnectInfo::new("baaz", "0.0.0-conformance")
}

/// A connected provider for `agent` over its fake, not yet in a session.
fn connect(agent: Agent, tag: &str, plans: Plan) -> (Provider, PathBuf, Option<provider_muse::MuseHub>) {
    let dir = scratch(tag);
    let program = wrapper(agent, &dir, plans);
    let program = program.to_string_lossy().into_owned();
    match agent {
        Agent::ClaudeCode => {
            let home = dir.join("owner");
            std::fs::create_dir_all(home.join(".claude")).expect("owner home");
            let adapter = provider_claude_code::ClaudeCodeAdapter::new(&program)
                .with_home(home)
                .with_config_dir(dir.join("claude-home"));
            let mut provider = Provider::new(adapter);
            provider.connect(&info()).expect("the fake answers --version");
            (provider, dir, None)
        }
        Agent::Codex => {
            let home = dir.join("owner");
            std::fs::create_dir_all(home.join(".codex")).expect("owner home");
            let adapter = provider_codex::CodexAdapter::new(&program).with_home(home);
            adapter.set_state_dir(dir.join("state-dir"));
            let mut provider = Provider::new(adapter);
            provider.connect(&info()).expect("the fake answers --version");
            (provider, dir, None)
        }
        Agent::Muse => {
            let hub = provider_muse::MuseHub::connect(&program, &info()).expect("the puppet shakes hands");
            let mut lane = hub.lane();
            lane.connect(&info()).expect("a lane reports the handshake");
            (Provider::new(lane), dir, Some(hub))
        }
    }
}

/// Open a fresh session on `agent`'s fake.
fn open(agent: Agent, tag: &str, plans: Plan) -> Rig {
    let (provider, dir, hub) = connect(agent, tag, plans);
    let workspace = dir.join("work");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let events = provider.events();
    let ack = provider
        .send(Command::OpenSession {
            request_id: provider::new_request_id(),
            workspace: Some(workspace.to_string_lossy().into_owned()),
            model: None,
            model_provider: None,
            mode: None,
        })
        .unwrap_or_else(|error| panic!("{agent:?} opens: {error}"));
    let Ack::Session { session_id, .. } = ack else { panic!("{agent:?} open answered {ack:?}") };
    Rig { provider, events, session: session_id, dir, hub }
}

impl Rig {
    fn submit(&self, text: &str) -> Result<Ack, ProviderError> {
        self.provider.send(Command::SubmitInput {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            parts: vec![SubmissionPart::Text(text.into())],
            display_text: None,
            effort: None,
        })
    }

    /// Every event until `done` holds over what was seen (or [`WAIT`]).
    fn until(&self, done: impl Fn(&[ProviderEvent]) -> bool) -> Vec<ProviderEvent> {
        let mut seen = Vec::new();
        let deadline = Instant::now() + WAIT;
        while !done(&seen) && Instant::now() < deadline {
            if let Ok(event) = self.events.recv_timeout(Duration::from_millis(50)) {
                seen.push(event);
            }
        }
        seen
    }

    /// What the adapter has delivered by now, waiting `quiet` for more.
    fn drain(&self, quiet: Duration) -> Vec<ProviderEvent> {
        let mut seen = Vec::new();
        while let Ok(event) = self.events.recv_timeout(quiet) {
            seen.push(event);
        }
        seen
    }

    fn decide(&self, approval: &str, choice: &str) -> Result<Ack, ProviderError> {
        self.provider.send(Command::DecideApproval {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            approval: approval.to_owned(),
            choice: choice.to_owned(),
            stage_token: None,
            feedback: None,
        })
    }

    /// The fake's client frames for spawn `n`, as JSON (Claude Code and
    /// Codex only).
    fn frames_of_spawn(&self, n: u32) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.dir.join("state/frames.log"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(spawn, _)| spawn.parse::<u32>().ok() == Some(n))
            .filter_map(|(_, frame)| serde_json::from_str(frame).ok())
            .collect()
    }
}

// --------------------------------------------------------- the contract

fn deltas(events: &[ProviderEvent]) -> Vec<(usize, &Delta)> {
    events
        .iter()
        .enumerate()
        .filter_map(|(at, event)| match event {
            ProviderEvent::Deltas { deltas, .. } => Some(deltas.iter().map(move |delta| (at, delta))),
            _ => None,
        })
        .flatten()
        .collect()
}

/// Whether a turn has finished in `events`: the transcript's finish, or —
/// for an adapter that reports turn lifecycle (Muse), whose interrupted
/// turn may have said nothing to finish — its `TurnSettled`.
fn finished(events: &[ProviderEvent]) -> bool {
    deltas(events).iter().any(|(_, delta)| matches!(delta, Delta::TurnFinished { .. }))
        || events.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { .. }))
}

fn lost(events: &[ProviderEvent]) -> bool {
    events.iter().any(|event| matches!(event, ProviderEvent::ConnectionLost { .. }))
}

fn restored(events: &[ProviderEvent]) -> bool {
    events.iter().any(|event| matches!(event, ProviderEvent::ConnectionRestored))
}

/// The turn contract every view stands on, in order: the assistant turn
/// opens (an assistant `TurnStarted`, or `TurnRunning` before it) before
/// any content of it lands; its content lands before its `TurnFinished`;
/// and nothing of that turn follows its finish. Every delta names the
/// session it belongs to.
fn assert_turn_order(agent: Agent, session: &str, events: &[ProviderEvent]) {
    let all = deltas(events);
    let finish = all
        .iter()
        .rev()
        .find_map(|(at, delta)| match delta {
            Delta::TurnFinished { turn_id, .. } => Some((*at, turn_id.clone())),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{agent:?}: the turn never finished: {events:#?}"));
    let (finish_at, turn) = finish;
    let start_at = all
        .iter()
        .find_map(|(at, delta)| match delta {
            Delta::TurnStarted { turn: Turn::Assistant { id, .. } } if *id == turn => Some(*at),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{agent:?}: turn {turn} finished without starting"));
    assert!(start_at <= finish_at, "{agent:?}: the start precedes the finish");
    let content: Vec<usize> = all
        .iter()
        .filter_map(|(at, delta)| match delta {
            Delta::BlockAdded { turn_id, .. } | Delta::BlockUpdated { turn_id, .. } if *turn_id == turn => Some(*at),
            _ => None,
        })
        .collect();
    assert!(!content.is_empty(), "{agent:?}: the turn said something");
    assert!(content.iter().all(|at| *at >= start_at && *at <= finish_at), "{agent:?}: content inside its turn");
    for event in events {
        if let ProviderEvent::Deltas { session_id: Some(id), .. }
        | ProviderEvent::TurnSettled { session_id: id, .. }
        | ProviderEvent::ApprovalRequested { session_id: id, .. } = event
        {
            assert_eq!(id, session, "{agent:?}: every event names this session");
        }
    }
}

fn taps(events: &[ProviderEvent]) -> Vec<String> {
    events
        .iter()
        .filter_map(|event| match event {
            ProviderEvent::ApprovalRequested { approval_id, .. } => Some(approval_id.clone()),
            _ => None,
        })
        .collect()
}

/// Whether some approval card left `Pending` in `events`.
fn a_card_settled(events: &[ProviderEvent]) -> bool {
    deltas(events).iter().any(|(_, delta)| {
        matches!(delta, Delta::BlockUpdated { block: Block::Approval { state, .. }, .. } if *state != ApprovalState::Pending)
    })
}

/// A refusal of something no longer pending, in any of the typed shapes
/// the seam has for it — never `Ok`, never an untyped failure.
fn refused_as_gone(error: &ProviderError) -> bool {
    error.is_unknown_approval()
        || matches!(error.hint(), RefusalHint::AlreadySettled | RefusalHint::ItemGone)
        || matches!(error, ProviderError::Unavailable { .. })
}

// --------------------------------------------------------- the scenarios

/// Open + first turn, a text answer.
#[test]
fn open_and_a_first_text_turn() {
    for agent in AGENTS {
        let plan: &[&str] = match agent {
            Agent::ClaudeCode => &[],
            Agent::Codex => &[],
            Agent::Muse => &["--emit-turn"],
        };
        let claude = [fixture("claude-code", "basic.jsonl")];
        let codex = [fixture("codex", "basic.jsonl")];
        let owned: Vec<&str> = match agent {
            Agent::ClaudeCode => claude.iter().map(String::as_str).collect(),
            Agent::Codex => codex.iter().map(String::as_str).collect(),
            Agent::Muse => plan.to_vec(),
        };
        let rig = open(agent, "basic", &owned);
        let ack = rig.submit("hello").unwrap_or_else(|error| panic!("{agent:?} takes the turn: {error}"));
        assert!(matches!(ack, Ack::TurnAccepted { queued: false, .. }), "{agent:?}: {ack:?}");
        let events = rig.until(finished);
        assert_turn_order(agent, &rig.session, &events);
        assert!(!lost(&events), "{agent:?}: no loss on a clean turn");
    }
}

/// A tool call (read/search): the tool's card lands inside the turn.
/// Muse's puppet has no tool shape: covered by `provider-muse`'s own
/// fold tests on the msp fixtures.
#[test]
fn a_tool_call_lands_inside_its_turn() {
    for agent in [Agent::ClaudeCode, Agent::Codex] {
        let plan = match agent {
            Agent::ClaudeCode => fixture("claude-code", "read-search.jsonl"),
            _ => fixture("codex", "read-search.jsonl"),
        };
        let rig = open(agent, "tool", &[&plan]);
        rig.submit("find it").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
        let events = rig.until(finished);
        assert_turn_order(agent, &rig.session, &events);
        let tools = deltas(&events)
            .iter()
            .filter(|(_, delta)| {
                matches!(delta, Delta::BlockAdded { block: Block::ToolCall { .. }, .. }
                    | Delta::BlockUpdated { block: Block::ToolCall { .. }, .. })
            })
            .count();
        assert!(tools > 0, "{agent:?}: a tool card: {events:#?}");
    }
}

/// An approval asked, then allowed or denied: one tap, one decision
/// accepted, a second decision for the same id refused as already settled
/// (exactly once), and the turn finishes.
#[test]
fn an_approval_is_asked_once_and_settles_exactly_once() {
    let cases: Vec<(Agent, String, &str)> = vec![
        (Agent::ClaudeCode, fixture("claude-code", "permission.jsonl"), "allow"),
        (Agent::ClaudeCode, fixture("claude-code", "permission-deny.jsonl"), "deny"),
        (Agent::Codex, fixture("codex", "approval.jsonl"), "accept"),
        (Agent::Codex, fixture("codex", "approval.jsonl"), "decline"),
        (Agent::Muse, "--turn-shape=approval".to_owned(), "c-yes"),
    ];
    for (agent, plan, choice) in cases {
        let rig = open(agent, "approval", &[&plan]);
        rig.submit("do the thing").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
        let asked = rig.until(|seen| !taps(seen).is_empty());
        let ids = taps(&asked);
        assert_eq!(ids.len(), 1, "{agent:?} asks once: {asked:#?}");
        let approval = ids[0].clone();
        let pending = rig.provider.send(Command::ListPending { session_id: rig.session.clone() });
        if let Ok(Ack::PendingWork { approvals, .. }) = &pending {
            assert!(approvals.iter().any(|item| item.id == approval), "{agent:?}: listed while it waits");
        }
        rig.decide(&approval, choice).unwrap_or_else(|error| panic!("{agent:?} {choice}: {error}"));
        let again = rig.decide(&approval, choice).expect_err("a second decision is refused");
        assert!(refused_as_gone(&again), "{agent:?}: typed as settled, got {again:?}");
        let rest = rig.until(finished);
        assert!(finished(&rest), "{agent:?} {choice}: the turn ends: {rest:#?}");
        assert!(taps(&rest).is_empty(), "{agent:?}: never asked twice");
        if agent != Agent::Muse {
            let mut whole = asked.clone();
            whole.extend(rest);
            assert!(a_card_settled(&whole), "{agent:?} {choice}: the card left Pending");
        }
    }
}

/// Interrupt mid-turn: accepted and the turn ends — or, where the adapter
/// cannot stop a turn, a typed refusal (never `Ok` and nothing).
#[test]
fn an_interrupt_ends_the_turn_or_is_refused_typed() {
    for agent in AGENTS {
        let plan = match agent {
            Agent::ClaudeCode => fixture("claude-code", "basic.jsonl"),
            Agent::Codex => fixture("codex", "interrupt.jsonl"),
            Agent::Muse => "--turn-shape=open".to_owned(),
        };
        let rig = open(agent, "interrupt", &[&plan]);
        rig.submit("a long one").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
        let started = rig.until(|seen| {
            seen.iter().any(|event| matches!(event, ProviderEvent::TurnRunning { .. }))
                || deltas(seen).iter().any(|(_, delta)| matches!(delta, Delta::TurnStarted { turn: Turn::Assistant { .. } }))
        });
        let stop = rig.provider.send(Command::InterruptTurn {
            request_id: provider::new_request_id(),
            session_id: rig.session.clone(),
            turn: None,
            retract: false,
        });
        match stop {
            Ok(_) => {
                let rest = rig.until(finished);
                let mut whole = started;
                whole.extend(rest);
                assert!(finished(&whole), "{agent:?}: the interrupted turn ends: {whole:#?}");
            }
            Err(error) => {
                assert!(
                    matches!(error, ProviderError::Rejected { .. } | ProviderError::Unsupported { .. }),
                    "{agent:?}: an interrupt it cannot do is refused typed, got {error:?}"
                );
                assert_ne!(
                    rig.provider.capabilities().state(Capability::TurnControl),
                    &CapabilityState::Native,
                    "{agent:?}: a Native turn control never refuses"
                );
            }
        }
    }
}

/// Resume a stored session: its history replays before the ack returns
/// (Claude Code from its stored transcript, Codex from `thread/resume`'s
/// turns); Muse re-attaches metadata-only and pages history later
/// (`pages_history`), so its contract is the ack on the same id and a
/// working turn after it.
#[test]
fn a_resumed_session_replays_its_history() {
    for agent in AGENTS {
        let stored = "af18b5bb-1ff5-4ba2-825c-94804b78834e";
        let plan = match agent {
            Agent::ClaudeCode => fixture("claude-code", "basic.jsonl"),
            Agent::Codex => format!("{}@1", fixture("codex", "resume.jsonl")),
            Agent::Muse => "--emit-turn".to_owned(),
        };
        let (provider, dir, hub) = connect(agent, "resume", &[&plan]);
        let session = match agent {
            Agent::ClaudeCode => {
                let slug = dir.join("claude-home/projects/-conformance");
                std::fs::create_dir_all(&slug).expect("slug dir");
                std::fs::copy(fixture("claude-code", "stored-history.jsonl"), slug.join(format!("{stored}.jsonl")))
                    .expect("stored transcript");
                stored.to_owned()
            }
            Agent::Codex => "01a0de81-0a42-7462-9176-0ff61cb96b77".to_owned(),
            Agent::Muse => "s-resumed".to_owned(),
        };
        let events = provider.events();
        let ack = provider
            .send(Command::ResumeSession {
                request_id: provider::new_request_id(),
                session_id: session.clone(),
                cursor: None,
                metadata_only: agent == Agent::Muse,
                mode: None,
            })
            .unwrap_or_else(|error| panic!("{agent:?} resumes: {error}"));
        assert!(matches!(&ack, Ack::Session { session_id, .. } if *session_id == session), "{agent:?}: {ack:?}");
        let rig = Rig { provider, events, session: session.clone(), dir, hub };
        let replayed = rig.drain(Duration::from_millis(300));
        if agent == Agent::Muse {
            rig.submit("after the resume").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
            let turn = rig.until(finished);
            assert_turn_order(agent, &session, &turn);
            continue;
        }
        let users = deltas(&replayed)
            .iter()
            .filter(|(_, delta)| matches!(delta, Delta::TurnStarted { turn: Turn::User { .. } }))
            .count();
        assert!(users > 0, "{agent:?}: the stored ask replays: {replayed:#?}");
        assert!(finished(&replayed), "{agent:?}: the stored answer replays finished");
        assert!(!lost(&replayed), "{agent:?}: a resume loses nothing");
    }
}

/// An error turn: the turn ends, then settles `Failed` with the
/// provider's own words — what the row's "Failed" stands on.
#[test]
fn an_error_turn_settles_failed_with_its_words() {
    for agent in AGENTS {
        let plan = match agent {
            Agent::ClaudeCode => hand_written("claude-failed-turn.jsonl"),
            Agent::Codex => hand_written("codex-failed-turn.jsonl"),
            Agent::Muse => "--turn-shape=failed".to_owned(),
        };
        let rig = open(agent, "error", &[&plan]);
        rig.submit("this fails").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
        let events = rig.until(|seen| {
            seen.iter().any(|event| {
                matches!(event, ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Failed { .. }, .. })
            })
        });
        let failure = events.iter().position(|event| {
            matches!(event, ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Failed { .. }, .. })
        });
        let failure = failure.unwrap_or_else(|| panic!("{agent:?}: the failure settles: {events:#?}"));
        let ProviderEvent::TurnSettled { outcome: provider::facts::TurnOutcome::Failed { message, signed_out }, .. } =
            &events[failure]
        else {
            unreachable!()
        };
        assert!(!signed_out, "{agent:?}: not a sign-in failure");
        let message = message.clone().unwrap_or_default();
        assert!(
            message.contains("simulated") || message.contains("closed the stream"),
            "{agent:?}: the provider's own words: {message:?}"
        );
        if agent != Agent::Muse {
            let finish = events.iter().position(|event| {
                matches!(event, ProviderEvent::Deltas { deltas, .. } if deltas.iter().any(|delta| matches!(delta, Delta::TurnFinished { .. })))
            });
            assert!(finish.is_some_and(|at| at < failure), "{agent:?}: the finish lands before the failure");
        }
    }
}

/// The child dies mid-turn with an approval waiting (the scripted kill):
/// the turn closes and the approval settles before the loss is reported;
/// after it nothing is pending and a late decision is refused typed; the
/// child's stderr is there for the failure view; and the session revives —
/// on the next send (Claude Code, Codex) or the hub's respawn (Muse) — and
/// the next turn runs.
#[test]
fn a_child_crash_settles_everything_and_the_session_revives() {
    for agent in AGENTS {
        let (first, then) = match agent {
            Agent::ClaudeCode => (
                format!("{}#crash-at-request", fixture("claude-code", "permission.jsonl")),
                fixture("claude-code", "basic.jsonl"),
            ),
            Agent::Codex => (
                format!("{}#crash-at-request", fixture("codex", "approval.jsonl")),
                format!("{}@1", fixture("codex", "resume.jsonl")),
            ),
            Agent::Muse => ("--turn-shape=crash".to_owned(), "--emit-turn".to_owned()),
        };
        let rig = open(agent, "crash", &[&first, &then]);
        rig.submit("this dies").unwrap_or_else(|error| panic!("{agent:?}: {error}"));
        let events = rig.until(lost);
        let lost_at = events
            .iter()
            .position(|event| matches!(event, ProviderEvent::ConnectionLost { .. }))
            .unwrap_or_else(|| panic!("{agent:?}: the loss is reported: {events:#?}"));
        let ids = taps(&events);
        if agent != Agent::Muse {
            assert_eq!(ids.len(), 1, "{agent:?}: the approval was waiting: {events:#?}");
            let before = &events[..lost_at];
            assert!(a_card_settled(before), "{agent:?}: the card settled before the loss");
            assert!(finished(before), "{agent:?}: the turn closed before the loss");
        }
        assert!(events[lost_at + 1..].is_empty() || !taps(&events[lost_at + 1..]).iter().any(|id| ids.contains(id)));
        // Nothing pending after the loss, and a late decision is refused.
        match rig.provider.send(Command::ListPending { session_id: rig.session.clone() }) {
            Ok(Ack::PendingWork { approvals, questions }) => {
                assert!(approvals.is_empty() && questions.is_empty(), "{agent:?}: nothing pending after the loss")
            }
            Ok(other) => panic!("{agent:?}: ListPending answered {other:?}"),
            Err(error) => assert!(matches!(error, ProviderError::Unavailable { .. }), "{agent:?}: {error:?}"),
        }
        for id in &ids {
            let late = rig.decide(id, if agent == Agent::Codex { "accept" } else { "allow" });
            let late = late.expect_err("a decision for the dead child's approval is refused");
            assert!(refused_as_gone(&late), "{agent:?}: typed, got {late:?}");
        }
        // The child's own last words, for the failure view.
        let stderr = rig.provider.stderr_tail();
        assert!(
            stderr.iter().any(|line| line.contains("simulated")),
            "{agent:?}: the stderr tail survives the child: {stderr:?}"
        );
        assert!(stderr.len() <= provider::STDERR_TAIL_LINES);
        // Revive.
        if let Some(hub) = &rig.hub {
            hub.reconnect().expect("the hub respawns the shared child");
        }
        rig.submit("again").unwrap_or_else(|error| panic!("{agent:?} revives on the next send: {error}"));
        let after = rig.until(|seen| finished(seen) && restored(seen));
        assert!(restored(&after), "{agent:?}: the connection is restored: {after:#?}");
        assert!(finished(&after), "{agent:?}: the revived session runs its turn: {after:#?}");
        if agent == Agent::Codex {
            let resumed = rig.frames_of_spawn(1).iter().any(|frame| frame["method"] == "thread/resume");
            assert!(resumed, "Codex revives with thread/resume on a fresh app-server");
        }
    }
}

/// A child that cannot start at all: the open fails typed and the child's
/// stderr is kept for the failure view (Codex; Claude Code's open returns
/// before its child speaks, so its death arrives as a lost connection,
/// covered above).
#[test]
fn a_child_that_cannot_start_fails_the_open_with_its_stderr() {
    let (provider, dir, _) = connect(Agent::Codex, "die", &["unused#die"]);
    let error = provider
        .send(Command::OpenSession {
            request_id: provider::new_request_id(),
            workspace: None,
            model: None,
            model_provider: None,
            mode: None,
        })
        .expect_err("a child that dies on its first breath fails the open");
    assert!(matches!(error, ProviderError::Unavailable { .. }), "{error:?}");
    let stderr = provider.stderr_tail();
    assert!(stderr.iter().any(|line| line.contains("simulated")), "the open keeps the child's words: {stderr:?}");
    drop(provider);
    let _ = std::fs::remove_dir_all(dir);
}

/// Where a capability is absent, the refusal is typed: every command whose
/// capability an adapter declares `Unavailable` is refused by the gate as
/// `Unsupported`, naming that capability.
#[test]
fn an_absent_capability_is_refused_typed() {
    let session = || "s-any".to_owned();
    let id = provider::new_request_id;
    let commands = vec![
        Command::ForkSession { request_id: id(), session_id: session(), through_turn: None, metadata_only: false, mode: None },
        Command::CompactSession { request_id: id(), session_id: session(), through_turn: None },
        Command::RunShell { request_id: id(), session_id: session(), command: "true".into() },
        Command::SteerInput {
            request_id: id(),
            session_id: session(),
            expected_turn: "t".into(),
            parts: vec![SubmissionPart::Text("x".into())],
            effort: None,
        },
        Command::AnswerQuestion { request_id: id(), session_id: session(), question: "q".into(), answers: Vec::new() },
        Command::ReadStoredOutput { session_id: session(), item: "i".into(), output: "o".into(), offset: 0, length: None },
        Command::BeginLogin { api_key: None },
        Command::LogOut,
    ];
    for agent in AGENTS {
        let plan: &str = match agent {
            Agent::Muse => "--emit-turn",
            _ => "unused",
        };
        let (provider, dir, hub) = connect(agent, "caps", &[plan]);
        let caps = provider.capabilities();
        let mut refused = 0;
        for command in &commands {
            if !matches!(caps.state(command.required_capability()), CapabilityState::Unavailable { .. }) {
                continue;
            }
            let error = provider.send(command.clone()).expect_err("an absent capability never answers Ok");
            assert!(error.is_unsupported(), "{agent:?} {}: typed refusal, got {error:?}", command.capability());
            assert_eq!(error.capability(), Some(command.capability()), "{agent:?}: names the capability");
            refused += 1;
        }
        // Inside a declared capability the adapter's own refusals are
        // typed too: Claude Code and Codex sign in outside the session,
        // and say so as `Unsupported` rather than answering `Ok`.
        if agent != Agent::Muse {
            let error = provider.send(Command::BeginLogin { api_key: None }).expect_err("no in-session sign-in");
            assert!(error.is_unsupported(), "{agent:?}: typed, got {error:?}");
            assert_eq!(error.capability(), Some("begin-login"));
            refused += 1;
        }
        assert!(refused > 0, "{agent:?}: at least one absent capability was exercised");
        drop(provider);
        if let Some(hub) = hub {
            hub.shutdown();
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
