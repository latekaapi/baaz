//! The adapter against a stand-in agent (`tests/fixtures/acp_agent.py`)
//! playing each hand-written recording in `fixtures/acp/` over a real
//! pipe: never a real agent and never a model (docs/82 5.7).
//!
//! What each recording proves, through the adapter's real child, pump and
//! fold: a plain answer; a permission allowed and denied (decided once, a
//! second decision refused); a plan refined in place; a cancel; a child that
//! dies with its request waiting, and the session that revives; a stored
//! session re-opened with `session/load`.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aui_protocol::{ApprovalState, Block, Delta, Session, TodoState, ToolKind, ToolStatus, Turn};
use provider::facts::TurnOutcome;
use provider::{Ack, Capability, CapabilityState, Command, ConnectInfo, Provider, ProviderEvent, SubmissionPart};
use provider_acp::AcpAdapter;

const WAIT: Duration = Duration::from_secs(15);

fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn fixture(name: &str) -> String {
    repo().join("fixtures/acp").join(name).to_string_lossy().into_owned()
}

/// A scratch dir and the stand-in's wrapper in it, playing `plans` (one
/// per spawn, the last repeating).
fn stand_in(tag: &str, plans: &[&str]) -> (PathBuf, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "baaz-acp-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|at| at.as_nanos()).unwrap_or(0)
    ));
    let state = dir.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    let agent = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/acp_agent.py");
    let plans: String = plans.iter().map(|plan| format!(" --plan '{plan}'")).collect();
    let program = dir.join("agent");
    std::fs::write(
        &program,
        format!("#!/bin/sh\nexec /usr/bin/env python3 '{}' --state '{}'{plans} \"$@\"\n", agent.display(), state.display()),
    )
    .expect("wrapper");
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).expect("executable");
    (dir, program)
}

struct Rig {
    provider: Provider,
    events: crossbeam_channel::Receiver<ProviderEvent>,
    session: String,
    seen: Vec<ProviderEvent>,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.provider.shutdown();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn connected(tag: &str, plans: &[&str], state_dir: Option<&Path>) -> (Provider, PathBuf) {
    let (dir, program) = stand_in(tag, plans);
    let mut adapter = AcpAdapter::new(&program.to_string_lossy(), &[]);
    if let Some(state) = state_dir {
        adapter = adapter.with_state_dir(state.to_path_buf());
    }
    let mut provider = Provider::new(adapter);
    let handshake = provider.connect(&ConnectInfo::new("baaz", "0.0.0-test")).expect("the stand-in shakes hands");
    assert_eq!(handshake.agent_name, "Stand-in Agent");
    (provider, dir)
}

fn open(tag: &str, plans: &[&str]) -> Rig {
    let (provider, dir) = connected(tag, plans, None);
    let events = provider.events();
    let ack = provider
        .send(Command::OpenSession {
            request_id: provider::new_request_id(),
            workspace: Some(dir.to_string_lossy().into_owned()),
            model: None,
            model_provider: None,
            mode: None,
        })
        .expect("the session opens");
    let Ack::Session { session_id, .. } = ack else { panic!("open answered {ack:?}") };
    Rig { provider, events, session: session_id, seen: Vec::new(), dir }
}

impl Rig {
    fn submit(&self, text: &str) -> String {
        let ack = self
            .provider
            .send(Command::SubmitInput {
                request_id: provider::new_request_id(),
                session_id: self.session.clone(),
                parts: vec![SubmissionPart::Text(text.into())],
                display_text: None,
                effort: None,
            })
            .expect("the prompt is taken");
        let Ack::TurnAccepted { turn_id, queued: false } = ack else { panic!("submit answered {ack:?}") };
        turn_id
    }

    /// Events until `done` holds over everything seen (or the wait ends).
    fn until(&mut self, done: impl Fn(&[ProviderEvent]) -> bool) {
        let deadline = Instant::now() + WAIT;
        while !done(&self.seen) && Instant::now() < deadline {
            if let Ok(event) = self.events.recv_timeout(Duration::from_millis(50)) {
                self.seen.push(event);
            }
        }
        assert!(done(&self.seen), "never happened; saw {:#?}", self.seen);
    }

    fn decide(&self, approval: &str, choice: &str) -> Result<Ack, provider::ProviderError> {
        self.provider.send(Command::DecideApproval {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            approval: approval.to_owned(),
            choice: choice.to_owned(),
            stage_token: None,
            feedback: None,
        })
    }

    /// The transcript the deltas seen so far build.
    fn transcript(&self) -> Session {
        let mut session = Session::new(&self.session, provider_acp::fold::MARK, "", "");
        for event in &self.seen {
            if let ProviderEvent::Deltas { deltas, .. } = event {
                for delta in deltas {
                    assert!(session.apply(delta.clone()), "every delta lands: {delta:?}");
                }
            }
        }
        session
    }
}

fn settled(turn: &str) -> impl Fn(&[ProviderEvent]) -> bool + '_ {
    move |seen| seen.iter().any(|event| matches!(event, ProviderEvent::TurnSettled { turn_id, .. } if turn_id == turn))
}

fn outcome(seen: &[ProviderEvent], turn: &str) -> Option<TurnOutcome> {
    seen.iter().find_map(|event| match event {
        ProviderEvent::TurnSettled { turn_id, outcome, .. } if turn_id == turn => Some(outcome.clone()),
        _ => None,
    })
}

fn tap(seen: &[ProviderEvent]) -> Option<(String, Vec<String>)> {
    seen.iter().find_map(|event| match event {
        ProviderEvent::ApprovalRequested { approval_id, options, .. } => {
            Some((approval_id.clone(), options.iter().map(|option| option.choice.clone()).collect()))
        }
        _ => None,
    })
}

fn approval_state(session: &Session) -> Option<ApprovalState> {
    session.turns.iter().flat_map(Turn::blocks).find_map(|block| match block {
        Block::Approval { state, .. } => Some(state.clone()),
        _ => None,
    })
}

#[test]
fn a_plain_answer_streams_into_one_turn_and_settles_completed() {
    let mut rig = open("answer", &[&fixture("answer.jsonl")]);
    let turn = rig.submit("What does the retry helper do?");
    rig.until(settled(&turn));
    assert_eq!(outcome(&rig.seen, &turn), Some(TurnOutcome::Completed));
    let session = rig.transcript();
    let Some(Turn::Assistant { blocks, .. }) = session.turns.last() else { panic!("an assistant turn: {session:?}") };
    assert!(matches!(&blocks[0], Block::Thinking { .. }), "{blocks:?}");
    assert!(matches!(&blocks[1], Block::Text { text, streaming: false } if text.contains("five") && text.ends_with("attempt fails.")));
    assert!(
        rig.seen.iter().any(|event| matches!(event, ProviderEvent::SkillsListed { skills, .. } if skills.iter().any(|skill| skill.name == "review"))),
        "the agent's commands reach the slash menu"
    );
    assert!(rig.seen.iter().any(|event| matches!(event, ProviderEvent::SessionFacts { side, .. } if side.context.is_some())));
    // The handshake filled the table: this agent offers no images.
    assert!(!rig.provider.capabilities().state(Capability::ImageInput).allows_attempt());
    assert_eq!(rig.provider.capabilities().state(Capability::Approvals), &CapabilityState::Native);
}

#[test]
fn a_permission_is_asked_once_decided_once_and_the_call_runs() {
    let mut rig = open("allow", &[&fixture("permission.jsonl")]);
    let turn = rig.submit("Run the test suite.");
    rig.until(|seen| tap(seen).is_some());
    let (approval, choices) = tap(&rig.seen).expect("asked");
    assert_eq!(choices, vec!["allow-once", "allow-always", "reject-once"], "Once / Always here / Deny");
    let pending = rig.provider.send(Command::ListPending { session_id: rig.session.clone() }).expect("pending lists");
    assert!(matches!(&pending, Ack::PendingWork { approvals, .. } if approvals.len() == 1 && approvals[0].id == approval));
    assert!(rig.decide(&approval, "not-an-option").is_err(), "a choice the agent never offered is refused");
    rig.decide(&approval, "allow-once").expect("decided");
    let again = rig.decide(&approval, "allow-once").expect_err("a second decision is refused");
    assert!(again.is_unknown_approval(), "{again:?}");
    rig.until(settled(&turn));
    let session = rig.transcript();
    assert!(matches!(approval_state(&session), Some(ApprovalState::AllowedOnce { exit_code: 0, .. })), "{session:?}");
    let shell = session.turns.iter().flat_map(Turn::blocks).find(|block| matches!(block, Block::ToolCall { kind: ToolKind::Shell, .. }));
    assert!(matches!(shell, Some(Block::ToolCall { status: ToolStatus::Success, target, .. }) if target == "cargo test --workspace"));
    let answered = std::fs::read_to_string(rig.dir.join("state/frames.log")).expect("frames");
    assert!(answered.contains(r#""optionId": "allow-once""#), "the agent got the person's option: {answered}");
}

#[test]
fn a_denied_permission_settles_its_card_denied() {
    let mut rig = open("deny", &[&fixture("permission-deny.jsonl")]);
    let turn = rig.submit("Run the test suite.");
    rig.until(|seen| tap(seen).is_some());
    let (approval, _) = tap(&rig.seen).expect("asked");
    rig.decide(&approval, "reject-once").expect("decided");
    rig.until(settled(&turn));
    let session = rig.transcript();
    assert_eq!(approval_state(&session), Some(ApprovalState::Denied));
    assert_eq!(outcome(&rig.seen, &turn), Some(TurnOutcome::Completed), "the turn goes on another way");
}

#[test]
fn a_plan_is_one_card_refined_in_place_beside_a_read_and_an_edit() {
    let mut rig = open("plan", &[&fixture("plan.jsonl")]);
    let turn = rig.submit("Cap the backoff.");
    rig.until(settled(&turn));
    let session = rig.transcript();
    let blocks: Vec<&Block> = session.turns.iter().flat_map(Turn::blocks).collect();
    let todos: Vec<&Block> = blocks.iter().copied().filter(|block| matches!(block, Block::Todo { .. })).collect();
    assert_eq!(todos.len(), 1, "one plan card, rewritten whole: {blocks:?}");
    assert!(matches!(todos[0], Block::Todo { items } if items.iter().all(|item| item.state == TodoState::Done)));
    assert!(blocks.iter().any(|block| matches!(block, Block::ToolCall { kind: ToolKind::Read, target, .. } if target.ends_with("src/net.rs"))));
    assert!(blocks.iter().any(
        |block| matches!(block, Block::ToolCall { kind: ToolKind::Edit, diff_stat: Some(stat), .. } if stat.added == 1 && stat.removed == 1)
    ));
}

#[test]
fn a_cancel_ends_the_turn_interrupted() {
    let mut rig = open("cancel", &[&fixture("cancel.jsonl")]);
    let turn = rig.submit("Watch the build.");
    rig.until(|seen| seen.iter().any(|event| matches!(event, ProviderEvent::TurnRunning { .. })));
    rig.provider
        .send(Command::InterruptTurn { request_id: provider::new_request_id(), session_id: rig.session.clone(), turn: None, retract: false })
        .expect("the cancel is taken");
    rig.until(settled(&turn));
    assert_eq!(outcome(&rig.seen, &turn), Some(TurnOutcome::Interrupted));
    let cancels = std::fs::read_to_string(rig.dir.join("state/frames.log")).expect("frames");
    assert!(cancels.contains("session/cancel"), "the agent was told: {cancels}");
}

#[test]
fn a_child_that_dies_settles_everything_and_the_session_revives() {
    let mut rig = open("crash", &[&fixture("crash.jsonl"), &fixture("answer.jsonl")]);
    rig.submit("Delete the build cache.");
    rig.until(|seen| seen.iter().any(|event| matches!(event, ProviderEvent::ConnectionLost { .. })));
    let lost = rig.seen.iter().position(|event| matches!(event, ProviderEvent::ConnectionLost { .. })).expect("lost");
    let (approval, _) = tap(&rig.seen[..lost]).expect("the request was waiting");
    let before = rig.transcript();
    assert!(matches!(approval_state(&before), Some(ApprovalState::Cancelled { .. })), "settled before the loss");
    assert!(
        rig.seen[..lost].iter().any(|event| matches!(event, ProviderEvent::Deltas { deltas, .. } if deltas.iter().any(|delta| matches!(delta, Delta::TurnFinished { .. })))),
        "the turn closed before the loss"
    );
    let pending = rig.provider.send(Command::ListPending { session_id: rig.session.clone() }).expect("pending lists");
    assert!(matches!(pending, Ack::PendingWork { approvals, .. } if approvals.is_empty()));
    assert!(rig.decide(&approval, "allow-once").expect_err("refused").is_unknown_approval());
    assert!(rig.provider.stderr_tail().iter().any(|line| line.contains("simulated")), "the child's last words");
    // The next send revives the child and re-opens the session.
    let turn = rig.submit("Again.");
    rig.until(settled(&turn));
    assert!(rig.seen.iter().any(|event| matches!(event, ProviderEvent::ConnectionRestored)));
    let frames = std::fs::read_to_string(rig.dir.join("state/frames.log")).expect("frames");
    assert!(frames.lines().any(|line| line.starts_with("1 ") && line.contains("session/load")), "re-opened with session/load");
}

#[test]
fn a_stored_session_reopens_with_its_turns_then_takes_a_prompt() {
    let state = std::env::temp_dir().join(format!("baaz-acp-load-state-{}", std::process::id()));
    std::fs::create_dir_all(&state).expect("state");
    std::fs::write(state.join("acp-workspaces.json"), r#"{"sess-stored":"/Users/ada/work/acme-web"}"#).expect("seed");
    let (provider, dir) = connected("load", &[&fixture("load.jsonl")], Some(&state));
    let events = provider.events();
    let ack = provider
        .send(Command::ResumeSession {
            request_id: provider::new_request_id(),
            session_id: "sess-stored".into(),
            cursor: None,
            metadata_only: false,
            mode: None,
        })
        .expect("the stored session re-opens");
    assert!(matches!(&ack, Ack::Session { session_id, .. } if session_id == "sess-stored"));
    let mut rig = Rig { provider, events, session: "sess-stored".into(), seen: Vec::new(), dir };
    while let Ok(event) = rig.events.try_recv() {
        rig.seen.push(event);
    }
    let shape: Vec<&'static str> =
        rig.transcript().turns.iter().map(|turn| if matches!(turn, Turn::User { .. }) { "user" } else { "agent" }).collect();
    assert_eq!(shape, vec!["user", "agent", "user", "agent"], "the history came back before the ack");
    let turn = rig.submit("Where is it called from?");
    rig.until(settled(&turn));
    assert_eq!(outcome(&rig.seen, &turn), Some(TurnOutcome::Completed));
    let _ = std::fs::remove_dir_all(state);
    // A session Baaz never recorded a folder for cannot be re-opened.
    let (provider, _dir) = connected("load-unknown", &[&fixture("load.jsonl")], None);
    let refused = provider
        .send(Command::ResumeSession { request_id: provider::new_request_id(), session_id: "elsewhere".into(), cursor: None, metadata_only: false, mode: None })
        .expect_err("no folder, no load");
    assert!(refused.is_session_scoped(), "{refused:?}");
}

#[test]
fn every_recording_folds_offline_into_closed_turns() {
    for name in ["answer.jsonl", "permission.jsonl", "permission-deny.jsonl", "plan.jsonl", "cancel.jsonl", "crash.jsonl", "load.jsonl"] {
        let text = std::fs::read_to_string(fixture(name)).expect("fixture reads");
        let replay = provider_acp::replay::fold_recording(&text).unwrap_or_else(|| panic!("{name} is an ACP recording"));
        let mut session = Session::new("", provider_acp::fold::MARK, "", "");
        let mut finished = 0;
        for event in &replay.events {
            if let ProviderEvent::Deltas { deltas, .. } = event {
                for delta in deltas {
                    finished += usize::from(matches!(delta, Delta::TurnFinished { .. }));
                    assert!(session.apply(delta.clone()), "{name}: every delta lands: {delta:?}");
                }
            }
        }
        let assistants = session.turns.iter().filter(|turn| matches!(turn, Turn::Assistant { .. })).count();
        assert!(assistants > 0, "{name}: an agent turn");
        assert_eq!(finished, assistants, "{name}: every turn closed");
        assert!(replay.session_id.is_some(), "{name}: names its session");
    }
    assert!(provider_acp::replay::fold_recording("<-- {\"method\":\"turn/started\"}").is_none(), "another provider's recording");
}
