//! `provider-muse` — the muse implementation of the provider seam.
//!
//! [`MuseAdapter`] implements [`ProviderAdapter`] over a [`MuseClient`]
//! transport and a [`MuseFold`]. Every wire spelling lives here: this is
//! the only crate in the seam allowed to depend on `muse-client`, and the
//! per-command translation is in [`translate`]. Spawning the child,
//! `initialize`, and the schema-fingerprint warning live here too —
//! [`establish`] is what `baaz`'s connection path calls — while the app's
//! session views still ride the raw transport until they move onto the
//! trait (see [`SharedTransport`] and [`MuseAdapter::legacy_events`]).

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod caps;
pub mod errors;
mod translate;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{unbounded, Receiver, Sender};
use muse_adapter::MuseFold;
use muse_client::schema::{ApprovalRequestParams, ClientCapabilities, UserInputRequestParams};
use muse_client::{MuseClient, MuseConfig, MuseEvent};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ProviderAdapter, ProviderError,
    ProviderEvent, ProviderId,
};

pub use caps::{
    capabilities_for_version, muse_version_supported, MUSE_MCP_VERSION_FLOOR, MUSE_VERSION_FLOOR,
};
pub use translate::{approval_headline, question_headline};

/// The raw transport, shared between the adapter and the app's legacy
/// session views. Transitional: session views still take an
/// `Arc<MuseClient>` (they move onto [`Command`] one lane at a time), so
/// one spawned child is held here and cloned out — never spawned twice.
/// The follow-up that moves the last view removes this alias with them.
pub type SharedTransport = Arc<MuseClient>;

/// The muse implementation: neutral [`Command`]s in, MSP on the pipe,
/// neutral [`Ack`]s and [`ProviderEvent`]s out.
///
/// Owns one spawned client (shared with the app's legacy views — see
/// [`SharedTransport`]) and the fold its events run through. A forwarding
/// thread pumps the client's event channel into the fold, the provider
/// channel, and the legacy channel; it ends when the child exits
/// ([`MuseEvent::Closed`]) or the transport's sender is dropped. The pump
/// is detached, never joined: legacy holders can outlive any one owner
/// (a parked session view keeps its transport until it is re-seated), so a
/// join could wait on a sender that is still legitimately alive — the same
/// accepted design as the app's event bridge.
pub struct MuseAdapter {
    client: SharedTransport,
    fold: Arc<Mutex<MuseFold>>,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    legacy_tx: Sender<MuseEvent>,
    legacy_rx: Receiver<MuseEvent>,
    pump: Option<JoinHandle<()>>,
    /// The agent version the handshake negotiated, for the version-gated
    /// capabilities. `None` before [`ProviderAdapter::connect`] runs.
    negotiated: Mutex<Option<String>>,
    /// Whether `initialize` granted `userShell`. Fixed for the connection
    /// lifetime; read by the app to decide whether a session may run shell
    /// commands. `None` before [`ProviderAdapter::connect`] runs.
    granted_shell: Mutex<Option<bool>>,
    /// The schema-fingerprint warning from `initialize`, pre-formatted.
    /// A mismatch is additive evolution, never a failure: the app logs it
    /// and carries on. `None` when the fingerprints agreed.
    warning: Mutex<Option<String>>,
}

impl MuseAdapter {
    /// Wrap an already-spawned client. Spawning stays outside the seam:
    /// which binary, which flags, and whose policy are host decisions, not
    /// provider ones. [`ProviderAdapter::connect`] runs the handshake.
    /// Use [`establish`] for the full spawn-and-handshake path.
    pub fn new(client: MuseClient) -> Self {
        Self::shared(Arc::new(client))
    }

    /// Wrap an already-spawned client held in a shared transport, so the
    /// adapter and the app's legacy views ride one child.
    pub fn shared(client: SharedTransport) -> Self {
        let (tx, rx) = unbounded();
        let (legacy_tx, legacy_rx) = unbounded();
        Self {
            client,
            fold: Arc::new(Mutex::new(MuseFold::new())),
            tx,
            rx,
            legacy_tx,
            legacy_rx,
            pump: None,
            negotiated: Mutex::new(None),
            granted_shell: Mutex::new(None),
            warning: Mutex::new(None),
        }
    }

    /// The shared transport, for the app's legacy session views.
    /// Transitional with [`SharedTransport`].
    pub fn transport(&self) -> SharedTransport {
        Arc::clone(&self.client)
    }

    /// The raw transport events, in wire order, for the app's legacy pump.
    /// Cloned receivers are competing consumers: keep exactly one consumer
    /// and fan out from there.
    pub fn legacy_events(&self) -> Receiver<MuseEvent> {
        self.legacy_rx.clone()
    }

    /// Whether the handshake granted `userShell`. `false` before
    /// [`ProviderAdapter::connect`] runs.
    pub fn user_shell_granted(&self) -> bool {
        self.granted_shell.lock().expect("granted mutex").unwrap_or(false)
    }

    /// The schema-fingerprint warning from `initialize`, pre-formatted for
    /// the log. `None` when the fingerprints agreed — the common case.
    pub fn schema_warning(&self) -> Option<String> {
        self.warning.lock().expect("warning mutex").clone()
    }

    /// Forward one transport event: fan it out to the legacy channel, fold
    /// it, emit its deltas, and raise the tap on the shoulder a server
    /// request needs. Returns `false` when the pump should stop.
    fn forward(
        fold: &Mutex<MuseFold>,
        tx: &Sender<ProviderEvent>,
        legacy: &Sender<MuseEvent>,
        event: MuseEvent,
    ) -> bool {
        // The legacy fan-out first: every transport event reaches the raw
        // channel whole, whatever the fold makes of it. Best-effort — a
        // gone consumer must not stall the provider channel.
        let _ = legacy.send(event.clone());
        match event {
            MuseEvent::Closed(code) => {
                let _ = tx.send(ProviderEvent::ConnectionLost {
                    reason: match code {
                        Some(code) => format!("the agent process exited ({code})"),
                        None => "the agent process exited".into(),
                    },
                });
                false
            }
            MuseEvent::Notification { ref session_id, .. } => {
                // Cloned before the fold consumes the event; the borrow
                // ends here, so the move below is legal.
                let session = session_id.clone();
                let deltas = fold.lock().expect("fold mutex").apply(event);
                if !deltas.is_empty() {
                    let _ = tx.send(ProviderEvent::Deltas { session_id: session, deltas });
                }
                true
            }
            MuseEvent::ServerRequest { method, params, .. } => {
                let tap = match tap_for(&method, &params) {
                    Ok(tap) => tap,
                    // The card still arrives through the fold below; without
                    // this the app would show it with no prompt attached and
                    // look like it is waiting on nothing. The closed event
                    // enum has no prompt-shaped error, so the failure rides
                    // the only error-shaped event — and the pump continues,
                    // because one bad prompt must not end the session.
                    Err(error) => {
                        let _ = tx.send(ProviderEvent::ConnectionLost {
                            reason: error.to_string(),
                        });
                        None
                    }
                };
                let session_id = params
                    .get("sessionId")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let deltas = fold
                    .lock()
                    .expect("fold mutex")
                    .apply(MuseEvent::ServerRequest { id: request_id(&params), method, params });
                if !deltas.is_empty() {
                    let _ = tx.send(ProviderEvent::Deltas { session_id, deltas });
                }
                if let Some(tap) = tap {
                    let _ = tx.send(tap);
                }
                true
            }
        }
    }
}

/// The tap on the shoulder for a server request, decided from its params
/// before the fold consumes the event. Unknown methods get no tap — their
/// card still arrives through the fold, and that silence is deliberate.
/// A KNOWN method whose params fail to decode is a caller-visible error:
/// dropping it would cost the person the prompt while the card still
/// appears, so the app would look like it is waiting on nothing.
fn tap_for(
    method: &str,
    params: &serde_json::Value,
) -> Result<Option<ProviderEvent>, ProviderError> {
    match method {
        "approval/request" => {
            let request: ApprovalRequestParams = serde_json::from_value(params.clone())
                .map_err(|error| ProviderError::Rejected {
                    reason: format!("unusable approval/request params: {error}"),
                })?;
            let headline = approval_headline(&request);
            Ok(Some(ProviderEvent::ApprovalRequested {
                session_id: request.session_id,
                headline,
                approval_id: request.approval_id,
            }))
        }
        "userInput/request" => {
            let request: UserInputRequestParams = serde_json::from_value(params.clone())
                .map_err(|error| ProviderError::Rejected {
                    reason: format!("unusable userInput/request params: {error}"),
                })?;
            let headline = question_headline(&request);
            Ok(Some(ProviderEvent::QuestionRaised {
                session_id: request.session_id,
                headline,
                question_id: request.user_input_id,
            }))
        }
        _ => Ok(None),
    }
}

/// A stand-in id for re-wrapping a server request after its tap was read:
/// the fold only needs *an* id, and the params already carry the real one
/// (`approvalId` / `userInputId`), so nothing here invents identity.
fn request_id(params: &serde_json::Value) -> serde_json::Value {
    params
        .get("approvalId")
        .or_else(|| params.get("userInputId"))
        .cloned()
        .unwrap_or(serde_json::Value::Null)
}

impl ProviderAdapter for MuseAdapter {
    fn id(&self) -> ProviderId {
        aui_protocol::Provider::Muse
    }

    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        if self.pump.is_some() {
            return Err(ProviderError::Rejected { reason: "already connected".into() });
        }
        let (result, warning) = self
            .client
            .initialize(
                &client.client_name,
                &client.client_version,
                ClientCapabilities {
                    // Sign-in is on the wire now (D22): `account/*` is
                    // experimental, so the opt-in is required — without it
                    // every account method answers `-32601` /
                    // `experimentalRequired`. `userShell` is the `!` escape
                    // hatch, requested below through the neutral
                    // `ConnectInfo::capabilities`.
                    experimental_api: Some(true),
                    opt_out_notification_methods: None,
                    requested_capabilities: if client.capabilities.is_empty() {
                        None
                    } else {
                        Some(client.capabilities.clone())
                    },
                    ..Default::default()
                },
            )
            .map_err(translate::transport_error)?;
        let live = self.client.events();
        let fold = Arc::clone(&self.fold);
        let tx = self.tx.clone();
        let legacy = self.legacy_tx.clone();
        let pump = std::thread::Builder::new()
            .name("provider-muse-pump".into())
            .spawn(move || {
                while let Ok(event) = live.recv() {
                    if !Self::forward(&fold, &tx, &legacy, event) {
                        break;
                    }
                }
            })
            .map_err(|error| ProviderError::Unavailable {
                reason: format!("could not start the event pump: {error}"),
            })?;
        self.pump = Some(pump);
        let version = result.server_info.version.clone();
        *self.negotiated.lock().expect("negotiated mutex") = Some(version.clone());
        *self.granted_shell.lock().expect("granted mutex") =
            Some(result.granted_capabilities.iter().any(|c| c.as_wire() == Some("userShell")));
        *self.warning.lock().expect("warning mutex") = warning.map(|warning| format!("{warning:?}"));
        Ok(Handshake {
            provider: aui_protocol::Provider::Muse,
            agent_name: result.server_info.name,
            agent_version: version,
        })
    }

    fn capabilities(&self) -> CapabilitySet {
        // Before connect no version was negotiated: assume the floor, so
        // the set is the supported one and nothing version-gated is
        // promised. No command maps to `ClientTools`, so the pre-connect
        // assumption never blocks a send.
        let version = self.negotiated.lock().expect("negotiated mutex").clone();
        match version {
            Some(version) => caps::capabilities_for_version(&version),
            None => caps::capabilities_for_version(caps::MUSE_VERSION_FLOOR),
        }
    }

    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        // `dispatch` takes `&self` and only the page arm touches the
        // fold, so the fold lives behind a mutex while the client — already
        // safe to share — stays directly owned. Commands stay concurrent
        // everywhere except the fold lock. The `Unavailable` refusal happens
        // before this runs, in `provider::Provider::send`, which is the only
        // path that reaches here.
        translate::dispatch(&self.client, &self.fold, command)
    }

    fn events(&self) -> Receiver<ProviderEvent> {
        self.rx.clone()
    }

    fn shutdown(&mut self) {
        // Best-effort early hang-up when this adapter holds the last
        // transport clone; otherwise the drop chain cleans the child up
        // when the legacy holders let go.
        if let Some(client) = Arc::get_mut(&mut self.client) {
            client.shutdown();
        }
        // Detach, never join — see the struct docs. The pump ends when the
        // transport's sender is dropped.
        self.pump.take();
    }
}

impl Drop for MuseAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Spawn `muse serve` and start its reader and writer threads.
///
/// **Durable, always.** `--no-session-log` accepts a turn and then emits no
/// view events at all (`docs/01-transport.md` §4), so an app that wants a
/// transcript may never set it.
///
/// Blocking: run it on the background executor.
pub fn spawn(program: &str) -> Result<MuseClient, ProviderError> {
    MuseClient::spawn(&MuseConfig {
        program: program.into(),
        trust_workspace: true,
        no_session_log: false,
        extra_args: Vec::new(),
    })
    .map_err(translate::transport_error)
}

/// What spawning and shaking hands produced: the adapter (which moves
/// behind the [`Provider`](provider::Provider) gate), the shared transport
/// and raw event stream the app's legacy views still ride, and the neutral
/// handshake facts the app logs.
pub struct Established {
    /// The connected adapter. Move it into a [`Provider`](provider::Provider);
    /// the raw dispatch is reachable only through its gate.
    pub adapter: MuseAdapter,
    /// The one spawned child, shared with the legacy views.
    pub transport: SharedTransport,
    /// The raw transport events, in wire order. Single consumer.
    pub legacy: Receiver<MuseEvent>,
    /// Who answered, neutrally.
    pub handshake: Handshake,
    /// The schema-fingerprint warning, pre-formatted for the log, or `None`
    /// when the fingerprints agreed.
    pub warning: Option<String>,
    /// Whether `initialize` granted `userShell`.
    pub user_shell: bool,
}

/// Spawn `muse serve` ([`spawn`]) and shake hands over it: identify the
/// client (requesting its [`ConnectInfo::capabilities`]) and learn who
/// answered. Blocking: run it on the background executor.
pub fn establish(program: &str, client: &ConnectInfo) -> Result<Established, ProviderError> {
    let transport = Arc::new(spawn(program)?);
    let mut adapter = MuseAdapter::shared(Arc::clone(&transport));
    let handshake = adapter.connect(client)?;
    let legacy = adapter.legacy_events();
    let warning = adapter.schema_warning();
    let user_shell = adapter.user_shell_granted();
    Ok(Established { adapter, transport, legacy, handshake, warning, user_shell })
}
