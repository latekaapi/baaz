//! The seam: one object-safe trait every provider implements, and the
//! wrapper every caller holds.
//!
//! Blocking, not async — and that is load-bearing, not laziness. The app
//! already runs every command on its background executor and every event
//! through a forwarding thread, so an async trait would buy nothing here;
//! and `async fn` in traits is not object-safe without a helper crate this
//! crate is forbidden from adding. The app holds a [`Provider`] and chooses
//! at runtime, so every trait method here must stay callable through the
//! vtable: no generics, no `Self`-returning constructors.
//!
//! The gate lives on [`Provider`], not on the trait, and that placement is
//! the guarantee. A provided `send` on the trait would be a convention: any
//! `impl ProviderAdapter` block can define its own `fn send`, and Rust then
//! dispatches to that method without ever falling back to the trait default
//! — silently dropping the refusal for that adapter. [`Provider::send`] is
//! an inherent method on a concrete struct instead, so no adapter `impl`
//! block can shadow it, and coherence forbids adapters from adding methods
//! to `Provider` at all. There is deliberately no accessor for the inner
//! adapter, so the raw dispatch is unreachable except through the gate.

use crossbeam_channel::Receiver;

use crate::{
    Ack, CapabilitySet, CapabilityState, Command, ProviderError, ProviderEvent, ProviderId,
};

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
///
/// The trait exposes only the raw dispatch ([`dispatch`](Self::dispatch)):
/// it cannot refuse, and nothing about it is enforced. The enforced refusal
/// is [`Provider::send`], which every caller holds instead of a bare trait
/// object. An adapter that forgets to refuse still refuses, because its
/// dispatch runs only behind that gate.
///
/// The trait is deliberately unsealed — a future second provider implements
/// it from another crate. Sealing would narrow *who* can implement without
/// stopping an implementor from misbehaving; the wrapper narrows *what any
/// caller can reach*, which is the property that holds.
pub trait ProviderAdapter: Send {
    /// Which agent this is.
    fn id(&self) -> ProviderId;

    /// Shake hands: identify the client and learn who answered. Must be
    /// called before any command is sent; a second call is refused.
    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError>;

    /// What this provider can do, before the app asks. The UI reads the
    /// set to decide which buttons to offer; [`Provider::send`] reads it to
    /// refuse what is [`CapabilityState::Unavailable`] before any adapter
    /// code runs.
    fn capabilities(&self) -> CapabilitySet;

    /// The adapter's real dispatch. Raw and ungated: it runs only when
    /// [`Provider::send`] has already refused what is
    /// [`CapabilityState::Unavailable`], and no caller reaches it any other
    /// way — [`Provider`] exposes no path to the bare adapter. May still
    /// refuse anything further with [`ProviderError::Unsupported`]: the gate
    /// is a floor, so partial support inside one capability (following a
    /// session but not paging it) stays a typed refusal in the adapter.
    /// (`Emulated`, `Native`, and `Unverified` all proceed through the gate:
    /// honest ignorance is attempted, never refused.)
    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError>;

    /// The event stream, in arrival order.
    ///
    /// Cloned receivers are **competing** consumers, not a broadcast: each
    /// event goes to exactly one clone. Keep exactly one consumer and fan
    /// out from there.
    fn events(&self) -> Receiver<ProviderEvent>;

    /// Hang up. Idempotent; also runs on drop.
    fn shutdown(&mut self);
}

/// The one way to talk to a provider: an adapter behind the enforced gate.
///
/// The application holds this, never a bare `Box<dyn ProviderAdapter>`.
/// `send` is an inherent method here — not a trait method any adapter can
/// override — so every command passes the capability check before the
/// adapter's dispatch runs. The rest of the trait's surface is forwarded
/// unchanged.
pub struct Provider {
    inner: Box<dyn ProviderAdapter>,
}

impl Provider {
    /// Hold an adapter behind the gate. After this, the adapter's raw
    /// dispatch is reachable only through [`send`](Self::send).
    pub fn new(adapter: impl ProviderAdapter + 'static) -> Self {
        Self { inner: Box::new(adapter) }
    }

    /// Which agent this is.
    pub fn id(&self) -> ProviderId {
        self.inner.id()
    }

    /// Shake hands: identify the client and learn who answered. Must be
    /// called before [`send`](Self::send); a second call is refused.
    pub fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        self.inner.connect(client)
    }

    /// What this provider can do, before the app asks.
    pub fn capabilities(&self) -> CapabilitySet {
        self.inner.capabilities()
    }

    /// Do one thing, synchronously. Admission only: the ack says the
    /// provider took the command, and the outcome arrives as
    /// [`ProviderEvent`]s. A provider that cannot do it answers
    /// [`ProviderError::Unsupported`] — never `Ok`.
    ///
    /// The gate: the command's [`Command::required_capability`] is looked up
    /// in [`capabilities`](Self::capabilities) first, and an `Unavailable`
    /// state is refused here, before [`dispatch`](ProviderAdapter::dispatch)
    /// runs. An adapter whose dispatch would have returned `Ok` still
    /// refuses, because its dispatch is unreachable except through here.
    pub fn send(&self, command: Command) -> Result<Ack, ProviderError> {
        let needed = command.required_capability();
        if let CapabilityState::Unavailable { reason } = self.inner.capabilities().state(needed) {
            return Err(ProviderError::unsupported(command.capability(), reason.clone()));
        }
        self.inner.dispatch(command)
    }

    /// The event stream, in arrival order (competing consumers — see
    /// [`ProviderAdapter::events`]).
    pub fn events(&self) -> Receiver<ProviderEvent> {
        self.inner.events()
    }

    /// Hang up. Idempotent; also runs on drop.
    pub fn shutdown(&mut self) {
        self.inner.shutdown();
    }
}
