//! `provider` — the neutral vocabulary between the app and any agent.
//!
//! Adapters translate **into** this vocabulary, not the reverse: the render
//! direction ([`aui_protocol::Delta`]) was already neutral, and this crate
//! adds the missing command direction and lifecycle. Nothing here may name
//! a provider's wire — no method name, type, field, or capability string
//! carries one — so a second adapter never has to emit the first vendor's
//! shapes to be usable.
//!
//! The crate depends on `aui-protocol` for the render model and identity,
//! plus `crossbeam-channel` for the event stream (the same competing-
//! consumer receiver the transports already use). Anything else — in
//! particular any wire crate — fails the dependency test in `tests/`.
//!
//! # Blocking, not async
//!
//! [`ProviderAdapter`] is object-safe and blocking: the app runs commands
//! on its background executor and events through a forwarding thread, so an
//! async trait would buy nothing, and `async fn` is not object-safe without
//! a helper crate.
//!
//! # Saying no
//!
//! A provider that cannot do something answers [`ProviderError::Unsupported`]
//! naming the capability. "Not supported" cannot be spelled as success:
//! [`Provider::send`] returns `Result<Ack, ProviderError>`, and the refusal
//! is enforced there — mechanically, before any adapter code runs — rather
//! than left to each adapter's good behaviour.
//!
//! The declared side of the same promise is [`CapabilitySet`]: what the
//! provider says it can do before the app asks, so a button that cannot
//! work is not offered in the first place.
//!
//! The application holds a [`Provider`], never a bare
//! `Box<dyn ProviderAdapter>`: the wrapper is the seam as far as any caller
//! is concerned.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod ack;
mod capability;
mod command;
mod error;
mod event;
pub mod scripted;
mod traits;

pub use ack::{Ack, ModelSummary, PendingApproval, PendingQuestion, SessionSummary};
pub use aui_protocol::Delta;
pub use capability::{Capability, CapabilitySet, CapabilityState};
pub use command::{Command, ProviderId, QuestionAnswer, SubmissionPart};
pub use crossbeam_channel::Receiver;
pub use error::ProviderError;
pub use event::ProviderEvent;
pub use traits::{ConnectInfo, Handshake, Provider, ProviderAdapter};
