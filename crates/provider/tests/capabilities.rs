//! The check is the deliverable: an `Unavailable` capability refuses
//! through the enforced path even when the adapter would have said `Ok`.
//!
//! `ForgetfulAdapter` is the adapter that forgot — its inner dispatch
//! returns `Ok` for everything. Every test below calls the trait's `send`
//! (the provided method), never `send_inner` directly, so they prove the
//! guarantee itself rather than one adapter's good behaviour. `Unverified`
//! is attempted, never refused.

use std::collections::BTreeSet;

use crossbeam_channel::{unbounded, Receiver};
use provider::{
    Ack, Capability, CapabilitySet, CapabilityState, Command, ConnectInfo, Handshake,
    ProviderAdapter, ProviderError, ProviderEvent, ProviderId, SubmissionPart,
};

/// An adapter that forgot to refuse: every inner dispatch succeeds. If the
/// gate ever stops working, this adapter's `Ok` leaks through and the
/// refusal test below fails — which is exactly the mutation check in
/// `docs/17-providers.md`, automated.
struct ForgetfulAdapter {
    rx: Receiver<ProviderEvent>,
}

impl ForgetfulAdapter {
    fn new() -> Self {
        let (_tx, rx) = unbounded();
        Self { rx }
    }

    /// `ForkSession` is `Unavailable` with a marker reason, `SteerTurn` is
    /// `Unverified`, everything else is `Native`.
    fn declared() -> CapabilitySet {
        CapabilitySet::new(Capability::all().map(|capability| {
            let state = match capability {
                Capability::ForkSession => {
                    CapabilityState::Unavailable { reason: "no fork here".into() }
                }
                Capability::SteerTurn => CapabilityState::Unverified,
                _ => CapabilityState::Native,
            };
            (capability, state)
        }))
    }

    fn fork() -> Command {
        Command::ForkSession {
            request_id: "r".into(),
            session_id: "s".into(),
            through_turn: None,
            metadata_only: false,
        }
    }

    fn steer() -> Command {
        Command::SteerInput {
            request_id: "r".into(),
            session_id: "s".into(),
            expected_turn: "t".into(),
            parts: vec![SubmissionPart::Text("x".into())],
        }
    }
}

impl ProviderAdapter for ForgetfulAdapter {
    fn id(&self) -> ProviderId {
        aui_protocol::Provider::Codex
    }

    fn connect(&mut self, _client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        Ok(Handshake {
            provider: aui_protocol::Provider::Codex,
            agent_name: "forgetful".into(),
            agent_version: "0.0.0".into(),
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        Self::declared()
    }

    fn send_inner(&self, _command: Command) -> Result<Ack, ProviderError> {
        Ok(Ack::Accepted)
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    fn shutdown(&mut self) {}
}

#[test]
fn unavailable_is_refused_through_the_enforced_path_even_when_the_adapter_would_say_ok() {
    let adapter = ForgetfulAdapter::new();
    // NB: `send`, not `send_inner` — the enforced path. The adapter above
    // would answer `Ok`; the gate must refuse before it gets the chance.
    match adapter.send(ForgetfulAdapter::fork()) {
        Err(ProviderError::Unsupported { capability, reason }) => {
            assert_eq!(capability, "fork-session", "the refusal must name the command");
            assert!(reason.contains("no fork here"), "the refusal must carry the reason");
        }
        other => panic!("an Unavailable capability must never return Ok, got {other:?}"),
    }
}

#[test]
fn unverified_is_attempted_not_refused() {
    let adapter = ForgetfulAdapter::new();
    let ack = adapter.send(ForgetfulAdapter::steer()).expect("Unverified must be attempted");
    assert_eq!(ack, Ack::Accepted, "the attempt must reach the adapter");
}

#[test]
fn native_is_attempted() {
    let adapter = ForgetfulAdapter::new();
    let ack = adapter
        .send(Command::SubmitInput {
            request_id: "r".into(),
            session_id: "s".into(),
            parts: vec![SubmissionPart::Text("hi".into())],
            display_text: None,
        })
        .expect("Native must be attempted");
    assert_eq!(ack, Ack::Accepted, "the attempt must reach the adapter");
}

#[test]
fn capability_catalog_has_no_silent_gaps() {
    let all = Capability::all();
    assert_eq!(all.len(), Capability::COUNT, "all() must cover every capability");
    let mut seen = BTreeSet::new();
    let mut names = BTreeSet::new();
    for capability in all {
        assert!(seen.insert(capability), "duplicate capability {capability:?}");
        assert!(
            names.insert(capability.name()),
            "duplicate capability name `{}`",
            capability.name()
        );
    }
}

#[test]
fn states_carry_reasons_where_one_exists() {
    assert_eq!(CapabilityState::Native.reason(), None);
    assert_eq!(CapabilityState::Unverified.reason(), None);
    assert_eq!(
        CapabilityState::Emulated { reason: "differs".into() }.reason(),
        Some("differs")
    );
    assert_eq!(
        CapabilityState::Unavailable { reason: "cannot".into() }.reason(),
        Some("cannot")
    );
    assert!(CapabilityState::Native.allows_attempt());
    assert!(CapabilityState::Emulated { reason: "x".into() }.allows_attempt());
    assert!(CapabilityState::Unverified.allows_attempt(), "Unverified is attempted, never refused");
    assert!(!CapabilityState::Unavailable { reason: "x".into() }.allows_attempt());
}
