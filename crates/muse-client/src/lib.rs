//! `muse-client` — the transport for Meta's Muse Code agent.
//!
//! One `muse serve` child, spoken to over **JSON-RPC 2.0 as newline-delimited
//! JSON on stdio**. This crate frames, routes and surfaces; it holds no policy
//! and no opinion about what the events mean. Interpretation is
//! `muse-adapter`'s job, and nothing here depends on `gpui` or `aui`.
//!
//! # Shape
//!
//! - [`frame`] classifies one line into a [`Frame`]. It is pure, so every wire
//!   capture in `fixtures/msp/*.jsonl` can be replayed through it in a test.
//! - [`schema`] is the typed surface: a Rust type for every params, result and
//!   notification payload in `msp.d.ts`, with camelCase serde and open enums
//!   that tolerate strings this build has never heard of.
//! - [`MuseClient`] owns the child and two I/O threads, so the UI thread never
//!   blocks on a pipe. Requests go out on the writer thread and settle on the
//!   reader thread; everything else arrives as a [`MuseEvent`].
//!
//! # The five rules the probes taught us
//!
//! 1. **Ids are two spaces.** Our request ids are a monotonic `i64`. The server
//!    has its own, and its `approval/request` / `userInput/request` frames use
//!    it — so a frame is classified on *shape* (`id` **and** `method` ⇒ a
//!    server request), never on the id.
//! 2. **Never answer a server request with a result.** Settle it with
//!    `approval/decide` or `userInput/answer|cancel|clarify`, and de-duplicate
//!    it against the sibling `…/requested` notification on `approvalId` /
//!    `userInputId`.
//! 3. **Ack ≠ outcome, and ack ≠ first.** A `status: "accepted"` means admitted.
//!    View events for a command can and do arrive *before* its response, so
//!    nothing may gate folding on an ack.
//! 4. **`commandId` is a client-minted UUIDv7** — see [`new_command_id`]. The
//!    server rejects a v4.
//! 5. **`view/gap` means the transcript has a hole.** The client fills it with
//!    the sanctioned splice: park live events for that session, `view/page` the
//!    range forward, emit the page, release the parked events, drop the overlap.
//!
//! # Reconnecting
//!
//! When the child exits the reader emits [`MuseEvent::Closed`] and fails every
//! in-flight request. The app respawns, calls [`MuseClient::initialize`] again,
//! and `session/resume`s each open session with the **last observed
//! `viewCursor`**, which serves `history.mode: "none"` and streams only the
//! suffix.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod client;
mod error;

pub mod frame;
pub mod schema;

pub use client::{new_command_id, MuseClient, MuseConfig, MuseEvent, SchemaWarning};
pub use error::{MuseError, Result};
pub use frame::Frame;
