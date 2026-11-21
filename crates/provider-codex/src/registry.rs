//! Codex in the registry (docs/82 5.4): what it declares about itself and
//! how its session lanes open. The app reaches this adapter through
//! [`CodexFactory`] alone; everything a lane needs before `Command::OpenSession`
//! arrives in a [`SessionSpawn`].

use std::path::{Path, PathBuf};

use aui_protocol::PermissionMode;
use provider::{
    ModeDescriptor, Provider, ProviderDescriptor, ProviderError, ProviderFactory,
    ProviderTrait, SessionSpawn, StoredSessions,
};

use crate::{CodexAdapter, TerminalRelay};

/// Codex's declaration.
pub static DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    key: "codex",
    label: "Codex",
    icon: provider::ProviderId::Codex,
    binaries: &["codex"],
    env_overrides: &["BAAZ_CODEX"],
    // The vendor's own installer (owner 2026-10-25): no Node needed, lands
    // in ~/.local/bin, updates itself. npm left new users without Node at
    // a dead end.
    install_command: Some("curl -fsSL https://chatgpt.com/codex/install.sh | sh"),
    // The CLI's own updater, whichever way it was installed.
    update_command: Some("codex update"),
    docs_url: "https://developers.openai.com/codex",
    version_floor: crate::CODEX_VERSION_FLOOR,
    account_label: "ChatGPT",
    auth_status_args: Some(&["login", "status"]),
    usage_hint: "No reading yet",
    sign_out_note: "This runs Codex `account/logout` — Codex will be signed out on this Mac, not just in Baaz.",
    // `account/logout` over its own app-server.
    sign_out_args: None,
    own_mcp_note: Some(
        "Off: sessions see only Baaz's tools. On: your ~/.codex/config.toml \
            servers and plugins load too. Applies to sessions started after this change.",
    ),
    mcp_listing: Some(crate::terminal::mcp_listing),
    utility_models: &[("", "Codex default")],
    modes: &MODES,
    // The app-server schema defines a `plan` collaboration mode, but no
    // request field carries it in the vendored schema and no fixture shows
    // one: not reached blind, so the Plan toggle is absent.
    plan: provider::PlanMode::None,
    routes: &[],
    traits: &[ProviderTrait::ModelsLimitModes, ProviderTrait::DeviceCodeSignIn],
    capabilities: |_| crate::caps::capabilities(),
    command_env: None,
};

/// The app's registry line for this provider (docs/82 5.7).
pub static REGISTRATION: provider::Registration = provider::Registration {
    descriptor: &DESCRIPTOR,
    factory: |_client| std::sync::Arc::new(CodexFactory),
};

/// Codex's modes: the `(approval, sandbox)` pair each opens a thread with
/// ([`crate::child::codex_thread_mode`]). Read-only declines every MCP
/// approval prompt, but a tool its own config sets to
/// `approval_mode = "approve"` never prompts — the line says so.
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
        sandbox: Some("workspace-write"),
    },
    ModeDescriptor {
        id: PermissionMode::PromptUnmatched,
        label: "Ask",
        description: "Anything without a matching rule raises an approval card.",
        asks: true,
        sandbox: Some("workspace-write"),
    },
    ModeDescriptor {
        id: PermissionMode::DenyUnmatched,
        label: "Read-only",
        description: "Anything without a matching rule is refused. MCP tools your Codex config auto-approves still run.",
        asks: false,
        sandbox: Some("read-only"),
    },
];

/// How a Codex lane opens: an app-server per session, under Baaz's own
/// Codex home.
pub struct CodexFactory;

impl ProviderFactory for CodexFactory {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &DESCRIPTOR
    }


    /// Hold the resolved CLI behind the gate with the window's terminal
    /// relay (process-scoped `-c` overrides — the owner's
    /// `~/.codex/config.toml` is never touched), the owner-MCP opt-in and
    /// the Baaz-owned Codex home pinned, and shake hands.
    fn open(&self, spawn: &SessionSpawn) -> Result<Provider, ProviderError> {
        let program = spawn.program.as_ref().ok_or_else(not_installed)?;
        let adapter = CodexAdapter::new(&program.to_string_lossy());
        if let Some(server) = spawn.mcp_servers.first() {
            adapter.set_terminal_relay(TerminalRelay { bridge: server.command.clone(), socket: server.socket.clone() });
        }
        adapter.set_use_own_mcp(spawn.inherit_owner_mcp);
        // Every app-server this adapter spawns runs with
        // `CODEX_HOME=<state>/codex-home`; the rotating `auth.json` link is
        // re-checked before each spawn by the adapter itself. Best-effort.
        if let Some(owner) = spawn.owner_home.as_deref() {
            match crate::home::ensure_home(owner, &spawn.state_dir) {
                Ok(_) => adapter.set_state_dir(spawn.state_dir.clone()),
                Err(error) => eprintln!("baaz: could not prepare the Baaz Codex home: {error}"),
            }
        }
        let mut provider = Provider::new(adapter);
        provider.connect(&spawn.client)?;
        Ok(provider)
    }

    fn homes(&self, state_dir: &Path, owner_home: Option<&Path>) -> Vec<PathBuf> {
        let mut homes = vec![crate::home::home_dir(state_dir)];
        homes.extend(owner_home.map(crate::home::owner_home_dir));
        homes
    }

    fn stored_sessions(&self, home: &Path) -> StoredSessions {
        crate::rollout::stored_rollouts(home)
    }

    fn stored_transcript(&self, raw: &str) -> String {
        crate::rollout::search_text(raw)
    }
}

fn not_installed() -> ProviderError {
    ProviderError::launch_refusal(
        provider::RefusalHint::NotInstalled,
        format!("{} isn't installed", DESCRIPTOR.label),
        format!(
            "{} was not found on PATH or in the usual install locations ({} for an override)",
            DESCRIPTOR.binaries[0], DESCRIPTOR.env_overrides[0]
        ),
    )
}
