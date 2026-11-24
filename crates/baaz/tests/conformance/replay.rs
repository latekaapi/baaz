//! Every fixture of every adapter, replayed through the adapter's real
//! child-facing code and checked against the seam invariants
//! ([`super::invariants`]).
//!
//! Each adapter's child is a stand-in that replays one recorded fixture on
//! its stdout — never a real CLI and never a model:
//!
//! * Claude Code and Codex: `tests/fixtures/conformance/fake_agent.py`
//!   (`fixtures/claude-code/*.jsonl`, `fixtures/codex/*.jsonl`);
//! * Muse: `tests/fixtures/conformance/msp_replay.py` through the real hub
//!   (`fixtures/msp/*.jsonl`);
//! * ACP: `crates/provider-acp/tests/fixtures/acp_agent.py`
//!   (`fixtures/acp/*.jsonl`, hand-written: docs/82 5.7).
//!
//! The driver replays the client half: it opens (or resumes) the session
//! the fixture recorded, submits one prompt per recorded turn, interrupts
//! where the recording did, decides every approval it is asked (with the
//! recorded decision, then once more to prove the second is refused) and
//! answers every question. Each command, its ack and every event land in
//! one [`Trace`] in the order the driver saw them.
//!
//! The suite is parameterised over [`ADAPTERS`]: an adapter joins with one
//! line there and a `cases` function that lists its fixtures (ACP did, in
//! docs/82 5.7).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aui_protocol::{Block, Delta, Turn};
use provider::{Ack, Command, ProviderError, ProviderEvent, QuestionAnswer, SubmissionPart};

use super::invariants::{self, Expect, Sent, Trace, Usage, Violation};
use super::{connect, repo, Agent};

/// How long one turn may run before the driver stops waiting for it.
const TURN_WAIT: Duration = Duration::from_secs(15);
/// How long a stream may stay quiet before the driver calls it done: a
/// stand-in replays in milliseconds, so this is only reached by a turn the
/// recording itself never ended.
const QUIET: Duration = Duration::from_millis(1500);
/// The tail drain after the script: anything still in flight.
const TAIL: Duration = Duration::from_millis(400);

/// One adapter in the suite: its stand-in and its fixtures.
pub struct Adapter {
    pub agent: Agent,
    pub cases: fn() -> Vec<Case>,
}

/// The adapters the suite runs. A new adapter joins here.
pub const ADAPTERS: [Adapter; 4] = [
    Adapter { agent: Agent::ClaudeCode, cases: claude_cases },
    Adapter { agent: Agent::Codex, cases: codex_cases },
    Adapter { agent: Agent::Muse, cases: muse_cases },
    Adapter { agent: Agent::Acp, cases: acp_cases },
];

/// How the session starts.
#[derive(Clone, Debug)]
pub enum Start {
    Open,
    /// Resume `session`; for Claude Code, from `stored` planted as its
    /// on-disk transcript.
    Resume { session: String, stored: Option<PathBuf> },
}

/// What the recorded client did after the session started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    Submit,
    /// Interrupt the turn the previous submit started, once it runs.
    Interrupt,
    /// Compact now (docs/85 10.4): the recorded `/compact` or
    /// `thread/compact/start`.
    Compact,
}

/// One fixture, as one adapter plays it.
#[derive(Clone, Debug)]
pub struct Case {
    pub name: String,
    /// The stand-in's plan (`fake_agent.py` spec, or `msp_replay.py` flags).
    pub plan: String,
    pub start: Start,
    pub steps: Vec<Step>,
    /// The recorded decision for an approval, as the adapter's choice token.
    pub choice: String,
    pub expect: Expect,
}

fn jsonl(dir: &str) -> Vec<PathBuf> {
    let root = repo().join("fixtures").join(dir);
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap_or_else(|error| panic!("{}: {error}", root.display()))
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    paths
}

fn lines(path: &Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

fn stem(path: &Path) -> String {
    path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default()
}

// ------------------------------------------------------------ Claude Code

/// Every `fixtures/claude-code` recording. A stream recording plays one
/// submit per recorded `result`; the stored transcript (`stored-history`)
/// is resumed from disk; a recording of a resumed child (`SessionStart:
/// resume`) is played after resuming that stored transcript.
fn claude_cases() -> Vec<Case> {
    jsonl("claude-code").iter().map(|path| claude_case(path)).collect()
}

/// One Claude Code recording, as the driver plays it.
pub fn claude_case(path: &Path) -> Case {
    const STORED: &str = "af18b5bb-1ff5-4ba2-825c-94804b78834e";
    let stored = repo().join("fixtures/claude-code/stored-history.jsonl");
    let path = path.to_path_buf();
    let frames = lines(&path);
    let name = stem(&path);
    let results: Vec<&serde_json::Value> =
        frames.iter().filter(|frame| frame.get("_dir").is_none() && frame["type"] == "result").collect();
    // A recording of a resumed child opens with the resume hook (a
    // later one is a child restarted mid-recording, still one session).
    let resumed_child = frames
        .iter()
        .find_map(|frame| frame["hook_name"].as_str().filter(|hook| hook.starts_with("SessionStart:")))
        == Some("SessionStart:resume");
    let stored_only = results.is_empty() && frames.iter().any(|frame| frame.get("parentUuid").is_some());
    let start = if stored_only || resumed_child {
        Start::Resume { session: STORED.to_owned(), stored: Some(if stored_only { path.clone() } else { stored.clone() }) }
    } else {
        Start::Open
    };
    // A recorded `/compact` (a manual `compact_boundary`) answers with a
    // result of its own: that result is the compaction's, not a turn's.
    let compactions = frames
        .iter()
        .filter(|frame| frame["subtype"] == "compact_boundary" && frame["compact_metadata"]["trigger"] == "manual")
        .count();
    let steps = if stored_only {
        Vec::new()
    } else {
        let mut steps = vec![Step::Submit; results.len().saturating_sub(compactions)];
        steps.extend(std::iter::repeat_n(Step::Compact, compactions));
        steps
    };
    let choice = frames
        .iter()
        .filter(|frame| frame["_dir"] == "host->cli")
        .find_map(|frame| frame["frame"]["response"]["response"]["behavior"].as_str().map(str::to_owned))
        .unwrap_or_else(|| "allow".to_owned());
    // The CLI's `total_cost_usd` runs per child: the session spent
    // each child's last reading. A recording spans a child restart
    // where the running total starts again below where it was.
    let mut cost = None;
    let mut child_total = 0.0;
    for result in &results {
        let Some(total) = result["total_cost_usd"].as_f64() else { continue };
        if total < child_total {
            cost = Some(cost.unwrap_or(0.0) + child_total);
        }
        child_total = total;
    }
    let cost = results.last().map(|_| cost.unwrap_or(0.0) + child_total);
    let tokens_out = results.iter().map(|result| result["usage"]["output_tokens"].as_u64().unwrap_or(0)).sum();
    let tokens_in = results
        .iter()
        .map(|result| {
            let usage = &result["usage"];
            ["input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens"]
                .iter()
                .map(|key| usage[*key].as_u64().unwrap_or(0))
                .sum::<u64>()
        })
        .sum();
    let wire_usage = (!results.is_empty()).then_some(Usage {
        tokens_in: Some(tokens_in),
        tokens_out: Some(tokens_out),
        cost_usd: cost,
    });
    Case {
        name: format!("claude-code/{name}"),
        plan: path.to_string_lossy().into_owned(),
        start,
        steps,
        choice,
        expect: Expect { closed: true, wire_usage },
    }
}

// ------------------------------------------------------------------ Codex

/// Every `fixtures/codex` recording, one case per recorded app-server
/// session (`initialize`): the driver opens (`thread/start`) or resumes
/// (`thread/resume`), submits per `turn/start` and interrupts per
/// `turn/interrupt`, exactly as the recorded client did.
fn codex_cases() -> Vec<Case> {
    jsonl("codex").iter().flat_map(|path| codex_cases_in(path)).collect()
}

/// One Codex recording's cases: one per recorded app-server session.
pub fn codex_cases_in(path: &Path) -> Vec<Case> {
    let mut cases = Vec::new();
    let frames = lines(path);
    let mut segments: Vec<Vec<&serde_json::Value>> = Vec::new();
    for frame in &frames {
        let starts = frame["_dir"] == "client->server" && frame["frame"]["method"] == "initialize";
        if starts || segments.is_empty() {
            segments.push(Vec::new());
        }
        segments.last_mut().expect("a segment").push(frame);
    }
    let several = segments.len() > 1;
    for (at, segment) in segments.iter().enumerate() {
        let mut start = Start::Open;
        let mut steps = Vec::new();
        let mut choice = "accept".to_owned();
        // The thread's total before its first live turn, and at the end.
        let mut before_live: Option<(u64, u64)> = None;
        let mut total_now: (u64, u64) = (0, 0);
        let mut last_total: Option<(u64, u64)> = None;
        let mut started = 0usize;
        let mut completed = 0usize;
        for frame in segment {
            let body = &frame["frame"];
            if frame["_dir"] == "client->server" {
                match body["method"].as_str() {
                    Some("thread/resume") => {
                        let thread = body["params"]["threadId"].as_str().unwrap_or_default().to_owned();
                        start = Start::Resume { session: thread, stored: None };
                    }
                    Some("turn/start") => steps.push(Step::Submit),
                    Some("turn/interrupt") => steps.push(Step::Interrupt),
                    Some("thread/compact/start") => steps.push(Step::Compact),
                    None => {
                        if let Some(decision) = body["result"]["decision"].as_str() {
                            choice = decision.to_owned();
                        }
                    }
                    _ => {}
                }
                continue;
            }
            match body["method"].as_str() {
                Some("turn/started") => {
                    before_live.get_or_insert(total_now);
                    started += 1;
                }
                Some("turn/completed") => completed += 1,
                Some("thread/tokenUsage/updated") => {
                    let total = &body["params"]["tokenUsage"]["total"];
                    total_now =
                        (total["inputTokens"].as_u64().unwrap_or(0), total["outputTokens"].as_u64().unwrap_or(0));
                    last_total = Some(total_now);
                }
                _ => {}
            }
        }
        // What the wire says the live turns spent: the thread's total at
        // the end, less what it held before the first of them started (a
        // resume re-sends the history's last reading first).
        let wire_usage = last_total.map(|(input, output)| {
            let (before_in, before_out) = before_live.unwrap_or_default();
            Usage { tokens_in: Some(input - before_in), tokens_out: Some(output - before_out), cost_usd: None }
        });
        let suffix = if several { format!("@{at}") } else { String::new() };
        cases.push(Case {
            name: format!("codex/{}{suffix}", stem(path)),
            plan: format!("{}@{at}", path.to_string_lossy()),
            start,
            steps,
            choice,
            expect: Expect { closed: started == completed, wire_usage },
        });
    }
    cases
}

// ------------------------------------------------------------------- Muse

/// Every `fixtures/msp` capture, replayed by `msp_replay.py` on the
/// session the hub opens: one submit per recorded turn of the capture's
/// first session.
fn muse_cases() -> Vec<Case> {
    jsonl("msp").iter().map(|path| muse_case(path)).collect()
}

/// One msp capture, as the driver plays it.
pub fn muse_case(path: &Path) -> Case {
    let path = path.to_path_buf();
    let text = std::fs::read_to_string(&path).expect("capture reads");
    // The server half as the client keeps it: a page's events less
    // those whose cursor already arrived (as `msp_replay.py` plays).
    let mut server: Vec<serde_json::Value> = Vec::new();
    let mut cursors = std::collections::HashSet::new();
    for line in text.lines() {
        let Some(body) = line.strip_prefix("<-- ") else { continue };
        let Ok(frame) = serde_json::from_str::<serde_json::Value>(body) else { continue };
        match frame["result"]["events"].as_array() {
            Some(events) if frame.get("method").is_none() => {
                for event in events {
                    let cursor = event["params"]["viewCursor"].as_str().map(str::to_owned);
                    if cursor.is_none() || cursors.insert(cursor) {
                        server.push(event.clone());
                    }
                }
            }
            _ if frame.get("method").is_some() => {
                cursors.insert(frame["params"]["viewCursor"].as_str().map(str::to_owned));
                server.push(frame);
            }
            _ => {}
        }
    }
    let session_of = |frame: &serde_json::Value| {
        frame["params"]["sessionId"]
            .as_str()
            .or_else(|| frame["params"]["session"]["sessionId"].as_str())
            .map(str::to_owned)
    };
    let primary = server.iter().find_map(session_of);
    let mine = |frame: &&serde_json::Value| session_of(frame) == primary;
    let mut started: Vec<String> = Vec::new();
    for turn in server
        .iter()
        .filter(mine)
        .filter(|frame| frame["method"] == "turn/started")
        .filter_map(|frame| frame["params"]["turnId"].as_str())
    {
        if !started.iter().any(|seen| seen == turn) {
            started.push(turn.to_owned());
        }
    }
    let completed: Vec<String> = server
        .iter()
        .filter(mine)
        .filter(|frame| frame["method"] == "turn/completed")
        .filter_map(|frame| frame["params"]["turnId"].as_str().map(str::to_owned))
        .collect();
    let closed = started.iter().all(|turn| completed.contains(turn));
    let usage: Vec<&serde_json::Value> =
        server.iter().filter(mine).filter(|frame| frame["method"] == "session/tokenUsage").collect();
    let wire_usage = (!usage.is_empty()).then(|| Usage {
        tokens_in: Some(usage.iter().map(|frame| frame["params"]["promptTokens"].as_u64().unwrap_or(0)).sum()),
        tokens_out: Some(
            usage.iter().map(|frame| frame["params"]["usage"]["outputTokens"].as_u64().unwrap_or(0)).sum(),
        ),
        cost_usd: None,
    });
    let choice = server
        .iter()
        .find(|frame| frame["method"] == "approval/request")
        .and_then(|frame| frame["params"]["availableChoices"][0]["choiceId"].as_str())
        .unwrap_or("c-yes")
        .to_owned();
    Case {
        name: format!("msp/{}", stem(&path)),
        plan: format!("--replay {}", path.to_string_lossy()),
        start: Start::Open,
        steps: vec![Step::Submit; started.len()],
        choice,
        expect: Expect { closed, wire_usage },
    }
}

// -------------------------------------------------------------------- ACP

/// Every `fixtures/acp` recording, played by `acp_agent.py`: the driver
/// opens (`session/new`) or re-opens (`session/load`) the session it
/// records, submits per `session/prompt`, interrupts per `session/cancel`
/// and decides the permission with the recorded option.
fn acp_cases() -> Vec<Case> {
    jsonl("acp").iter().map(|path| acp_case(path)).collect()
}

/// One ACP recording, as the driver plays it.
pub fn acp_case(path: &Path) -> Case {
    let frames = lines(path);
    let client = |frame: &&serde_json::Value| frame["_dir"] == "client->agent";
    let mut start = Start::Open;
    let mut steps = Vec::new();
    let mut choice = "allow-once".to_owned();
    let mut prompts = Vec::new();
    for frame in frames.iter().filter(client) {
        let body = &frame["frame"];
        match body["method"].as_str() {
            Some("session/load") => {
                let session = body["params"]["sessionId"].as_str().unwrap_or_default().to_owned();
                start = Start::Resume { session, stored: None };
            }
            Some("session/prompt") => {
                steps.push(Step::Submit);
                prompts.push(body["id"].clone());
            }
            Some("session/cancel") => steps.push(Step::Interrupt),
            None => {
                if let Some(option) = body["result"]["outcome"]["optionId"].as_str() {
                    choice = option.to_owned();
                }
            }
            _ => {}
        }
    }
    // Every prompt answered (with a stop reason or an error), or the agent
    // died: either way the wire ended every turn it opened.
    let answered = prompts.iter().all(|id| {
        frames.iter().any(|frame| frame["_dir"] == "agent->client" && frame["frame"]["id"] == *id && frame["frame"].get("method").is_none())
    });
    let died = frames.iter().any(|frame| frame.get("exit").is_some());
    Case {
        name: format!("acp/{}", stem(path)),
        plan: path.to_string_lossy().into_owned(),
        start,
        steps,
        choice,
        expect: Expect { closed: answered || died, wire_usage: None },
    }
}

// ---------------------------------------------------------------- driver

/// One adapter on its stand-in, recording everything into a trace.
struct Driver {
    agent: Agent,
    provider: provider::Provider,
    events: crossbeam_channel::Receiver<ProviderEvent>,
    hub: Option<provider_muse::MuseHub>,
    dir: PathBuf,
    session: String,
    trace: Trace,
    choice: String,
    /// accepted id → streamed id (`TurnIdentified`).
    alias: HashMap<String, String>,
    decided: Vec<String>,
    answered: Vec<String>,
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.provider.shutdown();
        if let Some(hub) = &self.hub {
            hub.shutdown();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Driver {
    /// Pull whatever has arrived, without waiting; react to taps.
    fn drain_now(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            self.take(event);
        }
    }

    fn take(&mut self, event: ProviderEvent) {
        if let ProviderEvent::TurnIdentified { accepted, turn_id, .. } = &event {
            self.alias.insert(accepted.clone(), turn_id.clone());
        }
        self.trace.event(event);
    }

    fn send(&mut self, sent: Sent, command: Command) -> Result<Ack, ProviderError> {
        self.drain_now();
        self.trace.sent(sent.clone());
        let result = self.provider.send(command);
        self.drain_now();
        self.trace.acked(sent, result.clone());
        result
    }

    /// Wait for the next event up to `wait`.
    fn next(&mut self, wait: Duration) -> bool {
        match self.events.recv_timeout(wait) {
            Ok(event) => {
                self.take(event);
                true
            }
            Err(_) => false,
        }
    }

    /// Answer what the stream asked and has not been answered yet.
    fn react(&mut self) {
        let asks: Vec<(bool, String)> = self
            .trace
            .events()
            .filter_map(|event| match event {
                ProviderEvent::ApprovalRequested { approval_id, .. } => Some((true, approval_id.clone())),
                ProviderEvent::QuestionRaised { question_id, .. } => Some((false, question_id.clone())),
                _ => None,
            })
            .collect();
        for (approval, id) in asks {
            if approval && !self.decided.contains(&id) {
                self.decided.push(id.clone());
                self.decide(&id);
            } else if !approval && !self.answered.contains(&id) {
                self.answered.push(id.clone());
                self.answer(&id);
            }
        }
    }

    fn decide(&mut self, approval: &str) {
        let session = self.session.clone();
        let token = match self.send(Sent::ListPending, Command::ListPending { session_id: session.clone() }) {
            Ok(Ack::PendingWork { approvals, .. }) => {
                approvals.into_iter().find(|item| item.id == approval).and_then(|item| item.stage_token)
            }
            _ => None,
        };
        let command = || Command::DecideApproval {
            request_id: provider::new_request_id(),
            session_id: session.clone(),
            approval: approval.to_owned(),
            choice: self.choice.clone(),
            stage_token: token.clone(),
            feedback: None,
        };
        let (first, second) = (command(), command());
        let _ = self.send(Sent::Decide(approval.to_owned()), first);
        // Answered once: a second decision for the same id must be refused.
        let _ = self.send(Sent::Decide(approval.to_owned()), second);
    }

    fn answer(&mut self, question: &str) {
        let card = self.trace.events().find_map(|event| match event {
            ProviderEvent::Deltas { deltas, .. } => deltas.iter().find_map(|delta| question_block(delta, question)),
            _ => None,
        });
        let answers = match card {
            Some((id, Some(label))) => vec![QuestionAnswer {
                question_id: id,
                selected_label: Some(label),
                selected_labels: None,
                free_text: None,
                note: None,
            }],
            Some((id, None)) => vec![QuestionAnswer {
                question_id: id,
                selected_label: None,
                selected_labels: None,
                free_text: Some("yes".into()),
                note: None,
            }],
            None => Vec::new(),
        };
        let command = Command::AnswerQuestion {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            question: question.to_owned(),
            answers,
        };
        let _ = self.send(Sent::Answer(question.to_owned()), command);
    }

    fn ended(&self, turn: &str) -> bool {
        let streamed = self.alias.get(turn).map(String::as_str).unwrap_or(turn);
        self.trace.events().any(|event| match event {
            ProviderEvent::TurnSettled { turn_id, .. } => turn_id == turn || turn_id == streamed,
            ProviderEvent::Deltas { deltas, .. } => deltas.iter().any(
                |delta| matches!(delta, Delta::TurnFinished { turn_id, .. } if turn_id == turn || turn_id == streamed),
            ),
            ProviderEvent::ConnectionLost { .. } => true,
            _ => false,
        })
    }

    fn running(&self, turn: &str) -> bool {
        let streamed = self.alias.get(turn).map(String::as_str).unwrap_or(turn);
        self.trace.events().any(|event| match event {
            ProviderEvent::TurnRunning { turn_id, .. } => turn_id == turn || turn_id == streamed,
            ProviderEvent::Deltas { deltas, .. } => deltas.iter().any(|delta| {
                matches!(delta, Delta::TurnStarted { turn: Turn::Assistant { id, .. } } if id == turn || id == streamed)
            }),
            _ => false,
        })
    }

    /// Run `turn` to its end (or until the stream goes quiet), answering
    /// what it asks; interrupt it once it runs when `interrupt` says so.
    fn follow(&mut self, turn: &str, interrupt: bool) {
        let deadline = Instant::now() + TURN_WAIT;
        let mut interrupted = !interrupt;
        let mut quiet_since = Instant::now();
        while Instant::now() < deadline {
            self.react();
            if !interrupted && self.running(turn) {
                interrupted = true;
                let command = Command::InterruptTurn {
                    request_id: provider::new_request_id(),
                    session_id: self.session.clone(),
                    turn: Some(turn.to_owned()),
                    retract: false,
                };
                let _ = self.send(Sent::Interrupt, command);
            }
            if interrupted && self.ended(turn) {
                return;
            }
            if self.next(Duration::from_millis(50)) {
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() > QUIET {
                return;
            }
        }
    }

    fn submit(&mut self, interrupt: bool) {
        let command = Command::SubmitInput {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            parts: vec![SubmissionPart::Text("replay the next recorded turn".into())],
            display_text: None,
            effort: None,
        };
        match self.send(Sent::Submit, command) {
            Ok(Ack::TurnAccepted { turn_id, .. }) => self.follow(&turn_id, interrupt),
            other => panic!("{:?}: the submit was refused: {other:?}", self.agent),
        }
    }

    /// Compact now, and let the compaction's own turn play out.
    fn compact(&mut self) {
        let command = Command::CompactSession {
            request_id: provider::new_request_id(),
            session_id: self.session.clone(),
            through_turn: None,
        };
        match self.send(Sent::Compact, command) {
            Ok(Ack::Accepted) => self.settle(),
            other => panic!("{:?}: the compaction was refused: {other:?}", self.agent),
        }
    }

    /// The tail: whatever is still in flight, answered.
    fn settle(&mut self) {
        let deadline = Instant::now() + TURN_WAIT;
        while Instant::now() < deadline {
            self.react();
            if !self.next(TAIL) {
                return;
            }
        }
    }
}

/// The question card `delta` carries for `question`: its id and the first
/// option's label, if it has options.
fn question_block(delta: &Delta, question: &str) -> Option<(String, Option<String>)> {
    let block = match delta {
        Delta::BlockAdded { block, .. } | Delta::BlockUpdated { block, .. } => block,
        _ => return None,
    };
    match block {
        // Muse names a card `<request>:<question>`; the answer names the
        // question.
        Block::Question { id, options, .. } if id == question || id.starts_with(&format!("{question}:")) => {
            let inner = id.strip_prefix(&format!("{question}:")).unwrap_or(id);
            Some((inner.to_owned(), options.first().map(|option| option.label.clone())))
        }
        _ => None,
    }
}

/// Play `case` on `agent`'s stand-in: the whole trace, and what the
/// invariants say about it.
pub fn run(agent: Agent, case: &Case) -> (Trace, Vec<Violation>) {
    let tag = case.name.replace('/', "-");
    let (provider, dir, hub) = connect(agent, &tag, &[&case.plan]);
    let events = provider.events();
    let mut driver = Driver {
        agent,
        provider,
        events,
        hub,
        dir,
        session: String::new(),
        trace: Trace::default(),
        choice: case.choice.clone(),
        alias: HashMap::new(),
        decided: Vec::new(),
        answered: Vec::new(),
    };
    let workspace = driver.dir.join("work");
    std::fs::create_dir_all(&workspace).expect("workspace");
    let opened = match &case.start {
        Start::Open => {
            let command = Command::OpenSession {
                request_id: provider::new_request_id(),
                workspace: Some(workspace.to_string_lossy().into_owned()),
                model: None,
                model_provider: None,
                mode: None,
                env: Vec::new(),
            };
            driver.send(Sent::Open, command)
        }
        Start::Resume { session, stored } => {
            if let Some(stored) = stored {
                let slug = driver.dir.join("claude-home/projects/-conformance");
                std::fs::create_dir_all(&slug).expect("slug dir");
                std::fs::copy(stored, slug.join(format!("{session}.jsonl"))).expect("stored transcript");
            }
            let command = Command::ResumeSession {
                request_id: provider::new_request_id(),
                session_id: session.clone(),
                cursor: None,
                metadata_only: false,
                mode: None,
                workspace: super::recorded_workspace(&driver.dir),
                env: Vec::new(),
            };
            driver.send(Sent::Resume(session.clone()), command)
        }
    };
    match opened {
        Ok(Ack::Session { session_id, .. }) => driver.session = session_id,
        other => panic!("{}: the session would not start: {other:?}", case.name),
    }
    let mut steps = case.steps.iter().peekable();
    while let Some(step) = steps.next() {
        match step {
            Step::Submit => {
                let interrupt = steps.peek() == Some(&&Step::Interrupt);
                driver.submit(interrupt);
            }
            Step::Compact => driver.compact(),
            Step::Interrupt => {}
        }
    }
    driver.settle();
    let trace = std::mem::take(&mut driver.trace);
    let violations = invariants::check(&trace, &case.expect);
    if std::env::var("CONFORMANCE_DUMP").is_ok_and(|wanted| case.name.contains(&wanted)) {
        eprintln!("--- {}\n{}", case.name, invariants::describe(&trace));
    }
    (trace, violations)
}

/// One suite, every adapter, every fixture, no network: each fixture
/// replayed through its adapter keeps every seam invariant.
/// What a set of traces exercised, so a green run can say it checked
/// something: turns, taps, decisions, refusals, questions, interrupts.
#[derive(Default, Debug)]
struct Exercised {
    turns: usize,
    finished: usize,
    taps: usize,
    decisions: usize,
    second_refused: usize,
    questions: usize,
    interrupts: usize,
}

impl Exercised {
    fn add(&mut self, trace: &Trace) {
        let mut decided = std::collections::HashSet::new();
        for entry in &trace.entries {
            match entry {
                invariants::Entry::Acked(Sent::Submit, Ok(_)) => self.turns += 1,
                invariants::Entry::Acked(Sent::Interrupt, _) => self.interrupts += 1,
                invariants::Entry::Acked(Sent::Answer(_), _) => self.questions += 1,
                invariants::Entry::Acked(Sent::Decide(id), result) => {
                    if decided.insert(id.clone()) {
                        self.decisions += 1;
                    } else if result.as_ref().is_err_and(invariants::refused_as_gone) {
                        self.second_refused += 1;
                    }
                }
                invariants::Entry::Event(ProviderEvent::ApprovalRequested { .. }) => self.taps += 1,
                invariants::Entry::Event(ProviderEvent::Deltas { deltas, .. }) => {
                    self.finished += deltas.iter().filter(|delta| matches!(delta, Delta::TurnFinished { .. })).count();
                }
                _ => {}
            }
        }
    }
}

#[test]
fn every_fixture_of_every_adapter_keeps_the_seam_invariants() {
    let runs: Vec<(Agent, usize, Vec<String>, Exercised)> = std::thread::scope(|scope| {
        let handles: Vec<_> = ADAPTERS
            .iter()
            .map(|adapter| {
                scope.spawn(move || {
                    let cases = (adapter.cases)();
                    let mut failures = Vec::new();
                    let mut exercised = Exercised::default();
                    for case in &cases {
                        let (trace, violations) = run(adapter.agent, case);
                        exercised.add(&trace);
                        if !violations.is_empty() {
                            let listed: Vec<String> = violations.iter().map(ToString::to_string).collect();
                            failures.push(format!(
                                "{} ({} entries):\n    {}",
                                case.name,
                                trace.entries.len(),
                                listed.join("\n    ")
                            ));
                        }
                        let events = trace.events().count();
                        assert!(events > 0, "{}: the replay produced no events at all", case.name);
                    }
                    (adapter.agent, cases.len(), failures, exercised)
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("an adapter's replay panicked")).collect()
    });
    let mut failures = Vec::new();
    for (agent, count, failed, exercised) in &runs {
        println!("conformance: {agent:?}: {count} fixtures x {} invariants; {exercised:?}", invariants::INVARIANTS.len());
        assert!(*count > 0, "{agent:?}: no fixtures found");
        assert!(exercised.turns > 0 && exercised.finished > 0, "{agent:?}: no turn ran: {exercised:?}");
        failures.extend(failed.iter().cloned());
    }
    assert!(failures.is_empty(), "seam invariants broken:\n{}", failures.join("\n"));
}

/// PM-10 (docs/81 B): what a submission must be folded with is recorded
/// before the command is written, so an echo that beats the ack still
/// shows it. Codex's stand-in answers `turn/start` and streams the turn's
/// echo in the same breath; the bubble must read the display text every
/// time. A guard, not a reproduction: the stand-in's timing rarely let the
/// echo win before the record moved ahead of the write, so this held then
/// too — it pins the contract end to end, through the real adapter.
#[test]
fn a_display_text_rides_an_echo_that_beats_the_ack() {
    let path = repo().join("fixtures/codex/basic.jsonl");
    let prompt = lines(&path)
        .iter()
        .find(|frame| frame["_dir"] == "client->server" && frame["frame"]["method"] == "turn/start")
        .and_then(|frame| frame["frame"]["params"]["input"][0]["text"].as_str().map(str::to_owned))
        .expect("the recorded prompt");
    let case = codex_cases_in(&path).remove(0);
    for round in 0..5 {
        let (provider, dir, hub) = connect(Agent::Codex, &format!("display-{round}"), &[&case.plan]);
        let events = provider.events();
        let mut driver = Driver {
            agent: Agent::Codex,
            provider,
            events,
            hub,
            dir,
            session: String::new(),
            trace: Trace::default(),
            choice: case.choice.clone(),
            alias: HashMap::new(),
            decided: Vec::new(),
            answered: Vec::new(),
        };
        let open = Command::OpenSession {
            request_id: provider::new_request_id(),
            workspace: None,
            model: None,
            model_provider: None,
            mode: None,
            env: Vec::new(),
        };
        let Ok(Ack::Session { session_id, .. }) = driver.send(Sent::Open, open) else { panic!("the session opens") };
        driver.session = session_id;
        let submit = Command::SubmitInput {
            request_id: provider::new_request_id(),
            session_id: driver.session.clone(),
            parts: vec![SubmissionPart::Text(prompt.clone())],
            display_text: Some("ready?".into()),
            effort: None,
        };
        let Ok(Ack::TurnAccepted { turn_id, .. }) = driver.send(Sent::Submit, submit) else { panic!("the turn starts") };
        driver.follow(&turn_id, false);
        let shown: Vec<String> = driver
            .trace
            .events()
            .filter_map(|event| match event {
                ProviderEvent::Deltas { deltas, .. } => Some(deltas.iter().filter_map(|delta| match delta {
                    Delta::TurnStarted { turn: Turn::User { text, .. } } => Some(text.clone()),
                    _ => None,
                })),
                _ => None,
            })
            .flatten()
            .collect();
        assert_eq!(shown, vec!["ready?".to_owned()], "round {round}: the bubble shows the display text\n{}", invariants::describe(&driver.trace));
    }
}
