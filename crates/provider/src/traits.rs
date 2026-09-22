//! The seam: one object-safe trait every provider implements.
//!
//! Blocking, not async — and that is load-bearing, not laziness. The app
//! already runs every command on its background executor and every event
//! through a forwarding thread, so an async trait would buy nothing here;
//! and `async fn` in traits is not object-safe without a helper crate this
//! crate is forbidden from adding. The app holds `Box<dyn ProviderAdapter>`
//! and chooses at runtime, so every method here must stay callable through
//! the vtable: no generics, no `Self`-returning constructors.

use crossbeam_channel::Receiver;

use crate::{Ack, Command, ProviderError, ProviderEvent, ProviderId};

/// Who this client is, for the provider's handshake.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectInfo {
    /// Lowercase client name, e.g. `"baaz"`.
    pub client_name: String,
    /// Client version string.
    pub client_version: String,
    /// Capability names the client wants, e.g. `"bulk-export"`. These are
    /// backend-defined identifiers the seam does not interpret: unknown
    /// entries are not an error — the provider grants what it knows.
    pub capabilities: Vec<String>,
}

impl ConnectInfo {
    /// A minimal handshake identity: name and version, no capabilities.
    pub fn new(client_name: &str, client_version: &str) -> Self {
        Self {
            client_name: client_name.to_owned(),
            client_version: client_version.to_owned(),
            capabilities: Vec::new(),
        }
    }
}

/// What the handshake established.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Handshake {
    /// Which agent answered.
    pub provider: ProviderId,
    /// The agent's own name for itself.
    pub agent_name: String,
    /// The agent's version string.
    pub agent_version: String,
}

/// One agent, behind the neutral vocabulary.
///
/// Implementors translate *into* this vocabulary: neutral [`Command`]s in,
/// neutral [`Ack`]s and [`ProviderEvent`]s out. No method name, type name,
/// field, or capability string here may carry a provider's wire spelling.
pub trait ProviderAdapter: Send {
    /// Which agent this is.
    fn id(&self) -> ProviderId;

    /// Shake hands: identify the client and learn who answered. Must be
    /// called before [`Self::send`]; a second call is refused.
    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError>;

    /// Do one thing, synchronously. Admission only: the ack says the
    /// provider took the command, and the outcome arrives as
    /// [`ProviderEvent`]s. A provider that cannot do it answers
    /// [`ProviderError::Unsupported`] — never `Ok`.
    fn send(&self, command: Command) -> Result<Ack, ProviderError>;

    /// The event stream, in arrival order.
    ///
    /// Cloned receivers are **competing** consumers, not a broadcast: each
    /// event goes to exactly one clone. Keep exactly one consumer and fan
    /// out from there.
    fn events(&self) -> Receiver<ProviderEvent>;

    /// Hang up. Idempotent; also runs on drop.
    fn shutdown(&mut self);
}
