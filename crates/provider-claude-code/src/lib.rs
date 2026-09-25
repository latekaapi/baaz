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
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, PendingApproval, ProviderAdapter,
    ProviderError, ProviderEvent, ProviderId, SessionSummary,
};

pub use caps::{capabilities, claude_version_supported, CLAUDE_VERSION_FLOOR};

use argv::SessionLaunch;
use child::RunningChild;
use fold::{ClaudeFold, UnknownControlRequest};
use frame::ApprovalRequest;

/// One claimed answer: the request is already out of its queue, so this
/// value is the only right to answer it. Exactly one claimant can hold
/// it, which is what keeps two concurrent decisions from writing two
/// `control_response` lines for one `request_id`.
#[derive(Debug)]
enum ClaimedApproval {
    /// A `can_use_tool` request plus its minted answer line.
    Known(ApprovalRequest, String),
    /// An unknown-subtype request plus its minted answer line.
    Unknown(UnknownControlRequest, String),
}

impl ClaimedApproval {
    /// The minted `control_response` line to write to the child.
    fn line(&self) -> &str {
        match self {
            ClaimedApproval::Known(_, line) | ClaimedApproval::Unknown(_, line) => line,
        }
    }
}

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
    model: Mutex<Option<String>>,
    workspace: Mutex<Option<PathBuf>>,
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
            model: Mutex::new(None),
            workspace: Mutex::new(None),
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

    /// Override the session workspace cwd for stored-history lookup
    /// (tests). Production sets this from `OpenSession.workspace` in
    /// [`Self::spawn_launch`]; it is what [`history::stored_transcript_path`]
    /// resolves and slugs.
    pub fn with_workspace(self, workspace: PathBuf) -> Self {
        *self.workspace.lock().expect("workspace mutex") = Some(workspace);
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
        // The launch's `--model`, when one was requested: the fold learns
        // the rest from the `init` frame, but an alias never appears there
        // under its own spelling, so the request is remembered as stated.
        if let Some(model) = &launch.model {
            *self.model.lock().expect("model mutex") = Some(model.clone());
        }
        // Remember the session's own workspace cwd for stored-history
        // lookup. Only a stated cwd counts: resume/fork launches carry
        // none, and an unknown cwd stays unknown (honest unavailable)
        // rather than guessed.
        if let Some(cwd) = &launch.cwd {
            *self.workspace.lock().expect("workspace mutex") = Some(PathBuf::from(cwd));
        }
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

    /// Claim `session_id` without spawning: the offline stand-in for the
    /// `spawn_launch` claim, so tests can drive session-scoped commands
    /// (notably `SelectModel`) against a folded fixture transcript. Never
    /// spawns — the program name in tests does not exist on purpose.
    #[cfg(test)]
    fn attach_session_for_tests(&self, session_id: &str) {
        *self.session_id.lock().expect("session mutex") = Some(session_id.to_owned());
    }

    /// Answer one pending `can_use_tool` — or unknown-subtype — request
    /// with an explicit human decision. The pending lookup comes first: an
    /// unknown approval id is rejected even with no child running, and no
    /// path here invents an allow — without a live child the decision
    /// cannot be delivered, so the request goes back on its queue rather
    /// than being answered.
    fn decide_approval(
        &self,
        session_id: &str,
        approval: &str,
        choice: &str,
        feedback: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        if !self.has_pending(approval) {
            return Err(ProviderError::Rejected {
                reason: format!("unknown approval {approval:?}: no pending request carries that id"),
            });
        }
        self.check_session(session_id)?;
        // Take-then-send: the removal is what claims the right to answer,
        // so two concurrent decisions for one id cannot both pass the
        // "is it pending" check — the loser finds nothing and fails
        // cleanly instead of writing a second `control_response`.
        let claimed = self.claim_approval(approval, choice, feedback)?;
        let delivered = match self.child.lock().expect("child mutex").as_mut() {
            Some(running) => running.send_line(claimed.line()).map_err(|error| {
                ProviderError::Unavailable {
                    reason: format!("the session child is unreachable: {error}"),
                }
            }),
            None => Err(ProviderError::Unavailable {
                reason: "no session child is running".into(),
            }),
        };
        match delivered {
            Ok(()) => Ok(Ack::Accepted),
            Err(error) => {
                // Undeliverable, so unanswered: put the claim back rather
                // than dropping a decision the child never saw.
                let mut fold = self.fold.lock().expect("fold mutex");
                match claimed {
                    ClaimedApproval::Known(request, _) => fold.requeue_approval(request),
                    ClaimedApproval::Unknown(request, _) => fold.requeue_unknown(request),
                }
                Err(error)
            }
        }
    }

    /// Whether any queue holds `approval`: known or unknown-subtype.
    fn has_pending(&self, approval: &str) -> bool {
        let fold = self.fold.lock().expect("fold mutex");
        fold.pending_approvals().iter().any(|queued| queued.request_id == approval)
            || fold.pending_unknown().iter().any(|queued| queued.request_id == approval)
    }

    /// Claim the single right to answer `approval` and mint its
    /// `control_response` line, in one lock hold: the removal is the
    /// claim. A second caller finds nothing pending and gets a clean
    /// rejection. Unknown ids and unknown choices are rejected without
    /// consuming any claim, so a typo never eats the request.
    fn claim_approval(
        &self,
        approval: &str,
        choice: &str,
        feedback: Option<&str>,
    ) -> Result<ClaimedApproval, ProviderError> {
        let mut fold = self.fold.lock().expect("fold mutex");
        let known = fold.pending_approvals().iter().any(|queued| queued.request_id == approval);
        let unknown = fold.pending_unknown().iter().any(|queued| queued.request_id == approval);
        if !known && !unknown {
            return Err(ProviderError::Rejected {
                reason: format!("unknown approval {approval:?}: no pending request carries that id"),
            });
        }
        if choice != "allow" && choice != "deny" {
            return Err(ProviderError::Rejected {
                reason: format!(
                    "unknown approval choice {choice:?} for {approval}: offer \"allow\" or \"deny\""
                ),
            });
        }
        if known {
            let request = fold.take_approval(approval).expect("presence checked above");
            let line = fold::decide_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Known(request, line))
        } else {
            let request = fold.take_unknown(approval).expect("presence checked above");
            let line = fold::decide_unknown_approval(&request, choice, feedback)?;
            Ok(ClaimedApproval::Unknown(request, line))
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
        // The doc §3 rule: resolve the session's own workspace cwd, slug
        // it, look in that one directory. No workspace on record, an
        // unresolvable cwd, an absent slug directory, or a missing file
        // all degrade to None — the callers answer honest unavailable.
        // Never scan other directories: that would serve one workspace's
        // transcript inside another.
        let workspace = self.workspace.lock().expect("workspace mutex").clone()?;
        let home = self.home()?;
        history::stored_transcript_path(&workspace, session_id, Some(&home))
            .filter(|candidate| candidate.is_file())
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
            Command::SelectModel { session_id, model, .. } => {
                self.check_session(&session_id)?;
                // Admission, like Codex: the running turn keeps its model
                // and the recorded one is the session's effective model
                // from here on — the chip, the next footers, and the next
                // resume's `--model`. The live child keeps its spawn-time
                // flag until that reopen; there is no per-turn model
                // channel over stream-json stdin, and spelling one would
                // be the lie this seam exists to prevent. Applies to a
                // session with turns in it exactly as to a fresh one:
                // nothing here counts turns.
                *self.model.lock().expect("model mutex") = Some(model.clone());
                self.fold.lock().expect("fold mutex").set_model(&model);
                Ok(Ack::Accepted)
            }
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
            // The control channel answers here: a pending `can_use_tool`
            // — or unknown-subtype — request is decided with one of its
            // card's own choices (`"allow"`/`"deny"`), written to the
            // child as a `control_response`. Unseen ids and unknown
            // choices are rejected; nothing is ever auto-allowed.
            Command::DecideApproval { session_id, approval, choice, feedback, .. } => {
                self.decide_approval(&session_id, &approval, &choice, feedback.as_deref())
            }
            Command::ListPending { session_id } => {
                self.require_child()?;
                self.check_session(&session_id)?;
                let fold = self.fold.lock().expect("fold mutex");
                Ok(Ack::PendingWork {
                    approvals: fold
                        .pending_approvals()
                        .iter()
                        .map(|request| PendingApproval {
                            id: request.request_id.clone(),
                            session_id: session_id.clone(),
                            headline: frame::approval_headline(request),
                            stage_token: None,
                        })
                        .collect(),
                    questions: Vec::new(),
                })
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
                let fold = self.fold.lock().expect("fold mutex");
                let label = fold.account().label();
                // `claude --version` succeeds whether or not the CLI is
                // authenticated, so `connect()` proves nothing about auth,
                // and this crate has no `auth status` probe — nothing here
                // runs the CLI beyond spawn/version. The only auth evidence
                // this adapter ever observes is a rate-limit meter reading
                // from the live child: the CLI only emits one while making
                // authenticated API calls. So `signed_in` is true exactly
                // when a reading has been seen (label present); `false`
                // means "no login observed", never "definitely logged out".
                // A fresh or logged-out CLI reads false — fail closed, never
                // claim a login nobody checked.
                Ok(Ack::Account { signed_in: label.is_some(), label })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn meter_line() -> &'static str {
        r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1790384400,"rateLimitType":"seven_day","utilization":0.97,"isUsingOverage":true,"surpassedThreshold":0.75,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1790187000},"seven_day":{"utilization":0.97,"resetsAt":1790384400}}}}"#
    }

    #[test]
    fn read_account_is_false_until_a_meter_reading_is_seen() {
        // Fresh adapter: no meter observed, so no login claimed. A
        // hardcoded `true` fails this test.
        let adapter = ClaudeCodeAdapter::new("claude");
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        assert!(
            matches!(ack, Ack::Account { signed_in: false, label: None }),
            "no reading seen, no login claimed: {ack:?}"
        );
    }

    #[test]
    fn select_model_applies_on_a_session_with_turns() {
        // Offline throughout: the program name does not exist, so a stray
        // spawn would fail loudly, and the transcript below is
        // `basic.jsonl` folded through the shared stream path — a session
        // with a finished turn in it, not a fresh one. Model is
        // switchable; provider is not.
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let path =
            format!("{}/../../fixtures/claude-code/basic.jsonl", env!("CARGO_MANIFEST_DIR"));
        let text = std::fs::read_to_string(path).expect("fixture reads");
        {
            let mut fold = adapter.fold.lock().expect("fold mutex");
            for line in text.lines() {
                if line.trim().is_empty() {
                    continue;
                }
                crate::fold::step_line(&mut fold, line, &mut |_| {});
            }
        }
        let session =
            adapter.fold.lock().expect("fold mutex").session_id().expect("init seen").to_owned();
        adapter.attach_session_for_tests(&session);
        let ack = adapter
            .dispatch(Command::SelectModel {
                request_id: "r-1".into(),
                session_id: session.clone(),
                model: "sonnet".into(),
                model_provider: None,
            })
            .expect("a session with turns changes model");
        assert_eq!(ack, Ack::Accepted);
        assert_eq!(adapter.fold.lock().expect("fold mutex").model(), Some("sonnet"));
        assert_eq!(
            adapter.model.lock().expect("model mutex").as_deref(),
            Some("sonnet"),
            "the recorded model rides the next resume's --model"
        );
        // Another session's id is refused and changes nothing.
        let refused = adapter
            .dispatch(Command::SelectModel {
                request_id: "r-2".into(),
                session_id: "someone-else".into(),
                model: "opus".into(),
                model_provider: None,
            })
            .expect_err("a foreign session is refused");
        assert!(matches!(refused, ProviderError::Rejected { .. }), "got {refused:?}");
        assert_eq!(adapter.fold.lock().expect("fold mutex").model(), Some("sonnet"));
    }

    #[test]
    fn read_account_is_true_after_a_meter_reading() {
        // A rate-limit reading from the live child is authenticated API
        // traffic observed first-hand: signed in, with the meter label.
        let adapter = ClaudeCodeAdapter::new("claude");
        let frame = frame::decode_line(meter_line()).expect("meter line decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let ack = adapter.dispatch(Command::ReadAccount).expect("account reads");
        match ack {
            Ack::Account { signed_in: true, label: Some(label) } => {
                assert!(label.contains("97%"), "live meter label: {label}");
            }
            other => panic!("expected signed-in account with a label, got {other:?}"),
        }
    }

    fn race_adapter_with_pending() -> std::sync::Arc<ClaudeCodeAdapter> {
        // Pure decode-and-fold: no `claude` process is ever spawned (the
        // binary name does not exist, so a stray spawn would fail loudly).
        let adapter = std::sync::Arc::new(ClaudeCodeAdapter::new("claude-must-never-spawn"));
        let frame = frame::decode_line(
            r#"{"type":"control_request","request_id":"req-race","request":{"subtype":"can_use_tool","tool_name":"Bash","tool_use_id":"toolu_1","input":{}}}"#,
        )
        .expect("decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        assert_eq!(
            adapter.fold.lock().expect("fold mutex").pending_approvals().len(),
            1,
            "one pending request to fight over"
        );
        adapter
    }

    /// THE SINGLE-DECISION TEST. Eight threads decide one id: the claim is
    /// the removal, so exactly one mints the answer line and the rest get
    /// a clean rejection — never a second `control_response` with the
    /// same `request_id`. The claim is the whole race: `decide_approval`
    /// writes exactly the line its claim minted.
    #[test]
    fn concurrent_decisions_for_one_id_claim_exactly_one_line() {
        let adapter = race_adapter_with_pending();
        let mut handles = Vec::new();
        for _ in 0..8 {
            let adapter = std::sync::Arc::clone(&adapter);
            handles.push(std::thread::spawn(move || {
                adapter.claim_approval("req-race", "deny", None).map(|claimed| claimed.line().to_owned())
            }));
        }
        let mut lines = Vec::new();
        let mut rejected = 0;
        for handle in handles {
            match handle.join().expect("thread joins") {
                Ok(line) => lines.push(line),
                Err(error) => {
                    assert!(
                        matches!(error, ProviderError::Rejected { .. }),
                        "losers get a clean rejection, never a second line: {error}"
                    );
                    rejected += 1;
                }
            }
        }
        assert_eq!(lines.len(), 1, "exactly one decision claims the right to answer");
        assert_eq!(rejected, 7);
        let written: serde_json::Value = serde_json::from_str(&lines[0]).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(serde_json::Value::as_str),
            Some("req-race")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("deny")
        );
        assert!(adapter.fold.lock().expect("fold mutex").pending_approvals().is_empty());
        // …and a retry afterwards still finds nothing: claimed exactly once.
        let error =
            adapter.claim_approval("req-race", "deny", None).expect_err("nothing left to claim");
        assert!(matches!(error, ProviderError::Rejected { .. }));
    }

    /// An unknown control subtype arrives, the caller answers, a response
    /// line is minted: no received request is ever unanswerable.
    #[test]
    fn unknown_control_subtype_can_be_answered() {
        let adapter = ClaudeCodeAdapter::new("claude-must-never-spawn");
        let frame = frame::decode_line(
            r#"{"type":"control_request","request_id":"req-x","request":{"subtype":"frobnicate"}}"#,
        )
        .expect("decodes");
        adapter.fold.lock().expect("fold mutex").apply(&frame);
        let claimed =
            adapter.claim_approval("req-x", "deny", Some("no such tool")).expect("answerable");
        let written: serde_json::Value =
            serde_json::from_str(claimed.line()).expect("encodes JSON");
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("request_id"))
                .and_then(serde_json::Value::as_str),
            Some("req-x")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("behavior"))
                .and_then(serde_json::Value::as_str),
            Some("deny")
        );
        assert_eq!(
            written
                .get("response")
                .and_then(|response| response.get("response"))
                .and_then(|response| response.get("message"))
                .and_then(serde_json::Value::as_str),
            Some("no such tool")
        );
        let error =
            adapter.claim_approval("req-x", "deny", None).expect_err("claimed exactly once");
        assert!(matches!(error, ProviderError::Rejected { .. }));
    }
}
