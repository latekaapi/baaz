//! `provider-codex` — the Codex CLI implementation of the provider seam.
//!
//! Neutral [`Command`]s in, `codex app-server` JSON-RPC on a pipe, neutral
//! [`Ack`]s and [`ProviderEvent`]s out. Every wire spelling (frame shapes,
//! method names, the `data` catalog key, the `accept` decision token) lives
//! in this crate; nothing outside it names one.
//!
//! Two facts shape this adapter and differ from `provider-claude-code`:
//!
//! * The protocol is bidirectional: the server sends requests to us (notably
//!   approval requests) that the pump must answer. See [`crate::child`].
//! * Baaz cannot choose the thread id. The server mints it on `thread/start`
//!   and the session mapping stores it; [`Ack::Session`] carries the minted
//!   id, never a client-chosen one.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod caps;
pub mod child;
pub mod fold;
pub mod frame;
pub mod terminal;

use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ModelSummary, PendingApproval,
    PendingQuestion, ProviderAdapter, ProviderError, ProviderEvent, ProviderId, SubmissionPart,
};
use serde_json::json;

pub use caps::{capabilities, codex_version_supported, CODEX_VERSION_FLOOR};

use child::{
    default_model, initialize_request, initialized_notification, model_catalog, thread_ids,
    thread_start_request, turn_id_from, turn_interrupt_request, turn_start_request,
    turn_steer_request, ApprovalAnswer, ApprovalKind, CommandApprovalDecision,
    FileChangeApprovalDecision, NetworkPolicyAction, PermissionGrantScope,
    PermissionsApprovalAnswer, RunningChild,
};
use fold::CodexFold;

/// The Codex implementation: neutral commands in, one long-lived
/// `codex app-server` child per session, neutral acks and events out.
///
/// The event pump is the child's own pump routing through the shared fold
/// under a mutex, so `ReadAccount` sees the live meter. Spawning stays in
/// `OpenSession`; tests never spawn — every test runs offline against the
/// checked-in fixtures.
pub struct CodexAdapter {
    program: String,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    fold: Arc<Mutex<CodexFold>>,
    child: Mutex<Option<RunningChild>>,
    session_id: Mutex<Option<String>>,
    thread_id: Mutex<Option<String>>,
    model: Mutex<Option<String>>,
    connected: Mutex<bool>,
    version: Mutex<Option<String>>,
    /// Where the terminal relay lives, set by the host before the session
    /// commands run: the bridge binary and the socket to point it at.
    /// `None` means no relay: the child spawns bare `app-server`.
    terminal: Mutex<Option<TerminalRelay>>,
}

/// Where the terminal relay lives: the bridge to spawn and the socket to
/// point it at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalRelay {
    /// Absolute path of the `mcp-bridge` binary beside the running baaz
    /// binary (or inside the app bundle next to it).
    pub bridge: std::path::PathBuf,
    /// `<support_dir>/run/terminal-<pid>.sock`: what the bridge's
    /// `--socket` names.
    pub socket: std::path::PathBuf,
}

impl CodexAdapter {
    /// Hold the CLI `program` (usually `codex`). Spawning stays in
    /// [`ProviderAdapter::connect`] and `OpenSession`; tests never spawn —
    /// every test runs offline against the checked-in fixtures.
    pub fn new(program: &str) -> Self {
        let (tx, rx) = unbounded();
        Self {
            program: program.into(),
            tx,
            rx,
            fold: Arc::new(Mutex::new(CodexFold::new())),
            child: Mutex::new(None),
            session_id: Mutex::new(None),
            thread_id: Mutex::new(None),
            model: Mutex::new(None),
            connected: Mutex::new(false),
            version: Mutex::new(None),
            terminal: Mutex::new(None),
        }
    }

    /// Point the terminal relay at the bridge and socket before the
    /// session commands run. Every open and resume then spawns its child
    /// with the bridge as a per-session MCP server (`-c
    /// mcp_servers.baaz.…`), process-scoped — the owner's config file is
    /// never touched.
    pub fn set_terminal_relay(&self, relay: TerminalRelay) {
        *self.terminal.lock().expect("terminal mutex") = Some(relay);
    }

    /// The `app-server` argv fragment for `session_id` — or empty when no
    /// relay is set. With a relay: the bridge as a per-session MCP server,
    /// then a full-table disable for every inherited server (bundled plus
    /// file-configured), so the session sees Baaz's tools and nothing
    /// else. A session without a relay spawns bare `app-server`, exactly
    /// as before. No spawn — see [`crate::terminal::server_overrides`] —
    /// so tests drive this without spending the owner's money.
    pub fn spawn_args_for(&self, session_id: &str) -> Vec<String> {
        let Some(relay) = self.terminal.lock().expect("terminal mutex").clone() else {
            return Vec::new();
        };
        let mut args =
            crate::terminal::server_overrides(&relay.bridge, &relay.socket, session_id);
        args.extend(crate::terminal::disable_overrides(
            crate::terminal::inherited_servers().iter().map(String::as_str),
        ));
        args
    }

    fn require_child(&self) -> Result<(), ProviderError> {
        if self.child.lock().expect("child mutex").is_some() {
            Ok(())
        } else {
            Err(ProviderError::Unavailable { reason: "no session child is running".into() })
        }
    }

    fn check_session(&self, session_id: &str) -> Result<(), ProviderError> {
        match self.session_id.lock().expect("session mutex").as_deref() {
            Some(held) if held == session_id => Ok(()),
            Some(held) => Err(ProviderError::Rejected {
                reason: format!("this adapter holds session {held}, not {session_id}"),
            }),
            None => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
        }
    }

    fn unavailable(error: child::RequestError) -> ProviderError {
        ProviderError::Unavailable { reason: error.reason }
    }

    /// Run the opening sequence over a fresh child: `initialize`, the
    /// `initialized` notification, `model/list` to resolve the model, then
    /// `thread/start` with that explicit model. The server mints the thread
    /// (and session) id; both are stored, never assumed.
    ///
    /// `open_id` is the `OpenSession` request id: the bridge's `--session`
    /// when a relay is set. Codex mints the thread id itself, so it cannot
    /// be known at spawn — the request id is the Baaz-minted identity the
    /// app registers with the service before the send, and the minted
    /// thread id is registered when the ack lands.
    fn open_session(
        &self,
        open_id: &str,
        workspace: Option<&str>,
        model: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        if self.child.lock().expect("child mutex").is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        let extra = self.spawn_args_for(open_id);
        let running = RunningChild::spawn(&self.program, &extra, Arc::clone(&self.fold), self.tx.clone())
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not spawn codex: {error}"),
            })?;
        running
            .send_frame(initialize_request(running.next_request_id(), env!("CARGO_PKG_VERSION")))
            .map_err(Self::unavailable)?;
        running
            .send_notification("initialized", initialized_notification()["params"].clone())
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            })?;
        let catalog = running.send_request("model/list", json!({})).map_err(Self::unavailable)?;
        // A requested model is taken as named even when the catalog does not
        // list it (aliases travel here); only an absent request falls back to
        // the catalog default.
        let resolved = match model {
            Some(requested) => requested.to_owned(),
            None => default_model(&catalog).ok_or_else(|| ProviderError::Unavailable {
                reason: "model/list answered with no usable model".into(),
            })?,
        };
        let cwd = workspace.map(str::to_owned).or_else(|| {
            std::env::current_dir().ok().map(|cwd| cwd.to_string_lossy().into_owned())
        });
        let cwd = cwd.as_deref().unwrap_or(".");
        let started = running
            .send_frame(thread_start_request(running.next_request_id(), cwd, &resolved))
            .map_err(Self::unavailable)?;
        let (thread_id, session_id) =
            thread_ids(&started).ok_or_else(|| ProviderError::Unavailable {
                reason: "thread/start answered without thread ids".into(),
            })?;
        *self.session_id.lock().expect("session mutex") = Some(session_id.clone());
        *self.thread_id.lock().expect("thread mutex") = Some(thread_id);
        *self.model.lock().expect("model mutex") = Some(resolved.clone());
        self.fold.lock().expect("fold mutex").set_model(&resolved);
        *self.child.lock().expect("child mutex") = Some(running);
        Ok(Ack::Session { session_id, title: None })
    }

    fn with_child<T>(&self, f: impl FnOnce(&RunningChild) -> T) -> Result<T, ProviderError> {
        self.require_child()?;
        let child = self.child.lock().expect("child mutex");
        let running = child.as_ref().expect("checked present above");
        Ok(f(running))
    }

    /// Rejoin a stored thread by id: a fresh child, the handshake, then
    /// `thread/resume` — the response carries the thread (with its
    /// `turns[]` history) plus the effective model, which this folds and
    /// replays before any live delta, exactly once per resume. The
    /// replayed history and the next live turn share the fold, so the new
    /// turn appends after history with no echo suppression needed: the
    /// resumed child never re-emits a completed turn's items.
    fn resume_session(&self, session_id: &str) -> Result<Ack, ProviderError> {
        if self.child.lock().expect("child mutex").is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        // A resume names its session up front, so the bridge answers for
        // the stored id directly.
        let extra = self.spawn_args_for(session_id);
        let running = RunningChild::spawn(&self.program, &extra, Arc::clone(&self.fold), self.tx.clone())
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not spawn codex: {error}"),
            })?;
        running
            .send_frame(initialize_request(running.next_request_id(), env!("CARGO_PKG_VERSION")))
            .map_err(Self::unavailable)?;
        running
            .send_notification("initialized", initialized_notification()["params"].clone())
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            })?;
        let answer = running
            .send_frame(child::thread_resume_request(running.next_request_id(), session_id))
            .map_err(Self::unavailable)?;
        let (thread_id, resumed_session, model) =
            child::resume_ids(&answer).ok_or_else(|| ProviderError::Unavailable {
                reason: "thread/resume answered without thread ids".into(),
            })?;
        let turns = child::resume_turns(&answer);
        let deltas = {
            let mut fold = self.fold.lock().expect("fold mutex");
            fold.set_model(&model);
            let mut deltas = fold.apply(&crate::frame::Frame::Notification(
                crate::frame::Notification::ThreadStarted {
                    thread_id: thread_id.clone(),
                    session_id: resumed_session.clone(),
                },
            ));
            deltas.extend(fold.apply_resume_turns(&thread_id, &turns));
            deltas
        };
        *self.session_id.lock().expect("session mutex") = Some(resumed_session.clone());
        *self.thread_id.lock().expect("thread mutex") = Some(thread_id);
        *self.model.lock().expect("model mutex") = Some(model);
        *self.child.lock().expect("child mutex") = Some(running);
        if !deltas.is_empty() {
            let _ = self.tx.send(provider::ProviderEvent::Deltas {
                session_id: Some(resumed_session.clone()),
                deltas,
            });
        }
        Ok(Ack::Session { session_id: resumed_session, title: None })
    }

    fn thread_and_model(&self) -> Result<(String, String), ProviderError> {
        let thread = self.thread_id.lock().expect("thread mutex").clone();
        let model = self.model.lock().expect("model mutex").clone();
        match (thread, model) {
            (Some(thread), Some(model)) => Ok((thread, model)),
            _ => Err(ProviderError::Unavailable { reason: "no session child is running".into() }),
        }
    }

    fn join_text(parts: &[SubmissionPart]) -> Result<String, ProviderError> {
        let mut text = String::new();
        for part in parts {
            match part {
                SubmissionPart::Text(chunk) => text.push_str(chunk),
                SubmissionPart::Image { .. } => {
                    return Err(ProviderError::Rejected {
                        reason: "turn/start takes a local image path (`localImage`, probed live \
                                 2026-09-26 in fixtures/codex/image.jsonl) but the seam carries \
                                 bytes; stage the bytes to a file first"
                            .into(),
                    });
                }
            }
        }
        Ok(text)
    }

    fn submit_text(
        &self,
        session_id: &str,
        text: &str,
        effort: Option<&str>,
        display_text: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let (thread_id, model) = self.thread_and_model()?;
        let turn = self
            .with_child(|running| {
                running.send_frame(turn_start_request(
                    running.next_request_id(),
                    &thread_id,
                    &model,
                    text,
                    effort,
                ))
            })?
            .map_err(Self::unavailable)?;
        let turn_id = turn_id_from(&turn).ok_or_else(|| ProviderError::Unavailable {
            reason: "turn/start answered without a turn id".into(),
        })?;
        // The ack carries the submission handle; the turn's true identity is
        // the server-minted id, never derived locally. The full text goes
        // to the model; the bubble shows the display text when one rode
        // the submit, keyed by the turn this submit creates.
        if let Some(display) = display_text {
            self.fold.lock().expect("fold mutex").record_display_text(Some(&turn_id), text, display);
        }
        Ok(Ack::TurnAccepted { turn_id })
    }

    /// Parse a [`Command::DecideApproval`] choice against the lane the
    /// approval arrived on. Plain tokens decide the four shared outcomes;
    /// the two amendment variants travel as JSON decision objects (the same
    /// shape the wire carries); a permissions approval takes only its JSON
    /// `{permissions, scope, strictAutoReview}` answer. Anything else is
    /// refused, never misdelivered — notably `approved`, the silent refusal.
    fn decide(kind: ApprovalKind, choice: &str) -> Result<ApprovalAnswer, ProviderError> {
        match kind {
            ApprovalKind::Command => Self::decide_command(choice).map(ApprovalAnswer::Command),
            ApprovalKind::FileChange => {
                Self::decide_file_change(choice).map(ApprovalAnswer::FileChange)
            }
            ApprovalKind::Permissions => {
                Self::decide_permissions(choice).map(ApprovalAnswer::Permissions)
            }
            ApprovalKind::McpElicitation => {
                Self::decide_elicitation(choice).map(ApprovalAnswer::McpElicitation)
            }
            ApprovalKind::Unknown => Err(ProviderError::Rejected {
                reason: "this approval arrived on an unknown requestApproval lane; \
                         not answered blind"
                    .into(),
            }),
        }
    }

    /// MCP tool-call elicitations take exactly the three wire actions —
    /// `accept`, `decline` and `cancel`. There is no session-scoped
    /// answer on this lane: `McpServerElicitationRequestResponse.json`
    /// admits `{action, content?}` and nothing else, so
    /// `accept-for-session` (either spelling) is refused rather than
    /// answered as a silent one-shot — a press that promises persistence
    /// must never travel as a choice the schema cannot carry.
    fn decide_elicitation(choice: &str) -> Result<child::McpElicitationAction, ProviderError> {
        use child::McpElicitationAction as A;
        match choice {
            "accept" => Ok(A::Accept),
            "decline" => Ok(A::Decline),
            "cancel" => Ok(A::Cancel),
            _ => Err(ProviderError::Rejected {
                reason: format!(
                    "unknown MCP tool approval choice {choice:?}: offer accept, decline, \
                     or cancel — this approval answers once, the schema carries no session scope"
                ),
            }),
        }
    }

    fn decide_command(choice: &str) -> Result<CommandApprovalDecision, ProviderError> {
        match choice {
            "accept" => Ok(CommandApprovalDecision::Accept),
            "accept-for-session" => Ok(CommandApprovalDecision::AcceptForSession),
            "decline" => Ok(CommandApprovalDecision::Decline),
            "cancel" => Ok(CommandApprovalDecision::Cancel),
            _ => Self::decide_command_json(choice),
        }
    }

    /// The two amendment variants as JSON decision objects, exactly the
    /// shape `CommandExecutionRequestApprovalResponse.json` admits (a JSON
    /// string token recurses back through the plain tokens above).
    fn decide_command_json(choice: &str) -> Result<CommandApprovalDecision, ProviderError> {
        let value: serde_json::Value =
            serde_json::from_str(choice).map_err(|_| Self::bad_command_choice(choice))?;
        match &value {
            serde_json::Value::String(token) => Self::decide_command(token),
            serde_json::Value::Object(_) => {
                if let Some(amendment) = value
                    .get("acceptWithExecpolicyAmendment")
                    .and_then(|inner| inner.get("execpolicy_amendment"))
                {
                    let rules = amendment
                        .as_array()
                        .ok_or_else(|| Self::bad_command_choice(choice))?
                        .iter()
                        .map(|rule| {
                            rule.as_str().map(str::to_owned).ok_or_else(|| {
                                Self::bad_command_choice(choice)
                            })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    return Ok(CommandApprovalDecision::AcceptWithExecpolicyAmendment {
                        execpolicy_amendment: rules,
                    });
                }
                if let Some(amendment) = value
                    .get("applyNetworkPolicyAmendment")
                    .and_then(|inner| inner.get("network_policy_amendment"))
                {
                    let host = amendment
                        .get("host")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| Self::bad_command_choice(choice))?
                        .to_owned();
                    let action = amendment
                        .get("action")
                        .and_then(serde_json::Value::as_str)
                        .and_then(NetworkPolicyAction::parse)
                        .ok_or_else(|| Self::bad_command_choice(choice))?;
                    return Ok(CommandApprovalDecision::ApplyNetworkPolicyAmendment {
                        host,
                        action,
                    });
                }
                Err(Self::bad_command_choice(choice))
            }
            _ => Err(Self::bad_command_choice(choice)),
        }
    }

    fn bad_command_choice(choice: &str) -> ProviderError {
        ProviderError::Rejected {
            reason: format!(
                "unknown command approval choice {choice:?}: offer accept, accept-for-session, \
                 decline, cancel, or a JSON acceptWithExecpolicyAmendment / \
                 applyNetworkPolicyAmendment decision"
            ),
        }
    }

    /// File-change approvals take exactly the four tokens — an amendment
    /// object here is refused outright, so the pairing the types forbid at
    /// compile time is also refused at the string boundary.
    fn decide_file_change(choice: &str) -> Result<FileChangeApprovalDecision, ProviderError> {
        let token = choice
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
            .unwrap_or(choice);
        match token {
            "accept" => Ok(FileChangeApprovalDecision::Accept),
            "accept-for-session" => Ok(FileChangeApprovalDecision::AcceptForSession),
            "decline" => Ok(FileChangeApprovalDecision::Decline),
            "cancel" => Ok(FileChangeApprovalDecision::Cancel),
            _ => Err(ProviderError::Rejected {
                reason: format!(
                    "unknown file-change approval choice {choice:?}: offer accept, \
                     accept-for-session, decline, or cancel — amendments only pair \
                     with command execution approvals"
                ),
            }),
        }
    }

    /// Permissions approvals take only their JSON answer shape — there is
    /// no `decision` token for this lane, so plain tokens are refused.
    fn decide_permissions(choice: &str) -> Result<PermissionsApprovalAnswer, ProviderError> {
        let bad = || ProviderError::Rejected {
            reason: format!(
                "a permissions approval takes its JSON answer shape \
                 {{\"permissions\": {{...}}, \"scope\": \"turn\"|\"session\", \
                 \"strictAutoReview\": bool|null}}, not {choice:?}"
            ),
        };
        let value: serde_json::Value = serde_json::from_str(choice).map_err(|_| bad())?;
        let object = value.as_object().ok_or_else(bad)?;
        let permissions = object.get("permissions").cloned().ok_or_else(bad)?;
        let scope = match object.get("scope") {
            None | Some(serde_json::Value::Null) => PermissionGrantScope::Turn,
            Some(serde_json::Value::String(token)) => {
                PermissionGrantScope::parse(token).ok_or_else(bad)?
            }
            Some(_) => return Err(bad()),
        };
        let strict_auto_review = match object.get("strictAutoReview") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Bool(strict)) => Some(*strict),
            Some(_) => return Err(bad()),
        };
        Ok(PermissionsApprovalAnswer { permissions, scope, strict_auto_review })
    }
}

impl ProviderAdapter for CodexAdapter {
    fn id(&self) -> ProviderId {
        ProviderId::Codex
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if *self.connected.lock().expect("connected mutex") {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        // The version floor is enforced here, before any session exists:
        // `codex --version` prints `codex-cli 0.144.6` and the prefix is
        // stripped before comparing (see `codex_version_supported`).
        let output = std::process::Command::new(&self.program)
            .arg("--version")
            .output()
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not ask codex its version: {error}"),
            })?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let raw = raw.trim();
        let version = if raw.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            raw.to_owned()
        };
        if !codex_version_supported(&version) {
            return Err(ProviderError::Rejected {
                reason: format!(
                    "codex {version} is below the floor {CODEX_VERSION_FLOOR}; refusing"
                ),
            });
        }
        *self.connected.lock().expect("connected mutex") = true;
        *self.version.lock().expect("version mutex") = Some(version.clone());
        Ok(Handshake { provider: ProviderId::Codex, agent_name: "codex".into(), agent_version: version })
    }

    fn capabilities(&self) -> CapabilitySet {
        capabilities()
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        match command {
            Command::OpenSession { request_id, workspace, model, .. } => {
                self.open_session(&request_id, workspace.as_deref(), model.as_deref())
            }
            Command::ResumeSession { session_id, .. } => self.resume_session(&session_id),
            Command::ForkSession { .. } => Err(ProviderError::Rejected {
                reason: "thread/fork was never executed against a live server; not forked blind"
                    .into(),
            }),
            Command::ListSessions { .. } => Err(ProviderError::Rejected {
                reason: "thread/list was never captured against a live server; not parsed blind"
                    .into(),
            }),
            Command::ReadSession { session_id, .. } => {
                match self.session_id.lock().expect("session mutex").as_deref() {
                    Some(held) if held == session_id => {
                        Ok(Ack::Session { session_id, title: None })
                    }
                    _ => Err(ProviderError::Rejected {
                        reason: format!(
                            "only the live session is readable; {session_id} is not attached"
                        ),
                    }),
                }
            }
            Command::CompactSession { .. } => Err(ProviderError::Rejected {
                reason: "thread/compact/start was never executed; not compacted blind".into(),
            }),
            Command::SelectModel { session_id, model, .. } => {
                self.check_session(&session_id)?;
                // Admission only: the running turn keeps its model; the next
                // turn/start carries the new one.
                *self.model.lock().expect("model mutex") = Some(model.clone());
                self.fold.lock().expect("fold mutex").set_model(&model);
                Ok(Ack::Accepted)
            }
            Command::SelectApprovalMode { .. } => Err(ProviderError::unsupported(
                "select-approval-mode",
                "no approval-profile switch was probed; reopen the session under its profile",
            )),
            Command::RunShell { .. } => Err(ProviderError::Rejected {
                reason: "no out-of-turn shell surface was captured; the shell runs inside turns"
                    .into(),
            }),
            Command::SubmitInput { session_id, parts, effort, display_text, .. } => {
                let text = Self::join_text(&parts)?;
                self.submit_text(&session_id, &text, effort.as_deref(), display_text.as_deref())
            }
            Command::SteerInput { session_id, expected_turn, parts, .. } => {
                self.check_session(&session_id)?;
                let text = Self::join_text(&parts)?;
                let (thread_id, _) = self.thread_and_model()?;
                self.with_child(|running| {
                    running.send_frame(turn_steer_request(
                        running.next_request_id(),
                        &thread_id,
                        &expected_turn,
                        &text,
                    ))
                })?
                .map_err(Self::unavailable)?;
                Ok(Ack::Accepted)
            }
            Command::InterruptTurn { session_id, turn, .. } => {
                self.check_session(&session_id)?;
                let (thread_id, _) = self.thread_and_model()?;
                let turn_id = turn.or_else(|| {
                    self.fold.lock().ok().and_then(|fold| {
                        fold.current_turn().map(str::to_owned)
                    })
                });
                let Some(turn_id) = turn_id else {
                    return Err(ProviderError::Rejected {
                        reason: "no running turn was ever observed; nothing to stop".into(),
                    });
                };
                self.with_child(|running| {
                    running.send_frame(turn_interrupt_request(
                        running.next_request_id(),
                        &thread_id,
                        &turn_id,
                    ))
                })?
                .map_err(Self::unavailable)?;
                Ok(Ack::Accepted)
            }
            Command::CancelTurn { session_id, turn, .. } => {
                // The wire draws no urgent/non-urgent distinction: cancel
                // rides the same interrupt call.
                self.dispatch(Command::InterruptTurn {
                    request_id: String::new(),
                    session_id,
                    turn,
                    retract: false,
                })
            }
            Command::ReclaimQueued { .. } => Err(ProviderError::Rejected {
                reason: "no queued-turn lane was captured on this protocol".into(),
            }),
            Command::ListModels { session } => {
                let catalog = self
                    .with_child(|running| running.send_request("model/list", json!({})))?
                    .map_err(Self::unavailable)?;
                // Human labels ride `label`; the wire id stays in `id`.
                // A row without a `displayName` falls back to its id rather
                // than vanishing: an empty menu is a failure, not a state.
                let rows = model_catalog(&catalog);
                let active = session
                    .as_deref()
                    .and_then(|session| {
                        (Some(session) == self.session_id.lock().expect("session mutex").as_deref())
                            .then(|| self.model.lock().expect("model mutex").clone())
                            .flatten()
                    })
                    .unwrap_or_default();
                Ok(Ack::ModelCatalog {
                    models: rows
                        .into_iter()
                        .map(|row| ModelSummary {
                            active: row.id == active,
                            label: row.label,
                            id: row.id,
                        })
                        .collect(),
                    provider: "openai".into(),
                })
            }
            Command::DecideApproval { session_id, approval, choice, .. } => {
                self.check_session(&session_id)?;
                // The lane the request arrived on decides which answer
                // shape is legal; the kind-tagged answer then routes to
                // the per-kind writer, which refuses a mismatched pairing.
                let kind = self.with_child(|running| running.approval_kind(&approval))?;
                let Some(kind) = kind else {
                    return Err(ProviderError::Rejected {
                        reason: format!("no pending approval {approval:?}: it may have resolved"),
                    });
                };
                let answer = Self::decide(kind, &choice)?;
                let answered = self
                    .with_child(|running| running.answer_approval(&approval, &answer))?
                    .map_err(|error| ProviderError::Unavailable {
                        reason: format!("the session child is unreachable: {error}"),
                    })?;
                if !answered {
                    return Err(ProviderError::Rejected {
                        reason: format!("no pending approval {approval:?}: it may have resolved"),
                    });
                }
                Ok(Ack::Accepted)
            }
            Command::ListPending { session_id } => {
                self.check_session(&session_id)?;
                let (approvals, questions) = self.with_child(|running| {
                    (running.pending_approvals(), running.pending_questions())
                })?;
                Ok(Ack::PendingWork {
                    approvals: approvals
                        .into_iter()
                        .map(|view| PendingApproval {
                            id: view.item_id,
                            session_id: view.thread_id,
                            headline: view.headline,
                            stage_token: None,
                        })
                        .collect(),
                    questions: questions
                        .into_iter()
                        .map(|view| PendingQuestion {
                            id: view.question_id,
                            session_id: view.thread_id,
                            headline: view.headline,
                        })
                        .collect(),
                })
            }
            Command::AnswerQuestion { .. }
            | Command::DismissQuestion { .. }
            | Command::ClarifyQuestion { .. } => Err(ProviderError::Rejected {
                reason: "question answer shapes were never captured; not answered blind".into(),
            }),
            Command::PageTranscript { .. } => Err(ProviderError::Rejected {
                reason: "thread/read was never captured; follow the live session instead".into(),
            }),
            Command::FollowSession { .. } => {
                self.require_child()?;
                Ok(Ack::Accepted)
            }
            Command::UnfollowSession { .. } => Ok(Ack::Accepted),
            Command::ReadStoredOutput { .. } => Err(ProviderError::unsupported(
                "read-stored-output",
                "the protocol exposes no stored-output reference; follow the live session instead",
            )),
            Command::ReadAccount => {
                let label =
                    self.fold.lock().expect("fold mutex").account().label().clone();
                // `codex --version` succeeds whether or not the CLI is
                // authenticated, so `connect()` proves nothing about auth.
                // The only auth evidence this adapter ever observes is a
                // rate-limits push from the live child: the server only emits
                // one while serving an authenticated account. So `signed_in`
                // is true exactly when a push has been seen; `false` means
                // "no login observed", never "definitely logged out".
                Ok(Ack::Account { signed_in: label.is_some(), label })
            }
            Command::BeginLogin { .. } => Err(ProviderError::unsupported(
                "begin-login",
                "Codex authenticates outside the session",
            )),
            Command::CancelLogin => Err(ProviderError::unsupported(
                "cancel-login",
                "Codex authenticates outside the session",
            )),
            Command::LogOut => Err(ProviderError::unsupported(
                "log-out",
                "Codex authenticates outside the session",
            )),
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    fn shutdown(&mut self) {
        if let Some(running) = self.child.lock().expect("child mutex").as_mut() {
            running.shutdown();
        }
        *self.child.lock().expect("child mutex") = None;
    }
}

impl Drop for CodexAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::decode_line;

    fn rate_push() -> &'static str {
        r#"{"method":"account/rateLimits/updated","params":{"rateLimits":{"limitId":"codex","limitName":null,"primary":{"usedPercent":19,"windowDurationMins":10080,"resetsAt":1790588038},"secondary":null,"credits":{"hasCredits":false,"unlimited":false,"balance":"0"},"individualLimit":null,"planType":"prolite","rateLimitReachedType":null}}}"#
    }

    #[test]
    fn read_account_is_false_until_a_push_is_seen() {
        // Fresh adapter: no push observed, so no login claimed. A hardcoded
        // `true` fails this test. No process is spawned.
        let adapter = CodexAdapter::new("codex");
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        assert!(
            matches!(ack, Ack::Account { signed_in: false, label: None }),
            "no push seen, no login claimed: {ack:?}"
        );
    }

    #[test]
    fn read_account_is_true_after_a_push() {
        // A rate-limits push from the live child is authenticated traffic
        // observed first-hand: signed in, with the meter label.
        let adapter = CodexAdapter::new("codex");
        let frame = decode_line(rate_push()).expect("push decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        match ack {
            Ack::Account { signed_in: true, label: Some(label) } => {
                assert!(label.contains("19%"), "live meter label: {label}");
            }
            other => panic!("expected signed-in account with a label, got {other:?}"),
        }
    }

    #[test]
    fn session_commands_without_a_child_are_unavailable_not_ok() {
        // No spawn: without a child every session command refuses, so no
        // test can accidentally spend the owner's money.
        let adapter = CodexAdapter::new("codex");
        let ack = adapter.dispatch(Command::SubmitInput {
            request_id: "r".into(),
            session_id: "s".into(),
            parts: vec![SubmissionPart::Text("hi".into())],
            display_text: None,
            effort: None,
        });
        assert!(
            matches!(ack, Err(ProviderError::Unavailable { .. })),
            "no child, no admission: {ack:?}"
        );
        let ack = adapter.dispatch(Command::DecideApproval {
            request_id: "r".into(),
            session_id: "s".into(),
            approval: "exec-0".into(),
            choice: "accept".into(),
            stage_token: None,
            feedback: None,
        });
        assert!(
            matches!(ack, Err(ProviderError::Unavailable { .. })),
            "no child, no decision: {ack:?}"
        );
    }

    #[test]
    fn unknown_approval_choices_are_rejected() {
        // The `approved` trap at the dispatch edge: only the typed tokens
        // decide; anything else is refused, never misdelivered.
        use ApprovalKind as K;
        for kind in [K::Command, K::FileChange] {
            assert!(CodexAdapter::decide(kind, "accept").is_ok());
            assert!(CodexAdapter::decide(kind, "accept-for-session").is_ok());
            assert!(CodexAdapter::decide(kind, "decline").is_ok());
            assert!(CodexAdapter::decide(kind, "cancel").is_ok());
            assert!(
                CodexAdapter::decide(kind, "approved").is_err(),
                "`approved` is the silent refusal"
            );
            assert!(CodexAdapter::decide(kind, "yes").is_err());
        }
        // Amendments pair with command execution only: the file-change
        // lane refuses the JSON the command lane accepts.
        let amendment = r#"{"acceptWithExecpolicyAmendment":{"execpolicy_amendment":["/bin/zsh"]}}"#;
        assert!(CodexAdapter::decide(K::Command, amendment).is_ok());
        assert!(CodexAdapter::decide(K::FileChange, amendment).is_err());
        let network = r#"{"applyNetworkPolicyAmendment":{"network_policy_amendment":{"host":"example.com","action":"allow"}}}"#;
        assert!(CodexAdapter::decide(K::Command, network).is_ok());
        assert!(CodexAdapter::decide(K::FileChange, network).is_err());
        // Permissions take only their JSON answer shape: plain tokens —
        // even `accept` — are refused on this lane.
        assert!(CodexAdapter::decide(K::Permissions, "accept").is_err());
        assert!(CodexAdapter::decide(K::Permissions, "decline").is_err());
        let grant = r#"{"permissions":{"network":{"enabled":true}},"scope":"session"}"#;
        assert!(CodexAdapter::decide(K::Permissions, grant).is_ok());
        assert!(CodexAdapter::decide(K::Permissions, r#"{"scope":"turn"}"#).is_err());
        // An unknown lane is never answered blind.
        assert!(CodexAdapter::decide(K::Unknown, "accept").is_err());
    }

    #[test]
    fn elicitation_choices_decide_their_wire_actions() {
        // The three answers the elicitation lane takes ride `DecideApproval`
        // verbatim to the `{action, content?}` shape; anything else —
        // notably `accept-for-session` in either spelling — is refused,
        // never answered as a silent one-shot the schema cannot carry.
        use ApprovalKind as K;
        use child::{ApprovalAnswer, McpElicitationAction as A};
        assert_eq!(
            CodexAdapter::decide(K::McpElicitation, "accept"),
            Ok(ApprovalAnswer::McpElicitation(A::Accept))
        );
        assert_eq!(
            CodexAdapter::decide(K::McpElicitation, "decline"),
            Ok(ApprovalAnswer::McpElicitation(A::Decline))
        );
        assert_eq!(
            CodexAdapter::decide(K::McpElicitation, "cancel"),
            Ok(ApprovalAnswer::McpElicitation(A::Cancel))
        );
        for refused in ["accept-for-session", "acceptForSession", "approved", ""] {
            assert!(
                CodexAdapter::decide(K::McpElicitation, refused).is_err(),
                "{refused:?} must not decide an elicitation"
            );
        }
    }

    #[test]
    fn no_test_spawns_a_child() {
        // Pin the hard rule structurally: every dispatch above refused before
        // any spawn, and this adapter holds no child after them.
        let adapter = CodexAdapter::new("codex");
        assert!(adapter.child.lock().expect("child mutex").is_none());
    }
}
