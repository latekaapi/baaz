//! What a provider declares it can do, before the app asks.
//!
//! [`Capability`] names the thing, [`CapabilityState`] says how it stands,
//! and [`CapabilitySet`] is the whole declaration — what
//! [`crate::ProviderAdapter::capabilities`] returns. The UI reads the set to
//! decide which buttons to offer; the enforced [`crate::Provider::send`]
//! reads it to refuse what is [`CapabilityState::Unavailable`] before any
//! adapter code runs.

use crate::Command;

/// The things a provider may or may not do, coarser than [`Command`].
///
/// Derived from the command surface but grouped where the UI asks one
/// question for several commands (interrupt/cancel/reclaim are one stop
/// story; the question commands are one settle-the-prompt story). The seven
/// the planned providers genuinely differ on — forking a session, steering
/// a running turn, reasoning traces, sub-agent turns, a session-scoped
/// shell, client-side tools, compacting — each stand alone, so a future
/// provider can differ on exactly one of them.
///
/// [`Capability::ClientTools`], [`Capability::ReasoningTraces`] and
/// [`Capability::SubagentTurns`] have no commanding command: they describe
/// the handshake and the transcript, not a button that sends. No command
/// maps to them, so the refusal gate never fires for them — they exist so
/// the UI can ask before it acts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Capability {
    /// Open, resume, list, and read sessions.
    SessionLifecycle,
    /// Branch a session into a new one.
    ForkSession,
    /// Compact a session's context.
    CompactSession,
    /// Switch a session's model or approval mode.
    SessionConfig,
    /// Run a shell command inside the session, outside any turn.
    SessionShell,
    /// Submit input: start a turn, or queue behind the running one.
    SubmitTurn,
    /// Inject input into the running turn.
    SteerTurn,
    /// Stop the foreground turn, cancel it, or reclaim a queued submission.
    TurnControl,
    /// List the model catalog.
    ModelCatalog,
    /// Decide approvals and pull the pending set.
    Approvals,
    /// Answer, dismiss, or clarify a raised question.
    Questions,
    /// Page, follow, and unfollow the transcript; fetch stored output.
    Transcript,
    /// Read the account, run the login flow, log out.
    Account,
    /// Serve client-side tools to the agent (wire `sessionMcp`).
    ClientTools,
    /// Stream reasoning blocks into the transcript.
    ReasoningTraces,
    /// Render turns owned by a sub-agent.
    SubagentTurns,
}

impl Capability {
    /// How many capabilities exist. Sizes [`CapabilitySet`]'s table, so a
    /// new variant breaks every declaration site until it is declared —
    /// there is no silent default to fall into.
    pub const COUNT: usize = 16;

    /// Every capability, in declaration order. The order is the table index,
    /// so keep it in sync with the enum order.
    pub fn all() -> [Capability; Capability::COUNT] {
        [
            Capability::SessionLifecycle,
            Capability::ForkSession,
            Capability::CompactSession,
            Capability::SessionConfig,
            Capability::SessionShell,
            Capability::SubmitTurn,
            Capability::SteerTurn,
            Capability::TurnControl,
            Capability::ModelCatalog,
            Capability::Approvals,
            Capability::Questions,
            Capability::Transcript,
            Capability::Account,
            Capability::ClientTools,
            Capability::ReasoningTraces,
            Capability::SubagentTurns,
        ]
    }

    /// The kebab-case name, for logs and refusal-adjacent UI. Distinct from
    /// [`Command::capability`], which names the refused command: the gate
    /// keys on [`Command::required_capability`], the refusal names the
    /// command that was asked for.
    pub fn name(&self) -> &'static str {
        match self {
            Capability::SessionLifecycle => "session-lifecycle",
            Capability::ForkSession => "fork-session",
            Capability::CompactSession => "compact-session",
            Capability::SessionConfig => "session-config",
            Capability::SessionShell => "session-shell",
            Capability::SubmitTurn => "submit-turn",
            Capability::SteerTurn => "steer-turn",
            Capability::TurnControl => "turn-control",
            Capability::ModelCatalog => "model-catalog",
            Capability::Approvals => "approvals",
            Capability::Questions => "questions",
            Capability::Transcript => "transcript",
            Capability::Account => "account",
            Capability::ClientTools => "client-tools",
            Capability::ReasoningTraces => "reasoning-traces",
            Capability::SubagentTurns => "subagent-turns",
        }
    }
}

/// How one capability stands: four states, and the rule beside them.
///
/// - `Native` — the provider does this itself.
/// - `Emulated` — Baaz does it on the provider's behalf, and the result is
///   not identical to native. A person is entitled to know which they are
///   getting, so the state carries what differs.
/// - `Unavailable` — this provider cannot do it, by design or by version.
///   Carries the human reason: without one, whoever renders the missing
///   button cannot say why it is gone. A command whose capability is
///   `Unavailable` never returns `Ok` — see [`crate::Provider::send`],
///   whose gate enforces this before any adapter code runs.
/// - `Unverified` — nobody has checked. Honest ignorance: it is attempted,
///   never refused. Not a synonym for `Unavailable`, and it must never be
///   quietly collapsed into one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CapabilityState {
    /// The provider does this itself.
    Native,
    /// Baaz does it on the provider's behalf; the result differs from native.
    Emulated {
        /// What Baaz does instead, and how the result differs.
        reason: String,
    },
    /// This provider cannot do it. Commands needing it are refused.
    Unavailable {
        /// Why not — by design or by version.
        reason: String,
    },
    /// Nobody has checked. Attempted, never refused.
    Unverified,
}

impl CapabilityState {
    /// The human reason, when the state carries one. `Native` and
    /// `Unverified` carry none; `Emulated` and `Unavailable` always do — a
    /// refusal or a badge without words is useless to whoever renders it.
    /// `Unavailable` and `Emulated` take the reason as a constructor field
    /// precisely so the reasonless form is unrepresentable.
    pub fn reason(&self) -> Option<&str> {
        match self {
            CapabilityState::Native | CapabilityState::Unverified => None,
            CapabilityState::Emulated { reason } | CapabilityState::Unavailable { reason } => {
                Some(reason)
            }
        }
    }

    /// Whether [`crate::Provider::send`] attempts the command.
    /// Everything but `Unavailable` proceeds — in particular `Unverified`
    /// is attempted, never refused.
    pub fn allows_attempt(&self) -> bool {
        !matches!(self, CapabilityState::Unavailable { .. })
    }
}

/// What [`crate::ProviderAdapter::capabilities`] returns: one state per
/// [`Capability`], no silent gaps.
///
/// Built from exactly [`Capability::COUNT`] entries. A missing capability
/// is a loud [`new`](Self::new) panic, not a quiet default, and a new
/// variant changes `COUNT`, which breaks every declaration site at compile
/// time. There is deliberately no `Default` and no empty set. Both
/// directions fail loudly: forget to declare, and the build or the startup
/// tells you.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapabilitySet {
    states: [CapabilityState; Capability::COUNT],
}

impl CapabilitySet {
    /// Declare every capability's state. Panics on a missing capability or
    /// a duplicate entry — both are declaration bugs, and a panic where the
    /// set is built beats a button that silently lies.
    pub fn new(entries: [(Capability, CapabilityState); Capability::COUNT]) -> Self {
        let mut slots: [Option<CapabilityState>; Capability::COUNT] =
            std::array::from_fn(|_| None);
        for (capability, state) in entries {
            let slot = &mut slots[capability as usize];
            assert!(
                slot.is_none(),
                "duplicate declaration for capability `{}`",
                capability.name()
            );
            *slot = Some(state);
        }
        let mut missing = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            if slot.is_none() {
                missing.push(Capability::all()[index].name());
            }
        }
        assert!(missing.is_empty(), "capability set is missing: {}", missing.join(", "));
        Self { states: slots.map(|slot| slot.expect("checked present above")) }
    }

    /// The declared state. Always present by construction — there is no
    /// missing case to soften.
    pub fn state(&self, capability: Capability) -> &CapabilityState {
        &self.states[capability as usize]
    }
}

impl Command {
    /// The capability this command needs. Coarse on purpose: several
    /// commands share one UI question, and the seven the planned providers
    /// differ on each have their own. The enforced [`crate::Provider::send`]
    /// refuses with [`crate::ProviderError::Unsupported`] when this
    /// capability's state is [`CapabilityState::Unavailable`]; the refusal
    /// itself names the command via [`Command::capability`]. The lookup
    /// lives in the wrapper, not in any adapter.
    pub fn required_capability(&self) -> Capability {
        match self {
            Command::OpenSession { .. }
            | Command::ResumeSession { .. }
            | Command::ListSessions { .. }
            | Command::ReadSession { .. } => Capability::SessionLifecycle,
            Command::ForkSession { .. } => Capability::ForkSession,
            Command::CompactSession { .. } => Capability::CompactSession,
            Command::SelectModel { .. } | Command::SelectApprovalMode { .. } => {
                Capability::SessionConfig
            }
            Command::RunShell { .. } => Capability::SessionShell,
            Command::SubmitInput { .. } => Capability::SubmitTurn,
            Command::SteerInput { .. } => Capability::SteerTurn,
            Command::InterruptTurn { .. } | Command::CancelTurn { .. } | Command::ReclaimQueued { .. } => {
                Capability::TurnControl
            }
            Command::ListModels { .. } => Capability::ModelCatalog,
            Command::DecideApproval { .. } | Command::ListPending { .. } => Capability::Approvals,
            Command::AnswerQuestion { .. }
            | Command::DismissQuestion { .. }
            | Command::ClarifyQuestion { .. } => Capability::Questions,
            Command::PageTranscript { .. }
            | Command::FollowSession { .. }
            | Command::UnfollowSession { .. }
            | Command::ReadStoredOutput { .. } => Capability::Transcript,
            Command::ReadAccount
            | Command::BeginLogin { .. }
            | Command::CancelLogin
            | Command::LogOut => Capability::Account,
        }
    }
}
