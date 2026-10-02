//! The billing-tier glue: the probe's lifecycle and the banner it produces.
//!
//! [`crate::tier`] owns the oracle — what the `muse` TUI's `/upgrade` card
//! says, and the cache beside it. This module owns the application's half of
//! it: when a probe runs, where its answer is kept, and how the answer reaches
//! the open session as a [`TierBanner`]. [`Harness`] keeps only `tier` and
//! `tier_probing`; nothing else in the app reads the probe's internals.
//!
//! The rule the whole file exists for (`docs/06-billing.md`): **a probe that
//! failed never stops the app**. Every failure becomes [`Tier::Unavailable`],
//! which draws a quiet banner and blocks nothing.

use gpui::Context;

use crate::app::Harness;
use crate::session::TierBanner;
use crate::tier::{self, Tier};
use crate::wire::WireCall;

/// Whether `tier` refuses a send until "Send anyway" is pressed.
///
/// Pay-as-you-go is the one tier that blocks: every turn bills API usage, so
/// the banner stops the person being billed by surprise. Everything else —
/// a known subscription, or an unknown plan — sends normally.
pub(crate) fn tier_blocks(tier: &Tier, send_anyway: bool) -> bool {
    matches!(tier, Tier::PayAsYouGo) && !send_anyway
}

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
    /// the background path; meanwhile the banner keeps the existing
    /// probing/Checking state, never the pay-as-you-go banner. **A probe that
    /// fails never stops the app**: it becomes [`Tier::Unavailable`], which
    /// draws a quiet banner and blocks nothing.
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
                self.push_tier(cx);
                return;
            }
        }
        if self.tier_probing {
            return;
        }
        self.tier_probing = true;
        // The banner answers at once: "Check again" reads "Checking…"
        // while the probe runs.
        self.push_tier(cx);
        let program = self.args.program.clone();
        let client = self.client.clone();
        self.wire_call(
            cx,
            move || {
                // The wire read stays at the edge: `probe_primary_confirmed`
                // decides from the already-read observation, which is what
                // makes the decision reachable from a test without a live
                // connection. A provisional pay-as-you-go re-probes here, on
                // the background path, while the banner keeps Checking.
                let read = client.as_ref().and_then(|client| tier::read_usage_value(client));
                tier::probe_primary_confirmed(
                    read.as_ref(),
                    tier::auth_mtime(),
                    tier::now_secs(),
                    || tier::probe(&program),
                    || std::thread::sleep(tier::PAYG_CONFIRM_INTERVAL),
                )
            },
            move |this, result, cx| {
                this.tier_probing = false;
                // The reason is the module's own words, never the terminal's.
                let tier = result.unwrap_or_else(Tier::Unavailable);
                tier::remember(&tier);
                this.tier = Some(tier.clone());
                this.push_tier(cx);
                // An asked-for probe reports back; a boot probe that found the
                // plan simply clears its banner.
                if force {
                    let (title, detail) = match &tier {
                        Tier::Subscription { plan, .. } => ("Plan checked", format!("Plan: {plan}")),
                        Tier::PayAsYouGo => {
                            ("Plan checked", "Plan: pay-as-you-go — every turn bills API usage".to_owned())
                        }
                        Tier::Unavailable(reason) => ("Still unknown", format!("Still unknown: {reason}")),
                    };
                    this.overlays.update(cx, |overlays, _| overlays.toast(title, detail));
                }
                cx.notify();
            },
        );
    }

    /// The banner the open session should be drawing, given the tier and
    /// whether "Send anyway" has been pressed. A known subscription draws
    /// nothing — the banner leaves once the plan is known — and while a probe runs the unknown-plan banner's button reads
    /// "Checking…".
    pub(crate) fn tier_banner(&self) -> Option<TierBanner> {
        match self.tier.as_ref()? {
            Tier::Subscription { .. } => None,
            Tier::PayAsYouGo => Some(payg_banner(self.send_anyway, self.tier_probing)),
            Tier::Unavailable(_) => Some(TierBanner {
                text: "Muse did not say which plan this login is on, so Baaz cannot tell \
                       whether turns bill API usage."
                    .to_owned(),
                blocking: false,
                checking: self.tier_probing,
            }),
        }
    }

    /// Hand the current banner to whatever session is open.
    pub(crate) fn push_tier(&mut self, cx: &mut Context<Self>) {
        let banner = self.tier_banner();
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.set_tier_banner(banner, cx));
        }
        cx.notify();
    }

    /// Fold one `usage/changed` notification into the tier in place: the
    /// footer meter and the banner follow through [`Self::push_tier`].
    ///
    /// A frame that does not decode keeps the known tier — it never clears
    /// it — and so does a frame stamped before the installed credential: the
    /// wire carries no account identity, and a notification for a login that
    /// is no longer installed must not move the tier. Cached through
    /// [`tier::remember`] exactly as a probe answer is, and persisted
    /// beside it as the Muse usage snapshot, so the account menu shows
    /// the newest reading with its true age.
    pub(crate) fn apply_usage_changed(&mut self, params: &serde_json::Value, cx: &mut Context<Self>) {
        if let Some(tier) = tier::tier_from_changed_current(params, tier::auth_mtime()) {
            tier::remember(&tier);
            // The snapshot carries the notification's own arrival time,
            // and a tier with no numbers never wipes the last good one:
            // the rows keep it with "unavailable now" beside it.
            if tier::tier_has_numbers(&tier) {
                crate::provider_status::record_muse_snapshot_at(
                    Some(tier.footer_label()),
                    tier.weekly_fraction().map(f64::from),
                    tier::observed_secs_from_changed_params(params),
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
    /// ([`tier::read_usage_value`]), never the pty scrape, never a toast.
    /// A current observation replaces the tier and the stored Muse
    /// snapshot; anything else — a cold host, a stale frame, a failed
    /// read — keeps what the menu had. The caller marks Muse refreshing;
    /// this clears it and repaints either way, so the "Refreshing…" row
    /// always settles.
    pub(crate) fn refresh_muse_usage(&mut self, cx: &mut Context<Self>) {
        let client = self.client.clone();
        self.wire_call(
            cx,
            move || {
                // A panic on the background path must never stick the row
                // on "Refreshing…": the completion below still runs and
                // clears it (and the 20 s guard bounds a hang).
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let value = client.as_ref().and_then(|client| tier::read_usage_value(client));
                    let observed_at =
                        value.as_ref().and_then(tier::observed_secs_from_read_value);
                    value
                        .and_then(|value| tier::tier_from_read_current(&value, tier::auth_mtime()))
                        .map(|tier| (tier, observed_at))
                }));
                outcome.ok().flatten()
            },
            move |this, result, cx| {
                match result {
                    Some((tier, observed_at)) if tier::tier_has_numbers(&tier) => {
                        tier::remember(&tier);
                        crate::provider_status::record_muse_snapshot_at(
                            Some(tier.footer_label()),
                            tier.weekly_fraction().map(f64::from),
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
                crate::provider_status::clear_usage_refreshing(&[crate::providers::ProviderId::Muse]);
                cx.notify();
            },
        );
    }
}

/// The pay-as-you-go banner. While a re-verify is in flight the answer is
/// provisional (B7), so it reads "Checking…" and never blocks a send.
fn payg_banner(send_anyway: bool, probing: bool) -> TierBanner {
    TierBanner {
        text: "This login is on pay-as-you-go: every turn bills API usage. \
               Sign out and back in after subscribing, or send anyway."
            .to_owned(),
        blocking: tier_blocks(&Tier::PayAsYouGo, send_anyway) && !probing,
        checking: probing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subscription() -> Tier {
        Tier::Subscription {
            plan: "Muse Code High Usage".into(),
            current_pct: Some(2),
            weekly_pct: Some(2),
            resets: vec![],
            usage_unavailable: false,
        }
    }

    /// `--tier payg` still yields the blocking banner: pay-as-you-go is the
    /// one tier that refuses a send until "Send anyway", and nothing else
    /// does.
    #[test]
    fn pay_as_you_go_is_the_one_tier_that_blocks() {
        // `--tier payg` fakes exactly this tier (`main.rs`), and the banner
        // it draws blocks until "Send anyway".
        assert!(tier_blocks(&Tier::PayAsYouGo, false));
        assert!(!tier_blocks(&Tier::PayAsYouGo, true), "Send anyway lifts it for this run");
        assert!(!tier_blocks(&subscription(), false));
        assert!(!tier_blocks(&Tier::Unavailable("scripted".into()), false));
    }

    /// B7: a re-verify of a remembered pay-as-you-go runs with that tier
    /// still in place; the banner then reads "Checking…" and blocks nothing.
    #[test]
    fn a_reverifying_pay_as_you_go_banner_checks_and_never_blocks() {
        let checking = payg_banner(false, true);
        assert!(checking.checking && !checking.blocking, "provisional while probing");
        let settled = payg_banner(false, false);
        assert!(settled.blocking && !settled.checking, "a confirmed answer blocks until Send anyway");
    }
}
