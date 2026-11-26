//! The account meter: `rate_limit_event` (doc §5) into the seam's shape,
//! and — before the first turn sends one — the `get_usage` control reply
//! (docs/85 16.1; unofficial, probed live 26 Nov 2026 on 2.1.276).
//!
//! The seam's account shape is [`provider::Ack::Account`]: `signed_in` plus
//! a display label, never key material. This module keeps the latest meter
//! reading beside that shape so `isUsingOverage` — the Claude Code analogue
//! of a pay-as-you-go login — stays reachable after the fold: a person is
//! entitled to know before they send.

use serde_json::Value;

/// One usage window inside a `rate_limit_event`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageWindow {
    /// Fraction used, 0.0–1.0. `None` when the event omitted
    /// `utilization`: unknown is unknown, never a 0 % bar.
    pub utilization: Option<f64>,
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
    /// Utilization of the named window. `None` when the event omitted
    /// it: a missing number is not a zero reading.
    pub utilization: Option<f64>,
    /// Resets-at of the named window.
    pub resets_at: Option<i64>,
    /// **True means turns are billing beyond the plan.** Preserved and
    /// reachable via [`AccountSnapshot::is_using_overage`].
    pub is_using_overage: bool,
    /// The five-hour window, when reported.
    pub five_hour: Option<UsageWindow>,
    /// The seven-day window, when reported.
    pub seven_day: Option<UsageWindow>,
    /// The plan (`get_usage`'s `subscription_type`, e.g. `max`). A
    /// `rate_limit_event` names none.
    pub plan: Option<String>,
}

impl RateLimitInfo {
    /// Decode the wire `rate_limit_info` object. Unknown fields are ignored;
    /// an absent `utilization` decodes to `None` (unknown) rather than
    /// zero: a missing number must never render as a 0 % bar.
    pub fn decode(info: &Value) -> Self {
        let window = |key: &str| {
            info.get("unifiedWindows").and_then(|windows| windows.get(key)).map(|window| {
                UsageWindow {
                    utilization: window.get("utilization").and_then(Value::as_f64),
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
            utilization: info.get("utilization").and_then(Value::as_f64),
            resets_at: info.get("resetsAt").and_then(Value::as_i64),
            is_using_overage: info
                .get("isUsingOverage")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            five_hour: window("five_hour"),
            seven_day: window("seven_day"),
            plan: None,
        }
    }

    /// Decode a `get_usage` control reply (`response.response`, probed
    /// live 26 Nov 2026, claude 2.1.276): `rate_limits.five_hour` and
    /// `.seven_day` carry `utilization` in **percent** (0–100, unlike the
    /// event's fraction) and `resets_at` as an ISO time; the plan is
    /// `subscription_type`. `None` for any other reply, or when the CLI
    /// says no rate limits are available (an API-key account). The CLI
    /// marks the reply experimental, so anything missing is unknown.
    pub fn from_get_usage(inner: &Value) -> Option<Self> {
        let limits = inner.get("rate_limits")?.as_object()?;
        if inner.get("rate_limits_available").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let window = |key: &str| {
            let window = limits.get(key)?.as_object()?;
            Some(UsageWindow {
                utilization: window.get("utilization").and_then(Value::as_f64).map(|percent| percent / 100.0),
                resets_at: window.get("resets_at").and_then(Value::as_str).and_then(parse_iso_utc),
            })
        };
        let (five_hour, seven_day) = (window("five_hour"), window("seven_day"));
        if five_hour.is_none() && seven_day.is_none() {
            return None;
        }
        Some(Self {
            status: String::new(),
            rate_limit_type: String::new(),
            utilization: None,
            resets_at: None,
            is_using_overage: false,
            five_hour,
            seven_day,
            plan: inner.get("subscription_type").and_then(Value::as_str).filter(|plan| !plan.is_empty()).map(capitalised),
        })
    }
}

/// `max` → `Max`: the plan as the card names it.
fn capitalised(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Unix seconds of an ISO-8601 time with an offset (`2026-11-26T09:59:59.94+00:00`,
/// or `Z`), as `get_usage` writes `resets_at`. `None` for anything else.
fn parse_iso_utc(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut ymd = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let split = rest.find(|c| c == 'Z' || c == '+' || c == '-')?;
    let (clock, offset) = rest.split_at(split);
    let mut hms = clock.split(':');
    let hour: i64 = hms.next()?.parse().ok()?;
    let minute: i64 = hms.next()?.parse().ok()?;
    let second: i64 = hms.next()?.split('.').next()?.parse().ok()?;
    let offset_secs = match offset {
        "Z" => 0,
        _ => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let (h, m) = offset[1..].split_once(':')?;
            sign * (h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
        }
    };
    // Days from the civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset_secs)
}

/// The meter's status text for a person: `allowed_warning` reads
/// "Allowed warning", `allowed` reads "Allowed". An empty status reads
/// "Unknown" rather than a blank row.
pub fn humanize_status(status: &str) -> String {
    if status.trim().is_empty() {
        return "Unknown".into();
    }
    let spaced = status.replace('_', " ");
    let mut chars = spaced.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => "Unknown".into(),
    }
}

/// A `rateLimitType` in minutes, when the name is a known window length:
/// the unified windows ride under these names, so the card can label the
/// named window from its length instead of echoing the wire spelling.
/// Unix seconds now. Best-effort: zero when the clock is unavailable.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

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
    /// Unix time the latest reading arrived on the wire. Stamped when the
    /// fold observes the event, so the store persists the reading with its
    /// true age instead of the next turn's time.
    pub observed_at: Option<i64>,
}

impl AccountSnapshot {
    /// Record a reading, stamping when it arrived on the wire.
    pub fn observe(&mut self, info: RateLimitInfo) {
        self.latest = Some(info);
        self.observed_at = Some(now_secs());
    }

    /// Record a `get_usage` reading only while no `rate_limit_event` has
    /// arrived: the event comes with every turn and carries the overage
    /// flag, so it always wins; the polled reply fills the meter before
    /// the first turn (docs/85 16.1).
    pub fn observe_polled(&mut self, info: RateLimitInfo) {
        if self.latest.as_ref().map_or(true, |latest| latest.rate_limit_type.is_empty()) {
            self.observe(info);
        }
    }

    /// Pin the wire-arrival stamp (tests drive the clock through this).
    #[cfg(test)]
    pub fn set_observed_at(&mut self, at: i64) {
        self.observed_at = Some(at);
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
    /// twice. Windows whose utilization is unknown are skipped, never
    /// drawn as a 0 % bar. When a reading was seen but no window reports
    /// a usable number, the report carries no windows and the meter's
    /// status text as the plan slot, so the row reads e.g. "Allowed"
    /// instead of a fake bar. `None` until the first reading arrives.
    pub fn usage_report(&self) -> Option<provider::UsageReport> {
        let info = self.latest.as_ref()?;
        let mut windows = Vec::new();
        let named_minutes = window_minutes(&info.rate_limit_type);
        // The named window only covers its unified twin when it actually
        // carded: a live `allowed` event names `five_hour` but carries no
        // top-level utilization, and skipping the unified five-hour window
        // then would drop the 5h reading altogether.
        let mut covered = None;
        if let Some(used) = info.utilization {
            covered = named_minutes;
            windows.push(provider::UsageWindow {
                label: named_minutes
                    .map(provider::window_label)
                    .unwrap_or_else(|| info.rate_limit_type.clone()),
                used_fraction: used.clamp(0.0, 1.0),
                resets_at: info.resets_at,
                window_minutes: named_minutes,
            });
        }
        for (window, minutes) in [(&info.five_hour, 300u64), (&info.seven_day, 10080u64)] {
            if Some(minutes) == covered {
                continue;
            }
            let Some(window) = window else { continue };
            let Some(used) = window.utilization else { continue };
            windows.push(provider::UsageWindow {
                label: provider::window_label(minutes),
                used_fraction: used.clamp(0.0, 1.0),
                resets_at: window.resets_at,
                window_minutes: Some(minutes),
            });
        }
        if windows.is_empty() {
            return Some(provider::UsageReport {
                plan: Some(humanize_status(&info.status)),
                windows: Vec::new(),
                observed_at: self.observed_at,
            });
        }
        Some(provider::UsageReport { plan: info.plan.clone(), windows, observed_at: self.observed_at })
    }

    /// The display label for [`provider::Ack::Account`]: window, percentage,
    /// and reset time, with an overage banner when billing beyond the plan.
    /// When the reading carries no usable utilization the label reads the
    /// meter's status text (e.g. "Allowed") instead of a fabricated 0 %.
    /// `None` until the first reading arrives.
    pub fn label(&self) -> Option<String> {
        let info = self.latest.as_ref()?;
        // A polled `get_usage` reading names no window of its own: the
        // first window with a number stands for it.
        let polled = info.utilization.is_none().then(|| {
            [("five_hour", &info.five_hour), ("seven_day", &info.seven_day)]
                .into_iter()
                .find_map(|(name, window)| window.as_ref().and_then(|w| w.utilization.map(|used| (name, used, w.resets_at))))
        }).flatten();
        let named = info.utilization.map(|used| (info.rate_limit_type.as_str(), used, info.resets_at));
        let mut label = match named.or(polled) {
            Some((window, used, resets_at)) => {
                let percent = (used * 100.0).round() as u64;
                match resets_at {
                    Some(resets) => format!("Claude Code {window} {percent}% (resets {resets})"),
                    None => format!("Claude Code {window} {percent}%"),
                }
            }
            None => format!("Claude Code {} {}", info.rate_limit_type, humanize_status(&info.status)),
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
        let used = info.utilization.expect("partial.jsonl reports a number");
        assert!((used - 0.79).abs() < f64::EPSILON);
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
    fn a_named_window_without_a_top_level_number_still_cards_both_windows() {
        // The shape the live CLI sends on an `allowed` reading (captured
        // 2026-09-29): the named window is `five_hour`, there is no
        // top-level `utilization`, and both numbers ride `unifiedWindows`.
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1790647800,"rateLimitType":"five_hour","overageStatus":"rejected","overageDisabledReason":"org_level_disabled","isUsingOverage":false,"unifiedWindows":{"five_hour":{"utilization":0.12,"resetsAt":1790647800},"seven_day":{"utilization":0.41,"resetsAt":1790989200}}}}"#;
        let Frame::RateLimit(info) = decode_line(line).expect("decodes") else {
            panic!("expected a rate-limit frame");
        };
        let mut snapshot = AccountSnapshot::default();
        snapshot.observe(info);
        let report = snapshot.usage_report().expect("a reading was seen");
        let minutes: Vec<_> = report.windows.iter().map(|window| window.window_minutes).collect();
        assert_eq!(minutes, [Some(300), Some(10080)], "both the 5h and the weekly window card");
        assert!((report.windows[0].used_fraction - 0.12).abs() < 1e-9);
        assert!((report.windows[1].used_fraction - 0.41).abs() < 1e-9);
    }

    #[test]
    fn missing_utilization_is_unknown_and_draws_no_bar() {
        // A `rate_limit_event` without any `utilization` decodes to
        // unknown, not 0.0: the report carries no windows (no fake bar)
        // and the status text instead, and the label reads the status
        // rather than "0%".
        let line = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"seven_day","isUsingOverage":false,"unifiedWindows":{"five_hour":{"resetsAt":1790187000},"seven_day":{"resetsAt":1790384400}}}}"#;
        let Frame::RateLimit(info) = decode_line(line).expect("decodes") else {
            panic!("expected a rate-limit frame");
        };
        assert_eq!(info.utilization, None, "a missing number is unknown, never zero");
        assert_eq!(info.five_hour.as_ref().and_then(|window| window.utilization), None);
        let mut snapshot = AccountSnapshot::default();
        snapshot.observe(info);
        let report = snapshot.usage_report().expect("a reading was seen");
        assert!(report.windows.is_empty(), "unknown windows draw no bar");
        assert_eq!(report.plan.as_deref(), Some("Allowed"), "the row reads the status text instead");
        let label = snapshot.label().expect("label");
        assert!(label.contains("Allowed"), "the label reads the status, not 0%: {label}");
        assert!(!label.contains("0%"), "no fabricated percentage: {label}");
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

    /// docs/85 16.1: the live `get_usage` reply (claude 2.1.276, 26 Nov
    /// 2026) decodes through the control-response frame into the meter:
    /// percent becomes a fraction, the ISO reset a Unix time, the plan
    /// reads "Max"; a later `rate_limit_event` replaces it, and a polled
    /// reply never replaces an event.
    #[test]
    fn a_live_get_usage_reply_fills_the_meter_until_a_turn_reports() {
        const REPLY: &str = include_str!("../../../fixtures/claude-code/get-usage.json");
        let Frame::ControlResponse { usage: Some(info), .. } = decode_line(REPLY.trim()).expect("decodes") else {
            panic!("a get_usage reply carries the limits");
        };
        assert_eq!(info.plan.as_deref(), Some("Max"));
        let five = info.five_hour.clone().expect("five hour");
        assert_eq!(five.utilization, Some(0.05));
        assert_eq!(five.resets_at, Some(1_795_687_199), "2026-11-26T09:59:59.945433+00:00");
        assert_eq!(info.seven_day.as_ref().and_then(|w| w.utilization), Some(0.67));

        let mut snapshot = AccountSnapshot::default();
        snapshot.observe_polled(info.clone());
        let report = snapshot.usage_report().expect("report");
        assert_eq!(report.plan.as_deref(), Some("Max"));
        assert_eq!(report.windows.iter().map(|w| (w.label.as_str(), w.used_fraction)).collect::<Vec<_>>(), vec![("Session · 5h", 0.05), ("Weekly", 0.67)]);
        assert!(snapshot.label().expect("label").starts_with("Claude Code five_hour 5%"));

        let event = r#"{"type":"rate_limit_event","rate_limit_info":{"status":"allowed","resetsAt":1790647800,"rateLimitType":"five_hour","isUsingOverage":true,"unifiedWindows":{"five_hour":{"utilization":0.12,"resetsAt":1790647800}}}}"#;
        let Frame::RateLimit(event) = decode_line(event).expect("decodes") else { panic!("event") };
        snapshot.observe(event);
        snapshot.observe_polled(info);
        assert!(snapshot.is_using_overage(), "the turn's event stays: a polled reply never replaces it");

        for other in [r#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{}}}"#,
                      r#"{"type":"control_response","response":{"subtype":"success","request_id":"x","response":{"rate_limits_available":false,"rate_limits":{"five_hour":{"utilization":1}}}}}"#] {
            assert!(matches!(decode_line(other).expect("decodes"), Frame::ControlResponse { usage: None, .. }), "{other}");
        }
    }
}
