//! ACP in the registry (docs/82 5.4, 5.7): what it declares about itself,
//! and how its lanes open. The app reaches this adapter through
//! [`REGISTRATION`] alone — one line in its registry table.
//!
//! The agent is configured, not found: [`COMMAND_ENV`] holds its command
//! line (`BAAZ_ACP_AGENT="gemini --experimental-acp"`: the program, then its
//! arguments, split on whitespace). Until it is set the provider is not
//! offered anywhere ([`provider::ProviderDescriptor::offered`]; rule 13).

use std::path::Path;

use aui_protocol::PermissionMode;
use provider::{ModeDescriptor, Provider, ProviderDescriptor, ProviderError, ProviderFactory, ProviderTrait, SessionSpawn};

use crate::AcpAdapter;

/// The environment variable holding the agent's command line.
pub const COMMAND_ENV: &str = "BAAZ_ACP_AGENT";

/// ACP's declaration. Its label is the one place an "ACP" word lives
/// (rule 14: a provider's own row reads it from here).
pub static DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    key: "acp",
    label: "ACP agent",
    icon: crate::fold::MARK,
    // Configured, not found: no binary is searched for.
    binaries: &[],
    env_overrides: &[],
    install_command: None,
    update_command: None,
    docs_url: "https://agentclientprotocol.com",
    // ACP versions the protocol, not the agent: any agent that answers
    // `initialize` with version 1 is supported.
    version_floor: "0.0.0",
    account_label: "the agent's own sign-in",
    auth_status_args: None,
    usage_hint: "The agent reports no usage",
    sign_out_note: "The agent signs in and out on its own; Baaz cannot sign it out.",
    sign_out_args: None,
    own_mcp_note: None,
    // No config file of its own names MCP servers: Settings › MCP lists
    // only Baaz's `baaz` server for it.
    mcp_listing: None,
    utility_models: &[],
    modes: &MODES,
    plan: provider::PlanMode::None,
    routes: &[],
    traits: &[ProviderTrait::ResumesInNamedFolder],
    capabilities: |_| crate::caps::capabilities(crate::caps::last_handshake().as_ref()),
    command_env: Some(COMMAND_ENV),
};

/// The one mode an ACP agent runs in as far as Baaz can tell: its own
/// policy decides, and whatever it asks about raises a card. (Its own
/// modes, `session/set_mode`, do not map onto Baaz's four.)
static MODES: [ModeDescriptor; 1] = [ModeDescriptor {
    id: PermissionMode::OnRequest,
    label: "Auto",
    description: "The agent's own policy decides; whatever it asks about raises an approval card.",
    asks: true,
    sandbox: None,
}];

/// The app's registry line for this provider.
pub static REGISTRATION: provider::Registration =
    provider::Registration { descriptor: &DESCRIPTOR, factory: |_client| std::sync::Arc::new(AcpFactory) };

/// How an ACP lane opens: the configured agent, one child per session.
pub struct AcpFactory;

impl ProviderFactory for AcpFactory {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &DESCRIPTOR
    }

    /// The configured agent behind the gate, with the window's terminal
    /// relay, connected (the agent's `initialize`). It keeps nothing under
    /// Baaz's state dir: a resume names its folder.
    fn open(&self, spawn: &SessionSpawn) -> Result<Provider, ProviderError> {
        let program = spawn.program.as_ref().ok_or_else(not_configured)?;
        let args: Vec<String> = DESCRIPTOR.configured_command().map(|words| words[1..].to_vec()).unwrap_or_default();
        let adapter = AcpAdapter::new(&program.to_string_lossy(), &args);
        if let Some(relay) = spawn.mcp_servers.first() {
            adapter.set_terminal_relay(relay.clone());
        }
        let mut provider = Provider::new(adapter);
        provider.connect(&spawn.client)?;
        Ok(provider)
    }

    fn replay(&self, recording: &str) -> Option<provider::Replay> {
        crate::replay::fold_recording(recording)
    }
}

/// The open's refusal when no agent is configured, or its program is not
/// found.
fn not_configured() -> ProviderError {
    let named = DESCRIPTOR.configured_command().and_then(|words| words.into_iter().next());
    let reason = match named {
        Some(program) if !Path::new(&program).is_absolute() => {
            format!("{COMMAND_ENV} names `{program}`, which is not on PATH")
        }
        Some(program) => format!("{COMMAND_ENV} names `{program}`, which does not exist"),
        None => format!("no agent is configured: set {COMMAND_ENV} to its command line"),
    };
    ProviderError::launch_refusal(provider::RefusalHint::NotInstalled, format!("{} isn't set up", DESCRIPTOR.label), reason)
}
