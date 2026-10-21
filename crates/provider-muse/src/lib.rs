//! `provider-muse` — the muse implementation of the provider seam.
//!
//! A [`MuseHub`] owns the one `muse serve` child and its MSP fold; each
//! session rides it as a [`MuseLane`], a [`ProviderAdapter`] like Claude
//! Code's and Codex's. Every wire spelling lives here: this is the only
//! crate in the seam allowed to depend on `muse-client`, and the per-command
//! translation is in `translate`. Spawning, `initialize` and the
//! schema-fingerprint warning live here too — [`establish`] is what `baaz`'s
//! connection path calls. [`MuseAdapter`] is the whole connection as one
//! adapter, for tests; [`SharedTransport`] and [`MuseHub::legacy_events`]
//! are transitional.

#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

mod caps;
pub mod errors;
pub mod facts;
mod hub;
pub mod modes;
pub mod output;
pub mod terminal;
mod translate;

use std::sync::Arc;

use crossbeam_channel::Receiver;
use muse_client::schema::{ApprovalRequestParams, UserInputRequestParams};
use muse_client::{MuseClient, MuseConfig, MuseEvent};
use provider::{
    Ack, CapabilitySet, Command, ConnectInfo, Handshake, ProviderAdapter, ProviderError,
    ProviderEvent, ProviderId,
};

pub use caps::{
    capabilities_for_version, muse_version_supported, MUSE_MCP_VERSION_FLOOR, MUSE_VERSION_FLOOR,
};
pub use hub::{side_of, MuseHub, MuseLane};
pub use terminal::TerminalRelay;
pub use translate::{approval_headline, question_headline};

/// The raw transport, shared between the hub and the application's
/// transitional paths (account, usage, titles) until they speak neutral
/// events.
pub type SharedTransport = Arc<MuseClient>;

/// The whole connection as one adapter: a [`MuseHub`] lane that sees every
/// session's events. What tests drive the command arms through; the
/// application holds a [`MuseHub`] and one [`MuseLane`] per session.
pub struct MuseAdapter {
    lane: MuseLane,
}

impl MuseAdapter {
    /// Wrap an already-spawned client; [`ProviderAdapter::connect`] runs the
    /// handshake. Spawning stays outside: which binary and flags are host
    /// decisions. [`MuseHub::connect`] is the full spawn-and-handshake path.
    pub fn new(client: MuseClient) -> Self {
        Self { lane: MuseHub::from_client(client).watch_all() }
    }

    /// The hub under this adapter.
    pub fn hub(&self) -> MuseHub {
        self.lane.hub()
    }

    /// Whether the handshake granted `userShell`.
    pub fn user_shell_granted(&self) -> bool {
        self.hub().user_shell_granted()
    }

    /// Whether the handshake granted `sessionMcp` (muse ≥ 1.3).
    pub fn session_mcp_granted(&self) -> bool {
        self.hub().session_mcp_granted()
    }

    /// Point the terminal relay at the bridge and socket (see
    /// [`MuseHub::set_terminal_relay`]).
    pub fn set_terminal_relay(&self, bridge: std::path::PathBuf, socket: std::path::PathBuf) {
        self.hub().set_terminal_relay(bridge, socket);
    }

    /// The schema-fingerprint warning from `initialize`.
    pub fn schema_warning(&self) -> Option<String> {
        self.hub().schema_warning()
    }

    /// The raw transport events, in wire order (transitional).
    pub fn legacy_events(&self) -> Receiver<MuseEvent> {
        self.hub().legacy_events()
    }
}

impl ProviderAdapter for MuseAdapter {
    fn id(&self) -> ProviderId {
        self.lane.id()
    }
    fn connect(&mut self, client: &ConnectInfo) -> Result<Handshake, ProviderError> {
        self.lane.connect(client)
    }
    fn capabilities(&self) -> CapabilitySet {
        self.lane.capabilities()
    }
    fn dispatch(&self, command: Command) -> Result<Ack, ProviderError> {
        self.lane.dispatch(command)
    }
    fn events(&self) -> Receiver<ProviderEvent> {
        self.lane.events()
    }
    fn shutdown(&mut self) {
        self.hub().shutdown();
        self.lane.shutdown();
    }
}

/// The tap on the shoulder for a server request, decided from its params
/// before the fold consumes the event. Unknown methods get no tap — their
/// card still arrives through the fold, and that silence is deliberate.
/// A KNOWN method whose params fail to decode is a caller-visible error:
/// dropping it would cost the person the prompt while the card still
/// appears, so the app would look like it is waiting on nothing.
pub(crate) fn tap_for(
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

/// Spawn `muse serve` and start its reader and writer threads.
///
/// **Durable, always.** `--no-session-log` accepts a turn and then emits no
/// view events at all (`docs/01-transport.md` §4), so an app that wants a
/// transcript may never set it.
///
/// The child spawns with the shared scrubbed env
/// ([`provider::child_env`]): that crate's `Command` lives inside
/// `muse-client`, out of reach of a direct [`provider::child_env::scrub_command`],
/// so the scrub rides as per-child `env_remove`s through
/// `muse-client`'s `spawn_with_env` — the process env is never touched.
/// Title and summary side sessions ride this same child over the wire —
/// no new process — so they are covered by this one scrub.
///
/// Blocking: run it on the background executor.
pub fn spawn(program: &str) -> Result<MuseClient, ProviderError> {
    let removals = provider::child_env::scrubbed_removals();
    MuseClient::spawn_with_env(
        &MuseConfig {
            program: program.into(),
            trust_workspace: true,
            no_session_log: false,
            extra_args: Vec::new(),
        },
        &removals,
        &[],
    )
    .map_err(translate::transport_error)
}

/// What spawning and shaking hands produced: the hub, the transitional
/// raw transport and event stream, and the neutral handshake facts the app
/// logs.
pub struct Established {
    /// The hub: one lane per session, plus the connection stream.
    pub hub: MuseHub,
    /// The one spawned child (transitional).
    pub transport: SharedTransport,
    /// The raw transport events, in wire order (transitional). Single consumer.
    pub legacy: Receiver<MuseEvent>,
    /// Who answered, neutrally.
    pub handshake: Handshake,
    /// The schema-fingerprint warning, pre-formatted for the log.
    pub warning: Option<String>,
    /// Whether `initialize` granted `userShell`.
    pub user_shell: bool,
    /// Whether `initialize` granted `sessionMcp` (muse ≥ 1.3).
    pub session_mcp: bool,
}

/// Spawn `muse serve` ([`spawn`]) and shake hands over it. Blocking.
pub fn establish(program: &str, client: &ConnectInfo) -> Result<Established, ProviderError> {
    let hub = MuseHub::connect(program, client)?;
    let transport = hub.transport().ok_or_else(|| ProviderError::Unavailable { reason: "Muse is not running".into() })?;
    let handshake = hub.handshake().ok_or_else(|| ProviderError::Unavailable { reason: "Muse is not running".into() })?;
    Ok(Established {
        legacy: hub.legacy_events(),
        transport,
        handshake,
        warning: hub.schema_warning(),
        user_shell: hub.user_shell_granted(),
        session_mcp: hub.session_mcp_granted(),
        hub,
    })
}
