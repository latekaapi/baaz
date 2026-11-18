//! The session lifecycle, split along its seams: listing ([`list`]),
//! opening and parking ([`open`]), provider-lane opens ([`provider_open`]),
//! the replace state machine ([`switch`]), the session-event sink
//! ([`events`]), handoffs ([`handoff`]) and the scripted run's glue
//! ([`steps`]).
//!
//! Every file is part of [`Harness`]; see [`crate::app`] for what the
//! entity owns.

use super::*;

mod list;
mod open;
mod provider_open;
mod switch;
mod events;
mod handoff;
mod steps;
mod state;

use provider_open::*;
pub(crate) use state::{HandoffState, OpenState};

/// Named from outside the app module: the parked-MRU rule the registry
/// applies and the step drain the login script shares.
pub(crate) use open::evict_parked;
pub(crate) use steps::drain_steps;

#[cfg(test)]
pub(in crate::app) mod tests;
