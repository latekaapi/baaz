//! The billing-tier glue: the probe's lifecycle and the mode it reports.
//!
//! [`crate::tier`] owns the oracle — what the `muse` TUI's `/upgrade` card
//! says, and the cache beside it. This module owns the application's half of
//! it: when a probe runs, where its answer is kept, and how the answer
//! reaches the one billing rule ([`baaz_core::billing::report`], docs/85
//! 11.6) — the same rule every provider's sends go through. [`Harness`]
//! keeps only `tier` and `tier_probing`; nothing else in the app reads the
//! probe's internals.
//!
//! The rule the whole file exists for (`docs/06-billing.md`): **a probe that
//! failed never stops the app**. A failure leaves a login whose plan kind is
//! known as it was and a login never confirmed on [`Tier::Unavailable`]
//! ([`tier::settle_probe`]); both get
//! another probe by themselves, bounded ([`tier::ProbeRetry`]).
//!
//! Why the bound and the keeping (2026-11-04): one failed boot probe set
//! [`Tier::Unavailable`] and nothing ever looked again. The menu's re-read is
//! wire-only, reads nothing on a cold host, and a numeric-id reading cannot
//! settle an unknown; only pay-as-you-go re-verified. The owner's subscribed
//! window carried the banner for days, on every Muse session.

use gpui::Context;

use crate::app::Harness;
use crate::tier::{self, Tier};
use crate::wire::WireCall;

impl Harness {
    /// Find out what this login is entitled to (see `docs/06-billing.md`).
    ///
    /// The cache answers the ordinary boot; a probe only runs when `auth.json`
    /// has changed since the cached answer was taken, when a remembered
    /// pay-as-you-go is older than ten minutes, or when `force` says the
    /// person asked. The probe asks `usage/read` first and falls through to
    /// the `/upgrade` screen scrape on a cold host or any read failure — the
    /// scrape stays the only oracle for pay-as-you-go. A pay-as-you-go card
    /// answer is provisional until a second probe agrees, with the waits on
    /// the background path; meanwhile the mode stays what it was. **A probe that
    /// fails never stops the app**: see [`Self::land_tier_probe`].
    pub(crate) fn probe_tier(&mut self, force: bool, cx: &mut Context<Self>) {
        // `--tier` fakes the probe for a screenshot, and nothing else.
        if let Some(faked) = self.args.tier.clone() {
            self.tier = Some(faked);
            self.push_tier(cx);
            return;
        }
        if !force {
            if let Some(cached) = tier::cached() {
                self.tier = Some(cached);
                self.tier_retry_settled();
                self.push_tier(cx);
                return;
            }
        }
        self.start_tier_probe(force, cx);
    }

    /// Run the probe. `report`: a person asked, so the answer gets a toast.
    fn start_tier_probe(&mut self, report: bool, cx: &mut Context<Self>) {
        if self.tier_probing {
            return;
        }
        self.tier_probing = true;
        self.push_tier(cx);
        let program = self.args.program.clone();
        let slot = self.providers.clone();
        self.wire_call(
            cx,
            move || {
                // The wire read stays at the edge: `probe_primary_confirmed`
                // decides from the already-read observation, which is what
                // makes the decision reachable from a test without a live
                // connection. A provisional pay-as-you-go re-probes here, on
                // the background path, while the mode stays what it was.
                let read = read_usage(&slot);
                tier::probe_primary_confirmed(
                    read.as_ref(),
                    tier::auth_mtime(),
                    tier::now_secs(),
                    || run_probe(&program),
                    || std::thread::sleep(tier::PAYG_CONFIRM_INTERVAL),
                )
            },
            move |this, result, cx| this.land_tier_probe(result, report, cx),
        );
    }

    /// A finished probe. A verdict lands. No verdict changes nothing for a
    /// login whose kind is known, reports unknown for one that is
    /// not, and either way owes another probe.
    pub(crate) fn land_tier_probe(&mut self, answer: Result<Tier, String>, report: bool, cx: &mut Context<Self>) {
        self.tier_probing = false;
        let known = tier::known_now().or_else(|| matches!(self.tier, Some(Tier::PayAsYouGo)).then_some(Tier::PayAsYouGo));
        let settled = tier::settle_probe(answer, known);
        // The reason is the module's own words, never the terminal's. It
        // used to go nowhere, and a four-day-old banner could not say why.
        if let Some(reason) = &settled.unanswered {
            crate::baaz_log!("tier probe: no verdict ({reason}); holding {}", settled.tier.footer_label());
        }
        if settled.remember {
            tier::remember(&settled.tier);
        }
        // The window's own numbers are at least as new as the file's.
        let keep_numbers =
            settled.unanswered.is_some() && matches!((&self.tier, &settled.tier), (Some(Tier::Subscription { .. }), Tier::Subscription { .. }));
        if !keep_numbers {
            self.tier = Some(settled.tier.clone());
        }
        match &settled.unanswered {
            None => self.tier_retry_settled(),
            Some(_) => self.owe_tier_probe(cx),
        }
        self.push_tier(cx);
        // An asked-for probe reports back; a boot probe that found the
        // plan simply reports its mode.
        if report {
            let (title, detail) = match (&settled.tier, &settled.unanswered) {
                (Tier::Unavailable(reason), _) => ("Still unknown", format!("Still unknown: {reason}")),
                (tier, Some(reason)) => {
                    ("Could not check", format!("Keeping {}: {reason}", tier.footer_label()))
                }
                (Tier::Subscription { plan, .. }, None) => ("Plan checked", format!("Plan: {plan}")),
                (Tier::PayAsYouGo, None) => {
                    ("Plan checked", "Plan: pay-as-you-go — every turn bills API usage".to_owned())
                }
            };
            self.overlays.update(cx, |overlays, cx| overlays.toast(title, detail, cx));
        }
    }

    /// The probe owed after one with no verdict: on the backoff's clock, and
    /// nothing at all once that is spent, until an occasion.
    fn owe_tier_probe(&mut self, cx: &mut Context<Self>) {
        let delay = self.tier_retry.failed(tier::now_secs());
        self.tier_retry_timer = delay.map(|delay| self.tier_probe_after(delay, cx));
    }

    fn tier_probe_after(&self, delay: std::time::Duration, cx: &mut Context<Self>) -> gpui::Task<()> {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                // Signed out meanwhile: the login is gone, and so is the debt.
                if matches!(this.auth, crate::login::Auth::SignedIn(_)) {
                    this.start_tier_probe(false, cx);
                } else {
                    this.tier_retry_settled();
                }
            });
        })
    }

    /// Nothing is owed any more: a verdict, or the login went away.
    pub(crate) fn tier_retry_settled(&mut self) {
        self.tier_retry.settled();
        self.tier_retry_timer = None;
    }

    /// The machine woke: an owed probe gets another go, once the network is
    /// back. The timers themselves do not count asleep, so a thirty-minute
    /// wait over a night is still thirty minutes away without this.
    pub(crate) fn tier_probe_on_wake(&mut self, cx: &mut Context<Self>) {
        if self.tier_retry.woke() {
            self.tier_retry_timer = Some(self.tier_probe_after(tier::PROBE_AFTER_WAKE, cx));
        }
    }

    /// A billing-tier lane just opened: the person is about to send there,
    /// so an owed probe past its gap runs now.
    pub(crate) fn tier_probe_on_muse_lane(&mut self, cx: &mut Context<Self>) {
        if self.tier_retry.occasion_due(tier::now_secs()) {
            self.start_tier_probe(false, cx);
        }
    }

    /// Hand the tier's billing mode to the one rule (docs/85 11.6): the
    /// header chip and the next send's check read it from there.
    pub(crate) fn push_tier(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = crate::providers::ProviderId::with(provider::ProviderTrait::BillingTier) {
            baaz_core::billing::report(id, self.tier.as_ref().map(Tier::billing_mode));
        }
        cx.notify();
    }

    /// Fold one usage reading (`UsageChanged`) into the tier in place: the
    /// footer meter and the billing mode follow through [`Self::push_tier`].
    ///
    /// A frame that does not decode keeps the known tier — it never clears
    /// it — and so does a frame stamped before the installed credential: the
    /// wire carries no account identity, and a notification for a login that
    /// is no longer installed must not move the tier. Cached through
    /// [`tier::remember`] exactly as a probe answer is, and persisted
    /// beside it as the Muse usage snapshot, so the account menu shows
    /// the newest reading with its true age.
    pub(crate) fn apply_usage_changed(&mut self, report: &provider::UsageReport, cx: &mut Context<Self>) {
        let admitted = tier::report_settles_kind(report, self.tier.as_ref());
        if let Some(tier) = tier::tier_from_report_current(report, tier::auth_mtime()).filter(|_| admitted) {
            tier::remember_observed(&tier, tier::report_names_plan(report));
            // The snapshot carries the notification's own arrival time,
            // and a tier with no numbers never wipes the last good one:
            // the rows keep it with "unavailable now" beside it.
            if tier::tier_has_numbers(&tier) {
                crate::provider_status::record_muse_snapshot_at(
                    Some(tier.footer_label()),
                    tier.usage_fraction().map(f64::from),
                    report.observed_at,
                );
            } else {
                crate::provider_status::note_muse_unavailable_now();
            }
            self.tier = Some(tier);
            self.push_tier(cx);
        }
        cx.notify();
    }

    /// A quiet Muse usage re-read for the account menu: the wire only
    /// (`Command::ReadUsage`), never the pty scrape, never a toast.
    /// A current observation replaces the tier and the stored Muse
    /// snapshot; anything else — a cold host, a stale frame, a failed
    /// read — keeps what the menu had. The caller marks Muse refreshing;
    /// this clears it and repaints either way, so the "Refreshing…" row
    /// always settles.
    pub(crate) fn refresh_muse_usage(&mut self, cx: &mut Context<Self>) {
        let slot = self.providers.clone();
        let held = self.tier.clone();
        self.wire_call(
            cx,
            move || {
                // A panic on the background path must never stick the row
                // on "Refreshing…": the completion below still runs and
                // clears it (and the 20 s guard bounds a hang).
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let report = read_usage(&slot)?;
                    let observed_at = report.observed_at;
                    // A numeric-id reading never decides the plan kind.
                    if !tier::report_settles_kind(&report, held.as_ref()) {
                        return None;
                    }
                    let named = tier::report_names_plan(&report);
                    tier::tier_from_report_current(&report, tier::auth_mtime()).map(|tier| (tier, observed_at, named))
                }));
                outcome.ok().flatten()
            },
            move |this, result, cx| {
                match result {
                    Some((tier, observed_at, named)) if tier::tier_has_numbers(&tier) => {
                        tier::remember_observed(&tier, named);
                        crate::provider_status::record_muse_snapshot_at(
                            Some(tier.footer_label()),
                            tier.usage_fraction().map(f64::from),
                            observed_at,
                        );
                        this.tier = Some(tier);
                        this.push_tier(cx);
                    }
                    // A cold host, a stale frame, a failed read, or a tier
                    // with no numbers: keep what the menu had (and, when a
                    // good snapshot stands, say "unavailable now" beside
                    // it) — either way the row settles.
                    _ => crate::provider_status::note_muse_unavailable_now(),
                }
                let billed: Vec<_> = crate::providers::ProviderId::with(provider::ProviderTrait::BillingTier).into_iter().collect();
                this.set_usage_refreshing(&billed, false, cx);
            },
        );
    }
}

/// The card. Tests never reach a real CLI: a test window that signs in, or
/// a retry's timer under a test clock, lands on this refusal instead.
fn run_probe(program: &str) -> Result<Tier, String> {
    #[cfg(feature = "probe")]
    #[cfg(test)]
    {
        let _ = program;
        Err("tests never reach a real CLI".to_owned())
    }
    #[cfg(not(all(test, feature = "probe")))]
    {
        tier::probe(program)
    }
}

impl Harness {
    /// `billing-last:<provider>:<mode>`: the capture's last send on that
    /// provider's account was in `mode` (docs/85 11.6).
    #[cfg(feature = "probe")]
    pub(crate) fn step_billing_last(&mut self, rest: &str) -> crate::steps::StepResult {
        let (provider, mode) = rest.split_once(':').ok_or_else(|| format!("billing-last: want <provider>:<mode>, got {rest:?}"))?;
        let id = crate::providers::ProviderId::parse_known(provider).ok_or_else(|| format!("billing-last: unknown provider {provider:?}"))?;
        let mode = baaz_core::billing::BillingMode::parse(mode);
        if !mode.is_known() {
            return Err(format!("billing-last: want subscription or perToken, got {rest:?}"));
        }
        baaz_core::billing::fixture_last_send(id, mode);
        Ok(())
    }
}

/// Muse's latest usage reading over the control lane: `None` when Muse is
/// not up, the read failed, or the host has observed nothing yet. Blocking:
/// call it on a background thread. A failed read never stops the app and
/// never becomes [`Tier::Unavailable`] directly.
fn read_usage(slot: &crate::providers::Registry) -> Option<provider::UsageReport> {
    match slot.control().and_then(|lane| lane.send(provider::Command::ReadUsage)) {
        Ok(provider::Ack::Usage { report }) => {
            crate::baaz_log!("usage read → {}", if report.is_some() { "a reading" } else { "nothing observed yet" });
            report
        }
        Ok(other) => {
            crate::baaz_log!("usage read answered {other:?}");
            None
        }
        Err(error) => {
            crate::baaz_log!("usage read failed: {error}");
            None
        }
    }
}
