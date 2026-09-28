//! The provider registry: the backends a new session can start on.
//!
//! Three entries — `muse` (existing), `claude-code`, `codex`. Each entry
//! exposes its capability map read live from the adapter crate that owns
//! the evidence (`provider-muse`, `provider-claude-code` and `provider-codex`
//! `caps.rs`): this registry never re-probes and keeps no hand-copied
//! table, so a fixture that upgrades a cell upgrades the UI with it —
//! the drift `docs/20` names is unrepresentable here.
//!
//! # One session, one serving lane
//!
//! Two state machines read one event stream today: the legacy pump in
//! `conn.rs` and [`ProviderCall`](crate::wire::ProviderCall). Wiring a
//! session view to both would give two writers to one on-screen state —
//! random-looking UI corruption no unit test can catch. So a session is
//! served by exactly one of them, decided at session creation from the
//! [`SessionHost`](crate::session::SessionHost)'s provider id and then
//! never changed:
//!
//! * `muse` sessions ride the legacy pump (the transitional bundle, until
//!   the last view moves lane by lane).
//! * `claude-code` and `codex` sessions ride the provider lane: neutral
//!   [`Command`](provider::Command)s through the capability gate.
//!
//! There is deliberately no way to switch a live session's provider: the
//! id lives in a private field on the view with no setter, set once from
//! the host at construction. [`check_single_lane`] is the loud detector
//! for the day both lanes claim one session — it returns the violation as
//! text instead of corrupting silently.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use provider::{Capability, CapabilityState, Provider, ProviderError};

/// Which backend a session belongs to. Fixed at session creation; a live
/// session never changes lanes.
///
/// Serialized kebab-case (`muse`, `claude-code`, `codex`) for the
/// provider-status cache, matching [`ProviderId::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderId {
    /// The existing backend, served by the legacy pump.
    Muse,
    /// Claude Code, served over the provider lane.
    ClaudeCode,
    /// Codex, served over the provider lane.
    Codex,
}

impl ProviderId {
    /// Every entry in the registry, in switcher order.
    pub fn all() -> [ProviderId; 3] {
        [ProviderId::Muse, ProviderId::ClaudeCode, ProviderId::Codex]
    }

    /// The wire id: what the session host and the CLI flag carry.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Muse => "muse",
            ProviderId::ClaudeCode => "claude-code",
            ProviderId::Codex => "codex",
        }
    }

    /// Parse a wire id back. Unknown ids fall back to `muse`, the lane
    /// every existing session already rides — a mistyped flag still opens
    /// a working session rather than nothing.
    pub fn parse(raw: &str) -> ProviderId {
        match raw {
            "claude-code" => ProviderId::ClaudeCode,
            "codex" => ProviderId::Codex,
            _ => ProviderId::Muse,
        }
    }

    /// The switcher label: a human name, never the wire id.
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::Muse => "Muse",
            ProviderId::ClaudeCode => "Claude Code",
            ProviderId::Codex => "Codex",
        }
    }

    /// One honest line per entry, for the switcher subtitle.
    pub fn blurb(self) -> &'static str {
        match self {
            ProviderId::Muse => "The existing backend, full capability set.",
            ProviderId::ClaudeCode => "Steering and interruption are unverified; questions arrive as prose.",
            ProviderId::Codex => "Native steering and interruption; approvals carry the model's reason.",
        }
    }

    /// The composer placeholder for a session on this provider: the person
    /// is asking this provider, so the empty composer names it.
    pub fn composer_placeholder(self) -> String {
        format!("Ask {}, or type / for commands", self.label())
    }

    /// The hero subtitle for a session on this provider in a workspace
    /// called `display`: the session runs on this provider, so the empty
    /// state names it rather than assuming Muse.
    pub fn hero_subtitle(self, display: &str) -> String {
        format!("{} runs in {display}.", self.label())
    }

    /// The mark beside the composer model chip: each provider's own badge,
    /// so a Codex session never wears the Muse "M".
    pub fn icon(self) -> aui_icons::Provider {
        match self {
            ProviderId::Muse => aui_icons::Provider::Muse,
            ProviderId::ClaudeCode => aui_icons::Provider::Claude,
            ProviderId::Codex => aui_icons::Provider::Codex,
        }
    }

    /// The needs-you banner headline: whoever the session runs on is the
    /// one waiting on the person.
    pub fn waiting_headline(self) -> String {
        format!("{} is waiting for you.", self.label())
    }
}

/// The capability map for one registry entry, read from the adapter crate
/// that owns the evidence — never a hand-copied table. A fixture that
/// upgrades a cell there upgrades this answer with it; the human reasons
/// ride along from the source, verbatim.
pub fn capability_state(id: ProviderId, capability: Capability) -> CapabilityState {
    match id {
        ProviderId::Muse => muse_capability_state(muse_table_version(), capability),
        ProviderId::ClaudeCode => {
            provider_claude_code::caps::capabilities().state(capability).clone()
        }
        ProviderId::Codex => provider_codex::caps::capabilities().state(capability).clone(),
    }
}

/// The muse version the registry reads the adapter table at. Baaz never
/// observes the connected muse's version — the handshake's `agent_version`
/// is logged, never stored — so the table reads at the newest floor the
/// seam supports ([`MUSE_MCP_VERSION_FLOOR`](provider_muse::MUSE_MCP_VERSION_FLOOR)):
/// client tools granted, everything else as declared. That is exactly
/// today's answer, now derived instead of repeated.
fn muse_table_version() -> &'static str {
    provider_muse::MUSE_MCP_VERSION_FLOOR
}

/// muse's declared state for one agent version, straight from the owning
/// crate. The versioned entry point the registry's fixed answer above
/// goes through — and what a caller that *does* know its server's version
/// reads instead.
pub fn muse_capability_state(version: &str, capability: Capability) -> CapabilityState {
    provider_muse::capabilities_for_version(version).state(capability).clone()
}

/// What the UI may offer for one capability on one provider: `None` means
/// offered normally; `Some` carries the typed reason the control shows
/// beside its disabled state.
///
/// `Unavailable` refuses (the gate never lets the command through, so the
/// control must not invite the press). `Unverified` is attempted everywhere
/// — the seam never refuses it — so it stays offered but visibly marked:
/// honest ignorance, never quietly collapsed into either neighbour.
pub fn gate(id: ProviderId, capability: Capability) -> Option<String> {
    match capability_state(id, capability) {
        CapabilityState::Native | CapabilityState::Emulated { .. } => None,
        CapabilityState::Unavailable { reason } => Some(reason),
        CapabilityState::Unverified => Some(format!(
            "Unverified on {}: nobody has probed it live, so it is attempted, never refused",
            id.label()
        )),
    }
}

/// Whether this provider's sessions ride the legacy pump. `muse` does
/// until the last view moves; every new provider rides the provider lane
/// from its first session. Which lane serves a session is decided at
/// session creation from the host's provider id and then not changed.
pub fn uses_legacy_pump(id: ProviderId) -> bool {
    matches!(id, ProviderId::Muse)
}

// ------------------------------------------------- finding the CLIs

/// The binary a provider lane spawns: `None` for muse, which rides the
/// legacy pump and never spawns through here.
fn binary_name(id: ProviderId) -> Option<&'static str> {
    match id {
        ProviderId::Muse => None,
        ProviderId::ClaudeCode => Some("claude"),
        ProviderId::Codex => Some("codex"),
    }
}

/// The env override naming the binary, when the provider has one.
fn env_override(id: ProviderId) -> Option<&'static str> {
    match id {
        ProviderId::Muse => None,
        ProviderId::ClaudeCode => Some("BAAZ_CLAUDE"),
        ProviderId::Codex => Some("BAAZ_CODEX"),
    }
}

/// Where the `claude` / `codex` binary comes from, in order: the env
/// override (`BAAZ_CLAUDE` / `BAAZ_CODEX`), else the login-shell `PATH`
/// (which already carries the fixed fallbacks). The login `PATH` matters
/// because the app also launches from the Dock with a minimal `PATH` that
/// names almost nothing.
///
/// `None` for muse (no binary) and when nothing on the search path exists.
pub fn resolve_program(id: ProviderId) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    // The login `PATH` first (it already ends in the install dirs), then
    // the fixed fallbacks again so a bare `PATH` search and this one agree.
    let mut path_dirs: Vec<PathBuf> = std::env::split_paths(provider::env_path::login_path()).collect();
    for dir in fallback_dirs(&home) {
        if !path_dirs.contains(&dir) {
            path_dirs.push(dir);
        }
    }
    let env = env_override(id).and_then(std::env::var_os).map(PathBuf::from);
    resolve_program_with(id, env.as_deref(), &path_dirs, &fallback_dirs(&home))
}

/// The fixed fallbacks in search order: the home installs first, then the
/// two system prefixes. Rooted at `home` so tests can point them at a temp
/// dir instead of the real filesystem.
fn fallback_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".claude/local"),
    ]
}

/// The search behind [`resolve_program`], pure so tests can drive it with a
/// temp dir instead of the real `PATH` and `HOME`: pass an empty fallback
/// list and nothing outside the given dirs can answer.
fn resolve_program_with(
    id: ProviderId,
    env_override: Option<&Path>,
    path_dirs: &[PathBuf],
    fallback_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let binary = binary_name(id)?;
    if let Some(candidate) = env_override.filter(|p| !p.as_os_str().is_empty()) {
        // An explicit override names the binary directly; a directory names
        // the binary inside it. Either way it must exist to count.
        let direct = candidate.to_path_buf();
        if is_executable_file(&direct) {
            return Some(direct);
        }
        let nested = candidate.join(binary);
        if is_executable_file(&nested) {
            return Some(nested);
        }
        return None;
    }
    if let Some(found) = path_dirs.iter().map(|dir| dir.join(binary)).find(|p| is_executable_file(p)) {
        return Some(found);
    }
    fallback_dirs.iter().map(|dir| dir.join(binary)).find(|p| is_executable_file(p))
}

/// An existing file counts as a program. The executable bit is deliberately
/// not checked: the unit tests seed plain files, and a missing bit surfaces
/// as the spawn's own error rather than as "not installed".
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// How [`crate::app::Harness::open_on_provider`] connects: wrapped in a
/// factory so tests can inject a scripted provider instead of spawning a
/// real child. The default spawns nothing on the UI thread — resolving,
/// spawning and shaking hands all block, so the caller runs this on the
/// background executor.
pub(crate) type ProviderFactory = Arc<dyn Fn(ProviderId) -> Result<Provider, ProviderError> + Send + Sync>;

/// The production factory: resolve the CLI, hold its adapter behind the
/// gate, and shake hands. The session itself opens later, with
/// `Command::OpenSession` on the same background turn.
pub(crate) fn default_provider_factory() -> ProviderFactory {
    Arc::new(open_provider)
}

/// This window's terminal socket — what the relay's bridge is pointed at.
/// The same path the app's [`TerminalService`](crate::terminal::TerminalService)
/// serves (or would, when a second window owns the name — then the bridge
/// answers that the terminal is unavailable rather than failing).
pub(crate) fn terminal_socket_path() -> PathBuf {
    crate::terminal::service::socket_path_for(&crate::store::support_dir(), std::process::id())
}

/// Resolve, wrap, and connect one provider lane's child. `Err` when the
/// binary is missing or the handshake fails — the caller surfaces it with
/// the provider's name and opens nothing, never a silent muse fallback.
fn open_provider(id: ProviderId) -> Result<Provider, ProviderError> {
    let program = resolve_program(id).ok_or_else(|| ProviderError::Unavailable {
        reason: match binary_name(id) {
            Some(binary) => format!(
                "{binary} was not found on PATH or in the usual install locations ({} for an override)",
                env_override(id).unwrap_or("BAAZ_CLAUDE")
            ),
            None => "muse sessions ride the legacy pump, never a spawned child".into(),
        },
    })?;
    let program = program.to_string_lossy().into_owned();
    let mut provider = match id {
        ProviderId::ClaudeCode => {
            // The terminal relay's route (T2): the bridge beside this
            // binary, pointed at this window's socket, with per-session
            // configs under the support dir. Per-session only — the
            // operator's own connectors stay out via `--strict-mcp-config`.
            let adapter = provider_claude_code::ClaudeCodeAdapter::new(&program);
            adapter.set_terminal_relay(provider_claude_code::TerminalRelay {
                bridge: crate::terminal::relay::bridge_path(),
                socket: terminal_socket_path(),
                config_dir: crate::terminal::relay::claude_config_dir(
                    &crate::store::support_dir(),
                ),
            });
            Provider::new(adapter)
        }
        ProviderId::Codex => {
            // The terminal relay's route (T2): the bridge as a per-session
            // MCP server through process-scoped `-c` overrides — the
            // owner's `~/.codex/config.toml` is never touched.
            let adapter = provider_codex::CodexAdapter::new(&program);
            adapter.set_terminal_relay(provider_codex::TerminalRelay {
                bridge: crate::terminal::relay::bridge_path(),
                socket: terminal_socket_path(),
            });
            Provider::new(adapter)
        }
        ProviderId::Muse => {
            return Err(ProviderError::Unavailable {
                reason: "muse sessions ride the legacy pump, never a spawned child".into(),
            });
        }
    };
    provider.connect(&crate::conn::connect_info())?;
    Ok(provider)
}

// ------------------------------------------------- the supplied model lists

/// One row of Baaz's supplied Claude Code model list: the `ModelCatalog:
/// Emulated` cell made concrete. `--model` takes aliases and ids but no
/// fixture enumerates them, so Baaz owns this list — and it stays owned
/// here, never upgraded into a probed `Native`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuppliedModel {
    /// What `--model` carries: an alias, never a dated full id.
    pub id: &'static str,
    /// The human label the picker shows; the raw alias never renders.
    pub label: &'static str,
    /// One honest line: what the alias asks for.
    pub detail: &'static str,
}

/// The model a fresh Claude Code session runs on when nothing else says
/// otherwise (V1): the CLI's out-of-box default, matching the supplied
/// list's standing first row. The chip reads this before the first turn
/// so a new session names a model, never the provider — see
/// [`claude_code_seed_model`], which outranks this with the operator's own
/// settings and the last reported model before falling back here.
pub fn claude_code_default_model() -> &'static str {
    "sonnet"
}

/// The file holding the last model a Claude Code init/result frame
/// reported on this machine, inside Baaz's own state dir.
const CLAUDE_CODE_LAST_MODEL_FILE: &str = "claude-code-last-model.json";

/// What the state dir remembers: the last reported Claude Code model.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct LastClaudeCodeModel {
    /// The wire model id, as the frame reported it.
    #[serde(default)]
    model: String,
}

/// Where the last reported Claude Code model lives: one small JSON file in
/// Baaz's own store, beside `provider.json`. Every read is best-effort;
/// every write is atomic.
fn claude_code_last_model_path() -> std::path::PathBuf {
    crate::store::support_dir().join(CLAUDE_CODE_LAST_MODEL_FILE)
}

/// Whether this is a deterministic capture: the last-reported model then
/// reads as nothing and never writes, the same hermeticity rule the
/// provider-session store follows, so a capture never paints the owner's
/// real model into the chip.
fn claude_code_seed_deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// The last model a Claude Code init/result frame reported on this
/// machine, as [`write_claude_code_last_reported_model`] stored it. `None`
/// when nothing was ever reported, the file names no model, or this is a
/// deterministic capture.
pub fn read_claude_code_last_reported_model() -> Option<String> {
    if claude_code_seed_deterministic() {
        return None;
    }
    let stored: LastClaudeCodeModel = crate::store::read_json(&claude_code_last_model_path());
    let model = stored.model.trim().to_owned();
    if model.is_empty() { None } else { Some(model) }
}

/// Remember the last model a Claude Code session reported. Best-effort: a
/// store that cannot be written loses the seed, never the session. A
/// deterministic capture never writes.
pub fn write_claude_code_last_reported_model(model: &str) {
    if claude_code_seed_deterministic() {
        return;
    }
    let model = model.trim();
    if model.is_empty() {
        return;
    }
    if let Ok(text) = serde_json::to_string(&LastClaudeCodeModel { model: model.to_owned() }) {
        let _ = crate::store::write_atomic(&claude_code_last_model_path(), text.as_bytes());
    }
}

/// The `ANTHROPIC_MODEL` seed: the env var the CLI itself honours. Empty
/// or whitespace-only means unset — there is no empty-named model.
pub fn claude_code_env_model() -> Option<String> {
    let model = std::env::var("ANTHROPIC_MODEL").ok()?;
    let model = model.trim().to_owned();
    if model.is_empty() { None } else { Some(model) }
}

/// The `model` key of one `settings.json`-shaped file. Anything unreadable
/// — missing, truncated, unparseable, or a non-string `model` — is `None`,
/// never an error.
fn settings_model_at(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let model = value.get("model")?.as_str()?;
    let model = model.trim().to_owned();
    if model.is_empty() { None } else { Some(model) }
}

/// The `claude` settings seed: `model` in `~/.claude/settings.json`,
/// overridden by `<workspace>/.claude/settings.json` and then by
/// `<workspace>/.claude/settings.local.json`, matching the CLI's own
/// precedence. `HOME` names the home dir (tests point it at a temp dir);
/// `workspace` is the session's workspace root.
pub fn claude_code_settings_model(
    home: Option<&std::path::Path>,
    workspace: Option<&std::path::Path>,
) -> Option<String> {
    let home = home
        .map(|home| home.to_path_buf())
        .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))?;
    let mut seed = settings_model_at(&home.join(".claude").join("settings.json"));
    if let Some(workspace) = workspace {
        if let Some(model) =
            settings_model_at(&workspace.join(".claude").join("settings.json"))
        {
            seed = Some(model);
        }
        if let Some(model) =
            settings_model_at(&workspace.join(".claude").join("settings.local.json"))
        {
            seed = Some(model);
        }
    }
    seed
}

/// The chip's pre-first-turn model for a Claude Code session with no
/// stored pick: `ANTHROPIC_MODEL`, then `claude` settings, then the last
/// model an init/result frame reported on this machine, then `"sonnet"`.
///
/// Display-only: the session's own stored pick (pending, fold, history,
/// catalog) outranks all of this in the view, and none of this reaches
/// the child's argv — a session the person never picked a model for still
/// spawns with no `--model` flag, letting the CLI resolve its own
/// default exactly as before.
pub fn claude_code_seed_model(workspace: Option<&std::path::Path>) -> String {
    if let Some(model) = claude_code_env_model() {
        return model;
    }
    if let Some(model) = claude_code_settings_model(None, workspace) {
        return model;
    }
    if let Some(model) = read_claude_code_last_reported_model() {
        return model;
    }
    claude_code_default_model().to_owned()
}

/// A hermetic `HOME` + `BAAZ_STATE_DIR` for tests that read the seed:
/// temp dirs, no `ANTHROPIC_MODEL`, no deterministic flag. Holding the
/// store's env lock serializes every test that points these variables
/// elsewhere (see `the_last_chosen_provider_survives_a_relaunch`), and
/// `Drop` restores everything and removes the sandbox, so a failure
/// cannot leak one test's seed into the next — or into the owner's real
/// home and store.
#[cfg(test)]
pub(crate) struct TestEnvSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    base: std::path::PathBuf,
    home: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    old_home: Option<std::ffi::OsString>,
    old_state_dir: Option<std::ffi::OsString>,
    old_anthropic: Option<std::ffi::OsString>,
    old_deterministic: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl TestEnvSandbox {
    pub(crate) fn enter(name: &str) -> Self {
        let guard = crate::store::test_env_lock();
        let base = std::env::temp_dir().join(format!(
            "baaz-claude-seed-{}-{name}",
            std::process::id()
        ));
        let home = base.join("home");
        let state_dir = base.join("state");
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&home).expect("temp home");
        std::fs::create_dir_all(&state_dir).expect("temp state dir");
        let sandbox = TestEnvSandbox {
            _guard: guard,
            base,
            home,
            state_dir,
            old_home: std::env::var_os("HOME"),
            old_state_dir: std::env::var_os("BAAZ_STATE_DIR"),
            old_anthropic: std::env::var_os("ANTHROPIC_MODEL"),
            old_deterministic: std::env::var_os("BAAZ_DETERMINISTIC"),
        };
        std::env::set_var("HOME", &sandbox.home);
        std::env::set_var("BAAZ_STATE_DIR", &sandbox.state_dir);
        std::env::remove_var("ANTHROPIC_MODEL");
        std::env::remove_var("BAAZ_DETERMINISTIC");
        sandbox
    }

    pub(crate) fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    pub(crate) fn write_home_settings(&self, model: &str) {
        let dir = self.home.join(".claude");
        std::fs::create_dir_all(&dir).expect("home .claude");
        std::fs::write(dir.join("settings.json"), format!("{{\"model\": \"{model}\"}}"))
            .expect("home settings");
    }

    pub(crate) fn write_workspace_settings(
        &self,
        workspace: &std::path::Path,
        file: &str,
        model: &str,
    ) {
        let dir = workspace.join(".claude");
        std::fs::create_dir_all(&dir).expect("workspace .claude");
        std::fs::write(dir.join(file), format!("{{\"model\": \"{model}\"}}"))
            .expect("workspace settings");
    }
}

#[cfg(test)]
impl Drop for TestEnvSandbox {
    fn drop(&mut self) {
        match &self.old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.old_state_dir {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        match &self.old_anthropic {
            Some(value) => std::env::set_var("ANTHROPIC_MODEL", value),
            None => std::env::remove_var("ANTHROPIC_MODEL"),
        }
        match &self.old_deterministic {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

/// The Claude Code models Baaz offers: aliases `--model` accepts, in
/// picker order. No default is marked — no probe established one — so the
/// current session model marks the active row instead.
pub fn claude_code_models() -> [SuppliedModel; 3] {
    [
        SuppliedModel {
            id: "sonnet",
            label: "Claude Sonnet",
            detail: "The everyday model (--model sonnet; full ids work too)",
        },
        SuppliedModel {
            id: "opus",
            label: "Claude Opus",
            detail: "The largest model (--model opus; full ids work too)",
        },
        SuppliedModel {
            id: "haiku",
            label: "Claude Haiku",
            detail: "The fast model (--model haiku; full ids work too)",
        },
    ]
}

/// Baaz's supplied Claude Code catalog in the seam's neutral shape, with
/// the row matching `current` flagged active. A full dated id still
/// selects (aliases travel) without flagging a row it does not name.
pub fn claude_code_catalog(current: Option<&str>) -> Vec<provider::ModelSummary> {
    claude_code_models()
        .into_iter()
        .map(|row| provider::ModelSummary {
            id: row.id.to_owned(),
            label: row.label.to_owned(),
            active: Some(row.id) == current,
            ..Default::default()
        })
        .collect()
}

/// The human name the composer chip shows for a Claude Code model id the
/// catalog has no row for. The wire reports full ids (`claude-opus-5[1m]`,
/// `claude-opus-5`, dated `claude-haiku-4-5-20251001`) while the supplied
/// menu only names aliases — so without this the chip falls back to the
/// raw id. The menu keeps the id in the row detail; the chip shows this
/// label instead (W8: the chip read `claude-opus-5[1m]`).
///
/// Aliases resolve through the menu's own labels; full ids prettify to
/// family plus dotted version plus context (`claude-opus-5[1m]` →
/// `Opus 5 · 1M`, `claude-haiku-4-5-20251001` → `Haiku 4.5`). A trailing
/// `-YYYYMMDD` date strip is dropped, version dashes join with `.`, and
/// `fable` is known. Anything unrecognised passes through unchanged — an
/// honest raw id, not a mangled guess.
pub fn claude_code_model_label(id: &str) -> String {
    if let Some(row) = claude_code_models().into_iter().find(|row| row.id == id) {
        return row.label.to_owned();
    }
    let body = id.strip_prefix("claude-").unwrap_or(id);
    let (body, context) = match body.strip_suffix(']') {
        Some(inner) => match inner.split_once('[') {
            Some((base, context)) => (base, Some(context)),
            None => (body, None),
        },
        None => (body, None),
    };
    let (family, version) =
        body.split_once('-').map_or((body, ""), |(family, rest)| (family, rest));
    let family_label = match family {
        "opus" => "Opus",
        "sonnet" => "Sonnet",
        "haiku" => "Haiku",
        "fable" => "Fable",
        _ => return id.to_owned(),
    };
    // A dated id ends in `-YYYYMMDD`: the date is a build stamp, not the
    // version, so it goes before the dashes join with dots.
    let mut segments: Vec<&str> = version.split('-').filter(|segment| !segment.is_empty()).collect();
    if let Some(date) = segments.last() {
        if date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit()) {
            segments.pop();
        }
    }
    let mut label = family_label.to_owned();
    if !segments.is_empty() {
        label.push(' ');
        label.push_str(&segments.join("."));
    }
    if let Some(context) = context.filter(|context| !context.is_empty()) {
        label.push_str(" · ");
        label.push_str(&context.to_uppercase());
    }
    label
}

// ------------------------------------------------- the last-chosen provider

/// What the store remembers: the backend new sessions start on.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct LastProvider {
    /// The wire id (`muse`, `claude-code`, `codex`).
    #[serde(default)]
    provider: String,
}

/// Where the last-chosen provider lives: one small JSON file in Baaz's own
/// store, beside the projects file rather than inside it. Every read is
/// best-effort like every other store read; every write is atomic.
fn last_provider_path() -> std::path::PathBuf {
    crate::store::support_dir().join("provider.json")
}

/// The backend new sessions start on, as last chosen. `None` when nothing
/// was ever chosen or the file names no backend — the caller falls back to
/// the command line.
pub fn read_last_provider() -> Option<ProviderId> {
    let stored: LastProvider = crate::store::read_json(&last_provider_path());
    match stored.provider.as_str() {
        "muse" => Some(ProviderId::Muse),
        "claude-code" => Some(ProviderId::ClaudeCode),
        "codex" => Some(ProviderId::Codex),
        _ => None,
    }
}

/// Remember the backend new sessions start on. Best-effort: a store that
/// cannot be written leaves the in-memory pick, which still names every
/// session this run starts.
pub fn write_last_provider(id: ProviderId) {
    let text = serde_json::to_string(&LastProvider { provider: id.as_str().to_owned() });
    if let Ok(text) = text {
        let _ = crate::store::write_atomic(&last_provider_path(), text.as_bytes());
    }
}

/// The loud detector for the two-writers bug: when both lanes claim the
/// same session, return the violation as text (logged and bannered)
/// instead of letting two state machines drive one screen silently.
/// `None` is the only healthy answer.
pub fn check_single_lane(legacy_active: bool, provider_active: bool, session_id: &str) -> Option<String> {
    if legacy_active && provider_active {
        Some(format!(
            "Session {session_id} is served by two lanes at once; the legacy pump and the provider lane must never both drive one screen"
        ))
    } else {
        None
    }
}

// ------------------------------------------------- external approvals

/// Where an approval waiting on the person came from. Claude Code sends
/// one control request (`can_use_tool`); Codex sends five.
///
/// Constructed by the provider lane decoders when they land (today only
/// by tests and fixtures); the surface already renders every variant.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalApprovalKind {
    /// Claude Code `can_use_tool` on the control channel: `tool_name`,
    /// `display_name`, `input`, `tool_use_id`, `permission_suggestions`.
    ClaudeCanUseTool,
    /// Codex `item/commandExecution/requestApproval`.
    CodexCommand,
    /// Codex `item/fileChange/requestApproval`.
    CodexFileChange,
    /// Codex `item/permissions/requestApproval`.
    CodexPermissions,
    /// Codex `item/tool/requestUserInput`.
    CodexUserInput,
    /// Codex `mcpServer/elicitation/request`.
    CodexMcpElicitation,
}

impl ExternalApprovalKind {
    /// The short source tag the card shows beside the headline.
    pub fn tag(self) -> &'static str {
        match self {
            ExternalApprovalKind::ClaudeCanUseTool => "Claude Code tool request",
            ExternalApprovalKind::CodexCommand => "Codex command approval",
            ExternalApprovalKind::CodexFileChange => "Codex file-change approval",
            ExternalApprovalKind::CodexPermissions => "Codex permissions approval",
            ExternalApprovalKind::CodexUserInput => "Codex question",
            ExternalApprovalKind::CodexMcpElicitation => "Codex tool question",
        }
    }
}

/// The person's answer. `decline` ("no, do something else" — the turn
/// continues) and `cancel` ("no, stop" — the turn is interrupted) are
/// different answers and ride as different choices; the card offers both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    /// Run it, once.
    Accept,
    /// Run it and stop asking for the rest of the session.
    AcceptForSession,
    /// Refuse; the turn continues.
    Decline,
    /// Refuse; the turn is interrupted.
    Cancel,
}

impl ApprovalChoice {
    /// Every choice the card offers, in card order.
    pub fn all() -> [ApprovalChoice; 4] {
        [
            ApprovalChoice::Accept,
            ApprovalChoice::AcceptForSession,
            ApprovalChoice::Decline,
            ApprovalChoice::Cancel,
        ]
    }

    /// The decision token the provider lane sends.
    pub fn choice_id(self) -> &'static str {
        match self {
            ApprovalChoice::Accept => "accept",
            ApprovalChoice::AcceptForSession => "acceptForSession",
            ApprovalChoice::Decline => "decline",
            ApprovalChoice::Cancel => "cancel",
        }
    }

    /// The card button label. Decline and Cancel never share one: "no, do
    /// something else" and "no, stop" are different answers.
    pub fn label(self) -> &'static str {
        match self {
            ApprovalChoice::Accept => "Approve",
            ApprovalChoice::AcceptForSession => "Approve for this session",
            ApprovalChoice::Decline => "Deny",
            ApprovalChoice::Cancel => "Deny and stop",
        }
    }
}

/// One approval from a new provider, waiting on the person. Rendered on
/// the existing approvals surface; decided through the provider lane; the
/// card changes only when the server's notification resolves it — never
/// on the press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalApproval {
    /// The provider-side id (`tool_use_id` on Claude Code, `itemId` on Codex).
    pub id: String,
    /// The owning session.
    pub session_id: String,
    /// Which backend asked.
    pub provider: ProviderId,
    /// Which of the six request shapes this is.
    pub kind: ExternalApprovalKind,
    /// One-line human summary: the tool name, the command, the question.
    pub headline: String,
    /// The model-written `reason` sentence (Codex), or the tool input
    /// summary (Claude Code). Shown on the card; never empty.
    pub reason: String,
    /// Claude Code's `permission_suggestions`: the "don't ask again"
    /// affordance (`addRules`/`allow`/`localSettings`). Shown beside the
    /// session-scoped choice; `None` on Codex, which scopes
    /// `acceptForSession` itself.
    pub dont_ask_again: Option<String>,
    /// Opaque per-stage token passed back verbatim with the decision.
    /// Codex answers a single yes/no per approval, so this is `None`
    /// there; a backend that stages requires `Some`.
    pub stage_token: Option<String>,
    /// The choice already sent and awaiting the server's notification.
    /// `Some` means the card shows "sent, waiting" and offers no second
    /// press — the card is never ahead of the server.
    pub decision_sent: Option<String>,
}

impl ExternalApproval {
    /// Still waiting on the person (nothing sent yet).
    pub fn is_pending(&self) -> bool {
        self.decision_sent.is_none()
    }
}

/// The pending external approvals of one session, keyed by provider-side
/// id. Pure state — no widgets, no wire — so the decide-then-wait rule is
/// testable without a window:
///
/// * `decide` records the sent choice and returns the decision token. It
///   never settles the card: the card changes only on `resolve`, which is
///   what the server's notification calls.
/// * a second `decide` while one is in flight is refused (`None`): one
///   press, one command, then wait.
#[derive(Clone, Debug, Default)]
pub struct ExternalApprovalStore {
    approvals: HashMap<String, ExternalApproval>,
}

impl ExternalApprovalStore {
    /// Empty, for a session with nothing waiting.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Park a provider approval request on the surface.
    pub fn inject(&mut self, approval: ExternalApproval) {
        self.approvals.insert(approval.id.clone(), approval);
    }

    /// Look one up, for the card.
    pub fn get(&self, id: &str) -> Option<&ExternalApproval> {
        self.approvals.get(id)
    }

    /// The ones still waiting on the person, oldest first by id.
    pub fn pending(&self) -> Vec<&ExternalApproval> {
        let mut out: Vec<&ExternalApproval> =
            self.approvals.values().filter(|a| a.is_pending()).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Everything the server has not resolved yet: pending presses plus
    /// sent decisions still waiting. Both render; only the former offers
    /// buttons — a sent card shows "waiting for the server" instead.
    pub fn outstanding(&self) -> Vec<&ExternalApproval> {
        let mut out: Vec<&ExternalApproval> = self.approvals.values().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Record the press: returns the `(choice_id, stage_token)` the
    /// provider lane sends. The card stays pending — the server's
    /// notification settles it through [`Self::resolve`]. A repeat press
    /// while one is in flight, or an unknown id, returns `None` and sends
    /// nothing twice.
    pub fn decide(&mut self, id: &str, choice: ApprovalChoice) -> Option<(String, Option<String>)> {
        let approval = self.approvals.get_mut(id)?;
        if approval.decision_sent.is_some() {
            return None;
        }
        let token = (choice.choice_id().to_owned(), approval.stage_token.clone());
        approval.decision_sent = Some(choice.choice_id().to_owned());
        Some(token)
    }

    /// Settle the card from the server's notification. Returns whether
    /// anything was waiting under that id.
    pub fn resolve(&mut self, id: &str) -> bool {
        self.approvals.remove(id).is_some()
    }

    /// Re-park a card whose decision never landed (the lane refused it):
    /// clear the in-flight mark so the press can be tried again. The card
    /// never settled — settling is the server's job alone.
    pub fn repark(&mut self, id: &str) {
        if let Some(approval) = self.approvals.get_mut(id) {
            approval.decision_sent = None;
        }
    }

    /// Whether anything the server has not resolved yet is on the card —
    /// a press sent and still waiting counts, because the card has not
    /// moved. Read by the screenshot capture's pending-approval answer.
    pub fn has_pending(&self) -> bool {
        !self.approvals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// W4: the UI table is the adapter table — for all three providers.
    /// A fixture that upgrades a cell in any adapter crate fails here
    /// until the UI reads it, which it does by construction now: there is
    /// no hand-copied table left to drift.
    #[test]
    fn the_registry_table_equals_the_adapter_tables() {
        for capability in provider::Capability::all() {
            assert_eq!(
                capability_state(ProviderId::ClaudeCode, capability),
                *provider_claude_code::caps::capabilities().state(capability),
                "claude-code {:?} drifted from its adapter",
                capability
            );
            assert_eq!(
                capability_state(ProviderId::Codex, capability),
                *provider_codex::caps::capabilities().state(capability),
                "codex {:?} drifted from its adapter",
                capability
            );
            assert_eq!(
                capability_state(ProviderId::Muse, capability),
                *provider_muse::capabilities_for_version(muse_table_version()).state(capability),
                "muse {:?} drifted from its adapter",
                capability
            );
        }
    }

    #[test]
    fn three_entries_in_the_registry() {
        assert_eq!(ProviderId::all().len(), 3);
        let ids: Vec<&str> = ProviderId::all().iter().map(|p| p.as_str()).collect();
        assert_eq!(ids, vec!["muse", "claude-code", "codex"]);
    }

    #[test]
    fn labels_are_human_names_not_wire_ids() {
        for id in ProviderId::all() {
            assert!(!id.label().contains('-') || id == ProviderId::ClaudeCode);
            assert_ne!(id.label(), id.as_str());
        }
    }

    /// W8, defect 6: the chip humanises full Claude Code model ids the
    /// alias menu never lists — `claude-opus-5[1m]` reads `Opus 5 · 1M`,
    /// never the raw id — while aliases keep the menu's own labels and
    /// unknown ids pass through unmangled.
    #[test]
    fn claude_code_chip_names_full_model_ids() {
        assert_eq!(claude_code_model_label("claude-opus-5[1m]"), "Opus 5 · 1M");
        assert_eq!(claude_code_model_label("claude-opus-5"), "Opus 5");
        assert_eq!(claude_code_model_label("claude-sonnet-4-5"), "Sonnet 4.5");
        assert_eq!(claude_code_model_label("claude-haiku-4-5"), "Haiku 4.5");
        // Dated ids drop the build stamp and join the version with dots;
        // fable is a known family; the 1M context suffix is kept.
        assert_eq!(claude_code_model_label("claude-haiku-4-5-20251001"), "Haiku 4.5");
        assert_eq!(claude_code_model_label("claude-fable-5-1[1m]"), "Fable 5.1 · 1M");
        assert_eq!(claude_code_model_label("fable"), "Fable");
        assert_eq!(claude_code_model_label("opus"), "Claude Opus");
        assert_eq!(claude_code_model_label("sonnet"), "Claude Sonnet");
        assert_eq!(claude_code_model_label("haiku"), "Claude Haiku");
        assert_eq!(claude_code_model_label("future-model-9"), "future-model-9");
        assert_eq!(claude_code_model_label("claude-unknownthing"), "claude-unknownthing");
    }

    #[test]
    fn claude_code_seed_prefers_env_over_everything() {
        let sandbox = TestEnvSandbox::enter("env");
        sandbox.write_home_settings("haiku");
        write_claude_code_last_reported_model("claude-opus-5[1m]");
        std::env::set_var("ANTHROPIC_MODEL", "opus");
        assert_eq!(claude_code_seed_model(None), "opus");
        // Whitespace-only env is unset — there is no empty-named model —
        // so the next rung answers instead.
        std::env::set_var("ANTHROPIC_MODEL", "   ");
        assert_eq!(claude_code_seed_model(None), "haiku");
    }

    #[test]
    fn claude_code_seed_reads_settings_with_workspace_precedence() {
        let sandbox = TestEnvSandbox::enter("settings");
        // Nothing anywhere: the out-of-box default, as today.
        assert_eq!(claude_code_seed_model(None), "sonnet");
        // The global file seeds the chip.
        sandbox.write_home_settings("opus");
        assert_eq!(claude_code_seed_model(None), "opus");
        // The workspace file overrides the global one, and the local
        // file overrides both — the CLI's own precedence.
        let workspace = sandbox.state_dir().join("work");
        std::fs::create_dir_all(&workspace).expect("workspace");
        sandbox.write_workspace_settings(&workspace, "settings.json", "haiku");
        assert_eq!(claude_code_seed_model(Some(&workspace)), "haiku");
        // A session elsewhere still reads the global seed.
        assert_eq!(claude_code_seed_model(None), "opus");
        sandbox.write_workspace_settings(&workspace, "settings.local.json", "sonnet");
        assert_eq!(claude_code_seed_model(Some(&workspace)), "sonnet");
    }

    #[test]
    fn claude_code_seed_falls_back_to_the_last_reported_model() {
        // Named for its `Drop`: nothing is read through it, but it owns
        // the temp HOME / state dir and the env lock while this runs.
        let _sandbox = TestEnvSandbox::enter("last");
        assert_eq!(read_claude_code_last_reported_model(), None);
        write_claude_code_last_reported_model("claude-opus-5[1m]");
        assert_eq!(
            read_claude_code_last_reported_model().as_deref(),
            Some("claude-opus-5[1m]")
        );
        assert_eq!(claude_code_seed_model(None), "claude-opus-5[1m]");
        assert_eq!(claude_code_model_label(&claude_code_seed_model(None)), "Opus 5 · 1M");
        // The hermeticity rule the other stores follow: a deterministic
        // capture neither reads nor writes the owner's last model.
        std::env::set_var("BAAZ_DETERMINISTIC", "1");
        assert_eq!(read_claude_code_last_reported_model(), None);
        assert_eq!(claude_code_seed_model(None), "sonnet");
        write_claude_code_last_reported_model("opus");
        std::env::remove_var("BAAZ_DETERMINISTIC");
        assert_eq!(
            read_claude_code_last_reported_model().as_deref(),
            Some("claude-opus-5[1m]"),
            "the capture wrote nothing over the owner's seed"
        );
    }

    #[test]
    fn session_chrome_names_the_session_provider() {
        // The hero subtitle, composer placeholder, chip badge and
        // needs-you headline all derive from the session's provider: a
        // future hardcode of any one of them fails here, on every provider.
        for id in ProviderId::all() {
            let label = id.label();
            assert_eq!(id.hero_subtitle("harness"), format!("{label} runs in harness."));
            assert_eq!(
                id.composer_placeholder(),
                format!("Ask {label}, or type / for commands")
            );
            assert_eq!(id.waiting_headline(), format!("{label} is waiting for you."));
        }
        assert_eq!(ProviderId::Muse.icon(), aui_icons::Provider::Muse);
        assert_eq!(ProviderId::ClaudeCode.icon(), aui_icons::Provider::Claude);
        assert_eq!(ProviderId::Codex.icon(), aui_icons::Provider::Codex);
        // No two providers share a badge: the chip mark always identifies
        // the session's lane.
        let icons: Vec<_> = ProviderId::all().iter().map(|p| p.icon()).collect();
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[0], icons[2]);
        assert_ne!(icons[1], icons[2]);
    }

    #[test]
    fn unknown_ids_open_a_working_muse_session() {
        assert_eq!(ProviderId::parse(""), ProviderId::Muse);
        assert_eq!(ProviderId::parse("echo"), ProviderId::Muse);
        assert_eq!(ProviderId::parse("claude-code"), ProviderId::ClaudeCode);
        assert_eq!(ProviderId::parse("codex"), ProviderId::Codex);
    }

    #[test]
    fn the_last_chosen_provider_survives_a_relaunch() {
        // Serialized against every other test that points the store at a
        // temp dir: two tests pointing it at two dirs at once would read
        // each other's state.
        let guard = crate::store::test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        // Nothing chosen yet: no pick, so the boot falls back to the
        // command line rather than inventing one.
        assert_eq!(read_last_provider(), None);
        write_last_provider(ProviderId::Codex);
        assert_eq!(read_last_provider(), Some(ProviderId::Codex));
        write_last_provider(ProviderId::Muse);
        assert_eq!(read_last_provider(), Some(ProviderId::Muse));
        // A file this build cannot parse is ordinary, not a pick.
        std::fs::write(dir.join("provider.json"), b"not json").expect("seed bad json");
        assert_eq!(read_last_provider(), None);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_screen_differs_between_providers() {
        // SteerTurn: Unverified on Claude Code (marked, attempted),
        // Native on Codex (offered plainly). The point-of-use gates read
        // this table, so the same control cannot offer the same on both.
        assert!(gate(ProviderId::ClaudeCode, Capability::SteerTurn).is_some());
        assert!(gate(ProviderId::Codex, Capability::SteerTurn).is_none());
        assert!(gate(ProviderId::ClaudeCode, Capability::TurnControl).is_some());
        assert!(gate(ProviderId::Codex, Capability::TurnControl).is_none());
        // Questions: Unavailable on Claude Code (refused, with the prose
        // reason), Unverified on Codex (attempted, marked) — different
        // states, different text, never averaged into one.
        let claude = gate(ProviderId::ClaudeCode, Capability::Questions).expect("gated");
        let codex = gate(ProviderId::Codex, Capability::Questions).expect("gated");
        assert_ne!(claude, codex);
        assert!(claude.contains("prose"));
    }

    #[test]
    fn unverified_is_marked_never_collapsed() {
        // Unverified keeps the typed marker (attempted, not refused);
        // Unavailable carries the provider's own reason.
        let marked = gate(ProviderId::ClaudeCode, Capability::SteerTurn).unwrap();
        assert!(marked.contains("Unverified"));
        let refused = gate(ProviderId::ClaudeCode, Capability::Questions).unwrap();
        assert!(!refused.contains("Unverified"));
    }

    #[test]
    fn muse_keeps_the_legacy_pump_and_new_providers_do_not() {
        assert!(uses_legacy_pump(ProviderId::Muse));
        assert!(!uses_legacy_pump(ProviderId::ClaudeCode));
        assert!(!uses_legacy_pump(ProviderId::Codex));
    }

    #[test]
    fn both_lanes_at_once_is_loud_not_silent() {
        assert!(check_single_lane(false, false, "s").is_none());
        assert!(check_single_lane(true, false, "s").is_none());
        assert!(check_single_lane(false, true, "s").is_none());
        let violation = check_single_lane(true, true, "s").expect("loud");
        assert!(violation.contains('s'));
    }

    #[test]
    fn decline_and_cancel_are_different_answers() {
        assert_ne!(ApprovalChoice::Decline.choice_id(), ApprovalChoice::Cancel.choice_id());
        assert_ne!(ApprovalChoice::Decline.label(), ApprovalChoice::Cancel.label());
        assert_eq!(ApprovalChoice::Decline.choice_id(), "decline");
        assert_eq!(ApprovalChoice::Cancel.choice_id(), "cancel");
        let ids: Vec<&str> =
            ApprovalChoice::all().iter().map(|c| c.choice_id()).collect();
        assert_eq!(ids, vec!["accept", "acceptForSession", "decline", "cancel"]);
    }

    fn sample(kind: ExternalApprovalKind) -> ExternalApproval {
        ExternalApproval {
            id: "appr-1".into(),
            session_id: "s-1".into(),
            provider: ProviderId::Codex,
            kind,
            headline: "Allow creating /tmp/probe.txt?".into(),
            reason: "Allow creating /tmp/probe.txt containing HELLO as requested?".into(),
            dont_ask_again: None,
            stage_token: None,
            decision_sent: None,
        }
    }

    #[test]
    fn the_card_is_never_ahead_of_the_server() {
        let mut store = ExternalApprovalStore::empty();
        store.inject(sample(ExternalApprovalKind::CodexCommand));
        assert!(store.has_pending());
        // The press sends exactly one decision and the card stays pending.
        let sent = store.decide("appr-1", ApprovalChoice::Decline).expect("sent");
        assert_eq!(sent.0, "decline");
        assert!(store.has_pending(), "a sent decision still waits on the server");
        // A repeat press sends nothing twice.
        assert!(store.decide("appr-1", ApprovalChoice::Cancel).is_none());
        // Only the server's notification settles the card.
        assert!(store.resolve("appr-1"));
        assert!(!store.has_pending());
    }

    /// Seed `dir` with a fake binary and resolve against it alone: no
    /// real `PATH` or `HOME` is read.
    fn seeded_binary(dir: &std::path::Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"fake").expect("seed a fake binary");
        path
    }

    /// No `PATH` entries and no fallbacks: nothing outside the dirs the
    /// test names can answer, on any machine this runs on.
    fn empty_search() -> (Vec<PathBuf>, Vec<PathBuf>) {
        (Vec::new(), Vec::new())
    }

    #[test]
    fn muse_has_no_binary_to_resolve() {
        let (paths, fallbacks) = empty_search();
        assert_eq!(resolve_program_with(ProviderId::Muse, None, &paths, &fallbacks), None);
    }

    #[test]
    fn path_lookup_finds_the_binary() {
        let dir = std::env::temp_dir().join(format!("baaz-resolve-path-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let claude = seeded_binary(&dir, "claude");
        let (_, fallbacks) = empty_search();
        assert_eq!(
            resolve_program_with(ProviderId::ClaudeCode, None, std::slice::from_ref(&dir), &fallbacks),
            Some(claude)
        );
        // `codex` was never seeded, so only `claude` resolves here — and
        // the empty fallback list keeps the real install locations out of
        // the answer on any machine this runs on.
        assert_eq!(
            resolve_program_with(ProviderId::Codex, None, std::slice::from_ref(&dir), &fallbacks),
            None
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_env_override_wins_over_path() {
        let path_dir = std::env::temp_dir().join(format!("baaz-resolve-env-path-{}", std::process::id()));
        let env_dir = std::env::temp_dir().join(format!("baaz-resolve-env-over-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path_dir);
        let _ = std::fs::remove_dir_all(&env_dir);
        std::fs::create_dir_all(&path_dir).expect("temp dir");
        std::fs::create_dir_all(&env_dir).expect("temp dir");
        seeded_binary(&path_dir, "codex");
        let preferred = seeded_binary(&env_dir, "codex");
        let (_, fallbacks) = empty_search();
        // A directory override names the binary inside it.
        assert_eq!(
            resolve_program_with(
                ProviderId::Codex,
                Some(&env_dir),
                std::slice::from_ref(&path_dir),
                &fallbacks
            ),
            Some(preferred)
        );
        // A missing override is a miss, not a fallthrough to PATH: an
        // explicit but wrong path must not silently resolve elsewhere.
        let missing = env_dir.join("no-such-dir");
        assert_eq!(
            resolve_program_with(ProviderId::Codex, Some(&missing), &[], &fallbacks),
            None
        );
        let _ = std::fs::remove_dir_all(&path_dir);
        let _ = std::fs::remove_dir_all(&env_dir);
    }

    #[test]
    fn dock_fallbacks_cover_a_minimal_path() {
        let home = std::env::temp_dir().join(format!("baaz-resolve-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).expect("temp dir");
        let claude = seeded_binary(&local_bin, "claude");
        let claude_local = home.join(".claude/local");
        std::fs::create_dir_all(&claude_local).expect("temp dir");
        seeded_binary(&claude_local, "claude");
        // No PATH entries at all, as from the Dock: the fallbacks still
        // find it, `~/.local/bin` ahead of `~/.claude/local`.
        assert_eq!(
            resolve_program_with(ProviderId::ClaudeCode, None, &[], &fallback_dirs(&home)),
            Some(claude)
        );
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn all_six_request_shapes_reach_the_store() {
        let mut store = ExternalApprovalStore::empty();
        let kinds = [
            ExternalApprovalKind::ClaudeCanUseTool,
            ExternalApprovalKind::CodexCommand,
            ExternalApprovalKind::CodexFileChange,
            ExternalApprovalKind::CodexPermissions,
            ExternalApprovalKind::CodexUserInput,
            ExternalApprovalKind::CodexMcpElicitation,
        ];
        for (i, kind) in kinds.into_iter().enumerate() {
            let mut approval = sample(kind);
            approval.id = format!("appr-{i}");
            store.inject(approval);
        }
        assert_eq!(store.pending().len(), 6);
    }
}
