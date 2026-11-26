//! The Codex account snapshot: the latest `account/rateLimits/*` reading,
//! kept beside the fold (moved out of `fold.rs` in docs/82 5.3 — it is the
//! money guard's feed, not transcript), the same split Claude Code's
//! `account.rs` makes.

use serde_json::Value;

/// The money guard's feed: the latest `account/rateLimits/updated` push, kept
/// beside the seam's account shape. The window lengths ride along with the
/// percents so the usage card can label each window from its length.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountSnapshot {
    /// Primary window usage percent, when reported.
    pub used_percent: Option<u64>,
    /// Primary window length in minutes, when reported.
    pub primary_minutes: Option<u64>,
    /// Plan name (`prolite`, …), when reported.
    pub plan: Option<String>,
    /// Unix time the window resets, when reported.
    pub resets_at: Option<u64>,
    /// Secondary window usage percent, when reported.
    pub secondary_used_percent: Option<u64>,
    /// Secondary window length in minutes, when reported.
    pub secondary_minutes: Option<u64>,
    /// Unix time the secondary window resets, when reported.
    pub secondary_resets_at: Option<u64>,
    /// Unix time the latest push or read arrived on the wire. Stamped when
    /// the fold observes it, so the store persists the reading with its
    /// true age instead of the next turn's time.
    pub observed_at: Option<i64>,
}

/// Unix seconds now. Best-effort: zero when the clock is unavailable.
fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

impl AccountSnapshot {
    /// Record a push. Unknown fields are ignored; a push that names no
    /// window at all still counts as "a reading was seen". Accepts either
    /// the push params (`{rateLimits: …}`) or the bare limits object the
    /// `account/rateLimits/read` answer carries — same object, either
    /// wrapping. Stamps when the reading arrived on the wire.
    pub fn observe(&mut self, params: &Value) {
        let limits = params.get("rateLimits").unwrap_or(params);
        let primary = limits.get("primary");
        if let Some(used) =
            primary.and_then(|primary| primary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.used_percent = Some(used);
        }
        if let Some(minutes) = primary
            .and_then(|primary| primary.get("windowDurationMins"))
            .and_then(Value::as_u64)
        {
            self.primary_minutes = Some(minutes);
        }
        if let Some(plan) = limits.get("planType").and_then(Value::as_str) {
            self.plan = Some(plan.to_owned());
        }
        if let Some(resets) = primary
            .and_then(|primary| primary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.resets_at = Some(resets);
        }
        let secondary = limits.get("secondary");
        if let Some(used) =
            secondary.and_then(|secondary| secondary.get("usedPercent")).and_then(Value::as_u64)
        {
            self.secondary_used_percent = Some(used);
        }
        if let Some(minutes) = secondary
            .and_then(|secondary| secondary.get("windowDurationMins"))
            .and_then(Value::as_u64)
        {
            self.secondary_minutes = Some(minutes);
        }
        if let Some(resets) = secondary
            .and_then(|secondary| secondary.get("resetsAt"))
            .and_then(Value::as_u64)
        {
            self.secondary_resets_at = Some(resets);
        }
        self.observed_at = Some(now_secs());
    }

    /// Pin the wire-arrival stamp (tests drive the clock through this).
    #[cfg(test)]
    pub fn set_observed_at(&mut self, at: i64) {
        self.observed_at = Some(at);
    }

    /// The structured usage reading for the seam: the plan plus one window
    /// per reported percent, labelled from each window's length. `None`
    /// until a push or a rate-limits read has been seen.
    pub fn usage_report(&self) -> Option<provider::UsageReport> {
        if !self.has_reading() {
            return None;
        }
        let mut windows = Vec::new();
        if let Some(used) = self.used_percent {
            windows.push(provider::UsageWindow {
                label: self
                    .primary_minutes
                    .map(provider::window_label)
                    .unwrap_or_else(|| "Primary".into()),
                used_fraction: (used as f64 / 100.0).clamp(0.0, 1.0),
                resets_at: self.resets_at.map(|resets| resets as i64),
                window_minutes: self.primary_minutes,
            });
        }
        if let Some(used) = self.secondary_used_percent {
            windows.push(provider::UsageWindow {
                label: self
                    .secondary_minutes
                    .map(provider::window_label)
                    .unwrap_or_else(|| "Secondary".into()),
                used_fraction: (used as f64 / 100.0).clamp(0.0, 1.0),
                resets_at: self.secondary_resets_at.map(|resets| resets as i64),
                window_minutes: self.secondary_minutes,
            });
        }
        Some(provider::UsageReport { unofficial: false,
            plan: self.plan.clone(),
            windows,
            observed_at: self.observed_at,
        })
    }

    /// Whether any push has been seen at all.
    pub fn has_reading(&self) -> bool {
        self.used_percent.is_some() || self.plan.is_some() || self.resets_at.is_some()
    }

    /// The display label for [`provider::Ack::Account`]. `None` until the
    /// first push arrives.
    pub fn label(&self) -> Option<String> {
        if !self.has_reading() {
            return None;
        }
        let plan = self.plan.as_deref().unwrap_or("codex");
        let mut label = match self.used_percent {
            Some(used) => format!("Codex {plan} {used}%"),
            None => format!("Codex {plan}"),
        };
        if let Some(resets) = self.resets_at {
            label.push_str(&format!(" (resets {resets})"));
        }
        Some(label)
    }
}
