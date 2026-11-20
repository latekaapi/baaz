//! The session lifecycle, split along its seams: listing ([`list`]),
//! opening and parking ([`open`]), provider-lane opens ([`provider_open`]),
//! the replace state machine ([`switch`]), the session-event sink
//! ([`events`]), handoffs ([`handoff`]), a session's worktree
//! ([`worktree`]), Undo's confirm and toast ([`undo`]) and the scripted
//! run's glue ([`steps`]).
//!
//! Every file is part of [`Harness`]; see [`crate::app`] for what the
//! entity owns.

use super::*;

mod boot;
mod list;
mod open;
mod provider_open;
mod switch;
mod events;
mod handoff;
mod worktree;
mod undo;
#[cfg(feature = "probe")]
mod steps;

use provider_open::*;
pub(crate) use baaz_core::lifecycle::HandoffState;
/// The open/switch/replace state machine ([`baaz_core::lifecycle`]) over
/// this app's drafts.
pub(crate) type OpenState = baaz_core::lifecycle::OpenState<crate::session::Draft>;

/// Named from outside the app module: the parked-MRU rule the registry
/// applies and the step drain the login script shares.
pub(crate) use open::evict_parked;
#[cfg(feature = "probe")]
pub(crate) use steps::drain_steps;

#[cfg(test)]
pub(in crate::app) mod tests;
