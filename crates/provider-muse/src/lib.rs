//! `provider-muse` — the muse implementation of the provider seam.
//!
//! [`MuseAdapter`] implements [`ProviderAdapter`] over a [`MuseClient`]
//! transport and a [`MuseFold`]. Every wire spelling lives here: this is
//! the only crate in the seam allowed to depend on `muse-client`, and the
//! per-command translation is in [`translate`]. Nothing is moved out of
//! `baaz` — the app still talks to `muse-client` directly, and rewiring it
//! onto this trait is a separate task.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod caps;
mod translate;

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crossbeam_channel::{unbounded, Receiver, Sender};
use muse_adapter::MuseFold;
use muse_client::schema::{
    ApprovalRequestParams, ClientCapabilities, UserInputRequestParams,
};
use muse_client::{MuseClient, MuseEvent};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ProviderAdapter, ProviderError,
    ProviderEvent, ProviderId,
};

pub use caps::{
    capabilities_for_version, muse_version_supported, MUSE_MCP_VERSION_FLOOR, MUSE_VERSION_FLOOR,
};
pub use translate::{approval_headline, question_headline};

/// The muse implementation: neutral [`Command`]s in, MSP on the pipe,
/// neutral [`Ack`]s and [`ProviderEvent`]s out.
///
/// Owns one spawned client and the fold its events run through. A
/// forwarding thread pumps the client's event channel into the fold and the
/// provider channel; it ends when the child exits ([`MuseEvent::Closed`])
/// or [`ProviderAdapter::shutdown`] runs.
pub struct MuseAdapter {
    client: MuseClient,
    fold: Arc<Mutex<MuseFold>>,
    tx: Sender<ProviderEvent>,
    rx: Receiver<ProviderEvent>,
    pump: Option<JoinHandle<()>>,
    /// The agent version the handshake negotiated, for the version-gated
    /// capabilities. `None` before [`ProviderAdapter::connect`] runs.
    negotiated: Mutex<Option<String>>,
}

impl MuseAdapter {
    /// Wrap an already-spawned client. Spawning stays outside the seam:
    /// which binary, which flags, and whose policy are host decisions, not
    /// provider ones. [`ProviderAdapter::connect`] runs the handshake.
    pub fn new(client: MuseClient) -> Self {
        let (tx, rx) = unbounded();
        Self {
            client,
            fold: Arc::new(Mutex::new(MuseFold::new())),
            tx,
            rx,
            pump: None,
            negotiated: Mutex::new(None),
        }
    }

    /// Forward one transport event: fold it, emit its deltas, and raise the
    /// tap on the shoulder a server request needs. Returns `false` when the
    /// pump should stop.
    fn forward(
        fold: &Mutex<MuseFold>,
        tx: &Sender<ProviderEvent>,
        event: MuseEvent,
    ) -> bool {
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
        let (result, _warning) = self
            .client
            .initialize(
                &client.client_name,
                &client.client_version,
                ClientCapabilities {
                    experimental_api: None,
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
        let pump = std::thread::Builder::new()
            .name("provider-muse-pump".into())
            .spawn(move || {
                while let Ok(event) = live.recv() {
                    if !Self::forward(&fold, &tx, event) {
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
        self.client.shutdown();
        if let Some(pump) = self.pump.take() {
            let _ = pump.join();
        }
    }
}

impl Drop for MuseAdapter {
    fn drop(&mut self) {
        self.shutdown();
    }
}
