//! `provider-acp` — any Agent Client Protocol agent behind the provider
//! seam (docs/82 5.7; docs/81 B §c.1).
//!
//! ACP is JSON-RPC over the agent's stdio: the client shakes hands
//! (`initialize`), opens or re-opens a session (`session/new`,
//! `session/load`), sends one prompt per turn (`session/prompt`, answered
//! when the turn ends) and may cancel it (`session/cancel`); the agent
//! reports everything as `session/update` notifications and asks before a
//! tool runs (`session/request_permission`). One adapter covers every agent
//! with an ACP mode — Gemini CLI, OpenCode and the rest — instead of a crate
//! each.
//!
//! The adapter stands on the shared runtime ([`provider::runtime`]: spawn in
//! a new session, group kill, the pid ledger, the line reader, the stdin
//! writer, the request map, the guarded pump) and the shared transcript
//! builder ([`transcript_core`]); what is ACP's own is the frames
//! ([`wire`]), the fold from updates to blocks ([`fold`]), the capability
//! table its handshake fills ([`caps`]), and its registration
//! ([`registry`]).
//!
//! The agent is whatever the person configured: the command line in
//! `BAAZ_ACP_AGENT` ([`registry::COMMAND_ENV`]). Nothing is installed or run
//! until it is set, and until then the provider is not offered at all.
//!
//! Every wire spelling lives in this crate; nothing outside it names an
//! ACP method or field.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod caps;
pub mod child;
pub mod fold;
pub mod registry;
pub mod replay;
pub mod wire;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, McpServer, PendingApproval, ProviderAdapter, ProviderError,
    ProviderEvent, ProviderId, Refusal, RefusalHint, Severity, SubmissionPart,
};
use serde_json::Value;

use child::{lock, RunningChild, RpcError, Shared};
use fold::{AcpFold, Ending};
use wire::AgentHandshake;

/// How long an agent may take to answer `initialize`: an agent started
/// through a package runner fetches itself first.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);

/// How long `session/new` and `session/load` may take (a load streams the
/// whole stored session first).
const SESSION_TIMEOUT: Duration = Duration::from_secs(120);

/// Where the session runs: its id as the app knows it, and its folder.
#[derive(Clone, Debug)]
struct Place {
    session: String,
    cwd: String,
}

/// One ACP agent: neutral commands in, one agent child per session,
/// neutral acks and events out.
pub struct AcpAdapter {
    program: String,
    args: Vec<String>,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    shared: Arc<Mutex<Shared>>,
    child: Mutex<Option<RunningChild>>,
    handshake: Mutex<Option<AgentHandshake>>,
    client: Mutex<Option<ConnectInfo>>,
    place: Mutex<Option<Place>>,
    relay: Mutex<Option<McpServer>>,
    stderr: provider::StderrTail,
    /// The session's `OpenSession.env`, which every agent child after the
    /// open starts with (the child started at connect is restarted with it).
    env: Mutex<Vec<(String, String)>>,
}

impl AcpAdapter {
    /// The agent `program args…`. Nothing runs until
    /// [`ProviderAdapter::connect`].
    pub fn new(program: &str, args: &[String]) -> Self {
        let (tx, rx) = unbounded();
        Self {
            program: program.to_owned(),
            args: args.to_vec(),
            tx,
            rx,
            shared: Arc::new(Mutex::new(Shared::new())),
            child: Mutex::new(None),
            handshake: Mutex::new(None),
            client: Mutex::new(None),
            place: Mutex::new(None),
            relay: Mutex::new(None),
            stderr: provider::StderrTail::new(),
            env: Mutex::new(Vec::new()),
        }
    }

    /// Give every session the terminal relay's bridge as a stdio MCP
    /// server.
    pub fn set_terminal_relay(&self, relay: McpServer) {
        *self.relay.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(relay);
    }

    /// The child's pid, while one runs (tests).
    pub fn pid(&self) -> Option<u32> {
        self.child.lock().ok()?.as_ref().map(RunningChild::pid)
    }

    fn handshake(&self) -> Option<AgentHandshake> {
        self.handshake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    fn place(&self) -> Option<Place> {
        self.place.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// The MCP servers a session gets: the terminal relay's bridge, its
    /// `--session` naming `bridge_session`.
    fn servers(&self, bridge_session: &str) -> Vec<wire::StdioServer> {
        let relay = self.relay.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        relay
            .map(|relay| wire::StdioServer {
                name: "baaz".into(),
                command: relay.command.to_string_lossy().into_owned(),
                args: vec![
                    "--terminal".into(),
                    "--socket".into(),
                    relay.socket.to_string_lossy().into_owned(),
                    "--session".into(),
                    bridge_session.to_owned(),
                ],
            })
            .into_iter()
            .collect()
    }

    /// The child's last stderr line, for a failure's reason.
    fn last_words(&self) -> String {
        self.stderr.lines().last().map(|line| format!(" ({})", line.trim())).unwrap_or_default()
    }

    /// Spawn the agent and shake hands: the handshake is remembered for the
    /// capability table ([`caps::remember`]).
    fn start(&self) -> Result<AgentHandshake, ProviderError> {
        let client = self.client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let client = client.unwrap_or_else(|| ConnectInfo::new("baaz", "0"));
        let env = self.env.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        let running = RunningChild::spawn(&self.program, &self.args, &env, Arc::clone(&self.shared), self.tx.clone(), self.stderr.clone())
            .map_err(|error| ProviderError::Unavailable { reason: format!("could not start the agent `{}`: {error}", self.program) })?;
        let answer = running.request(
            wire::INITIALIZE,
            wire::initialize_params(&client.client_name, &client.client_version),
            INITIALIZE_TIMEOUT,
        );
        let handshake = answer
            .map_err(|error| error.message)
            .and_then(|result| AgentHandshake::decode(&result))
            .map_err(|reason| {
                running.shutdown();
                let words = self.stderr.settled(Duration::from_millis(300)).last().map(|line| format!(" ({})", line.trim())).unwrap_or_default();
                ProviderError::Unavailable { reason: format!("the agent did not shake hands: {reason}{words}") }
            })?;
        if handshake.protocol_version != wire::PROTOCOL_VERSION {
            running.shutdown();
            return Err(ProviderError::launch_refusal(
                RefusalHint::None,
                "The agent speaks another protocol version",
                format!("the agent answered ACP version {}; Baaz speaks version {}", handshake.protocol_version, wire::PROTOCOL_VERSION),
            ));
        }
        caps::remember(&handshake);
        *self.handshake.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handshake.clone());
        let old = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).replace(running);
        // A dead child's pump already ended; hanging it up only reaps it.
        drop(old);
        Ok(handshake)
    }

    /// Whether the child is gone (it died, or was never started).
    fn child_lost(&self) -> bool {
        self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().is_none_or(RunningChild::is_lost)
    }

    /// Run `f` on the live child.
    fn with_child<T>(&self, f: impl FnOnce(&RunningChild) -> T) -> Result<T, ProviderError> {
        let child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match child.as_ref() {
            Some(running) if !running.is_lost() => Ok(f(running)),
            _ => Err(ProviderError::Unavailable { reason: "the agent is not running".into() }),
        }
    }

    /// A child that died is started again before the next command, and the
    /// session it served re-opened: with `session/load` when the agent can
    /// (its replay is not folded twice), else as a fresh agent session the
    /// app keeps its id for, with a notice that the earlier turns stay
    /// behind. `ConnectionRestored` follows.
    fn revive(&self) -> Result<(), ProviderError> {
        if !self.child_lost() {
            return Ok(());
        }
        let handshake = self.start()?;
        if let Some(place) = self.place() {
            let wire_id = lock(&self.shared).wire.clone().unwrap_or_else(|| place.session.clone());
            if handshake.load_session {
                lock(&self.shared).muted = true;
                let params = wire::session_load_params(&wire_id, &place.cwd, &self.servers(&place.session));
                let loaded = self.with_child(|running| running.request(wire::SESSION_LOAD, params, SESSION_TIMEOUT))?;
                lock(&self.shared).muted = false;
                loaded.map_err(|error| ProviderError::Unavailable { reason: format!("the agent could not re-open the session: {error}") })?;
            } else {
                let params = wire::session_new_params(&place.cwd, &self.servers(&place.session));
                lock(&self.shared).opening = Some(child::Opening::Rewire);
                let opened = self.with_child(|running| running.request(wire::SESSION_NEW, params, SESSION_TIMEOUT));
                lock(&self.shared).opening = None;
                let fresh = opened?
                    .ok()
                    .and_then(|result| result.get("sessionId").and_then(Value::as_str).map(str::to_owned))
                    .ok_or_else(|| ProviderError::Unavailable { reason: "the restarted agent opened no session".into() })?;
                lock(&self.shared).wire = Some(fresh);
                let _ = self.tx.send(ProviderEvent::Notice {
                    session_id: place.session.clone(),
                    text: "The agent restarted without the earlier turns: it cannot re-open a stored conversation.".into(),
                });
            }
        }
        let _ = self.tx.send(ProviderEvent::ConnectionRestored);
        Ok(())
    }

    fn check_session(&self, session_id: &str) -> Result<Place, ProviderError> {
        match self.place() {
            Some(place) if place.session == session_id => Ok(place),
            _ => Err(ProviderError::Rejected { reason: format!("session {session_id} is not open on this agent") }),
        }
    }

    fn open_session(&self, request_id: &str, workspace: Option<&str>, env: Vec<(String, String)>) -> Result<Ack, ProviderError> {
        // The child started at connect has no session yet: one that must
        // carry the session's variables is started again with them.
        let mut wanted = self.env.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if *wanted != env {
            *wanted = env;
            drop(wanted);
            if let Some(old) = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
                old.shutdown();
            }
        } else {
            drop(wanted);
        }
        self.revive()?;
        let cwd = workspace
            .filter(|workspace| !workspace.is_empty())
            .map(str::to_owned)
            .or_else(|| std::env::var("HOME").ok())
            .unwrap_or_else(|| "/".into());
        let params = wire::session_new_params(&cwd, &self.servers(request_id));
        // The pump adopts the session as its answer passes
        // ([`child::Opening`]): the agent may speak for it first.
        lock(&self.shared).opening = Some(child::Opening::Fresh(cwd.clone()));
        let opened = self.with_child(|running| running.request(wire::SESSION_NEW, params, SESSION_TIMEOUT));
        lock(&self.shared).opening = None;
        let result = opened?.map_err(|error| self.open_failure(&error))?;
        let session = result
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ProviderError::Unavailable { reason: "the agent opened a session with no id".into() })?
            .to_owned();
        {
            let mut shared = lock(&self.shared);
            if shared.wire.as_deref() != Some(session.as_str()) {
                shared.fold = AcpFold::new(&session, &cwd);
                shared.wire = Some(session.clone());
                shared.prompts.clear();
                shared.parked.clear();
            }
        }
        *self.place.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Place { session: session.clone(), cwd: cwd.clone() });
        Ok(Ack::Session { session_id: session, title: None })
    }

    /// How a refused `session/new` reads: a sign-in the agent wants first,
    /// or its own words.
    fn open_failure(&self, error: &RpcError) -> ProviderError {
        if error.code == Some(wire::AUTH_REQUIRED) {
            let methods = self.handshake().map(|handshake| handshake.auth_methods.join(", ")).unwrap_or_default();
            let how = if methods.is_empty() { String::new() } else { format!(" ({methods})") };
            return ProviderError::launch_refusal(
                RefusalHint::SignedOut,
                "Sign in to the agent first",
                format!("the agent wants a sign-in before it opens a session{how}: {}", error.message),
            );
        }
        ProviderError::Unavailable { reason: format!("the agent would not open a session: {error}{}", self.last_words()) }
    }

    /// `session/load` in the folder the app recorded for the session (the
    /// resume's `workspace`): the agent re-opens a session only there, and
    /// this adapter keeps no record of its own.
    fn resume_session(&self, session_id: &str, workspace: Option<&str>) -> Result<Ack, ProviderError> {
        if !self.handshake().is_some_and(|handshake| handshake.load_session) {
            return Err(ProviderError::unsupported("resume-session", "this agent cannot re-open a stored session"));
        }
        let Some(cwd) = workspace.filter(|workspace| !workspace.is_empty()).map(str::to_owned) else {
            return Err(ProviderError::Refused(Refusal {
                hint: RefusalHint::HistoryMissing,
                title: "This conversation cannot be re-opened".into(),
                reason: format!("Baaz has no record of the folder session {session_id} ran in, which the agent needs to re-open it"),
                severity: Severity::Banner,
                session_scoped: true,
            }));
        };
        self.revive()?;
        {
            let mut shared = lock(&self.shared);
            shared.fold = AcpFold::new(session_id, &cwd);
            shared.fold.begin_history();
            shared.wire = Some(session_id.to_owned());
            shared.prompts.clear();
            shared.parked.clear();
        }
        *self.place.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Place { session: session_id.to_owned(), cwd: cwd.clone() });
        let params = wire::session_load_params(session_id, &cwd, &self.servers(session_id));
        let loaded = self.with_child(|running| running.request(wire::SESSION_LOAD, params, SESSION_TIMEOUT))?;
        let mut shared = lock(&self.shared);
        shared.fold.end_history();
        child::emit_deltas(&mut shared, &self.tx);
        drop(shared);
        loaded.map_err(|error| ProviderError::Unavailable { reason: format!("the agent could not re-open the session: {error}") })?;
        Ok(Ack::Session { session_id: session_id.to_owned(), title: None })
    }

    /// A submission's prompt: its content blocks, and the words its bubble
    /// shows.
    fn prompt_of(&self, parts: &[SubmissionPart]) -> Result<(Vec<Value>, String), ProviderError> {
        let images = self.handshake().is_some_and(|handshake| handshake.image);
        let mut blocks = Vec::new();
        let mut words = Vec::new();
        for part in parts {
            match part {
                SubmissionPart::Text(text) => {
                    blocks.push(wire::text_block(text));
                    words.push(text.as_str());
                }
                SubmissionPart::Image { .. } if !images => {
                    return Err(ProviderError::unsupported("submit-input", "the agent's handshake offers no image prompts"));
                }
                SubmissionPart::Image { base64_data, media_type, .. } => blocks.push(wire::image_block(base64_data, media_type)),
            }
        }
        if blocks.is_empty() {
            return Err(ProviderError::Rejected { reason: "a submission needs at least one part".into() });
        }
        Ok((blocks, words.join("\n")))
    }

    fn submit(&self, request_id: &str, session_id: &str, parts: &[SubmissionPart], display: Option<&str>) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        self.revive()?;
        let (blocks, words) = self.prompt_of(parts)?;
        let turn = request_id.to_owned();
        let wire_id = {
            let mut shared = lock(&self.shared);
            if let Some(running) = shared.fold.running_turn() {
                return Err(ProviderError::Rejected {
                    reason: format!("turn {running} is still running: an ACP agent takes one prompt at a time"),
                });
            }
            let wire_id = shared.wire.clone().unwrap_or_else(|| session_id.to_owned());
            // Folded before the prompt is written (PM-10): the answer can
            // beat this call.
            shared.fold.submitted(&turn, display.unwrap_or(&words), Some(now_ms()));
            child::emit_deltas(&mut shared, &self.tx);
            let _ = self.tx.send(ProviderEvent::TurnRunning { session_id: session_id.to_owned(), turn_id: turn.clone() });
            wire_id
        };
        let sent = self.with_child(|running| running.prompt(&self.shared, &turn, wire::prompt_params(&wire_id, blocks)));
        if let Err(error) = sent.and_then(|written| written.map_err(|error| ProviderError::Unavailable { reason: error.to_string() })) {
            let mut shared = lock(&self.shared);
            let ending = Ending::Failed { code: 0, message: format!("the prompt never reached the agent: {error}") };
            child::end_turn(&mut shared, &ending, &self.tx, &|_| {});
            return Err(error);
        }
        Ok(Ack::TurnAccepted { turn_id: turn, queued: false })
    }

    fn interrupt(&self, session_id: &str) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let (wire_id, parked) = {
            let mut shared = lock(&self.shared);
            if shared.fold.running_turn().is_none() {
                return Err(ProviderError::Rejected { reason: "no turn is running".into() });
            }
            // The cancel moots every request still waiting (ACP: the client
            // answers each `cancelled`); their cards say so.
            let parked: Vec<child::Parked> = std::mem::take(&mut shared.parked);
            for ask in &parked {
                shared.fold.abandoned(&ask.approval, "The turn was stopped before it was answered.");
            }
            child::emit_deltas(&mut shared, &self.tx);
            (shared.wire.clone().unwrap_or_else(|| session_id.to_owned()), parked)
        };
        self.with_child(|running| {
            let _ = running.send(&wire::cancel_notification(&wire_id));
            for ask in &parked {
                let _ = running.send(&wire::permission_cancelled(&ask.rpc));
            }
        })?;
        Ok(Ack::Accepted)
    }

    fn decide(&self, session_id: &str, approval: &str, choice: &str) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let rpc = {
            let mut shared = lock(&self.shared);
            let Some(at) = shared.parked.iter().position(|parked| parked.approval == approval) else {
                return Err(ProviderError::unknown_approval(approval, "it may have resolved"));
            };
            let Some(kind) = shared.fold.option_kind(approval, choice) else {
                return Err(ProviderError::Refused(Refusal {
                    hint: RefusalHint::InvalidChoice,
                    title: "Not one of the agent's options".into(),
                    reason: format!("{choice:?} is not an option the agent offered for {approval:?}"),
                    severity: Severity::Banner,
                    session_scoped: true,
                }));
            };
            let parked = shared.parked.remove(at);
            // Recorded before the answer goes out: the call it lets run can
            // finish before this returns.
            shared.fold.decided(approval, kind);
            child::emit_deltas(&mut shared, &self.tx);
            parked.rpc
        };
        self.with_child(|running| running.send(&wire::permission_selected(&rpc, choice)))?
            .map_err(|error| ProviderError::Unavailable { reason: format!("the agent is unreachable: {error}") })?;
        Ok(Ack::Accepted)
    }
}

/// Now, as Unix milliseconds.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|at| u64::try_from(at.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

impl ProviderAdapter for AcpAdapter {
    fn id(&self) -> ProviderId {
        fold::MARK
    }

    /// Spawn the agent and shake hands: its `initialize` answer fills the
    /// capability table.
    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if self.handshake().is_some() {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        *self.client.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(client.clone());
        let handshake = self.start()?;
        let fallback = std::path::Path::new(&self.program)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.program.clone());
        Ok(Handshake {
            provider: fold::MARK,
            agent_name: handshake.name.unwrap_or(fallback),
            agent_version: handshake.version.unwrap_or_default(),
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        caps::capabilities(self.handshake().as_ref())
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        match command {
            Command::OpenSession { request_id, workspace, env, .. } => self.open_session(&request_id, workspace.as_deref(), env),
            Command::ResumeSession { session_id, workspace, .. } => self.resume_session(&session_id, workspace.as_deref()),
            Command::SubmitInput { request_id, session_id, parts, display_text, .. } => {
                self.submit(&request_id, &session_id, &parts, display_text.as_deref())
            }
            Command::InterruptTurn { session_id, .. } | Command::CancelTurn { session_id, .. } => self.interrupt(&session_id),
            Command::DecideApproval { session_id, approval, choice, .. } => self.decide(&session_id, &approval, &choice),
            Command::ListPending { session_id } => {
                self.check_session(&session_id)?;
                let shared = lock(&self.shared);
                Ok(Ack::PendingWork {
                    approvals: shared
                        .parked
                        .iter()
                        .map(|parked| PendingApproval {
                            id: parked.approval.clone(),
                            session_id: session_id.clone(),
                            headline: parked.headline.clone(),
                            stage_token: None,
                        })
                        .collect(),
                    questions: Vec::new(),
                })
            }
            Command::ReadSession { session_id, .. } => {
                self.check_session(&session_id)?;
                Ok(Ack::SessionRead { session_id, title: None, first_shell_command: None })
            }
            // "Retry now" on a lost child: revive it here, without a turn.
            Command::FollowSession { session_id, .. } => {
                self.check_session(&session_id)?;
                self.revive()?;
                Ok(Ack::Accepted)
            }
            Command::UnfollowSession { .. } => Ok(Ack::Accepted),
            Command::ListSessions { .. } => Err(ProviderError::Rejected {
                reason: "ACP v1 lists no sessions; Baaz keeps its own record of them".into(),
            }),
            Command::ReclaimQueued { .. } => Err(ProviderError::Rejected { reason: "an ACP agent queues nothing".into() }),
            Command::PageTranscript { .. } | Command::ReadStoredOutput { .. } => Err(ProviderError::unsupported(
                "page-transcript",
                "an ACP agent's history comes back whole with session/load; follow the live session instead",
            )),
            // Refused by the gate before they arrive (their capability is
            // Unavailable in every table): answered typed all the same.
            Command::ForkSession { .. }
            | Command::CompactSession { .. }
            | Command::SelectModel { .. }
            | Command::SelectApprovalMode { .. }
            | Command::SetPlanMode { .. }
            | Command::RunShell { .. }
            | Command::SteerInput { .. }
            | Command::ListModels { .. }
            | Command::AnswerQuestion { .. }
            | Command::DismissQuestion { .. }
            | Command::ClarifyQuestion { .. }
            | Command::ReadAccount
            | Command::ReadUsage
            | Command::BeginLogin { .. }
            | Command::CancelLogin
            | Command::LogOut => {
                let capability = command.capability();
                Err(ProviderError::unsupported(capability, "ACP v1 carries no such request"))
            }
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    fn stderr_tail(&self) -> Vec<String> {
        self.stderr.lines()
    }

    fn shutdown(&mut self) {
        let child = self.child.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
        if let Some(running) = child {
            running.shutdown();
        }
    }
}

impl Drop for AcpAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}
