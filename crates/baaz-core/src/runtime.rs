//! What this process runs against, decided once in `main` (docs/82 4.10,
//! audit A R7).
//!
//! A visual-probe capture used to be ~94 `deterministic()` checks in 36
//! files, each reading `BAAZ_DETERMINISTIC` on its own and each meaning
//! something slightly different: freeze the clock, ignore the person's
//! files, script the provider statuses, keep the browser in memory, draw
//! at rest. Those are five separate facts about the run, and this module
//! names them once:
//!
//! * [`Runtime::state_dir`]: where Baaz's own state lives, and whether it
//!   is a capture's isolated store — no user file (layout, keymap, tabs,
//!   provider sessions, hand-off snapshots, the import journal) is read
//!   and none is written.
//! * [`Runtime::clock`]: wall time, or frozen so relative labels read the
//!   same words run to run ([`crate::clock`]).
//! * [`Runtime::provider_factory`]: how sessions and status reach the
//!   providers — spawned CLIs, or the scripted stand-in
//!   ([`Lanes::Scripted`], `--no-connect` / `--replay`); and whether the
//!   install, sign-in, usage and completion probes run or the scripted
//!   statuses answer.
//! * [`Runtime::browser_backend`]: the person's browser stores (tabs,
//!   history, adblock lists, a Chrome profile), or an in-memory one.
//! * [`captures`]: the capture-only presentation hooks — draw settled,
//!   take no keyboard focus, start no watcher, prompt or prewarm, show
//!   fixtures where this machine's git and `gh` would answer. Only a
//!   `probe` build has them; without the feature it is the constant
//!   `false` and every branch on it compiles away.
//!
//! `main` builds one [`Runtime`] from the environment and the command line
//! and [`install`]s it before anything reads state. Most readers have no
//! gpui context (the stores, the clock, provider probes on their own
//! threads), so the runtime is a process value rather than a gpui
//! `Global`. Unit tests never install one: each reader then derives the
//! fact from the environment on the spot, so a test that sets
//! `BAAZ_DETERMINISTIC` or `BAAZ_STATE_DIR` under
//! [`crate::store::test_env_lock`] still sees it. The clock and the
//! presentation hooks are read once per process in that fallback, exactly
//! as the old clock flag was, so a test flipping the variable never
//! freezes a clock under a test running beside it.

use std::path::PathBuf;
use std::sync::OnceLock;

/// The one runtime this process runs against, once `main` installed it.
static INSTALLED: OnceLock<Runtime> = OnceLock::new();

/// Everything a run decides once about the world it runs in. See the module
/// docs for what each part replaces.
#[derive(Clone, Debug)]
pub struct Runtime {
    /// Where Baaz's own state lives, and whether it is a capture's store.
    pub state_dir: StateDir,
    /// Wall time, or frozen for a capture.
    pub clock: Clock,
    /// How sessions and status reach the providers.
    pub provider_factory: ProviderSource,
    /// The browser's stores: the person's, or in memory.
    pub browser_backend: BrowserBackend,
    /// The capture-only presentation hooks ([`captures`]).
    captures: bool,
}

/// Where Baaz's state lives this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StateDir {
    /// `BAAZ_STATE_DIR`, a capture's throwaway directory, or
    /// `~/Library/Application Support/baaz`.
    pub path: PathBuf,
    /// A capture's store: the person's files are never read and the user
    /// stores are never written, whatever directory the run names, so a
    /// baseline never depends on one machine's afternoon.
    pub isolated: bool,
    /// The directory is this process's own throwaway store, removed as the
    /// capture ends ([`crate::store::remove_capture_state`]).
    pub throwaway: bool,
}

/// The clock every label and elapsed cell reads ([`crate::clock`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Clock {
    /// `Instant::now()` and `Local::now()`.
    Wall,
    /// Frozen at first use, and "now" for data is the newest timestamp in
    /// the data: a capture reads the same words however old its fixture is.
    Frozen,
}

impl Clock {
    /// Whether this clock is frozen.
    pub fn is_frozen(self) -> bool {
        self == Clock::Frozen
    }
}

/// How the window's session lanes are opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lanes {
    /// Spawn each provider's CLI (Muse lanes ride the shared hub).
    Spawn,
    /// The scripted stand-in until Muse connects: `--no-connect` and
    /// `--replay` runs open lanes that send nothing anywhere.
    Scripted,
}

/// How sessions and status reach the providers this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderSource {
    /// How session lanes open ([`crate::providers::ProviderFactory`]).
    pub lanes: Lanes,
    /// Whether the scripted statuses answer for every provider: no CLI is
    /// resolved or run for install, sign-in, usage or one-shot completions,
    /// the status cache is neither read nor written, and the setup sheet's
    /// terminal plays a script.
    pub scripted: bool,
}

/// The browser's stores this run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BrowserBackend {
    /// `tabs.json`, `browser.db`, the downloaded adblock lists and the
    /// person's Chrome profile.
    Native,
    /// Nothing read from or written to disk: tabs start empty, history is
    /// in memory, the bundled adblock lists apply, no Chrome profile.
    Memory,
}

impl BrowserBackend {
    /// Whether nothing of the browser's touches disk this run.
    pub fn in_memory(self) -> bool {
        self == BrowserBackend::Memory
    }
}

impl Runtime {
    /// The runtime `args` and the environment describe: a capture
    /// (`BAAZ_DETERMINISTIC=1`, probe builds only) isolates the state,
    /// freezes the clock, scripts the provider statuses, keeps the browser
    /// in memory and draws at rest; the run's command line picks `lanes`
    /// (`--no-connect` / `--replay` open scripted ones: [`Lanes::scripted_if`]).
    pub fn new(lanes: Lanes) -> Self {
        let mut runtime = Self::from_env(env_captures());
        runtime.provider_factory.lanes = lanes;
        runtime
    }

    /// The runtime of a run whose capture flag is `capture`, lanes spawned.
    fn from_env(capture: bool) -> Self {
        Self {
            state_dir: StateDir::resolve(capture),
            clock: if capture { Clock::Frozen } else { Clock::Wall },
            provider_factory: ProviderSource { lanes: Lanes::Spawn, scripted: capture },
            browser_backend: if capture { BrowserBackend::Memory } else { BrowserBackend::Native },
            captures: capture,
        }
    }
}

impl Lanes {
    /// `--no-connect` and `--replay` (and `--bench`, which rides the replay
    /// lane) open scripted lanes; everything else spawns. `offline_or_replay`
    /// is whether the command line asked for either.
    pub fn scripted_if(offline_or_replay: bool) -> Self {
        if offline_or_replay {
            Lanes::Scripted
        } else {
            Lanes::Spawn
        }
    }
}

impl StateDir {
    /// The state directory a run with capture flag `capture` uses:
    /// `BAAZ_STATE_DIR` when named, a per-process temp dir under unit tests,
    /// a capture's throwaway store, else the real one.
    fn resolve(capture: bool) -> Self {
        if let Some(path) = crate::store::state_dir_override() {
            return Self { path, isolated: capture, throwaway: false };
        }
        // A test that forgot to point the store somewhere disposable must
        // never write the person's real state: `cargo test` once rewrote
        // their `projects.json`. Unit tests fall back to a per-process temp
        // dir.
        if cfg!(any(test, feature = "test-support")) {
            let path = std::env::temp_dir().join(format!("baaz-test-state-{}", std::process::id()));
            // Made as it is named: a missing dir canonicalizes to its raw
            // `/var/…` spelling, and `projects::resolved` memoizes that for a
            // while, so whichever test looked first decided whether this run's
            // own workspace (`/private/var/…` once made) read as its own —
            // `sidebar::a_session_another_client_made_is_not_listed` failed
            // under a loaded workspace run.
            let _ = std::fs::create_dir_all(&path);
            return Self { path, isolated: capture, throwaway: false };
        }
        // The same rule for a capture (the visual probe): an entry that
        // names no state dir of its own must never write the person's real
        // store. Before this, `composer-*`, `lane-*` and `reopen-failed`
        // captures left probe prompts in the owner's history and set
        // `provider.json`.
        if capture {
            return Self { path: capture_state_dir(), isolated: true, throwaway: true };
        }
        Self { path: crate::store::default_support_dir(), isolated: false, throwaway: false }
    }
}

/// The throwaway store a capture with no `BAAZ_STATE_DIR` uses: one fresh
/// directory per process, so two shots of the same entry start from the
/// same (empty) state.
pub fn capture_state_dir() -> PathBuf {
    std::env::temp_dir().join(format!("baaz-capture-state-{}", std::process::id()))
}

/// Make `runtime` the process's runtime. `main` calls this once, after the
/// command line is parsed and before anything reads state; a second call is
/// ignored (the first runtime stands).
pub fn install(runtime: Runtime) {
    let _ = INSTALLED.set(runtime);
}

/// Whether the environment asks for a capture: `BAAZ_DETERMINISTIC=1`, which
/// the visual probe always sets. Read live. Never in a build without the
/// `probe` feature (`main` refuses the variable there).
pub fn env_captures() -> bool {
    cfg!(feature = "probe") && std::env::var("BAAZ_DETERMINISTIC").as_deref() == Ok("1")
}

/// [`env_captures`] read once per process: the uninstalled fallback for the
/// clock and the presentation hooks.
fn env_captures_once() -> bool {
    static FLAG: OnceLock<bool> = OnceLock::new();
    *FLAG.get_or_init(env_captures)
}

/// The capture-only presentation hooks: draw settled (menus, sheets,
/// toasts and heroes at rest, reduced motion), take no keyboard focus, start
/// no file watcher, watchdog, prompt or prewarm, and show fixtures where
/// this machine's git, `gh` and new-tab wallpaper would answer. The probe
/// sets them; a build without the `probe` feature has none.
pub fn captures() -> bool {
    if !cfg!(feature = "probe") {
        return false;
    }
    INSTALLED.get().map_or_else(env_captures_once, |runtime| runtime.captures)
}

/// The run's clock.
pub fn clock() -> Clock {
    match INSTALLED.get() {
        Some(runtime) => runtime.clock,
        None if env_captures_once() => Clock::Frozen,
        None => Clock::Wall,
    }
}

/// The run's state directory.
pub fn state_dir() -> StateDir {
    match INSTALLED.get() {
        Some(runtime) => runtime.state_dir.clone(),
        None => StateDir::resolve(env_captures_once()),
    }
}

/// Whether the person's files are off limits this run ([`StateDir::isolated`]).
pub fn state_isolated() -> bool {
    INSTALLED.get().map_or_else(env_captures, |runtime| runtime.state_dir.isolated)
}

/// Whether the scripted statuses answer for every provider
/// ([`ProviderSource::scripted`]).
pub fn providers_scripted() -> bool {
    INSTALLED.get().map_or_else(env_captures, |runtime| runtime.provider_factory.scripted)
}

/// How a window opens its session lanes: the installed runtime's choice,
/// else `from_args`, what the window's own command line says.
pub fn lanes_for(from_args: Lanes) -> Lanes {
    INSTALLED.get().map_or(from_args, |runtime| runtime.provider_factory.lanes)
}

/// The browser's stores this run.
pub fn browser_backend() -> BrowserBackend {
    match INSTALLED.get() {
        Some(runtime) => runtime.browser_backend,
        None if env_captures() => BrowserBackend::Memory,
        None => BrowserBackend::Native,
    }
}

/// The per-window capture hooks a `--screenshot` run reads and the
/// `--steps` runner sets.
///
/// A token rather than a callback because the capture runs outside the entity
/// tree: `shot::capture_and_quit` holds a `WindowHandle`, not the session. A
/// token rather than a pair of process globals because a second window would
/// then corrupt the first one's wait (finding `support-16`):
/// the app's `Harness` owns one, hands the same one to every session view
/// it opens, and `main` hands the same one to the capture, so each window
/// waits on its own flags. Without the `probe` feature it holds nothing and
/// every setter is a no-op.
#[derive(Clone, Default)]
pub struct CaptureToken(#[cfg(feature = "probe")] std::sync::Arc<CaptureFlags>);

/// What a [`CaptureToken`] carries.
#[cfg(feature = "probe")]
#[derive(Default)]
pub struct CaptureFlags {
    /// Whether the open session is showing a pending approval right now. The
    /// session sets it on every frame it renders.
    pending_approval: std::sync::atomic::AtomicBool,
    /// Whether a `--steps` list is still running.
    ///
    /// A scripted capture used to race its own script: the delay is measured
    /// from the first frame, and a step list containing a `wait:` outlives it,
    /// so the PNG showed the window before the steps that were the point of
    /// taking it.
    steps_running: std::sync::atomic::AtomicBool,
    /// Whether the app can execute a `--steps` list at all: the window is
    /// open, the wire is connected (or the run has none), and the boot
    /// session has been decided. Set just before the drain, so the capture
    /// can tell "not ready yet" apart from "ready, and the script already
    /// finished between two polls".
    steps_ready: std::sync::atomic::AtomicBool,
    /// Whether the `--steps`/`--login-steps` task reached the end of its
    /// list. Set on every exit path, including the early ones, so the
    /// capture can tell "the script ran to completion" apart from "the drain
    /// never happened".
    steps_done: std::sync::atomic::AtomicBool,
    /// The sidebar's hover peek held open (`sidebar-peek`), so a capture can
    /// show the peek without a pointer.
    peek_pinned: std::sync::atomic::AtomicBool,
}

impl CaptureToken {
    /// Called by the session view each frame: is a card waiting on the person?
    pub fn set_pending_approval(&self, pending: bool) {
        #[cfg(feature = "probe")]
        self.0.pending_approval.store(pending, std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(feature = "probe"))]
        let _ = pending;
    }

    /// Whether the sidebar's peek is held open by the probe. Always `false`
    /// without the `probe` feature.
    pub fn peek_pinned(&self) -> bool {
        #[cfg(feature = "probe")]
        return self.0.peek_pinned.load(std::sync::atomic::Ordering::Relaxed);
        #[cfg(not(feature = "probe"))]
        false
    }
}

#[cfg(feature = "probe")]
impl CaptureToken {
    /// `sidebar-peek`: hold the sidebar's peek open, or let it go.
    pub fn toggle_peek_pinned(&self) {
        self.0.peek_pinned.fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a card is waiting on the person.
    pub fn pending_approval(&self) -> bool {
        self.0.pending_approval.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Called by the application around its `--steps` and `--login-steps` loops.
    pub fn set_steps_running(&self, running: bool) {
        self.0.steps_running.store(running, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether a `--steps` list is still running.
    pub fn steps_running(&self) -> bool {
        self.0.steps_running.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Called by the application when it first becomes able to execute a
    /// `--steps` list (see [`CaptureFlags::steps_ready`]).
    pub fn set_steps_ready(&self, ready: bool) {
        self.0.steps_ready.store(ready, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the app can execute a `--steps` list.
    pub fn steps_ready(&self) -> bool {
        self.0.steps_ready.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Called by the application when its `--steps`/`--login-steps` task
    /// ends, on every exit path (see [`CaptureFlags::steps_done`]).
    pub fn set_steps_done(&self, done: bool) {
        self.0.steps_done.store(done, std::sync::atomic::Ordering::Relaxed);
    }

    /// Whether the steps task reached the end of its list.
    pub fn steps_done(&self) -> bool {
        self.0.steps_done.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scheduling loops in `shot` all start from this: a step list that
    /// has not been armed yet reads exactly like one that already finished,
    /// so the capture's first wait ("has it started") has to exist at all —
    /// this is the state it is waiting to see flip.
    #[test]
    fn a_fresh_capture_token_has_nothing_running_or_pending() {
        let token = CaptureToken::default();
        assert!(!token.pending_approval());
        assert!(!token.steps_running());
        assert!(!token.steps_ready());
        assert!(!token.steps_done());
        assert!(!token.peek_pinned());
    }

    #[test]
    fn steps_running_round_trips() {
        let token = CaptureToken::default();
        token.set_steps_running(true);
        assert!(token.steps_running());
        token.set_steps_running(false);
        assert!(!token.steps_running());
    }

    #[test]
    fn steps_ready_and_done_round_trip() {
        let token = CaptureToken::default();
        token.set_steps_ready(true);
        assert!(token.steps_ready());
        token.set_steps_done(true);
        assert!(token.steps_done());
    }

    #[test]
    fn pending_approval_and_the_peek_round_trip() {
        let token = CaptureToken::default();
        token.set_pending_approval(true);
        assert!(token.pending_approval());
        token.set_pending_approval(false);
        assert!(!token.pending_approval());
        token.toggle_peek_pinned();
        assert!(token.peek_pinned());
        token.toggle_peek_pinned();
        assert!(!token.peek_pinned());
    }

    /// A capture is all five facts at once; a plain run is none of them.
    #[test]
    fn a_capture_runtime_sets_every_fact_and_a_plain_one_none() {
        let capture = Runtime::from_env(true);
        assert!(capture.captures && capture.clock.is_frozen());
        assert!(capture.provider_factory.scripted && capture.state_dir.isolated);
        assert_eq!(capture.browser_backend, BrowserBackend::Memory);
        let plain = Runtime::from_env(false);
        assert!(!plain.captures && !plain.clock.is_frozen());
        assert!(!plain.provider_factory.scripted && !plain.state_dir.isolated);
        assert_eq!(plain.browser_backend, BrowserBackend::Native);
        assert_eq!(plain.provider_factory.lanes, Lanes::Spawn);
    }
}
