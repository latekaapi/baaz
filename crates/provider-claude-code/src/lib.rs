//! `provider-claude-code` — the Claude Code CLI implementation of the
//! provider seam.
//!
//! Neutral [`Command`]s in, `claude` argv on a pipe, neutral [`Ack`]s and
//! [`ProviderEvent`]s out. Every wire spelling (frame shapes, flags, slug
//! transform) lives in this crate; nothing outside it names one.
//!
//! Honest limits (see the report): the gate reads five recorded files. This
//! adapter has never run against a live CLI in this task — no turn was ever
//! steered or interrupted, `--fork-session` and sub-agent forwarding were
//! read off `--help`, the `<cwd-slug>` transform was observed on one
//! directory, and no Baaz UI has ever rendered one of these frames.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod account;
pub mod argv;
pub mod caps;
pub mod child;
pub mod fold;
pub mod frame;
pub mod history;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use aui_protocol::Delta;
use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ProviderAdapter, ProviderError,
    ProviderEvent, ProviderId, SessionSummary,
};

pub use caps::{capabilities, claude_version_supported, CLAUDE_VERSION_FLOOR};

use argv::SessionLaunch;
use child::RunningChild;
use fold::ClaudeFold;

/// The Claude Code implementation: neutral commands in, one long-lived
/// `claude` child per session, neutral acks and events out.
///
/// The event pump is the same decode-and-fold the fixture tests exercise
/// ([`fold::pump_reader`]), sharing the fold under a mutex so `ReadAccount`
/// sees the live meter.
pub struct ClaudeCodeAdapter {
    program: String,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    fold: Arc<Mutex<ClaudeFold>>,
    child: Mutex<Option<RunningChild>>,
    session_id: Mutex<Option<String>>,
    home_override: Option<PathBuf>,
    connected: Mutex<bool>,
    version: Mutex<Option<String>>,
}

impl ClaudeCodeAdapter {
    /// Hold the CLI `program` (usually `claude`). Spawning stays in
    /// [`ProviderAdapter::connect`] and the session commands; tests never
    /// spawn — every test runs offline against the checked-in fixtures.
    pub fn new(program: &str) -> Self {
        let (tx, rx) = unbounded();
        Self {
            program: program.into(),
            tx,
            rx,
            fold: Arc::new(Mutex::new(ClaudeFold::new())),
            child: Mutex::new(None),
            session_id: Mutex::new(None),
            home_override: None,
            connected: Mutex::new(false),
            version: Mutex::new(None),
        }
    }

    /// Override `$HOME` for stored-history lookup (tests).
    pub fn with_home(mut self, home: PathBuf) -> Self {
        self.home_override = Some(home);
        self
    }

    fn home(&self) -> Option<PathBuf> {
        match &self.home_override {
            Some(home) => Some(home.clone()),
            None => std::env::var_os("HOME").map(PathBuf::from),
        }
    }

    fn spawn_launch(&self, launch: &SessionLaunch) -> Result<Ack, ProviderError> {
        let mut child = self.child.lock().expect("child mutex");
        if child.is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        let running = RunningChild::spawn(
            &self.program,
            launch,
            Arc::clone(&self.fold),
            self.tx.clone(),
        )
        .map_err(|error| ProviderError::Unavailable {
            reason: format!("could not spawn claude: {error}"),
        })?;
        *child = Some(running);
        *self.session_id.lock().expect("session mutex") = Some(launch.session_id.clone());
        Ok(Ack::Session { session_id: launch.session_id.clone(), title: None })
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

    fn submit_text(&self, session_id: &str, text: &str, turn_id: String) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let line = argv::user_input_line(text);
        match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                reason: format!("the session child is unreachable: {error}"),
            })?,
            None => {
                return Err(ProviderError::Unavailable {
                    reason: "no session child is running".into(),
                })
            }
        }
        // The ack carries the submission handle; the turn's true identity
        // arrives on the event stream (the child replays user messages).
        Ok(Ack::TurnAccepted { turn_id })
    }

    fn find_stored(&self, session_id: &str) -> Option<PathBuf> {
        let home = self.home()?;
        let projects = home.join(".claude").join("projects");
        let Ok(entries) = std::fs::read_dir(projects) else { return None };
        for entry in entries.filter_map(|entry| entry.ok()) {
            let candidate = entry.path().join(format!("{session_id}.jsonl"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        None
    }

    fn stored_deltas(&self, session_id: &str) -> Result<Vec<Delta>, ProviderError> {
        let Some(path) = self.find_stored(session_id) else {
            return Err(ProviderError::Unavailable {
                reason: format!(
                    "no stored transcript for session {session_id}: a resumed child does not \
                     replay history, and no file names it — refusing to guess a different file"
                ),
            });
        };
        let text = std::fs::read_to_string(&path).map_err(|error| ProviderError::Unavailable {
            reason: format!("stored transcript is unreadable: {error}"),
        })?;
        let mut fold = ClaudeFold::new();
        let mut deltas = Vec::new();
        for line in text.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let Ok(frame) = frame::decode_line(line) else { continue };
            deltas.extend(fold.apply(&frame));
        }
        Ok(deltas)
    }
}

impl ProviderAdapter for ClaudeCodeAdapter {
    fn id(&self) -> ProviderId {
        aui_protocol::Provider::Claude
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if *self.connected.lock().expect("connected mutex") {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        // The version floor is enforced here, before any session exists:
        // `claude --version` prints `2.1.276 (Claude Code)` and the shared
        // parser tolerates that trailer.
        let output = std::process::Command::new(&self.program)
            .arg("--version")
            .output()
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not ask claude its version: {error}"),
            })?;
        let raw = String::from_utf8_lossy(&output.stdout);
        let raw = raw.trim();
        let version = if raw.is_empty() {
            String::from_utf8_lossy(&output.stderr).trim().to_owned()
        } else {
            raw.to_owned()
        };
        if !claude_version_supported(&version) {
            return Err(ProviderError::Rejected {
                reason: format!(
                    "claude {version} is below the floor {CLAUDE_VERSION_FLOOR}; refusing"
                ),
            });
        }
        *self.connected.lock().expect("connected mutex") = true;
        *self.version.lock().expect("version mutex") = Some(version.clone());
        Ok(Handshake {
            provider: aui_protocol::Provider::Claude,
            agent_name: "claude-code".into(),
            agent_version: version,
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        capabilities()
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        match command {
            Command::OpenSession { request_id, workspace, model, .. } => {
                let launch = argv::argv_for_open(&request_id, workspace.as_deref(), model.as_deref(), None);
                self.spawn_launch(&launch)
            }
            Command::ResumeSession { session_id, .. } => {
                let launch = argv::argv_for_resume(&session_id, None, None);
                self.spawn_launch(&launch)
            }
            Command::ForkSession { request_id, session_id, .. } => {
                let launch = argv::argv_for_fork(&request_id, &session_id, None, None);
                self.spawn_launch(&launch)
            }
            Command::ListSessions { workspace, .. } => {
                let home = self.home().ok_or_else(|| ProviderError::Unavailable {
                    reason: "no home directory to look for stored sessions under".into(),
                })?;
                let projects = home.join(".claude").join("projects");
                let mut ids = Vec::new();
                match workspace {
                    Some(workspace) => {
                        let resolved = history::resolve_cwd(std::path::Path::new(&workspace))
                            .map_err(|_| ProviderError::Unavailable {
                                reason: format!(
                                    "workspace {workspace} does not resolve; refusing to guess a slug"
                                ),
                            })?;
                        let dir = projects.join(history::slug_for_cwd(&resolved));
                        ids.extend(history::stored_session_ids(&dir));
                    }
                    None => {
                        let Ok(entries) = std::fs::read_dir(&projects) else {
                            return Ok(Ack::SessionIndex { sessions: Vec::new(), next_cursor: None });
                        };
                        for entry in entries.filter_map(|entry| entry.ok()) {
                            ids.extend(history::stored_session_ids(&entry.path()));
                        }
                    }
                }
                Ok(Ack::SessionIndex {
                    sessions: ids
                        .into_iter()
                        .map(|session_id| SessionSummary { session_id, title: None })
                        .collect(),
                    next_cursor: None,
                })
            }
            Command::ReadSession { session_id, .. } => match self.find_stored(&session_id) {
                Some(_) => Ok(Ack::Session { session_id, title: None }),
                None => Err(ProviderError::Unavailable {
                    reason: format!(
                        "no stored transcript for session {session_id}: refusing to guess a \
                         different file"
                    ),
                }),
            },
            Command::CompactSession { .. } => Err(ProviderError::unsupported(
                "compact-session",
                "no compact-now command exists over --print; compaction happens when the \
                 --autocompact window fills, not when asked",
            )),
            Command::SelectModel { .. } => Err(ProviderError::unsupported(
                "select-model",
                "session config is spawn-time (--model); reopen the session to change it",
            )),
            Command::SelectApprovalMode { .. } => Err(ProviderError::unsupported(
                "select-approval-mode",
                "session config is spawn-time (--permission-mode); reopen the session to change it",
            )),
            Command::RunShell { .. } => Err(ProviderError::unsupported(
                "run-shell",
                "no out-of-turn shell surface was probed over stream-json stdin; the Bash tool \
                 runs inside turns",
            )),
            Command::SubmitInput { request_id, session_id, parts, .. } => {
                let mut text = String::new();
                for part in &parts {
                    match part {
                        provider::SubmissionPart::Text(chunk) => text.push_str(chunk),
                        provider::SubmissionPart::Image { .. } => {
                            return Err(ProviderError::Rejected {
                                reason: "no image input shape was probed over stream-json stdin"
                                    .into(),
                            })
                        }
                    }
                }
                self.submit_text(&session_id, &text, request_id)
            }
            // Unverified, so attempted: a second stdin frame mid-turn is the
            // only lane the process shape offers, unprobed as it is.
            Command::SteerInput { request_id, session_id, parts, .. } => {
                let _ = request_id;
                let mut text = String::new();
                for part in &parts {
                    match part {
                        provider::SubmissionPart::Text(chunk) => text.push_str(chunk),
                        provider::SubmissionPart::Image { .. } => {
                            return Err(ProviderError::Rejected {
                                reason: "no image input shape was probed over stream-json stdin"
                                    .into(),
                            })
                        }
                    }
                }
                self.check_session(&session_id)?;
                let line = argv::user_input_line(&text);
                match self.child.lock().expect("child mutex").as_mut() {
                    Some(running) => {
                        running.send_line(&line).map_err(|error| ProviderError::Unavailable {
                            reason: format!("the session child is unreachable: {error}"),
                        })?;
                        Ok(Ack::Accepted)
                    }
                    None => Err(ProviderError::Unavailable {
                        reason: "no session child is running".into(),
                    }),
                }
            }
            // TurnControl is Unverified: interrupt/cancel over stdin was
            // never probed, and spelling it as success would be the lie this
            // seam exists to prevent.
            Command::InterruptTurn { .. } => Err(ProviderError::Rejected {
                reason: "interrupt over stream-json stdin was never probed; not attempted blind"
                    .into(),
            }),
            Command::CancelTurn { .. } => Err(ProviderError::Rejected {
                reason: "cancel over stream-json stdin was never probed; not attempted blind".into(),
            }),
            Command::ReclaimQueued { .. } => Err(ProviderError::Rejected {
                reason: "no queued-turn lane was probed over stream-json stdin".into(),
            }),
            Command::ListModels { .. } => Err(ProviderError::unsupported(
                "list-models",
                "no model-catalog surface was probed; Baaz supplies the list",
            )),
            Command::DecideApproval { .. } => Err(ProviderError::Rejected {
                reason: "no approval prompt surface was captured from the CLI; every approval id \
                         is unknown"
                    .into(),
            }),
            Command::ListPending { .. } => {
                self.require_child()?;
                // No prompt surface was captured, so the only honest
                // non-empty answer is impossible: report none.
                Ok(Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
            }
            // Unreachable through the gate (Questions is Unavailable);
            // refused here too, so the raw dispatch can never spell it Ok.
            Command::AnswerQuestion { .. } => Err(ProviderError::unsupported(
                "answer-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::DismissQuestion { .. } => Err(ProviderError::unsupported(
                "dismiss-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::ClarifyQuestion { .. } => Err(ProviderError::unsupported(
                "clarify-question",
                "Claude Code asks in prose; there is no question id to answer",
            )),
            Command::PageTranscript { session_id, after, limit, backward } => {
                let deltas = self.stored_deltas(&session_id)?;
                let len = deltas.len();
                let limit = (limit as usize).max(1);
                let (start, end) = if backward {
                    let end = after
                        .as_deref()
                        .and_then(|cursor| cursor.parse::<usize>().ok())
                        .unwrap_or(len)
                        .min(len);
                    (end.saturating_sub(limit), end)
                } else {
                    let start = after
                        .as_deref()
                        .and_then(|cursor| cursor.parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(len);
                    (start, (start + limit).min(len))
                };
                let next_cursor = if backward {
                    if start > 0 { Some(start.to_string()) } else { None }
                } else if end < len {
                    Some(end.to_string())
                } else {
                    None
                };
                Ok(Ack::TranscriptPage { deltas: deltas[start..end].to_vec(), next_cursor })
            }
            Command::FollowSession { .. } => {
                self.require_child()?;
                Ok(Ack::Accepted)
            }
            Command::UnfollowSession { .. } => Ok(Ack::Accepted),
            Command::ReadStoredOutput { .. } => Err(ProviderError::unsupported(
                "read-stored-output",
                "the CLI exposes no stored-output reference; page the stored transcript instead",
            )),
            Command::ReadAccount => {
                let label = self.fold.lock().expect("fold mutex").account().label();
                Ok(Ack::Account { signed_in: true, label })
            }
            Command::BeginLogin { .. } => Err(ProviderError::unsupported(
                "begin-login",
                "Claude Code authenticates outside the session; use `claude auth`",
            )),
            Command::CancelLogin => Err(ProviderError::unsupported(
                "cancel-login",
                "Claude Code authenticates outside the session; use `claude auth`",
            )),
            Command::LogOut => Err(ProviderError::unsupported(
                "log-out",
                "Claude Code authenticates outside the session; use `claude auth`",
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

impl Drop for ClaudeCodeAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}
