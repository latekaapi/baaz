//! Neutral usage readings: one card per provider in the account menu.
//!
//! The account menu shows a `usage_card` per Connected provider (Muse,
//! Claude Code, Codex). The numbers behind those cards travel here — a
//! plan plus labelled windows with used fractions and reset times —
//! never as display label strings: labels are a render concern and two
//! providers never share one. Nothing here names a provider's wire: a
//! window length in minutes maps to its card label in one place
//! ([`window_label`]), so every lane names the same window the same way.

/// One usage window on a provider's card.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageWindow {
    /// The card label, from the window's length ([`window_label`]).
    pub label: String,
    /// Fraction used, 0.0–1.0.
    pub used_fraction: f64,
    /// Unix time the window resets, when the provider reported one.
    pub resets_at: Option<i64>,
    /// The window's length in minutes, when known. Kept so a later
    /// reader can re-derive the label rather than parsing it back out.
    pub window_minutes: Option<u64>,
}

/// A provider's usage reading: the plan plus its windows. No windows
/// means nothing reported one yet — the card reads "Not reported yet" —
/// never a guess.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UsageReport {
    /// The plan, when known (`"Pro"`, `"Business"`, `"prolite"`, …).
    pub plan: Option<String>,
    /// The windows, possibly empty.
    pub windows: Vec<UsageWindow>,
}

/// Name a window length in minutes for its card: a five-hour window is
/// the session window, a week is Weekly, a month is Monthly, and anything
/// else reads as its own duration (`7d`, `5h`, `45m`).
pub fn window_label(window_minutes: u64) -> String {
    match window_minutes {
        300 => "Session · 5h".into(),
        10080 => "Weekly".into(),
        43200 => "Monthly".into(),
        minutes if minutes % 1440 == 0 => format!("{}d", minutes / 1440),
        minutes if minutes % 60 == 0 => format!("{}h", minutes / 60),
        minutes => format!("{minutes}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_lengths_read_as_session_weekly_monthly() {
        assert_eq!(window_label(300), "Session · 5h");
        assert_eq!(window_label(10080), "Weekly");
        assert_eq!(window_label(43200), "Monthly");
    }

    #[test]
    fn unknown_lengths_read_as_their_own_duration() {
        assert_eq!(window_label(1440), "1d");
        assert_eq!(window_label(60), "1h");
        assert_eq!(window_label(45), "45m");
    }
}
