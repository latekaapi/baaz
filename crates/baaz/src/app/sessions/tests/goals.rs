//! Goals over the scripted provider (docs/85 Stage 15): a message sets a
//! goal, Baaz runs the project's check after each goal turn and sends the
//! failing tail once, the ceiling pauses it with one progress note, the
//! offer chip sets nothing by itself, a usage wait resumes by itself, and a
//! relaunch resends once. The check runs in a temp folder; no real CLI.

use super::checkpoints::{cleanup, folder, session_with};
use super::undo::recording_factory_numbered;
use super::*;
use baaz_core::goals::GoalState;

fn runtime_of(baaz: &gpui::Entity<Harness>, vc: &mut gpui::VisualTestContext) -> gpui::Entity<SessionRuntime> {
    vc.update(|_, cx| baaz.read(cx).active.clone().expect("open").read(cx).runtime().clone())
}

fn goal_of(rt: &gpui::Entity<SessionRuntime>, vc: &mut gpui::VisualTestContext) -> baaz_core::goals::Goal {
    vc.update(|_, cx| rt.read(cx).goal.clone().expect("a goal"))
}

/// A turn of the session finishes, as the provider would fold it (the
/// scripted lane here pumps no events of its own).
fn finish_turn(rt: &gpui::Entity<SessionRuntime>, vc: &mut gpui::VisualTestContext, n: u32, text: &str) {
    use aui_protocol::{Block, Turn, TurnMeta};
    use provider::Delta;
    let session = vc.update(|_, cx| rt.read(cx).session_id.clone());
    let id = format!("goal-turn-{n}");
    let deltas = vec![
        Delta::TurnStarted { turn: Turn::Assistant { id: id.clone(), blocks: Vec::new(), meta: TurnMeta::default(), timestamp: None } },
        Delta::BlockAdded { turn_id: id.clone(), block: Block::Text { text: text.to_owned(), streaming: false } },
        Delta::TurnFinished { turn_id: id, meta: TurnMeta { tokens_in: 1_000, tokens_out: 200, ..TurnMeta::default() } },
    ];
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(session), deltas }], cx)));
    vc.run_until_parked();
}

fn with_check(root: &std::path::Path, command: &str) {
    std::fs::create_dir_all(root.join(".baaz")).unwrap();
    std::fs::write(root.join(".baaz/project.toml"), format!("check = \"{command}\"\n")).unwrap();
}

/// 15.1 + 15.3: "until" sets a native goal naming the check; the first
/// turn's check fails, so Baaz sends its tail once; the second passes and
/// the goal is met, recorded in baaz.db.
#[gpui::test]
fn a_goal_from_the_message_runs_the_check_and_ends_met(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-met");
    let root = folder(&state.2, true);
    // Fails once (and leaves the file), then passes.
    with_check(&root, "test -f done.txt || (touch done.txt; echo not yet; false)");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, session) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("Fix the date parser and keep going until the tests pass.".into(), cx)));
    vc.run_until_parked();
    finish_turn(&rt, vc, 1, "I changed the parser.");
    assert_eq!(goal_of(&rt, vc).state, GoalState::Running, "the check failed once");
    // The provider holds the goal (Codex): Baaz waits for it to carry on by
    // itself, and sends the failing tail only once the grace passed with no
    // turn of its own.
    assert!(!sent.lock().unwrap().iter().any(|s| s.0.starts_with("Baaz ran")), "not before the grace");
    vc.executor().advance_clock(crate::goals::NATIVE_CONTINUE_GRACE + std::time::Duration::from_secs(1));
    vc.run_until_parked();
    finish_turn(&rt, vc, 2, "Fixed the last case.");
    let seen: Vec<String> = sent.lock().unwrap().iter().map(|s| s.0.clone()).collect();
    assert!(seen.iter().any(|s| s.starts_with("goal:the tests pass (verified by `test -f done.txt")), "the native goal names its check: {seen:#?}");
    assert!(seen.iter().any(|s| s == "Fix the date parser and keep going until the tests pass."), "{seen:#?}");
    let continued: Vec<&String> = seen.iter().filter(|s| s.starts_with("Baaz ran `test -f done.txt")).collect();
    assert_eq!(continued.len(), 1, "one message with the failing tail: {seen:#?}");
    assert!(continued[0].contains("not yet") && continued[0].ends_with("Keep going until the goal is met: the tests pass"));
    let goal = goal_of(&rt, vc);
    assert_eq!(goal.state, GoalState::Met, "{goal:?}");
    assert_eq!(goal.turns, 2);
    assert!(goal.last_check.as_deref().unwrap().ends_with(": passed"));
    assert!(seen.iter().any(|s| s == "goal:clear"), "the provider's own goal is cleared once met: {seen:#?}");
    let stored = crate::db::call(crate::db::path(), move |opened| baaz_core::db::goals::load(&opened.conn, &session).unwrap()).flatten().expect("stored");
    assert_eq!(stored.state, GoalState::Met);
    cleanup(&root, state);
}

/// Gate 15 live: a provider that carries on by itself (Codex's native goal)
/// gets no failing-check message when it started its own next turn within
/// the grace.
#[gpui::test]
fn a_native_goal_that_carries_on_by_itself_gets_no_message(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-native-continues");
    let root = folder(&state.2, true);
    with_check(&root, "test -f done.txt || (touch done.txt; false)");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, _) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("Keep going until the tests pass".into(), cx)));
    vc.run_until_parked();
    finish_turn(&rt, vc, 1, "Half done.");
    // Codex starts (and finishes) its own next turn inside the grace.
    finish_turn(&rt, vc, 2, "All done.");
    vc.executor().advance_clock(crate::goals::NATIVE_CONTINUE_GRACE + std::time::Duration::from_secs(1));
    vc.run_until_parked();
    assert_eq!(goal_of(&rt, vc).state, GoalState::Met);
    assert!(!sent.lock().unwrap().iter().any(|s| s.0.starts_with("Baaz ran")), "the provider carried on by itself");
    cleanup(&root, state);
}

/// Gate 15 live: an agent that will not move the failing check (it held to
/// an earlier "change only add()") is not sent the same failure forever:
/// the third identical failure blocks the goal and asks the person.
#[gpui::test]
fn the_same_failure_three_turns_running_blocks_the_goal(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-stuck");
    let root = folder(&state.2, true);
    with_check(&root, "echo F test_mul; false");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, _) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("Keep going until the tests pass".into(), cx)));
    vc.run_until_parked();
    for n in 1..=4 {
        finish_turn(&rt, vc, n, "Blocked: I may only change add().");
        vc.executor().advance_clock(crate::goals::NATIVE_CONTINUE_GRACE + std::time::Duration::from_secs(1));
        vc.run_until_parked();
    }
    let goal = goal_of(&rt, vc);
    assert_eq!(goal.state, GoalState::Blocked, "{goal:?}");
    assert!(goal.reason.as_deref().unwrap().contains("failed the same way 3 turns running"));
    let messages = sent.lock().unwrap().iter().filter(|s| s.0.starts_with("Baaz ran")).count();
    assert_eq!(messages, 2, "the first two failures are sent; the third blocks");
    cleanup(&root, state);
}

/// 15.4 + 15.7: over its ceiling the goal pauses and says why in one line;
/// the progress note is written once and replaced, never duplicated.
#[gpui::test]
fn the_ceiling_pauses_the_goal_and_the_progress_note_is_replaced(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-ceiling");
    let root = folder(&state.2, true);
    with_check(&root, "true");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, session) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    let workspace = vc.update(|_, cx| baaz.read(cx).session_workspace(&session, cx));
    let notes = baaz_core::memory::project_dir(std::path::Path::new(&workspace));
    let count_notes = || std::fs::read_dir(&notes).map(|d| d.filter_map(Result::ok).filter(|e| e.file_name().to_string_lossy().starts_with("goal-progress")).count()).unwrap_or(0);
    for round in 0..2 {
        vc.update(|_, cx| {
            rt.update(cx, |rt, _| {
                let mut g = baaz_core::goals::Goal::new(&session, "codex", "all 15 imported", Some("true".into()), true, baaz_core::goals::Limits { minutes: 0, tokens: 1 }, 0);
                g.turns = round;
                rt.goal = Some(g);
            })
        });
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.goal_turn_ended(rt.clone(), cx)));
        vc.run_until_parked();
        let goal = goal_of(&rt, vc);
        assert_eq!(goal.state, GoalState::BudgetReached);
        assert!(goal.reason.as_deref().unwrap().starts_with("Reached ") && goal.reason.as_deref().unwrap().ends_with("Resume to keep going."), "{goal:?}");
        assert_eq!(count_notes(), 1, "one note, replaced (round {round})");
    }
    let body = std::fs::read_dir(&notes).unwrap().filter_map(Result::ok).find(|e| e.file_name().to_string_lossy().starts_with("goal-progress")).map(|e| std::fs::read_to_string(e.path()).unwrap()).unwrap();
    assert!(body.contains("Goal: all 15 imported") && body.contains("How to check: `true` exits 0"), "{body}");
    assert!(!sent.lock().unwrap().iter().any(|s| s.0.starts_with("Keep going")), "a paused goal sends nothing");
    // Resume starts the ceiling over and sends the next turn.
    vc.update(|_, cx| rt.update(cx, |rt, cx| {
        if let Some(g) = rt.goal.as_mut() {
            g.limits = baaz_core::goals::Limits { minutes: 60, tokens: 1_000_000 };
        }
        rt.resume_goal(cx)
    }));
    vc.run_until_parked();
    finish_turn(&rt, vc, 9, "Carried on.");
    assert_eq!(goal_of(&rt, vc).state, GoalState::Met, "resumed, its check passed");
    cleanup(&root, state);
}

/// 15.1: a possible end only offers the chip; accepting sets the goal,
/// and a typed `/goal clear` stops it.
#[gpui::test]
fn a_possible_goal_offers_a_chip_and_a_typed_goal_clears(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-offer");
    let root = folder(&state.2, true);
    with_check(&root, "false");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, _) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("Make the login test pass".into(), cx)));
    vc.run_until_parked();
    let (goal, offer) = vc.update(|_, cx| (rt.read(cx).goal.clone(), rt.read(cx).goal_offer.clone()));
    assert!(goal.is_none(), "an offer sets nothing by itself");
    assert_eq!(offer.as_deref(), Some("Make the login test pass"));
    // The chip's dismiss: nothing is set.
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.dismiss_goal_offer(cx)));
    assert!(vc.update(|_, cx| rt.read(cx).goal_offer.is_none()));
    // Offered again and accepted: the goal is set, and its first turn is
    // the work toward it.
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("Make the login test pass".into(), cx)));
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.accept_goal_offer(cx)));
    assert_eq!(goal_of(&rt, vc).condition, "Make the login test pass");
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.stop_goal(cx)));
    // A typed goal, then its clear (the check fails, so it would go on:
    // the ceiling below stops the loop after the first judgement).
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("/goal the login test passes".into(), cx)));
    vc.update(|_, cx| rt.update(cx, |rt, cx| rt.send_with_goal("/goal clear".into(), cx)));
    vc.run_until_parked();
    assert_eq!(goal_of(&rt, vc).state, GoalState::Stopped);
    let seen: Vec<String> = sent.lock().unwrap().iter().map(|s| s.0.clone()).collect();
    assert!(seen.iter().any(|s| s == "Work toward this goal until it is met: the login test passes"), "{seen:#?}");
    assert!(seen.iter().any(|s| s == "goal:clear"), "{seen:#?}");
    cleanup(&root, state);
}

/// 15.4: a usage limit makes the goal wait; the tick past the reset
/// resumes it by itself.
#[gpui::test]
fn a_usage_limit_waits_and_resumes_by_itself(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-usage");
    let root = folder(&state.2, true);
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, session) = session_with(cx, &root, factory);
    let rt = runtime_of(&baaz, vc);
    vc.update(|_, cx| rt.update(cx, |rt, _| {
        rt.goal = Some(baaz_core::goals::Goal::new(&session, "codex", "all 15 imported", Some("true".into()), true, baaz_core::goals::limits_for(provider::BillingMode::Subscription), baaz_core::clock::now_ms() as i64));
    }));
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.goal_turn_failed(rt.clone(), Some("You've hit your usage limit."), cx)));
    let goal = goal_of(&rt, vc);
    assert_eq!(goal.state, GoalState::UsageWait);
    let at = goal.resume_at.expect("a resume time");
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.goal_tick(at - 1, cx)));
    assert_eq!(goal_of(&rt, vc).state, GoalState::UsageWait, "not yet");
    vc.update(|_, cx| baaz.update(cx, |h, cx| h.goal_tick(at, cx)));
    vc.run_until_parked();
    assert!(sent.lock().unwrap().iter().any(|s| s.0 == "Keep going until the goal is met: all 15 imported"), "resumed by itself");
    cleanup(&root, state);
}

/// 15.6: a goal the last run left running is resent once — "Continue from
/// where you stopped", never its first message — and only once.
#[gpui::test]
fn a_relaunch_resends_a_running_goal_once(cx: &mut gpui::TestAppContext) {
    let state = lane_state("goal-resend");
    let root = folder(&state.2, true);
    with_check(&root, "true");
    let (factory, sent) = recording_factory_numbered(true);
    let (baaz, vc, session) = session_with(cx, &root, factory);
    // Left by an earlier run: last changed before this one began.
    let goal = baaz_core::goals::Goal::new(&session, "codex", "all 15 imported", Some("true".into()), true, baaz_core::goals::limits_for(provider::BillingMode::Subscription), 1);
    crate::db::call(crate::db::path(), move |opened| baaz_core::db::goals::save(&opened.conn, &goal).unwrap());
    // And one this run set (another session's): never resent.
    let mut own = baaz_core::goals::Goal::new("s-this-run", "codex", "x", None, true, baaz_core::goals::limits_for(provider::BillingMode::Subscription), baaz_core::clock::now_ms() as i64);
    own.updated_at = baaz_core::clock::now_ms() as i64 + 1;
    crate::db::call(crate::db::path(), move |opened| baaz_core::db::goals::save(&opened.conn, &own).unwrap());
    for _ in 0..2 {
        vc.update(|_, cx| baaz.update(cx, |h, cx| h.resume_goals_on_open(cx)));
        vc.run_until_parked();
    }
    let resent = sent.lock().unwrap().iter().filter(|s| s.0 == crate::orchestra::CONTINUE_TEXT).count();
    assert_eq!(resent, 1, "once");
    let stored = crate::db::call(crate::db::path(), move |opened| baaz_core::db::goals::load(&opened.conn, &session).unwrap()).flatten().unwrap();
    assert!(stored.resent);
    cleanup(&root, state);
}
