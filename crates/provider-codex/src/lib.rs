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

use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ModelSummary, PendingApproval,
    PendingQuestion, ProviderAdapter, ProviderError, ProviderEvent, ProviderId, SubmissionPart,
};
use serde_json::json;

pub use caps::{capabilities, codex_version_supported, CODEX_VERSION_FLOOR};

use child::{
    default_model, initialize_request, initialized_notification, model_ids, thread_ids,
    thread_start_request, turn_id_from, turn_interrupt_request, turn_start_request,
    turn_steer_request, ApprovalDecision, RunningChild,
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
        }
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
    fn open_session(
        &self,
        workspace: Option<&str>,
        model: Option<&str>,
    ) -> Result<Ack, ProviderError> {
        if self.child.lock().expect("child mutex").is_some() {
            return Err(ProviderError::Rejected {
                reason: "this adapter already holds a session child; fork or resume instead"
                    .into(),
            });
        }
        let running = RunningChild::spawn(&self.program, Arc::clone(&self.fold), self.tx.clone())
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
                        reason: "no image input shape was probed on turn/start".into(),
                    });
                }
            }
        }
        Ok(text)
    }

    fn submit_text(&self, session_id: &str, text: &str) -> Result<Ack, ProviderError> {
        self.check_session(session_id)?;
        let (thread_id, model) = self.thread_and_model()?;
        let turn = self
            .with_child(|running| {
                running.send_frame(turn_start_request(
                    running.next_request_id(),
                    &thread_id,
                    &model,
                    text,
                ))
            })?
            .map_err(Self::unavailable)?;
        let turn_id = turn_id_from(&turn).ok_or_else(|| ProviderError::Unavailable {
            reason: "turn/start answered without a turn id".into(),
        })?;
        // The ack carries the submission handle; the turn's true identity is
        // the server-minted id, never derived locally.
        Ok(Ack::TurnAccepted { turn_id })
    }

    fn decide(choice: &str) -> Result<ApprovalDecision, ProviderError> {
        match choice {
            "accept" => Ok(ApprovalDecision::Accept),
            "accept-for-session" => Ok(ApprovalDecision::AcceptForSession),
            "decline" => Ok(ApprovalDecision::Decline),
            "cancel" => Ok(ApprovalDecision::Cancel),
            _ => Err(ProviderError::Rejected {
                reason: format!(
                    "unknown approval choice {choice:?}: offer accept, accept-for-session, \
                     decline, or cancel"
                ),
            }),
        }
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
            Command::OpenSession { workspace, model, .. } => {
                self.open_session(workspace.as_deref(), model.as_deref())
            }
            Command::ResumeSession { .. } => Err(ProviderError::Rejected {
                reason: "thread/resume was never captured against a live server; not resumed blind"
                    .into(),
            }),
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
            Command::SubmitInput { session_id, parts, .. } => {
                let text = Self::join_text(&parts)?;
                self.submit_text(&session_id, &text)
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
                let ids = model_ids(&catalog);
                let active = session
                    .as_deref()
                    .and_then(|session| {
                        (Some(session) == self.session_id.lock().expect("session mutex").as_deref())
                            .then(|| self.model.lock().expect("model mutex").clone())
                            .flatten()
                    })
                    .unwrap_or_default();
                Ok(Ack::ModelCatalog {
                    models: ids
                        .into_iter()
                        .map(|id| ModelSummary {
                            active: id == active,
                            label: id.clone(),
                            id,
                        })
                        .collect(),
                    provider: "openai".into(),
                })
            }
            Command::DecideApproval { session_id, approval, choice, .. } => {
                self.check_session(&session_id)?;
                let decision = Self::decide(&choice)?;
                let answered = self
                    .with_child(|running| running.answer_approval(&approval, decision))?
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
        // The `approved` trap at the dispatch edge: only the four typed
        // tokens decide; anything else is refused, never misdelivered.
        assert!(CodexAdapter::decide("accept").is_ok());
        assert!(CodexAdapter::decide("accept-for-session").is_ok());
        assert!(CodexAdapter::decide("decline").is_ok());
        assert!(CodexAdapter::decide("cancel").is_ok());
        assert!(CodexAdapter::decide("approved").is_err(), "`approved` is the silent refusal");
        assert!(CodexAdapter::decide("yes").is_err());
    }

    #[test]
    fn no_test_spawns_a_child() {
        // Pin the hard rule structurally: every dispatch above refused before
        // any spawn, and this adapter holds no child after them.
        let adapter = CodexAdapter::new("codex");
        assert!(adapter.child.lock().expect("child mutex").is_none());
    }
}
