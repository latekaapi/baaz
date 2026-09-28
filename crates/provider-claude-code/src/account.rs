//! The account meter: `rate_limit_event` (doc §5) into the seam's shape.
//!
//! The seam's account shape is [`provider::Ack::Account`]: `signed_in` plus
//! a display label, never key material. This module keeps the latest meter
//! reading beside that shape so `isUsingOverage` — the Claude Code analogue
//! of the pay-as-you-go banner — stays reachable after the fold: a person is
//! entitled to know before they send.

use serde_json::Value;

/// One usage window inside a `rate_limit_event`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageWindow {
    /// Fraction used, 0.0–1.0.
    pub utilization: f64,
    /// Unix time the window resets.
    pub resets_at: Option<i64>,
}

/// A decoded `rate_limit_event.rate_limit_info` (doc §5, verbatim shape).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimitInfo {
    /// e.g. `allowed_warning`.
    pub status: String,
    /// e.g. `seven_day`.
    pub rate_limit_type: String,
    /// Utilization of the named window.
    pub utilization: f64,
    /// Resets-at of the named window.
    pub resets_at: Option<i64>,
    /// **True means turns are billing beyond the plan.** Preserved and
    /// reachable via [`AccountSnapshot::is_using_overage`].
    pub is_using_overage: bool,
    /// The five-hour window, when reported.
    pub five_hour: Option<UsageWindow>,
    /// The seven-day window, when reported.
    pub seven_day: Option<UsageWindow>,
}

impl RateLimitInfo {
    /// Decode the wire `rate_limit_info` object. Unknown fields are ignored;
    /// absent numbers default to zero rather than failing the fold.
    pub fn decode(info: &Value) -> Self {
        let window = |key: &str| {
            info.get("unifiedWindows").and_then(|windows| windows.get(key)).map(|window| {
                UsageWindow {
                    utilization: window
                        .get("utilization")
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0),
                    resets_at: window.get("resetsAt").and_then(Value::as_i64),
                }
            })
        };
        Self {
            status: info.get("status").and_then(Value::as_str).unwrap_or_default().to_owned(),
            rate_limit_type: info
                .get("rateLimitType")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            utilization: info.get("utilization").and_then(Value::as_f64).unwrap_or(0.0),
            resets_at: info.get("resetsAt").and_then(Value::as_i64),
            is_using_overage: info
                .get("isUsingOverage")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
        }
    }
}

/// A `rateLimitType` in minutes, when the name is a known window length:
/// the unified windows ride under these names, so the card can label the
/// named window from its length instead of echoing the wire spelling.
fn window_minutes(rate_limit_type: &str) -> Option<u64> {
    match rate_limit_type {
        "five_hour" => Some(300),
        "seven_day" => Some(10080),
        _ => None,
    }
}

/// The latest meter reading, kept by the fold.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSnapshot {
    /// The latest reading, when any `rate_limit_event` has been seen.
    pub latest: Option<RateLimitInfo>,
}

impl AccountSnapshot {
    /// Record a reading.
    pub fn observe(&mut self, info: RateLimitInfo) {
        self.latest = Some(info);
    }

    /// Whether the latest reading says turns are billing beyond the plan.
    /// False until a reading says otherwise — absence of a meter is not
    /// evidence of overage.
    pub fn is_using_overage(&self) -> bool {
        self.latest.as_ref().map(|info| info.is_using_overage).unwrap_or(false)
    }

    /// The structured usage reading for the seam: one window per reported
    /// utilization, labelled from each window's length. The named window
    /// first, then the unified five-hour and seven-day windows except the
    /// one the named window already covers — the same window never cards
    /// twice. `None` until the first reading arrives.
    pub fn usage_report(&self) -> Option<provider::UsageReport> {
        let info = self.latest.as_ref()?;
        let mut windows = Vec::new();
        let named_minutes = window_minutes(&info.rate_limit_type);
        windows.push(provider::UsageWindow {
            label: named_minutes
                .map(provider::window_label)
                .unwrap_or_else(|| info.rate_limit_type.clone()),
            used_fraction: info.utilization.clamp(0.0, 1.0),
            resets_at: info.resets_at,
            window_minutes: named_minutes,
        });
        for (window, minutes) in [(&info.five_hour, 300u64), (&info.seven_day, 10080u64)] {
            if Some(minutes) == named_minutes {
                continue;
            }
            let Some(window) = window else { continue };
            windows.push(provider::UsageWindow {
                label: provider::window_label(minutes),
                used_fraction: window.utilization.clamp(0.0, 1.0),
                resets_at: window.resets_at,
                window_minutes: Some(minutes),
            });
        }
        Some(provider::UsageReport { plan: None, windows })
    }

    /// The display label for [`provider::Ack::Account`]: window, percentage,
    /// and reset time, with an overage banner when billing beyond the plan.
    /// `None` until the first reading arrives.
    pub fn label(&self) -> Option<String> {
        let info = self.latest.as_ref()?;
        let percent = (info.utilization * 100.0).round() as u64;
        let mut label = match info.resets_at {
            Some(resets) => {
                format!("Claude Code {} {percent}% (resets {resets})", info.rate_limit_type)
            }
            None => format!("Claude Code {} {percent}%", info.rate_limit_type),
        };
        if info.is_using_overage {
            label.push_str(" · using overage");
        }
        Some(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::{decode_line, Frame};

    fn fixture(name: &str) -> Vec<String> {
        let path = format!("{}/../../fixtures/claude-code/{name}", env!("CARGO_MANIFEST_DIR"));
        std::fs::read_to_string(path).expect("fixture reads").lines().map(str::to_owned).collect()
    }

    #[test]
    fn fixtures_decode_a_meter_reading() {
        let mut snapshot = AccountSnapshot::default();
        for line in fixture("partial.jsonl") {
            if let Frame::RateLimit(info) = decode_line(&line).expect("decodes") {
                snapshot.observe(info);
            }
        }
        let info = snapshot.latest.as_ref().expect("partial.jsonl carries a reading");
        assert_eq!(info.rate_limit_type, "seven_day");
        assert!((info.utilization - 0.79).abs() < f64::EPSILON);
        assert!(!info.is_using_overage);
        assert!(snapshot.label().expect("label").contains("79%"));
    }

    #[test]
    fn usage_report_labels_windows_from_their_lengths() {
        // The card's shape, not a label: the named seven-day window reads
        // Weekly, the unified five-hour window reads Session · 5h, and
        // the unified seven-day twin is skipped — the same window never
        // cards twice.
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1790384400,"rateLimitType":"seven_day","utilization":0.97,"isUsingOverage":true,"surpassedThreshold":0.75,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1790187000},"seven_day":{"utilization":0.97,"resetsAt":1790384400}}}}"#;
        let Frame::RateLimit(info) = decode_line(line).expect("decodes") else {
            panic!("expected a rate-limit frame");
        };
        let mut snapshot = AccountSnapshot::default();
        assert!(snapshot.usage_report().is_none(), "no reading, no report");
        snapshot.observe(info);
        let report = snapshot.usage_report().expect("a reading was seen");
        assert_eq!(report.plan, None);
        assert_eq!(report.windows.len(), 2);
        assert_eq!(report.windows[0].label, "Weekly");
        assert!((report.windows[0].used_fraction - 0.97).abs() < f64::EPSILON);
        assert_eq!(report.windows[0].resets_at, Some(1790384400));
        assert_eq!(report.windows[1].label, "Session · 5h");
        assert!((report.windows[1].used_fraction - 0.9).abs() < f64::EPSILON);
        assert_eq!(report.windows[1].resets_at, Some(1790187000));
    }

    #[test]
    fn overage_true_is_preserved_and_bannered() {
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed_warning","resetsAt":1790384400,"rateLimitType":"seven_day","utilization":0.97,"isUsingOverage":true,"surpassedThreshold":0.75,"unifiedWindows":{"five_hour":{"utilization":0.9,"resetsAt":1790187000},"seven_day":{"utilization":0.97,"resetsAt":1790384400}}}}"#;
        let Frame::RateLimit(info) = decode_line(line).expect("decodes") else {
            panic!("expected a rate-limit frame");
        };
        let mut snapshot = AccountSnapshot::default();
        assert!(!snapshot.is_using_overage(), "absence is not overage");
        snapshot.observe(info);
        assert!(snapshot.is_using_overage());
        assert!(snapshot.label().expect("label").contains("overage"));
    }
}
