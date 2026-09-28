//! The account menu's Usage section: one row per enabled provider.
//!
//! The rows render from the provider status service — the single store
//! probes, lane peeks and the tier feed — never from label strings. Every
//! enabled provider gets a row in registry order, whether or not it is
//! connected: a provider that cannot be probed or is merely unverified
//! reads its reason instead of vanishing silently. Disabled providers are
//! omitted. Muse's row additionally reads the app's live muse
//! connection and tier when signed in, regardless of the `muse` binary
//! lookup.

use aui::screens::{UsageRowData, UsageRowState, UsageWindow};
use aui_icons::Provider as AuiProvider;

use crate::providers::ProviderId;
use crate::provider_status::{Headline, ProviderStatus};

/// The app's live muse connection and tier for the Muse row: present
/// exactly when the app is signed in to Muse. The row then reports the
/// tier's weekly fraction even when no `muse` binary was found.
#[derive(Clone, Debug, PartialEq)]
pub struct MuseFeed {
    /// The tier's plan label (`High Usage`, …), when known.
    pub plan: Option<String>,
    /// The tier's weekly fraction, when the probe reported one.
    pub weekly_fraction: Option<f64>,
}

/// One usage row per enabled provider, in registry order. Disabled
/// providers are omitted; a missing entry is omitted (nothing known to
/// row). State per provider:
/// * windows known → `Windows` with `resets in …` per window and the
///   `as of …` footnote when the reading is dated;
/// * connected but no usable reading → `Unavailable` with a
///   provider-specific reason;
/// * not connected → `Unavailable` with the headline plus its advisory.
pub fn usage_rows(
    statuses: &[ProviderStatus],
    muse: Option<MuseFeed>,
    now: i64,
) -> Vec<UsageRowData> {
    let mut rows = Vec::new();
    for id in ProviderId::all() {
        let Some(status) = statuses.iter().find(|status| status.provider == id) else {
            continue;
        };
        if !status.enabled {
            continue;
        }
        if id == ProviderId::Muse {
            if let Some(feed) = muse.as_ref() {
                rows.push(muse_row(status, feed, now));
                continue;
            }
        }
        if status.headline() != Headline::Connected {
            rows.push(unavailable_row(status));
            continue;
        }
        rows.push(connected_row(status, now));
    }
    rows
}

/// The Muse row while the app is signed in: the tier's weekly fraction
/// first (the binary lookup never gates it), then whatever snapshot the
/// refreshes recorded, then the no-reading reason.
fn muse_row(status: &ProviderStatus, feed: &MuseFeed, now: i64) -> UsageRowData {
    let snapshot = status.usage.as_ref();
    if let Some(snapshot) = snapshot {
        if !snapshot.windows.is_empty() {
            return connected_row(status, now);
        }
    }
    if let Some(used) = feed.weekly_fraction {
        let as_of = snapshot.map(|snapshot| age_footnote(snapshot.as_of, now));
        let mut row = UsageRowData::new(
            AuiProvider::Muse,
            UsageRowState::Windows(
                vec![UsageWindow::new("Weekly", used as f32, resets_in_text(None, now))],
                as_of,
            ),
        );
        let plan = feed.plan.clone().or_else(|| snapshot.and_then(|snapshot| snapshot.plan.clone()));
        if let Some(plan) = plan {
            row = row.plan(plan);
        }
        return row;
    }
    let mut row = UsageRowData::new(
        AuiProvider::Muse,
        UsageRowState::Unavailable("No reading yet — appears after your first Muse turn".into()),
    );
    let plan = feed.plan.clone().or_else(|| snapshot.and_then(|snapshot| snapshot.plan.clone()));
    if let Some(plan) = plan {
        row = row.plan(plan);
    }
    row
}

/// A connected provider's row: live windows as bars, an all-expired
/// reading as its reset note (never a stale percentage), and an empty
/// reading as the provider's no-reading reason. A Claude Code reading
/// whose windows all lacked a usable number carries the meter's status
/// text where a plan would sit — the row reads it instead of a bar.
fn connected_row(status: &ProviderStatus, now: i64) -> UsageRowData {
    let provider = status.provider.icon();
    let snapshot = status.usage.as_ref();
    let windows = snapshot.map(|snapshot| snapshot.windows.as_slice()).unwrap_or(&[]);
    if windows.is_empty() {
        if status.provider == ProviderId::ClaudeCode {
            if let Some(text) = snapshot.and_then(|snapshot| snapshot.plan.clone()) {
                return UsageRowData::new(provider, UsageRowState::Unavailable(text.into()));
            }
        }
        return UsageRowData::new(provider, UsageRowState::Unavailable(no_reading_reason(status.provider).into()));
    }
    let live: Vec<_> = windows.iter().filter(|window| window.resets_at.is_none_or(|at| at > now)).collect();
    if live.is_empty() {
        let label = windows.first().map(|window| window.label.as_str()).unwrap_or("Usage");
        return UsageRowData::new(
            provider,
            UsageRowState::Unavailable(format!("{label} reset — no new reading yet").into()),
        );
    }
    let as_of = snapshot.map(|snapshot| age_footnote(snapshot.as_of, now));
    let bars: Vec<UsageWindow> = live
        .iter()
        .map(|window| {
            UsageWindow::new(
                window.label.clone(),
                window.used_fraction as f32,
                resets_in_text(window.resets_at, now),
            )
        })
        .collect();
    let mut row = UsageRowData::new(provider, UsageRowState::Windows(bars, as_of));
    if let Some(plan) = snapshot.and_then(|snapshot| snapshot.plan.clone()) {
        row = row.plan(plan);
    }
    row
}

/// A not-connected provider's row: the headline plus whatever the probe
/// already said beyond it, so the row names the fix instead of hiding.
fn unavailable_row(status: &ProviderStatus) -> UsageRowData {
    UsageRowData::new(status.provider.icon(), UsageRowState::Unavailable(not_connected_reason(status).into()))
}

/// Why a not-connected provider has no reading: the headline, plus the
/// advisory's own words when it carries any.
pub fn not_connected_reason(status: &ProviderStatus) -> String {
    match status.headline() {
        Headline::Checking => "Checking…".into(),
        Headline::Disabled => "Disabled".into(),
        Headline::NotInstalled => "Not installed".into(),
        Headline::CantRun => match &status.advisory {
            crate::provider_status::Advisory::CantRun { stderr, .. } if !stderr.trim().is_empty() => {
                format!("{} — {}", status.headline_text(), stderr.trim())
            }
            _ => status.headline_text(),
        },
        Headline::SignedOut => "Signed out".into(),
        Headline::Unverified => "Installed · sign-in not verified".into(),
        Headline::Connected => status.headline_text(),
    }
}

/// Why a connected provider with no usable reading shows no bar.
fn no_reading_reason(id: ProviderId) -> &'static str {
    match id {
        ProviderId::Muse => "No reading yet — appears after your first Muse turn",
        ProviderId::ClaudeCode => "No reading yet — appears after a Claude Code turn",
        ProviderId::Codex => "No reading yet",
    }
}

/// A reading's `as of …` footnote.
fn age_footnote(as_of: i64, now: i64) -> gpui::SharedString {
    format!("as of {}", age_text(as_of, now)).into()
}

/// Unix seconds now. Best-effort: zero when the clock is unavailable.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// A window's reset footnote (`41m`, `3h 12m`, `2d 4h`): how long until
/// `resets_at` from `now`. Unknown when the provider never said — the
/// row then reads "resets in unknown", never a fabricated countdown.
pub fn resets_in_text(resets_at: Option<i64>, now: i64) -> String {
    let Some(resets_at) = resets_at else {
        return "unknown".into();
    };
    let remaining = resets_at.saturating_sub(now);
    if remaining <= 0 {
        return "soon".into();
    }
    let minutes = remaining / 60;
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{}h {}m", hours, minutes % 60);
    }
    format!("{}d {}h", hours / 24, hours % 24)
}

/// A reading's age footnote (`just now`, `2m ago`, `3h ago`, `4d ago`):
/// how long since `as_of` at `now`.
pub fn age_text(as_of: i64, now: i64) -> String {
    let age = now.saturating_sub(as_of);
    if age < 60 {
        return "just now".into();
    }
    let minutes = age / 60;
    if minutes < 60 {
        return format!("{minutes}m ago");
    }
    let hours = minutes / 60;
    if hours < 48 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_status::{Auth, Installed, UsageSnapshot, UsageWindow as SnapshotWindow};

    fn status(id: ProviderId) -> ProviderStatus {
        ProviderStatus {
            provider: id,
            installed: Installed::yes("1.0", "/bin/x"),
            auth: Auth::SignedIn { email: None, plan: None, method: None },
            enabled: true,
            advisory: crate::provider_status::Advisory::None,
            checked_at: Some(1700000000),
            usage: None,
        }
    }

    fn snapshot(provider: &str, as_of: i64) -> UsageSnapshot {
        UsageSnapshot {
            provider: provider.into(),
            plan: Some("prolite".into()),
            windows: vec![SnapshotWindow {
                label: "Weekly".into(),
                used_fraction: 0.85,
                resets_at: Some(1700003600),
            }],
            as_of,
        }
    }

    fn unavailable_text(row: &UsageRowData) -> &str {
        match &row.state {
            UsageRowState::Unavailable(reason) => reason.as_ref(),
            other => panic!("expected an unavailable row, got {other:?}"),
        }
    }

    #[test]
    fn rows_cover_every_enabled_provider_with_reasons() {
        // Connected Codex with a reading rows its windows; a Connected
        // Muse and Claude Code with no reading row their own no-reading
        // reasons; a signed-out provider rows "Signed out" — nobody
        // vanishes.
        let mut muse = status(ProviderId::Muse);
        muse.usage = None;
        let mut claude = status(ProviderId::ClaudeCode);
        claude.usage = None;
        let mut codex = status(ProviderId::Codex);
        codex.usage = Some(snapshot("codex", 1700000000));
        let mut signed_out = status(ProviderId::Codex);
        signed_out.auth = Auth::SignedOut;
        let rows = usage_rows(&[muse, claude, codex], None, 1700000000);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].provider, AuiProvider::Muse);
        assert!(unavailable_text(&rows[0]).contains("first Muse turn"));
        assert_eq!(rows[1].provider, AuiProvider::Claude);
        assert!(unavailable_text(&rows[1]).contains("Claude Code turn"));
        assert_eq!(rows[2].provider, AuiProvider::Codex);
        match &rows[2].state {
            UsageRowState::Windows(windows, as_of) => {
                assert_eq!(windows.len(), 1);
                assert_eq!(as_of.as_deref(), Some("as of just now"));
            }
            other => panic!("expected windows, got {other:?}"),
        }
        let signed_out_rows = usage_rows(&[signed_out], None, 1700000000);
        assert_eq!(signed_out_rows.len(), 1);
        assert_eq!(unavailable_text(&signed_out_rows[0]), "Signed out");
    }

    #[test]
    fn disabled_providers_are_omitted() {
        let mut claude = status(ProviderId::ClaudeCode);
        claude.enabled = false;
        let muse = status(ProviderId::Muse);
        let rows = usage_rows(&[muse, claude], None, 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, AuiProvider::Muse);
    }

    #[test]
    fn not_connected_rows_name_the_reason() {
        let mut missing = status(ProviderId::Codex);
        missing.installed = Installed::No;
        let mut cant_run = status(ProviderId::ClaudeCode);
        cant_run.advisory =
            crate::provider_status::Advisory::cant_run("env: node: No such file or directory", false);
        let mut checking = status(ProviderId::Muse);
        checking.checked_at = None;
        let mut unverified = status(ProviderId::Codex);
        unverified.auth = Auth::Unverified;
        let rows = usage_rows(&[checking, cant_run, missing.clone()], None, 1700000000);
        assert_eq!(unavailable_text(&rows[0]), "Checking…");
        assert!(
            unavailable_text(&rows[1]).contains("env: node: No such file or directory"),
            "the row keeps the probe's own words: {}",
            unavailable_text(&rows[1])
        );
        let missing_rows = usage_rows(&[missing], None, 1700000000);
        assert_eq!(unavailable_text(&missing_rows[0]), "Not installed");
        let unverified_rows = usage_rows(&[unverified], None, 1700000000);
        assert_eq!(unavailable_text(&unverified_rows[0]), "Installed · sign-in not verified");
    }

    #[test]
    fn muse_row_reads_tier_without_a_binary() {
        // No `muse` binary: Not installed. The app is signed in, so the
        // tier's weekly fraction still rows a Weekly bar.
        let mut muse = status(ProviderId::Muse);
        muse.installed = Installed::No;
        muse.auth = Auth::Unknown;
        let feed = MuseFeed { plan: Some("High Usage".into()), weekly_fraction: Some(0.02) };
        let rows = usage_rows(&[muse], Some(feed), 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, AuiProvider::Muse);
        assert_eq!(rows[0].plan.as_deref(), Some("High Usage"));
        match &rows[0].state {
            UsageRowState::Windows(windows, _) => {
                assert_eq!(windows.len(), 1);
                assert_eq!(windows[0].label.to_string(), "Weekly");
                assert!((windows[0].used_fraction - 0.02).abs() < f32::EPSILON);
            }
            other => panic!("expected the tier's Weekly bar, got {other:?}"),
        }
    }

    #[test]
    fn expired_windows_read_reset_not_a_stale_percentage() {
        let mut codex = status(ProviderId::Codex);
        codex.usage = Some(snapshot("codex", 1699990000));
        let rows = usage_rows(&[codex], None, 1700003600 + 60);
        assert_eq!(rows.len(), 1);
        assert!(
            unavailable_text(&rows[0]).contains("reset — no new reading yet"),
            "got {}",
            unavailable_text(&rows[0])
        );
    }

    #[test]
    fn claude_status_text_rows_instead_of_an_unknown_bar() {
        // A Claude Code reading with no usable number (missing
        // `utilization` decodes to unknown) rows the meter's status text,
        // never a bar.
        let mut claude = status(ProviderId::ClaudeCode);
        claude.usage = Some(UsageSnapshot {
            provider: "claude-code".into(),
            plan: Some("Allowed".into()),
            windows: Vec::new(),
            as_of: 1700000000,
        });
        let rows = usage_rows(&[claude], None, 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(unavailable_text(&rows[0]), "Allowed");
    }

    #[test]
    fn reset_and_age_footnotes_read_honestly() {
        assert_eq!(resets_in_text(None, 1700000000), "unknown");
        assert_eq!(resets_in_text(Some(1699999999), 1700000000), "soon");
        assert_eq!(resets_in_text(Some(1700000000 + 41 * 60), 1700000000), "41m");
        assert_eq!(resets_in_text(Some(1700000000 + (3 * 60 + 12) * 60), 1700000000), "3h 12m");
        assert_eq!(
            resets_in_text(Some(1700000000 + (2 * 24 + 4) * 3600), 1700000000),
            "2d 4h"
        );
        assert_eq!(age_text(1700000000, 1700000003), "just now");
        assert_eq!(age_text(1700000000 - 120, 1700000000), "2m ago");
        assert_eq!(age_text(1700000000 - 3 * 3600, 1700000000), "3h ago");
        assert_eq!(age_text(1700000000 - 4 * 86400, 1700000000), "4d ago");
    }
}
