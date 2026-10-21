//! Muse's account and usage vocabulary, in the seam's words.
//!
//! `usage/read` and `usage/changed` carry one payload shape
//! ([`SubscriptionUsage`]); [`report_from`] turns it into the neutral
//! [`UsageReport`] every provider's card reads. The windows keep a fixed
//! order — the current window first, the weekly block second — so a reader
//! that needs "current" and "weekly" apart (the Muse tier) takes them by
//! position. Stamps drop from milliseconds to whole seconds; every decision
//! that reads them (is this observation from the credential now installed?)
//! already compares whole seconds.

use muse_client::schema::{
    AccountLoginOutcome, AccountState, AccountStateKind, SubscriptionUsage, UsageReadResult,
};
use provider::{window_label, AccountLane, LoginOutcome, UsageReport, UsageWindow};

/// The weekly block's length: the wire sends none.
const WEEK_MINUTES: u64 = 7 * 24 * 60;

/// One wire usage observation as a neutral reading: the plan is the tier id
/// verbatim (an opaque number since muse 1.4.0, a name before), the current
/// window first, the weekly block second.
pub fn report_from(usage: &SubscriptionUsage) -> UsageReport {
    let minutes = u64::from(usage.window.window_duration_mins);
    UsageReport {
        plan: Some(usage.tier.clone()),
        windows: vec![
            UsageWindow {
                label: window_label(minutes),
                used_fraction: f64::from(usage.window.used_percent) / 100.0,
                resets_at: Some(secs(usage.window.resets_at_ms)),
                window_minutes: Some(minutes),
            },
            UsageWindow {
                label: window_label(WEEK_MINUTES),
                used_fraction: f64::from(usage.weekly.used_percent) / 100.0,
                resets_at: Some(secs(usage.weekly.resets_at_ms)),
                window_minutes: Some(WEEK_MINUTES),
            },
        ],
        observed_at: Some(secs(usage.observed_at_ms)),
    }
}

/// A `usage/read` result object, as read: `None` for the `{}` cold host
/// and for anything that does not decode.
pub fn report_from_read(value: &serde_json::Value) -> Option<UsageReport> {
    let result: UsageReadResult = serde_json::from_value(value.clone()).ok()?;
    result.usage.as_ref().map(report_from)
}

/// A `usage/changed` notification's params: `None` when they do not decode.
pub fn report_from_changed(params: &serde_json::Value) -> Option<UsageReport> {
    serde_json::from_value::<SubscriptionUsage>(params.clone()).ok().as_ref().map(report_from)
}

/// The neutral lane for a wire credential lane.
pub fn lane_of(kind: &AccountStateKind) -> AccountLane {
    match kind {
        AccountStateKind::LoggedOut => AccountLane::LoggedOut,
        AccountStateKind::EnvKey => AccountLane::EnvKey,
        AccountStateKind::ApiKey => AccountLane::ApiKey,
        AccountStateKind::AccountLogin => AccountLane::AccountLogin,
        other => AccountLane::Other(other.as_wire().unwrap_or("unknown").to_owned()),
    }
}

/// An `account/read` / `account/logout` answer or `account/changed`
/// params, as the seam's account ack.
pub fn account_ack(state: AccountState) -> provider::Ack {
    let lane = lane_of(&state.state);
    provider::Ack::Account { signed_in: lane.signed_in(), label: state.label, lane }
}

/// The neutral outcome for a wire login outcome.
pub fn outcome_of(outcome: &AccountLoginOutcome) -> LoginOutcome {
    match outcome {
        AccountLoginOutcome::Granted => LoginOutcome::Granted,
        AccountLoginOutcome::Denied => LoginOutcome::Denied,
        AccountLoginOutcome::Expired => LoginOutcome::Expired,
        AccountLoginOutcome::Cancelled => LoginOutcome::Cancelled,
        AccountLoginOutcome::Failed => LoginOutcome::Failed,
        other => LoginOutcome::Other(other.as_wire().unwrap_or("unknown").to_owned()),
    }
}

/// The connection-level event one account or usage notification makes, or
/// `None` for any other method (and for params that do not decode).
pub fn connection_event(method: &str, params: &serde_json::Value) -> Option<provider::ProviderEvent> {
    use provider::ProviderEvent;
    match method {
        "account/changed" => {
            let state: AccountState = serde_json::from_value(params.clone()).ok()?;
            Some(ProviderEvent::AccountChanged { lane: lane_of(&state.state), label: state.label })
        }
        "account/loginCompleted" => {
            let done: muse_client::schema::AccountLoginCompletedParams =
                serde_json::from_value(params.clone()).ok()?;
            Some(ProviderEvent::LoginCompleted { outcome: outcome_of(&done.outcome), message: done.message })
        }
        "usage/changed" => report_from_changed(params).map(|report| ProviderEvent::UsageChanged { report }),
        _ => None,
    }
}

fn secs(ms: u64) -> i64 {
    (ms / 1000) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reading_keeps_current_then_weekly_and_whole_seconds() {
        let params = serde_json::json!({
            "observedAtMs": 1_700_000_000_999u64,
            "tier": "max",
            "weekly": {"resetsAtMs": 1_700_100_000_000u64, "usedPercent": 140},
            "window": {"resetsAtMs": 1_700_010_000_000u64, "usedPercent": 7, "windowDurationMins": 300},
        });
        let report = report_from_changed(&params).expect("decodes");
        assert_eq!(report.plan.as_deref(), Some("max"));
        assert_eq!(report.observed_at, Some(1_700_000_000));
        assert_eq!(report.windows[0].window_minutes, Some(300));
        assert!((report.windows[0].used_fraction - 0.07).abs() < 1e-9);
        assert_eq!(report.windows[1].label, "Weekly");
        assert!((report.windows[1].used_fraction - 1.4).abs() < 1e-9, "over-quota survives");
        assert_eq!(report_from_read(&serde_json::json!({})), None, "the cold host observed nothing");
    }

    #[test]
    fn account_notifications_become_connection_events() {
        let changed = connection_event("account/changed", &serde_json::json!({
            "credentialRequired": true, "state": "envKey", "label": "env"
        }));
        assert_eq!(
            changed,
            Some(provider::ProviderEvent::AccountChanged { lane: AccountLane::EnvKey, label: Some("env".into()) })
        );
        let done = connection_event("account/loginCompleted", &serde_json::json!({"outcome": "denied", "message": "no"}));
        assert_eq!(
            done,
            Some(provider::ProviderEvent::LoginCompleted { outcome: LoginOutcome::Denied, message: Some("no".into()) })
        );
        assert_eq!(connection_event("turn/started", &serde_json::json!({})), None);
    }
}
