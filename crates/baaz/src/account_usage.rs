//! The account menu's Usage section: one card per Connected provider.
//!
//! The cards render from the provider status service — the single store
//! probes, lane peeks and the tier feed — never from label strings. A
//! provider whose headline is not Connected gets no card; a Connected one
//! with no reading yet cards "Not reported yet" with no "as of" footnote
//! (there is no reading to date).

use crate::providers::ProviderId;
use crate::provider_status::{Headline, ProviderStatus};

/// One usage card's model: the provider, its plan, its windows, and when
/// the reading landed. `as_of` is `None` when nothing reported one yet.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCardModel {
    /// Which provider this card is for.
    pub provider: ProviderId,
    /// The plan, when known.
    pub plan: Option<String>,
    /// The windows, possibly empty.
    pub windows: Vec<UsageCardWindow>,
    /// Unix time the reading landed, when any did.
    pub as_of: Option<i64>,
}

/// One window on a usage card.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCardWindow {
    /// The card label (`Weekly`, `Session · 5h`, …).
    pub label: String,
    /// Fraction used, 0.0–1.0.
    pub used_fraction: f64,
    /// Unix time the window resets, when known.
    pub resets_at: Option<i64>,
}

/// The cards for `statuses`: one per Connected provider, in registry
/// order. Anything else — Checking, Signed out, Disabled — cards
/// nothing.
pub fn usage_cards(statuses: &[ProviderStatus]) -> Vec<UsageCardModel> {
    let mut cards = Vec::new();
    for id in ProviderId::all() {
        let Some(status) = statuses.iter().find(|status| status.provider == id) else {
            continue;
        };
        if status.headline() != Headline::Connected {
            continue;
        }
        let snapshot = status.usage.as_ref();
        cards.push(UsageCardModel {
            provider: id,
            plan: snapshot.and_then(|snapshot| snapshot.plan.clone()),
            windows: snapshot
                .map(|snapshot| {
                    snapshot
                        .windows
                        .iter()
                        .map(|window| UsageCardWindow {
                            label: window.label.clone(),
                            used_fraction: window.used_fraction,
                            resets_at: window.resets_at,
                        })
                        .collect()
                })
                .unwrap_or_default(),
            as_of: snapshot.map(|snapshot| snapshot.as_of),
        });
    }
    cards
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
/// card then reads "resets in unknown", never a fabricated countdown.
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
    use crate::provider_status::{Auth, Installed, UsageSnapshot, UsageWindow};

    fn connected(id: ProviderId, usage: Option<UsageSnapshot>) -> ProviderStatus {
        ProviderStatus {
            provider: id,
            installed: Installed::yes("1.0", "/bin/x"),
            auth: Auth::SignedIn { email: None, plan: None, method: None },
            enabled: true,
            advisory: crate::provider_status::Advisory::None,
            checked_at: Some(1700000000),
            usage,
        }
    }

    fn snapshot() -> UsageSnapshot {
        UsageSnapshot {
            provider: "codex".into(),
            plan: Some("prolite".into()),
            windows: vec![UsageWindow {
                label: "Weekly".into(),
                used_fraction: 0.85,
                resets_at: Some(1700003600),
            }],
            as_of: 1700000000,
        }
    }

    #[test]
    fn cards_render_only_for_connected_providers() {
        // Three Connected providers card in registry order; a missing
        // entry never cards.
        let statuses = vec![
            connected(ProviderId::Muse, None),
            connected(ProviderId::ClaudeCode, None),
            connected(ProviderId::Codex, Some(snapshot())),
        ];
        let cards = usage_cards(&statuses);
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].provider, ProviderId::Muse);
        assert_eq!(cards[1].provider, ProviderId::ClaudeCode);
        assert_eq!(cards[2].provider, ProviderId::Codex);
        assert_eq!(cards[2].plan.as_deref(), Some("prolite"));
        assert_eq!(cards[2].windows.len(), 1);
        assert_eq!(cards[2].as_of, Some(1700000000));
        // A Connected provider with no reading cards empty with no
        // as-of: "Not reported yet", never "as of never".
        assert!(cards[0].windows.is_empty());
        assert_eq!(cards[0].as_of, None);
    }

    #[test]
    fn non_connected_headlines_card_nothing() {
        let mut checking = connected(ProviderId::Codex, Some(snapshot()));
        checking.checked_at = None;
        assert!(usage_cards(&[checking]).is_empty(), "Checking cards nothing");
        let signed_out = ProviderStatus {
            auth: Auth::SignedOut,
            checked_at: Some(1700000000),
            ..connected(ProviderId::Codex, Some(snapshot()))
        };
        assert!(usage_cards(&[signed_out]).is_empty(), "Signed out cards nothing");
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
