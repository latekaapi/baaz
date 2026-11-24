//! `provider-runtime` — the process layer every provider adapter stands on.
//!
//! Claude Code, Codex and Muse each run a child process and speak to it
//! over stdio. They used to carry a private copy of each piece below, and
//! the copies drifted: one reaped a dead child while holding a lock, one
//! ended its stream at the first invalid UTF-8 byte, one was never killed
//! at quit (docs/81 B PM-2, PM-5…PM-9, PM-12). One copy now:
//!
//! - [`ChildProcess`] spawns in a new session through [`session_spawn`]
//!   (never `pre_exec`), kills the whole group — SIGTERM, SIGKILL after a
//!   bound, then reap — and records itself in the pid [`ledger`] that the
//!   next launch sweeps;
//! - [`LineReader`] reads lines lossily with an 8 MB cap;
//! - [`StdinWriter`] owns a child's stdin on one thread;
//! - [`StderrTail`] keeps the child's last stderr lines;
//! - [`RequestMap`] holds requests in flight, each with its own deadline;
//! - [`pump`] runs a stdout reader with every line under [`guard`];
//! - [`RespawnBackoff`] paces respawns of a crashing shared child.
//!
//! # Why a crate of its own
//!
//! `provider_muse::client` is a wire crate kept free of `aui`, and `provider` (which
//! depends on `aui-protocol`) must never depend on a wire crate. The
//! runtime sits beneath both: OS and channels only, so the wire crate and
//! the seam share one copy without either taking the other's
//! dependencies. `provider` re-exports it as `provider::runtime`.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod backoff;
pub mod guard;
pub mod ledger;
mod lines;
mod process;
pub mod pump;
mod requests;
#[cfg(target_os = "macos")]
pub mod session_spawn;
pub mod stderr_tail;
mod stdin;

pub use backoff::{Respawn, RespawnBackoff};
pub use lines::{Line, LineFault, LineReader, LINE_CAP};
pub use process::{end_groups, foreground_group, start_time, ChildProcess, Exit, Identity, Spawned, TERM_GRACE};
pub use requests::{RequestMap, Unanswered, Waiter};
pub use stderr_tail::{StderrTail, STDERR_TAIL_LINES};
pub use stdin::StdinWriter;
