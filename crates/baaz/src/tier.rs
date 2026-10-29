//! Which credential tier this login is on, and the guard built on top of it.
//!
//! # The finding
//!
//! Muse has two credential tiers, **pay-as-you-go** and **subscription**, and
//! the tier is decided by the login token rather than by anything Baaz
//! sends. A pay-as-you-go token bills every turn as API usage even though
//! `auth.json` says `mechanism: oauth`. Nothing on the wire says which tier
//! a token is on:
//! `initialize` and `model/list` carry no account or plan field, `auth.json`
//! carries only the mechanism, the storage, the API base and the person's name
//! and email, and the session log records only a `credential_backend`.
//!
//! # Two oracles: the wire first, the screen scrape as fallback
//!
//! Since muse 1.3.0 the numbers the card shows are on the wire, typed:
//! `usage/read` answers `{usage?}` (absent on a cold host)
//! and the server emits `usage/changed` as observations land. Those are the
//! primary oracle now — they reach Baaz through the Muse lane as neutral
//! [`provider::UsageReport`]s (`Command::ReadUsage`, `UsageChanged`) —
//! ([`tier_from_report`], [`observation_is_current`]): on a reading made after the current
//! credential was installed the tier is built directly from it and cached
//! through [`remember`] exactly as a probe answer is. The wire carries no
//! credential identity, so anything older belongs to a previous login and
//! falls through to the scrape instead.
//!
//! The wire cannot replace the scrape outright, so the scrape stays as the
//! fallback ([`probe`]). Two verified reasons:
//!
//! 1. `usage/read` answers `{}` on a cold host — it only populates after a
//!    turn has observed a provider response, so a fresh boot has nothing.
//! 2. Nothing on the wire says a login is pay-as-you-go. `usage/read`
//!    returns a reading when a subscription observation exists
//!    and nothing otherwise; absence is "not known yet", not pay-as-you-go.
//!
//! The `muse` TUI's `/upgrade` card is still the only oracle that
//! distinguishes [`Tier::PayAsYouGo`] from [`Tier::Unavailable`] — and
//! `PayAsYouGo` is what raises the blocking banner that stops the person
//! being billed API rates by surprise. So on `None` (the `{}` case) or any
//! read failure, the existing pty probe runs unchanged: it spawns `muse`
//! under a pseudo-terminal with no prompt, answers the cursor-position query
//! the TUI opens with (`ESC [ 6 n` → `ESC [ 1;1 R`, without which it never
//! draws), types `/upgrade`, presses Enter and reads the card, which says
//! either
//!
//! > You are currently subscribed to the *Muse Code High Usage* plan.
//! > Current 2% used · Resets at … Weekly 2% used · Resets …
//!
//! or "you're on pay-as-you-go" / "Subscriptions aren't currently available
//! for your account". **Opening the TUI starts a
//! session record and makes no model call**, so the probe costs nothing.
//!
//! Two rules this module keeps:
//!
//! * **The raw terminal output is never logged.** Only the parsed plan name
//!   and the two percentages ever leave this module.
//! * **A failed probe never stops the app.** Every failure is
//!   [`Tier::Unavailable`], which shows a quiet banner and blocks nothing.
//!
//! The result is cached in `~/Library/Application Support/baaz/tier.json`
//! keyed by the modification time of `auth.json`, so a logout and a fresh
//! login re-probe and an ordinary boot does not.

use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use provider::{UsageReport, UsageWindow};

/// The ceiling on one probe, start to finish.
const PROBE_CEILING: Duration = Duration::from_secs(20);
/// The longest the TUI gets to draw its composer before the probe types at
/// it anyway. A ceiling, not a wait: the probe types as soon as
/// [`COMPOSER_READY`] is on screen. muse 1.4.9 behind its launcher took past
/// the old fixed 4 s to draw anything, so `/upgrade` and its Enter landed
/// before the composer existed and the card never drew (2026-10-16).
const SETTLE: Duration = Duration::from_millis(9_000);
/// The composer's prompt glyph: the TUI is taking keystrokes once it is drawn.
const COMPOSER_READY: &str = "\u{276f}";
/// How long the composer gets after its prompt draws, for its input handler
/// to catch up with its paint.
const READY_GRACE: Duration = Duration::from_millis(400);
/// The longest the palette gets between the text and the Enter; the Enter
/// goes as soon as the composer echoes the command, plus [`PALETTE`].
const PALETTE_CEILING: Duration = Duration::from_millis(3_000);
/// How long the palette gets, once the command is echoed, before the Enter.
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

/// A plan string fit to show: it names the plan in words.
///
/// The wire (the reading's `tier`, muse 1.4.0) carries an opaque numeric
/// id there instead of a name. Anything without a letter is such an id —
/// all digits or otherwise id-shaped — never a name, and must never reach a
/// surface.
pub fn is_human_plan_name(plan: &str) -> bool {
    !plan.is_empty() && plan.chars().any(|c| c.is_ascii_alphabetic())
}

/// The generic label a subscription shows when no human plan name is known
/// for this login.
pub const GENERIC_PLAN_LABEL: &str = "Subscription";

/// The plan name to show for `raw`: the raw name when it is human, else the
/// last human name `known` for this login, else the generic label. A numeric
/// plan id is never displayed.
pub fn resolve_plan_name(raw: &str, known: Option<&str>) -> String {
    if is_human_plan_name(raw) {
        return raw.to_owned();
    }
    if let Some(known) = known {
        if is_human_plan_name(known) {
            return known.to_owned();
        }
    }
    GENERIC_PLAN_LABEL.to_owned()
}

/// One tier reset clause: the card's own words, plus the reset instant they
/// were rendered from when the wire supplied one.
///
/// Wire-built tiers carry `resets_at_ms`, and the account menu's Muse rows
/// format durations from it ([`crate::account_usage::resets_in_text`]).
/// Probe-built clauses and old cache files carry words only and read as
/// unknown there — never a fabricated countdown, and never `resets in at …`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResetClause {
    /// The clause as printed, e.g. `Resets at 5:17 PM`.
    pub text: String,
    /// The reset instant in epoch milliseconds, when known.
    pub resets_at_ms: Option<u64>,
}

impl ResetClause {
    /// A clause rendered from a wire stamp, carrying the stamp.
    pub fn timed(text: String, resets_at_ms: u64) -> Self {
        Self { text, resets_at_ms: Some(resets_at_ms) }
    }
}

impl From<String> for ResetClause {
    fn from(text: String) -> Self {
        Self { text, resets_at_ms: None }
    }
}

impl From<&str> for ResetClause {
    fn from(text: &str) -> Self {
        Self { text: text.to_owned(), resets_at_ms: None }
    }
}

impl serde::Serialize for ResetClause {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.resets_at_ms {
            // Probe-style files stay plain strings, exactly as older builds
            // wrote them.
            None => serializer.serialize_str(&self.text),
            Some(resets_at_ms) => {
                use serde::ser::SerializeStruct;
                let mut clause = serializer.serialize_struct("ResetClause", 2)?;
                clause.serialize_field("text", &self.text)?;
                clause.serialize_field("resetsAtMs", &resets_at_ms)?;
                clause.end()
            }
        }
    }
}

/// The two shapes a clause takes in `tier.json`: a plain string (the probe
/// path and every file older builds wrote) or a map carrying the instant.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum ResetClauseWire {
    Text(String),
    Full(ResetClauseFull),
}

/// The map shape of [`ResetClause`], in the file's camelCase register.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResetClauseFull {
    #[serde(default)]
    text: String,
    #[serde(default)]
    resets_at_ms: Option<u64>,
}

impl<'de> serde::Deserialize<'de> for ResetClause {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match ResetClauseWire::deserialize(deserializer)? {
            ResetClauseWire::Text(text) => ResetClause::from(text),
            ResetClauseWire::Full(full) => ResetClause { text: full.text, resets_at_ms: full.resets_at_ms },
        })
    }
}

/// What the login token is entitled to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "tier", rename_all = "camelCase")]
pub enum Tier {
    /// A subscription plan, with whatever the card said about it.
    #[serde(rename_all = "camelCase")]
    Subscription {
        /// The plan's name, e.g. `Muse Code High Usage`. The wire may carry
        /// an opaque numeric tier id instead (muse 1.4.0): surfaces resolve
        /// it through [`resolve_plan_name`], never verbatim.
        plan: String,
        /// The current window's usage, as a whole percent. `None` either
        /// because the card has not finished drawing yet, or because
        /// `usage_unavailable` below says it never will.
        current_pct: Option<u32>,
        /// The weekly window's usage, as a whole percent. Same absence rule
        /// as `current_pct`.
        weekly_pct: Option<u32>,
        /// The reset clauses, in the order the card printed them.
        resets: Vec<ResetClause>,
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
            Tier::Subscription { plan, .. } => {
                let shown = resolve_plan_name(plan, None);
                shown.strip_prefix("Muse Code ").unwrap_or(&shown).to_owned()
            }
            Tier::PayAsYouGo => "Pay-as-you-go".to_owned(),
            Tier::Unavailable(_) => "Plan unknown".to_owned(),
        }
    }

    /// The sidebar footer's usage meter: the weekly window's fraction in
    /// `0..=1`, or `None` when the probe said nothing usable. This is the only
    /// place the weekly number is drawn — [`Self::footer_label`] above it
    /// names the plan and stops there (D5).
    /// The fraction the usage snapshot records: the weekly window when the
    /// card has it, otherwise the current (5-hour) window — a read with only
    /// the current window still has numbers and must not read as unavailable.
    pub fn usage_fraction(&self) -> Option<f32> {
        match self {
            Tier::Subscription { weekly_pct: Some(pct), .. } | Tier::Subscription { current_pct: Some(pct), .. } => {
                Some((*pct as f32 / 100.0).clamp(0.0, 1.0))
            }
            _ => None,
        }
    }

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
    ///
    /// A measured percentage reads as "N% used" and the still-drawing dash
    /// as "— used", but the card's own word `unavailable` stands alone —
    /// "unavailable used" is what a screenshot showed, and the same rule
    /// [`print_tier_field`] already applies to `--print-tier`.
    pub fn status_lines(&self) -> String {
        match self {
            Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable, .. } => {
                let usage = |p: &Option<u32>| {
                    match p {
                        Some(p) => format!("{p}% used"),
                        None if *usage_unavailable => "unavailable".to_owned(),
                        None => "— used".to_owned(),
                    }
                };
                let mut text = format!(
                    "Plan: {}\nCurrent usage: {}\nWeekly usage: {}",
                    resolve_plan_name(plan, None),
                    usage(current_pct),
                    usage(weekly_pct)
                );
                for reset in resets {
                    text.push('\n');
                    text.push_str(&reset.text);
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
    /// The last human plan name seen for this login, persisted alongside the
    /// tier so a later numeric wire reading resolves back to it. Absent in
    /// files older builds wrote.
    #[serde(default)]
    pub last_human_plan: Option<String>,
    /// When this answer was taken, in whole seconds since the epoch. A
    /// remembered pay-as-you-go older than ten minutes re-verifies instead
    /// of being trusted; absent in files older builds wrote, which read as
    /// needing re-verification for pay-as-you-go.
    #[serde(default)]
    pub probed_at_secs: Option<u64>,
    /// Whether a subscription here was confirmed by an oracle that can tell
    /// a subscription from pay-as-you-go: the `/upgrade` card, or a wire
    /// observation naming its plan in words. A wire observation carrying
    /// only the opaque numeric tier id (muse 1.4.x) is numbers, not a plan
    /// kind — a pay-as-you-go login gets those too (see
    /// [`report_settles_kind`]). An unconfirmed subscription is never served
    /// from the cache: the next boot asks the card. Absent in files older
    /// builds wrote, which therefore re-probe once.
    #[serde(default)]
    pub kind_confirmed: bool,
}

/// Forget the remembered tier and plan name: a deliberate sign-out means
/// the next login may be another account, which must never inherit this
/// one's plan name through `lastHumanPlan`.
pub fn forget() {
    let _ = std::fs::remove_file(cache_path());
}

/// `~/Library/Application Support/baaz/tier.json`.
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

/// How long a cached answer is trusted before an ordinary boot probes
/// again. A forced probe — "Check again", `/usage`, `/status`
/// — always re-probes; a second window booting inside the hour reuses the
/// first's answer instead of driving a second TUI at the same workspace.
const CACHE_TTL: Duration = Duration::from_secs(3600);

/// How long a remembered pay-as-you-go is trusted before the next boot or
/// menu refresh re-verifies it instead. A false pay-as-you-go tells the
/// owner every turn is billed, so it gets the short bound; a remembered
/// subscription keeps [`CACHE_TTL`].
pub const PAYG_REVERIFY_AFTER: Duration = Duration::from_secs(600);

/// How long a remembered `usage_unavailable` card is trusted before the
/// next boot or menu refresh re-reads it instead. The card said the
/// server is not reporting numbers right now — a transient state, often
/// already gone after the next turn — so it gets the short bound; a
/// remembered subscription with numbers keeps [`CACHE_TTL`].
pub const UNAVAILABLE_REVERIFY_AFTER: Duration = Duration::from_secs(300);

/// How long a remembered Muse usage answer is trusted before the account
/// menu re-reads it instead: about five minutes past its probe. The menu
/// calls this through [`muse_menu_refresh_due`].
pub const USAGE_REVERIFY_AFTER: Duration = Duration::from_secs(300);

/// A pay-as-you-go answer's age past which it re-verifies rather than being
/// trusted. `None` (a file from before the stamp existed) reads as stale:
/// re-probe rather than trust indefinitely.
pub fn payg_answer_stale(probed_at_secs: Option<u64>, now_secs: u64) -> bool {
    match probed_at_secs {
        Some(taken) => now_secs.saturating_sub(taken) > PAYG_REVERIFY_AFTER.as_secs(),
        None => true,
    }
}

/// A `usage_unavailable` answer's age past which it expires rather than
/// being trusted. `None` (a file from before the stamp existed) reads as
/// stale: re-read rather than trust indefinitely.
pub fn unavailable_answer_stale(probed_at_secs: Option<u64>, now_secs: u64) -> bool {
    match probed_at_secs {
        Some(taken) => now_secs.saturating_sub(taken) > UNAVAILABLE_REVERIFY_AFTER.as_secs(),
        None => true,
    }
}

/// Whether the account menu should re-read Muse's usage on open: the
/// tier never answered, the probe failed, the card carries
/// `usage_unavailable` or no numbers at all, or its answer is older
/// than about five minutes. A fresh answer with numbers is left alone.
pub fn muse_menu_refresh_due(tier: Option<&Tier>) -> bool {
    let Some(tier) = tier else {
        return true;
    };
    match tier {
        Tier::PayAsYouGo => false,
        Tier::Unavailable(_) => true,
        Tier::Subscription { current_pct, weekly_pct, usage_unavailable, .. } => {
            if *usage_unavailable || (current_pct.is_none() && weekly_pct.is_none()) {
                return true;
            }
            let cached: Cached = crate::store::read_json(&cache_path());
            match cached.probed_at_secs {
                Some(taken) => now_secs().saturating_sub(taken) > USAGE_REVERIFY_AFTER.as_secs(),
                None => true,
            }
        }
    }
}

/// This machine's wall clock in whole seconds since the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// The cached tier, when it was probed against the `auth.json` that is on disk
/// now and inside [`CACHE_TTL`]. A stale entry — a login or a logout since,
/// or an answer older than the hour — reads as `None`, which is what makes
/// the caller re-probe. A remembered pay-as-you-go older than ten minutes
/// reads as `None` sooner ([`payg_answer_stale`]), and a remembered
/// `usage_unavailable` older than about five minutes does too
/// ([`unavailable_answer_stale`]); a remembered subscription with numbers
/// keeps today's behaviour.
pub fn cached() -> Option<Tier> {
    cached_at(now_secs())
}

/// [`cached`] with an explicit clock, so tests drive the age without waiting.
pub fn cached_at(now: u64) -> Option<Tier> {
    let cached: Cached = crate::store::read_json(&cache_path());
    if cached.auth_mtime != auth_mtime() {
        return None;
    }
    if matches!(cached.tier, Some(Tier::PayAsYouGo)) && payg_answer_stale(cached.probed_at_secs, now) {
        return None;
    }
    if matches!(cached.tier, Some(Tier::Subscription { .. })) && !cached.kind_confirmed {
        return None;
    }
    if matches!(cached.tier, Some(Tier::Subscription { usage_unavailable: true, .. }))
        && unavailable_answer_stale(cached.probed_at_secs, now)
    {
        return None;
    }
    let modified = std::fs::metadata(cache_path()).ok()?.modified().ok()?;
    if modified.elapsed().ok()? > CACHE_TTL {
        return None;
    }
    let Cached { tier, last_human_plan, .. } = cached;
    let mut tier = tier?;
    // Files written before the id guard (or by hand) may hold a numeric
    // plan: resolve it here so no surface ever shows it.
    if let Tier::Subscription { plan, .. } = &mut tier {
        if !is_human_plan_name(plan) {
            *plan = resolve_plan_name(plan, last_human_plan.as_deref());
        }
    }
    Some(tier)
}

/// Remember `tier` against the `auth.json` on disk now. Best-effort: a store
/// that cannot be written means the next boot probes again, which is a cost
/// and not a failure.
///
/// A numeric wire plan is resolved before it is persisted — to the last
/// human name for this login, else the generic label — and that name is kept
/// alongside, so what is stored never shows an id. A reading that names no
/// plan (pay-as-you-go, an unrecognised card, a numeric wire id) never
/// erases the name a previous reading established, even across a credential
/// refresh that changes the mtime key under it.
///
/// For a probe's answer: what it says about the plan kind is confirmed.
pub fn remember(tier: &Tier) {
    remember_at(tier, auth_mtime(), now_secs());
}

/// [`remember`] for a wire observation admitted through
/// [`report_settles_kind`]: its kind is confirmed only when it named its plan
/// (`named`) or a confirmed subscription for this same login stands behind
/// it — a numeric-id reading refreshes the numbers and nothing else.
pub fn remember_observed(tier: &Tier, named: bool) {
    let mtime = auth_mtime();
    let prev: Cached = crate::store::read_json(&cache_path());
    let inherited =
        prev.kind_confirmed && prev.auth_mtime == mtime && matches!(prev.tier, Some(Tier::Subscription { .. }));
    remember_with(tier, mtime, now_secs(), named || inherited);
}

/// [`remember`] with explicit stamps, so tests drive the mtime and the clock.
pub fn remember_at(tier: &Tier, mtime: Option<u64>, now: u64) {
    remember_with(tier, mtime, now, true);
}

fn remember_with(tier: &Tier, mtime: Option<u64>, now: u64, kind_confirmed: bool) {
    let prev: Cached = crate::store::read_json(&cache_path());
    let prev_known = prev.last_human_plan.filter(|name| is_human_plan_name(name));
    let own = match tier {
        Tier::Subscription { plan, .. } if is_human_plan_name(plan) => Some(plan.clone()),
        _ => None,
    };
    let last_human_plan = own.clone().or(prev_known);
    let mut tier = tier.clone();
    if let Tier::Subscription { plan, .. } = &mut tier {
        if !is_human_plan_name(plan) {
            *plan = last_human_plan.clone().unwrap_or_else(|| GENERIC_PLAN_LABEL.to_owned());
        }
    }
    let cached =
        Cached { auth_mtime: mtime, tier: Some(tier), last_human_plan, probed_at_secs: Some(now), kind_confirmed };
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
        Ok(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable, .. }) => {
            println!("Subscription: {}", resolve_plan_name(&plan, None));
            println!("Current: {}", print_tier_field(current_pct, usage_unavailable));
            println!("Weekly: {}", print_tier_field(weekly_pct, usage_unavailable));
            for reset in resets {
                println!("{}", reset.text);
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

/// How long the quit path waits for live probes to drain after the kill,
/// and how long a [`Pty`] drop spends reaping its child: past either bound
/// the exit (or the drop) proceeds without the child rather than hanging.
const QUIT_WAIT: Duration = Duration::from_millis(500);

/// Kill live tier-probe children and wait, bounded, for their drops.
///
/// Only the real QUIT path runs this — the screenshot quit, the app-quit
/// hook, and the ⌘Q menu action — so a probe mid-flight never orphans its
/// `muse` TUI child. Bounded either way: the quit proceeds when the wait
/// expires, and the wait is skipped entirely when no probe is live.
pub fn cleanup_probes() {
    kill_live_probes();
    if LIVE_PROBES.lock().map(|live| live.is_empty()).unwrap_or(true) {
        return;
    }
    wait_for_probes_gone(QUIT_WAIT);
}

/// Signal live tier-probe children without waiting for them.
///
/// The ONLY cleanup a window close (the red dot, ⌘W) runs: closing hides
/// the app rather than quitting, so the probe thread is still alive to reap
/// its own child on its next pump tick. Nothing in the close path may sleep
/// on the main thread — a stuck probe must never freeze the hide — so this
/// never waits, not even boundedly.
pub fn cleanup_probes_for_close() {
    kill_live_probes();
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

/// `~/Library/Application Support/baaz/tier-probe/probe.pid`: the live
/// probe child's pid, or nothing when no probe is running.
///
/// A Baaz process that is force-quit mid-probe never drops its `Pty`, so the TUI —
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

/// SIGKILL the previous probe's child when it outlived its Baaz process,
/// and every other orphaned probe TUI on the machine.
///
/// The pid file only knows this state directory's last probe. Capture and
/// test runs each probe under their own throwaway `BAAZ_STATE_DIR`, and a
/// force-quit one leaves its TUI reparented to pid 1 where no pid file will
/// ever name it: on 2026-10-16 three such TUIs had lived for weeks, one at
/// 38% CPU holding muse's session index open while its WAL grew past a
/// gigabyte. So the sweep also kills any [`is_orphaned_probe`] process.
fn sweep_stale_probe() {
    let kill = |pid: u32| {
        // SAFETY: as in `kill_live_probes`.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
    };
    sweep_stale_with(read_probe_pid(), &is_probe_child, &kill);
    if let Some(table) = process_table() {
        for pid in orphaned_probes(&table) {
            kill(pid);
        }
    }
}

/// `ps`'s view of every process: one `pid ppid command` line each.
fn process_table() -> Option<String> {
    let out = std::process::Command::new("ps").arg("-axo").arg("pid=,ppid=,command=").output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The pids in a `pid ppid command` table that are orphaned probe TUIs.
///
/// Pure, so the matcher is tested on text rather than on live processes.
/// The table is parsed here rather than handed to `pgrep -f`, whose pattern
/// sits in its own command line and so matches itself.
fn orphaned_probes(table: &str) -> Vec<u32> {
    let own = std::process::id();
    table
        .lines()
        .filter_map(|line| {
            // `ps` right-aligns its columns, so fields sit behind runs of spaces.
            let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
            let (ppid, command) = rest.trim_start().split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            let ppid: u32 = ppid.parse().ok()?;
            let command = command.trim();
            (pid > 1 && pid != own && ppid == 1 && is_orphaned_probe(command)).then_some(pid)
        })
        .collect()
}

/// Whether `command` is exactly the line [`probe_command`] spawns — a `muse`
/// binary, `--workspace` on some `…/tier-probe`, then `--trust-workspace`,
/// and nothing after it — in any state directory.
///
/// Deliberately that narrow: the person's own `muse` (any other workspace,
/// or none), `muse serve`, and every app session's child never end that
/// way. Paths may hold spaces (`Application Support`), so the line is cut
/// on the flags rather than split on whitespace. The caller adds the other
/// half — the parent is gone — so a live Baaz's own probe is never touched.
fn is_orphaned_probe(command: &str) -> bool {
    let Some((program, rest)) = command.split_once(" --workspace ") else {
        return false;
    };
    let Some(workspace) = rest.trim_end().strip_suffix(" --trust-workspace") else {
        return false;
    };
    let binary = program.rsplit('/').next().unwrap_or(program);
    binary.starts_with("muse") && !binary.contains(' ') && workspace.ends_with("/tier-probe")
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

/// How long a probe waits for another Baaz process's probe before going ahead
/// alone. Past one ceiling the holder is either done or
/// stuck; either way the waiter stops waiting.
const LOCK_WAIT: Duration = Duration::from_secs(30);

/// One probe at a time across Baaz processes, held while the TUI runs.
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
/// under it and the pid file swept, so nothing it owns outlives Baaz
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
/// `Current — / Weekly —` while the card still had ink to lay. Anything
/// else is complete as parsed.
fn complete(tier: &Tier) -> bool {
    match tier {
        Tier::Subscription { current_pct, weekly_pct, usage_unavailable, .. } => {
            (current_pct.is_some() && weekly_pct.is_some()) || *usage_unavailable
        }
        Tier::PayAsYouGo | Tier::Unavailable(_) => true,
    }
}

// ---------------------------------------------------------------------------
// The wire oracle (muse 1.3.0): `usage/read` first, `usage/changed` live
// ---------------------------------------------------------------------------

/// Build a [`Tier::Subscription`] from one Muse usage reading.
///
/// `plan` is the provider's subscription tier id verbatim when it names the
/// plan in words, else the generic label (muse 1.4.0 sends an opaque numeric
/// id there); `current_pct` the window's `used_percent`, `weekly_pct` the
/// weekly block's, and `resets` the two reset clauses rendered from their
/// epoch-millisecond stamps in the card's own register (see [`format_reset`]),
/// each carrying its stamp for the duration rows. Callers that know the
/// login restore the last human name through [`restore_known_plan`].
///
/// The percentages are verbatim, including over-quota values above 100 — the
/// schema says those are valid. [`Tier::weekly_fraction`] clamps for the
/// meter; [`Tier::status_lines`] prints the true number.
///
/// `observed_at_ms` is not consulted here: this builder trusts its input,
/// and currency is the callers' job. [`probe_primary`] and
/// [`tier_from_changed_current`] admit only observations made after the
/// current credential was installed ([`observation_is_current`]), because the
/// wire carries no account or credential identity and the host serves its
/// last observation across a sign-out and sign-in on the same connection. A
/// stale observation belongs to a previous login and must never become the
/// tier: trusting it would show a subscription while the login bills
/// pay-as-you-go rates with no blocking banner.
pub fn tier_from_report(report: &UsageReport) -> Option<Tier> {
    let (window, weekly) = muse_windows(report)?;
    let mut resets = Vec::new();
    if let Some(clause) = window.resets_at.and_then(|at| reset_clause(at as u64 * 1000, false)) {
        resets.push(clause);
    }
    if let Some(clause) = weekly.resets_at.and_then(|at| reset_clause(at as u64 * 1000, true)) {
        resets.push(clause);
    }
    Some(Tier::Subscription {
        // The wire may carry an opaque numeric tier id (muse 1.4.0), never
        // a name: with no previous reading this reads as the generic label,
        // and the admitting callers restore the last human name below.
        plan: resolve_plan_name(report.plan.as_deref().unwrap_or_default(), None),
        current_pct: Some(percent(window)),
        weekly_pct: Some(percent(weekly)),
        resets,
        usage_unavailable: false,
    })
}

/// A Muse reading's two windows: the current one first, the weekly block
/// second (the order `provider_muse::usage::report_from` keeps). `None`
/// for a reading with fewer.
fn muse_windows(report: &UsageReport) -> Option<(&UsageWindow, &UsageWindow)> {
    Some((report.windows.first()?, report.windows.get(1)?))
}

/// A window's used fraction back as the card's whole percent, over-quota
/// values included.
fn percent(window: &UsageWindow) -> u32 {
    (window.used_fraction * 100.0).round().max(0.0) as u32
}

/// [`tier_from_report`] over a wire reading (`usage/changed` params), for
/// the tests that build one.
#[cfg(test)]
pub fn tier_from_usage(usage: &serde_json::Value) -> Tier {
    tier_from_report(&provider_muse::usage::report_from_changed(usage).expect("a wire reading decodes"))
        .expect("a wire reading has both windows")
}

/// One wire reset clause: the card's words rendered from the stamp, carrying
/// the stamp for the duration rows. `None` only when the stamp is not a
/// representable local time (see [`format_reset`]).
fn reset_clause(resets_at_ms: u64, with_date: bool) -> Option<ResetClause> {
    format_reset(resets_at_ms, with_date).map(|text| ResetClause::timed(text, resets_at_ms))
}

/// The last human plan name persisted for the login installed at
/// `auth_mtime_secs`, or `None` when none was ever known. Reads `tier.json`
/// directly with no freshness bound: the name stays valid while the login
/// does.
fn last_human_plan(auth_mtime_secs: Option<u64>) -> Option<String> {
    let auth_mtime_secs = auth_mtime_secs?;
    let cached: Cached = crate::store::read_json(&cache_path());
    if cached.auth_mtime != Some(auth_mtime_secs) {
        return None;
    }
    cached.last_human_plan.filter(|name| is_human_plan_name(name))
}

/// Restore the last human plan name onto a wire-built tier whose raw tier id
/// is not a name. A no-op for human plans and non-subscriptions.
fn restore_known_plan(tier: &mut Tier, raw_tier: &str, auth_mtime_secs: Option<u64>) {
    if is_human_plan_name(raw_tier) {
        return;
    }
    if let Tier::Subscription { plan, .. } = tier {
        *plan = last_human_plan(auth_mtime_secs).unwrap_or_else(|| GENERIC_PLAN_LABEL.to_owned());
    }
}

/// One `usage/read` result object → the tier it names, or `None` when the
/// host has observed nothing (the `{}` case: the `usage` key absent, never
/// `null`) or the result does not decode.
///
/// `None` is **not** [`Tier::Unavailable`]: it is "not known yet", and the
/// caller falls through to the pty fallback ([`probe`]) rather than settling
/// for unknown.
#[cfg(test)]
pub fn tier_from_read_value(value: &serde_json::Value) -> Option<Tier> {
    provider_muse::usage::report_from_read(value).and_then(|report| tier_from_report(&report))
}

/// One `usage/changed` notification's params → the tier it names, or `None`
/// when the params do not decode. A malformed frame never clears a known
/// tier: the caller keeps what it had.
#[cfg(test)]
pub fn tier_from_changed(params: &serde_json::Value) -> Option<Tier> {
    provider_muse::usage::report_from_changed(params).and_then(|report| tier_from_report(&report))
}

/// One usage reading → the tier it names, when the observation belongs to
/// the credential now installed.
///
/// [`tier_from_report`] builds; this admits in between. A
/// notification stamped before the current credential was installed belongs
/// to a previous login — the wire carries no account identity — and reads as
/// `None`, so the caller keeps what it had instead of painting another
/// login's plan. The build still goes through the pure arm, so the tested
/// wording stays in one place.
pub fn tier_from_report_current(report: &UsageReport, auth_mtime_secs: Option<u64>) -> Option<Tier> {
    if !report_is_current(report, auth_mtime_secs) {
        return None;
    }
    let mut tier = tier_from_report(report)?;
    restore_known_plan(&mut tier, report.plan.as_deref().unwrap_or_default(), auth_mtime_secs);
    Some(tier)
}

/// [`tier_from_report_current`] over `usage/changed` params, for tests.
#[cfg(test)]
pub fn tier_from_changed_current(params: &serde_json::Value, auth_mtime_secs: Option<u64>) -> Option<Tier> {
    tier_from_report_current(&provider_muse::usage::report_from_changed(params)?, auth_mtime_secs)
}

/// Whether a reading belongs to the credential now installed
/// ([`observation_is_current`], in the reading's whole seconds).
fn report_is_current(report: &UsageReport, auth_mtime_secs: Option<u64>) -> bool {
    report.observed_at.is_some_and(|at| observation_is_current(at.max(0) as u64 * 1000, auth_mtime_secs))
}

/// One reset clause in the card's own register, from an epoch-millisecond
/// stamp, in the person's local timezone (`chrono::Local` — the same clock
/// the sidebar and transcript timestamps already use, and the one the card
/// itself prints in).
///
/// The window reads `Resets at 5:17 PM`; the weekly block reads
/// `Resets Sep 14 at 5:30 AM`, the date keeping the two clauses apart exactly
/// as the card's two clauses do. `None` only when the stamp is not a
/// representable local time, in which case the clause is skipped rather than
/// printed wrong.
fn format_reset(resets_at_ms: u64, with_date: bool) -> Option<String> {
    use chrono::{Local, TimeZone};
    let at = Local.timestamp_millis_opt(resets_at_ms as i64).single()?;
    if with_date {
        Some(format!("Resets {} at {}", at.format("%b %-d"), at.format("%-I:%M %p")))
    } else {
        Some(format!("Resets at {}", at.format("%-I:%M %p")))
    }
}

/// Whether a wire observation belongs to the credential now installed.
///
/// `observed_at_ms` is the host's arrival stamp in epoch **milliseconds**;
/// `auth_mtime_secs` is `auth.json`'s modification time in whole epoch
/// **seconds** ([`auth_mtime`]). The comparison converts the observation down
/// to whole seconds (`observed_at_ms / 1000`) and trusts it only when that
/// whole second is strictly after the credential's — no skew tolerance: both
/// stamps come from this machine's clock, so there is no skew to budget for,
/// and a same-second-or-older observation falls through to the scrape. The
/// tie goes to distrust on purpose: a false fall-through costs a four-second
/// scrape, while a false trust bills the person with no warning. `None` (no
/// `auth.json`) trusts nothing.
pub fn observation_is_current(observed_at_ms: u64, auth_mtime_secs: Option<u64>) -> bool {
    match auth_mtime_secs {
        Some(mtime) => observed_at_ms / 1000 > mtime,
        None => false,
    }
}


/// Whether a reading names its plan in words — the only reading that says
/// which kind of plan the login is on.
pub fn report_names_plan(report: &UsageReport) -> bool {
    report.plan.as_deref().is_some_and(is_human_plan_name)
}

/// [`report_names_plan`] over a wire value (`usage/changed` params or a
/// `usage/read` result), for tests.
#[cfg(test)]
pub fn wire_names_plan(value: &serde_json::Value) -> bool {
    wire_report(value).is_some_and(|report| report_names_plan(&report))
}

/// Whether a usage observation may become the tier, given the tier `held`
/// now.
///
/// The `/upgrade` card is the oracle for subscription versus
/// pay-as-you-go. Since muse 1.4.0 the wire's `tier` is an opaque numeric
/// id, and a pay-as-you-go login receives `usage/changed` frames all the
/// same (live 2026-10-17, muse 1.4.10: the card read "No active
/// subscription — you're on pay-as-you-go" while the first turn's frame,
/// 0 % / 0 % under a numeric id, flipped the footer to "Subscription"). So
/// a numeric-id observation only refreshes the numbers of a subscription
/// already known; it never decides the kind — never over a pay-as-you-go,
/// never over an unknown plan. A reading that names its plan in words
/// still settles the kind, as it always did.
pub fn report_settles_kind(report: &UsageReport, held: Option<&Tier>) -> bool {
    report_names_plan(report) || matches!(held, Some(Tier::Subscription { .. }))
}

/// [`report_settles_kind`] over a wire value, for tests.
#[cfg(test)]
pub fn wire_settles_kind(value: &serde_json::Value, held: Option<&Tier>) -> bool {
    wire_report(value).is_some_and(|report| report_settles_kind(&report, held))
}

/// A wire usage value — a `usage/read` result or `usage/changed` params —
/// as the reading the hub would hand over.
#[cfg(test)]
pub fn wire_report(value: &serde_json::Value) -> Option<UsageReport> {
    provider_muse::usage::report_from_read(value).or_else(|| provider_muse::usage::report_from_changed(value))
}

/// [`tier_from_report_current`] over a `usage/read` result, for tests.
#[cfg(test)]
pub fn tier_from_read_current(value: &serde_json::Value, auth_mtime_secs: Option<u64>) -> Option<Tier> {
    tier_from_report_current(&provider_muse::usage::report_from_read(value)?, auth_mtime_secs)
}

/// Whether the tier carries usable numbers for the Muse usage snapshot: a
/// subscription with at least one window. Anything else — pay-as-you-go,
/// an unknown probe, an unavailable card — must never be stored over the
/// last good numbers; the caller keeps those and notes "unavailable now".
pub fn tier_has_numbers(tier: &Tier) -> bool {
    match tier {
        Tier::Subscription { current_pct, weekly_pct, usage_unavailable: false, .. } => {
            current_pct.is_some() || weekly_pct.is_some()
        }
        _ => false,
    }
}

/// When the remembered tier answer was taken, in whole seconds: the age a
/// menu-open snapshot re-recorded from the in-memory tier keeps, instead
/// of the menu-open's own time.
pub fn cached_probed_at() -> Option<u64> {
    let cached: Cached = crate::store::read_json(&cache_path());
    cached.probed_at_secs
}

/// Decide the primary oracle's answer from an already-read `usage/read`
/// result object.
///
/// Testable by construction: the wire read (`Command::ReadUsage`) and the
/// scrape ([`probe`]) both live outside, so a test drives this directly with
/// a fixed result object and a recording fallback — no live connection, no
/// pseudo-terminal. A current observation ([`tier_from_read_current`])
/// answers at once and the fallback never runs; anything else — no result
/// (a read failure), no observation (the `{}` cold host), a stale one from a
/// previous login, or no credential at all — runs the fallback, which stays
/// the only oracle that can say pay-as-you-go.
///
/// Every error is a `String` that is safe to show: it names what went wrong,
/// never what the terminal said.
#[cfg(test)]
pub fn probe_primary(
    read: Option<&serde_json::Value>,
    auth_mtime_secs: Option<u64>,
    fallback: impl FnOnce() -> Result<Tier, String>,
) -> Result<Tier, String> {
    if let Some(tier) = read.and_then(|value| tier_from_read_current(value, auth_mtime_secs)) {
        return Ok(tier);
    }
    fallback()
}

/// How many extra PTY probes a provisional answer may cost: the first answer
/// plus up to two more, each pair [`PAYG_CONFIRM_INTERVAL`] apart.
pub const PAYG_CONFIRM_EXTRA_ATTEMPTS: usize = 2;
/// How far apart confirmation probes run. On the background path only —
/// tests inject their own wait.
pub const PAYG_CONFIRM_INTERVAL: Duration = Duration::from_secs(15);
/// How recently the credential may have been refreshed for a card answer to
/// count as provisional: a token refresh lands a new `auth.json` mtime, and
/// the card plausibly shows its transient wording while muse re-checks the
/// new login.
pub const AUTH_FRESH_WINDOW_SECS: u64 = 60;

/// Whether `auth_mtime_secs` is within [`AUTH_FRESH_WINDOW_SECS`] of `now` —
/// a refresh just happened. `None` (no credential) is never fresh.
pub fn is_auth_fresh(auth_mtime_secs: Option<u64>, now_secs: u64) -> bool {
    match auth_mtime_secs {
        Some(mtime) => now_secs >= mtime && now_secs - mtime <= AUTH_FRESH_WINDOW_SECS,
        None => false,
    }
}

/// [`probe_primary`] with a provisional pay-as-you-go: the card's
/// pay-as-you-go answer is never trusted from a single probe, and neither is
/// any card answer probed within [`AUTH_FRESH_WINDOW_SECS`] of a credential
/// refresh. A provisional answer re-probes on the caller's thread — the app
/// calls this on the background path, where it keeps the existing
/// probing/Checking state and never the pay-as-you-go banner — up to
/// [`PAYG_CONFIRM_EXTRA_ATTEMPTS`] more times, [`PAYG_CONFIRM_INTERVAL`]
/// apart (`sleep`, injected so tests never wait).
///
/// A subscription is still accepted at once, and so is anything the wire
/// says: a current `usage/read` observation answers before any probe runs,
/// which is what lets it replace a remembered pay-as-you-go. Pay-as-you-go
/// is accepted only when two consecutive probes agree; anything else —
/// a subscription, an unrecognised card, a failed probe, or probes that
/// never agree — settles without it, so an unconfirmed pay-as-you-go is
/// never surfaced and never remembered.
///
/// Every error is a `String` that is safe to show: it names what went wrong,
/// never what the terminal said.
pub fn probe_primary_confirmed(
    read: Option<&UsageReport>,
    auth_mtime_secs: Option<u64>,
    now_secs: u64,
    mut fallback: impl FnMut() -> Result<Tier, String>,
    sleep: impl Fn(),
) -> Result<Tier, String> {
    // Only a reading that names its plan answers for the card: a numeric
    // id is not evidence of a subscription ([`report_settles_kind`]).
    if let Some(tier) =
        read.filter(|report| report_names_plan(report)).and_then(|report| tier_from_report_current(report, auth_mtime_secs))
    {
        return Ok(tier);
    }
    let fresh = is_auth_fresh(auth_mtime_secs, now_secs);
    let first = fallback()?;
    if matches!(first, Tier::Unavailable(_)) {
        return Ok(first);
    }
    if matches!(first, Tier::Subscription { .. }) && !fresh {
        return Ok(first);
    }
    let mut prev_payg = matches!(first, Tier::PayAsYouGo);
    for _ in 0..PAYG_CONFIRM_EXTRA_ATTEMPTS {
        sleep();
        // A failed probe propagates: the caller settles it as unknown, never
        // as pay-as-you-go.
        let next = fallback()?;
        if matches!(next, Tier::Subscription { .. }) {
            return Ok(next);
        }
        if matches!(next, Tier::PayAsYouGo) {
            if prev_payg {
                return Ok(Tier::PayAsYouGo);
            }
            prev_payg = true;
        } else {
            // An unrecognised card fails safe, breaking the consecutive run.
            return Ok(next);
        }
    }
    // Unreachable: every later answer either agrees, subscribes, or fails
    // safe — but a provisional answer must never escape unconfirmed.
    Ok(Tier::Unavailable("the /upgrade card did not give the same answer twice".to_owned()))
}

/// Drive the TUI and read the `/upgrade` card. Blocking for up to
/// [`PROBE_CEILING`]; call it on a background thread.
///
/// One probe runs at a time across Baaz processes ([`acquire_probe_lock`]): a
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
    let opened = Instant::now();
    let mut text = String::new();

    // 1. Let it paint, answering the cursor-position query. Without the reply
    //    the TUI waits for the terminal it thinks it is talking to and draws
    //    nothing at all.
    //    Typed as soon as the composer is drawn — never on a fixed clock,
    //    which a slow start outruns.
    pty.pump_until(&mut text, SETTLE.min(deadline - Instant::now()), |flat| flat.contains(COMPOSER_READY));
    pty.pump(&mut text, READY_GRACE);

    // 2. `/upgrade`, then Enter once the composer has echoed it and the
    //    palette has caught up.
    let typed_at = text.len();
    pty.write(b"/upgrade")?;
    pty.pump_until_from(&mut text, typed_at, PALETTE_CEILING, |flat| flat.contains("upgrade"));
    pty.pump(&mut text, PALETTE);
    pty.write(b"\r")?;
    // Everything read so far is the composer and the slash palette, and the
    // palette's own row for `/upgrade` says the words the card says. Matching
    // it would report the plan the person is being *offered*. Only what the
    // card draws after the Enter counts, so the buffer starts again here.
    let before_enter = std::mem::take(&mut text);

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

    // 3b. An early pay-as-you-go is not an answer yet. muse 1.4.14 loads the
    //    subscription a few seconds after its composer appears, and a card
    //    opened before that draws "No active subscription — you're on
    //    pay-as-you-go" on a subscribed login, and never redraws (measured
    //    2026-10-29: typed at 3 s and 5 s after launch → pay-as-you-go;
    //    at 6.5, 8, 8 and 10 s → the real plan; re-asking in the same TUI at
    //    ~10 s → the real plan). So the card is asked again in this TUI once
    //    the status has had [`STATUS_LOAD`] to land, and that answer stands.
    //    A fresh TUI per confirmation, as before, raced the same way every
    //    time and confirmed the wrong answer.
    if matches!(tier, Some(Tier::PayAsYouGo)) {
        let ready_at = opened + STATUS_LOAD;
        let now = Instant::now();
        if ready_at > now {
            pty.pump(&mut text, (ready_at - now).min(deadline.saturating_duration_since(now)));
        }
        let _ = pty.write(b"\x1b");
        pty.pump(&mut text, PALETTE);
        let asked_again = text.len();
        pty.write(b"/upgrade")?;
        pty.pump_until_from(&mut text, asked_again, PALETTE_CEILING, |flat| flat.contains("upgrade"));
        pty.pump(&mut text, PALETTE);
        pty.write(b"\r")?;
        let mut again = String::new();
        pty.pump(&mut again, CARD.min(deadline.saturating_duration_since(Instant::now())));
        let mut second = parse_card(&again);
        while second.as_ref().is_none_or(|t| !complete(t)) && Instant::now() < deadline && again.len() < MAX_OUTPUT {
            pty.pump(&mut again, TICK);
            second = parse_card(&again).or(second);
        }
        if second.is_some() {
            tier = second;
        }
        text.push_str(&again);
    }
    let tier = tier.map(|tier| reconcile_with_auth(tier, subs_active()));

    // 4. Two interrupts is how the TUI is asked to leave; the drop SIGKILLs
    //    it if it declines.
    let _ = pty.write(b"\x03");
    pty.pump(&mut text, Duration::from_millis(200));
    let _ = pty.write(b"\x03");
    pty.pump(&mut text, Duration::from_millis(200));

    // A probe that came back with nothing is a bug in the driver, and the one
    // thing that must never help debug it is the terminal's own bytes. Under
    // `BAAZ_TIER_DEBUG` the driver reports how much it read and which of a
    // fixed list of harmless words it saw — never the output itself.
    if std::env::var_os("BAAZ_TIER_DEBUG").is_some() {
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
    // `BAAZ_TIER_DUMP=<file>` is the one deliberate exception: the person
    // running the probe by hand names a file, and the raw terminal bytes —
    // before and after the Enter — land there, with the flattened screen
    // beside each, so a card that will not draw can be seen. Never set by
    // the app itself.
    if let Some(dump) = std::env::var_os("BAAZ_TIER_DUMP") {
        let report = format!(
            "== before Enter (raw) ==\n{before_enter}\n== before Enter (flattened) ==\n{}\n\
             == after Enter (raw) ==\n{text}\n== after Enter (flattened) ==\n{}\n",
            flatten(&before_enter),
            flatten(&text),
        );
        let _ = std::fs::write(dump, report);
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

/// How long after launch muse needs before its `/upgrade` card reflects the
/// subscription (5–6.5 s measured on muse 1.4.14; see the probe's step 3b).
const STATUS_LOAD: Duration = Duration::from_secs(9);

/// `providers.meta.subs_active` from muse's `auth.json` (written by muse
/// 1.4.x on each credential refresh; absent on older logins). Only flags,
/// never the token, are read.
pub fn subs_active() -> Option<bool> {
    let text = std::fs::read_to_string(crate::auth::auth_path()).ok()?;
    subs_active_in(&text)
}

/// [`subs_active`] over `auth.json`'s text. Pure, for tests.
pub fn subs_active_in(auth_json: &str) -> Option<bool> {
    let value: serde_json::Value = serde_json::from_str(auth_json).ok()?;
    value.get("providers")?.get("meta")?.get("subs_active")?.as_bool()
}

/// The card's answer checked against muse's own `subs_active` flag. A
/// pay-as-you-go card on a credential muse itself marks as subscribed is
/// the card answering before the subscription loaded — never a reason to
/// raise the blocking pay-as-you-go banner over a subscribed login. It reads
/// as a subscription whose plan the card did not name. Every other answer
/// stands: `subs_active: false` (or no flag) never overrides a subscription
/// the card named.
pub fn reconcile_with_auth(tier: Tier, subs_active: Option<bool>) -> Tier {
    match (tier, subs_active) {
        (Tier::PayAsYouGo, Some(true)) => Tier::Subscription {
            plan: GENERIC_PLAN_LABEL.to_owned(),
            current_pct: None,
            weekly_pct: None,
            resets: Vec::new(),
            usage_unavailable: true,
        },
        (tier, _) => tier,
    }
}

/// The literal sentences [`parse_card`] keys on.
///
/// Named constants rather than string literals inside the matcher so the
/// wording is pinned in one place and a test can assert it (finding
/// `support-11`): any TUI rewording lands in [`Tier::Unavailable`], which only
/// warns, so the wording is a fact this build depends on and has to state.
pub const CARD_SUBSCRIBED: &str = "subscribed to the";
/// The sentence muse 1.4.9 draws on a subscription whose plan it cannot
/// name — "You are currently subscribed to a usage plan." — lowercased. A
/// subscription all the same, shown under [`GENERIC_PLAN_LABEL`].
pub const CARD_SUBSCRIBED_UNNAMED: &str = "subscribed to a usage plan";
/// The pay-as-you-go sentences, lowercased, any one of which is a match.
pub const CARD_PAY_AS_YOU_GO: [&str; 4] = [
    "pay-as-you-go",
    "pay as you go",
    "subscriptions aren't currently available",
    "subscriptions are not currently available",
];
/// The sentence muse 1.3.0 draws in place of both percentages while a known
/// plan's usage windows are not being reported — observed while the login's
/// quota sat fully spent, but the card gives no reason, so this is read as
/// "no numbers, ever, this probe" rather than assumed to mean any one cause.
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

/// `…subscribed to the Muse Code High Usage plan.` → `Muse Code High Usage`;
/// `…subscribed to a usage plan.` → [`GENERIC_PLAN_LABEL`]. A named plan wins
/// when the card says both.
fn plan_name(text: &str) -> Option<String> {
    named_plan(text).or_else(|| {
        text.to_lowercase().contains(CARD_SUBSCRIBED_UNNAMED).then(|| GENERIC_PLAN_LABEL.to_owned())
    })
}

/// The plan [`CARD_SUBSCRIBED`] names, when it names one.
fn named_plan(text: &str) -> Option<String> {
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

/// Every `Resets …` clause, cut where the next clause starts. The probe path
/// carries words only: no stamp, so the rows read these as unknown.
fn reset_clauses(text: &str) -> Vec<ResetClause> {
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
        if clause.len() > "Resets".len() && !out.iter().any(|known: &ResetClause| known.text == clause) {
            out.push(ResetClause::from(clause));
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

/// Which `muse` the probe spawns: an explicit path that names a file wins
/// as-is, else the `muse` search (env override, then `PATH`, then the Dock
/// fallbacks) and the login-shell `PATH` — never a bare `"muse"`, which a
/// Dock launch (minimal `PATH`) cannot execute.
fn resolve_probe_muse(muse: &str) -> PathBuf {
    let candidate = Path::new(muse);
    if !muse.is_empty() && candidate.is_file() {
        return candidate.to_path_buf();
    }
    if let Some(program) = provider_muse::resolve_program() {
        return program;
    }
    if let Some(program) = provider::env_path::find_program("muse") {
        return program;
    }
    PathBuf::from(muse)
}

/// The `muse` invocation the probe spawns: the throwaway workspace, sized
/// for the card's own lines, and trusted for this run only.
///
/// A pure builder — no process touched — so the trust flag is a unit-testable
/// fact rather than something only a live probe could catch a regression in.
///
/// The command runs the resolved absolute `muse` with the login-shell
/// `PATH` (the program's own directory first), so a Dock launch still runs
/// a home install. No other env var is changed.
fn probe_command(muse: &str) -> std::process::Command {
    let program = resolve_probe_muse(muse);
    let mut command = std::process::Command::new(&program);
    command.env("PATH", provider::env_path::child_path_for(&program));
    command
        .arg("--workspace")
        .arg(probe_workspace())
        // muse 1.3.0 shows a "Do you trust this workspace?" gate on an
        // untrusted `--workspace` before it will draw anything else, and
        // this probe's own scratch directory (nothing to load: no skills,
        // no rules) is never pre-trusted under a fresh `BAAZ_STATE_DIR`.
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
        // A Baaz process that was force-quit mid-probe left its child behind;
        // SIGKILL it before starting the next one.
        sweep_stale_probe();
        let command = probe_command(muse);
        let pty = Self::spawn(command)?;
        let pid = pty.child.id();
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.push(pid);
        }
        // Best-effort: without it the next probe cannot sweep this one when
        // this Baaz process is force-quit.
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

    /// [`Self::pump`] until the flattened screen satisfies `ready` or
    /// `window` runs out, whichever is first.
    fn pump_until(&mut self, text: &mut String, window: Duration, ready: impl Fn(&str) -> bool) {
        self.pump_until_from(text, 0, window, ready);
    }

    /// [`Self::pump_until`], judging only what arrived past byte `from`.
    fn pump_until_from(&mut self, text: &mut String, from: usize, window: Duration, ready: impl Fn(&str) -> bool) {
        let until = Instant::now() + window;
        while Instant::now() < until && text.len() < MAX_OUTPUT {
            if ready(&flatten(text.get(from..).unwrap_or(""))) {
                return;
            }
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            self.pump(text, TICK.min(until.saturating_duration_since(Instant::now())));
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
        // same orphan.
        // SAFETY: `kill` with a pid and a signal neither reads nor writes.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        // Close the master FIRST, before reaping: an exiting child whose
        // controlling tty still holds unread output blocks in its final tty
        // close until the master side drains or closes, so `wait` before
        // `close` deadlocks parent against child (2026-10-06: ⌘W froze the
        // app for the full close wait every time). Closing first lets the
        // child's tty close complete, and the reap below is bounded anyway.
        // SAFETY: `master` is owned by `self` and closed exactly once.
        unsafe { libc::close(self.master) };
        self.master = -1;
        // Reap boundedly (`waitpid` with `WNOHANG`, [`QUIT_WAIT`] ceiling):
        // a child that still has not exited by then is left for init — its
        // pid file stays, so the next probe's sweep SIGKILLs it — rather
        // than blocking this thread. Never `Child::wait` here: it blocks
        // indefinitely, which is the deadlock above.
        let deadline = Instant::now() + QUIT_WAIT;
        let mut reaped = false;
        loop {
            // SAFETY: `waitpid` with a pid, a status out-pointer and
            // `WNOHANG` neither reads nor writes Rust state.
            let mut status = 0;
            let rc = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
            if rc == pid as libc::pid_t || rc == -1 {
                reaped = rc == pid as libc::pid_t;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Sync `Child`'s state without blocking: already reaped above, or
        // still running (left for init).
        let _ = self.child.try_wait();
        // Always de-registered, reaped or not, so no exit path ever waits on
        // this pid again.
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.retain(|popped| *popped != pid);
        }
        // Only this probe's own entry — and only when reaped: a live child's
        // pid file stays so the next probe's sweep finds and SIGKILLs it. A
        // newer probe may also have written the file since, and removing its
        // pid would blind the next sweep.
        if reaped && read_probe_pid() == Some(pid) {
            let _ = std::fs::remove_file(probe_pid_path());
        }
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
        assert_eq!(
            resets,
            vec![ResetClause::from("Resets at 3:00 PM"), ResetClause::from("Resets Monday")]
        );
        assert!(resets.iter().all(|clause| clause.resets_at_ms.is_none()), "the probe path carries words only");
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
        assert_eq!(
            resets,
            vec![ResetClause::from("Resets at 5:17 PM"), ResetClause::from("Resets Sep 14 at 5:30 AM")]
        );
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
        assert_eq!(CARD_SUBSCRIBED_UNNAMED, "subscribed to a usage plan");
        assert_eq!(CARD_SUBSCRIBED_UNNAMED, CARD_SUBSCRIBED_UNNAMED.to_lowercase());
    }

    #[test]
    fn an_unnamed_usage_plan_is_a_subscription_under_the_generic_label() {
        // muse 1.4.9's sentence for a plan it does not name.
        let card = "Upgrade You are currently subscribed to a usage plan. Current 3% used · Resets 4pm \
                    Weekly 12% used · Resets Oct 20";
        match parse_card(card) {
            Some(Tier::Subscription { plan, current_pct, weekly_pct, .. }) => {
                assert_eq!(plan, GENERIC_PLAN_LABEL);
                assert_eq!(current_pct, Some(3));
                assert_eq!(weekly_pct, Some(12));
            }
            other => panic!("expected a subscription, got {other:?}"),
        }
        // It is not mistaken for a plan called "a".
        assert_eq!(plan_name("subscribed to a usage plan."), Some(GENERIC_PLAN_LABEL.to_owned()));
    }

    #[test]
    fn a_named_plan_wins_over_the_unnamed_sentence() {
        let card = "You are currently subscribed to a usage plan. \
                    You are currently subscribed to the Muse Code High Usage usage plan.";
        assert_eq!(plan_name(card), Some("Muse Code High Usage".to_owned()));
    }

    #[test]
    fn the_pay_as_you_go_card_muse_1_4_9_draws_parses() {
        // Flattened from a live probe on 2026-10-16.
        let card = "No active subscription — you're on pay-as-you-go. Subscribe in Account Center \
                    (https://accountscenter.meta.com/muse_code/?ep=payg) Voice input ❯";
        assert_eq!(parse_card(card), Some(Tier::PayAsYouGo));
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

    /// Owner screenshot 2026-09-16: the status card read "Current usage:
    /// unavailable" while the footer meter read 0%. Neither surface
    /// misreads muse 1.3.0's card — the probe parses the two windows and
    /// the unavailable sentence independently, so one consistent parse
    /// carries all three facts: the current window unreported, the sentence
    /// present, and a reported weekly zero. The status card names the first
    /// two, the meter draws the third. (A stale-cache race between the
    /// footer's paint and the card's open could split them the same way;
    /// both read `self.tier` on their own frames, so only the card's own
    /// bytes — pinned here in `parse_card`'s style, not copied — would close
    /// it. Do not "fix" either surface: each reports what it parsed.)
    #[test]
    fn an_unavailable_current_window_keeps_a_reported_weekly_zero() {
        let card = "You are currently subscribed to the Team plan. \
                    Usage currently unavailable. Weekly 0% used";
        let Some(tier @ Tier::Subscription { current_pct, weekly_pct, usage_unavailable, .. }) =
            parse_card(card)
        else {
            panic!("expected a subscription");
        };
        assert_eq!(current_pct, None);
        assert_eq!(weekly_pct, Some(0));
        assert!(usage_unavailable);
        assert!(tier.status_lines().contains("Current usage: unavailable"));
        assert!(tier.status_lines().contains("Weekly usage: 0% used"));
        assert_eq!(tier.weekly_fraction(), Some(0.0));
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
        assert!(unavailable.status_lines().contains("Current usage: unavailable"));
        assert!(unavailable.status_lines().contains("Weekly usage: unavailable"));
        assert!(!unavailable.status_lines().contains("unavailable used"));
        // Naming the plan either way — this is what keeps a known plan from
        // ever reading as the generic "Muse did not say which plan" banner.
        assert!(unavailable.status_lines().contains("Plan: Power Usage"));
    }

    /// `--print-tier` prints the card's own word, `unavailable`, with no
    /// "used" glued onto it — unlike a real percentage or the still-drawing
    /// dash, both of which are a measurement and read naturally as "N% used"
    /// / "— used".
    /// muse 1.4.x writes `subs_active` beside the credential; Baaz reads
    /// only that flag, and an early pay-as-you-go card on a login muse marks
    /// subscribed never raises the blocking banner (owner, 2026-10-29).
    #[test]
    fn subs_active_overrides_only_an_early_pay_as_you_go() {
        let auth = r#"{"schema_version":1,"providers":{"meta":{"mechanism":"oauth","subs_active":true}}}"#;
        assert_eq!(subs_active_in(auth), Some(true));
        assert_eq!(subs_active_in(r#"{"providers":{"meta":{"mechanism":"oauth"}}}"#), None);
        assert_eq!(subs_active_in("not json"), None);
        let reconciled = reconcile_with_auth(Tier::PayAsYouGo, Some(true));
        assert!(matches!(&reconciled, Tier::Subscription { plan, .. } if plan == GENERIC_PLAN_LABEL), "{reconciled:?}");
        assert!(complete(&reconciled), "a reconciled subscription is a complete answer");
        assert_eq!(reconcile_with_auth(Tier::PayAsYouGo, Some(false)), Tier::PayAsYouGo);
        assert_eq!(reconcile_with_auth(Tier::PayAsYouGo, None), Tier::PayAsYouGo);
        let named = Tier::Subscription {
            plan: "Muse Code High Usage".into(),
            current_pct: Some(3),
            weekly_pct: Some(5),
            resets: Vec::new(),
            usage_unavailable: false,
        };
        assert_eq!(reconcile_with_auth(named.clone(), Some(false)), named, "the flag never demotes a named plan");
    }

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
    fn the_probe_spawns_with_a_repaired_path() {
        // A Dock launch cannot run the probe's child on the inherited
        // `PATH`, so the command carries the login-shell `PATH` itself —
        // and no other env var is added here beyond the pre-existing
        // TERM/LINES/COLUMNS.
        let command = probe_command("muse");
        let path = command
            .get_envs()
            .find(|(key, _)| *key == "PATH")
            .and_then(|(_, value)| value)
            .expect("the probe sets PATH");
        assert!(!path.is_empty(), "PATH is never emptied");
    }

    #[test]
    fn an_explicit_muse_file_wins_as_is() {
        let dir = std::env::temp_dir().join(format!(
            "baaz-tier-muse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("muse");
        std::fs::write(&file, b"fake").expect("seed muse");
        assert_eq!(resolve_probe_muse(&file.to_string_lossy()), file);
        let _ = std::fs::remove_dir_all(&dir);
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
    fn any_orphaned_probe_is_swept_and_nothing_else() {
        let table = "\
            1     0 /sbin/launchd
          32272     1 /Users/me/.local/bin/muse-bin-1.4.4-R5419.1 --workspace /var/folders/h8/T//bz.cQBu/tier-probe --trust-workspace
          32957     1 /Users/me/.local/bin/muse --workspace /Users/me/Library/Application Support/baaz/tier-probe --trust-workspace
          40001  3999 /Users/me/.local/bin/muse-bin-1.4.9-R6297.1 --workspace /tmp/st/tier-probe --trust-workspace
          40002     1 /Users/me/.local/bin/muse-bin-1.4.9-R6297.1 serve
          40003     1 /Users/me/.local/bin/muse-bin-1.4.9-R6297.1 --workspace /Users/me/code/app --trust-workspace
          40004     1 /Users/me/.local/bin/muse-bin-1.4.9-R6297.1 --workspace /Users/me/tier-probe --trust-workspace --yolo
          40005     1 /usr/bin/vim --workspace /tmp/tier-probe --trust-workspace
          40006     1 grep /tier-probe --trust-workspace
          40007     1 /bin/zsh -c ps | grep muse --workspace /x/tier-probe --trust-workspace
        ";
        // The two orphans, in two state directories; never a probe whose Baaz
        // is alive (40001), `muse serve`, the person's own sessions, another
        // program, or a shell line that merely mentions the words.
        assert_eq!(orphaned_probes(table), vec![32272, 32957]);
        let own = format!("{} 1 /bin/muse --workspace /t/tier-probe --trust-workspace", std::process::id());
        assert!(orphaned_probes(&own).is_empty(), "never this process");
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

    /// The verbatim 1.2.1 card, captured and
    /// redacted: the plan name still arrives glued to the template's own
    /// "usage", and both windows carry percentages — `Current 13%`,
    /// `Weekly 36%` in the fixture. The parser takes both, not the first
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
        assert_eq!(
            resets,
            vec![ResetClause::from("Resets at 7:14 PM"), ResetClause::from("Resets Sep 14 at 5:30 AM")]
        );
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
        // percentages that are never coming (a fully-spent quota printed
        // dashes forever under the old rule).
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
        let dir = std::env::temp_dir().join(format!("baaz-lock-test-{}", std::process::id()));
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

    /// `usage/read` with an observation builds the subscription directly —
    /// plan, both percentages and both reset clauses — with the numbers
    /// verbatim, including an over-quota weekly above 100. The meter clamps;
    /// the status lines print the true number.
    #[test]
    fn usage_read_some_builds_the_subscription_verbatim() {
        // Through the call site's own arm (`tier_from_read_value`, what the
        // wire read feeds the probe): a helper pinned below the arm would
        // keep passing with the arm reverted to `None`.
        let result = serde_json::json!({
            "usage": {
                "observedAtMs": 1_786_000_000_000u64,
                "tier": "Muse Code High Usage",
                "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 137 },
                "window": {
                    "resetsAtMs": 1_786_003_020_000u64,
                    "usedPercent": 12,
                    "windowDurationMins": 300,
                },
            },
        });
        let tier = tier_from_read_value(&result).expect("Some(usage) is an answer, not a fallback");
        let Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable } = &tier else {
            panic!("expected a subscription, got {tier:?}");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(*current_pct, Some(12));
        // Over-quota values above 100 are valid and carried verbatim.
        assert_eq!(*weekly_pct, Some(137));
        assert!(!usage_unavailable);
        assert_eq!(resets.len(), 2, "{resets:?}");
        assert!(resets[0].text.starts_with("Resets at "), "{resets:?}");
        assert!(resets[1].text.starts_with("Resets ") && resets[1].text.contains(" at "), "{resets:?}");
        // The meter clamps; the status lines print the true number.
        assert_eq!(tier.weekly_fraction(), Some(1.0));
        assert!(tier.status_lines().contains("Weekly usage: 137% used"), "{}", tier.status_lines());
        assert!(tier.status_lines().contains("Current usage: 12% used"), "{}", tier.status_lines());
    }

    /// `usage/read` on a cold host answers `{}` — the `usage` key absent,
    /// never `null` — and that is "not known yet", not `Unavailable`: it
    /// falls through to the pty fallback rather than settling for unknown.
    #[test]
    fn usage_read_empty_falls_through_to_the_fallback() {
        let cold: serde_json::Value = serde_json::json!({});
        assert_eq!(tier_from_read_value(&cold), None, "the {{}} case falls through, it is not Unavailable");
        // The reading the hub hands over says the same: nothing observed.
        assert_eq!(provider_muse::usage::report_from_read(&cold), None);
    }

    /// A `usage/changed` notification replaces an already-known tier in place
    /// — the footer meter stops being a boot-time snapshot. A malformed frame
    /// never clears what was known.
    #[test]
    fn usage_changed_updates_an_already_known_tier() {
        let known = Tier::Subscription {
            plan: "Muse Code High Usage".into(),
            current_pct: Some(2),
            weekly_pct: Some(2),
            resets: vec!["Resets at 3:00 PM".into()],
            usage_unavailable: false,
        };
        let params = serde_json::json!({
            "observedAtMs": 1_786_000_000_000u64,
            "tier": "Muse Code High Usage",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": { "resetsAtMs": 1_786_003_020_000u64, "usedPercent": 4, "windowDurationMins": 300 },
        });
        let updated = tier_from_changed(&params).expect("a well-formed notification is an answer");
        assert_ne!(updated, known, "the notification moves the tier it found");
        let Tier::Subscription { current_pct, weekly_pct, .. } = &updated else {
            panic!("expected a subscription, got {updated:?}");
        };
        assert_eq!(*current_pct, Some(4));
        assert_eq!(*weekly_pct, Some(9));
        // A frame that does not decode keeps the known tier: the caller only
        // replaces on `Some`.
        assert_eq!(tier_from_changed(&serde_json::json!({"tier": 7})), None);
        assert_eq!(tier_from_changed(&serde_json::json!({})), None);
    }

    /// The wire's reset clauses speak the card's register, in local time:
    /// `Resets at …` for the window, `Resets <date> at …` for the weekly
    /// block, so the two stay apart exactly as the card's two clauses do.
    #[test]
    fn wire_reset_clauses_follow_the_card() {
        let window = format_reset(1_786_003_020_000, false).expect("a representable stamp formats");
        let weekly = format_reset(1_786_012_200_000, true).expect("a representable stamp formats");
        assert!(window.starts_with("Resets at "), "{window}");
        assert!(weekly.starts_with("Resets ") && weekly.contains(" at "), "{weekly}");
        assert_ne!(window, weekly);
    }

    /// One `usage/read` result object with a chosen arrival stamp, for
    /// driving the decision ([`probe_primary`]) without a live connection.
    fn test_read(observed_at_ms: u64) -> serde_json::Value {
        serde_json::json!({
            "usage": {
                "observedAtMs": observed_at_ms,
                "tier": "Muse Code High Usage",
                "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
                "window": {
                    "resetsAtMs": 1_786_003_020_000u64,
                    "usedPercent": 12,
                    "windowDurationMins": 300,
                },
            },
        })
    }

    /// A fresh observation answers from the wire and the scrape never runs.
    ///
    /// This drives the decision ([`probe_primary`]), not the pure builders
    /// below it: deleting the primary-oracle path fails here first.
    #[test]
    fn primary_fresh_observation_answers_without_the_scrape() {
        let mtime = 1_786_000_000u64;
        let read = test_read((mtime + 120) * 1000);
        let mut scraped = false;
        let tier = probe_primary(Some(&read), Some(mtime), || {
            scraped = true;
            Ok(Tier::PayAsYouGo)
        })
        .expect("a current observation is an answer, not a fallback");
        assert!(!scraped, "the fallback must not run when the wire answers");
        let Tier::Subscription { plan, current_pct, weekly_pct, .. } = &tier else {
            panic!("expected a subscription, got {tier:?}");
        };
        assert_eq!(plan, "Muse Code High Usage");
        assert_eq!(*current_pct, Some(12));
        assert_eq!(*weekly_pct, Some(9));
    }

    /// The regression guard: an observation older than the installed
    /// credential belongs to a previous login and falls through to the
    /// scrape, which stays the only oracle that can say pay-as-you-go.
    ///
    /// The shape of the real incident: a subscription turn observed, then a
    /// sign-out and sign-in on pay-as-you-go over the same connection, with
    /// the host still serving the previous account's window. Trusting it
    /// would hide the blocking banner while every turn bills API rates.
    #[test]
    fn primary_stale_observation_falls_through_to_the_scrape() {
        let mtime = 1_786_000_000u64;
        let read = test_read((mtime - 60) * 1000);
        let mut scraped = false;
        let tier = probe_primary(Some(&read), Some(mtime), || {
            scraped = true;
            Ok(Tier::PayAsYouGo)
        })
        .expect("the fallback answers");
        assert!(scraped, "a previous login's observation must not be trusted");
        assert_eq!(tier, Tier::PayAsYouGo);
    }

    /// The tie goes to distrust: an observation stamped in the same whole
    /// second as the credential falls through, and with no `auth.json` at
    /// all nothing is trusted.
    #[test]
    fn primary_same_second_observation_falls_through() {
        let mtime = 1_786_000_000u64;
        let mut scraped = false;
        let read = test_read(mtime * 1000 + 500);
        let tier = probe_primary(Some(&read), Some(mtime), || {
            scraped = true;
            Ok(Tier::PayAsYouGo)
        })
        .expect("the fallback answers");
        assert!(scraped, "same-second is not strictly newer, so it falls through");
        assert_eq!(tier, Tier::PayAsYouGo);
        let mut scraped = false;
        let read = test_read((mtime + 120) * 1000);
        let tier = probe_primary(Some(&read), None, || {
            scraped = true;
            Ok(Tier::PayAsYouGo)
        })
        .expect("the fallback answers");
        assert!(scraped, "no credential trusts nothing");
        assert_eq!(tier, Tier::PayAsYouGo);
    }

    /// No observation (the `{}` cold host) falls through to the scrape.
    #[test]
    fn primary_no_observation_falls_through_to_the_scrape() {
        let mut scraped = false;
        let tier = probe_primary(None, Some(1_786_000_000), || {
            scraped = true;
            Ok(Tier::PayAsYouGo)
        })
        .expect("the fallback answers");
        assert!(scraped, "nothing observed means the scrape decides");
        assert_eq!(tier, Tier::PayAsYouGo);
    }

    /// A `usage/changed` stamped before the installed credential belongs to a
    /// previous login and must not move the tier; a current one still does.
    #[test]
    fn usage_changed_for_a_stale_credential_does_not_update() {
        let mtime = 1_786_000_000u64;
        let stale = serde_json::json!({
            "observedAtMs": (mtime - 60) * 1000,
            "tier": "Muse Code High Usage",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": { "resetsAtMs": 1_786_003_020_000u64, "usedPercent": 4, "windowDurationMins": 300 },
        });
        // Decoding still works; admission is the gate.
        assert!(tier_from_changed(&stale).is_some());
        assert_eq!(
            tier_from_changed_current(&stale, Some(mtime)),
            None,
            "a previous login's notification must not update the tier"
        );
        let fresh = serde_json::json!({
            "observedAtMs": (mtime + 60) * 1000,
            "tier": "Muse Code High Usage",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": { "resetsAtMs": 1_786_003_020_000u64, "usedPercent": 4, "windowDurationMins": 300 },
        });
        let updated = tier_from_changed_current(&fresh, Some(mtime))
            .expect("a current notification is an answer");
        let Tier::Subscription { current_pct, weekly_pct, .. } = &updated else {
            panic!("expected a subscription, got {updated:?}");
        };
        assert_eq!(*current_pct, Some(4));
        assert_eq!(*weekly_pct, Some(9));
        assert_eq!(tier_from_changed_current(&fresh, None), None, "no credential trusts nothing");
    }

    /// A plan string fit to show names the plan in words. The wire (muse
    /// 1.4.0) sends an opaque numeric id in the reading's `tier`
    /// instead — observed live as `27681631238169137` — and anything without
    /// a letter is such an id, never a name.
    #[test]
    fn id_shaped_plan_names_are_never_human() {
        assert!(!is_human_plan_name("27681631238169137"));
        assert!(!is_human_plan_name(""));
        assert!(!is_human_plan_name("   "));
        assert!(!is_human_plan_name("123-456"));
        assert!(is_human_plan_name("Power Usage"));
        assert!(is_human_plan_name("Muse Code High Usage"));
        assert_eq!(resolve_plan_name("Power Usage", None), "Power Usage");
        assert_eq!(resolve_plan_name("27681631238169137", Some("Power Usage")), "Power Usage");
        assert_eq!(resolve_plan_name("27681631238169137", None), "Subscription");
        assert_eq!(resolve_plan_name("27681631238169137", Some("123")), "Subscription");
    }

    /// One wire observation with fixed stamps, for driving the builders
    /// without a live connection.
    fn wire_usage(tier: &str) -> serde_json::Value {
        serde_json::json!({
            "observedAtMs": 1_786_000_000_000u64,
            "tier": tier,
            "weekly": {"resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9},
            "window": {"resetsAtMs": 1_786_003_020_000u64, "usedPercent": 4, "windowDurationMins": 300},
        })
    }

    /// A numeric wire plan with no previous reading reads as the generic
    /// label everywhere — never the raw id — while the reset instants still
    /// ride along for the duration rows.
    #[test]
    fn a_numeric_wire_plan_falls_back_to_the_generic_label() {
        let tier = tier_from_usage(&wire_usage("27681631238169137"));
        let Tier::Subscription { plan, resets, .. } = &tier else {
            panic!("expected a subscription, got {tier:?}");
        };
        assert_eq!(plan, "Subscription");
        assert_eq!(tier.footer_label(), "Subscription");
        assert!(tier.status_lines().contains("Plan: Subscription"), "{}", tier.status_lines());
        assert!(!tier.status_lines().contains("27681631238169137"), "{}", tier.status_lines());
        assert_eq!(resets.len(), 2);
        assert!(resets.iter().all(|clause| clause.resets_at_ms.is_some()), "{resets:?}");
        assert_eq!(resets[0].resets_at_ms, Some(1_786_003_020_000));
        assert_eq!(resets[1].resets_at_ms, Some(1_786_012_200_000));
    }

    /// A numeric wire reading keeps the last human plan name for this login:
    /// first launch read "Power Usage" from the probe, and the usage
    /// notification that follows must not switch the menu to the id.
    #[test]
    fn a_numeric_wire_reading_keeps_the_last_human_plan() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let mtime = 1_786_000_000u64;
        let prev = Cached {
            auth_mtime: Some(mtime),
            tier: Some(Tier::Subscription {
                plan: "Muse Code Power Usage".into(),
                current_pct: Some(0),
                weekly_pct: Some(2),
                resets: vec![ResetClause::from("Resets at 5:17 PM")],
                usage_unavailable: false,
            }),
            last_human_plan: Some("Muse Code Power Usage".into()),
            probed_at_secs: Some(mtime),
            kind_confirmed: true,
        };
        crate::store::write_atomic(&cache_path(), &serde_json::to_vec_pretty(&prev).expect("seed"))
            .expect("seed tier.json");
        let params = serde_json::json!({
            "observedAtMs": (mtime + 60) * 1000,
            "tier": "27681631238169137",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": { "resetsAtMs": 1_786_003_020_000u64, "usedPercent": 4, "windowDurationMins": 300 },
        });
        let tier = tier_from_changed_current(&params, Some(mtime)).expect("a current notification is an answer");
        let Tier::Subscription { plan, current_pct, weekly_pct, .. } = &tier else {
            panic!("expected a subscription, got {tier:?}");
        };
        assert_eq!(plan, "Muse Code Power Usage");
        assert_eq!(tier.footer_label(), "Power Usage");
        assert_eq!(*current_pct, Some(4));
        assert_eq!(*weekly_pct, Some(9));
    }

    /// `remember` persists the last human name alongside the tier and never a
    /// numeric id: a numeric wire reading for the same login keeps the probe's
    /// name, and with no history at all the file holds the generic label. A
    /// failed probe never erases the established name.
    #[test]
    fn remember_persists_the_last_human_plan_and_never_a_numeric_id() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let human = Tier::Subscription {
            plan: "Muse Code Power Usage".into(),
            current_pct: Some(0),
            weekly_pct: Some(2),
            resets: vec![ResetClause::from("Resets at 5:17 PM")],
            usage_unavailable: false,
        };
        let numeric = Tier::Subscription {
            plan: "27681631238169137".into(),
            current_pct: Some(4),
            weekly_pct: Some(9),
            resets: vec![ResetClause::timed("Resets at 5:17 PM".into(), 1_786_003_020_000)],
            usage_unavailable: false,
        };
        remember(&human);
        remember(&numeric);
        let back: Cached = crate::store::read_json(&cache_path());
        assert_eq!(back.auth_mtime, auth_mtime());
        assert_eq!(back.last_human_plan.as_deref(), Some("Muse Code Power Usage"));
        let Tier::Subscription { plan, .. } = back.tier.expect("a tier was remembered") else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code Power Usage");
        // A failed probe keeps the established name.
        remember(&Tier::Unavailable("no tty".into()));
        let back: Cached = crate::store::read_json(&cache_path());
        assert_eq!(back.last_human_plan.as_deref(), Some("Muse Code Power Usage"));
        // No history at all: the generic label, never the id.
        std::fs::remove_file(cache_path()).expect("clear tier.json");
        remember(&numeric);
        let back: Cached = crate::store::read_json(&cache_path());
        assert_eq!(back.last_human_plan, None);
        let Tier::Subscription { plan, .. } = back.tier.expect("a tier was remembered") else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Subscription");
    }

    /// `tier.json` as older builds wrote it — string clauses, no
    /// `lastHumanPlan`, possibly a numeric plan — still reads, and resolves
    /// to words, never an id.
    #[test]
    fn old_tier_json_without_new_keys_still_reads() {
        let old = r#"{"authMtime":1,"tier":{"tier":"subscription","plan":"Muse Code Power Usage","currentPct":0,"weeklyPct":2,"resets":["Resets at 5:17 PM","Resets Sep 14 at 5:30 AM"],"usageUnavailable":false}}"#;
        let cached: Cached = serde_json::from_str(old).expect("old tier.json deserialises");
        assert_eq!(cached.last_human_plan, None);
        let Tier::Subscription { plan, resets, .. } = cached.tier.expect("a tier") else {
            panic!("expected a subscription");
        };
        assert_eq!(plan, "Muse Code Power Usage");
        assert_eq!(resets.len(), 2);
        assert!(resets.iter().all(|clause| clause.resets_at_ms.is_none()));
        // And a numeric plan from such a file never reaches a surface.
        let old_numeric = r#"{"authMtime":1,"tier":{"tier":"subscription","plan":"27681631238169137","currentPct":0,"weeklyPct":5,"resets":["Resets at 7:57 AM"],"usageUnavailable":false}}"#;
        let cached: Cached = serde_json::from_str(old_numeric).expect("old tier.json deserialises");
        let tier = cached.tier.expect("a tier");
        assert_eq!(tier.footer_label(), "Subscription");
        assert!(!tier.status_lines().contains("27681631238169137"), "{}", tier.status_lines());
    }

    /// Clauses round-trip through `tier.json`: timed clauses as maps carrying
    /// the instant, probe-style clauses as the plain strings older builds
    /// wrote — and those strings still read.
    #[test]
    fn reset_clauses_keep_their_instant_across_tier_json() {
        let clause = ResetClause::timed("Resets at 5:17 PM".into(), 1_786_003_020_000);
        let value = serde_json::to_value(&clause).expect("a timed clause serialises");
        assert_eq!(value, serde_json::json!({"text": "Resets at 5:17 PM", "resetsAtMs": 1_786_003_020_000_i64}));
        let back: ResetClause = serde_json::from_value(value).expect("a timed clause deserialises");
        assert_eq!(back, clause);
        let plain: ResetClause = serde_json::from_str("\"Resets at 5:17 PM\"").expect("a string clause reads");
        assert_eq!(plain, ResetClause::from("Resets at 5:17 PM"));
        assert_eq!(plain.resets_at_ms, None);
        let plain_value = serde_json::to_value(&plain).expect("a plain clause serialises");
        assert_eq!(plain_value, serde_json::Value::String("Resets at 5:17 PM".into()));
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

    /// A scripted PTY fallback: `answers` in order, counting calls, so the
    /// confirmation loop is driven with explicit timestamps and no live
    /// connection, no pseudo-terminal, and no waits.
    fn scripted_probe(
        answers: Vec<Result<Tier, String>>,
        calls: &std::cell::Cell<usize>,
    ) -> impl FnMut() -> Result<Tier, String> + '_ {
        let mut answers = answers.into_iter();
        move || {
            calls.set(calls.get() + 1);
            answers.next().expect("more PTY probes than scripted answers")
        }
    }

    /// One subscription answer for the confirmation tests.
    fn test_subscription() -> Tier {
        Tier::Subscription {
            plan: "Muse Code High Usage".into(),
            current_pct: Some(2),
            weekly_pct: Some(2),
            resets: vec![],
            usage_unavailable: false,
        }
    }

    /// A one-off pay-as-you-go followed by a subscription resolves to the
    /// subscription: the provisional answer is never surfaced and never
    /// remembered.
    #[test]
    fn one_off_payg_then_subscription_resolves_to_subscription() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let mtime = 1_786_000_000u64;
        let now = mtime + 3600;
        let calls = std::cell::Cell::new(0);
        let sleeps = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            None,
            Some(mtime),
            now,
            scripted_probe(vec![Ok(Tier::PayAsYouGo), Ok(test_subscription())], &calls),
            || sleeps.set(sleeps.get() + 1),
        )
        .expect("the subscription answers");
        assert!(matches!(tier, Tier::Subscription { .. }), "got {tier:?}");
        assert_eq!(calls.get(), 2, "the provisional answer re-probes once");
        assert_eq!(sleeps.get(), 1, "confirmation probes run apart, on the background path");
        // Remembering the decision persists the subscription, never the
        // provisional pay-as-you-go.
        remember_at(&tier, auth_mtime(), now_secs());
        let back: Cached = crate::store::read_json(&cache_path());
        assert!(matches!(back.tier, Some(Tier::Subscription { .. })), "got {:?}", back.tier);
    }

    /// Two consecutive pay-as-you-go answers confirm pay-as-you-go.
    #[test]
    fn two_consecutive_payg_answers_confirm_payg() {
        let mtime = 1_786_000_000u64;
        let calls = std::cell::Cell::new(0);
        let sleeps = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            None,
            Some(mtime),
            mtime + 3600,
            scripted_probe(vec![Ok(Tier::PayAsYouGo), Ok(Tier::PayAsYouGo)], &calls),
            || sleeps.set(sleeps.get() + 1),
        )
        .expect("two agreeing probes are an answer");
        assert_eq!(tier, Tier::PayAsYouGo);
        assert_eq!(calls.get(), 2, "confirmation costs one re-probe");
        assert_eq!(sleeps.get(), 1);
    }

    /// A provisional pay-as-you-go whose re-probe fails settles as unknown,
    /// never as pay-as-you-go.
    #[test]
    fn a_failed_reprobe_never_surfaces_payg() {
        let mtime = 1_786_000_000u64;
        let calls = std::cell::Cell::new(0);
        let result = probe_primary_confirmed(
            None,
            Some(mtime),
            mtime + 3600,
            scripted_probe(
                vec![Ok(Tier::PayAsYouGo), Err("the /upgrade card did not answer in time".into())],
                &calls,
            ),
            || {},
        );
        assert!(result.is_err(), "got {result:?}");
        assert_eq!(calls.get(), 2);
    }

    /// A card answer probed within a minute of a credential refresh is
    /// provisional even when it names a subscription; an older credential's
    /// subscription is still accepted at once.
    #[test]
    fn a_just_refreshed_credential_makes_a_subscription_provisional() {
        let mtime = 1_786_000_000u64;
        // Long after the refresh: one probe is the answer.
        let calls = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            None,
            Some(mtime),
            mtime + 3600,
            scripted_probe(vec![Ok(test_subscription())], &calls),
            || panic!("no re-probe past a refresh"),
        )
        .expect("a settled credential's subscription answers at once");
        assert!(matches!(tier, Tier::Subscription { .. }));
        assert_eq!(calls.get(), 1);
        // Seconds after the refresh: the same answer needs one agreeing read.
        let calls = std::cell::Cell::new(0);
        let sleeps = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            None,
            Some(mtime),
            mtime + 10,
            scripted_probe(vec![Ok(test_subscription()), Ok(test_subscription())], &calls),
            || sleeps.set(sleeps.get() + 1),
        )
        .expect("two agreeing probes are an answer");
        assert!(matches!(tier, Tier::Subscription { .. }));
        assert_eq!(calls.get(), 2, "a refresh just happened: re-probe once");
        assert_eq!(sleeps.get(), 1);
        assert!(is_auth_fresh(Some(mtime), mtime + 10));
        assert!(!is_auth_fresh(Some(mtime), mtime + 3600));
        assert!(!is_auth_fresh(None, mtime + 10));
    }

    /// A remembered `usage_unavailable` expires after about five minutes,
    /// and a successful wire read always replaces it with numbers.
    #[test]
    fn an_unavailable_answer_expires_while_a_wire_read_replaces_it() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let mtime = auth_mtime();
        let taken = now_secs();
        let mut dark = test_subscription();
        let Tier::Subscription { usage_unavailable, current_pct, weekly_pct, .. } = &mut dark else {
            unreachable!();
        };
        *usage_unavailable = true;
        *current_pct = None;
        *weekly_pct = None;
        remember_at(&dark, mtime, taken);
        assert!(
            matches!(cached_at(taken + 240), Some(Tier::Subscription { .. })),
            "a four-minute-old unavailable is still trusted"
        );
        assert_eq!(cached_at(taken + 361), None, "past about five minutes it expires");
        assert!(unavailable_answer_stale(None, taken), "a file from before the stamp reads as stale");
        // ...and the re-read answers from the wire when it can: a current
        // observation builds numbers, replacing the dark card outright.
        // A fixed mtime here (like the pay-as-you-go override test):
        // `auth_mtime` reads the real config dir, which the sandbox does
        // not cover, so only a fixed stamp is a current observation.
        let mtime = 1_786_000_000u64;
        let changed = serde_json::json!({
            "observedAtMs": (mtime + 60) * 1000,
            "tier": "Muse Code High Usage",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": {
                "resetsAtMs": 1_786_003_020_000u64,
                "usedPercent": 4,
                "windowDurationMins": 300,
            },
        });
        let tier = tier_from_changed_current(&changed, Some(mtime)).expect("a current wire read builds");
        let Tier::Subscription { current_pct, weekly_pct, usage_unavailable, .. } = &tier else {
            panic!("expected a subscription, got {tier:?}");
        };
        assert_eq!((*current_pct, *weekly_pct), (Some(4), Some(9)));
        assert!(!*usage_unavailable, "numbers replace the dark card");
    }

    /// A remembered pay-as-you-go older than ten minutes re-verifies on the
    /// next boot; a remembered subscription keeps today's behaviour.
    #[test]
    fn a_stale_remembered_payg_reverifies_while_a_subscription_stays_trusted() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let mtime = auth_mtime();
        let taken = now_secs();
        remember_at(&Tier::PayAsYouGo, mtime, taken);
        assert_eq!(cached_at(taken + 30), Some(Tier::PayAsYouGo), "a fresh pay-as-you-go is still trusted");
        assert_eq!(cached_at(taken + 601), None, "older than ten minutes: the next boot re-probes");
        assert!(payg_answer_stale(None, taken), "a file from before the stamp reads as stale");
        remember_at(&test_subscription(), mtime, taken);
        assert!(
            matches!(cached_at(taken + 601), Some(Tier::Subscription { .. })),
            "a remembered subscription keeps today's behaviour"
        );
    }

    /// A current wire observation replaces a remembered pay-as-you-go
    /// without any PTY probe — on `usage/read` and on `usage/changed`.
    #[test]
    fn a_current_wire_observation_overrides_a_remembered_payg_without_probing() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        // The cache does not satisfy a stale pay-as-you-go in the first
        // place: the next boot re-verifies instead of trusting it.
        remember_at(&Tier::PayAsYouGo, auth_mtime(), now_secs().saturating_sub(3600));
        assert_eq!(
            cached_at(now_secs()),
            None,
            "a pay-as-you-go remembered over ten minutes ago re-verifies"
        );
        // ...and the re-verification answers from the wire when it can.
        let mtime = 1_786_000_000u64;
        let read = test_read((mtime + 60) * 1000);
        let calls = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            wire_report(&read).as_ref(),
            Some(mtime),
            mtime + 3600,
            scripted_probe(vec![Ok(Tier::PayAsYouGo)], &calls),
            || panic!("the wire answers first: no PTY probe runs"),
        )
        .expect("a current observation is an answer");
        assert!(matches!(tier, Tier::Subscription { .. }), "got {tier:?}");
        assert_eq!(calls.get(), 0);
        let changed = serde_json::json!({
            "observedAtMs": (mtime + 60) * 1000,
            "tier": "Muse Code High Usage",
            "weekly": { "resetsAtMs": 1_786_012_200_000u64, "usedPercent": 9 },
            "window": {
                "resetsAtMs": 1_786_003_020_000u64,
                "usedPercent": 4,
                "windowDurationMins": 300,
            },
        });
        assert!(
            tier_from_changed_current(&changed, Some(mtime)).is_some(),
            "a current usage/changed replaces a remembered pay-as-you-go"
        );
    }

    /// Remembering pay-as-you-go across a credential refresh keeps the last
    /// human plan name instead of writing it away.
    #[test]
    fn remembering_payg_across_a_refresh_keeps_the_last_human_plan() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let mtime = auth_mtime();
        let taken = now_secs();
        remember_at(&test_subscription(), mtime, taken);
        // A refresh installs a new auth.json: the next remember lands under
        // a different mtime key.
        let refreshed = mtime.map(|m| m.saturating_add(120)).or(Some(taken));
        assert_ne!(refreshed, mtime);
        remember_at(&Tier::PayAsYouGo, refreshed, taken + 120);
        let back: Cached = crate::store::read_json(&cache_path());
        assert_eq!(back.tier, Some(Tier::PayAsYouGo));
        assert_eq!(
            back.last_human_plan.as_deref(),
            Some("Muse Code High Usage"),
            "a pay-as-you-go reading names no plan, so it must not erase the established one"
        );
    }

    /// B7 review: a deliberate sign-out forgets the plan, so another account
    /// signing in next never inherits this one's name.
    #[test]
    fn sign_out_forgets_the_last_human_plan() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        remember_at(&test_subscription(), auth_mtime(), now_secs());
        forget();
        let back: Cached = crate::store::read_json(&cache_path());
        assert_eq!(back.last_human_plan, None);
        assert_eq!(back.tier, None);
    }

#[cfg(test)]
mod usage_fraction_tests {
    use super::*;

    #[test]
    fn a_current_window_only_read_still_has_a_usage_fraction() {
        let tier = Tier::Subscription {
            plan: "Power Usage".into(),
            current_pct: Some(40),
            weekly_pct: None,
            resets: Vec::new(),
            usage_unavailable: false,
        };
        assert_eq!(tier.weekly_fraction(), None);
        assert_eq!(tier.usage_fraction(), Some(0.4));
    }
}

    /// A numeric-id wire reading (muse 1.4.x) is numbers, not a plan kind:
    /// a pay-as-you-go login gets them too (live 2026-10-17, muse 1.4.10 —
    /// the card said pay-as-you-go, the first turn's frame flipped the
    /// footer to "Subscription"). It refreshes a known subscription and
    /// never decides the kind; a reading naming its plan still does.
    #[test]
    fn a_numeric_wire_reading_never_decides_the_plan_kind() {
        let numeric = wire_usage("27681631238169137");
        let named = wire_usage("Muse Code High Usage");
        assert!(!wire_settles_kind(&numeric, Some(&Tier::PayAsYouGo)), "never over a pay-as-you-go");
        assert!(!wire_settles_kind(&numeric, None), "never over an unknown plan");
        assert!(!wire_settles_kind(&numeric, Some(&Tier::Unavailable("x".into()))));
        assert!(wire_settles_kind(&numeric, Some(&test_subscription())), "it refreshes a known subscription");
        assert!(wire_settles_kind(&named, Some(&Tier::PayAsYouGo)), "a named plan still settles the kind");
        let read = serde_json::json!({"usage": numeric});
        assert!(!wire_names_plan(&read), "the usage/read shape too");
    }

    /// The probe's wire arm answers only for a reading that names its plan:
    /// a numeric-id `usage/read` falls through to the card, which is what
    /// says pay-as-you-go.
    #[test]
    fn a_numeric_usage_read_falls_through_to_the_card() {
        let mtime = 1_786_000_000u64;
        let mut read = test_read((mtime + 60) * 1000);
        read["usage"]["tier"] = serde_json::json!("27681631238169137");
        let calls = std::cell::Cell::new(0);
        let tier = probe_primary_confirmed(
            wire_report(&read).as_ref(),
            Some(mtime),
            mtime + 3600,
            scripted_probe(vec![Ok(Tier::PayAsYouGo), Ok(Tier::PayAsYouGo)], &calls),
            || {},
        );
        assert_eq!(tier, Ok(Tier::PayAsYouGo));
        assert_eq!(calls.get(), 2, "the card answered, confirmed once");
    }

    /// A subscription remembered from a numeric-id reading alone is not
    /// served from the cache: the next boot asks the card. One that a
    /// confirmed subscription stands behind keeps being served.
    #[test]
    fn an_unconfirmed_wire_subscription_is_not_served_from_the_cache() {
        let _sandbox = crate::provider_status::TestSandbox::hold();
        let numeric = tier_from_usage(&wire_usage("27681631238169137"));
        remember_observed(&numeric, false);
        assert_eq!(cached_at(now_secs()), None, "the card decides the kind on the next boot");
        remember(&test_subscription());
        remember_observed(&numeric, false);
        assert!(
            matches!(cached_at(now_secs()), Some(Tier::Subscription { .. })),
            "a confirmed subscription's numbers refresh and stay served"
        );
    }
}

/// T1 (2026-10-06): ⌘W / the red dot froze the app because `Pty::drop`
/// waited on a child that was itself waiting on the pty master, and the
/// close hooks then slept the full probe wait on the main thread. One
/// sequential test pins all three bounds, in one function so parallel unit
/// tests cannot interleave on the shared [`LIVE_PROBES`] registry.
#[cfg(test)]
mod t1_close_tests {
    use super::*;

    fn live_contains(pid: u32) -> bool {
        LIVE_PROBES.lock().map(|live| live.contains(&pid)).unwrap_or(false)
    }

    fn live_forget(pid: u32) {
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.retain(|popped| *popped != pid);
        }
    }

    #[test]
    fn window_close_never_waits_and_pty_drop_cannot_deadlock() {
        // 1. The drop: a real child under the probe's own pty constructor,
        // flooding the tty with nobody reading the master. Dropping the
        // `Pty` must return within a second and de-register the pid.
        let mut flood = std::process::Command::new("/bin/sh");
        flood.arg("-c").arg("while :; do echo x; done");
        let pty = Pty::spawn(flood).expect("a pty for the flooding child");
        let flood_pid = pty.child.id();
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.push(flood_pid);
        }
        // Let the child's output fill the pty buffer, so it is blocked in a
        // tty write exactly like the stuck `muse` TUI was.
        std::thread::sleep(Duration::from_millis(200));
        let start = Instant::now();
        drop(pty);
        assert!(start.elapsed() < Duration::from_secs(1), "Pty::drop blocked {:?}", start.elapsed());
        assert!(!live_contains(flood_pid), "the dropped probe stays registered");

        // 2. The window-close cleanup: a live probe that never leaves the
        // registry (its SIGKILLed child has no `Pty` left to drop it, the
        // stuck case). Signalling only — it must return in milliseconds.
        let mut stuck =
            std::process::Command::new("/bin/sleep").arg("30").spawn().expect("a stuck-probe stand-in");
        let stuck_pid = stuck.id();
        if let Ok(mut live) = LIVE_PROBES.lock() {
            live.push(stuck_pid);
        }
        let start = Instant::now();
        cleanup_probes_for_close();
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "window-close cleanup slept {:?} on a stuck probe",
            start.elapsed()
        );

        // 3. The quit cleanup with the same stuck registration: bounded by
        // `QUIT_WAIT`, never the old multi-second wait. The slack is for a
        // shared CI runner: 100 ms over the 500 ms wait failed there at
        // 624 ms (2026-10-17 and -18) while catching nothing a 500 ms
        // margin would miss.
        let start = Instant::now();
        cleanup_probes();
        assert!(
            start.elapsed() <= QUIT_WAIT + Duration::from_millis(500),
            "quit cleanup took {:?} with a stuck probe",
            start.elapsed()
        );

        // 4. The quit cleanup with nothing live: immediate, no wait at all.
        live_forget(stuck_pid);
        let _ = stuck.kill();
        let _ = stuck.wait();
        let start = Instant::now();
        cleanup_probes();
        assert!(
            start.elapsed() < Duration::from_millis(50),
            "quit cleanup with no probe slept {:?}",
            start.elapsed()
        );
    }

}
