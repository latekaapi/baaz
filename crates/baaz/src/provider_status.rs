//! One [`ProviderStatus`] per backend, probed in the background, cached
//! on disk (design `docs/23-providers-connect.md` §2–§3).
//!
//! * Probes run in parallel off the UI thread, with timeouts (8s; the
//!   Codex app-server 12s). A missing binary is Not installed; a non-zero
//!   exit or a timeout on `--version` is Can't run (a timeout reads
//!   "Couldn't check — Re-check", never "Not installed"); an auth probe
//!   that fails while the binary runs is "Installed · sign-in not verified".
//! * Every probe writes all statuses atomically to
//!   `<state dir>/provider-status.json`; boot reads it before any probe.
//! * `BAAZ_DETERMINISTIC=1` never probes, reads or writes: statuses come
//!   from the scripted source [`scripted_statuses`] (env
//!   `BAAZ_PROVIDER_STATUS_SCRIPT`) that a test or probe sets.
//! * No screens change here: [`boot`] only logs `baaz: providers → …` so
//!   the next task can wire the UI.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use provider_codex::probe::CodexProbe;

use crate::providers::ProviderId;

/// `--version` probes may take this long; the Codex app-server handshake
/// gets [`CODEX_TIMEOUT`]. Both run off the UI thread.
pub const VERSION_TIMEOUT: Duration = Duration::from_secs(8);
/// How long the short-lived `codex app-server` probe may take.
pub const CODEX_TIMEOUT: Duration = Duration::from_secs(12);
/// Window-focus re-probes run at most this often per provider.
pub const FOCUS_THROTTLE: Duration = Duration::from_secs(15);

/// Whether this is a deterministic capture: no probe, no cache read, no
/// cache write — statuses come from [`scripted_statuses`].
pub fn deterministic() -> bool {
    std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// Which provider `program` names, for routing scripted runs.
#[cfg(test)]
pub fn provider_for_program(program: &str) -> ProviderId {
    if program.contains("codex") {
        ProviderId::Codex
    } else if program.contains("claude") {
        ProviderId::ClaudeCode
    } else {
        ProviderId::Muse
    }
}

// ------------------------------------------------------------- the data

/// Whether the provider's CLI is on this machine.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Installed {
    /// Present, with what `--version` said and where it was found.
    Yes {
        /// The first line of `--version`, trimmed.
        version: String,
        /// The resolved binary path.
        path: String,
    },
    /// No binary on `PATH` or in the usual install locations.
    No,
    /// Not probed yet this launch (fresh boot with no cache).
    Unknown,
}

impl Installed {
    /// The present case, from a `--version` answer and a resolved path.
    pub fn yes(version: &str, path: &str) -> Self {
        Installed::Yes { version: version.into(), path: path.into() }
    }
}

/// Who holds the provider's login.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Auth {
    /// A login with what the auth probe reported.
    SignedIn {
        /// The account email, when reported.
        email: Option<String>,
        /// The human plan label, when known.
        plan: Option<String>,
        /// How it signed in (`oauth`, `chatgpt`, `apiKey`, …), when known.
        method: Option<String>,
    },
    /// The auth probe ran and found no login.
    SignedOut,
    /// The binary runs but the auth probe failed: installed, sign-in not
    /// verified.
    Unverified,
    /// Not probed yet.
    Unknown,
}

/// What the person should know beyond the headline.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Advisory {
    /// Nothing to say.
    None,
    /// The CLI runs but is older than the floor the seam supports.
    TooOld {
        /// The minimum version Baaz supports.
        need: String,
    },
    /// The binary exists but `--version` failed: non-zero exit or timeout.
    CantRun {
        /// A stderr excerpt (or a timeout note), never the whole stream.
        stderr: String,
        /// True when the failure was a timeout: the headline then reads
        /// "Couldn't check — Re-check", never "Not installed".
        timed_out: bool,
    },
}

impl Advisory {
    /// The can't-run case, from a stderr excerpt and whether it timed out.
    pub fn cant_run(stderr: &str, timed_out: bool) -> Self {
        Advisory::CantRun { stderr: stderr.into(), timed_out }
    }
}

/// One usage window in the neutral shape every provider stores.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageWindow {
    /// Labelled from the window's length (`Weekly`, `Session · 5h`, …).
    pub label: String,
    /// Fraction used, 0.0–1.0.
    pub used_fraction: f64,
    /// Unix time the window resets, when known.
    pub resets_at: Option<i64>,
}

/// The per-provider usage snapshot a probe or a live session last yielded.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UsageSnapshot {
    /// The wire id (`muse`, `claude-code`, `codex`).
    pub provider: String,
    /// The plan, when known.
    pub plan: Option<String>,
    /// The windows, possibly empty when nothing reported one yet.
    pub windows: Vec<UsageWindow>,
    /// Unix time the snapshot was taken.
    pub as_of: i64,
}

/// One provider's status: installed, auth, the person's switch, an
/// advisory, when it was last checked, and the latest usage snapshot.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderStatus {
    /// Which backend this is.
    pub provider: ProviderId,
    /// Whether its CLI is on this machine.
    pub installed: Installed,
    /// Who holds its login.
    pub auth: Auth,
    /// The person's switch; default true. Disabled stops its probes and
    /// hides it from the composer menu (wired by a later task).
    pub enabled: bool,
    /// What the person should know beyond the headline.
    pub advisory: Advisory,
    /// Unix time of the last probe, or `None` when neither this launch
    /// nor the cache has said anything yet.
    pub checked_at: Option<i64>,
    /// The latest usage snapshot, when a probe or session yielded one.
    pub usage: Option<UsageSnapshot>,
}

impl ProviderStatus {
    /// A status that knows nothing yet: the Checking headline.
    pub fn checking(provider: ProviderId) -> Self {
        Self {
            provider,
            installed: Installed::Unknown,
            auth: Auth::Unknown,
            enabled: true,
            advisory: Advisory::None,
            checked_at: None,
            usage: None,
        }
    }

    /// Record a usage snapshot (a probe or a live session yielded one).
    pub fn set_usage(&mut self, snapshot: UsageSnapshot) {
        self.usage = Some(snapshot);
    }

    /// The headline state, first match wins: Checking → Disabled →
    /// Not installed → Can't run → Signed out →
    /// Installed · sign-in not verified → Connected.
    pub fn headline(&self) -> Headline {
        if self.checked_at.is_none() {
            return Headline::Checking;
        }
        if !self.enabled {
            return Headline::Disabled;
        }
        if self.installed == Installed::No {
            return Headline::NotInstalled;
        }
        if matches!(self.advisory, Advisory::CantRun { .. }) {
            return Headline::CantRun;
        }
        match &self.auth {
            Auth::SignedOut => Headline::SignedOut,
            Auth::SignedIn { .. } => Headline::Connected,
            Auth::Unverified | Auth::Unknown => Headline::Unverified,
        }
    }

    /// The headline as the person reads it. A timeout reads
    /// "Couldn't check — Re-check", never "Not installed".
    pub fn headline_text(&self) -> String {
        match self.headline() {
            Headline::Checking => "Checking…".into(),
            Headline::Disabled => "Disabled".into(),
            Headline::NotInstalled => "Not installed".into(),
            Headline::CantRun => match &self.advisory {
                Advisory::CantRun { timed_out: true, .. } => "Couldn't check — Re-check".into(),
                _ => "Can't run".into(),
            },
            Headline::SignedOut => "Signed out".into(),
            Headline::Unverified => "Installed · sign-in not verified".into(),
            Headline::Connected => {
                let mut line = String::from("Connected");
                if let Auth::SignedIn { email, plan, .. } = &self.auth {
                    if let Some(email) = email {
                        line.push_str(&format!(" · {email}"));
                    }
                    if let Some(plan) = plan {
                        line.push_str(&format!(" · {plan}"));
                    }
                }
                line
            }
        }
    }
}

/// The headline states of [`ProviderStatus::headline`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Headline {
    /// No probe result yet this launch and no cache.
    Checking,
    /// The person's switch is off.
    Disabled,
    /// No binary on this machine.
    NotInstalled,
    /// The binary exists but `--version` failed.
    CantRun,
    /// The auth probe ran and found no login.
    SignedOut,
    /// The binary runs but sign-in is not verified.
    Unverified,
    /// Signed in.
    Connected,
}

// ------------------------------------------------------------- the cache

/// `<state dir>/provider-status.json`: every status, written atomically
/// after each probe and read at boot before any probe.
pub fn cache_path() -> PathBuf {
    crate::store::support_dir().join("provider-status.json")
}

/// Read the cached statuses. Best-effort like every store read, and never
/// read at all under `BAAZ_DETERMINISTIC=1`.
pub fn read_cache() -> Vec<ProviderStatus> {
    if deterministic() {
        return Vec::new();
    }
    let text = match std::fs::read_to_string(cache_path()) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    serde_json::from_str(&text).unwrap_or_default()
}

/// Write every status atomically. A no-op under
/// `BAAZ_DETERMINISTIC=1`: a capture never paints the owner's real
/// logins into the store.
pub fn write_cache(statuses: &[ProviderStatus]) {
    if deterministic() {
        return;
    }
    if let Ok(text) = serde_json::to_string_pretty(statuses) {
        let _ = crate::store::write_atomic(&cache_path(), text.as_bytes());
    }
}

/// The scripted statuses a test or probe sets (env
/// `BAAZ_PROVIDER_STATUS_SCRIPT` as a JSON array of [`ProviderStatus`]):
/// the only statuses a deterministic run ever reports. `None` when unset
/// or unparseable — then every provider reads Checking.
pub fn scripted_statuses() -> Option<Vec<ProviderStatus>> {
    let script = std::env::var("BAAZ_PROVIDER_STATUS_SCRIPT").ok()?;
    serde_json::from_str(&script).ok()
}

// ------------------------------------------------------------- probing

/// What one command run produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    /// The binary could not be spawned (missing).
    NotFound,
    /// The command ran (or timed out waiting for it).
    Output {
        /// The exit code, or `None` when no exit was observed.
        code: Option<i32>,
        /// What it printed.
        stdout: String,
        /// What it complained about.
        stderr: String,
        /// True when the wait expired first: Can't run, never Not
        /// installed.
        timed_out: bool,
    },
}

/// Run `program` with `args` (a `--version` or `auth status` call).
pub type RunCommand = Arc<dyn Fn(&str, &[String]) -> RunOutcome + Send + Sync>;
/// Run the short-lived `codex app-server` probe against `program`.
pub type ProbeCodexServer = Arc<dyn Fn(&str, Duration) -> Result<CodexProbe, String> + Send + Sync>;

/// The injectable command surface: scripted outputs in tests, real
/// processes in production.
#[derive(Clone)]
pub struct Probes {
    /// Resolve the provider's binary, or `None` when it is not installed.
    pub resolve: Arc<dyn Fn(ProviderId) -> Option<PathBuf> + Send + Sync>,
    /// Run `program` with `args` (a `--version` or `auth status` call).
    pub run: RunCommand,
    /// Run the short-lived `codex app-server` probe against `program`.
    pub codex_server: ProbeCodexServer,
}

impl Probes {
    /// The production surface: real resolution, real processes with
    /// [`VERSION_TIMEOUT`], and the real app-server probe.
    pub fn real() -> Self {
        Self {
            resolve: Arc::new(default_resolve),
            run: Arc::new(|program, args| real_run(program, args, VERSION_TIMEOUT)),
            codex_server: Arc::new(|program, timeout| {
                provider_codex::probe::probe_app_server(program, timeout)
            }),
        }
    }

    /// A surface that never resolves: every provider reads Not installed.
    /// Tests that never probe use this.
    #[cfg(test)]
    pub fn never() -> Self {
        Self {
            resolve: Arc::new(|_| None),
            run: Arc::new(|_, _| panic!("Probes::never ran a command")),
            codex_server: Arc::new(|_, _| Err("Probes::never probed app-server".into())),
        }
    }

    /// A surface that panics on any touch: deterministic-mode tests prove
    /// no probe runs by holding this.
    #[cfg(test)]
    pub fn panicking() -> Self {
        Self {
            resolve: Arc::new(|_| panic!("deterministic run resolved a binary")),
            run: Arc::new(|_, _| panic!("deterministic run spawned a command")),
            codex_server: Arc::new(|_, _| panic!("deterministic run probed app-server")),
        }
    }
}

/// The production resolver: the provider lane's search for `claude` /
/// `codex`, and the `muse` binary for `muse`, which rides the legacy pump
/// and has no lane binary. The `muse` search is the env override, then
/// `PATH`, then the shared login-shell `PATH` (which already ends in the
/// Dock fallbacks). A Dock launch still finds a home install the minimal
/// `PATH` never names.
fn default_resolve(id: ProviderId) -> Option<PathBuf> {
    match id {
        ProviderId::Muse => {
            if let Some(program) = std::env::var_os("BAAZ_MUSE") {
                let candidate = PathBuf::from(&program);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            // The override Muse sessions honour too (`MUSE_BIN`), so the
            // Providers page never disagrees with a session that works —
            // named here rather than called through the muse wire crate, which the
            // seam ratchet keeps out of this file.
            if let Some(program) = std::env::var_os("MUSE_BIN") {
                let candidate = PathBuf::from(&program);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            if let Some(program) = std::env::var_os("PATH").and_then(|paths| {
                std::env::split_paths(&paths).map(|dir| dir.join("muse")).find(|candidate| {
                    candidate.is_file()
                })
            }) {
                return Some(program);
            }
            provider::env_path::find_program("muse")
        }
        ProviderId::ClaudeCode | ProviderId::Codex => crate::providers::resolve_program(id),
    }
}

/// Resolve one provider's binary the way the probes do. Used for the
/// sign-out/sign-in commands that reuse the probe's resolution.
pub fn binary_path(id: ProviderId) -> Option<PathBuf> {
    default_resolve(id)
}

/// Run `program` with `args`, waiting at most `timeout`. Read-only by
/// construction: the callers only ever ask for `--version` and
/// `auth status --json`. Blocking; call it off the UI thread.
///
/// The child's `PATH` is the login-shell `PATH` with the program's own
/// directory first, so a Dock launch (minimal `PATH`) still runs a home
/// install and its `env`-shebang neighbours; the inherited desktop-agent
/// env is scrubbed (see [`provider::child_env`]). No Baaz home is set:
/// probes read the owner's home, exactly what they verify.
fn real_run(program: &str, args: &[String], timeout: Duration) -> RunOutcome {
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .env("PATH", provider::env_path::child_path_for(std::path::Path::new(program)))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    provider::child_env::scrub_command(&mut command);
    let mut child = match command.spawn()
    {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return RunOutcome::NotFound;
        }
        Err(error) => {
            return RunOutcome::Output {
                code: None,
                stdout: String::new(),
                stderr: error.to_string(),
                timed_out: false,
            };
        }
    };
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                // The child has exited, so draining its pipes cannot
                // block: `--version` output is a line, not a stream.
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    use std::io::Read as _;
                    let _ = pipe.read_to_end(&mut stderr);
                }
                return RunOutcome::Output {
                    code: exit.code(),
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    timed_out: false,
                };
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return RunOutcome::Output {
                        code: None,
                        stdout: String::new(),
                        stderr: String::new(),
                        timed_out: true,
                    };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return RunOutcome::Output {
                    code: None,
                    stdout: String::new(),
                    stderr: error.to_string(),
                    timed_out: false,
                };
            }
        }
    }
}

/// What the existing muse connection says: the account state without a
/// second connection. `None` means the connection has said nothing yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MuseAccount {
    /// The account email, when the connection reported one.
    pub email: Option<String>,
    /// The plan or tier label, when known.
    pub plan: Option<String>,
}

/// Unix seconds now. Best-effort: zero when the clock is unavailable.
fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// The first `max` characters of `text`: a stderr excerpt, never the
/// whole stream.
fn excerpt(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// The version from `--version` output: its first non-empty line,
/// trimmed to 80 characters.
fn parse_version(stdout: &str) -> String {
    let first = stdout.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    excerpt(first, 80)
}

/// The status service: the three statuses, when each was last probed,
/// and the injectable command surface behind every probe.
pub struct Service {
    statuses: HashMap<ProviderId, ProviderStatus>,
    last_probe: HashMap<ProviderId, Instant>,
    probes: Probes,
    muse_auth: Option<MuseAccount>,
    /// The last Muse read carried no numbers while a good snapshot stands:
    /// the rows keep the old numbers and their age, with an "unavailable
    /// now" note beside them, instead of wiping to an empty snapshot.
    muse_unavailable_now: bool,
}

impl Service {
    /// A service holding Checking statuses and the given command surface.
    pub fn with_probes(probes: Probes) -> Self {
        let statuses = ProviderId::all()
            .into_iter()
            .map(|id| (id, ProviderStatus::checking(id)))
            .collect();
        Self {
            statuses,
            last_probe: HashMap::new(),
            probes,
            muse_auth: None,
            muse_unavailable_now: false,
        }
    }

    /// Whether the last Muse read found no numbers while a good snapshot
    /// stands: the rows keep the old numbers with an "unavailable now"
    /// note beside them.
    pub fn muse_unavailable_now(&self) -> bool {
        self.muse_unavailable_now
    }

    /// Feed the service the existing muse connection's account state. No
    /// second connection is ever opened: the Muse auth probe is this.
    pub fn set_muse_auth(&mut self, account: Option<MuseAccount>) {
        self.muse_auth = account;
    }

    /// This provider's status.
    pub fn status(&self, id: ProviderId) -> ProviderStatus {
        self.statuses.get(&id).cloned().unwrap_or_else(|| ProviderStatus::checking(id))
    }

    /// Replace one status (tests and the cache loader).
    pub fn put(&mut self, status: ProviderStatus) {
        self.statuses.insert(status.provider, status);
    }

    /// Overlay the on-disk cache: boot reads it before any probe, so the
    /// window can render from what the last run learned. Never reads
    /// under `BAAZ_DETERMINISTIC=1`.
    pub fn load_cache(&mut self) {
        for cached in read_cache() {
            self.statuses.insert(cached.provider, cached);
        }
    }

    /// Write every status atomically (a no-op in deterministic mode).
    pub fn save_cache(&self) {
        let statuses: Vec<ProviderStatus> =
            ProviderId::all().iter().map(|id| self.status(*id)).collect();
        write_cache(&statuses);
    }

    /// Flip the person's Enabled switch and persist it: the cache carries
    /// `enabled`, so the next launch remembers. Probing keeps the flag —
    /// a re-probe never re-enables a disabled provider.
    pub fn set_enabled(&mut self, id: ProviderId, on: bool) {
        let mut status = self.status(id);
        status.enabled = on;
        self.statuses.insert(id, status);
        self.save_cache();
    }

    /// Probe one provider now, whatever the throttle says, and write the
    /// cache after. In deterministic mode this only applies the scripted
    /// source — no binary is resolved, no command runs.
    pub fn probe_one(&mut self, id: ProviderId) {
        if deterministic() {
            self.apply_scripted();
            return;
        }
        let enabled = self.status(id).enabled;
        let mut status = probe_provider(id, &self.probes, self.muse_auth.clone());
        // A re-probe never re-enables: the switch is the person's, not
        // the probe's.
        status.enabled = enabled;
        self.install_probed(status);
    }

    /// Install a fresh probe result. A probe that read no usage keeps the
    /// reading already held (restored from the cache at boot, or recorded
    /// from a live turn): a Claude Code status probe never reads usage, so
    /// replacing wholesale wiped the saved reading at every launch and
    /// wrote the wipe back to disk.
    fn install_probed(&mut self, mut status: ProviderStatus) {
        let id = status.provider;
        if status.usage.is_none() {
            status.usage = self.statuses.get(&id).and_then(|old| old.usage.clone());
        }
        self.statuses.insert(id, status);
        self.last_probe.insert(id, Instant::now());
        self.save_cache();
    }

    /// The person's Re-check: probe one provider now.
    pub fn recheck(&mut self, id: ProviderId) {
        self.probe_one(id);
    }

    /// Probe every enabled provider in parallel, off this thread's
    /// caller: one thread per provider, each writing the cache as its
    /// probe lands. Disabled providers keep their cached status and are
    /// not probed. In deterministic mode this only applies the scripted
    /// source.
    pub fn probe_all(&mut self) {
        if deterministic() {
            self.apply_scripted();
            return;
        }
        let probes = self.probes.clone();
        let muse_auth = self.muse_auth.clone();
        let ids: Vec<ProviderId> = ProviderId::all()
            .into_iter()
            .filter(|id| self.status(*id).enabled)
            .collect();
        let mut handles = Vec::new();
        for id in ids {
            let probes = probes.clone();
            let muse_auth = muse_auth.clone();
            handles.push(std::thread::spawn(move || probe_provider(id, &probes, muse_auth)));
        }
        for handle in handles {
            let status = handle.join().expect("a probe thread panicked");
            self.install_probed(status);
        }
    }

    /// Apply the scripted source, leaving every unscripted provider
    /// Checking.
    fn apply_scripted(&mut self) {
        let Some(scripted) = scripted_statuses() else { return };
        for status in scripted {
            self.statuses.insert(status.provider, status);
        }
    }

    /// Mark `id` probed at `now` (tests drive the clock).
    pub fn mark_probed(&mut self, id: ProviderId, now: Instant) {
        self.last_probe.insert(id, now);
    }

    /// The tracked providers whose last probe is at least
    /// [`FOCUS_THROTTLE`] before `now`: what a regained window focus
    /// re-probes. Untracked providers (never probed this launch) are not
    /// due — the launch probe already covers them.
    pub fn due_for_refresh(&self, now: Instant) -> Vec<ProviderId> {
        ProviderId::all()
            .into_iter()
            .filter(|id| match self.last_probe.get(id) {
                Some(at) => now.duration_since(*at) >= FOCUS_THROTTLE,
                None => false,
            })
            .collect()
    }

    /// A regained window focus at `now`: mark the due providers fresh and
    /// hand them back for re-probing (at most every 15s per provider).
    pub fn note_focus_regained(&mut self, now: Instant) -> Vec<ProviderId> {
        let due = self.due_for_refresh(now);
        for id in &due {
            self.last_probe.insert(*id, now);
        }
        due
    }

    /// Store a usage snapshot a probe or a live session yielded. True when
    /// the stored reading changed: an identical snapshot is kept as-is, so
    /// the caller can skip its cache write.
    pub fn record_usage(&mut self, snapshot: UsageSnapshot) -> bool {
        let id = ProviderId::parse(&snapshot.provider);
        let Some(status) = self.statuses.get_mut(&id) else {
            return false;
        };
        if status.usage.as_ref() == Some(&snapshot) {
            return false;
        }
        status.set_usage(snapshot);
        true
    }

    /// Store a live lane's structured usage reading (see
    /// [`provider::UsageReport`]): what `read_usage` on an open Codex or
    /// Claude Code session last observed. The report's labels already
    /// name each window from its length, so they ride through verbatim.
    ///
    /// The reading persists with its wire-arrival time
    /// ([`provider::UsageReport::observed_at`]), never the persisting
    /// turn's: an hours-old reading re-seen at the next turn keeps its age.
    /// True when the stored reading changed — an unchanged reading (same
    /// values and observation time) is kept as-is, so the caller skips its
    /// cache write instead of re-stamping the age or writing every turn.
    pub fn record_lane_usage(&mut self, id: ProviderId, report: &provider::UsageReport) -> bool {
        let observed_at = report.observed_at;
        let candidate = UsageSnapshot {
            provider: id.as_str().into(),
            plan: report.plan.clone(),
            windows: report
                .windows
                .iter()
                .map(|window| UsageWindow {
                    label: window.label.clone(),
                    used_fraction: window.used_fraction,
                    resets_at: window.resets_at,
                })
                .collect(),
            as_of: observed_at.unwrap_or_else(now_secs),
        };
        let Some(status) = self.statuses.get_mut(&id) else {
            return false;
        };
        if let Some(held) = status.usage.as_ref() {
            if held.plan == candidate.plan && held.windows == candidate.windows {
                // Same values: without a new observation time there is
                // nothing to persist; with one the age moves to it.
                match observed_at {
                    None => return false,
                    Some(at) if held.as_of == at => return false,
                    _ => {}
                }
            }
        }
        status.set_usage(candidate);
        true
    }

    /// Store Muse's usage in the neutral shape: what its tier probe
    /// reports today. The weekly fraction is the Weekly window; the plan
    /// is the tier's own label. The reading persists with its wire-read
    /// time (`observed_at`), never the persisting moment's.
    ///
    /// An empty read (no fraction) never replaces a good snapshot: the
    /// last numbers and their age stand, flagged through
    /// [`Self::muse_unavailable_now`] so the row reads "unavailable now"
    /// beside them. True when the stored reading changed, so the caller
    /// skips its cache write for an unchanged or a kept reading.
    pub fn record_muse_usage(
        &mut self,
        plan: Option<String>,
        used_fraction: Option<f64>,
        resets_at: Option<i64>,
        observed_at: Option<i64>,
    ) -> bool {
        let Some(used) = used_fraction else {
            // No numbers: keep the last good snapshot and its age, and say
            // so — but only when there is one to keep. With nothing held
            // the empty snapshot lands as before (the row reads its
            // no-reading reason), which still counts as a change.
            if self
                .statuses
                .get(&ProviderId::Muse)
                .and_then(|status| status.usage.as_ref())
                .is_some_and(|held| !held.windows.is_empty())
            {
                self.muse_unavailable_now = true;
                return false;
            }
            return self.record_usage(UsageSnapshot {
                provider: ProviderId::Muse.as_str().into(),
                plan,
                windows: Vec::new(),
                as_of: observed_at.unwrap_or_else(now_secs),
            });
        };
        self.muse_unavailable_now = false;
        let candidate = UsageSnapshot {
            provider: ProviderId::Muse.as_str().into(),
            plan,
            windows: vec![UsageWindow {
                label: "Weekly".into(),
                used_fraction: used.clamp(0.0, 1.0),
                resets_at,
            }],
            as_of: observed_at.unwrap_or_else(now_secs),
        };
        let Some(status) = self.statuses.get_mut(&ProviderId::Muse) else {
            return false;
        };
        if let Some(held) = status.usage.as_ref() {
            if held.plan == candidate.plan && held.windows == candidate.windows {
                match observed_at {
                    None => return false,
                    Some(at) if held.as_of == at => return false,
                    _ => {}
                }
            }
        }
        status.set_usage(candidate);
        true
    }
}

/// Probe one provider to a status: resolve, `--version`, then the auth
/// lane. Pure against [`Probes`] — tests drive it with scripted outputs.
fn probe_provider(
    id: ProviderId,
    probes: &Probes,
    muse_auth: Option<MuseAccount>,
) -> ProviderStatus {
    let mut status = ProviderStatus::checking(id);
    let Some(program) = (probes.resolve)(id) else {
        status.installed = Installed::No;
        status.auth = Auth::Unknown;
        status.checked_at = Some(now_secs());
        return status;
    };
    let program = program.to_string_lossy().into_owned();
    let version = (probes.run)(&program, &["--version".into()]);
    match version {
        RunOutcome::NotFound => {
            // The binary vanished between resolve and spawn: not installed.
            status.installed = Installed::No;
            status.auth = Auth::Unknown;
            status.checked_at = Some(now_secs());
            return status;
        }
        RunOutcome::Output { code, stdout, stderr, timed_out } => {
            if timed_out {
                status.installed =
                    Installed::yes("", &program);
                status.advisory = Advisory::cant_run(
                    &format!("timed out after {}s", VERSION_TIMEOUT.as_secs()),
                    true,
                );
            } else if code != Some(0) {
                status.installed = Installed::yes("", &program);
                let detail = if stderr.trim().is_empty() { &stdout } else { &stderr };
                status.advisory = Advisory::cant_run(&excerpt(detail.trim(), 500), false);
            } else {
                status.installed = Installed::yes(&parse_version(&stdout), &program);
            }
        }
    }
    if !matches!(status.installed, Installed::Yes { .. }) {
        status.checked_at = Some(now_secs());
        return status;
    }
    if matches!(status.advisory, Advisory::CantRun { .. }) {
        // The binary does not run: no auth lane to ask.
        status.auth = Auth::Unknown;
        status.checked_at = Some(now_secs());
        return status;
    }
    match id {
        ProviderId::Muse => {
            status.auth = match muse_auth {
                Some(account) => Auth::SignedIn {
                    email: account.email,
                    plan: account.plan,
                    method: Some("account".into()),
                },
                None => Auth::Unknown,
            };
        }
        ProviderId::ClaudeCode => {
            let auth = (probes.run)(
                &program,
                &["auth".into(), "status".into(), "--json".into()],
            );
            status.auth = match auth {
                RunOutcome::Output { code: Some(0), stdout, .. } => {
                    match provider_claude_code::auth_status::parse_auth_status(&stdout) {
                        Some(parsed) if parsed.logged_in => Auth::SignedIn {
                            plan: parsed.plan_label(),
                            email: parsed.email,
                            method: parsed.auth_method,
                        },
                        Some(_) => Auth::SignedOut,
                        None => Auth::Unverified,
                    }
                }
                // The binary runs but the auth probe failed: installed,
                // sign-in not verified — never Signed out, never Can't run.
                _ => Auth::Unverified,
            };
        }
        ProviderId::Codex => {
            match (probes.codex_server)(&program, CODEX_TIMEOUT) {
                Ok(probe) => {
                    status.auth = if probe.account.signed_in {
                        Auth::SignedIn {
                            email: probe.account.email,
                            plan: probe.plan.or(probe.account.plan),
                            method: probe.account.method,
                        }
                    } else {
                        Auth::SignedOut
                    };
                    status.set_usage(UsageSnapshot {
                        provider: id.as_str().into(),
                        plan: status_plan(&status.auth),
                        windows: probe
                            .windows
                            .into_iter()
                            .map(|window| UsageWindow {
                                label: window.label,
                                used_fraction: window.used_fraction,
                                resets_at: window.resets_at,
                            })
                            .collect(),
                        as_of: now_secs(),
                    });
                }
                // The binary runs but the app-server probe failed:
                // installed, sign-in not verified.
                Err(_) => {
                    status.auth = Auth::Unverified;
                }
            }
        }
    }
    status.checked_at = Some(now_secs());
    status
}

/// The plan on a just-probed auth, for the usage snapshot beside it.
fn status_plan(auth: &Auth) -> Option<String> {
    match auth {
        Auth::SignedIn { plan, .. } => plan.clone(),
        _ => None,
    }
}

// ------------------------------------------------------------- first run

/// The `onboarding_completed` flag: presence in the state dir means the
/// person finished first-run setup (settable by the later task's screen).
pub fn onboarding_completed_path() -> PathBuf {
    crate::store::support_dir().join("onboarding_completed")
}

/// Set the first-run flag. Best-effort, and never in deterministic mode.
/// Settable by the later first-run screen; nothing sets it yet.
#[allow(dead_code)]
pub fn set_onboarding_completed() {
    if deterministic() {
        return;
    }
    let _ = std::fs::create_dir_all(crate::store::support_dir());
    let _ = std::fs::write(onboarding_completed_path(), b"");
}

/// Whether this is a first run, from stored facts only — never from a
/// live probe: the flag is unset and no sessions or projects exist.
pub fn is_first_run() -> bool {
    if onboarding_completed_path().exists() {
        return false;
    }
    if !crate::sessions::read().is_empty() {
        return false;
    }
    if !crate::projects::read().projects.is_empty() {
        return false;
    }
    true
}

// ------------------------------------------------------------- boot

/// One `provider=headline` cell of the startup log.
fn log_cell(id: ProviderId, status: &ProviderStatus) -> String {
    format!("{}={}", id.as_str(), status.headline_text())
}

/// Log the computed statuses: `baaz: providers → …`.
fn log_statuses(statuses: &HashMap<ProviderId, ProviderStatus>) {
    let cells: Vec<String> = ProviderId::all()
        .into_iter()
        .map(|id| {
            let status =
                statuses.get(&id).cloned().unwrap_or_else(|| ProviderStatus::checking(id));
            log_cell(id, &status)
        })
        .collect();
    eprintln!("baaz: providers → {}", cells.join("; "));
}

/// Read cache (or script) into Checking defaults: what boot renders from
/// before any probe lands.
fn boot_statuses() -> HashMap<ProviderId, ProviderStatus> {
    let mut statuses: HashMap<ProviderId, ProviderStatus> = ProviderId::all()
        .into_iter()
        .map(|id| (id, ProviderStatus::checking(id)))
        .collect();
    if deterministic() {
        if let Some(scripted) = scripted_statuses() {
            for status in scripted {
                statuses.insert(status.provider, status);
            }
        }
        return statuses;
    }
    for cached in read_cache() {
        statuses.insert(cached.provider, cached);
    }
    statuses
}

/// A hermetic `BAAZ_STATE_DIR` for tests: a temp dir, the env lock held
/// so no two tests point the store elsewhere at once, and everything
/// restored on drop. Mirrors the keymap tests' sandbox.
#[cfg(test)]
pub(crate) struct TestSandbox {
    _guard: std::sync::MutexGuard<'static, ()>,
    dir: PathBuf,
    old_state_dir: Option<std::ffi::OsString>,
    old_deterministic: Option<std::ffi::OsString>,
    old_script: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl TestSandbox {
    /// Hold the store env lock and point `BAAZ_STATE_DIR` at a fresh temp
    /// dir, with `BAAZ_DETERMINISTIC` and `BAAZ_PROVIDER_STATUS_SCRIPT`
    /// cleared.
    pub fn hold() -> Self {
        let guard = crate::store::test_env_lock();
        let dir = std::env::temp_dir().join(format!(
            "baaz-provider-status-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let old_state_dir = std::env::var_os("BAAZ_STATE_DIR");
        let old_deterministic = std::env::var_os("BAAZ_DETERMINISTIC");
        let old_script = std::env::var_os("BAAZ_PROVIDER_STATUS_SCRIPT");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        std::env::remove_var("BAAZ_DETERMINISTIC");
        std::env::remove_var("BAAZ_PROVIDER_STATUS_SCRIPT");
        Self { _guard: guard, dir, old_state_dir, old_deterministic, old_script }
    }

    /// The temp state dir this sandbox points at.
    pub fn state_dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Turn deterministic mode on or off.
    pub fn set_deterministic(&self, on: bool) {
        if on {
            std::env::set_var("BAAZ_DETERMINISTIC", "1");
        } else {
            std::env::remove_var("BAAZ_DETERMINISTIC");
        }
    }

    /// Set an env var for the sandbox's lifetime.
    pub fn set_var(&self, key: &str, value: &str) {
        std::env::set_var(key, value);
    }
}

#[cfg(test)]
impl Drop for TestSandbox {
    fn drop(&mut self) {
        match &self.old_state_dir {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        match &self.old_deterministic {
            Some(value) => std::env::set_var("BAAZ_DETERMINISTIC", value),
            None => std::env::remove_var("BAAZ_DETERMINISTIC"),
        }
        match &self.old_script {
            Some(value) => std::env::set_var("BAAZ_PROVIDER_STATUS_SCRIPT", value),
            None => std::env::remove_var("BAAZ_PROVIDER_STATUS_SCRIPT"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

// ------------------------------------------------------------- live wiring

/// What the live muse connection last reported, without ever opening a
/// second one: `None` until `account/read` or `account/changed` speaks.
/// Background refreshes seed their Muse probe from this, so a normal
/// launch reports Muse Connected once the connection signs in.
static MUSE_LIVE: OnceLock<Mutex<Option<MuseAccount>>> = OnceLock::new();

/// How often the account menu refreshes its usage cards: lane peeks are
/// cheap but not free, and the probes behind a re-check are not cheap at
/// all.
pub const USAGE_REFRESH_THROTTLE: Duration = Duration::from_secs(60);
/// How long a "Refreshing…" row waits for its read before settling on its
/// own: a hung probe or a completion that never runs (a panic on the
/// background path, a dropped entity) must never stick the row forever.
pub const USAGE_REFRESH_GUARD: Duration = Duration::from_secs(20);

/// The window-activation edge plus the service behind it: the same
/// service boot probes, so focus refreshes reuse its per-provider
/// throttle instead of keeping a second clock.
struct LiveService {
    /// Whether the window was active on the last frame.
    was_active: bool,
    /// The statuses boot probed, refreshed on focus regain.
    service: Service,
    /// When the account menu last refreshed each provider's usage card:
    /// the 60 s throttle is per provider, so one provider's fresh read
    /// never spends another's.
    last_usage_refresh: HashMap<ProviderId, Instant>,
    /// The providers with an asynchronous usage read in flight (the Muse
    /// wire re-read, the Codex probe): their menu rows read "Refreshing…"
    /// until the read lands.
    usage_refreshing: HashSet<ProviderId>,
    /// Lanes whose `try_lock` peek failed while a turn finished: the
    /// reading is still in the lane's fold, and the next TurnFinished or
    /// menu open persists it.
    usage_pending: HashSet<ProviderId>,
}

static LIVE: OnceLock<Mutex<LiveService>> = OnceLock::new();

fn muse_live() -> MutexGuard<'static, Option<MuseAccount>> {
    MUSE_LIVE
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn live_service() -> MutexGuard<'static, LiveService> {
    LIVE.get_or_init(|| {
        Mutex::new(LiveService {
            was_active: false,
            service: seeded_live_service(),
            last_usage_refresh: HashMap::new(),
            usage_refreshing: HashSet::new(),
            usage_pending: HashSet::new(),
        })
    })
    .lock()
    .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write statuses to the cache off the caller's thread: the TurnFinished
/// hook and the menu-open refresh both run on the UI thread, and the file
/// write never blocks them. Best-effort like every store write.
fn save_cache_in_background(statuses: Vec<ProviderStatus>) {
    // Writes may be queued faster than they land: each carries a
    // generation, and under one lock only the newest generation writes, so
    // an older snapshot can never overwrite a newer one.
    static GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    static WRITER: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
    let generation = GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let mut written = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if generation > *written && generation == GENERATION.load(std::sync::atomic::Ordering::SeqCst) {
            write_cache(&statuses);
            *written = generation;
        }
    });
}

/// Every status the live service holds, for an off-thread cache write.
fn all_statuses(service: &Service) -> Vec<ProviderStatus> {
    ProviderId::all().iter().map(|id| service.status(*id)).collect()
}

/// Seed the live service with `statuses`: what boot renders from before
/// any probe lands — the on-disk cache, or the scripted source in
/// deterministic mode.
fn seed_live(statuses: &HashMap<ProviderId, ProviderStatus>) {
    let mut live = live_service();
    for (id, status) in statuses {
        live.service.put(status.clone());
        if status.checked_at.is_some() {
            live.service.mark_probed(*id, Instant::now());
        }
    }
}

/// Every provider's status as the live service last knew it, in registry
/// order: what boot seeded and the probes and lane refreshes have updated
/// since. The Settings → Providers page renders all of them; the account
/// menu renders one usage row per enabled provider.
pub fn live_statuses() -> Vec<ProviderStatus> {
    let live = live_service();
    ProviderId::all().iter().map(|id| live.service.status(*id)).collect()
}

/// Whether the account menu's usage refresh is due: at most every
/// [`USAGE_REFRESH_THROTTLE`] per provider. Marks the refresh when due,
/// so the caller that goes ahead owns exactly one refresh per window.
#[cfg(test)]
pub fn note_usage_refresh() -> bool {
    !note_usage_refresh_for(&ProviderId::all()).is_empty()
}

/// The subset of `ids` whose account-menu usage refresh is due: each at
/// most every [`USAGE_REFRESH_THROTTLE`], on its own clock — one
/// provider's fresh read never spends another's. Due providers are
/// marked, so the caller that goes ahead owns each one.
pub fn note_usage_refresh_for(ids: &[ProviderId]) -> Vec<ProviderId> {
    let mut live = live_service();
    let now = Instant::now();
    let due = usage_refresh_due(&live.last_usage_refresh, ids, now);
    for id in &due {
        live.last_usage_refresh.insert(*id, now);
    }
    due
}

/// The pure arm of [`note_usage_refresh_for`]: which of `ids` last
/// refreshed at least [`USAGE_REFRESH_THROTTLE`] before `now` (never
/// refreshed counts as due). Pure so tests drive the clocks without
/// touching the live service.
pub fn usage_refresh_due(
    last: &HashMap<ProviderId, Instant>,
    ids: &[ProviderId],
    now: Instant,
) -> Vec<ProviderId> {
    ids.iter()
        .filter(|id| {
            last.get(id).is_none_or(|at| now.duration_since(*at) >= USAGE_REFRESH_THROTTLE)
        })
        .copied()
        .collect()
}

/// Every enabled provider the account menu can read without a model
/// turn: connected lanes peek live, Muse answers the free wire read,
/// and Codex answers its read-only probe. Disabled providers are
/// omitted, and so are providers that are not connected (and are not
/// Muse): with no lane and no login there is nothing to read.
pub fn usage_refresh_targets(statuses: &[ProviderStatus]) -> Vec<ProviderId> {
    ProviderId::all()
        .into_iter()
        .filter(|id| {
            let Some(status) = statuses.iter().find(|status| status.provider == *id) else {
                return false;
            };
            if !status.enabled {
                return false;
            }
            status.headline() == Headline::Connected || *id == ProviderId::Muse
        })
        .collect()
}

/// Mark `ids` as asynchronously refreshing: their menu rows read
/// "Refreshing…" until their read lands and clears them. A guard clears
/// them after [`USAGE_REFRESH_GUARD`] even when the completion never runs
/// (a hung probe, a panic on the background path), so the row always
/// settles.
pub fn mark_usage_refreshing(ids: &[ProviderId]) {
    {
        let mut live = live_service();
        live.usage_refreshing.extend(ids.iter().copied());
    }
    clear_usage_refreshing_after(ids, USAGE_REFRESH_GUARD);
}

/// Clear `ids` from the refreshing set after `delay`, whatever their reads
/// did: the guard behind [`mark_usage_refreshing`]. A landed read clears
/// sooner through [`clear_usage_refreshing`); this only bounds the wait.
pub fn clear_usage_refreshing_after(ids: &[ProviderId], delay: Duration) {
    let ids = ids.to_vec();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        clear_usage_refreshing(&ids);
    });
}

/// Clear `ids` from the refreshing set: their menu rows read the landed
/// result (or what was there before) on the next render.
pub fn clear_usage_refreshing(ids: &[ProviderId]) {
    let mut live = live_service();
    for id in ids {
        live.usage_refreshing.remove(id);
    }
}

/// The providers with an asynchronous usage read in flight: what the
/// menu rows check before rendering their readings.
pub fn usage_refreshing() -> Vec<ProviderId> {
    live_service().usage_refreshing.iter().copied().collect()
}

/// Mark a lane's usage as still to persist: its `try_lock` peek failed
/// while a turn finished, so the reading stays in the lane's fold until
/// the next TurnFinished or menu open retries it. A successful persist
/// through [`record_observed_usage`] or [`record_refreshed_usage`] clears
/// it.
pub fn mark_usage_pending(id: ProviderId) {
    live_service().usage_pending.insert(id);
}

/// The lanes with a reading still to persist (see [`mark_usage_pending`]).
#[cfg(test)]
pub fn usage_pending() -> Vec<ProviderId> {
    live_service().usage_pending.iter().copied().collect()
}

/// Whether the last Muse read found no numbers while a good snapshot
/// stands (see [`Service::muse_unavailable_now`]).
pub fn muse_unavailable_now() -> bool {
    live_service().service.muse_unavailable_now()
}

/// Note a Muse read that carried no numbers: when a good snapshot stands
/// it is kept with its age and the rows read "unavailable now" beside it.
pub fn note_muse_unavailable_now() {
    let mut live = live_service();
    if live
        .service
        .status(ProviderId::Muse)
        .usage
        .as_ref()
        .is_some_and(|held| !held.windows.is_empty())
    {
        live.service.muse_unavailable_now = true;
    }
}

/// Persist one lane's usage reading the moment it arrives (a finished
/// turn): the single store the rows render from, written to the cache,
/// so the reading survives the view closing and restarts, and the menu
/// shows it with its true age. An unchanged reading changes nothing and
/// writes nothing; the file write runs off the UI thread.
pub fn record_observed_usage(id: ProviderId, report: &provider::UsageReport) {
    let statuses = {
        let mut live = live_service();
        live.usage_pending.remove(&id);
        if !live.service.record_lane_usage(id, report) {
            return;
        }
        all_statuses(&live.service)
    };
    save_cache_in_background(statuses);
}

/// Persist the Muse snapshot beside a tier answer: the weekly fraction
/// under the tier's own label, so the menu shows the newest reading
/// with its true age rather than the last menu-open peek. The reading
/// carries its wire-read time (`observed_at`); an empty read keeps the
/// last good numbers and their age (see
/// [`Service::record_muse_usage`]). Unchanged readings write nothing; the
/// file write runs off the UI thread.
pub fn record_muse_snapshot_at(
    plan: Option<String>,
    used_fraction: Option<f64>,
    observed_at: Option<i64>,
) {
    if used_fraction.is_none() {
        note_muse_unavailable_now();
    }
    let statuses = {
        let mut live = live_service();
        if !live.service.record_muse_usage(plan, used_fraction, None, observed_at) {
            return;
        }
        all_statuses(&live.service)
    };
    save_cache_in_background(statuses);
}


/// Persist a background probe's usage reading (today the Codex
/// app-server probe's): only the usage is taken, never the probe's
/// headline or switch — those stay the live service's own. An unchanged
/// reading writes nothing; the file write runs off the UI thread.
pub fn record_probed_snapshot(snapshot: UsageSnapshot) {
    let statuses = {
        let mut live = live_service();
        if !live.service.record_usage(snapshot) {
            return;
        }
        all_statuses(&live.service)
    };
    save_cache_in_background(statuses);
}

/// Store lane and Muse readings the account menu just refreshed: one
/// snapshot per provider, in the single store the rows render from. The
/// store is written to the cache, so a Claude Code reading survives
/// restarts: boot reloads it and the row shows its age. Unchanged
/// readings write nothing; the file write runs off the UI thread.
pub fn record_refreshed_usage(
    lanes: &[(ProviderId, provider::UsageReport)],
    muse: Option<(Option<String>, Option<f64>, Option<i64>)>,
) {
    let statuses = {
        let mut live = live_service();
        let mut changed = false;
        for (id, report) in lanes {
            if live.service.record_lane_usage(*id, report) {
                changed = true;
            }
            live.usage_pending.remove(id);
        }
        if let Some((plan, used_fraction, observed_at)) = muse {
            if used_fraction.is_none()
                && live
                    .service
                    .status(ProviderId::Muse)
                    .usage
                    .as_ref()
                    .is_some_and(|held| !held.windows.is_empty())
            {
                live.service.muse_unavailable_now = true;
            } else if live.service.record_muse_usage(plan, used_fraction, None, observed_at) {
                changed = true;
            }
        }
        if !changed {
            return;
        }
        all_statuses(&live.service)
    };
    save_cache_in_background(statuses);
}

/// Remember the existing muse connection's account state: the Muse auth
/// probe. Read-only — it never opens a second connection.
pub fn note_muse_account(account: Option<MuseAccount>) {
    *muse_live() = account;
}

/// The live connection's account, for seeding a refresh's Muse probe.
fn live_muse_auth() -> Option<MuseAccount> {
    muse_live().clone()
}

/// Seed a fresh live service from what boot renders from (cache, or the
/// scripted source in deterministic mode), so reads before the first
/// probe lands still answer.
fn seeded_live_service() -> Service {
    let mut service = Service::with_probes(Probes::real());
    service.set_muse_auth(live_muse_auth());
    for (_, status) in boot_statuses() {
        service.put(status);
    }
    service
}

/// Whether the person's Enabled switch is on for `id`.
pub fn provider_enabled(id: ProviderId) -> bool {
    live_service().service.status(id).enabled
}

/// Flip the person's Enabled switch and persist it through the cache.
pub fn set_provider_enabled(id: ProviderId, on: bool) {
    live_service().service.set_enabled(id, on);
}

/// Re-probe one provider in the background (the card's Re-check), and
/// re-log the line when it lands. Read-only probes only.
pub fn recheck_provider(id: ProviderId) {
    if deterministic() {
        live_service().service.recheck(id);
        return;
    }
    refresh_in_background(vec![id]);
}

/// Re-probe `ids` off this thread, publish into the live service, and
/// re-log the line. Read-only probes only — never a login, logout or
/// other auth-changing call.
fn refresh_in_background(ids: Vec<ProviderId>) {
    std::thread::spawn(move || {
        let mut probed = Service::with_probes(Probes::real());
        probed.set_muse_auth(live_muse_auth());
        probed.load_cache();
        for id in &ids {
            probed.recheck(*id);
        }
        let statuses: HashMap<ProviderId, ProviderStatus> = ProviderId::all()
            .into_iter()
            .map(|id| (id, probed.status(id)))
            .collect();
        {
            let mut live = live_service();
            let now = Instant::now();
            for id in &ids {
                if let Some(status) = statuses.get(id) {
                    live.service.put(status.clone());
                    live.service.mark_probed(*id, now);
                }
            }
        }
        log_statuses(&statuses);
    });
}

/// The live muse connection spoke (`account/read` or `account/changed`):
/// re-probe Muse now that [`note_muse_account`] holds what it said, so
/// the logged line follows sign-in and sign-out. A no-op in
/// deterministic mode.
pub fn refresh_muse_status() {
    if deterministic() {
        return;
    }
    refresh_in_background(vec![ProviderId::Muse]);
}

/// Re-probe one provider now, off this thread (the connect screen's
/// Re-check, and the re-probe after a sign-in). A no-op in deterministic
/// mode: captures never probe.
pub fn reprobe_provider(id: crate::providers::ProviderId) {
    if deterministic() {
        return;
    }
    refresh_in_background(vec![id]);
}

/// A window-activation edge, called every frame with
/// `window.is_window_active()`: a regained focus re-probes what is due
/// (at most every [`FOCUS_THROTTLE`] per provider) off this thread and
/// re-logs the line. Steady frames do nothing; deterministic runs do
/// nothing at all.
pub fn note_window_active(active: bool) {
    if deterministic() {
        return;
    }
    let due = {
        let mut live = live_service();
        let regained = active && !live.was_active;
        live.was_active = active;
        if !regained {
            return;
        }
        let due = live.service.note_focus_regained(Instant::now());
        due.into_iter().filter(|id| live.service.status(*id).enabled).collect::<Vec<_>>()
    };
    if due.is_empty() {
        return;
    }
    refresh_in_background(due);
}

/// Boot the status service: read the cache before any probe, log
/// `baaz: providers → …` (plus the stored-facts first-run line), and
/// probe all providers in parallel off the UI thread (each probe
/// re-logs the line as it lands). Deterministic runs report the
/// scripted source and never probe, read or write.
pub fn boot() {
    let seeded = boot_statuses();
    log_statuses(&seeded);
    // The menu renders from the live service from its first frame: seed
    // it with the cache (or the script, deterministically) before any
    // probe lands, so the cards never wait on a background thread.
    seed_live(&seeded);
    // The first-run fact is stored facts only, never a live probe — log
    // it here so the next task can wire the screen; nothing shown changes.
    eprintln!("baaz: first_run={}", is_first_run());
    if deterministic() {
        return;
    }
    std::thread::spawn(|| {
        let mut service = Service::with_probes(Probes::real());
        service.set_muse_auth(live_muse_auth());
        service.load_cache();
        service.probe_all();
        let now = Instant::now();
        let statuses: HashMap<ProviderId, ProviderStatus> = ProviderId::all()
            .into_iter()
            .map(|id| (id, service.status(id)))
            .collect();
        {
            let mut live = live_service();
            for id in ProviderId::all() {
                if let Some(status) = statuses.get(&id) {
                    live.service.put(status.clone());
                    live.service.mark_probed(id, now);
                }
            }
        }
        log_statuses(&statuses);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn sandbox() -> TestSandbox {
        TestSandbox::hold()
    }

    fn ok_version(version: &str) -> RunOutcome {
        RunOutcome::Output {
            code: Some(0),
            stdout: format!("{version}\n"),
            stderr: String::new(),
            timed_out: false,
        }
    }

    fn scripted_service(
        versions: HashMap<ProviderId, RunOutcome>,
        claude_auth: RunOutcome,
        codex: Result<provider_codex::probe::CodexProbe, String>,
    ) -> Service {
        let probes = Probes {
            resolve: Arc::new(move |id| {
                Some(PathBuf::from(match id {
                    ProviderId::Muse => "/bin/muse",
                    ProviderId::ClaudeCode => "/bin/claude",
                    ProviderId::Codex => "/bin/codex",
                }))
            }),
            run: Arc::new(move |program, args| {
                // The auth-status call and the `--version` call share one
                // program: route by argv, so a failing auth probe does not
                // masquerade as a failing binary.
                let is_version = args.first().is_some_and(|arg| arg == "--version");
                if !is_version && program.contains("claude") && !program.contains("codex") {
                    return claude_auth.clone();
                }
                versions
                    .get(&provider_for_program(program))
                    .cloned()
                    .unwrap_or(RunOutcome::NotFound)
            }),
            codex_server: Arc::new(move |_, _| codex.clone()),
        };
        Service::with_probes(probes)
    }

    #[test]
    fn every_headline_state_renders() {
        let _env = sandbox();
        let mut statuses = HashMap::new();
        statuses.insert(ProviderId::Muse, ProviderStatus::checking(ProviderId::Muse));
        let mut disabled = ProviderStatus::checking(ProviderId::ClaudeCode);
        disabled.enabled = false;
        // Disabled still needs a probe result (or cache) behind it:
        // with nothing known yet the headline is Checking, per the §2
        // precedence.
        disabled.checked_at = Some(1);
        statuses.insert(ProviderId::ClaudeCode, disabled);
        let mut missing = ProviderStatus::checking(ProviderId::Codex);
        missing.installed = Installed::No;
        missing.checked_at = Some(1);
        statuses.insert(ProviderId::Codex, missing);
        assert_eq!(statuses[&ProviderId::Muse].headline_text(), "Checking…");
        assert_eq!(statuses[&ProviderId::ClaudeCode].headline_text(), "Disabled");
        assert_eq!(statuses[&ProviderId::Codex].headline_text(), "Not installed");
    }

    #[test]
    fn cant_run_signed_out_unverified_and_connected_render() {
        let _env = sandbox();
        let mut status = ProviderStatus::checking(ProviderId::Muse);
        status.checked_at = Some(1);
        status.installed = Installed::yes("1.4.0", "/bin/muse");
        status.advisory = Advisory::cant_run("boom", false);
        assert_eq!(status.headline_text(), "Can't run");
        status.advisory = Advisory::None;
        status.auth = Auth::SignedOut;
        assert_eq!(status.headline_text(), "Signed out");
        status.auth = Auth::Unverified;
        assert_eq!(status.headline_text(), "Installed · sign-in not verified");
        status.auth = Auth::SignedIn {
            email: Some("a@x.com".into()),
            plan: Some("Claude Max".into()),
            method: Some("oauth".into()),
        };
        assert_eq!(status.headline_text(), "Connected · a@x.com · Claude Max");
    }

    #[test]
    fn precedence_is_checking_disabled_not_installed_cant_run_signed_out() {
        let _env = sandbox();
        // Disabled beats everything below it, even Can't run + Signed out.
        let mut status = ProviderStatus::checking(ProviderId::Codex);
        status.checked_at = Some(1);
        status.enabled = false;
        status.installed = Installed::No;
        status.advisory = Advisory::cant_run("x", false);
        status.auth = Auth::SignedOut;
        assert_eq!(status.headline(), Headline::Disabled);
        // Not installed beats Can't run.
        status.enabled = true;
        assert_eq!(status.headline(), Headline::NotInstalled);
        // Can't run beats Signed out.
        status.installed = Installed::yes("0.144.6", "/bin/codex");
        assert_eq!(status.headline(), Headline::CantRun);
        // Signed out beats Unverified.
        status.advisory = Advisory::None;
        assert_eq!(status.headline(), Headline::SignedOut);
    }

    #[test]
    fn a_version_timeout_is_cant_run_never_not_installed() {
        let _env = sandbox();
        let mut versions = HashMap::new();
        versions.insert(
            ProviderId::ClaudeCode,
            RunOutcome::Output {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            },
        );
        let mut service = scripted_service(
            versions,
            RunOutcome::Output {
                code: None,
                stdout: String::new(),
                stderr: String::new(),
                timed_out: true,
            },
            Err("unreachable".into()),
        );
        service.probe_one(ProviderId::ClaudeCode);
        let status = service.status(ProviderId::ClaudeCode);
        assert!(matches!(status.installed, Installed::Yes { .. }), "a timeout must not read Not installed");
        assert_eq!(status.headline(), Headline::CantRun);
        assert_eq!(status.headline_text(), "Couldn't check — Re-check");
    }

    #[test]
    fn a_missing_binary_is_not_installed() {
        let _env = sandbox();
        let probes = Probes {
            resolve: Arc::new(|_| None),
            run: Arc::new(|_, _| panic!("no binary, no run")),
            codex_server: Arc::new(|_, _| Err("unreachable".into())),
        };
        let mut service = Service::with_probes(probes);
        service.probe_one(ProviderId::Codex);
        assert_eq!(service.status(ProviderId::Codex).headline(), Headline::NotInstalled);
    }

    #[test]
    fn a_failing_auth_probe_with_a_running_binary_is_unverified() {
        let _env = sandbox();
        let mut versions = HashMap::new();
        versions.insert(ProviderId::ClaudeCode, ok_version("2.1.276"));
        let mut service = scripted_service(
            versions,
            RunOutcome::Output {
                code: Some(1),
                stdout: String::new(),
                stderr: "auth exploded".into(),
                timed_out: false,
            },
            Err("unreachable".into()),
        );
        service.probe_one(ProviderId::ClaudeCode);
        let status = service.status(ProviderId::ClaudeCode);
        assert_eq!(status.headline(), Headline::Unverified);
        assert_eq!(status.headline_text(), "Installed · sign-in not verified");
    }

    #[test]
    fn a_reprobe_keeps_the_saved_usage_reading() {
        // Review finding: boot restores the cache, then probes; the Claude
        // Code probe reads no usage and used to replace the status whole,
        // wiping the saved reading on every launch.
        let _env = sandbox();
        let mut versions = HashMap::new();
        versions.insert(ProviderId::ClaudeCode, ok_version("2.1.276"));
        let mut service = scripted_service(
            versions,
            RunOutcome::Output {
                code: Some(0),
                stdout: r#"{"loggedIn":true,"authMethod":"oauth","email":"a@x.com","subscriptionType":"max"}"#.into(),
                stderr: String::new(),
                timed_out: false,
            },
            Err("unreachable".into()),
        );
        let saved = UsageSnapshot {
            provider: "claude-code".into(),
            plan: Some("Max".into()),
            windows: Vec::new(),
            as_of: 1_700_000_000,
        };
        service.statuses.entry(ProviderId::ClaudeCode).or_insert_with(|| ProviderStatus::checking(ProviderId::ClaudeCode)).usage =
            Some(saved.clone());
        service.probe_one(ProviderId::ClaudeCode);
        assert_eq!(service.status(ProviderId::ClaudeCode).usage, Some(saved));
    }

    #[test]
    fn claude_max_and_pro_labels_come_from_subscription_type() {
        let _env = sandbox();
        let mut versions = HashMap::new();
        versions.insert(ProviderId::ClaudeCode, ok_version("2.1.276"));
        let mut service = scripted_service(
            versions,
            RunOutcome::Output {
                code: Some(0),
                stdout: r#"{"loggedIn":true,"authMethod":"oauth","email":"a@x.com","subscriptionType":"max"}"#.into(),
                stderr: String::new(),
                timed_out: false,
            },
            Err("unreachable".into()),
        );
        service.probe_one(ProviderId::ClaudeCode);
        assert_eq!(
            service.status(ProviderId::ClaudeCode).headline_text(),
            "Connected · a@x.com · Claude Max"
        );
    }

    #[test]
    fn codex_null_account_with_openai_auth_required_is_signed_out() {
        let _env = sandbox();
        let mut versions = HashMap::new();
        versions.insert(ProviderId::Codex, ok_version("codex-cli 0.144.6"));
        let mut service = scripted_service(
            versions,
            ok_version("x"),
            Ok(provider_codex::probe::CodexProbe::signed_out()),
        );
        service.probe_one(ProviderId::Codex);
        assert_eq!(service.status(ProviderId::Codex).headline(), Headline::SignedOut);
    }

    #[test]
    fn cache_round_trips_every_field() {
        let env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        let mut status = ProviderStatus::checking(ProviderId::Muse);
        status.checked_at = Some(1700000000);
        status.installed = Installed::yes("1.4.0", "/bin/muse");
        status.auth = Auth::SignedIn {
            email: Some("a@x.com".into()),
            plan: Some("Ultra".into()),
            method: Some("account".into()),
        };
        status.set_usage(UsageSnapshot {
            provider: "muse".into(),
            plan: Some("Ultra".into()),
            windows: vec![UsageWindow {
                label: "weekly".into(),
                used_fraction: 0.02,
                resets_at: Some(1700003600),
            }],
            as_of: 1700000000,
        });
        service.put(status);
        service.save_cache();
        assert!(env.state_dir().join("provider-status.json").is_file());
        let mut reread = Service::with_probes(Probes::never());
        // A fresh service knows nothing until it overlays the cache —
        // that read is what boot does before any probe.
        reread.load_cache();
        let loaded = reread.status(ProviderId::Muse);
        assert_eq!(loaded.installed, Installed::yes("1.4.0", "/bin/muse"));
        assert_eq!(loaded.headline_text(), "Connected · a@x.com · Ultra");
        assert_eq!(loaded.usage.as_ref().unwrap().windows.len(), 1);
    }

    #[test]
    fn muse_resolves_through_fallbacks_when_path_lacks_it() {
        // The Dock case: `PATH` names almost nothing, but `muse` sits in
        // `~/.local/bin`. The production resolver must still find it —
        // through the injected `resolve` surface the existing tests drive.
        let _lock = crate::store::test_env_lock();
        let old_path = std::env::var_os("PATH");
        let old_home = std::env::var_os("HOME");
        let old_muse = std::env::var_os("BAAZ_MUSE");
        let old_login = std::env::var_os(provider::env_path::LOGIN_PATH_ENV);
        let home = std::env::temp_dir().join(format!(
            "baaz-status-muse-home-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&home);
        let local_bin = home.join(".local/bin");
        std::fs::create_dir_all(&local_bin).expect("temp home");
        let expected = local_bin.join("muse");
        std::fs::write(&expected, b"fake").expect("seed muse");
        std::env::set_var("HOME", &home);
        std::env::set_var("PATH", "/usr/bin:/bin");
        std::env::remove_var("BAAZ_MUSE");
        std::env::remove_var(provider::env_path::LOGIN_PATH_ENV);
        let probes = Probes {
            resolve: Arc::new(default_resolve),
            run: Arc::new(|_, _| panic!("no run needed: resolution is the claim")),
            codex_server: Arc::new(|_, _| Err("unreachable".into())),
        };
        let resolved = (probes.resolve)(ProviderId::Muse);
        match &old_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        match &old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &old_muse {
            Some(value) => std::env::set_var("BAAZ_MUSE", value),
            None => std::env::remove_var("BAAZ_MUSE"),
        }
        match &old_login {
            Some(value) => std::env::set_var(provider::env_path::LOGIN_PATH_ENV, value),
            None => std::env::remove_var(provider::env_path::LOGIN_PATH_ENV),
        }
        let _ = std::fs::remove_dir_all(&home);
        assert_eq!(resolved, Some(expected));
    }

    #[test]
    fn set_enabled_persists_through_the_cache() {
        let _env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        service.set_enabled(ProviderId::Codex, false);
        assert!(!service.status(ProviderId::Codex).enabled);
        let mut reread = Service::with_probes(Probes::never());
        reread.load_cache();
        assert!(
            !reread.status(ProviderId::Codex).enabled,
            "the switch survives a restart through the cache"
        );
    }

    #[test]
    fn recheck_probes_and_keeps_the_switch_off() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;
        let _env = sandbox();
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = runs.clone();
        let probes = Probes {
            resolve: Arc::new(|_| Some(PathBuf::from("/bin/muse"))),
            run: Arc::new(move |_, _| {
                counted.fetch_add(1, Ordering::Relaxed);
                ok_version("1.4.0")
            }),
            codex_server: Arc::new(|_, _| Err("unused".into())),
        };
        let mut service = Service::with_probes(probes);
        service.set_muse_auth(Some(MuseAccount {
            email: Some("a@x.com".into()),
            plan: Some("Pro".into()),
        }));
        service.set_enabled(ProviderId::Muse, false);
        service.recheck(ProviderId::Muse);
        assert_eq!(runs.load(Ordering::Relaxed), 1, "re-check probes once");
        assert!(
            !service.status(ProviderId::Muse).enabled,
            "a re-probe never re-enables a disabled provider"
        );
    }

    #[test]
    fn deterministic_mode_touches_nothing_and_reads_the_script() {
        let env = sandbox();
        env.set_deterministic(true);
        let script = r#"[{"provider":"codex","installed":{"Yes":{"version":"0.144.6","path":"/bin/codex"}},"auth":"SignedOut","enabled":true,"advisory":"None","checked_at":1,"usage":null}]"#;
        env.set_var("BAAZ_PROVIDER_STATUS_SCRIPT", script);
        let mut service = Service::with_probes(Probes::panicking());
        service.probe_all();
        assert_eq!(service.status(ProviderId::Codex).headline(), Headline::SignedOut);
        assert!(
            !env.state_dir().join("provider-status.json").exists(),
            "deterministic mode never writes the cache"
        );
    }

    #[test]
    fn lane_readings_land_in_the_single_store_the_cards_render() {
        let _env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        assert!(service.status(ProviderId::Codex).usage.is_none());
        assert!(service.record_lane_usage(
            ProviderId::Codex,
            &provider::UsageReport {
                plan: Some("prolite".into()),
                windows: vec![provider::UsageWindow {
                    label: "Weekly".into(),
                    used_fraction: 0.85,
                    resets_at: Some(1790588038),
                    window_minutes: Some(10080),
                }],
                observed_at: None,
            },
        ));
        let snapshot = service.status(ProviderId::Codex).usage.expect("a reading was stored");
        assert_eq!(snapshot.plan.as_deref(), Some("prolite"));
        assert_eq!(snapshot.windows.len(), 1);
        assert_eq!(snapshot.windows[0].label, "Weekly");
        assert!((snapshot.windows[0].used_fraction - 0.85).abs() < 1e-9);
        // Muse's weekly fraction is the Weekly window under the tier's
        // own plan label.
        assert!(service.record_muse_usage(Some("High Usage".into()), Some(0.02), None, None));
        let muse = service.status(ProviderId::Muse).usage.expect("muse stored");
        assert_eq!(muse.plan.as_deref(), Some("High Usage"));
        assert_eq!(muse.windows.len(), 1);
        assert_eq!(muse.windows[0].label, "Weekly");
    }

    #[test]
    fn refreshed_claude_reading_persists_and_restores_from_cache() {
        // A `rate_limit_event` recording reaches the cache file with its
        // `as_of`: the next launch overlays it and the row shows its age.
        let env = sandbox();
        let before = now_secs();
        record_refreshed_usage(
            &[(
                ProviderId::ClaudeCode,
                provider::UsageReport {
                    plan: None,
                    windows: vec![provider::UsageWindow {
                        label: "Weekly".into(),
                        used_fraction: 0.42,
                        resets_at: None,
                        window_minutes: Some(10080),
                    }],
                    observed_at: None,
                },
            )],
            None,
        );
        let after = now_secs();
        // The cache write runs off this thread: wait for it, bounded.
        let cache = env.state_dir().join("provider-status.json");
        let mut waited = 0;
        while !cache.is_file() && waited < 100 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            waited += 1;
        }
        assert!(cache.is_file(), "the background write landed the cache");
        let cached: Vec<ProviderStatus> = read_cache();
        let claude =
            cached.iter().find(|status| status.provider == ProviderId::ClaudeCode).expect("cached");
        let usage = claude.usage.as_ref().expect("the reading was persisted");
        assert!((before..=after).contains(&usage.as_of), "the reading carries its landing time");
        assert_eq!(usage.windows.len(), 1);
        assert!((usage.windows[0].used_fraction - 0.42).abs() < 1e-9);
        // What boot does: a fresh service overlays the cache before any
        // probe, so the row renders from the persisted reading.
        let mut reread = Service::with_probes(Probes::never());
        reread.load_cache();
        let restored = reread.status(ProviderId::ClaudeCode).usage.expect("restored");
        assert_eq!(restored.as_of, usage.as_of);
        assert_eq!(restored.windows.len(), 1);
    }

    #[test]
    fn menu_usage_refresh_runs_at_most_once_a_minute() {
        // Against the live throttle: the first menu open refreshes, the
        // second (seconds later) does not. No other test touches the
        // throttle, so the first call here owns the window.
        assert!(note_usage_refresh(), "the first open refreshes");
        assert!(!note_usage_refresh(), "the second open rides the first");
    }

    #[test]
    fn a_turn_reading_is_persisted_and_wins_over_an_older_one() {
        // A reading that arrives with a turn reaches the cache file, and a
        // newer turn reading replaces an older held one: the menu shows
        // the newest reading with its true age.
        let env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        service.put(ProviderStatus {
            provider: ProviderId::ClaudeCode,
            installed: Installed::yes("2.1.276", "/bin/claude"),
            auth: Auth::SignedIn { email: None, plan: None, method: None },
            enabled: true,
            advisory: Advisory::None,
            checked_at: Some(1_700_000_000),
            usage: Some(UsageSnapshot {
                provider: "claude-code".into(),
                plan: Some("Max".into()),
                windows: vec![UsageWindow {
                    label: "Weekly".into(),
                    used_fraction: 0.10,
                    resets_at: None,
                }],
                as_of: 1_700_000_000,
            }),
        });
        let before = now_secs();
        assert!(service.record_lane_usage(
            ProviderId::ClaudeCode,
            &provider::UsageReport {
                plan: Some("Max".into()),
                windows: vec![provider::UsageWindow {
                    label: "Weekly".into(),
                    used_fraction: 0.41,
                    resets_at: None,
                    window_minutes: Some(10080),
                }],
                observed_at: None,
            },
        ));
        service.save_cache();
        let stored = service.status(ProviderId::ClaudeCode).usage.expect("the turn reading was stored");
        assert!((before..=now_secs()).contains(&stored.as_of), "the reading carries its landing time");
        assert!((stored.windows[0].used_fraction - 0.41).abs() < 1e-9, "the newer reading wins");
        let cached: Vec<ProviderStatus> = read_cache();
        let claude = cached.iter().find(|status| status.provider == ProviderId::ClaudeCode).expect("cached");
        assert_eq!(claude.usage, Some(stored), "the turn reading survives a restart through the cache");
        let _ = env;
    }

    #[test]
    fn the_usage_throttle_is_per_provider_not_global() {
        // Pure arm, no live state: a fresh refresh of one provider never
        // spends another's window.
        let now = Instant::now();
        let all = ProviderId::all();
        assert_eq!(usage_refresh_due(&HashMap::new(), &all, now), Vec::from(all), "never refreshed counts as due");
        let mut last = HashMap::new();
        last.insert(ProviderId::ClaudeCode, now);
        assert!(usage_refresh_due(&last, &[ProviderId::ClaudeCode], now).is_empty(), "seconds later it rides");
        assert_eq!(
            usage_refresh_due(&last, &[ProviderId::Codex], now),
            vec![ProviderId::Codex],
            "another provider is still due"
        );
        assert_eq!(
            usage_refresh_due(&last, &[ProviderId::ClaudeCode], now + Duration::from_secs(61)),
            vec![ProviderId::ClaudeCode],
            "past the minute it is due again"
        );
    }

    #[test]
    fn menu_open_requests_every_provider_readable_without_a_model_turn() {
        // Three connected providers: all three are requested. Disabled and
        // signed-out providers have nothing to read, so they are not.
        let _env = sandbox();
        let connected = |id| {
            let mut status = ProviderStatus::checking(id);
            status.installed = Installed::yes("1.0", "/bin/x");
            status.auth = Auth::SignedIn { email: None, plan: None, method: None };
            status.checked_at = Some(1_700_000_000);
            status
        };
        let statuses = vec![connected(ProviderId::Muse), connected(ProviderId::ClaudeCode), connected(ProviderId::Codex)];
        assert_eq!(usage_refresh_targets(&statuses), Vec::from(ProviderId::all()));
        let mut disabled = connected(ProviderId::Codex);
        disabled.enabled = false;
        let statuses = vec![connected(ProviderId::Muse), connected(ProviderId::ClaudeCode), disabled];
        assert!(!usage_refresh_targets(&statuses).contains(&ProviderId::Codex), "disabled is omitted");
        let mut signed_out = connected(ProviderId::Codex);
        signed_out.auth = Auth::SignedOut;
        let statuses = vec![connected(ProviderId::Muse), connected(ProviderId::ClaudeCode), signed_out];
        assert!(!usage_refresh_targets(&statuses).contains(&ProviderId::Codex), "signed out has nothing to read");
    }

    #[test]
    fn focus_refresh_is_throttled_to_fifteen_seconds_per_provider() {
        let _env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        let now = Instant::now();
        service.mark_probed(ProviderId::Muse, now);
        assert!(service.due_for_refresh(now).is_empty());
        assert!(service.due_for_refresh(now + Duration::from_secs(14)).is_empty());
        assert_eq!(
            service.due_for_refresh(now + Duration::from_secs(16)),
            vec![ProviderId::Muse]
        );
    }

    #[test]
    fn an_observed_reading_keeps_its_age_and_an_unchanged_one_writes_nothing() {
        // A reading observed at T and persisted later keeps age T — the
        // persisting turn never re-stamps it "just now" — and persisting
        // the same reading again changes nothing (the caller skips its
        // write from the false return).
        let _env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        let report = || provider::UsageReport {
            plan: None,
            windows: vec![provider::UsageWindow {
                label: "Weekly".into(),
                used_fraction: 0.42,
                resets_at: None,
                window_minutes: Some(10080),
            }],
            observed_at: Some(1_700_000_000),
        };
        assert!(service.record_lane_usage(ProviderId::ClaudeCode, &report()));
        assert_eq!(
            service.status(ProviderId::ClaudeCode).usage.as_ref().expect("stored").as_of,
            1_700_000_000,
            "the reading keeps its observation time"
        );
        assert!(
            !service.record_lane_usage(ProviderId::ClaudeCode, &report()),
            "an unchanged reading reports no change, so no write follows"
        );
        assert_eq!(
            service.status(ProviderId::ClaudeCode).usage.as_ref().expect("stored").as_of,
            1_700_000_000,
            "re-persisting never re-stamps the age"
        );
        // A newer observation of the same values moves the age to it.
        let mut newer = report();
        newer.observed_at = Some(1_700_000_100);
        assert!(service.record_lane_usage(ProviderId::ClaudeCode, &newer));
        assert_eq!(
            service.status(ProviderId::ClaudeCode).usage.as_ref().expect("stored").as_of,
            1_700_000_100
        );
    }

    #[test]
    fn an_empty_muse_read_keeps_the_good_numbers_and_notes_it() {
        // An unavailable/no-numbers Muse read never wipes the last good
        // snapshot: the numbers and their age stand, flagged so the row
        // reads "unavailable now" beside them. A good read clears the
        // flag again.
        let _env = sandbox();
        let mut service = Service::with_probes(Probes::never());
        assert!(service.record_muse_usage(Some("High Usage".into()), Some(0.02), None, Some(1_700_000_000)));
        assert!(!service.muse_unavailable_now());
        assert!(!service.record_muse_usage(Some("High Usage".into()), None, None, Some(1_700_000_100)));
        let held = service.status(ProviderId::Muse).usage.expect("the good snapshot stands");
        assert_eq!(held.windows.len(), 1, "the numbers are kept, not wiped");
        assert!((held.windows[0].used_fraction - 0.02).abs() < 1e-9);
        assert_eq!(held.as_of, 1_700_000_000, "their age is kept too");
        assert!(service.muse_unavailable_now(), "the row is told to say unavailable now");
        assert!(service.record_muse_usage(Some("High Usage".into()), Some(0.03), None, Some(1_700_000_200)));
        assert!(!service.muse_unavailable_now(), "a good read clears the note");
        let fresh = service.status(ProviderId::Muse).usage.expect("the good read lands");
        assert!((fresh.windows[0].used_fraction - 0.03).abs() < 1e-9);
        assert_eq!(fresh.as_of, 1_700_000_200);
    }

    #[test]
    fn a_stuck_refreshing_row_settles_through_the_guard() {
        // A "Refreshing…" mark with a completion that never runs still
        // clears: the guard behind `mark_usage_refreshing` bounds the wait
        // (20 s in production; a short delay proves the mechanism here).
        let _env = sandbox();
        mark_usage_refreshing(&[ProviderId::Codex]);
        assert!(usage_refreshing().contains(&ProviderId::Codex), "the row reads Refreshing…");
        clear_usage_refreshing(&[ProviderId::Codex]);
        assert!(!usage_refreshing().contains(&ProviderId::Codex), "a landed read clears it");
        mark_usage_refreshing(&[ProviderId::Codex]);
        clear_usage_refreshing_after(&[ProviderId::Codex], Duration::from_millis(50));
        let mut waited = 0;
        while usage_refreshing().contains(&ProviderId::Codex) && waited < 100 {
            std::thread::sleep(Duration::from_millis(50));
            waited += 1;
        }
        assert!(
            !usage_refreshing().contains(&ProviderId::Codex),
            "the guard clears a never-completing read"
        );
    }

    #[test]
    fn first_run_needs_no_flag_and_no_stored_sessions_or_projects() {
        let env = sandbox();
        assert!(is_first_run(), "an empty state dir is a first run");
        set_onboarding_completed();
        assert!(!is_first_run(), "the flag ends first run");
        std::fs::remove_file(onboarding_completed_path()).ok();
        let mut stored = crate::projects::Projects::default();
        stored.add(std::path::Path::new("/tmp/nowhere"));
        crate::projects::write(&stored);
        assert!(!is_first_run(), "a stored project ends first run");
        let _ = env;
    }
}
