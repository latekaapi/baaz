//! Muse in the registry (docs/82 5.4): what it declares about itself, and
//! the factory that hands out lanes on its one shared child.
//!
//! Muse sessions are lanes on one `muse serve` hub the window starts and
//! keeps, not a child each, so the factory is also the hub's slot: an open
//! waits while the hub starts ([`MUSE_START_WAIT`]), and the window drives
//! the hub's life through [`SharedChild`].

use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use aui_protocol::PermissionMode;
use provider::{
    ConnectInfo, McpServer, ModeDescriptor, Provider, ProviderAdapter as _, ProviderDescriptor,
    ProviderError, ProviderFactory, ProviderTrait, SessionSpawn, SharedChild, SharedStart,
};

use crate::MuseHub;

/// Muse's declaration.
pub static DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    key: "muse",
    label: "Muse",
    icon: provider::ProviderId::Muse,
    binaries: &["muse"],
    env_overrides: &["BAAZ_MUSE", "MUSE_BIN"],
    // No installer is published: the missing row links to the docs page a
    // person gets it from (the owner's link, 2026-10-23).
    install_command: None,
    // Its launcher updates itself.
    update_command: None,
    docs_url: "https://dev.meta.ai/lp/muse-code",
    version_floor: crate::MUSE_VERSION_FLOOR,
    account_label: "Meta account",
    // The account is read from the live connection.
    auth_status_args: None,
    usage_hint: "No reading yet — appears after your first Muse turn",
    sign_out_note: "This runs Muse `account/logout` — Muse will be signed out on this Mac, not just in Baaz.",
    // `account/logout` on the existing connection.
    sign_out_args: None,
    // Its sessions ride the shared child, which has no scoped argv.
    own_mcp_note: None,
    // No config file of its own names MCP servers: Settings › MCP lists
    // only Baaz's `baaz` server for it.
    mcp_listing: None,
    utility_models: &[(crate::oneshot::UTILITY_MODEL_ID, "Muse Spark 1.3"), ("", "Muse default")],
    modes: &MODES,
    // Its bundled `plan` skill, reached by a `/plan ` prefix
    // (`fixtures/msp/transcript-plan-probe.jsonl`).
    plan: provider::PlanMode::Skill,
    // `--provider meta` / `--provider echo` name the model vendor a Muse
    // open routes to (`OpenSession.model_provider`).
    routes: &["meta", "echo"],
    traits: &[
        ProviderTrait::NativeSessionList,
        ProviderTrait::BillingTier,
        ProviderTrait::LiveContextMeter,
        ProviderTrait::ReminderTail,
        ProviderTrait::CatalogFlagsSessionModel,
        ProviderTrait::TurnLifecycleEvents,
        ProviderTrait::SharedChild,
        ProviderTrait::LocalScriptedDraft,
        ProviderTrait::RoutesModelVendor,
        ProviderTrait::SideSessionCompletions,
        ProviderTrait::CatalogEfforts,
        ProviderTrait::ManagedSkills,
        ProviderTrait::ForkAtTurn,
        ProviderTrait::PagesHistory,
        ProviderTrait::AccountOnConnection,
    ],
    capabilities: |version| crate::capabilities_for_version(version.unwrap_or(crate::MUSE_MCP_VERSION_FLOOR)),
    command_env: None,
};

/// The app's registry line for this provider (docs/82 5.7).
pub static REGISTRATION: provider::Registration = provider::Registration {
    descriptor: &DESCRIPTOR,
    factory: |client| std::sync::Arc::new(MuseFactory::new(client.clone())),
};

/// Muse's modes are MSP's own four. Read-only refuses an unmatched MCP
/// call, but a tool the owner "always allowed" matches a persistent allow
/// rule, and its protocol lets a client select a profile, never add a
/// session deny — the line says so.
static MODES: [ModeDescriptor; 4] = [
    ModeDescriptor {
        id: PermissionMode::AllowAll,
        label: "Full access",
        description: "Nothing is gated; every action runs.",
        asks: false,
        sandbox: None,
    },
    ModeDescriptor {
        id: PermissionMode::OnRequest,
        label: "Auto",
        description: "Policy decides; unmatched actions ask only when the agent requests it.",
        asks: true,
        sandbox: None,
    },
    ModeDescriptor {
        id: PermissionMode::PromptUnmatched,
        label: "Ask",
        description: "Anything without a matching rule raises an approval card.",
        asks: true,
        sandbox: None,
    },
    ModeDescriptor {
        id: PermissionMode::DenyUnmatched,
        label: "Read-only",
        description: "Anything without a matching rule is refused. MCP tools you always allowed in Muse still run.",
        asks: false,
        sandbox: None,
    },
];

/// How long a Muse open waits for `muse serve` to come up before it fails.
pub const MUSE_START_WAIT: Duration = Duration::from_secs(45);

/// The Muse factory: the slot its one hub stands in, handing out lanes.
pub struct MuseFactory {
    state: Arc<(Mutex<Slot>, Condvar)>,
    client: ConnectInfo,
}

#[derive(Default)]
struct Slot {
    /// Where the hub stands now.
    state: HubState,
    /// The hub this run started, kept while it restarts or after it went
    /// down (its stderr tail is the exit reason).
    hub: Option<MuseHub>,
}

#[derive(Default)]
enum HubState {
    /// Not up yet: an open waits (the caller asked for it to start).
    #[default]
    Starting,
    /// Connected: lanes ride the hub.
    Ready,
    /// It could not start, and why.
    Down(String),
}

impl MuseFactory {
    /// An empty slot whose lanes shake hands as `client`.
    pub fn new(client: ConnectInfo) -> Self {
        Self { state: Arc::new((Mutex::new(Slot::default()), Condvar::new())), client }
    }

    /// Put an already-started hub in the slot, up: what [`SharedChild::start`]
    /// does with the hub it spawned, and what tests do with a stand-in's.
    pub fn install(&self, hub: MuseHub) {
        self.set(|slot| {
            slot.hub = Some(hub);
            slot.state = HubState::Ready;
        });
    }

    /// The hub, when it is up right now.
    pub fn hub(&self) -> Option<MuseHub> {
        let slot = self.state.0.lock().expect("muse slot");
        matches!(slot.state, HubState::Ready).then(|| slot.hub.clone()).flatten()
    }

    fn set(&self, change: impl FnOnce(&mut Slot)) {
        let (lock, ready) = &*self.state;
        change(&mut lock.lock().expect("muse slot"));
        ready.notify_all();
    }

    /// The hub this run started, up or not.
    fn last(&self) -> Option<MuseHub> {
        self.state.0.lock().expect("muse slot").hub.clone()
    }

    fn connected_lane(&self, hub: &MuseHub) -> Result<Provider, ProviderError> {
        let mut lane = hub.lane();
        lane.connect(&self.client)?;
        Ok(Provider::new(lane))
    }
}

impl ProviderFactory for MuseFactory {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &DESCRIPTOR
    }

    /// A Muse session-log capture (`--> ` / `<-- ` lines), folded through
    /// the same translation a hub lane runs ([`crate::replay::Capture`]).
    fn replay(&self, recording: &str) -> Option<provider::Replay> {
        let capture = crate::replay::Capture::parse(recording, "replay");
        if capture.is_empty() {
            return None;
        }
        Some(provider::Replay { session_id: capture.session_id(), sent: capture.sent.clone(), events: capture.events() })
    }

    /// A connected lane on the hub, waiting up to [`MUSE_START_WAIT`] while
    /// it starts. Blocking: the open path runs it on the background
    /// executor.
    fn open(&self, _spawn: &SessionSpawn) -> Result<Provider, ProviderError> {
        let (lock, ready) = &*self.state;
        let slot = lock.lock().expect("muse slot");
        let (slot, _) = ready
            .wait_timeout_while(slot, MUSE_START_WAIT, |slot| matches!(slot.state, HubState::Starting))
            .expect("muse slot");
        match (&slot.state, &slot.hub) {
            (HubState::Ready, Some(hub)) => {
                let hub = hub.clone();
                drop(slot);
                self.connected_lane(&hub)
            }
            (HubState::Down(reason), _) => Err(ProviderError::Unavailable { reason: reason.clone() }),
            _ => Err(ProviderError::Unavailable {
                reason: format!("Muse did not start within {}s", MUSE_START_WAIT.as_secs()),
            }),
        }
    }

    fn shared_child(&self) -> Option<&dyn SharedChild> {
        Some(self)
    }
}

impl SharedChild for MuseFactory {
    fn start(&self, program: &str, client: &ConnectInfo, mcp: Option<McpServer>) -> Result<SharedStart, ProviderError> {
        let established = crate::establish(program, client)?;
        // The relay rides the `sessionMcp` grant; set before the hub is up,
        // so no lane opens without it.
        if let Some(server) = mcp {
            established.hub.set_terminal_relay(server.command, server.socket);
        }
        let events = established.hub.connection_events();
        let start = SharedStart {
            events,
            agent_name: established.handshake.agent_name,
            agent_version: established.handshake.agent_version,
            warning: established.warning,
            client_tools: established.session_mcp,
        };
        self.install(established.hub);
        Ok(start)
    }

    fn reconnect(&self) -> Result<Vec<(String, ProviderError)>, ProviderError> {
        let hub = self.last().ok_or_else(|| ProviderError::Unavailable { reason: "Muse never started".into() })?;
        hub.reconnect()
    }

    fn started(&self) -> bool {
        self.last().is_some()
    }

    fn is_ready(&self) -> bool {
        matches!(self.state.0.lock().expect("muse slot").state, HubState::Ready)
    }

    fn set_starting(&self) {
        self.set(|slot| slot.state = HubState::Starting);
    }

    fn set_ready(&self) {
        self.set(|slot| {
            if slot.hub.is_some() {
                slot.state = HubState::Ready;
            }
        });
    }

    fn set_down(&self, reason: String) {
        self.set(|slot| slot.state = HubState::Down(reason));
    }

    /// A lane for connection-level commands (account, usage, the session
    /// list) when the hub is up now, never waiting: a sign-in screen asks
    /// and answers at once. Blocking only for the lane's own handshake
    /// report, which sends nothing on a hub that is up.
    fn control(&self) -> Result<Provider, ProviderError> {
        let hub = {
            let slot = self.state.0.lock().expect("muse slot");
            match (&slot.state, &slot.hub) {
                (HubState::Ready, Some(hub)) => hub.clone(),
                (HubState::Down(reason), _) => return Err(ProviderError::Unavailable { reason: reason.clone() }),
                _ => return Err(ProviderError::Unavailable { reason: "Muse is starting".into() }),
            }
        };
        self.connected_lane(&hub)
    }

    fn stderr_tail(&self) -> String {
        self.last().map(|hub| hub.stderr_tail()).unwrap_or_default()
    }

    fn refuses_resume_bridge(&self) -> bool {
        self.last().is_some_and(|hub| hub.refuses_resume_bridge())
    }

    fn followed(&self) -> Vec<String> {
        self.last().map(|hub| hub.followed()).unwrap_or_default()
    }

    fn schema_warning(&self) -> Option<String> {
        self.last().and_then(|hub| hub.schema_warning())
    }

    fn client_tools_granted(&self) -> bool {
        self.last().is_some_and(|hub| hub.session_mcp_granted())
    }

    fn forgive_crashes(&self) {
        if let Some(hub) = self.last() {
            hub.forgive_crashes();
        }
    }

    fn complete(
        &self,
        side_id: &str,
        workspace: &str,
        prompt: &str,
        label: &str,
        deadline: Duration,
    ) -> Result<String, ProviderError> {
        let hub = self.hub().ok_or_else(|| ProviderError::Unavailable { reason: "Muse is not running".into() })?;
        hub.complete(side_id, workspace, prompt, label, deadline)
    }

    fn shutdown(&self) {
        if let Some(hub) = self.last() {
            hub.shutdown();
        }
    }
}
