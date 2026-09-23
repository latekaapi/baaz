//! The second implementation, shared — and it is not optional.
//!
//! [`ScriptedProvider`] is the honest stand-in for "a provider that is not
//! muse": it implements [`ProviderAdapter`](crate::ProviderAdapter) from a
//! list of canned deltas, knows how to do almost nothing, and answers
//! everything else with the typed refusal. If writing it ever needs
//! something from a wire crate, the trait is wrong — it does not, and this
//! module proves it by using nothing but `provider` and `aui-protocol`.
//!
//! This lives in the library (not in a test file) so seam consumers —
//! `crates/baaz/src/conn.rs`'s connection-path test today — can drive the
//! same double instead of each inventing their own.

use std::sync::{Arc, Mutex};

use aui_protocol::{Block, Delta, Turn, TurnMeta, Provider as Backend};
use crossbeam_channel::{unbounded, Receiver};

use crate::{
    Ack, Capability, CapabilitySet, CapabilityState, Command, ConnectInfo, Handshake, ProviderAdapter,
    ProviderError, ProviderEvent, ProviderId, SubmissionPart,
};

/// A provider that is not muse: canned deltas, three commands, refusals
/// for everything else.
pub struct ScriptedProvider {
    inner: Arc<Inner>,
}

struct Inner {
    state: Mutex<State>,
    tx: crossbeam_channel::Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
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
        let (tx, rx) = unbounded();
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State { connected: false, shutdown: false, next_turn: 0 }),
                tx,
                rx,
            }),
        }
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
}

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
        // The three implemented stories stay `Native`; everything else is
        // `Unavailable` with the reason the old hand-rolled refusal
        // carried. `Transcript` stays `Native` even though only following
        // is implemented — the gate is a floor, and the paging arms keep
        // their own typed refusal inside `dispatch`.
        let off = || CapabilityState::Unavailable {
            reason: "scripted providers only open sessions, take input, and follow them".into(),
        };
        CapabilitySet::new([
            (Capability::SessionLifecycle, CapabilityState::Native),
            (Capability::ForkSession, off()),
            (Capability::CompactSession, off()),
            (Capability::SessionConfig, off()),
            (Capability::SessionShell, off()),
            (Capability::SubmitTurn, CapabilityState::Native),
            (Capability::SteerTurn, off()),
            (Capability::TurnControl, off()),
            (Capability::ModelCatalog, off()),
            (Capability::Approvals, off()),
            (Capability::Questions, off()),
            (Capability::Transcript, CapabilityState::Native),
            (Capability::Account, off()),
            (Capability::ClientTools, off()),
            (Capability::ReasoningTraces, off()),
            (Capability::SubagentTurns, off()),
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
            Command::OpenSession { .. } => {
                Ok(Ack::Session { session_id: "s-scripted".into(), title: None })
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
                for event in self.script_for(&session_id, &text) {
                    let _ = self.inner.tx.send(event);
                }
                Ok(Ack::TurnAccepted { turn_id: "a-latest".into() })
            }
            Command::FollowSession { .. } => Ok(Ack::Accepted),
            other => Err(ProviderError::unsupported(
                other.capability(),
                "scripted providers only open sessions, take input, and follow them",
            )),
        }
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.inner.rx.clone()
    }

    fn shutdown(&mut self) {
        self.inner.state.lock().expect("scripted mutex").shutdown = true;
    }
}
