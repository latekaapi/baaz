//! Branch from here over the scripted provider (docs/85 16.3): a branch at
//! a user message forks through that message's answer, opens as its own
//! session, nests under its source in the sidebar, counts under the
//! message and is recorded in baaz.db.

use super::checkpoints::{cleanup, folder, git, session_on, session_uuid_on, session_with, turn};
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

/// docs/87 18.9: on Codex and on Claude Code, in a git folder, a branch at
/// the first message runs in its own worktree cut at that turn's
/// checkpoint: the fork names the worktree, the branch resumes there, and
/// the worktree's files are the checkpoint's, not the folder's later ones.
#[gpui::test]
fn a_codex_branch_opens_in_its_own_worktree_at_the_turns_checkpoint(cx: &mut gpui::TestAppContext) {
    branch_in_worktree(cx, ProviderId::Codex, "branch-wt-codex");
}

#[gpui::test]
fn a_claude_branch_opens_in_its_own_worktree_at_the_turns_checkpoint(cx: &mut gpui::TestAppContext) {
    branch_in_worktree(cx, ProviderId::ClaudeCode, "branch-wt-claude");
}

fn branch_in_worktree(cx: &mut gpui::TestAppContext, provider_id: ProviderId, name: &str) {
    use aui_protocol::{Delta, Turn};
    let state = lane_state(name);
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, source) = session_on(cx, &root, factory, provider_id);
    let rt = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone());
    assert_eq!(vc.update(|_, cx| rt.read(cx).provider_kind()), provider_id);
    let file = root.join("a.txt");
    let ask = |vc: &mut gpui::VisualTestContext, n: usize| {
        let sid = source.clone();
        let user = Turn::User { id: format!("u-{n}"), text: format!("ask {n}"), attachments: Vec::new(), mentions: Vec::new(), timestamp: None };
        vc.update(|_, cx| rt.update(cx, |rt, cx| rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(sid), deltas: vec![Delta::TurnStarted { turn: user }] }], cx)));
        vc.run_until_parked();
    };
    ask(vc, 1);
    turn(&baaz, vc, &source, "a-1", || std::fs::write(&file, "one\ntwo\nthree\n").expect("turn 1 edits"));
    ask(vc, 2);
    turn(&baaz, vc, &source, "a-2", || std::fs::write(&file, "ONE\ntwo\nthree\n").expect("turn 2 edits"));
    let uuid = session_uuid_on(&source, provider_id.as_str());
    let checkpoint = git(&root, &["rev-parse", &format!("refs/baaz/{uuid}/a-1")]);
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.branch_from("u-1", cx)));
    // The worktree is cut off the UI thread; then the fork and its resume.
    for _ in 0..50 {
        vc.run_until_parked();
        if sent.lock().unwrap().iter().any(|s| s.0.starts_with("resumed:") && !s.1.is_empty()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = sent.lock().unwrap().clone();
    let fork = seen.iter().find(|s| s.0 == "fork:a-1").expect("the fork cuts at the first answer");
    let worktree = std::path::PathBuf::from(&fork.1);
    assert!(!fork.1.is_empty() && worktree != root, "the fork names its own folder: {seen:?}");
    assert!(seen.iter().any(|s| s.0.starts_with("resumed:") && s.1 == fork.1), "the branch resumes in the worktree: {seen:?}");
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), checkpoint, "cut at the turn's checkpoint");
    assert_eq!(std::fs::read_to_string(worktree.join("a.txt")).expect("worktree file"), "one\ntwo\nthree\n", "the checkpoint's files");
    assert_eq!(std::fs::read_to_string(&file).expect("folder file"), "ONE\ntwo\nthree\n", "the session's own folder is untouched");
    let _ = std::process::Command::new("git").current_dir(&root).args(["worktree", "remove", "--force"]).arg(&worktree).output();
    cleanup(&root, state);
}

/// docs/87 18.9: a folder that is not a git repository has no checkpoint
/// to cut at, so the branch shares the session's folder (no worktree).
#[gpui::test]
fn a_branch_in_a_folder_without_git_shares_it(cx: &mut gpui::TestAppContext) {
    use aui_protocol::{Delta, Turn};
    let state = lane_state("branch-nogit");
    let root = folder(&state.2, false);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, source) = session_with(cx, &root, factory);
    let rt = vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone());
    let sid = source.clone();
    let user = Turn::User { id: "u-1".into(), text: "ask".into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None };
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(sid), deltas: vec![Delta::TurnStarted { turn: user }] }], cx)));
    turn(&baaz, vc, &source, "a-1", || {});
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.branch_from("u-1", cx)));
    for _ in 0..50 {
        vc.run_until_parked();
        if sent.lock().unwrap().iter().any(|s| s.0 == "fork:a-1") {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let seen = sent.lock().unwrap().clone();
    let fork = seen.iter().find(|s| s.0 == "fork:a-1").expect("the branch forks");
    assert!(fork.1.is_empty(), "no worktree: the branch shares the folder: {seen:?}");
    assert!(vc.update(|_, cx| rt.read(cx).branches_share_folder()), "the chip says so");
    cleanup(&root, state);
}
