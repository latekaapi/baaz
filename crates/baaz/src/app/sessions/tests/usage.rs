//! Settings › Usage over a fixture ledger (docs/85 16.1): the page opens
//! by model over 30 days, and a split or range picked on it gathers the
//! table again from baaz.db's roll-up.

use super::checkpoints::{cleanup, folder, session_with};
use super::undo::recording_factory_numbered;
use super::*;

#[gpui::test]
fn the_usage_page_splits_the_ledger_and_follows_its_controls(cx: &mut gpui::TestAppContext) {
    let state = lane_state("usage-page");
    let root = folder(&state.2, true);
    let (factory, _) = recording_factory_numbered(true);
    let (baaz, vc, _) = session_with(cx, &root, factory);
    let now = baaz_core::clock::now_ms() as i64;
    crate::db::call(crate::db::path(), move |opened| baaz_core::db::usage_daily::insert_fixture(&opened.conn, now).unwrap());
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.open_settings_page("usage", cx)));
    vc.run_until_parked();
    let usage = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).settings_facts(cx).usage.expect("the usage facts"));
    let first = usage(vc);
    assert_eq!((first.range, first.split), (1, 1), "30 days by model on first open");
    let keys: Vec<String> = first.lines.iter().map(|l| l.key.to_string()).collect();
    assert_eq!(keys.first().map(String::as_str), Some("claude-sonnet-5-5"), "{keys:?}");
    assert!(first.total.contains("in the last 30 days"), "{}", first.total);
    // The page's own controls: by day over 7 days.
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.settings_page.update(cx, |page, _| {
        page.state.usage_split = Some(0);
        page.state.usage_range = Some(0);
    })));
    let by_day = usage(vc);
    assert_eq!(by_day.lines.len(), 7, "seven days, each with turns");
    assert!(by_day.lines[0].key.starts_with("20"), "days, newest first: {:?}", by_day.lines[0].key);
    assert!(by_day.total.contains("in the last 7 days"));
    cleanup(&root, state);
}
