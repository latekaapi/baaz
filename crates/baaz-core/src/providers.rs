//! The provider registry: the backends a new session can start on
//! (docs/82 5.4, docs/81 B §c.1).
//!
//! Each backend is an adapter crate's registration: a
//! [`ProviderDescriptor`](provider::ProviderDescriptor) declaring the facts
//! (label, binaries, install and update commands, docs, version floor,
//! modes, and the traits that name what it offers beyond the seam) and a
//! [`ProviderFactory`](provider::ProviderFactory) that opens its lanes.
//! [`ProviderId`] is the app's handle on one registration; this module is
//! the one place that names the adapter crates' registrations (its
//! `REGISTRATIONS` table: one line per provider — ACP joined with one,
//! docs/82 5.7), and the rest of the app asks the registry — a trait, a
//! label, a factory — never "is this provider X?". A provider the person
//! configures (ACP) is registered always and offered only once configured
//! ([`ProviderId::all`]).
//!
//! Capability maps are read live from the adapter that owns the evidence
//! (its descriptor's `capabilities`): this registry never re-probes and
//! keeps no hand-copied table, so a fixture that upgrades a cell upgrades
//! the UI with it.
//!
//! # One session, one serving lane
//!
//! A session is served by exactly one lane, decided at session creation
//! from the app's `session::SessionHost`'s provider id and then never
//! changed: there is deliberately no way to switch a live session's
//! provider — the id lives in a private field on the view with no setter,
//! set once from the host at construction. A provider whose sessions share
//! one child ([`provider::ProviderTrait::SharedChild`]) hands out lanes on
//! it; the others spawn a child per lane.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use provider::{
    Capability, CapabilityState, Provider, ProviderDescriptor, ProviderError, ProviderRegistry, ProviderTrait,
    SharedChild, UnknownProvider,
};

/// Every adapter this build registers, in **the one provider order**: the
/// one place the app names an adapter crate's registration (docs/82 5.4,
/// 5.7). A provider joins with one line here — nothing else in the app
/// names it.
///
/// This table's order is the order every list of providers shows —
/// the first-run Connect rows, Settings › Providers, the "New sessions
/// start in" menus, the composer's provider menu, the account menu's usage
/// bars, Continue in…, the import offers (docs/82 8.7, docs/81 F A5.3):
/// each reads [`ProviderId::all`], and none keeps an order of its own. The
/// per-lane CLIs first (Claude Code, Codex), the shared-child provider
/// after them, a provider the person configures last.
static REGISTRATIONS: &[&provider::Registration] = &[
    &provider_claude_code::registry::REGISTRATION,
    &provider_codex::registry::REGISTRATION,
    &provider_muse::registry::REGISTRATION,
    &provider_acp::registry::REGISTRATION,
];

/// Which backend a session belongs to: the app's handle on one registry
/// entry, by its key. Fixed at session creation; a live session never
/// changes lanes.
///
/// Serialized as the descriptor's key (`muse`, `claude-code`, `codex`,
/// `acp`) for the provider-status cache ([`ProviderId::as_str`]); reading
/// one back accepts only a registered key. Every value names a
/// registration: the field is private, and the only ways to one are the
/// registry itself and the named handles below.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProviderId(&'static str);

/// The registrations the app's own code still names: Muse's shared child,
/// Claude Code's and Codex's per-CLI details. A registration nothing in
/// the app names (ACP) has no handle here — the app reaches it through the
/// registry alone.
#[allow(non_upper_case_globals)]
impl ProviderId {
    /// The `provider-muse` registration.
    pub const Muse: ProviderId = ProviderId("muse");
    /// The `provider-claude-code` registration.
    pub const ClaudeCode: ProviderId = ProviderId("claude-code");
    /// The `provider-codex` registration.
    pub const Codex: ProviderId = ProviderId("codex");
}

impl std::fmt::Debug for ProviderId {
    /// The key in the old enum's spelling (`ClaudeCode`), so a logged or
    /// asserted id reads as it always did.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let camel: String = self
            .0
            .split('-')
            .map(|word| {
                let mut chars = word.chars();
                chars.next().map(|first| first.to_uppercase().chain(chars).collect::<String>()).unwrap_or_default()
            })
            .collect();
        f.write_str(&camel)
    }
}

impl serde::Serialize for ProviderId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0)
    }
}

impl<'de> serde::Deserialize<'de> for ProviderId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        ProviderId::parse_known(&raw).ok_or_else(|| serde::de::Error::custom(UnknownProvider(raw)))
    }
}

impl ProviderId {
    /// The declaration each handle stands for: its registration's.
    pub fn descriptor(self) -> &'static ProviderDescriptor {
        REGISTRATIONS
            .iter()
            .find(|registration| registration.descriptor.key == self.0)
            .map(|registration| registration.descriptor)
            .expect("every ProviderId names a registration")
    }

    /// Every registration, offered or not: what a stored record or a flag
    /// is read against.
    pub fn registered() -> impl Iterator<Item = ProviderId> {
        REGISTRATIONS.iter().map(|registration| ProviderId(registration.descriptor.key))
    }

    /// Every provider the app offers, in the one provider order
    /// (`REGISTRATIONS`): each registration whose descriptor says it is
    /// offered — one the person configures
    /// ([`provider::ProviderDescriptor::command_env`]) only once it is
    /// configured (rule 13). Every list of providers reads this; none sorts
    /// it again (docs/82 8.7).
    pub fn all() -> Vec<ProviderId> {
        ProviderId::registered().filter(|id| id.descriptor().command_env.is_none() || configured_command(*id).is_some()).collect()
    }

    /// The first provider in the one order: the fallback where one is
    /// wanted and nothing is known (never a provider for being itself —
    /// docs/50 P1).
    pub fn first() -> ProviderId {
        ProviderId::all().into_iter().next().expect("a provider is always offered")
    }

    /// Whether this provider declares `trait_`.
    pub fn has(self, trait_: ProviderTrait) -> bool {
        self.descriptor().has(trait_)
    }

    /// The first registered provider declaring `trait_`: how the app
    /// reaches "the provider with a billing tier" without naming it.
    pub fn with(trait_: ProviderTrait) -> Option<ProviderId> {
        ProviderId::all().into_iter().find(|id| id.has(trait_))
    }

    /// The provider whose sessions share one child the window keeps
    /// ([`ProviderTrait::SharedChild`]): whose account, usage and start the
    /// connection-level code follows. The first provider when none declares
    /// it.
    pub fn shared_child() -> ProviderId {
        ProviderId::with(ProviderTrait::SharedChild).unwrap_or_else(ProviderId::first)
    }

    /// The provider that keeps its own session list
    /// ([`ProviderTrait::NativeSessionList`]): the one whose listing,
    /// index facts, captures and local drafts become sidebar rows. The first
    /// provider when none declares it.
    pub fn native_lister() -> ProviderId {
        ProviderId::with(ProviderTrait::NativeSessionList).unwrap_or_else(ProviderId::first)
    }

    /// The wire id: what the session host, records and the CLI flag carry.
    pub fn as_str(self) -> &'static str {
        self.descriptor().key
    }

    /// The provider `raw` names — a registered key, or a model-vendor
    /// route a provider declares (`--provider echo`). Anything else is an
    /// error, never a fallback to some provider: a record written by a
    /// build that knows a provider this one does not stays unopened.
    pub fn resolve(raw: &str) -> Result<ProviderId, UnknownProvider> {
        ProviderId::registered()
            .find(|id| id.descriptor().answers_to(raw))
            .ok_or_else(|| UnknownProvider(raw.to_owned()))
    }

    /// Parse a stored key strictly: `None` for a key no registry entry
    /// owns (a route is not a stored key). What a stored record's provider
    /// is read with.
    pub fn parse_known(raw: &str) -> Option<ProviderId> {
        ProviderId::registered().find(|id| id.as_str() == raw)
    }

    /// The key this provider's per-project model/effort defaults live under
    /// in [`crate::projects::ProjectDefaults`]: the canonical wire id, so
    /// every spelling of one backend reads the one stored default and a
    /// pick on one lane never becomes another lane's start model.
    pub fn defaults_key(self) -> &'static str {
        self.as_str()
    }

    /// The switcher label: a human name, never the wire id.
    pub fn label(self) -> &'static str {
        self.descriptor().label
    }

    /// The composer placeholder for a session on this provider: the person
    /// is asking this provider, so the empty composer names it.
    pub fn composer_placeholder(self) -> String {
        format!("Ask {}, or type / for commands", self.label())
    }

    /// The needs-you banner headline: whoever the session runs on is the
    /// one waiting on the person.
    pub fn waiting_headline(self) -> String {
        format!("{} is waiting for you.", self.label())
    }
}

/// A recording some registered provider reads, folded with no child: the
/// provider whose own recording it is, and the fold — what `--replay`
/// opens (docs/82 5.7). `None` when no registered provider reads it.
pub fn replay(recording: &str) -> Option<(ProviderId, provider::Replay)> {
    let client = crate::conn::connect_info();
    REGISTRATIONS.iter().find_map(|registration| {
        let replay = (registration.factory)(&client).replay(recording)?;
        Some((ProviderId(registration.descriptor.key), replay))
    })
}

/// What app copy calls an agent when no one provider is meant: no
/// session, a tab no session owns, a sentence about every provider
/// (docs/82 5.6, standing rule 14: no provider name in app copy except on
/// that provider's own row). Lower case, for mid-sentence use.
pub const THE_AGENT: &str = "the agent";

/// The label of the provider whose sessions share one child, for copy
/// about that connection (starting it, losing it, signing in over it):
/// its own words, read from its descriptor. "The agent" when no provider
/// shares a child.
pub fn shared_label() -> &'static str {
    Registry::shared_id().map_or("The agent", ProviderId::label)
}

/// The capability map for one registry entry, read from the adapter crate
/// that owns the evidence — never a hand-copied table. A fixture that
/// upgrades a cell there upgrades this answer with it; the human reasons
/// ride along from the source, verbatim. Read at the declared floor: Baaz
/// does not keep the connected CLI's version, so a provider whose table
/// varies by version answers at the newest floor the seam supports.
pub fn capability_state(id: ProviderId, capability: Capability) -> CapabilityState {
    (id.descriptor().capabilities)(None).state(capability).clone()
}

/// Whether a control that sends a command needing `capability` is offered
/// on `id`'s sessions: the one test every such control asks (docs/82 0.7).
/// True only for `Native` and `Emulated`, exactly when the seam attempts it.
pub fn offers(id: ProviderId, capability: Capability) -> bool {
    capability_state(id, capability).allows_attempt()
}

/// What a control says when its provider cannot do it.
pub fn not_available(id: ProviderId) -> String {
    format!("Not available in {}.", id.label())
}

/// What the UI may offer for one capability on one provider: `None` means
/// offered normally; `Some` is the line a refused control shows instead
/// ([`not_available`]). The adapter's own reason stays in its table and the
/// seam's refusal, for logs — it is written for whoever maintains the
/// adapter, not for the person at the composer.
pub fn gate(id: ProviderId, capability: Capability) -> Option<String> {
    (!offers(id, capability)).then(|| not_available(id))
}

/// How a person gets, updates and reads about each provider's CLI — one
/// table for the connect screen and Settings (docs/65 §3.1 #9), declared by
/// each adapter. Commands are typed into a terminal for the person to run,
/// never run by Baaz.
impl ProviderId {
    /// The vendor install command; `None` when no installer is published,
    /// so the missing row links to [`Self::docs_url`] instead.
    pub fn install_command(self) -> Option<&'static str> {
        self.descriptor().install_command
    }

    /// The CLI's own update command, offered beside a too-old advisory.
    pub fn update_command(self) -> Option<&'static str> {
        self.descriptor().update_command
    }

    /// The oldest CLI Baaz supports, as data: below it the status carries a
    /// too-old advisory (never a block).
    pub fn version_floor(self) -> &'static str {
        self.descriptor().version_floor
    }

    /// The vendor docs behind every Docs button.
    pub fn docs_url(self) -> &'static str {
        self.descriptor().docs_url
    }
}

// ------------------------------------------------- finding the CLIs

/// The binary a lane spawns: the descriptor's first; `None` for a provider
/// whose sessions share one child the window starts instead.
fn binary_name(id: ProviderId) -> Option<&'static str> {
    if id.has(ProviderTrait::SharedChild) {
        return None;
    }
    id.descriptor().binaries.first().copied()
}

/// The env override naming the binary, when the provider has one and its
/// lanes spawn through [`resolve_program`].
fn env_override(id: ProviderId) -> Option<&'static str> {
    binary_name(id).and_then(|_| id.descriptor().env_overrides.first().copied())
}

/// Where a per-lane CLI's binary comes from, in order: the env override
/// (`BAAZ_CLAUDE` / `BAAZ_CODEX`), else the login-shell `PATH` (which
/// already carries the fixed fallbacks). The login `PATH` matters because
/// the app also launches from the Dock with a minimal `PATH` that names
/// almost nothing.
///
/// `None` for a shared-child provider (no binary per lane) and when nothing
/// on the search path exists.
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
    // A provider the person configures runs the program its command names
    // (docs/82 5.7), and nothing when it names none.
    if id.descriptor().command_env.is_some() {
        let program = configured_command(id)?.into_iter().next()?;
        return resolve_configured(&program, &path_dirs);
    }
    // The env override wins; then the path the person set in Settings
    // (D5 B4); either names the binary outright — a wrong one reads Not
    // installed rather than silently falling back to another.
    let env = env_override(id)
        .and_then(std::env::var_os)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::details::path_override(id));
    resolve_program_with(id, env.as_deref(), &path_dirs, &fallback_dirs(&home))
}

/// The command line a provider the person configures runs
/// ([`provider::ProviderDescriptor::command_env`]), when one is set. Under
/// test only a stand-in under the temp dir counts: a developer's own
/// configured agent never reaches a test (standing rule 4).
fn configured_command(id: ProviderId) -> Option<Vec<String>> {
    let words = id.descriptor().configured_command()?;
    #[cfg(any(test, feature = "test-support"))]
    if !crate::skills::is_stand_in(&words[0]) {
        return None;
    }
    Some(words)
}

/// A configured command's program: a path as written (it must exist), a
/// bare name on the search path. Pure, so tests drive it with a temp dir.
fn resolve_configured(program: &str, path_dirs: &[PathBuf]) -> Option<PathBuf> {
    if program.contains('/') {
        let path = PathBuf::from(program);
        return is_executable_file(&path).then_some(path);
    }
    path_dirs.iter().map(|dir| dir.join(program)).find(|candidate| is_executable_file(candidate))
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

/// The window's registry: every adapter's factory as registered, and how
/// this run opens lanes through them (the runtime's
/// [`crate::runtime::ProviderSource`] made concrete for one window). Every
/// open — new, reopen, fork, a disabled provider's lane — goes through
/// one, so a scripted run takes the same open path as a real one with the
/// scripted stand-in registered in front, and tests register theirs the
/// same way.
///
/// The open itself blocks — resolving, spawning and shaking hands — so the
/// open path runs it on the background executor; an open that
/// [lands inline](Self::lands_inline) (the scripted stand-in: no child,
/// nothing that blocks) runs on the spot instead, so a scripted open lands
/// in the same update as its click.
#[derive(Clone)]
pub struct Registry {
    /// The adapters' factories. The shared-child provider's factory is also
    /// its child's slot, so it lives here for the window's life whatever
    /// opens lanes.
    providers: ProviderRegistry,
    opens: Opens,
    /// This window's terminal socket: what each lane's relay bridge is
    /// pointed at.
    socket: PathBuf,
    /// The shared-child factory itself, for tests that put a stand-in's hub
    /// in its slot.
    #[cfg(any(test, feature = "test-support"))]
    muse: Arc<provider_muse::registry::MuseFactory>,
}

/// What a test registers in front of the adapters: one open for every key.
type TestOpen = Arc<dyn Fn(ProviderId) -> Result<Provider, ProviderError> + Send + Sync>;

#[derive(Clone)]
enum Opens {
    /// Through each adapter's factory, off the UI thread.
    Factories,
    /// The scripted stand-in, always (a disabled provider's lane).
    StandIn,
    /// The scripted stand-in until the shared child is up, then the
    /// factories: a `--no-connect` / `--replay` window. Tests that start a
    /// shared child keep reaching the real lane path.
    StandInUntilShared,
    /// A test's registration, off the UI thread.
    Test(TestOpen),
}

impl Registry {
    /// The window's registry for `lanes`, around its terminal socket: each
    /// registration's factory ([`REGISTRATIONS`]), made here and nowhere
    /// else.
    pub fn for_window(lanes: crate::runtime::Lanes, socket: PathBuf) -> Self {
        let client = crate::conn::connect_info();
        let providers = REGISTRATIONS
            .iter()
            .fold(ProviderRegistry::new(), |providers, registration| providers.register((registration.factory)(&client)));
        // Tests reach the shared child's slot itself: the typed factory
        // replaces the registered one in place, under the same key.
        #[cfg(any(test, feature = "test-support"))]
        let muse = Arc::new(provider_muse::registry::MuseFactory::new(client));
        #[cfg(any(test, feature = "test-support"))]
        let providers = providers.register(muse.clone());
        let opens = match lanes {
            crate::runtime::Lanes::Spawn => Opens::Factories,
            crate::runtime::Lanes::Scripted => Opens::StandInUntilShared,
        };
        Self {
            providers,
            opens,
            socket,
            #[cfg(any(test, feature = "test-support"))]
            muse,
        }
    }

    /// A spawning window registry with `open` registered in front of every
    /// adapter: a test's stand-in.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test(open: impl Fn(ProviderId) -> Result<Provider, ProviderError> + Send + Sync + 'static) -> Self {
        Self::for_window(crate::runtime::Lanes::Spawn, PathBuf::new()).with_test_open(open)
    }

    /// This registry opening the way `other` opens, its own slots kept: how
    /// a test swaps a window's opens without losing a hub it started.
    #[cfg(any(test, feature = "test-support"))]
    pub fn with_opens_of(&self, other: &Registry) -> Self {
        Self { opens: other.opens.clone(), ..self.clone() }
    }

    /// Put a started hub in the shared child's slot, up (tests drive a
    /// stand-in `muse serve`).
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_test_hub(&self, hub: provider_muse::MuseHub) {
        self.muse.install(hub);
    }

    /// This registry with `open` registered in front of every adapter (a
    /// test's stand-in): the shared child's slot is kept.
    pub fn with_test_open(&self, open: impl Fn(ProviderId) -> Result<Provider, ProviderError> + Send + Sync + 'static) -> Self {
        Self { opens: Opens::Test(Arc::new(open)), ..self.clone() }
    }

    /// This registry with the scripted stand-in in front: what a disabled
    /// provider's lane opens over — readable, resumable, and sending
    /// nothing anywhere.
    pub fn stand_in(&self) -> Self {
        Self { opens: Opens::StandIn, ..self.clone() }
    }

    /// The child a shared-child provider's sessions ride, when one is
    /// registered.
    pub fn shared(&self) -> Option<&dyn SharedChild> {
        self.providers.shared_child()
    }

    /// Whether the shared child is up right now.
    pub fn shared_ready(&self) -> bool {
        self.shared().is_some_and(|shared| shared.is_ready())
    }

    /// The provider whose sessions share one child, when one is registered.
    pub fn shared_id() -> Option<ProviderId> {
        ProviderId::with(ProviderTrait::SharedChild)
    }

    /// A lane on the shared child for connection-level commands (account,
    /// usage, the session list) when it is up now, never waiting.
    pub fn control(&self) -> Result<Provider, ProviderError> {
        match self.shared() {
            Some(shared) => shared.control(),
            None => Err(ProviderError::Unavailable { reason: "no provider shares a child".into() }),
        }
    }

    /// Connect a lane for `id`. Blocking unless [`Self::lands_inline`].
    pub fn open(&self, id: ProviderId) -> Result<Provider, ProviderError> {
        match &self.opens {
            Opens::StandIn => scripted_lane(),
            Opens::StandInUntilShared if !self.shared_ready() => scripted_lane(),
            Opens::Test(open) => open(id),
            Opens::Factories | Opens::StandInUntilShared => self.open_through_factory(id),
        }
    }

    /// Whether an open through this registry runs on the UI thread now: the
    /// scripted stand-in (a window's own scripted registry only while the
    /// shared child is not up). Such an open spawns nothing and never
    /// blocks.
    pub fn lands_inline(&self) -> bool {
        match &self.opens {
            Opens::Factories | Opens::Test(_) => false,
            Opens::StandIn => true,
            Opens::StandInUntilShared => !self.shared_ready(),
        }
    }

    /// Resolve, wrap, and connect one lane through `id`'s factory. `Err`
    /// when the binary is missing or the handshake fails — the caller
    /// surfaces it with the provider's name and opens nothing, never a
    /// fallback to another provider.
    fn open_through_factory(&self, id: ProviderId) -> Result<Provider, ProviderError> {
        let factory = self.providers.get(id.as_str()).map_err(|error| ProviderError::Unavailable { reason: error.to_string() })?;
        // Z5: the inherit-owner-servers opt-ins, read once per session
        // open — a flip applies to sessions started after it, never live
        // ones.
        let use_own = crate::layout::read().use_own_mcp;
        let spawn = provider::SessionSpawn {
            client: crate::conn::connect_info(),
            program: resolve_program(id),
            state_dir: crate::store::support_dir(),
            owner_home: std::env::var_os("HOME").map(PathBuf::from),
            mcp_servers: vec![terminal_relay(&self.socket)],
            inherit_owner_mcp: use_own.inherits(id),
            bills_by_key: crate::provider_status::live_statuses()
                .iter()
                .find(|status| status.provider == id)
                .is_some_and(|status| status.auth.bills_by_key()),
        };
        factory.open(&spawn)
    }
}

/// One connected scripted stand-in. Each its own session: the first keeps
/// `s-scripted` (probe entries reopen it by that id), later ones are
/// numbered. The numbering starts again in every process, so two runs on
/// one state dir mint the same ids and the second opens onto the first's
/// records: the visual probe gives each capture attempt its own copy of an
/// entry's state dir (`attempt_state` in `scripts/uiprobe.py`).
fn scripted_lane() -> Result<Provider, ProviderError> {
    use provider::ProviderAdapter as _;
    static SCRIPTED_LANES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = SCRIPTED_LANES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let session_id = if n == 0 { "s-scripted".to_owned() } else { format!("s-scripted-{}", n + 1) };
    let mut adapter = provider::scripted::ScriptedProvider::with_session_id(session_id);
    adapter.connect(&crate::conn::connect_info())?;
    Ok(Provider::new(adapter))
}

/// The terminal relay every lane gets: the bridge beside this binary,
/// pointed at the socket this window's service actually bound — threaded
/// through from the Harness, never recomputed from the support dir and pid
/// (that drifts from the bound fallback path and from test Harnesses'
/// isolated sockets).
pub fn terminal_relay(socket: &Path) -> provider::McpServer {
    provider::McpServer { command: crate::terminal_relay::bridge_path(), socket: socket.to_path_buf() }
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

/// The last model a Claude Code init/result frame reported on this
/// machine, as [`write_claude_code_last_reported_model`] stored it. `None`
/// when nothing was ever reported, the file names no model, or this is a
/// deterministic capture.
pub fn read_claude_code_last_reported_model() -> Option<String> {
    if crate::runtime::state_isolated() {
        return None;
    }
    let stored: LastClaudeCodeModel = crate::store::read_json(&claude_code_last_model_path());
    let model = stored.model.trim().to_owned();
    if model.is_empty() { None } else { Some(model) }
}

/// Remember the last model a Claude Code session reported. Best-effort: a
/// store that cannot be written loses the seed, never the session. A
/// deterministic capture never writes, and neither does a report of the
/// model already stored: the read is cheaper than the atomic write it
/// saves on every settle (docs/81 A F14).
pub fn write_claude_code_last_reported_model(model: &str) {
    if crate::runtime::state_isolated() {
        return;
    }
    let model = model.trim();
    if model.is_empty() || read_claude_code_last_reported_model().as_deref() == Some(model) {
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
#[cfg(any(test, feature = "test-support"))]
pub struct TestEnvSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    base: std::path::PathBuf,
    home: std::path::PathBuf,
    state_dir: std::path::PathBuf,
    old_home: Option<std::ffi::OsString>,
    old_state_dir: Option<std::ffi::OsString>,
    old_anthropic: Option<std::ffi::OsString>,
    old_deterministic: Option<std::ffi::OsString>,
}

#[cfg(any(test, feature = "test-support"))]
impl TestEnvSandbox {
    pub fn enter(name: &str) -> Self {
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

    pub fn state_dir(&self) -> &std::path::Path {
        &self.state_dir
    }

    pub fn write_home_settings(&self, model: &str) {
        let dir = self.home.join(".claude");
        std::fs::create_dir_all(&dir).expect("home .claude");
        std::fs::write(dir.join("settings.json"), format!("{{\"model\": \"{model}\"}}"))
            .expect("home settings");
    }

    pub fn write_workspace_settings(
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

#[cfg(any(test, feature = "test-support"))]
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
///
/// Labels are the bare family names the child's own catalog uses
/// (`displayName` "Haiku"), so the chip reads the same before and after
/// the catalog folds — it read "Claude Haiku", then "Haiku" (live
/// 2026-10-17) — and matches the full-id labels (`Haiku 4.5`).
pub fn claude_code_models() -> [SuppliedModel; 3] {
    [
        SuppliedModel {
            id: "sonnet",
            label: "Sonnet",
            detail: "The everyday model (--model sonnet; full ids work too)",
        },
        SuppliedModel {
            id: "opus",
            label: "Opus",
            detail: "The largest model (--model opus; full ids work too)",
        },
        SuppliedModel {
            id: "haiku",
            label: "Haiku",
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
            unavailable_modes: claude_code_unavailable_modes(row.id),
            ..Default::default()
        })
        .collect()
}

/// The approval modes a supplied Claude Code alias cannot run in, before
/// the child's own catalog says: Auto on Haiku, the one row 2.1.276's
/// `initialize` answer lists without `supportsAutoMode` (the CLI refuses
/// `set_permission_mode{auto}` on it). The child's catalog replaces this
/// the moment it folds.
fn claude_code_unavailable_modes(id: &str) -> Vec<aui_protocol::PermissionMode> {
    if provider_claude_code::argv::model_lacks_auto(id, None) {
        vec![aui_protocol::PermissionMode::OnRequest]
    } else {
        Vec::new()
    }
}

/// [`claude_code_unavailable_modes`] for every supplied alias that has
/// any, keyed like a folded catalog's limits.
pub fn claude_code_mode_limits() -> std::collections::HashMap<String, Vec<aui_protocol::PermissionMode>> {
    claude_code_models()
        .into_iter()
        .map(|row| (row.id.to_owned(), claude_code_unavailable_modes(row.id)))
        .filter(|(_, modes)| !modes.is_empty())
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

/// Whether a Claude Code id names the Haiku family anywhere in the id,
/// like the provider lane's own check: the CLI runs its internal
/// sub-calls on Haiku, so a Haiku report is the one report that can be
/// somebody else's.
pub fn claude_code_is_haiku_model(id: &str) -> bool {
    id.to_ascii_lowercase().contains("haiku")
}

/// Whether `id` can name a model at all: non-empty and not a `<...>`
/// placeholder such as the `<synthetic>` the CLI emits on
/// error/synthetic messages. Placeholders never claim a turn; the next
/// source answers instead.
pub fn claude_code_is_model_id(id: &str) -> bool {
    let id = id.trim();
    !id.is_empty() && !id.starts_with('<')
}

/// Whether a Claude Code turn report is an internal sub-call's rather
/// than the session's: a Haiku report while a non-Haiku pick stands —
/// the CLI runs its own sub-calls on Haiku, so that combination is the
/// sub-call's, never the session's. Every other report (another family,
/// a fallback, an unknown id) is the session's own. `None` pick means no
/// pick stands, so nothing is a sub-call. A placeholder report is never
/// a sub-call either — it is not a report at all (see
/// [`claude_code_is_model_id`]).
pub fn claude_code_is_subcall_report(report: &str, pick: Option<&str>) -> bool {
    if !claude_code_is_model_id(report) || !claude_code_is_haiku_model(report) {
        return false;
    }
    pick.map(str::trim)
        .is_some_and(|pick| !pick.is_empty() && !claude_code_is_haiku_model(pick))
}

/// Whether a Claude Code turn report may seed future sessions' chips:
/// always, except an internal sub-call's (see
/// [`claude_code_is_subcall_report`]) — persisting that would paint
/// every future fresh chip with a model nobody picked. A placeholder or
/// empty report never seeds, and with nothing known the report is the
/// best truth available. Unknown ids always seed: unknown is never a
/// sub-call.
pub fn claude_code_report_matches_session(report: &str, known: &str) -> bool {
    if !claude_code_is_model_id(report) {
        return false;
    }
    let known = known.trim();
    !claude_code_is_subcall_report(report, if known.is_empty() { None } else { Some(known) })
}

// ------------------------------------------------- the last-chosen provider

/// What the store remembers: the backend new sessions start on.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct LastProvider {
    /// The wire id (`muse`, `claude-code`, `codex`).
    #[serde(default)]
    provider: String,
}

/// The last pick's one writer ([`crate::store::JsonStore`]).
static LAST_PROVIDER: crate::store::JsonStore<LastProvider> = crate::store::JsonStore::new("provider.json");

/// The backend new sessions start on, as last chosen. `None` when nothing
/// was ever chosen or the file names no backend — the caller falls back to
/// the command line.
pub fn read_last_provider() -> Option<ProviderId> {
    let stored: LastProvider = LAST_PROVIDER.read();
    ProviderId::parse_known(&stored.provider)
}

/// Remember the backend new sessions start on. Best-effort: a store that
/// cannot be written leaves the in-memory pick, which still names every
/// session this run starts.
pub fn write_last_provider(id: ProviderId) {
    let _ = LAST_PROVIDER.write_now(&LastProvider { provider: id.as_str().to_owned() });
}

// ------------------------------------------------- external approvals

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
    /// The answers it offers, in the one vocabulary, each with the
    /// adapter's own choice id — typed on the seam's request (docs/82 5.5),
    /// never read off the id. Empty for an approval pulled from the
    /// pending set, whose inline card carries the choices.
    pub options: Vec<provider::ApprovalOption>,
    /// One-line human summary: the tool name, the command, the question.
    pub headline: String,
    /// The model-written `reason` sentence (Codex), or the tool input
    /// summary (Claude Code). Shown on the card; never empty.
    pub reason: String,
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

    /// Record the press: returns the `(choice_id, stage_token)` the
    /// provider lane sends. The card stays pending — the server's
    /// notification settles it through [`Self::resolve`]. A repeat press
    /// while one is in flight, or an unknown id, returns `None` and sends
    /// nothing twice.
    pub fn decide(&mut self, id: &str, choice: &str) -> Option<(String, Option<String>)> {
        let approval = self.approvals.get_mut(id)?;
        if approval.decision_sent.is_some() {
            return None;
        }
        let token = (choice.to_owned(), approval.stage_token.clone());
        approval.decision_sent = Some(choice.to_owned());
        Some(token)
    }

    /// Settle the card from the server's notification. Returns whether
    /// anything was waiting under that id.
    pub fn resolve(&mut self, id: &str) -> bool {
        self.approvals.remove(id).is_some()
    }

    /// Forget every card: the child that asked is gone, so nothing can
    /// settle them and no press could be delivered.
    pub fn clear(&mut self) {
        self.approvals.clear();
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

    /// The offline lanes run the scripted stand-in under every provider's
    /// descriptor (docs/82 5.9): each composer control the descriptor
    /// offers — every approval mode it declares, its own Plan toggle, the
    /// model picker — must land there, never come back "Not available"
    /// (the `composer-plan` capture bannered Claude Code's Plan). Each
    /// switch lands the way a live one does: on the stand-in's own event.
    #[test]
    fn the_stand_in_takes_every_composer_control_a_descriptor_offers() {
        use provider::{Command, PlanMode, ProviderEvent};
        let session = "s-controls";
        for id in ProviderId::registered() {
            let lane = scripted_lane().expect("a stand-in connects");
            let events = lane.events();
            let key = id.as_str();
            for mode in id.descriptor().modes {
                let sent = lane.send(Command::SelectApprovalMode { request_id: "r".into(), session_id: session.into(), mode: mode.id });
                assert!(sent.is_ok(), "{key}: the {} mode is refused: {sent:?}", mode.label);
                let echoed = events.try_recv().expect("the switch reports itself");
                assert!(matches!(echoed, ProviderEvent::ApprovalModeChanged { mode: m, .. } if m == mode.id), "{key}: drew {echoed:?}");
            }
            if id.descriptor().plan == PlanMode::Native {
                for on in [true, false] {
                    let sent = lane.send(Command::SetPlanMode { request_id: "r".into(), session_id: session.into(), on });
                    assert!(sent.is_ok(), "{key}: Plan {on} is refused: {sent:?}");
                    let echoed = events.try_recv().expect("the plan switch reports itself");
                    assert!(matches!(echoed, ProviderEvent::PlanModeChanged { on: o, .. } if o == on), "{key}: drew {echoed:?}");
                }
            }
            if offers(id, Capability::SessionConfig) {
                let sent = lane.send(Command::SelectModel { request_id: "r".into(), session_id: session.into(), model: "m".into(), model_provider: None });
                assert!(sent.is_ok(), "{key}: a model pick is refused: {sent:?}");
            }
        }
    }

    /// What a descriptor offers and the stand-in still refuses, each with
    /// why no offline capture meets the refusal. A capability a descriptor
    /// starts offering lands here or in the stand-in, by decision — never
    /// as a "Not available" banner found in a capture.
    const STAND_IN_REFUSES: &[(Capability, &str)] = &[
        (Capability::ForkSession, "a fork opens a second session; no capture forks a stand-in lane"),
        (Capability::CompactSession, "no capture sends /compact"),
        (Capability::SteerTurn, "a stand-in turn ends as it starts, so there is nothing to steer"),
        (Capability::TurnControl, "a stand-in turn ends as it starts, so there is nothing to stop"),
        (Capability::ModelCatalog, "the pull is quiet: its refusal is the menu's reason row, never a banner"),
        (Capability::Approvals, "the stand-in raises no approval, so no card answers one"),
        (Capability::Questions, "the stand-in asks no question, so no card answers one"),
        (Capability::Account, "captures read the account from the status script, not the lane"),
        (Capability::ClientTools, "no command needs it"),
        (Capability::ReasoningTraces, "no command needs it"),
    ];

    #[test]
    fn the_stand_in_refuses_only_what_no_capture_meets() {
        let lane = scripted_lane().expect("a stand-in connects");
        let held = lane.capabilities();
        for id in ProviderId::registered() {
            for capability in Capability::all() {
                if offers(id, capability) && !held.state(capability).allows_attempt() {
                    assert!(
                        STAND_IN_REFUSES.iter().any(|(refused, _)| *refused == capability),
                        "{} offers {} and the stand-in refuses it: take it in the stand-in, or say here why no capture meets it",
                        id.as_str(),
                        capability.name()
                    );
                }
            }
        }
        // The composer's own controls are never on the list.
        assert!(STAND_IN_REFUSES.iter().all(|(refused, _)| *refused != Capability::SessionConfig));
    }

    /// Read-only's detail line names what each backend still lets through
    /// on the owner's own MCP tools; every other line is the protocol's.
    #[test]
    fn read_only_detail_never_claims_more_than_the_provider_enforces() {
        use aui_protocol::PermissionMode;
        let line = |id: ProviderId, mode: PermissionMode| id.descriptor().mode(mode).expect("declared").description;
        let read_only = PermissionMode::DenyUnmatched;
        assert_eq!(line(ProviderId::ClaudeCode, read_only), read_only.description());
        assert!(line(ProviderId::Codex, read_only).contains("MCP tools your Codex config auto-approves still run"));
        assert!(line(ProviderId::Muse, read_only).contains("MCP tools you always allowed in Muse still run"));
        for id in [ProviderId::ClaudeCode, ProviderId::Codex, ProviderId::Muse] {
            for mode in [PermissionMode::AllowAll, PermissionMode::OnRequest, PermissionMode::PromptUnmatched] {
                assert_eq!(line(id, mode), mode.description());
            }
        }
    }

    /// W4: the UI table is the adapter table — for every provider.
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
                *provider_muse::capabilities_for_version(provider_muse::MUSE_MCP_VERSION_FLOOR).state(capability),
                "muse {:?} drifted from its adapter",
                capability
            );
        }
    }

    #[test]
    fn three_entries_in_the_registry() {
        assert_eq!(ProviderId::all().len(), 3);
        let ids: Vec<&str> = ProviderId::all().iter().map(|p| p.as_str()).collect();
        assert_eq!(ids, vec!["claude-code", "codex", "muse"], "the one provider order (docs/82 8.7)");
    }

    /// The terminal relay carries the threaded socket through: whatever the
    /// Harness's service actually bound (a fallback or an isolated test
    /// socket) is what every lane's bridge is pointed at — never a
    /// recompute.
    #[test]
    fn terminal_relays_carry_the_threaded_socket() {
        let socket = PathBuf::from("/tmp/bh-1-2/run/terminal-1000000.sock");
        assert_eq!(terminal_relay(&socket).socket, socket);
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
        assert_eq!(claude_code_model_label("opus"), "Opus");
        assert_eq!(claude_code_model_label("sonnet"), "Sonnet");
        assert_eq!(claude_code_model_label("haiku"), "Haiku");
        assert_eq!(claude_code_model_label("future-model-9"), "future-model-9");
        assert_eq!(claude_code_model_label("claude-unknownthing"), "claude-unknownthing");
    }

    /// Q2b: only a Haiku report against a non-Haiku pick is a sub-call.
    /// A Sonnet fallback (or /model switch) under a picked Opus is the
    /// session's own; a Haiku pick's Haiku report is its own; unknown ids
    /// are never sub-calls; placeholders never report at all.
    #[test]
    fn only_a_haiku_report_against_a_non_haiku_pick_is_a_subcall() {
        assert!(claude_code_is_subcall_report("claude-haiku-4-5-20251001", Some("opus")));
        assert!(!claude_code_is_subcall_report("claude-sonnet-4-5-20250822", Some("opus")));
        assert!(!claude_code_is_subcall_report("claude-opus-4-1-20250822", Some("opus")));
        assert!(!claude_code_is_subcall_report("future-model-9", Some("opus")));
        assert!(!claude_code_is_subcall_report("claude-haiku-4-5-20251001", Some("haiku")));
        assert!(!claude_code_is_subcall_report(
            "claude-haiku-4-5-20251001",
            Some("claude-haiku-4-5-20251001")
        ));
        assert!(!claude_code_is_subcall_report("claude-haiku-4-5-20251001", None));
        assert!(!claude_code_is_subcall_report("claude-haiku-4-5-20251001", Some("")));
        assert!(!claude_code_is_subcall_report("<synthetic>", Some("opus")));
        assert!(!claude_code_is_subcall_report("", Some("opus")));
    }

    /// Q2b: a sub-call's report never seeds a future chip, while any
    /// other report does — a Sonnet fallback under a picked Opus seeds
    /// as before Q2, with nothing known the report is the best truth
    /// available, and an empty or placeholder report never seeds.
    #[test]
    fn a_subcall_report_never_seeds_the_chip() {
        assert!(claude_code_report_matches_session("claude-opus-5[1m]", "opus"));
        assert!(claude_code_report_matches_session("claude-sonnet-4-5-20250822", "opus"));
        assert!(claude_code_report_matches_session("future-model-9", "opus"));
        assert!(claude_code_report_matches_session(
            "claude-haiku-4-5-20251001",
            "claude-haiku-4-5-20251001"
        ));
        assert!(!claude_code_report_matches_session("claude-haiku-4-5-20251001", "opus"));
        assert!(claude_code_report_matches_session("claude-haiku-4-5-20251001", ""));
        assert!(!claude_code_report_matches_session("", "opus"));
        assert!(!claude_code_report_matches_session("<synthetic>", "opus"));
        assert!(!claude_code_report_matches_session("  <synthetic>  ", ""));
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

    /// docs/81 B S-2: an id no registered provider answers to is an
    /// error, never Muse — a record or flag naming a provider this build
    /// does not know opens nothing. Keys and declared routes resolve.
    #[test]
    fn an_unknown_id_is_an_error_never_a_fallback() {
        for raw in ["", "gemini", "Muse", "claude", "ACP"] {
            assert_eq!(ProviderId::resolve(raw), Err(provider::UnknownProvider(raw.to_owned())), "{raw:?}");
            assert_eq!(ProviderId::parse_known(raw), None, "{raw:?}");
        }
        assert_eq!(ProviderId::resolve("echo"), Ok(ProviderId::Muse), "a route the descriptor declares");
        assert_eq!(ProviderId::parse_known("echo"), None, "a route is not a stored key");
        for id in ProviderId::registered() {
            assert_eq!(ProviderId::resolve(id.as_str()), Ok(id));
            assert_eq!(ProviderId::parse_known(id.as_str()), Some(id));
        }
    }

    /// docs/82 5.7: ACP is registered by one line and offered only once the
    /// person configures an agent; a stored key still reads back, and the
    /// handle prints and serialises as the old enum did.
    #[test]
    fn a_configured_provider_is_registered_but_offered_only_once_configured() {
        let keys: Vec<&str> = ProviderId::registered().map(ProviderId::as_str).collect();
        assert_eq!(keys, vec!["claude-code", "codex", "muse", "acp"], "the one provider order");
        let acp = ProviderId::parse_known("acp").expect("registered");
        assert_eq!(acp.descriptor().command_env, Some("BAAZ_ACP_AGENT"));
        assert!(!ProviderId::all().contains(&acp), "no stand-in configured under test: not offered");
        assert_eq!(resolve_program(acp), None, "nothing configured, nothing to run");
        assert_eq!(format!("{:?}", ProviderId::ClaudeCode), "ClaudeCode");
        assert_eq!(format!("{acp:?}"), "Acp");
        assert_eq!(serde_json::to_string(&ProviderId::Codex).expect("serialises"), r#""codex""#);
        assert_eq!(serde_json::from_str::<ProviderId>(r#""acp""#).ok(), Some(acp));
        assert!(serde_json::from_str::<ProviderId>(r#""gemini""#).is_err(), "an unknown key never reads as some provider");
    }

    #[test]
    fn a_configured_program_is_a_path_as_written_or_a_name_on_the_path() {
        let dir = std::env::temp_dir().join(format!("baaz-configured-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let agent = dir.join("my-agent");
        std::fs::write(&agent, "#!/bin/sh\n").expect("agent");
        assert_eq!(resolve_configured(&agent.to_string_lossy(), &[]), Some(agent.clone()), "a path as written");
        assert_eq!(resolve_configured("my-agent", std::slice::from_ref(&dir)), Some(agent.clone()), "a name on the path");
        assert_eq!(resolve_configured("my-agent", &[]), None);
        assert_eq!(resolve_configured("/nonexistent/baaz-test/agent", &[]), None, "a path that is not there is not installed");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Every fact the app shows about a provider is its adapter's
    /// declaration, and every behaviour difference is a declared trait:
    /// the registry holds no table of its own.
    #[test]
    fn the_registry_reads_each_adapters_declaration() {
        for id in ProviderId::all() {
            let declared = id.descriptor();
            assert_eq!(id.as_str(), declared.key);
            assert_eq!(id.label(), declared.label);
            assert_eq!(id.install_command(), declared.install_command);
            assert_eq!(id.update_command(), declared.update_command);
            assert_eq!(id.docs_url(), declared.docs_url);
            assert_eq!(id.version_floor(), declared.version_floor);
        }
        // One shared-child provider, and it is the one the window starts.
        let shared: Vec<ProviderId> =
            ProviderId::all().into_iter().filter(|id| id.has(ProviderTrait::SharedChild)).collect();
        assert_eq!(shared.len(), 1);
        assert_eq!(Registry::shared_id(), shared.first().copied());
        // A per-lane CLI spawns a binary; the shared child never does here.
        for id in ProviderId::all() {
            assert_eq!(binary_name(id).is_none(), id.has(ProviderTrait::SharedChild), "{id:?}");
        }
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
        // A file this build cannot parse is set aside and the last good
        // copy read instead (the store rule, docs/81 A F8): the pick before.
        std::fs::write(dir.join("provider.json"), b"not json").expect("seed bad json");
        assert_eq!(read_last_provider(), Some(ProviderId::Codex));
        // With no good copy either, it is no pick at all.
        std::fs::write(dir.join("provider.json"), b"not json").expect("seed bad json");
        let _ = std::fs::remove_file(dir.join("provider.json.bak"));
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
        // SteerTurn: Emulated on Claude Code (sent as its own turn), Native
        // on Codex — both offered. The point-of-use gates read this table.
        assert!(gate(ProviderId::ClaudeCode, Capability::SteerTurn).is_none());
        assert!(gate(ProviderId::Codex, Capability::SteerTurn).is_none());
        assert!(gate(ProviderId::Codex, Capability::TurnControl).is_none());
        // Questions: Native on Claude Code (AskUserQuestion, executed live),
        // refused by the Codex adapter — never averaged into one.
        assert!(gate(ProviderId::ClaudeCode, Capability::Questions).is_none());
        assert_eq!(gate(ProviderId::Codex, Capability::Questions).as_deref(), Some("Not available in Codex."));
    }

    /// docs/82 0.7: one capability truth. A control is offered only where
    /// the seam would attempt its command, and the table matches what each
    /// adapter actually does (F D1): no offered control is refused.
    #[test]
    fn offered_controls_match_what_each_adapter_does() {
        use Capability as C;
        let offered = |id, c| offers(id, c);
        // Claude Code: compact-now (`/compact`, docs/85 10.4), no
        // out-of-turn shell; fork, steer (as its own turn), images, questions.
        assert!(offered(ProviderId::ClaudeCode, C::CompactSession));
        assert!(!offered(ProviderId::ClaudeCode, C::SessionShell));
        assert!(offered(ProviderId::ClaudeCode, C::ForkSession));
        assert!(offered(ProviderId::ClaudeCode, C::SteerTurn));
        assert!(offered(ProviderId::ClaudeCode, C::ImageInput));
        // Codex: the adapter refuses fork, shell, question answers and
        // images; compact (`thread/compact/start`, docs/85 10.4), steer and
        // stop are native.
        assert!(offered(ProviderId::Codex, C::CompactSession));
        for c in [C::ForkSession, C::SessionShell, C::Questions, C::ImageInput] {
            assert!(!offered(ProviderId::Codex, c), "{c:?} on Codex");
        }
        assert!(offered(ProviderId::Codex, C::SteerTurn));
        assert!(offered(ProviderId::Codex, C::TurnControl));
        // Muse: everything command-backed but the never-cleanly-run shell.
        for c in [C::ForkSession, C::CompactSession, C::SteerTurn, C::TurnControl, C::ImageInput, C::Questions] {
            assert!(offered(ProviderId::Muse, c), "{c:?} on Muse");
        }
        assert!(!offered(ProviderId::Muse, C::SessionShell));
        // The words a refused control shows name the provider, nothing else.
        assert_eq!(gate(ProviderId::Codex, C::ForkSession).as_deref(), Some("Not available in Codex."));
    }

    fn sample() -> ExternalApproval {
        ExternalApproval {
            id: "appr-1".into(),
            session_id: "s-1".into(),
            provider: ProviderId::Codex,
            options: provider::ApprovalOption::of_choices(&provider_codex::fold::codex_choices(true)),
            headline: "Allow creating /tmp/probe.txt?".into(),
            reason: "Allow creating /tmp/probe.txt containing HELLO as requested?".into(),
            stage_token: None,
            decision_sent: None,
        }
    }

    #[test]
    fn the_card_is_never_ahead_of_the_server() {
        let mut store = ExternalApprovalStore::empty();
        store.inject(sample());
        assert!(store.has_pending());
        // The press sends exactly one decision and the card stays pending.
        let sent = store.decide("appr-1", "decline").expect("sent");
        assert_eq!(sent.0, "decline");
        assert!(store.has_pending(), "a sent decision still waits on the server");
        // A repeat press sends nothing twice.
        assert!(store.decide("appr-1", "accept").is_none());
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
    fn every_request_shape_reaches_the_store() {
        let mut store = ExternalApprovalStore::empty();
        for i in 0..3 {
            let mut approval = sample();
            approval.id = format!("appr-{i}");
            store.inject(approval);
        }
        assert_eq!(store.pending().len(), 3);
    }
}
