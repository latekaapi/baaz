//! The second implementation, shared — and it is not optional.
//!
//! [`ScriptedProvider`] is the honest stand-in for "a provider that is not
//! muse": it implements [`ProviderAdapter`](crate::ProviderAdapter) from a
//! list of canned deltas, knows how to do almost nothing, and answers
//! everything else with the typed refusal. Almost nothing is: open a
//! session, take input, follow it, and take the session's settings (model,
//! approval mode, plan mode) as asked — the settings because the app's
//! offline lanes run on this double under every provider's descriptor, and
//! a control the descriptor offers must not come back refused there (the
//! `composer-plan` capture banners "Not available" on Claude Code's Plan,
//! docs/82 5.9). [`ScriptedProvider::refusing`] takes a capability away
//! again, for the tests of a refusal. If writing it ever needs
//! something from a wire crate, the trait is wrong — it does not, and this
//! module proves it by using nothing but `provider` and `aui-protocol`.
//!
//! This lives in the library (not in a test file) so seam consumers —
//! `crates/baaz-core/src/conn.rs`'s connection-path test today — can drive the
//! same double instead of each inventing their own.

use std::sync::{Arc, Mutex};

use aui_protocol::{Block, Delta, Turn, TurnMeta, Provider as Backend};
use crossbeam_channel::{unbounded, Receiver};

use crate::{
    Ack, Capability, CapabilitySet, CapabilityState, Command, ConnectInfo, Handshake, ProviderAdapter,
    ProviderError, ProviderEvent, ProviderId, SubmissionPart,
};

/// A provider that is not muse: canned deltas, sessions, input and their
/// settings, refusals for everything else.
pub struct ScriptedProvider {
    inner: Arc<Inner>,
    /// Capabilities taken away ([`ScriptedProvider::refusing`]).
    refused: Vec<Capability>,
}

struct Inner {
    state: Mutex<State>,
    tx: crossbeam_channel::Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    /// The id `OpenSession` mints: `s-scripted` unless the host names
    /// another ([`ScriptedProvider::with_session_id`]).
    session_id: String,
}

struct State {
    connected: bool,
    shutdown: bool,
    next_turn: u64,
}

impl ScriptedProvider {
    /// A disconnected scripted provider. It answers as [`Backend::Codex`] —
    /// deliberately not muse — so a test failure here means the trait leans
    /// on the vendor it was first written against.
    pub fn new() -> Self {
        Self::with_session_id("s-scripted")
    }

    /// A disconnected scripted provider whose session is `session_id`: a
    /// host opening several scripted lanes at once gives each its own id.
    pub fn with_session_id(session_id: impl Into<String>) -> Self {
        let (tx, rx) = unbounded();
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State { connected: false, shutdown: false, next_turn: 0 }),
                tx,
                rx,
                session_id: session_id.into(),
            }),
            refused: Vec::new(),
        }
    }

    /// This stand-in with `capability` taken away: declared `Unavailable`,
    /// so the seam refuses its commands before they reach `dispatch` — a
    /// provider that cannot do it, for a test of how a refusal reads.
    pub fn refusing(mut self, capability: Capability) -> Self {
        self.refused.push(capability);
        self
    }

    /// `[hold <ms>] …`: how long a scripted turn stays running before it
    /// finishes — what a scripted run needs to show a busy session.
    fn hold_of(text: &str) -> Option<std::time::Duration> {
        let rest = text.trim_start().strip_prefix("[hold ")?;
        let ms: u64 = rest.split(']').next()?.trim().parse().ok()?;
        Some(std::time::Duration::from_millis(ms))
    }

    /// The canned reply to one submitted text: the person's turn, one
    /// assistant turn with one text block, and its footer.
    fn script_for(&self, session_id: &str, text: &str) -> Vec<ProviderEvent> {
        let mut state = self.inner.state.lock().expect("scripted mutex");
        state.next_turn += 1;
        let n = state.next_turn;
        drop(state);
        let user_id = format!("u-{n}");
        let turn_id = format!("a-{n}");
        vec![ProviderEvent::Deltas {
            session_id: Some(session_id.to_owned()),
            deltas: vec![
                Delta::TurnStarted {
                    turn: Turn::User {
                        id: user_id,
                        text: text.to_owned(),
                        attachments: Vec::new(),
                        mentions: Vec::new(),
                        timestamp: None,
                    },
                },
                Delta::TurnStarted {
                    turn: Turn::Assistant {
                        id: turn_id.clone(),
                        blocks: Vec::new(),
                        meta: TurnMeta::default(),
                        timestamp: None,
                    },
                },
                Delta::BlockAdded {
                    turn_id: turn_id.clone(),
                    block: Block::Text { text: format!("echo: {text}"), streaming: false },
                },
                Delta::TurnFinished {
                    turn_id,
                    meta: TurnMeta::default(),
                },
            ],
        }]
    }

    /// Send the canned reply for `text`: at once, or — for `[hold <ms>]` —
    /// the turn's start now and its end after the hold, from a thread, as a
    /// real child's later output would arrive.
    fn reply(&self, session_id: &str, text: &str) {
        let mut events = self.script_for(session_id, text);
        let Some(hold) = Self::hold_of(text) else {
            for event in events {
                let _ = self.inner.tx.send(event);
            }
            return;
        };
        let Some(ProviderEvent::Deltas { session_id: sid, deltas }) = events.pop() else { return };
        let split = deltas.iter().position(|delta| matches!(delta, Delta::BlockAdded { .. })).unwrap_or(deltas.len());
        let (start, end) = deltas.split_at(split);
        let _ = self.inner.tx.send(ProviderEvent::Deltas { session_id: sid.clone(), deltas: start.to_vec() });
        let (tx, end) = (self.inner.tx.clone(), end.to_vec());
        std::thread::spawn(move || {
            std::thread::sleep(hold);
            let _ = tx.send(ProviderEvent::Deltas { session_id: sid, deltas: end });
        });
    }
}

/// Why the stand-in refuses what it does not do.
const REFUSAL: &str = "scripted providers only open sessions, take input and their settings, and follow them";

impl Default for ScriptedProvider {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderAdapter for ScriptedProvider {
    fn id(&self) -> ProviderId {
        Backend::Codex
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        let mut state = self.inner.state.lock().expect("scripted mutex");
        if state.connected {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        state.connected = true;
        Ok(Handshake {
            provider: Backend::Codex,
            agent_name: "scripted".into(),
            agent_version: "0.0.0".into(),
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        // The implemented stories stay `Native`; everything else is
        // `Unavailable` with the reason the old hand-rolled refusal
        // carried. `Transcript` stays `Native` even though only following
        // is implemented — the gate is a floor, and the paging arms keep
        // their own typed refusal inside `dispatch`. A capability taken
        // away ([`ScriptedProvider::refusing`]) is `Unavailable` too.
        let off = || CapabilityState::Unavailable { reason: REFUSAL.into() };
        let on = |capability: Capability| if self.refused.contains(&capability) { off() } else { CapabilityState::Native };
        CapabilitySet::new([
            (Capability::SessionLifecycle, on(Capability::SessionLifecycle)),
            (Capability::ForkSession, off()),
            (Capability::CompactSession, off()),
            (Capability::SessionConfig, on(Capability::SessionConfig)),
            (Capability::SessionShell, off()),
            (Capability::SubmitTurn, on(Capability::SubmitTurn)),
            (Capability::SteerTurn, off()),
            (Capability::TurnControl, off()),
            (Capability::ModelCatalog, off()),
            (Capability::Approvals, off()),
            (Capability::Questions, off()),
            (Capability::Transcript, on(Capability::Transcript)),
            (Capability::Account, off()),
            (Capability::ClientTools, off()),
            (Capability::ReasoningTraces, off()),
            (Capability::SubagentTurns, off()),
            (Capability::ImageInput, on(Capability::ImageInput)),
        ])
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        let state = self.inner.state.lock().expect("scripted mutex");
        if !state.connected {
            return Err(ProviderError::Unavailable { reason: "not connected".into() });
        }
        if state.shutdown {
            return Err(ProviderError::Unavailable { reason: "shut down".into() });
        }
        drop(state);
        match command {
            Command::OpenSession { mode, .. } => {
                // The mode the open asked for is the mode it runs in: echo
                // it, the way a real adapter reports the mode it started.
                if let Some(mode) = mode {
                    let _ = self.inner.tx.send(ProviderEvent::ApprovalModeChanged {
                        session_id: self.inner.session_id.clone(),
                        mode,
                    });
                }
                Ok(Ack::Session { session_id: self.inner.session_id.clone(), title: None })
            }
            Command::SubmitInput { session_id, parts, .. } => {
                let text = parts
                    .iter()
                    .filter_map(|part| match part {
                        SubmissionPart::Text(text) => Some(text.clone()),
                        SubmissionPart::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                self.reply(&session_id, &text);
                Ok(Ack::TurnAccepted { turn_id: "a-latest".into(), queued: false })
            }
            Command::FollowSession { .. } => Ok(Ack::Accepted),
            // The session's settings, taken as asked: the mode and the plan
            // are echoed the way a real adapter reports what it switched
            // to, so the chip and the Plan toggle land on the event, as
            // they do live. A model pick has no event of its own; its ack
            // lands it.
            Command::SelectModel { .. } => Ok(Ack::Accepted),
            // A goal is recorded with the commands, like every send.
            Command::SetGoal { .. } => Ok(Ack::Accepted),
            Command::SelectApprovalMode { session_id, mode, .. } => {
                let _ = self.inner.tx.send(ProviderEvent::ApprovalModeChanged { session_id, mode });
                Ok(Ack::Accepted)
            }
            Command::SetPlanMode { session_id, on, .. } => {
                let _ = self.inner.tx.send(ProviderEvent::PlanModeChanged { session_id, on });
                Ok(Ack::Accepted)
            }
            other => Err(ProviderError::unsupported(other.capability(), REFUSAL)),
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.inner.rx.clone()
    }

    fn shutdown(&mut self) {
        self.inner.state.lock().expect("scripted mutex").shutdown = true;
    }
}
