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
use crate::provider_status::{Auth, Headline, ProviderStatus};
use crate::tier::{ResetClause, Tier, resolve_plan_name};

/// The app's live muse connection and tier for the Muse row: present
/// exactly when the app is signed in to Muse. The row then reports the
/// tier card's own windows even when no `muse` binary was found.
#[derive(Clone, Debug, PartialEq)]
pub struct MuseFeed {
    /// The tier probe's latest answer, when it has answered yet.
    pub tier: Option<Tier>,
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

/// The Muse row while the app is signed in: a live usage read with an
/// unexpired window wins over the tier probe (the binary lookup never
/// gates either); otherwise the tier card's own windows draw — plan with
/// the leading "Muse Code " dropped, "Current" and "Weekly" windows with
/// the card's own reset clauses — then the snapshot's own words.
fn muse_row(status: &ProviderStatus, feed: &MuseFeed, now: i64) -> UsageRowData {
    let snapshot = status.usage.as_ref();
    // The tier card is the richer source (both windows, their reset
    // clauses) and the app's own refresh derives the stored Muse snapshot
    // FROM it (weekly only, no resets) — so a card with numbers wins, and
    // the snapshot only stands in when the card has none.
    let card_has_numbers = matches!(
        feed.tier.as_ref(),
        Some(Tier::Subscription { current_pct, weekly_pct, .. })
            if current_pct.is_some() || weekly_pct.is_some()
    );
    if !card_has_numbers
        && snapshot.is_some_and(|snapshot| {
            snapshot.windows.iter().any(|window| window.resets_at.is_none_or(|at| at > now))
        })
    {
        return connected_row(status, now);
    }
    match feed.tier.as_ref() {
        Some(Tier::Subscription { plan, current_pct, weekly_pct, resets, usage_unavailable, .. }) => {
            // The wire may carry a numeric tier id, never a name: resolve it
            // before it reaches the row, exactly as the header does.
            let shown = resolve_plan_name(plan, None);
            let plan = shown.strip_prefix("Muse Code ").unwrap_or(&shown).to_owned();
            if *usage_unavailable {
                return UsageRowData::new(
                    AuiProvider::Muse,
                    UsageRowState::Unavailable("Usage currently unavailable".into()),
                )
                .plan(plan);
            }
            let mut windows = Vec::new();
            if let Some(pct) = current_pct {
                windows.push(UsageWindow::new("Current", tier_fraction(*pct), tier_reset_text(resets.first(), now)));
            }
            if let Some(pct) = weekly_pct {
                windows.push(UsageWindow::new("Weekly", tier_fraction(*pct), tier_reset_text(resets.get(1), now)));
            }
            if !windows.is_empty() {
                let as_of = snapshot.map(|snapshot| age_footnote(snapshot.as_of, now));
                return UsageRowData::new(AuiProvider::Muse, UsageRowState::Windows(windows, as_of)).plan(plan);
            }
            // A known plan with no numbers yet: the snapshot's own words,
            // still wearing the plan.
            let mut row = snapshot_row(status, snapshot, now);
            if row.plan.is_none() {
                row.plan = Some(plan.into());
            }
            row
        }
        Some(Tier::PayAsYouGo) => UsageRowData::new(
            AuiProvider::Muse,
            UsageRowState::Unavailable("Pay-as-you-go — turns bill API usage".into()),
        ),
        // A failed probe leaves the snapshot's own words standing.
        Some(Tier::Unavailable(_)) => snapshot_row(status, snapshot, now),
        // Signed in but the probe has not answered yet.
        None => UsageRowData::new(AuiProvider::Muse, UsageRowState::Unavailable("Checking…".into())),
    }
}

/// The Muse row without a usable tier answer: the snapshot's own words —
/// its reset note when its windows all expired, else the no-reading
/// reason. A snapshot never gates this; `None` reads the same reason.
fn snapshot_row(
    status: &ProviderStatus,
    snapshot: Option<&crate::provider_status::UsageSnapshot>,
    now: i64,
) -> UsageRowData {
    if snapshot.is_some_and(|snapshot| !snapshot.windows.is_empty()) {
        return connected_row(status, now);
    }
    UsageRowData::new(
        AuiProvider::Muse,
        UsageRowState::Unavailable(no_reading_reason(status.provider).into()),
    )
}

/// One tier-card percentage as a window fraction in `0..=1`.
fn tier_fraction(pct: u32) -> f32 {
    (pct as f32 / 100.0).clamp(0.0, 1.0)
}

/// One tier-card reset clause as a window's reset text: the clause's reset
/// instant as a duration (`3h 12m`), through the same [`resets_in_text`] the
/// other rows use — the library renders `resets in {text}`, so an absolute
/// clause would read `resets in at …`. A clause without an instant (the probe
/// path, old cache files) reads unknown, never a fabricated countdown.
fn tier_reset_text(clause: Option<&ResetClause>, now: i64) -> String {
    match clause.and_then(|clause| clause.resets_at_ms) {
        Some(ms) => resets_in_text(Some((ms / 1000) as i64), now),
        None => resets_in_text(None, now),
    }
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
        let mut row = UsageRowData::new(
            provider,
            UsageRowState::Unavailable(no_reading_reason(status.provider).into()),
        );
        if let Some(plan) = auth_plan(status) {
            row = row.plan(plan);
        }
        return row;
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
    if let Some(plan) = snapshot.and_then(|snapshot| snapshot.plan.clone()).or_else(|| auth_plan(status)) {
        row = row.plan(plan);
    }
    row
}

/// The status's own account plan when no usage snapshot carries one: the
/// auth probe's label with a provider prefix dropped ("Claude Max" →
/// "Max"). `None` for anonymous or plan-less logins.
fn auth_plan(status: &ProviderStatus) -> Option<String> {
    match &status.auth {
        Auth::SignedIn { plan: Some(plan), .. } => {
            Some(plan.strip_prefix("Claude ").unwrap_or(plan).to_owned())
        }
        _ => None,
    }
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

/// A reading's age footnote: just the age (`just now`, `2m ago`) — the
/// library renders the `as of` prefix itself, so passing it here would
/// read "as of as of just now".
fn age_footnote(as_of: i64, now: i64) -> gpui::SharedString {
    age_text(as_of, now).into()
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
                // The age only: the library renders the "as of" prefix.
                assert_eq!(as_of.as_deref(), Some("just now"));
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

    fn subscription(plan: &str, current_pct: Option<u32>, weekly_pct: Option<u32>) -> Tier {
        Tier::Subscription {
            plan: plan.into(),
            current_pct,
            weekly_pct,
            resets: vec!["Resets at 2:57 AM".into(), "Resets Oct 5 at 5:30 AM".into()],
            usage_unavailable: false,
        }
    }

    fn muse_rows(tier: Option<Tier>, now: i64) -> Vec<UsageRowData> {
        let muse = status(ProviderId::Muse);
        usage_rows(&[muse], Some(MuseFeed { tier }), now)
    }

    /// One wire-built Muse tier with fixed reset instants: the Current
    /// window resets 3h 2m after `now`, the Weekly block 4d 20h after.
    fn wired_subscription() -> Tier {
        // Built the way `tier_from_usage` builds it from the wire, without
        // naming the wire type here (the seam ratchet keeps the Muse client
        // out of new files).
        let now = 1700000000u64;
        Tier::Subscription {
            plan: "Muse Code Power Usage".into(),
            current_pct: Some(0),
            weekly_pct: Some(3),
            resets: vec![
                crate::tier::ResetClause::timed("Resets at 5:02 AM".into(), (now + 3 * 3600 + 2 * 60) * 1000),
                crate::tier::ResetClause::timed("Resets Nov 19 at 10:13 PM".into(), (now + 4 * 86400 + 20 * 3600) * 1000),
            ],
            usage_unavailable: false,
        }
    }

    fn window_texts(row: &UsageRowData) -> Vec<String> {
        match &row.state {
            UsageRowState::Windows(windows, _) => {
                windows.iter().map(|window| window.resets_at_text.to_string()).collect()
            }
            other => panic!("expected windows, got {other:?}"),
        }
    }

    #[test]
    fn muse_row_reads_tier_without_a_binary() {
        // No `muse` binary: Not installed. The app is signed in, so the
        // tier card's own windows still row Current and Weekly bars.
        let mut muse = status(ProviderId::Muse);
        muse.installed = Installed::No;
        muse.auth = Auth::Unknown;
        let rows = usage_rows(&[muse], Some(MuseFeed { tier: Some(subscription("Muse Code Power Usage", Some(0), Some(3))) }), 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, AuiProvider::Muse);
        assert_eq!(rows[0].plan.as_deref(), Some("Power Usage"));
        match &rows[0].state {
            UsageRowState::Windows(windows, as_of) => {
                assert_eq!(windows.len(), 2);
                assert_eq!(windows[0].label.to_string(), "Current");
                assert!((windows[0].used_fraction - 0.0).abs() < f32::EPSILON);
                // Words only, no instant (the probe path, old cache files):
                // unknown, never `resets in at …`.
                assert_eq!(windows[0].resets_at_text.to_string(), "unknown");
                assert_eq!(windows[1].label.to_string(), "Weekly");
                assert!((windows[1].used_fraction - 0.03).abs() < f32::EPSILON);
                assert_eq!(windows[1].resets_at_text.to_string(), "unknown");
                // The age only: the library renders the "as of" prefix, so
                // this string must never start with it.
                if let Some(as_of) = as_of {
                    assert!(!as_of.starts_with("as of"), "doubled prefix: {as_of}");
                }
            }
            other => panic!("expected the tier's Current and Weekly bars, got {other:?}"),
        }
    }

    /// Wire-built Muse windows pass durations to the library — the same
    /// `resets_in_text` the Codex/Claude rows use — so the menu reads
    /// `resets in 3h 2m`, never `resets in at …`.
    #[test]
    fn muse_windows_pass_durations_to_the_library() {
        let rows = muse_rows(Some(wired_subscription()), 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].plan.as_deref(), Some("Power Usage"));
        assert_eq!(window_texts(&rows[0]), vec!["3h 2m".to_owned(), "4d 20h".to_owned()]);
    }

    /// A numeric wire plan id never reaches the Muse row: the generic label
    /// stands in when no human name is known (the live tier already carries
    /// the last human name when there is one).
    #[test]
    fn a_numeric_plan_never_reaches_the_muse_row() {
        let tier = Tier::Subscription {
            plan: "27681631238169137".into(),
            current_pct: Some(0),
            weekly_pct: Some(5),
            resets: vec![
                ResetClause::timed("Resets at 7:57 AM".into(), (1700000000 + 3 * 3600) as u64 * 1000),
                ResetClause::timed("Resets Oct 5 at 5:30 AM".into(), (1700000000 + 4 * 86400) as u64 * 1000),
            ],
            usage_unavailable: false,
        };
        let rows = muse_rows(Some(tier), 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].plan.as_deref(), Some("Subscription"));
        assert!(
            !rows[0].plan.as_deref().unwrap_or_default().contains("27681631238169137"),
            "the id never shows: {:?}",
            rows[0].plan
        );
        assert_eq!(window_texts(&rows[0]), vec!["3h 0m".to_owned(), "4d 0h".to_owned()]);
    }

    #[test]
    fn muse_row_covers_every_tier_variant() {
        // A plan with no numbers yet keeps the plan beside the no-reading
        // reason; `usage_unavailable` names itself; pay-as-you-go names
        // the billing; no probe answer yet reads Checking.
        let bare = &muse_rows(Some(subscription("Muse Code Power Usage", None, None)), 1700000000)[0];
        assert_eq!(bare.plan.as_deref(), Some("Power Usage"));
        assert!(unavailable_text(bare).contains("first Muse turn"), "got {}", unavailable_text(bare));

        let mut dark = subscription("Muse Code Power Usage", None, None);
        let Tier::Subscription { usage_unavailable, .. } = &mut dark else { unreachable!() };
        *usage_unavailable = true;
        let dark = &muse_rows(Some(dark), 1700000000)[0];
        assert_eq!(dark.plan.as_deref(), Some("Power Usage"));
        assert_eq!(unavailable_text(dark), "Usage currently unavailable");

        let payg = &muse_rows(Some(Tier::PayAsYouGo), 1700000000)[0];
        assert_eq!(unavailable_text(payg), "Pay-as-you-go — turns bill API usage");

        let checking = &muse_rows(None, 1700000000)[0];
        assert_eq!(unavailable_text(checking), "Checking…");
    }

    #[test]
    fn muse_row_prefers_the_tier_card_and_falls_back_to_the_snapshot() {
        // The app derives the stored Muse snapshot from the tier (weekly
        // only, no resets), so a card with numbers must win: both windows,
        // real reset clauses. The snapshot stands in only when the card has
        // no numbers.
        let mut muse = status(ProviderId::Muse);
        muse.usage = Some(UsageSnapshot {
            provider: "muse".into(),
            plan: None,
            windows: vec![SnapshotWindow {
                label: "Session".into(),
                used_fraction: 0.5,
                resets_at: Some(1700003600),
            }],
            as_of: 1700000000,
        });
        let rows = usage_rows(&[muse.clone()], Some(MuseFeed { tier: Some(wired_subscription()) }), 1700000000);
        match &rows[0].state {
            UsageRowState::Windows(windows, _) => {
                let labels: Vec<String> = windows.iter().map(|w| w.label.to_string()).collect();
                assert_eq!(labels, ["Current", "Weekly"]);
                assert_eq!(windows[0].resets_at_text.to_string(), "3h 2m");
                assert_eq!(windows[1].resets_at_text.to_string(), "4d 20h");
            }
            other => panic!("expected the tier card's bars, got {other:?}"),
        }
        let rows = usage_rows(&[muse], Some(MuseFeed { tier: Some(subscription("Muse Code Power Usage", None, None)) }), 1700000000);
        match &rows[0].state {
            UsageRowState::Windows(windows, _) => assert_eq!(windows[0].label.to_string(), "Session"),
            other => panic!("expected the snapshot's bar, got {other:?}"),
        }
    }

    #[test]
    fn plans_fall_back_to_the_status_account_facts() {
        // No snapshot plan: Claude Code reads its auth fact with the
        // provider prefix dropped; Codex the same when its snapshot is
        // plan-less (its "prolite" snapshot keeps showing as before).
        let mut claude = status(ProviderId::ClaudeCode);
        claude.auth = Auth::SignedIn { email: None, plan: Some("Claude Max".into()), method: None };
        let rows = usage_rows(&[claude], None, 1700000000);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].plan.as_deref(), Some("Max"));
        assert!(unavailable_text(&rows[0]).contains("Claude Code turn"));

        let mut codex = status(ProviderId::Codex);
        codex.auth = Auth::SignedIn { email: None, plan: Some("prolite".into()), method: None };
        codex.usage = Some(UsageSnapshot {
            provider: "codex".into(),
            plan: None,
            windows: vec![SnapshotWindow {
                label: "Weekly".into(),
                used_fraction: 0.1,
                resets_at: Some(1700003600),
            }],
            as_of: 1700000000,
        });
        let rows = usage_rows(&[codex], None, 1700000000);
        assert_eq!(rows[0].plan.as_deref(), Some("prolite"));
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
