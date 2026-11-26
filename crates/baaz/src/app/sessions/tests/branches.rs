//! Branch from here over the scripted provider (docs/85 16.3): a branch at
//! a user message forks through that message's answer, opens as its own
//! session, nests under its source in the sidebar, counts under the
//! message and is recorded in baaz.db.

use super::checkpoints::{cleanup, folder, session_with};
use super::undo::recording_factory_numbered;
use super::*;

#[gpui::test]
fn a_branch_forks_through_the_answer_nests_and_counts(cx: &mut gpui::TestAppContext) {
    use aui_protocol::{Block, Delta, Turn, TurnMeta};
    let state = lane_state("branch");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, source) = session_with(cx, &root, factory);
    let rt = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone());
    // Two exchanges, folded as the provider would.
    let mut deltas = Vec::new();
    for n in 1..=2 {
        deltas.push(Delta::TurnStarted { turn: Turn::User { id: format!("u-{n}"), text: format!("ask {n}"), attachments: Vec::new(), mentions: Vec::new(), timestamp: None } });
        deltas.push(Delta::TurnStarted { turn: Turn::Assistant { id: format!("a-{n}"), blocks: Vec::new(), meta: TurnMeta::default(), timestamp: None } });
        deltas.push(Delta::BlockAdded { turn_id: format!("a-{n}"), block: Block::Text { text: format!("answer {n}"), streaming: false } });
        deltas.push(Delta::TurnFinished { turn_id: format!("a-{n}"), meta: TurnMeta::default() });
    }
    let sid = source.clone();
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(sid), deltas }], cx)));
    vc.run_until_parked();
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.branch_from("u-1", cx)));
    vc.run_until_parked();
    assert!(sent.lock().unwrap().iter().any(|s| s.0 == "fork:a-1"), "the fork cuts at the first message's answer: {:?}", sent.lock().unwrap());
    let (counts, leads) = vc.update(|_, cx| (rt.read(cx).branch_counts.clone(), baaz.read(cx).session_list.read(cx).worker_leads.clone()));
    assert_eq!(counts.get("u-1"), Some(&1));
    let branch = leads.iter().find(|(_, s)| **s == source).map(|(b, _)| b.clone()).expect("the branch nests under its source");
    let src = source.clone();
    let stored = crate::db::call(crate::db::path(), move |opened| baaz_core::db::branches::counts(&opened.conn, &src).unwrap()).unwrap_or_default();
    assert_eq!(stored, vec![("u-1".to_owned(), 1)]);
    assert_ne!(branch, source);
    // A message whose answer is still running has nothing to branch from.
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.branch_from("u-9", cx)));
    let banner = vc.update(|_, cx| rt.read(cx).banner_text().map(str::to_owned));
    assert!(banner.unwrap_or_default().contains("nothing to branch from"));
    cleanup(&root, state);
}
