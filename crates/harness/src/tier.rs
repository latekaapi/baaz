//! Which credential tier this login is on, and the guard built on top of it
//! (Phase 5 A1).
//!
//! # The finding
//!
//! Muse has two credential tiers, **pay-as-you-go** and **subscription**, and
//! the tier is decided by the login token rather than by anything the harness
//! sends. The owner's token from 2026-09-08 was on pay-as-you-go, so Phases 1
//! to 4 billed every turn as API usage even though `auth.json` said
//! `mechanism: oauth`. Nothing on the wire says which tier a token is on:
//! `initialize` and `model/list` carry no account or plan field, `auth.json`
//! carries only the mechanism, the storage, the API base and the person's name
//! and email, and the session log records only a `credential_backend`.
//!
//! # The one oracle
//!
//! The `muse` TUI's `/upgrade` card. It reads either
//!
//! > You are currently subscribed to the *Muse Code High Usage* plan.
//! > Current 2% used · Resets at … Weekly 2% used · Resets …
//!
//! or "you're on pay-as-you-go" / "Subscriptions aren't currently available
//! for your account". So [`probe`] runs `muse` under a pseudo-terminal with no
//! prompt, answers the cursor-position query the TUI opens with
//! (`ESC [ 6 n` → `ESC [ 1;1 R`, without which it never draws), types
//! `/upgrade`, presses Enter and reads the card. **Opening the TUI starts a
//! session record and makes no model call**, so the probe costs nothing.
//!
//! Two rules this module keeps:
//!
//! * **The raw terminal output is never logged.** Only the parsed plan name
//!   and the two percentages ever leave this module.
//! * **A failed probe never stops the app.** Every failure is
//!   [`Tier::Unavailable`], which shows a quiet banner and blocks nothing.
//!
//! The result is cached in `~/Library/Application Support/harness/tier.json`
//! keyed by the modification time of `auth.json`, so a logout and a fresh
//! login re-probe and an ordinary boot does not.

use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The ceiling on one probe, start to finish.
const PROBE_CEILING: Duration = Duration::from_secs(20);
/// How long to let the TUI paint before typing at it.
const SETTLE: Duration = Duration::from_millis(4_000);
/// How long the palette gets between the text and the Enter.
const PALETTE: Duration = Duration::from_millis(700);
/// The pty's size. Wide enough that the card's lines are not folded into
/// pieces that no longer contain the words being matched.
const ROWS: u16 = 60;
const COLS: u16 = 200;
/// How long the `/upgrade` card gets to finish drawing before it is read.
const CARD: Duration = Duration::from_secs(6);
/// One poll of the master side.
const TICK: Duration = Duration::from_millis(60);
/// How long past [`PROBE_CEILING`] an exit waits for a probe thread before
/// leaving anyway. `probe` always finishes below the ceiling, so the wait
/// always succeeds; the bound is what keeps a stuck probe from holding an
/// exit open, and the kill that follows is what keeps it from leaking.
const JOIN_GRACE: Duration = Duration::from_secs(5);
/// The most terminal output one probe will hold. The card arrives in the first
/// few tens of kilobytes; a TUI that decided to redraw forever is a bug, not a
/// reason to grow without bound.
const MAX_OUTPUT: usize = 512 * 1024;

/// What the login token is entitled to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "camelCase")]
pub enum Tier {
    /// A subscription plan, with whatever the card said about it.
    #[serde(rename_all = "camelCase")]
    Subscription {
        /// The plan's name, e.g. `Muse Code High Usage`.
        plan: String,
        /// The current window's usage, as a whole percent. `None` either
        /// because the card has not finished drawing yet, or because
        /// `usage_unavailable` below says it never will.
        current_pct: Option<u32>,
        /// The weekly window's usage, as a whole percent. Same absence rule
        /// as `current_pct`.
        weekly_pct: Option<u32>,
        /// The reset clauses, in the order the card printed them.
        resets: Vec<String>,
        /// The card said `Usage currently unavailable` instead of either
        /// percentage (muse 1.3.0): a known plan whose usage windows the
        /// server is not reporting right now — seen while this login's
        /// quota sits fully spent, waiting on its next reset. Distinct from
        /// `current_pct`/`weekly_pct` simply being `None` because the card
        /// has not finished drawing: this is the card's own final word, so
        /// the probe treats it as a complete answer rather than waiting out
        /// its deadline for numbers that are never coming.
        usage_unavailable: bool,
    },
    /// The card said pay-as-you-go: **every turn bills API usage**.
    PayAsYouGo,
    /// The probe could not tell. Never blocks anything.
    Unavailable(String),
}

impl Tier {
    /// The sidebar footer's third row.
    ///
    /// The plan drops a leading "Muse Code": the footer sits in a Muse client,
    /// under a Muse account, beside the Muse mark, and the sidebar is narrow
    /// enough that the product's name is what crowds out the part that
    /// matters. `/status` keeps the full name.
    pub fn footer_label(&self) -> String {
        match self {
            Tier::Subscription { plan, .. } => plan.strip_prefix("Muse Code ").unwrap_or(plan).to_owned(),
            Tier::PayAsYouGo => "Pay-as-you-go".to_owned(),
            Tier::Unavailable(_) => "Plan unknown".to_owned(),
        }
    }

    /// The sidebar footer's usage meter: the weekly window's fraction in
    /// `0..=1`, or `None` when the probe said nothing usable. This is the only
    /// place the weekly number is drawn — [`Self::footer_label`] above it
    /// names the plan and stops there (D5).
    pub fn weekly_fraction(&self) -> Option<f32> {
        match self {
            Tier::Subscription { weekly_pct: Some(pct), .. } => Some((*pct as f32 / 100.0).clamp(0.0, 1.0)),
            _ => None,
        }
    }

    /// Whether that row is warning-tinted: anything that is not a known
    /// subscription is.
    pub fn is_warning(&self) -> bool {
        !matches!(self, Tier::Subscription { .. })
    }

    /// The `/status` and `/usage` lines for this tier.
    pub fn status_lines(&self) -> String {
        match self {
            Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable } => {
                let pct = |p: &Option<u32>| {
                    match p {
                        Some(p) => format!("{p}%"),
                        None if *usage_unavailable => "unavailable".to_owned(),
                        None => "—".to_owned(),
                    }
                };
                let mut text = format!(
                    "Plan: {plan}\nCurrent usage: {} used\nWeekly usage: {} used",
                    pct(current_pct),
                    pct(weekly_pct)
                );
                for reset in resets {
                    text.push('\n');
                    text.push_str(reset);
                }
                text
            }
            Tier::PayAsYouGo => {
                "Plan: pay-as-you-go — every turn bills API usage".to_owned()
            }
            Tier::Unavailable(reason) => format!("Plan: unknown ({reason})"),
        }
    }
}

/// The cache file: one tier, keyed by the `auth.json` it was probed against.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Cached {
    /// `auth.json`'s modification time in whole seconds since the epoch, which
    /// is what makes a logout and a re-login re-probe.
    pub auth_mtime: Option<u64>,
    /// What the probe said.
    pub tier: Option<Tier>,
}

/// `~/Library/Application Support/harness/tier.json`.
pub fn cache_path() -> PathBuf {
    crate::store::support_dir().join("tier.json")
}

/// `auth.json`'s modification time in whole seconds, or `None` when it has
/// none (which is also true when there is no `auth.json` at all).
pub fn auth_mtime() -> Option<u64> {
    let meta = std::fs::metadata(crate::auth::auth_path()).ok()?;
    let modified = meta.modified().ok()?;
    modified.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// How long a cached answer is trusted before an ordinary boot probes again
/// (owner round 2, S4). A forced probe — "Check again", `/usage`, `/status`
/// — always re-probes; a second window booting inside the hour reuses the
/// first's answer instead of driving a second TUI at the same workspace.
const CACHE_TTL: Duration = Duration::from_secs(3600);

/// The cached tier, when it was probed against the `auth.json` that is on disk
/// now and inside [`CACHE_TTL`]. A stale entry — a login or a logout since,
/// or an answer older than the hour — reads as `None`, which is what makes
/// the caller re-probe.
pub fn cached() -> Option<Tier> {
    let modified = std::fs::metadata(cache_path()).ok()?.modified().ok()?;
    if modified.elapsed().ok()? > CACHE_TTL {
        return None;
    }
    let cached: Cached = crate::store::read_json(&cache_path());
    if cached.auth_mtime != auth_mtime() {
        return None;
    }
    cached.tier
}

/// Remember `tier` against the `auth.json` on disk now. Best-effort: a store
/// that cannot be written means the next boot probes again, which is a cost
/// and not a failure.
pub fn remember(tier: &Tier) {
    let cached = Cached { auth_mtime: auth_mtime(), tier: Some(tier.clone()) };
    if let Ok(text) = serde_json::to_vec_pretty(&cached) {
        let _ = crate::store::write_atomic(&cache_path(), &text);
    }
}

/// The `--print-tier` field text for one usage percentage: `"N% used"`,
/// the card's own word `"unavailable"` with no dangling "used" appended, or
/// `"— used"` while the probe has not been answered at all. Pure, so the
/// "unavailable" case has a unit test independent of a live probe.
fn print_tier_field(p: Option<u32>, usage_unavailable: bool) -> String {
    match p {
        Some(p) => format!("{p}% used"),
        None if usage_unavailable => "unavailable".to_owned(),
        None => "\u{2014} used".into(),
    }
}

/// `--print-tier`: probe and print, without opening a window.
///
/// The one way to ask this question from a script — the plan the login is on
/// decides what every turn costs, and a person should be able to check it
/// without launching the app. It prints the parsed fields and never the
/// terminal's own bytes, and it makes **no model call**.
pub fn print_and_exit(muse: &str) -> ! {
    // Joined, not called: `process::exit` below runs no destructors, so a
    // `Pty` still alive at that point would never be dropped and its child
    // would orphan. Joining first means the child is SIGKILLed and reaped
    // before the process leaves.
    let probed = probe_blocking(muse);
    // A probe is a probe: the window's cache is refreshed by this one too, so
    // checking the plan from a script saves the next boot the wait.
    if let Ok(tier) = &probed {
        remember(tier);
    }
    match probed {
        Ok(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }) => {
            println!("Subscription: {plan}");
            println!("Current: {}", print_tier_field(current_pct, usage_unavailable));
            println!("Weekly: {}", print_tier_field(weekly_pct, usage_unavailable));
            for reset in resets {
                println!("{reset}");
            }
            std::process::exit(0)
        }
        Ok(Tier::PayAsYouGo) => {
            println!("Pay-as-you-go: every turn bills API usage.");
            std::process::exit(0)
        }
        Ok(Tier::Unavailable(reason)) | Err(reason) => {
            println!("Plan unknown: {reason}");
            std::process::exit(1)
        }
    }
}

/// The workspace the probe's own TUI session is opened in.
///
/// Deliberately **not** the window's workspace: opening the TUI writes a
/// session record, and a probe should not put a row in the sidebar of the
/// project someone is working on.
fn probe_workspace() -> PathBuf {
    let dir = crate::store::support_dir().join("tier-probe");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The pids of probe children this process still owns. [`kill_live_probes`]
/// SIGKILLs them on exit paths that cannot wait for the probe thread; every
/// [`Pty`] removes its own pid on drop.
static LIVE_PROBES: std::sync::Mutex<Vec<u32>> = std::sync::Mutex::new(Vec::new());

/// SIGKILL every probe child this process still owns.
///
/// For exit paths that cannot join the probe thread (the screenshot's
/// `quit`): the thread then finishes against a dead child and its `Drop`
/// reaps it. SIGKILL, not SIGTERM — the TUI ignores SIGTERM, and on
/// 2026-09-09 two orphaned probes survived it for six hours and died on
/// SIGKILL. A SIGKILLed child whose `Pty` is already gone is reaped by init;
/// one whose `Pty` is still here is reaped by its `Drop`.
pub fn kill_live_probes() {
    let live: Vec<u32> = LIVE_PROBES.lock().map(|live| live.clone()).unwrap_or_default();
    for pid in live {
        // SAFETY: `kill` with a pid and a signal neither reads nor writes.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    }
}

/// Kill live tier-probe children and wait, bounded, for their drops.
///
/// The one cleanup every exit path runs — the screenshot quit, the window's
/// should-close hook, the app-quit hook, and the ⌘W / ⌘Q menu actions — so a
/// probe mid-flight never orphans its `muse` TUI child. Bounded either way:
/// the close or quit proceeds when the wait expires.
pub fn cleanup_probes() {
    kill_live_probes();
    wait_for_probes_gone(Duration::from_secs(3));
}

/// Wait, bounded, for every live probe to be dropped and reaped after
/// [`kill_live_probes`]: the probe thread sees the dead child on its next
/// pump tick and its `Drop` removes the pid file. For exits that want the
/// pid file gone — not just the child dead — before they leave. Bounded
/// either way: the quit proceeds when the wait expires.
pub fn wait_for_probes_gone(timeout: Duration) {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let gone = LIVE_PROBES.lock().map(|live| live.is_empty()).unwrap_or(true);
        if gone {
            return;
        }
        std::thread::sleep(TICK);
    }
}

/// `~/Library/Application Support/harness/tier-probe/probe.pid`: the live
/// probe child's pid, or nothing when no probe is running.
///
/// A harness that is force-quit mid-probe never drops its `Pty`, so the TUI —
/// its own session leader — is reparented to pid 1 and lives on. The pid file
/// is how the next probe finds and SIGKILLs it.
fn probe_pid_path() -> PathBuf {
    probe_workspace().join("probe.pid")
}

/// Read the pid file, or `None` when it is missing or does not parse: either
/// way there is nothing to sweep.
fn read_probe_pid() -> Option<u32> {
    parse_probe_pid(&std::fs::read_to_string(probe_pid_path()).ok()?)
}

/// The pid file holds one decimal pid. Anything else — empty, truncated,
/// pid 1 and below — reads as absent rather than as someone to kill.
fn parse_probe_pid(text: &str) -> Option<u32> {
    let pid: u32 = text.trim().parse().ok()?;
    if pid <= 1 { None } else { Some(pid) }
}

/// Decide about one pid-file entry. The pid is killed only when `is_probe`
/// says its command line is still a probe's, never on the pid alone: pids
/// are recycled, and killing one blind takes out whatever came next.
fn sweep_stale_with(current: Option<u32>, is_probe: &dyn Fn(u32) -> bool, kill: &dyn Fn(u32)) {
    if let Some(pid) = current {
        if is_probe(pid) {
            kill(pid);
        }
    }
}

/// SIGKILL the previous probe's child when it outlived its harness.
fn sweep_stale_probe() {
    sweep_stale_with(read_probe_pid(), &is_probe_child, &|pid| {
        // SAFETY: as in `kill_live_probes`.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    });
}

/// Whether `pid` is still a probe child: its command line names the probe
/// workspace. The workspace path is the check, not the bare word
/// "tier-probe" — pid reuse aside, other command lines can contain that.
fn is_probe_child(pid: u32) -> bool {
    let probe_dir = probe_workspace().to_string_lossy().into_owned();
    probe_cmdline(pid).is_some_and(|line| line.contains(&probe_dir))
}

/// One process's command line, through `ps`.
fn probe_cmdline(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps")
        .arg("-p")
        .arg(pid.to_string())
        .arg("-o")
        .arg("command=")
        .output()
        .ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        None
    }
}

/// How long a probe waits for another harness's probe before going ahead
/// alone (owner round 2, S4). Past one ceiling the holder is either done or
/// stuck; either way the waiter stops waiting.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// One probe at a time across harnesses, held while the TUI runs.
///
/// Two windows probing together drove two TUIs at the same throwaway
/// workspace, and the second read "Plan unknown" while the first owned it.
/// The lock serialises them; the waiter then reuses the winner's fresh
/// cache ([`probe`]) instead of re-driving. Best-effort like the pid file:
/// a lock whose holder died is stolen, and a wait past [`LOCK_WAIT`]
/// proceeds without it rather than failing the probe.
struct ProbeLock {
    path: PathBuf,
    pid: u32,
}

/// Take the probe lock, waiting boundedly. Returns the lock (if taken) and
/// whether any wait happened — a waiter re-checks the cache, which the
/// holder may have refreshed meanwhile.
fn acquire_probe_lock() -> (Option<ProbeLock>, bool) {
    let path = probe_workspace().join("probe.lock");
    let _ = std::fs::create_dir_all(probe_workspace());
    let start = Instant::now();
    let mut waited = false;
    loop {
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(_) => {
                let pid = std::process::id();
                let _ = std::fs::write(&path, pid.to_string());
                return (Some(ProbeLock { path, pid }), waited);
            }
            Err(_) => {
                if lock_is_stale(&path) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                if start.elapsed() >= LOCK_WAIT {
                    return (None, waited);
                }
                waited = true;
                std::thread::sleep(TICK);
            }
        }
    }
}

/// Whether the lock may be taken: its holder is gone, or it outlived any
/// probe — a ceiling and a grace past its write, so a stuck holder cannot
/// wedge every later probe.
fn lock_is_stale(path: &Path) -> bool {
    if let Ok(meta) = std::fs::metadata(path) {
        if let Ok(modified) = meta.modified() {
            if let Ok(age) = modified.elapsed() {
                if age > PROBE_CEILING + JOIN_GRACE + Duration::from_secs(5) {
                    return true;
                }
            }
        }
    }
    let pid: Option<u32> = std::fs::read_to_string(path).ok().and_then(|text| text.trim().parse().ok());
    match pid {
        Some(pid) => !pid_alive(pid),
        // Unparseable: nobody sane holds it.
        None => true,
    }
}

/// Whether `pid` names a live process. Signal 0 performs no action.
fn pid_alive(pid: u32) -> bool {
    if pid <= 1 {
        return false;
    }
    // Past `pid_t` width the cast wraps — `u32::MAX` becomes `-1`, which
    // addresses every signalable process and always answers alive. No real
    // pid lives there, so a corrupt lock file reads as stale instead.
    if pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` with a pid and signal 0 neither reads nor writes.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

impl Drop for ProbeLock {
    fn drop(&mut self) {
        // Only our own entry: a newer probe may hold the file now.
        let mine: Option<u32> =
            std::fs::read_to_string(&self.path).ok().and_then(|text| text.trim().parse().ok());
        if mine == Some(self.pid) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Run [`probe`] on a thread and join it with a bounded wait, so exiting
/// after it cannot orphan the child: the `Pty` is dropped — the child
/// SIGKILLed and reaped — before this returns.
///
/// A thread that somehow outlives the wait has its child SIGKILLed out from
/// under it and the pid file swept, so nothing it owns outlives the harness
/// either; the stuck probe then reports instead of hanging the exit open.
fn probe_blocking(muse: &str) -> Result<Tier, String> {
    let muse = muse.to_owned();
    let probe = std::thread::spawn(move || probe(&muse));
    let deadline = Instant::now() + PROBE_CEILING + JOIN_GRACE;
    while !probe.is_finished() && Instant::now() < deadline {
        std::thread::sleep(TICK);
    }
    if !probe.is_finished() {
        kill_live_probes();
        sweep_stale_probe();
        return Err("the /upgrade card did not answer in time".to_owned());
    }
    match probe.join() {
        Ok(probed) => probed,
        Err(_) => Err("the tier probe panicked".to_owned()),
    }
}

/// Whether a parse is an answer rather than a card still drawing: a
/// subscription counts only once both usage windows arrived. muse 1.2.1
/// draws the plan sentence first and the percentages land later, so a plan
/// without them is "not yet", not "none" — accepting it is what printed
/// `Current — / Weekly —` while the card still had ink to lay (owner round
/// 2, S4). Anything else is complete as parsed.
fn complete(tier: &Tier) -> bool {
    match tier {
        Tier::Subscription { current_pct, weekly_pct, usage_unavailable, .. } => {
            (current_pct.is_some() && weekly_pct.is_some()) || *usage_unavailable
        }
        Tier::PayAsYouGo | Tier::Unavailable(_) => true,
    }
}

/// Drive the TUI and read the `/upgrade` card. Blocking for up to
/// [`PROBE_CEILING`]; call it on a background thread.
///
/// One probe runs at a time across harnesses ([`acquire_probe_lock`]): a
/// second window waits for the first's answer, then reuses its cache when it
/// landed rather than driving a second TUI at the same workspace.
///
/// Every error is a `String` that is safe to show: it names what went wrong,
/// never what the terminal said.
pub fn probe(muse: &str) -> Result<Tier, String> {
    let deadline = Instant::now() + PROBE_CEILING;
    let (_lock, waited) = acquire_probe_lock();
    // Whoever held the lock just remembered: a fresh cache is their answer,
    // seconds old. Without a wait this changes nothing — a forced probe
    // with no contention still probes.
    if waited {
        if let Some(tier) = cached() {
            return Ok(tier);
        }
    }
    let mut pty = Pty::open(muse)?;
    let mut text = String::new();

    // 1. Let it paint, answering the cursor-position query. Without the reply
    //    the TUI waits for the terminal it thinks it is talking to and draws
    //    nothing at all.
    pty.pump(&mut text, SETTLE.min(deadline - Instant::now()));

    // 2. `/upgrade`, then Enter once the palette has caught up.
    pty.write(b"/upgrade")?;
    pty.pump(&mut text, PALETTE);
    pty.write(b"\r")?;
    // Everything read so far is the composer and the slash palette, and the
    // palette's own row for `/upgrade` says the words the card says. Matching
    // it would report the plan the person is being *offered*. Only what the
    // card draws after the Enter counts, so the buffer starts again here.
    text.clear();

    // 3. Let the card finish drawing before reading it. It arrives in pieces,
    //    and one of the early pieces names pay-as-you-go even on a subscribed
    //    account — the upgrade card is, after all, about upgrading — so a
    //    matcher that stopped at the first hit would report the opposite of
    //    the truth. Read the whole window, then parse once. A plan without
    //    its percentages is still drawing (see [`complete`]), so the loop
    //    holds for a complete answer; on the deadline a partial subscription
    //    still names the plan (the footer shows it without a meter), which
    //    beats "did not answer".
    pty.pump(&mut text, CARD.min(deadline.saturating_duration_since(Instant::now())));
    let mut tier = parse_card(&text);
    while tier.as_ref().is_none_or(|t| !complete(t)) && Instant::now() < deadline && text.len() < MAX_OUTPUT {
        pty.pump(&mut text, TICK);
        tier = parse_card(&text).or(tier);
    }

    // 4. Two interrupts is how the TUI is asked to leave; the drop SIGKILLs
    //    it if it declines.
    let _ = pty.write(b"\x03");
    pty.pump(&mut text, Duration::from_millis(200));
    let _ = pty.write(b"\x03");
    pty.pump(&mut text, Duration::from_millis(200));

    // A probe that came back with nothing is a bug in the driver, and the one
    // thing that must never help debug it is the terminal's own bytes. Under
    // `HARNESS_TIER_DEBUG` the driver reports how much it read and which of a
    // fixed list of harmless words it saw — never the output itself.
    if std::env::var_os("HARNESS_TIER_DEBUG").is_some() {
        let flat = flatten(&text);
        let seen: Vec<&str> = [
            "subscribed",
            "pay-as-you-go",
            "upgrade",
            "Upgrade",
            "plan",
            "%",
            "Ask",
            "muse",
            "error",
            "unavailable",
            "trust",
        ]
        .into_iter()
            .filter(|word| flat.contains(word))
            .collect();
        eprintln!("tier probe: {} bytes read, {} after flattening, saw {seen:?}", text.len(), flat.len());
    }

    // Two failures, and they are not the same failure (finding `support-11`).
    // A card that never drew is a probe that did not get an answer; a card
    // that drew and did not parse is a card whose wording this build does not
    // know, and the fix for that is in this file, not on the machine.
    tier.ok_or_else(|| {
        if flatten(&text).trim().is_empty() {
            "the /upgrade card did not answer in time".to_owned()
        } else {
            "the /upgrade card was not recognised; this build's wording may be out of date".to_owned()
        }
    })
}

/// The literal sentences [`parse_card`] keys on.
///
/// Named constants rather than string literals inside the matcher so the
/// wording is pinned in one place and a test can assert it (finding
/// `support-11`): any TUI rewording lands in [`Tier::Unavailable`], which only
/// warns, so the wording is a fact this build depends on and has to state.
pub const CARD_SUBSCRIBED: &str = "subscribed to the";
/// The pay-as-you-go sentences, lowercased, any one of which is a match.
pub const CARD_PAY_AS_YOU_GO: [&str; 4] = [
    "pay-as-you-go",
    "pay as you go",
    "subscriptions aren't currently available",
    "subscriptions are not currently available",
];
/// The sentence muse 1.3.0 draws in place of both percentages while a known
/// plan's usage windows are not being reported — observed while this login's
/// quota sits fully spent (owner round: quota exhausted until 2026-09-21),
/// but the card gives no reason, so this is read as "no numbers, ever, this
/// probe" rather than assumed to mean any one cause.
pub const CARD_USAGE_UNAVAILABLE: &str = "usage currently unavailable";

/// What the card said, or `None` while it has not said it yet.
///
/// Pure, and the only thing that ever looks at the terminal's bytes.
fn parse_card(raw: &str) -> Option<Tier> {
    let text = flatten(raw);
    if let Some(plan) = plan_name(&text) {
        return Some(Tier::Subscription {
            plan,
            current_pct: percent_after(&text, "Current"),
            weekly_pct: percent_after(&text, "Weekly"),
            resets: reset_clauses(&text),
            usage_unavailable: text.to_lowercase().contains(CARD_USAGE_UNAVAILABLE),
        });
    }
    let lower = text.to_lowercase();
    if CARD_PAY_AS_YOU_GO.iter().any(|sentence| lower.contains(sentence)) {
        return Some(Tier::PayAsYouGo);
    }
    None
}

/// `…subscribed to the Muse Code High Usage plan.` → `Muse Code High Usage`.
fn plan_name(text: &str) -> Option<String> {
    let start = text.find(CARD_SUBSCRIBED)? + CARD_SUBSCRIBED.len();
    let rest = text[start..].trim_start();
    let end = rest.find(" plan")?;
    let name = rest[..end].trim().trim_matches('*').trim();
    // The card's sentence is "subscribed to the {plan} usage plan.", so the
    // template's own word comes back glued to the name. Muse Code High Usage
    // is the plan; "Muse Code High Usage usage" is a sentence.
    let name = name.strip_suffix(" usage").unwrap_or(name).trim();
    if name.is_empty() || name.len() > 80 {
        return None;
    }
    Some(name.to_owned())
}

/// The whole percent that follows `keyword`, as in `Current 2% used`.
fn percent_after(text: &str, keyword: &str) -> Option<u32> {
    let start = text.find(keyword)? + keyword.len();
    // Only look as far as the next clause: a missing percentage should read as
    // absent rather than borrow the other window's.
    let window = clip(&text[start..], 40);
    let digits_at = window.find(|c: char| c.is_ascii_digit())?;
    let digits: String = window[digits_at..].chars().take_while(char::is_ascii_digit).collect();
    if !window[digits_at + digits.len()..].starts_with('%') {
        return None;
    }
    digits.parse().ok()
}

/// Every `Resets …` clause, cut where the next clause starts.
fn reset_clauses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(found) = text[from..].find("Resets") {
        let start = from + found;
        let rest = clip(&text[start..], 60);
        // Skip the first byte so the search does not stop on this clause's own
        // leading word.
        let end = ["·", "Weekly", "Current", " as of ", "Manage", "  "]
            .iter()
            .filter_map(|stop| rest[1..].find(stop).map(|i| i + 1))
            .min()
            .unwrap_or(rest.len());
        let clause = rest[..end].trim().trim_end_matches('.').to_owned();
        if clause.len() > "Resets".len() && !out.contains(&clause) {
            out.push(clause);
        }
        from = start + "Resets".len();
    }
    out
}

/// The first `chars` characters of `text`, cut on a character boundary.
fn clip(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((at, _)) => &text[..at],
        None => text,
    }
}

/// Terminal bytes to something matchable: escapes dropped, box drawing and
/// control characters flattened to spaces, runs of whitespace collapsed.
fn flatten(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.next() {
                // CSI and DEC private sequences: through the first final byte.
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: through BEL or ST.
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // `ESC ( B`, `ESC = `, `ESC >` and friends: one more byte.
                Some(_) => {}
                None => {}
            }
            out.push(' ');
            continue;
        }
        // Box drawing, block elements, and anything else that is decoration
        // rather than words.
        let decoration = ('\u{2500}'..='\u{259f}').contains(&c) || c.is_control();
        out.push(if decoration { ' ' } else { c });
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// The pseudo-terminal
// ---------------------------------------------------------------------------

/// The `muse` invocation the probe spawns: the throwaway workspace, sized
/// for the card's own lines, and trusted for this run only.
///
/// A pure builder — no process touched — so the trust flag is a unit-testable
/// fact rather than something only a live probe could catch a regression in.
fn probe_command(muse: &str) -> std::process::Command {
    let mut command = std::process::Command::new(muse);
    command
        .arg("--workspace")
        .arg(probe_workspace())
        // muse 1.3.0 shows a "Do you trust this workspace?" gate on an
        // untrusted `--workspace` before it will draw anything else, and
        // this probe's own scratch directory (nothing to load: no skills,
        // no rules) is never pre-trusted under a fresh `HARNESS_STATE_DIR`.
        // Without this flag the probe's blind `/upgrade`-then-Enter
        // keystrokes land on the trust prompt instead — Enter accepts its
        // default ("Trust and continue"), which dismisses the gate but eats
        // both the command text and the card, so the probe reads a bare
        // idle composer and reports "not recognised". `--trust-workspace`
        // skips the gate for this run only (it does not save the decision),
        // matching the trust posture every other muse child this app spawns
        // already uses (`MuseConfig::trust_workspace`).
        .arg("--trust-workspace")
        .env("TERM", "xterm-256color")
        .env("LINES", ROWS.to_string())
        .env("COLUMNS", COLS.to_string());
    command
}

/// A `muse` TUI on the far side of a pty, and the master side to talk to it.
struct Pty {
    master: RawFd,
    child: std::process::Child,
}

impl Pty {
    /// The probe's TUI in the throwaway workspace, swept and tracked: the
    /// previous run's orphan is SIGKILLed first, and this child's pid joins
    /// the live registry and the pid file so every exit can find it.
    fn open(muse: &str) -> Result<Self, String> {
        // A harness that was force-quit mid-probe left its child behind;
        // SIGKILL it before starting the next one.
        sweep_stale_probe();
        let command = probe_command(muse);
        let pty = Self::spawn(command)?;
        let pid = pty.child.id();
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.push(pid);
        }
        // Best-effort: without it the next probe cannot sweep this one when
        // this harness is force-quit.
        let _ = std::fs::write(probe_pid_path(), pid.to_string());
        Ok(pty)
    }

    /// `openpty`, then `command` on the slave side with its own session and
    /// the slave as its controlling terminal — which is what makes a TUI draw
    /// rather than refuse for want of a tty. The probe and the SIGKILL unit
    /// test share this path, so the test exercises the real kill.
    fn spawn(mut command: std::process::Command) -> Result<Self, String> {
        let (mut master, mut slave): (libc::c_int, libc::c_int) = (-1, -1);
        let mut size =
            libc::winsize { ws_row: ROWS, ws_col: COLS, ws_xpixel: 0, ws_ypixel: 0 };
        // SAFETY: both fds are out-parameters and `size` outlives the call.
        let rc = unsafe {
            libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size)
        };
        if rc != 0 {
            return Err("no pseudo-terminal was available".to_owned());
        }
        // SAFETY: `slave` is open and each `dup` hands one owned fd to one
        // `Stdio`, which closes it.
        let (stdin, stdout, stderr) = unsafe {
            (
                std::process::Stdio::from_raw_fd(libc::dup(slave)),
                std::process::Stdio::from_raw_fd(libc::dup(slave)),
                std::process::Stdio::from_raw_fd(libc::dup(slave)),
            )
        };
        command.stdin(stdin).stdout(stdout).stderr(stderr);
        // SAFETY: `setsid` and `ioctl` are async-signal-safe, which is the
        // whole contract `pre_exec` asks for.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(0, libc::TIOCSCTTY as libc::c_ulong, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().map_err(|e| format!("muse could not be started: {e}"))?;
        // SAFETY: the parent's copy of the slave is done with; the child holds
        // its own three.
        unsafe { libc::close(slave) };
        // Non-blocking, so a pump that has nothing to read returns instead of
        // parking a thread on a TUI that has finished drawing.
        // SAFETY: `master` is open and `F_SETFL` takes an int.
        unsafe { libc::fcntl(master, libc::F_SETFL, libc::O_NONBLOCK) };
        Ok(Self { master, child })
    }

    /// Read whatever is there for `window`, appending it to `text`.
    fn pump(&mut self, text: &mut String, window: Duration) {
        let until = Instant::now() + window;
        let mut buffer = [0u8; 8192];
        while Instant::now() < until {
            // The child may have been SIGKILLed out from under the probe by
            // an exit that could not join this thread: stop pumping a dead
            // child so the probe finishes and the drop — with its pid-file
            // cleanup — happens promptly instead of at the window's end.
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            // SAFETY: `master` is an open fd owned by `self`; the borrow ends
            // with the read.
            let mut file = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(self.master) });
            match file.read(&mut buffer) {
                Ok(0) => return,
                Ok(n) => {
                    text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                    if text.len() >= MAX_OUTPUT {
                        return;
                    }
                    // The TUI will not paint until its cursor-position query is
                    // answered, and it asks again after a redraw.
                    if buffer[..n].windows(4).any(|w| w == b"\x1b[6n") {
                        let _ = self.write(b"\x1b[1;1R");
                    }
                }
                Err(_) => std::thread::sleep(TICK.min(until.saturating_duration_since(Instant::now()))),
            }
        }
    }

    /// Type at the child.
    fn write(&self, bytes: &[u8]) -> Result<(), String> {
        // SAFETY: `master` is open and `bytes` outlives the call.
        let written = unsafe { libc::write(self.master, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
        if written < 0 {
            return Err("the terminal closed before the probe finished".to_owned());
        }
        Ok(())
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let pid = self.child.id();
        // SIGKILL, not SIGTERM: the TUI ignores SIGTERM, and a `Child::kill`
        // that named the wrong signal would read as a fix while leaking the
        // same orphan. The `wait` reaps it, so no zombie outlives the probe
        // while the harness is still running.
        // SAFETY: `kill` with a pid and a signal neither reads nor writes.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        let _ = self.child.wait();
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.retain(|popped| *popped != pid);
        }
        // Only this probe's own entry: a newer probe may have written the
        // file since, and removing its pid would blind the next sweep.
        if read_probe_pid() == Some(pid) {
            let _ = std::fs::remove_file(probe_pid_path());
        }
        // SAFETY: `master` is owned by `self` and closed exactly once.
        unsafe { libc::close(self.master) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_weekly_fraction_feeds_the_footer_meter() {
        let sub = |weekly_pct| Tier::Subscription {
            plan: "High Usage".into(),
            current_pct: None,
            weekly_pct,
            resets: vec![],
            usage_unavailable: false,
        };
        let frac = sub(Some(12)).weekly_fraction().unwrap();
        assert!((frac - 0.12).abs() < 1e-6, "{frac}");
        assert_eq!(sub(None).weekly_fraction(), None);
        assert_eq!(Tier::PayAsYouGo.weekly_fraction(), None);
        assert_eq!(Tier::Unavailable("no".into()).weekly_fraction(), None);
    }

    #[test]
    fn a_subscription_card_yields_the_plan_and_both_percentages() {
        let card = "\u{1b}[2J\u{1b}[1;1H\u{2502} You are currently subscribed to the Muse Code High Usage \
                    plan. \u{2502}\n\u{2502} Current 2% used \u{b7} Resets at 3:00 PM \u{2502}\n\
                    \u{2502} Weekly 7% used \u{b7} Resets Monday \u{2502}\n";
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }) =
            parse_card(card)
        else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(current_pct, Some(2));
        assert_eq!(weekly_pct, Some(7));
        assert_eq!(resets, vec!["Resets at 3:00 PM".to_owned(), "Resets Monday".to_owned()]);
        assert!(!usage_unavailable);
    }

    /// The card this machine's `muse` actually draws, flattened. Its wording is
    /// not what a reasonable person would guess: the plan's name arrives with
    /// the template's own "usage" glued to it, the two windows run together
    /// with no separator, and a footer line follows the second reset.
    #[test]
    fn the_card_this_muse_really_draws_parses() {
        let card = "You are currently subscribed to the Muse Code High Usage usage plan. \
                    Current 0% used \u{b7} Resets at 5:17 PM Weekly 2% used \u{b7} \
                    Resets Sep 14 at 5:30 AM as of 3:27 PM Manage your plan in Account Center";
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }) =
            parse_card(card)
        else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(current_pct, Some(0));
        assert_eq!(weekly_pct, Some(2));
        assert_eq!(resets, vec!["Resets at 5:17 PM".to_owned(), "Resets Sep 14 at 5:30 AM".to_owned()]);
        assert!(!usage_unavailable);
    }

    /// The wording this build reads the card by, pinned (finding
    /// `support-11`).
    ///
    /// `parse_card` matches these sentences literally and anything else falls
    /// through to `Unavailable`, which only warns — so a `muse` that rewords
    /// the card would silently stop reporting the plan. Changing either
    /// constant has to fail here first.
    #[test]
    fn the_cards_wording_is_pinned() {
        assert_eq!(CARD_SUBSCRIBED, "subscribed to the");
        assert_eq!(
            CARD_PAY_AS_YOU_GO,
            [
                "pay-as-you-go",
                "pay as you go",
                "subscriptions aren't currently available",
                "subscriptions are not currently available",
            ]
        );
        assert_eq!(CARD_USAGE_UNAVAILABLE, "usage currently unavailable");
        assert_eq!(CARD_USAGE_UNAVAILABLE, CARD_USAGE_UNAVAILABLE.to_lowercase());
        // Every one of them is lowercase, because the matcher lowercases the
        // card before looking; a capital here would never match.
        for sentence in CARD_PAY_AS_YOU_GO {
            assert_eq!(sentence, sentence.to_lowercase());
            assert_eq!(parse_card(&format!("Some heading. {sentence} today.")), Some(Tier::PayAsYouGo));
        }
        assert!(matches!(
            parse_card(&format!("You are currently {CARD_SUBSCRIBED} Team plan.")),
            Some(Tier::Subscription { .. })
        ));
    }

    #[test]
    fn either_pay_as_you_go_wording_is_pay_as_you_go() {
        assert_eq!(parse_card("Sorry — you're on pay-as-you-go."), Some(Tier::PayAsYouGo));
        assert_eq!(
            parse_card("Subscriptions aren't currently available for your account"),
            Some(Tier::PayAsYouGo)
        );
    }

    #[test]
    fn a_card_that_has_not_arrived_is_not_guessed_at() {
        assert_eq!(parse_card(""), None);
        assert_eq!(parse_card("\u{1b}[?25l\u{1b}[2J   Muse   ready"), None);
    }

    #[test]
    fn a_missing_percentage_is_absent_rather_than_borrowed() {
        let Some(Tier::Subscription { current_pct, weekly_pct, .. }) =
            parse_card("You are currently subscribed to the Team plan. Weekly 4% used")
        else {
            panic!("expected a subscription");
        };
        assert_eq!(current_pct, None);
        assert_eq!(weekly_pct, Some(4));
    }

    #[test]
    fn the_footer_row_names_the_plan_and_warns_on_anything_else() {
        let plan = Tier::Subscription {
            plan: "Muse Code High Usage".into(),
            current_pct: Some(1),
            weekly_pct: Some(2),
            resets: vec![],
            usage_unavailable: false,
        };
        // D5: the label is the plan's name and nothing else. The weekly
        // percentage belongs to the meter row under it, which reads the same
        // number from `weekly_fraction`; printing it twice said one fact in
        // two places and made the row look like two readings.
        assert_eq!(plan.footer_label(), "High Usage");
        assert_eq!(plan.weekly_fraction(), Some(0.02), "the number moved to the meter, it did not vanish");
        assert!(!plan.is_warning());
        assert_eq!(Tier::PayAsYouGo.footer_label(), "Pay-as-you-go");
        assert!(Tier::PayAsYouGo.is_warning());
        assert_eq!(Tier::Unavailable("no tty".into()).footer_label(), "Plan unknown");
        assert!(Tier::Unavailable("no tty".into()).is_warning());
    }

    /// A plan with no percentages reads as "—" (still drawing) unless the
    /// card said usage is unavailable, in which case `/status` says so
    /// plainly instead of a dash that looks like a bug.
    #[test]
    fn the_status_line_distinguishes_still_drawing_from_the_cards_own_word() {
        let still_drawing = Tier::Subscription {
            plan: "Power Usage".into(),
            current_pct: None,
            weekly_pct: None,
            resets: vec![],
            usage_unavailable: false,
        };
        assert!(still_drawing.status_lines().contains("Current usage: — used"));
        let unavailable = Tier::Subscription {
            plan: "Power Usage".into(),
            current_pct: None,
            weekly_pct: None,
            resets: vec![],
            usage_unavailable: true,
        };
        assert!(unavailable.status_lines().contains("Current usage: unavailable used"));
        assert!(unavailable.status_lines().contains("Weekly usage: unavailable used"));
        // Naming the plan either way — this is what keeps a known plan from
        // ever reading as the generic "Muse did not say which plan" banner.
        assert!(unavailable.status_lines().contains("Plan: Power Usage"));
    }

    /// `--print-tier` prints the card's own word, `unavailable`, with no
    /// "used" glued onto it — unlike a real percentage or the still-drawing
    /// dash, both of which are a measurement and read naturally as "N% used"
    /// / "— used".
    #[test]
    fn print_tier_field_does_not_append_used_to_unavailable() {
        assert_eq!(print_tier_field(None, true), "unavailable");
        assert_eq!(print_tier_field(None, false), "\u{2014} used");
        assert_eq!(print_tier_field(Some(42), true), "42% used");
        assert_eq!(print_tier_field(Some(0), false), "0% used");
    }

    /// The probe's own `--workspace` is always run with `--trust-workspace`:
    /// without it, muse 1.3.0's trust gate eats the probe's keystrokes and
    /// the card never draws (v0.1 prep task 2).
    #[test]
    fn the_probe_trusts_its_own_scratch_workspace() {
        let command = probe_command("muse");
        let args: Vec<String> = command.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
        assert!(args.contains(&"--trust-workspace".to_owned()), "{args:?}");
        assert!(args.contains(&"--workspace".to_owned()), "{args:?}");
    }

    #[test]
    fn escapes_and_box_drawing_never_reach_the_matcher() {
        assert_eq!(flatten("\u{1b}[31m\u{2502} a \u{2502}\u{1b}[0m\r\n b"), "a b");
        assert_eq!(flatten("\u{1b}]0;title\u{7}x"), "x");
    }

    #[test]
    fn the_pid_file_parses_or_it_is_ignored() {
        assert_eq!(parse_probe_pid("1234\n"), Some(1234));
        assert_eq!(parse_probe_pid("1234"), Some(1234));
        assert_eq!(parse_probe_pid(""), None);
        assert_eq!(parse_probe_pid("nope"), None);
        assert_eq!(parse_probe_pid("12x4"), None);
        // Pid 1 and below are never someone to kill.
        assert_eq!(parse_probe_pid("1"), None);
        assert_eq!(parse_probe_pid("0"), None);
    }

    #[test]
    fn only_a_live_probe_is_swept_never_a_pid_alone() {
        let killed = std::cell::RefCell::new(Vec::new());
        let kill = |pid: u32| killed.borrow_mut().push(pid);
        // A pid whose command line is still a probe's is killed.
        sweep_stale_with(Some(4242), &|_| true, &kill);
        assert_eq!(*killed.borrow(), vec![4242]);
        // A recycled pid — same number, some other command line — is not.
        killed.borrow_mut().clear();
        sweep_stale_with(Some(4242), &|_| false, &kill);
        assert!(killed.borrow().is_empty());
        // No pid file at all sweeps nothing.
        sweep_stale_with(None, &|_| true, &kill);
        assert!(killed.borrow().is_empty());
    }

    /// The verbatim 1.2.1 card (owner round 2, S4), captured 2026-09-13 and
    /// redacted: the plan name still arrives glued to the template's own
    /// "usage", and both windows carry percentages — `Current 13%`,
    /// `Weekly 36%` on this login. The parser takes both, not the first
    /// partial draw.
    #[test]
    fn the_1_2_1_card_parses_with_its_percentages() {
        let card = "Muse Code 1.2.1 Model set to muse-spark-1.3-contributor \
                    /upgrade Show your subscription plan \
                    You are currently subscribed to the Muse Code Power Usage usage plan. \
                    Current 13% used · Resets at 7:14 PM Weekly 36% used · \
                    Resets Sep 14 at 5:30 AM as of 3:29 PM Manage your plan in Account Center";
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }) =
            parse_card(card)
        else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code Power Usage");
        assert_eq!(current_pct, Some(13));
        assert_eq!(weekly_pct, Some(36));
        assert_eq!(resets, vec!["Resets at 7:14 PM".to_owned(), "Resets Sep 14 at 5:30 AM".to_owned()]);
        assert!(!usage_unavailable);
        assert!(complete(&Tier::Subscription {
            plan,
            current_pct,
            weekly_pct,
            resets,
            usage_unavailable,
        }));
    }

    /// The verbatim 1.3.0 card captured live against this login (v0.1 prep
    /// task 2), while its quota sat fully spent (exhausted until
    /// 2026-09-21): no percentages at all, on either window — the card says
    /// so outright rather than drawing zeroes. Nothing sensitive in it: a
    /// plan name and a public account-center URL, same as every other
    /// pinned card fixture in this file.
    #[test]
    fn the_1_3_0_exhausted_card_names_the_plan_with_no_percentages() {
        let card = "You are currently subscribed to the Muse Code Power Usage usage plan. \
                    Usage currently unavailable Manage your plan in Account Center \
                    (https://accountscenter.meta.com/muse_code)";
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }) =
            parse_card(card)
        else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code Power Usage");
        assert_eq!(current_pct, None);
        assert_eq!(weekly_pct, None);
        assert!(resets.is_empty());
        assert!(usage_unavailable, "the card's own words, not a still-drawing guess");
        // The missing numbers are the card's final word, not a partial
        // draw: the probe must not spend its whole deadline waiting for
        // percentages that are never coming (owner round: quota exhausted
        // until 2026-09-21 printed dashes forever under the old rule).
        assert!(complete(&Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable }));
    }

    /// A plan sentence on its own parses — and reads as still drawing, not
    /// as an answer. 1.2.1 lays the sentence before the usage windows, and
    /// accepting the early match is what printed the dashes.
    #[test]
    fn a_plan_without_its_percentages_is_still_drawing() {
        let partial = parse_card("You are currently subscribed to the Muse Code Power Usage usage plan.")
            .expect("the plan parses early");
        assert!(
            matches!(partial, Tier::Subscription { current_pct: None, weekly_pct: None, .. }),
            "percentages absent, not borrowed: {partial:?}"
        );
        assert!(!complete(&partial), "a plan without percentages is not an answer");
        let full = parse_card(
            "You are currently subscribed to the Muse Code Power Usage usage plan. \
             Current 1% used · Resets soon Weekly 32% used · Resets later",
        )
        .expect("the full card parses");
        assert!(complete(&full));
        assert!(complete(&Tier::PayAsYouGo));
    }

    /// A lock whose holder died is stolen; one whose holder lives is waited
    /// on; garbage is nobody's.
    #[test]
    fn a_dead_lock_is_stolen_and_a_live_one_waits() {
        let dir = std::env::temp_dir().join(format!("harness-lock-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let lock = dir.join("probe.lock");
        // A pid that cannot exist holds nothing.
        std::fs::write(&lock, u32::MAX.to_string()).expect("write lock");
        assert!(lock_is_stale(&lock), "a dead holder is stolen");
        // This process holds it: wait.
        std::fs::write(&lock, std::process::id().to_string()).expect("write lock");
        assert!(!lock_is_stale(&lock), "a live holder is waited on");
        // Garbage holds nothing either.
        std::fs::write(&lock, "nope").expect("write lock");
        assert!(lock_is_stale(&lock), "garbage is nobody's");
        // Missing is takeable, which reads as stale.
        std::fs::remove_file(&lock).expect("remove lock");
        assert!(lock_is_stale(&lock), "a missing lock is takeable");
        let _ = std::fs::remove_dir_all(&dir);
        assert!(pid_alive(std::process::id()));
        assert!(!pid_alive(1));
        assert!(!pid_alive(u32::MAX));
    }

    /// The `Drop` kill path sends SIGKILL: `/bin/sleep` through the same
    /// `spawn` the probe uses is gone — reaped, not a zombie — after drop.
    /// It never opens the real `muse`.
    #[test]
    fn dropping_the_pty_sigkills_the_child() {
        let pty = Pty::spawn({
            let mut sleep = std::process::Command::new("/bin/sleep");
            sleep.arg("60");
            sleep
        })
        .expect("spawn /bin/sleep");
        let pid = pty.child.id() as libc::pid_t;
        // Signal 0 checks without killing: the child is alive.
        // SAFETY: `kill` with a pid and signal 0 neither reads nor writes.
        assert_eq!(unsafe { libc::kill(pid, 0) }, 0);
        drop(pty);
        // Gone, not a zombie: the drop SIGKILLed and reaped it.
        assert_ne!(unsafe { libc::kill(pid, 0) }, 0);
    }
}
