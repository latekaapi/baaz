//! The declared set, from the agent's own handshake (docs/81 B S-4).
//!
//! An ACP agent says what it can do in its `initialize` answer: whether it
//! re-opens stored sessions (`loadSession`) and which content a prompt may
//! carry (`promptCapabilities`). Those cells come from that answer; the rest
//! are what ACP v1 itself carries for every agent, cell for cell against
//! the hand-written fixtures in `fixtures/acp/` — no ACP agent was ever run
//! for these (docs/82 5.7: trying one live is the owner's).
//!
//! The app reads a provider's table before any lane connects (its
//! descriptor's `capabilities`), so the last handshake this process saw is
//! remembered ([`remember`]) and the descriptor answers from it; before any
//! handshake the handshake-dependent cells read `Unverified`, which offers
//! nothing (rule 13).

use std::sync::Mutex;

use provider::{Capability, CapabilitySet, CapabilityState};

use crate::wire::AgentHandshake;

/// The last handshake an agent answered in this process.
static LAST: Mutex<Option<AgentHandshake>> = Mutex::new(None);

/// Remember `handshake` as what the configured agent declared.
pub fn remember(handshake: &AgentHandshake) {
    *LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handshake.clone());
}

/// The last handshake an agent answered in this process, if any.
pub fn last_handshake() -> Option<AgentHandshake> {
    LAST.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn unavailable(reason: &str) -> CapabilityState {
    CapabilityState::Unavailable { reason: reason.to_owned() }
}

/// The capability set for an agent that answered `handshake` (`None`:
/// none has yet).
pub fn capabilities(handshake: Option<&AgentHandshake>) -> CapabilitySet {
    let image = match handshake {
        Some(handshake) if handshake.image => CapabilityState::Native,
        Some(_) => unavailable("the agent's handshake offers no image prompts"),
        None => CapabilityState::Unverified,
    };
    CapabilitySet::new([
        // `session/new` on every agent; `session/load` only where the
        // handshake offers it — the adapter refuses a resume otherwise.
        (Capability::SessionLifecycle, CapabilityState::Native),
        (Capability::ForkSession, unavailable("ACP v1 has no session fork")),
        (Capability::CompactSession, unavailable("ACP v1 has no compaction request")),
        // ACP agents name their own modes (`session/set_mode`); Baaz's
        // four approval modes do not map onto them, and v1 has no model
        // switch.
        (Capability::SessionConfig, unavailable("an ACP agent names its own modes; Baaz's approval modes do not map onto them")),
        (Capability::SessionShell, unavailable("ACP v1 has no shell outside a turn")),
        (Capability::SubmitTurn, CapabilityState::Native),
        (Capability::SteerTurn, unavailable("an ACP agent takes one prompt at a time; there is no steer")),
        // `session/cancel`, ended `cancelled` (`fixtures/acp/cancel.jsonl`).
        (Capability::TurnControl, CapabilityState::Native),
        (Capability::ModelCatalog, unavailable("ACP v1 lists no models")),
        // `session/request_permission`, allowed and denied
        // (`fixtures/acp/permission*.jsonl`).
        (Capability::Approvals, CapabilityState::Native),
        (Capability::Questions, unavailable("ACP v1 asks no questions besides permissions")),
        (
            Capability::Transcript,
            CapabilityState::Emulated {
                reason: "follows the live session; a stored one comes back whole with session/load, never paged".into(),
            },
        ),
        (Capability::Account, unavailable("the agent signs in on its own; Baaz does not drive its sign-in")),
        // Baaz's terminal relay rides `mcpServers` on every session; no
        // agent has been seen calling it.
        (Capability::ClientTools, CapabilityState::Unverified),
        // `agent_thought_chunk` folds into thinking blocks.
        (Capability::ReasoningTraces, CapabilityState::Native),
        (Capability::SubagentTurns, CapabilityState::Unverified),
        (Capability::ImageInput, image),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake(image: bool) -> AgentHandshake {
        AgentHandshake { protocol_version: 1, name: None, version: None, load_session: true, image, auth_methods: Vec::new() }
    }

    #[test]
    fn image_prompts_come_from_the_handshake() {
        assert_eq!(capabilities(None).state(Capability::ImageInput), &CapabilityState::Unverified);
        assert_eq!(capabilities(Some(&handshake(true))).state(Capability::ImageInput), &CapabilityState::Native);
        assert!(!capabilities(Some(&handshake(false))).state(Capability::ImageInput).allows_attempt());
    }

    #[test]
    fn what_acp_cannot_carry_is_never_offered() {
        let set = capabilities(Some(&handshake(true)));
        for capability in [
            Capability::ForkSession,
            Capability::CompactSession,
            Capability::SessionConfig,
            Capability::SessionShell,
            Capability::SteerTurn,
            Capability::ModelCatalog,
            Capability::Questions,
            Capability::Account,
        ] {
            assert!(!set.state(capability).allows_attempt(), "{}", capability.name());
        }
        for capability in [Capability::SubmitTurn, Capability::TurnControl, Capability::Approvals] {
            assert_eq!(set.state(capability), &CapabilityState::Native, "{}", capability.name());
        }
    }
}
