//! Claude Code in the registry (docs/82 5.4): what it declares about
//! itself and how its session lanes open. The app reaches this adapter
//! through [`ClaudeCodeFactory`] alone; everything a lane needs before
//! `Command::OpenSession` arrives in a [`SessionSpawn`].

use std::path::{Path, PathBuf};

use provider::{
    ProviderDescriptor, ProviderError, ProviderFactory, ProviderTrait, Provider,
    SessionSpawn, StoredSessions,
};

use crate::{ClaudeCodeAdapter, TerminalRelay};

/// Claude Code's declaration.
pub static DESCRIPTOR: ProviderDescriptor = ProviderDescriptor {
    key: "claude-code",
    label: "Claude Code",
    icon: provider::ProviderId::Claude,
    binaries: &["claude"],
    env_overrides: &["BAAZ_CLAUDE"],
    install_command: Some("curl -fsSL https://claude.ai/install.sh | bash"),
    update_command: Some("claude update"),
    docs_url: "https://docs.anthropic.com/en/docs/claude-code",
    // The version this adapter was probed and verified against (docs/23,
    // docs/65 §3.1 #5): below it the status carries a too-old advisory.
    // The seam's own floor is lower ([`crate::CLAUDE_VERSION_FLOOR`]).
    version_floor: "2.1.276",
    account_label: "claude.ai",
    auth_status_args: Some(&["auth", "status", "--json"]),
    usage_hint: "No reading yet — appears after a Claude Code turn",
    sign_out_note: "This runs `claude auth logout` — Claude Code will be signed out on this Mac, not just in Baaz.",
    sign_out_args: Some(&["auth", "logout"]),
    own_mcp_note: Some(
        "Off: sessions see only Baaz's tools. On: your Claude Code MCP \
            servers and connectors load too. Applies to sessions started after this change.",
    ),
    mcp_listing: Some(crate::terminal::mcp_listing),
    // Read-only denies every tool not annotated `readOnlyHint` on the deny
    // layer, which beats any allow rule: the protocol's sentence holds.
    // Haiku is the utility model the app's one-shot completions run on.
    utility_models: &[("haiku", "Claude Haiku"), ("sonnet", "Claude Sonnet"), ("opus", "Claude Opus")],
    modes: &provider::registry::protocol_modes("Anything without a matching rule is refused.", None),
    // Its own `plan` permission mode (`claude --help` 2.1.276 lists it),
    // switched live over `set_permission_mode`.
    plan: provider::PlanMode::Native,
    routes: &[],
    traits: &[ProviderTrait::ModelsLimitModes],
    capabilities: |_| crate::caps::capabilities(),
    command_env: None,
};

/// The app's registry line for this provider (docs/82 5.7).
pub static REGISTRATION: provider::Registration = provider::Registration {
    descriptor: &DESCRIPTOR,
    factory: |_client| std::sync::Arc::new(ClaudeCodeFactory),
};

/// How a Claude Code lane opens: a child per session, under Baaz's own
/// Claude home.
pub struct ClaudeCodeFactory;

impl ProviderFactory for ClaudeCodeFactory {
    fn descriptor(&self) -> &'static ProviderDescriptor {
        &DESCRIPTOR
    }


    /// Hold the resolved CLI behind the gate with the window's terminal
    /// relay, the owner-MCP opt-in and the Baaz-owned Claude home pinned,
    /// and shake hands. The session opens later with `OpenSession`.
    fn open(&self, spawn: &SessionSpawn) -> Result<Provider, ProviderError> {
        let program = spawn.program.as_ref().ok_or_else(not_installed)?;
        let adapter = ClaudeCodeAdapter::new(&program.to_string_lossy());
        // Per-session configs under the state dir; the operator's own
        // connectors stay out via `--strict-mcp-config` unless opted in.
        if let Some(server) = spawn.mcp_servers.first() {
            adapter.set_terminal_relay(TerminalRelay {
                bridge: server.command.clone(),
                socket: server.socket.clone(),
                config_dir: spawn.state_dir.join("mcp"),
            });
        }
        adapter.set_use_own_mcp(spawn.inherit_owner_mcp);
        // The person's own servers live in their `~/.claude.json`, which
        // Baaz's Claude home does not read: read (never written) at each
        // opted-in launch and passed beside the bridge (docs/82 8.9).
        adapter.set_owner_mcp_source(spawn.owner_home.as_deref().map(|home| home.join(".claude.json")));
        pin_home(&adapter, &spawn.state_dir, spawn.owner_home.as_deref());
        let mut provider = Provider::new(adapter);
        provider.connect(&spawn.client)?;
        Ok(provider)
    }

    fn homes(&self, state_dir: &Path, owner_home: Option<&Path>) -> Vec<PathBuf> {
        let mut homes = vec![crate::home::home_dir(state_dir)];
        homes.extend(owner_home.map(crate::home::owner_config_dir));
        homes
    }

    fn stored_sessions(&self, home: &Path) -> StoredSessions {
        crate::history::stored_transcripts(home)
    }

    fn stored_transcript(&self, raw: &str) -> String {
        crate::history::search_text(raw)
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

/// Ensure the Baaz-owned Claude home and pin it on `adapter`, so its
/// children and its history resolve through `<state>/claude-home` even when
/// this process's own env names another config dir. Best-effort: a failure
/// is logged and the adapter keeps its default resolution (the same path),
/// so the open still proceeds.
fn pin_home(adapter: &ClaudeCodeAdapter, state_dir: &Path, owner_home: Option<&Path>) {
    // Adapter setup runs on the startup path: capture the launched-under-
    // a-desktop-agent fact before any provider thread spawns, so every
    // later per-`Command` scrub replays the launch value.
    provider::child_env::capture_startup_entrypoint();
    let Some(owner) = owner_home else { return };
    match crate::home::ensure_home(owner, state_dir) {
        Ok(home) => adapter.set_config_dir(home),
        Err(error) => eprintln!("baaz: could not prepare the Baaz Claude home: {error}"),
    }
}
