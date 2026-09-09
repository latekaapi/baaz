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
use std::path::PathBuf;
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
        /// The current window's usage, as a whole percent.
        current_pct: Option<u32>,
        /// The weekly window's usage, as a whole percent.
        weekly_pct: Option<u32>,
        /// The reset clauses, in the order the card printed them.
        resets: Vec<String>,
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
            Tier::Subscription { plan, weekly_pct, .. } => {
                let plan = plan.strip_prefix("Muse Code ").unwrap_or(plan);
                match weekly_pct {
                    Some(pct) => format!("{plan} · {pct}% weekly"),
                    None => plan.to_owned(),
                }
            }
            Tier::PayAsYouGo => "Pay-as-you-go".to_owned(),
            Tier::Unavailable(_) => "Plan unknown".to_owned(),
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
            Tier::Subscription { plan, current_pct, weekly_pct, resets } => {
                let pct = |p: &Option<u32>| p.map(|p| format!("{p}%")).unwrap_or_else(|| "—".into());
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

/// The cached tier, when it was probed against the `auth.json` that is on disk
/// now. A stale entry — a login or a logout since — reads as `None`, which is
/// what makes the caller re-probe.
pub fn cached() -> Option<Tier> {
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
        Ok(Tier::Subscription { plan, current_pct, weekly_pct, resets }) => {
            let pct = |p: Option<u32>| p.map(|p| format!("{p}%")).unwrap_or_else(|| "\u{2014}".into());
            println!("Subscription: {plan}");
            println!("Current: {} used", pct(current_pct));
            println!("Weekly: {} used", pct(weekly_pct));
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

/// Drive the TUI and read the `/upgrade` card. Blocking for up to
/// [`PROBE_CEILING`]; call it on a background thread.
///
/// Every error is a `String` that is safe to show: it names what went wrong,
/// never what the terminal said.
pub fn probe(muse: &str) -> Result<Tier, String> {
    let deadline = Instant::now() + PROBE_CEILING;
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
    //    the truth. Read the whole window, then parse once.
    pty.pump(&mut text, CARD.min(deadline.saturating_duration_since(Instant::now())));
    let mut tier = parse_card(&text);
    while tier.is_none() && Instant::now() < deadline && text.len() < MAX_OUTPUT {
        pty.pump(&mut text, TICK);
        tier = parse_card(&text);
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
        let seen: Vec<&str> = ["subscribed", "pay-as-you-go", "upgrade", "Upgrade", "plan", "%", "Ask", "muse", "error"]
            .into_iter()
            .filter(|word| flat.contains(word))
            .collect();
        eprintln!("tier probe: {} bytes read, {} after flattening, saw {seen:?}", text.len(), flat.len());
    }

    tier.ok_or_else(|| "the /upgrade card did not answer in time".to_owned())
}

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
        });
    }
    let lower = text.to_lowercase();
    if lower.contains("pay-as-you-go")
        || lower.contains("pay as you go")
        || lower.contains("subscriptions aren't currently available")
        || lower.contains("subscriptions are not currently available")
    {
        return Some(Tier::PayAsYouGo);
    }
    None
}

/// `…subscribed to the Muse Code High Usage plan.` → `Muse Code High Usage`.
fn plan_name(text: &str) -> Option<String> {
    let start = text.find("subscribed to the")? + "subscribed to the".len();
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
        let mut command = std::process::Command::new(muse);
        command
            .arg("--workspace")
            .arg(probe_workspace())
            .env("TERM", "xterm-256color")
            .env("LINES", ROWS.to_string())
            .env("COLUMNS", COLS.to_string());
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
    fn a_subscription_card_yields_the_plan_and_both_percentages() {
        let card = "\u{1b}[2J\u{1b}[1;1H\u{2502} You are currently subscribed to the Muse Code High Usage \
                    plan. \u{2502}\n\u{2502} Current 2% used \u{b7} Resets at 3:00 PM \u{2502}\n\
                    \u{2502} Weekly 7% used \u{b7} Resets Monday \u{2502}\n";
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets }) = parse_card(card) else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(current_pct, Some(2));
        assert_eq!(weekly_pct, Some(7));
        assert_eq!(resets, vec!["Resets at 3:00 PM".to_owned(), "Resets Monday".to_owned()]);
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
        let Some(Tier::Subscription { plan, current_pct, weekly_pct, resets }) = parse_card(card) else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(current_pct, Some(0));
        assert_eq!(weekly_pct, Some(2));
        assert_eq!(resets, vec!["Resets at 5:17 PM".to_owned(), "Resets Sep 14 at 5:30 AM".to_owned()]);
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
        };
        assert_eq!(plan.footer_label(), "High Usage · 2% weekly");
        assert!(!plan.is_warning());
        assert_eq!(Tier::PayAsYouGo.footer_label(), "Pay-as-you-go");
        assert!(Tier::PayAsYouGo.is_warning());
        assert_eq!(Tier::Unavailable("no tty".into()).footer_label(), "Plan unknown");
        assert!(Tier::Unavailable("no tty".into()).is_warning());
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
