//! The registry: what each adapter declares about itself, and the factory
//! that opens its sessions (docs/81 B §c.1 items 1–2, docs/82 5.4).
//!
//! Before this module the app knew its providers by name: a label, an
//! install command, a binary, a version floor and fourteen behaviour
//! predicates were `match` arms on an app enum, and construction ran
//! through per-adapter setters the app called itself. Now each adapter
//! crate declares a [`ProviderDescriptor`] (the facts) and registers a
//! [`ProviderFactory`] (the behaviour), and the app iterates a
//! [`ProviderRegistry`] instead of naming providers.
//!
//! What a provider does beyond the seam's common ground is a
//! [`ProviderTrait`] in its descriptor — a named capability the app asks
//! about ("does this provider's sessions share one child?"), never "is this
//! provider X?".

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aui_protocol::PermissionMode;
use crossbeam_channel::Receiver;

use crate::{CapabilitySet, ConnectInfo, Provider, ProviderError, ProviderEvent, ProviderId};

/// What an adapter declares about itself: the facts the app shows and the
/// traits it branches on. Static data, so every view can read it without a
/// live connection.
#[derive(Debug)]
pub struct ProviderDescriptor {
    /// The registry key: what records, the session host and the CLI flag
    /// carry (`claude-code`). Stable across releases.
    pub key: &'static str,
    /// The human name the switcher and the provider's own rows show.
    pub label: &'static str,
    /// The mark it wears in the shared icon set.
    pub icon: ProviderId,
    /// The binaries its CLI installs, first the one Baaz runs.
    pub binaries: &'static [&'static str],
    /// The environment variables that name the binary outright, first
    /// match wins.
    pub env_overrides: &'static [&'static str],
    /// The vendor install command, typed into a terminal for the person
    /// to run — never run by Baaz. `None` when no installer is published
    /// (the missing row links to [`Self::docs_url`] instead).
    pub install_command: Option<&'static str>,
    /// The CLI's own update command, offered beside a too-old advisory.
    pub update_command: Option<&'static str>,
    /// The vendor docs behind every Docs button.
    pub docs_url: &'static str,
    /// The oldest CLI Baaz supports, as data: below it the status carries
    /// a too-old advisory, never a block.
    pub version_floor: &'static str,
    /// The account a signed-in line names ("claude.ai", "ChatGPT").
    pub account_label: &'static str,
    /// The arguments that print the CLI's sign-in state, for the
    /// diagnostics report; `None` when the account is read from the live
    /// connection instead.
    pub auth_status_args: Option<&'static [&'static str]>,
    /// The usage card's line while no reading has arrived yet.
    pub usage_hint: &'static str,
    /// What confirming sign-out says it runs: a CLI sign-out signs the CLI
    /// out on this Mac, not just Baaz out of it.
    pub sign_out_note: &'static str,
    /// The CLI arguments that sign it out, for a provider that signs out
    /// with a plain command (the others sign out over their own wire).
    pub sign_out_args: Option<&'static [&'static str]>,
    /// The "Use my own MCP servers" switch's line, for a provider whose
    /// sessions Baaz scopes down to its own tools; `None` when it has no
    /// such switch.
    pub own_mcp_note: Option<&'static str>,
    /// The models a one-off assistant chat can start on, `(id, label)`,
    /// cheapest first; an empty id is the CLI's own default.
    pub utility_models: &'static [(&'static str, &'static str)],
    /// The approval modes it runs, in picker order, in its own words.
    pub modes: &'static [ModeDescriptor],
    /// Its plan mode, if it has one: what the one Plan toggle turns on
    /// (docs/82 5.5). [`PlanMode::None`] leaves the toggle absent.
    pub plan: PlanMode,
    /// The model-vendor routes its open can carry besides its key
    /// (`--provider echo`): a flag naming one of these opens this provider.
    pub routes: &'static [&'static str],
    /// What it offers beyond the seam's common ground.
    pub traits: &'static [ProviderTrait],
    /// Its capability table at a CLI version (its declared floor when the
    /// version is not known): the adapter's own evidence, never a copy.
    pub capabilities: fn(Option<&str>) -> CapabilitySet,
}

impl ProviderDescriptor {
    /// Whether this provider declares `trait_`.
    pub fn has(&self, trait_: ProviderTrait) -> bool {
        self.traits.contains(&trait_)
    }

    /// Its declaration for `mode`, when it runs that mode.
    pub fn mode(&self, mode: PermissionMode) -> Option<&ModeDescriptor> {
        self.modes.iter().find(|declared| declared.id == mode)
    }

    /// Whether `raw` names this provider: its key or one of its routes.
    pub fn answers_to(&self, raw: &str) -> bool {
        self.key == raw || self.routes.contains(&raw)
    }
}

/// One approval mode as the adapter runs it: the neutral mode it maps to,
/// and what the mode actually enforces on this provider, in its words.
#[derive(Debug)]
pub struct ModeDescriptor {
    /// The neutral mode this declaration stands for.
    pub id: PermissionMode,
    /// The picker label.
    pub label: &'static str,
    /// The picker's detail line: what this provider actually lets through
    /// — never more than the backend enforces.
    pub description: &'static str,
    /// Whether the mode can raise an approval card.
    pub asks: bool,
    /// The sandbox the provider applies under the mode, in its own words,
    /// when it applies one.
    pub sandbox: Option<&'static str>,
}

/// How a provider plans before it acts: the one Plan toggle turns on each
/// provider's own (docs/82 5.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanMode {
    /// It has none the seam can reach: the toggle is absent (rule 13).
    None,
    /// Its own plan permission mode, switched live with
    /// [`crate::Command::SetPlanMode`]; it reports entering and leaving it
    /// with [`crate::ProviderEvent::PlanModeChanged`].
    Native,
    /// A plan skill the prompt reaches by a `/plan ` prefix, with the
    /// session held in Read-only meanwhile; the reply becomes a plan card.
    Skill,
}

/// The four neutral modes with the protocol's own words, for an adapter
/// whose modes mean exactly what the protocol says; `deny_unmatched`
/// replaces Read-only's line where the provider lets more through.
pub const fn protocol_modes(deny_unmatched: &'static str, read_only_sandbox: Option<&'static str>) -> [ModeDescriptor; 4] {
    [
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
            description: deny_unmatched,
            asks: false,
            sandbox: read_only_sandbox,
        },
    ]
}

/// What a provider offers beyond the seam's common ground: the questions
/// the app used to ask as "is this provider X?", named.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderTrait {
    /// It keeps its own session list, which the app reads and merges; the
    /// others' sessions are the app's own records.
    NativeSessionList,
    /// The login's plan decides what a turn costs: its sessions carry the
    /// billing banner and its send guard.
    BillingTier,
    /// It reports the context window as it moves; the others' meters read
    /// the settled turns' totals.
    LiveContextMeter,
    /// A turn can stay open past its whole reply on the provider's own
    /// bookkeeping: the turn reads finished while it stays open.
    ReminderTail,
    /// Its one catalog flags the session's effective model, shared across
    /// its sessions.
    CatalogFlagsSessionModel,
    /// It announces each turn's start and end as events, so a view never
    /// infers a running turn from the transcript.
    TurnLifecycleEvents,
    /// A model can rule a mode out: a mode a model change leaves
    /// unsupported steps down to Ask.
    ModelsLimitModes,
    /// Its sessions are lanes on one child the window starts and keeps
    /// ([`SharedChild`]), not a child per session.
    SharedChild,
    /// In scripted chrome its new session is a local view rather than a
    /// scripted lane.
    LocalScriptedDraft,
    /// The open names the model vendor its requests route to and carries
    /// the approval mode only when one was picked.
    RoutesModelVendor,
    /// Utility completions run as a hidden side session on its shared
    /// connection, never through a one-shot CLI.
    SideSessionCompletions,
    /// Its catalog rows carry its own effort tiers, default and notes.
    CatalogEfforts,
    /// The Skills page manages its skills: they load into its sessions, and
    /// it writes a new one from a prompt (a `/create-skill` command).
    ManagedSkills,
    /// It re-attaches a stored session without its history and pages it
    /// in afterwards; the others replay it with the resume.
    PagesHistory,
    /// Its account lives on its shared connection: it signs in and out
    /// through the app's own sheet, and the account line reads the live
    /// connection.
    AccountOnConnection,
    /// It signs in with a device code its own child mints, shown in the
    /// app; the others sign in through their CLI in a terminal.
    DeviceCodeSignIn,
}

/// A stdio MCP server every session of a window gets: the terminal relay's
/// bridge, pointed at the window's socket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpServer {
    /// The bridge binary.
    pub command: PathBuf,
    /// The socket it relays to.
    pub socket: PathBuf,
}

/// What an open needs beyond the provider: everything construction used to
/// set through per-adapter setters. The session itself (workspace, mode,
/// model) opens afterwards with `Command::OpenSession` or
/// `Command::ResumeSession`, which carry those.
#[derive(Clone, Debug)]
pub struct SessionSpawn {
    /// Who this client is, for the lane's handshake.
    pub client: ConnectInfo,
    /// The resolved CLI, for a provider that spawns a child per session.
    pub program: Option<PathBuf>,
    /// Baaz's own state dir: the adapter's private home lives under it.
    pub state_dir: PathBuf,
    /// The person's home, for an adapter that links or reads its owner's
    /// config.
    pub owner_home: Option<PathBuf>,
    /// The MCP servers each session gets.
    pub mcp_servers: Vec<McpServer>,
    /// Whether the person opted in to the CLI's own configured MCP servers.
    pub inherit_owner_mcp: bool,
}

impl SessionSpawn {
    /// A spawn as `client` under `state_dir`, with nothing else set.
    pub fn new(client: ConnectInfo, state_dir: PathBuf) -> Self {
        Self { client, program: None, state_dir, owner_home: None, mcp_servers: Vec::new(), inherit_owner_mcp: false }
    }
}

/// The stored transcripts a provider keeps on disk, by session id.
pub type StoredSessions = HashMap<String, PathBuf>;

/// How one provider opens sessions and reads what it stored. Each adapter
/// crate registers one; the app holds them in a [`ProviderRegistry`].
pub trait ProviderFactory: Send + Sync {
    /// The facts this provider declares.
    fn descriptor(&self) -> &'static ProviderDescriptor;

    /// What it can do at `version` (the connected CLI's, when known; its
    /// declared floor otherwise).
    fn capabilities(&self, version: Option<&str>) -> CapabilitySet {
        (self.descriptor().capabilities)(version)
    }

    /// Connect one session lane. Blocking: resolving, spawning and shaking
    /// hands, or waiting for a shared child to come up.
    fn open(&self, spawn: &SessionSpawn) -> Result<Provider, ProviderError>;

    /// Whether an open through this factory runs on the spot: nothing to
    /// spawn and nothing that blocks.
    fn lands_inline(&self) -> bool {
        false
    }

    /// The homes its stored transcripts live in: Baaz's own under
    /// `state_dir`, then the owner's under `owner_home`. Empty when it
    /// stores none the app reads.
    fn homes(&self, _state_dir: &Path, _owner_home: Option<&Path>) -> Vec<PathBuf> {
        Vec::new()
    }

    /// The transcripts stored under one of its [`homes`](Self::homes), by
    /// session id.
    fn stored_sessions(&self, _home: &Path) -> StoredSessions {
        StoredSessions::new()
    }

    /// One stored transcript's searchable words: the person's and the
    /// agent's messages, capped.
    fn stored_transcript(&self, _raw: &str) -> String {
        String::new()
    }

    /// The child its sessions share, when they share one
    /// ([`ProviderTrait::SharedChild`]).
    fn shared_child(&self) -> Option<&dyn SharedChild> {
        None
    }
}

/// What came back from starting a shared child.
pub struct SharedStart {
    /// The connection stream: row facts, account, usage, connection loss.
    /// Kept across respawns.
    pub events: Receiver<ProviderEvent>,
    /// The agent's own name for itself.
    pub agent_name: String,
    /// The agent's version string.
    pub agent_version: String,
    /// A schema-fingerprint warning for the log; never a failure.
    pub warning: Option<String>,
    /// Whether the handshake granted client tools (the terminal relay's
    /// route).
    pub client_tools: bool,
}

/// The one child a provider's sessions share: the window starts it, keeps
/// it, and hands out lanes on it. Opens wait while it starts.
pub trait SharedChild: Send + Sync {
    /// Spawn `program`, shake hands as `client`, point the terminal relay
    /// at `mcp` — all before any waiting open gets a lane. Blocking.
    fn start(&self, program: &str, client: &ConnectInfo, mcp: Option<McpServer>) -> Result<SharedStart, ProviderError>;
    /// Respawn after a loss and re-attach the followed sessions; the ones
    /// it would not re-attach come back with why. Blocking.
    fn reconnect(&self) -> Result<Vec<(String, ProviderError)>, ProviderError>;
    /// Whether it ever started this run (a reconnect respawns it; a first
    /// start is [`Self::start`]).
    fn started(&self) -> bool;
    /// Whether it is up right now.
    fn is_ready(&self) -> bool;
    /// It is (re)starting: opens wait.
    fn set_starting(&self);
    /// It is up again after a respawn.
    fn set_ready(&self);
    /// It could not start: opens fail with `reason`.
    fn set_down(&self, reason: String);
    /// A lane for connection-level commands (account, usage, the session
    /// list) when it is up now, never waiting.
    fn control(&self) -> Result<Provider, ProviderError>;
    /// The child's last stderr lines, for the log.
    fn stderr_tail(&self) -> String;
    /// Whether a resume's terminal bridge was refused this run.
    fn refuses_resume_bridge(&self) -> bool;
    /// The sessions it follows.
    fn followed(&self) -> Vec<String>;
    /// The schema-fingerprint warning of the last start, for the log.
    fn schema_warning(&self) -> Option<String>;
    /// Whether the last handshake granted client tools.
    fn client_tools_granted(&self) -> bool;
    /// Forget past crashes: a person-asked reconnect starts the backoff
    /// over.
    fn forgive_crashes(&self);
    /// Run `prompt` once in a hidden side session `side_id` and return the
    /// reply. Blocking, up to `deadline`.
    fn complete(
        &self,
        side_id: &str,
        workspace: &str,
        prompt: &str,
        label: &str,
        deadline: std::time::Duration,
    ) -> Result<String, ProviderError>;
    /// Hang up for good.
    fn shutdown(&self);
}

/// Every provider this build registers, in registration order.
#[derive(Clone, Default)]
pub struct ProviderRegistry {
    factories: Vec<Arc<dyn ProviderFactory>>,
}

impl std::fmt::Debug for ProviderRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.factories.iter().map(|factory| factory.descriptor().key)).finish()
    }
}

/// A provider key no registered adapter answers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownProvider(pub String);

impl std::fmt::Display for UnknownProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "no provider `{}` is registered in this build", self.0)
    }
}

impl std::error::Error for UnknownProvider {}

impl ProviderRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `factory`. A second registration under the same key
    /// replaces the first, in place — how a test or a probe swaps one
    /// provider for a stand-in.
    pub fn register(mut self, factory: Arc<dyn ProviderFactory>) -> Self {
        let key = factory.descriptor().key;
        match self.factories.iter_mut().find(|existing| existing.descriptor().key == key) {
            Some(slot) => *slot = factory,
            None => self.factories.push(factory),
        }
        self
    }

    /// Every registered factory, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn ProviderFactory>> {
        self.factories.iter()
    }

    /// The factory answering to `raw` (its key or a route).
    pub fn get(&self, raw: &str) -> Result<&Arc<dyn ProviderFactory>, UnknownProvider> {
        self.factories
            .iter()
            .find(|factory| factory.descriptor().answers_to(raw))
            .ok_or_else(|| UnknownProvider(raw.to_owned()))
    }

    /// The first registered factory declaring `trait_`.
    pub fn with(&self, trait_: ProviderTrait) -> Option<&Arc<dyn ProviderFactory>> {
        self.factories.iter().find(|factory| factory.descriptor().has(trait_))
    }

    /// The shared child of the first provider whose sessions share one.
    pub fn shared_child(&self) -> Option<&dyn SharedChild> {
        self.factories.iter().find_map(|factory| factory.shared_child())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    static PLAIN: ProviderDescriptor = ProviderDescriptor {
        key: "plain",
        label: "Plain",
        icon: ProviderId::Pi,
        binaries: &["plain"],
        env_overrides: &[],
        install_command: None,
        update_command: None,
        docs_url: "https://example.invalid",
        version_floor: "1.0.0",
        account_label: "Plain",
        auth_status_args: None,
        usage_hint: "No reading yet",
        sign_out_note: "",
        sign_out_args: None,
        own_mcp_note: None,
        utility_models: &[],
        modes: &protocol_modes("Anything without a matching rule is refused.", None),
        plan: crate::PlanMode::None,
        routes: &["plain-route"],
        traits: &[ProviderTrait::ModelsLimitModes],
        capabilities: |_| {
            use crate::ProviderAdapter as _;
            crate::scripted::ScriptedProvider::new().capabilities()
        },
    };

    struct Plain(&'static str);

    impl ProviderFactory for Plain {
        fn descriptor(&self) -> &'static ProviderDescriptor {
            &PLAIN
        }
        fn open(&self, _spawn: &SessionSpawn) -> Result<Provider, ProviderError> {
            Err(ProviderError::Unavailable { reason: self.0.to_owned() })
        }
    }

    #[test]
    fn a_key_or_a_route_finds_its_factory_and_anything_else_is_an_error() {
        let registry = ProviderRegistry::new().register(Arc::new(Plain("first")));
        assert_eq!(registry.get("plain").map(|f| f.descriptor().label).ok(), Some("Plain"));
        assert!(registry.get("plain-route").is_ok(), "a declared route answers");
        assert_eq!(registry.get("muse").err(), Some(UnknownProvider("muse".into())), "never a fallback");
        assert!(registry.with(ProviderTrait::ModelsLimitModes).is_some());
        assert!(registry.with(ProviderTrait::SharedChild).is_none());
        assert!(registry.shared_child().is_none());
    }

    #[test]
    fn a_second_registration_under_one_key_replaces_the_first_in_place() {
        let registry =
            ProviderRegistry::new().register(Arc::new(Plain("first"))).register(Arc::new(Plain("second")));
        assert_eq!(registry.iter().count(), 1);
        let error = registry.get("plain").expect("registered").open(&SessionSpawn::new(ConnectInfo::new("test", "0"), PathBuf::new())).err();
        assert!(error.map(|e| e.to_string()).unwrap_or_default().contains("second"), "the second registration answers");
    }

    #[test]
    fn a_descriptor_answers_for_its_modes_and_traits() {
        assert!(PLAIN.has(ProviderTrait::ModelsLimitModes));
        assert!(!PLAIN.has(ProviderTrait::BillingTier));
        let read_only = PLAIN.mode(PermissionMode::DenyUnmatched).expect("declared");
        assert_eq!(read_only.label, "Read-only");
        assert!(!read_only.asks);
        for mode in PLAIN.modes {
            assert_eq!(mode.label, mode.id.label(), "the protocol's own words");
        }
    }
}
