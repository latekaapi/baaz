//! `muse-adapter` — folds Muse (MSP) view events into `aui_protocol`.
//!
//! [`MuseFold`] is the whole crate: MSP events in, [`aui_protocol::Delta`]s out,
//! plus a [`SideState`] per session for the facts the `Delta` enum cannot carry
//! (context pressure, the queue strip, the goal, the retry schedule, the
//! pending approvals and questions).
//!
//! It is pure — no I/O, no clock, no randomness — so replaying a wire capture
//! always produces the same session. That is what the fixture tests in
//! `tests/` assert, one snapshot per `fixtures/msp/*.jsonl`.
//!
//! # How MSP's shape becomes the library's
//!
//! MSP models a turn as a container of *items*, including the person's own
//! message; `aui_protocol` models the person's message as its own [`aui_protocol::Turn::User`]
//! and the reply as a [`aui_protocol::Turn::Assistant`] of blocks. So one MSP `turnId` folds
//! into up to two library turns:
//!
//! - the `userMessage` item becomes a `Turn::User` whose id is the **item id**;
//! - every other item becomes a block of a `Turn::Assistant` whose id is the
//!   **MSP `turnId`**, created lazily by its first block so it lands *after* the
//!   user's message in the transcript.
//!
//! A `userShell` item is the one kind outside a turn (`turnId: null`). It is
//! filed under its own `commandId`, which is also what any approval it raises
//! reports as its `turnId` — so the shell card and its approval card land in the
//! same turn. That identity is a capture finding, not something the schema says;
//! see `docs/01-transport.md`.
//!
//! Unknown item kinds take the rendering MSP mandates for them: a
//! [`aui_protocol::Block::Generic`] card carrying kind, status and
//! `fallbackText`. In 1.0.3 only `workflow` reaches it: `reminderChild` is
//! dropped by the fold, and `reasoning` falls back to its raw text when it
//! carries no summary (presentation policy, `docs/01-transport.md`).

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

pub mod failure;
mod fold;
pub mod mirror;
mod side;
mod slots;

pub use failure::{humanize, Failure};
pub use fold::{permission_mode, permission_profile, MuseFold, GAP_ABORT_PREFIX, SESSION_CACHE_LIMIT};
pub use side::{QueuedTurn, SideState};
pub use mirror::{neutral_side, Mirror};

/// MSP's per-stage approval race guard as the seam's opaque stage token
/// ([`provider::Command::DecideApproval`]): the guard serialized. Only Muse
/// code encodes and decodes it; the neutral side never looks inside.
pub fn stage_token(requirement: &muse_client::schema::ApprovalRequirementRef) -> String {
    serde_json::to_string(requirement).expect("stage token serializes")
}

/// Decode a [`stage_token`] back to MSP's guard; `None` for a token this
/// code did not mint.
pub fn parse_stage_token(token: &str) -> Option<muse_client::schema::ApprovalRequirementRef> {
    serde_json::from_str(token).ok()
}
