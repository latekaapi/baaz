//! Settings › Usage, the app's half (docs/85 16.1): the page's model from
//! the provider status service (each provider's limits, as the account
//! menu reads them) and from baaz.db's `usage_daily` roll-up (the table).
//!
//! Not keyed by session id: the roll-up is per day × provider × model ×
//! project.

use aui::screens::{UsageLimits, UsageLine, UsageRowState};
use baaz_core::db::usage_daily::{By, Totals};

/// Days per range, in [`aui::screens::USAGE_RANGES`]' order.
pub(crate) const RANGE_DAYS: [u32; 3] = [7, 30, 90];

/// The split at `index`, in [`aui::screens::USAGE_SPLITS`]' order.
pub(crate) fn split_at(index: usize) -> By {
    match index {
        0 => By::Day,
        2 => By::Project,
        3 => By::Provider,
        _ => By::Model,
    }
}

/// `4.8M`, `640k`, `950`.
pub(crate) fn tokens_words(n: i64) -> String {
    match n {
        ..=999 => n.to_string(),
        1_000..=999_999 => format!("{}k", n / 1_000),
        _ => format!("{:.1}M", n as f64 / 1_000_000.0),
    }
}

/// What a row cost: dollars billed, or "included" on a plan.
pub(crate) fn cost_words(usd: f64) -> String {
    if usd >= 0.005 { format!("${usd:.2}") } else { "included".to_owned() }
}

/// The table's rows; a project row names its project (`names` maps ids).
pub(crate) fn lines(totals: &[Totals], names: &dyn Fn(&str) -> String) -> Vec<UsageLine> {
    totals
        .iter()
        .map(|t| UsageLine {
            key: if t.key.is_empty() { "Unknown".into() } else { names(&t.key).into() },
            detail: format!("{} tokens \u{b7} {}% cached \u{b7} {} turn{}", tokens_words(t.tokens_in + t.tokens_out), t.cache_pct(), t.turns, if t.turns == 1 { "" } else { "s" }).into(),
            cost: cost_words(t.cost_usd).into(),
        })
        .collect()
}

/// The range's total, in one line.
pub(crate) fn total_line(totals: &[Totals], days: u32) -> String {
    let tokens: i64 = totals.iter().map(|t| t.tokens_in + t.tokens_out).sum();
    let turns: i64 = totals.iter().map(|t| t.turns).sum();
    let cost: f64 = totals.iter().map(|t| t.cost_usd).sum();
    let billed = if cost >= 0.005 { format!(", ${cost:.2} billed") } else { String::new() };
    format!("{} tokens, {turns} turns{billed} in the last {days} days", tokens_words(tokens))
}

/// The ledger split `by` over `days`, refreshed first; empty when baaz.db
/// cannot answer. Runs on baaz.db's writer thread (milliseconds).
pub(crate) fn totals(by: By, days: u32) -> Vec<Totals> {
    let now = baaz_core::clock::now_ms() as i64;
    crate::db::call(crate::db::path(), move |opened| {
        if let Err(why) = baaz_core::db::usage_daily::refresh(&opened.conn, now) {
            crate::baaz_log!("usage: the roll-up did not refresh ({why})");
        }
        baaz_core::db::usage_daily::totals(&opened.conn, by, days, now).unwrap_or_default()
    })
    .unwrap_or_default()
}

/// Each provider's limits, from the account menu's rows (connected
/// providers with a reading; the others are left to the Providers page).
pub(crate) fn limits(rows: Vec<aui::screens::UsageRowData>) -> Vec<UsageLimits> {
    rows.into_iter()
        .filter_map(|row| match row.state {
            UsageRowState::Windows(windows, as_of) if !windows.is_empty() => Some(UsageLimits { provider: row.provider, plan: row.plan, windows, as_of }),
            _ => None,
        })
        .collect()
}

/// `usage-fixture` (docs/85 16.1 captures): thirty days of fixture turns
/// in the capture's own baaz.db. Captures only (fails the step otherwise):
/// a real store keeps its own ledger.
#[cfg(feature = "probe")]
pub(crate) fn step_usage_fixture() -> crate::steps::StepResult {
    if !crate::runtime::captures() {
        return Err("usage-fixture writes fixture turns: captures only".into());
    }
    let now = baaz_core::clock::now_ms() as i64;
    crate::db::call(crate::db::path(), move |opened| baaz_core::db::usage_daily::insert_fixture(&opened.conn, now).map_err(|e| e.to_string()))
        .unwrap_or_else(|| Err("baaz.db did not answer".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(key: &str, tin: i64, tout: i64, cache: i64, turns: i64, cost: f64) -> Totals {
        Totals { key: key.into(), turns, tokens_in: tin, tokens_out: tout, cache_read: cache, cost_usd: cost }
    }

    #[test]
    fn the_table_words_tokens_cache_and_cost() {
        let rows = [t("p-web", 4_000_000, 800_000, 2_840_000, 212, 0.0), t("", 200_000, 10_000, 24_000, 1, 0.31)];
        let out = lines(&rows, &|id| if id == "p-web" { "acme-web".into() } else { id.into() });
        assert_eq!(out[0].key.as_ref(), "acme-web");
        assert_eq!(out[0].detail.as_ref(), "4.8M tokens \u{b7} 71% cached \u{b7} 212 turns");
        assert_eq!(out[0].cost.as_ref(), "included");
        assert_eq!((out[1].key.as_ref(), out[1].cost.as_ref()), ("Unknown", "$0.31"));
        assert_eq!(out[1].detail.as_ref(), "210k tokens \u{b7} 12% cached \u{b7} 1 turn");
        assert_eq!(total_line(&rows, 30), "5.0M tokens, 213 turns, $0.31 billed in the last 30 days");
        assert_eq!(split_at(0), By::Day);
        assert_eq!(split_at(3), By::Provider);
    }
}
