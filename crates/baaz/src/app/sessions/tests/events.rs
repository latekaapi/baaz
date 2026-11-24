//! Tests for the session-event sink: lane loss, outcomes, unread and needs-you, parked finishes, browser chats.

use super::*;
use crate::sidebar::SessionRow as _;

/// docs/71 §2–§3 (owner 2026-10-30): a lane whose child dies twice in
/// a row keeps its session on screen — transcript and composer — and
/// grows its lost banner into the typed failure (kind Crashed, the
/// stderr well); it never takes the centre or parks. Retry revives the
/// child and takes the failure down.
#[gpui::test]
fn a_lane_lost_twice_keeps_its_transcript_and_grows_the_banner(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("lost-twice");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("the lane is open"));
    vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.step_child_lost(cx)))).expect("the child loss lands");
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert!(view.read(cx).rt(cx).child_failure().is_none(), "one loss is the quiet banner only");
    });
    vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.step_child_lost(cx)))).expect("the child loss lands");
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.open_state.provider_open_error.is_none(), "no full-centre failure view for a lane that opened");
        assert!(harness.active.as_ref() == Some(&view), "the session stays open, transcript and composer");
        assert!(
            !harness.registry.is_view_parked(&view),
            "the view is not parked"
        );
        let failure = view.read(cx).rt(cx).child_failure().cloned().expect("the banner grew into the failure");
        assert_eq!(failure.kind, crate::provider_status::FailureKind::Crashed);
        assert_eq!(failure.stderr_excerpt.len(), 3, "the step's sample well");
    });
    vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.run_banner_action(cx))));
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert!(view.read(cx).rt(cx).child_failure().is_none(), "Retry revives and stands the failure down");
        assert!(baaz.read(cx).active.as_ref() == Some(&view), "the same view throughout");
    });
    lane_restore(state);
}

/// docs/71 §6: a Claude Code or Codex turn's outcome reaches the row's
/// "Failed" through the lane (`ProviderTurnOutcome`), and a later
/// success stands it down.
#[gpui::test]
fn a_lane_turn_outcome_moves_the_rows_failed_state(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("turn-outcome");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("the lane is open"));
    let session_id = vc.update(|_, cx| view.read(cx).rt(cx).session_id.clone());
    let outcome = |failed: bool, error: Option<&str>| crate::session::SessionEvent::ProviderTurnOutcome {
        session_id: session_id.clone(),
        failed,
        error: error.map(str::to_owned),
    };
    let failed = outcome(true, Some("stream disconnected"));
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_session_event(view.read(cx).runtime().clone(), &failed, cx)));
    vc.update(|_, cx| {
        let error = baaz.read(cx).session_list.read(cx).overrides.get(&session_id).and_then(|meta| meta.last_error.clone());
        assert_eq!(error.as_deref(), Some("stream disconnected"), "the failure is recorded");
    });
    let ok = outcome(false, None);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_session_event(view.read(cx).runtime().clone(), &ok, cx)));
    vc.update(|_, cx| {
        let error = baaz.read(cx).session_list.read(cx).overrides.get(&session_id).and_then(|meta| meta.last_error.clone());
        assert!(error.is_none(), "a later success stands it down");
    });
    lane_restore(state);
}

/// Task 3.1: a turn that finishes anywhere but the open session read at
/// its end leaves the row `DoneUnread` (persisted), reading it to the
/// end clears it, and a child Baaz stopped restarting reads `Failed`.
#[gpui::test]
fn unread_is_set_by_an_unseen_finish_and_cleared_by_reading(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("unread");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("the lane is open"));
    let session_id = vc.update(|_, cx| view.read(cx).rt(cx).session_id.clone());
    let unread = |id: &str, cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| baaz.read(cx).session_list.read(cx).overrides.get(id).is_some_and(|meta| meta.unread))
    };
    // The open session, at its end: the finish is read as it lands.
    let done = crate::session::SessionEvent::ProviderTurnOutcome { session_id: session_id.clone(), failed: false, error: None };
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_session_event(view.read(cx).runtime().clone(), &done, cx)));
    assert!(!unread(&session_id, vc), "a finish read as it lands is not unread");
    // Another session's finish: nobody read it.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.record_turn_outcome("s-elsewhere", false, None, cx)));
    assert!(unread("s-elsewhere", vc), "an unseen finish is unread");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.mark_read("s-elsewhere", cx)));
    assert!(!unread("s-elsewhere", vc), "reading it clears it");
    // The open session, flagged unread, arriving at its end clears.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.set_override(&session_id, |meta| meta.unread = true, cx)));
    let seen = crate::session::SessionEvent::SeenToEnd;
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_session_event(view.read(cx).runtime().clone(), &seen, cx)));
    assert!(!unread(&session_id, vc), "arriving at the end reads it");
    // Baaz stopped restarting the child: the row reads Failed.
    let lost = crate::session::SessionEvent::ChildFailed { message: "the agent exited".into(), stderr_excerpt: Vec::new() };
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.on_session_event(view.read(cx).runtime().clone(), &lost, cx)));
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let error = harness.session_list.read(cx).overrides.get(&session_id).and_then(|meta| meta.last_error.clone());
        assert_eq!(error.as_deref(), Some("the agent exited"), "a lost child is recorded as failed");
    });
    lane_restore(state);
}

/// Task 3.3: a session in the background that moves to needing the
/// person posts one notification (tagged with its id) and the badge
/// counts it; the first look after launch posts nothing; a click opens
/// the notification's session.
#[gpui::test]
fn a_background_session_needing_you_posts_and_badges(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("attention");
    // The test platform, like macOS, shows nothing outside an app with
    // an identity (an unbundled `cargo run` posts nothing).
    cx.update(|cx| cx.set_app_identity("sh.baaz.test", "Baaz"));
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let shown_before = vc.shown_system_notifications().len();
    // A parked session, working: the first look learns it.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let mut row = crate::sidebar::tests::entry("s-parked");
            row.label = "Rotate the keys".into();
            row.turns = 1;
            row.running = true;
            harness.session_list.update(cx, |list, _| list.sessions.push(row));
            harness.invalidate_list(cx);
            harness.check_attention(cx);
        })
    });
    assert_eq!(vc.shown_system_notifications().len(), shown_before, "a first look announces nothing");
    // It asks for an approval.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                let row = list.sessions.iter_mut().find(|e| e.id == "s-parked").expect("the row");
                row.apply_status_changed(true, &[provider::facts::Attention::ApprovalPending]);
            });
            harness.invalidate_list(cx);
            harness.check_attention(cx);
        })
    });
    let shown = vc.shown_system_notifications();
    assert_eq!(shown.len(), shown_before + 1, "one notification");
    let last = shown.last().expect("posted");
    assert_eq!(last.tag.as_ref(), "s-parked");
    assert_eq!(last.title.as_ref(), "Rotate the keys");
    assert_eq!(last.body.as_ref(), "Waiting for your approval");
    assert_eq!(vc.update(|_, cx| crate::attention::Notifier::badge(cx)), Some(1), "the badge counts it");
    // Nothing moved: nothing new.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.check_attention(cx)));
    assert_eq!(vc.shown_system_notifications().len(), shown_before + 1);
    lane_restore(state);
}

/// Task 3.4: ⌘⇧J (`jump_to_next_waiting`) opens the waiting sessions
/// one after another in the sidebar's order and wraps; the "Needs you"
/// filter narrows the sidebar to them.
#[gpui::test]
fn jump_to_next_waiting_cycles_in_order(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("next-waiting");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
            let now = chrono::Local::now();
            for (id, minutes, waiting) in [("w-new", 1, true), ("idle", 2, false), ("w-old", 3, true)] {
                let mut row = crate::sidebar::tests::entry(id);
                row.turns = 1;
                row.updated = now - chrono::Duration::minutes(minutes);
                if waiting {
                    row.attention = vec![provider::facts::Attention::ApprovalPending];
                }
                harness.session_list.update(cx, |list, _| list.sessions.push(row));
            }
            harness.invalidate_list(cx);
        });
    });
    let target = |vc: &mut gpui::VisualTestContext| {
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            harness.open_state.pending_id.clone().or_else(|| harness.active_id(cx))
        })
    };
    let mut seen = Vec::new();
    for _ in 0..3 {
        vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.jump_to_next_waiting(window, cx)));
        vc.run_until_parked();
        seen.push(target(vc).unwrap_or_default());
    }
    assert_eq!(seen, ["w-new", "w-old", "w-new"], "newest first, the idle row skipped, wrapping");
    // The filter: only the two waiting rows are drawn.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.toggle_needs_you_filter(cx)));
    let drawn = vc.update(|_, cx| {
        let grouping = baaz.read(cx).sidebar_grouping(cx);
        let mut ids = Vec::new();
        if let aui::nav::Grouping::Date(buckets) = grouping.as_ref() {
            for bucket in buckets {
                ids.extend(bucket.sessions.iter().map(|s| s.id.to_string()));
            }
        }
        ids
    });
    assert!(drawn.iter().all(|id| id == "w-new" || id == "w-old"), "only waiting rows: {drawn:?}");
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.toggle_needs_you_filter(cx)));
    lane_restore(state);
}

/// The W5b draft arm: a record with no settled turns holds no history
/// anywhere, so clicking it opens fresh (`OpenSession`, never
/// `ResumeSession`) and the stale record leaves with it — the sidebar
/// never accumulates dead rows. Resuming it would replay nothing.
#[gpui::test]
fn a_provider_draft_with_no_turns_opens_fresh_instead_of_resuming(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("draft-reopen");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let open_id = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone()
    });
    assert_eq!(open_id, "s-open");
    // The restart, with no turn ever settling: the record counts zero.
    let mut forgotten = Vec::new();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            if let Some(view) = harness.active.take() {
                forgotten.push(view);
            }
            forgotten.extend(harness.registry.drain_parked());
            harness.session_list.update(cx, |list, _| list.sessions.clear());
            harness.session_list.update(cx, |list, _| list.provider_sessions = crate::provider_sessions::read());
            harness.merge_provider_rows(cx);
        });
    });
    let _forgotten = forgotten;
    vc.update(|_, cx| {
        assert_eq!(
            baaz.read(cx).session_list.read(cx).provider_sessions.get(&open_id).map(|record| record.turns),
            Some(0),
            "the untouched draft counts no turns"
        );
    });
    // The click: the draft opens fresh, it is never resumed.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(open_id.clone(), window, cx));
    });
    vc.run_until_parked();
    let sent = commands.lock().expect("commands").clone();
    assert!(
        sent.iter().any(|command| matches!(
            command,
            provider::Command::OpenSession { .. }
        )),
        "the draft opens fresh, drew {sent:?}"
    );
    assert!(
        !sent.iter().any(|command| matches!(
            command,
            provider::Command::ResumeSession { .. }
        )),
        "nothing is resumed for a draft with no history, drew {sent:?}"
    );
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the fresh lane is open");
        assert!(view.read(cx).rt(cx).provider_kind() != ProviderId::Muse, "a lane opens, not a muse view");
        assert!(
            harness.overlays.read(cx).dialog().is_none(),
            "no failure dialog on a fresh open"
        );
        assert_eq!(
            harness.session_list.read(cx).provider_sessions.get(&open_id).map(|record| record.turns),
            Some(0),
            "the reopened record is the fresh one"
        );
    });
    lane_restore(state);
}

/// The W5 title arm through the real rejoin: a generated title the
/// muse side session wrote lands on the provider row's label.
#[gpui::test]
fn a_generated_title_lands_on_a_provider_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("retitle");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let open_id = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.set_override(&open_id, |meta| {
                meta.generated_title = Some("Shiny generated".into());
            }, cx);
        });
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == open_id).expect("the row");
        assert_eq!(row.label, "Shiny generated", "the auto-title names the provider row");
    });
    lane_restore(state);
}

/// A finished provider turn counts: one real send through the lane
/// (the scripted double echoes a user turn, an assistant turn and its
/// finish) raises the record's and the row's `turns` to at least one
/// and moves `updatedMs` — the count the empty filter and the row's
/// byline both read.
#[gpui::test]
fn a_finished_provider_turn_counts_and_moves_the_record(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("turn-count");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let open_id = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let id = harness.active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone();
            // An old stamp, so the move below is the turn's, not the open's.
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut(&id).expect("the open's record").updated_ms = 1);
            id
        })
    });
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane view");
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.send_text("hello there".to_owned(), cx)));
    });
    vc.run_until_parked();
    // The double's echo: the user turn, the assistant turn, its finish.
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane view");
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.test_drain_provider(cx)));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let record = &harness.session_list.read(cx).provider_sessions[&open_id];
        assert!(record.turns >= 1, "the finished turn counts on the record, held {}", record.turns);
        assert!(record.updated_ms > 1, "the finished turn moves the record");
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == open_id).expect("the row");
        assert!(row.turns >= 1, "the row reads the counted turn, held {}", row.turns);
        assert!(!row.is_empty(), "a counted session is never empty");
    });
    lane_restore(state);
}

/// A retry of a first turn that never settled keeps the generated
/// title (live, 2026-10-16): the row still reads zero turns, so the
/// admission's `first_send_update` used to title it from the resent
/// prompt — over the generated title, in the row and in the header that
/// falls back to it once the row is filtered.
#[gpui::test]
fn a_retried_first_send_keeps_the_generated_title(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("retry-title");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::ClaudeCode, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let (view, open_id) = vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane opened");
        let id = view.read(cx).rt(cx).session_id.clone();
        (view, id)
    });
    let accept = |vc: &mut gpui::VisualTestContext, turn: &str| {
        let (id, turn) = (open_id.clone(), turn.to_owned());
        vc.update(|_, cx| {
            view.update(cx, |_, cx| {
                cx.emit(SessionEvent::ProviderTurnAccepted {
                    session_id: id,
                    prompt: "create a simple html explainer".into(),
                    turn_id: turn,
                });
            });
        });
        vc.run_until_parked();
    };
    accept(vc, "t-first");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.set_override(&open_id, |meta| {
                meta.generated_title = Some("Simple HTML Database Structure Explainer".into());
            }, cx);
        });
    });
    // The first turn never settled; the person retries the same words.
    accept(vc, "t-retry");
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == open_id).expect("the row");
        assert_eq!(row.turns, 0, "the first turn never settled");
        assert_eq!(row.label, "Simple HTML Database Structure Explainer", "the retry keeps the title");
        assert_eq!(
            harness.collapsed_head_label(&open_id, cx).as_deref(),
            Some("Simple HTML Database Structure Explainer"),
            "the header reads the generated title"
        );
    });
    lane_restore(state);
}

/// B3b: a parked session finishing its turn flips its row from Working
/// to Settled without being activated. Before the fix the second open
/// cleared the first view's subscription, so the finish reached no one:
/// the `view_subscriptions` count below stays flat and the row keeps reading
/// Working.
#[gpui::test]
fn a_parked_session_finishing_its_turn_settles_its_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3b-park-turn");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).rt(cx).session_id.clone()
    });
    assert!(first.starts_with("s-park-"), "the quiet double mints the id");
    // One settled turn behind it, one in flight: the row reads Working.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut(&first).expect("the first record").turns = 1);
            harness.merge_provider_rows(cx);
            harness.session_list.update(cx, |list, _| {
                let row = list.sessions.iter_mut().find(|entry| entry.id == first).expect("the first row");
                row.running = true;
                row.turn_started = Some(crate::clock::now_local());
            });
            cx.notify();
        });
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == first).expect("the first row");
        assert_eq!(
            row.row_status(crate::clock::now_local()).kind,
            aui::nav::RowStatusKind::Working,
            "setup: the row reads Working before the switch"
        );
    });
    let subs_before = vc.update(|_, cx| baaz.read(cx).registry.subscription_count());
    // Switching away parks the first view; its subscription must stay.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let second =
            harness.active.clone().expect("the second lane opened").read(cx).rt(cx).session_id.clone();
        assert_ne!(second, first, "the second open is another session");
        assert!(
            harness.registry.is_parked(&first),
            "the first view parks instead of closing"
        );
        assert_eq!(
            harness.registry.subscription_count(),
            subs_before + 2,
            "both live sessions stay subscribed: each runtime and each view"
        );
    });
    // The parked turn finishes: no activation, just the lane event.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness
                .registry.parked(&first)
                .expect("the parked view");
            view.update(cx, |_, cx| {
                cx.emit(SessionEvent::ProviderTurnFinished {
                    session_id: first.clone(),
                    turn_id: "t-parked-1".into(),
                    meta: aui_protocol::TurnMeta::default(),
                });
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == first).expect("the first row");
        assert!(!row.running, "the parked finish stands the row down");
        assert_eq!(
            row.row_status(crate::clock::now_local()).kind,
            aui::nav::RowStatusKind::Settled,
            "Working becomes Settled without activating"
        );
    });
    lane_restore(state);
}

/// docs/81 A F8, docs/82 4.12: a truncated store the first launch imports
/// is left exactly as it is — never renamed, never overwritten — and the
/// window starts with one toast saying so.
#[gpui::test]
fn a_truncated_sessions_store_is_left_as_it_is_with_a_toast(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f8-truncated");
    let path = state.2.join(crate::db::json_import::SESSIONS_LEGACY);
    let damaged = br#"{"s-1":{"name":"My session","pinn"#;
    std::fs::write(&path, damaged).expect("seed");
    let _ = crate::store::take_problems();
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.raise_store_problems(cx)));
    let titles: Vec<String> = vc.update(|_, cx| {
        baaz.read(cx).overlays.read(cx).toasts.iter().map(|toast| toast.title.to_string()).collect()
    });
    assert_eq!(titles, vec![format!("{} could not be read", crate::db::json_import::SESSIONS_LEGACY)]);
    assert_eq!(std::fs::read(&path).unwrap(), damaged, "the damaged file stays exactly as it was");
    // A write after boot goes to the database, never to the file.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.set_override("s-2", |meta| meta.pinned = true, cx));
    });
    vc.run_until_parked();
    crate::sessions::flush();
    assert_eq!(std::fs::read(&path).unwrap(), damaged);
    assert!(crate::sessions::read().get("s-2").is_some_and(|meta| meta.pinned), "the edit is in the database");
    let aside = std::fs::read_dir(&state.2)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".corrupt-"))
        .count();
    assert_eq!(aside, 0, "nothing was set aside");
    lane_restore(state);
}

/// docs/81 A F9: a store that keeps failing to write raises one toast,
/// not one per edit.
#[gpui::test]
fn repeated_write_failures_raise_one_toast(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f9-write-fails");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let blocked = state.2.join("blocked").join("layout.json");
    std::fs::create_dir_all(blocked.join("inside")).expect("a directory where the file goes");
    let _ = crate::store::take_problems();
    for _ in 0..3 {
        assert!(crate::store::write_authored(&blocked, b"{}").is_err());
        vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.raise_store_problems(cx)));
    }
    let titles: Vec<String> = vc.update(|_, cx| {
        baaz.read(cx).overlays.read(cx).toasts.iter().map(|toast| toast.title.to_string()).collect()
    });
    assert_eq!(titles, vec!["Couldn't save layout.json".to_owned()]);
    lane_restore(state);
}

/// A plain view standing in for a browser chat, with its record marked.
fn chat_view(
    harness: &mut Harness,
    id: &str,
    window: &mut gpui::Window,
    cx: &mut Context<Harness>,
) -> Entity<SessionView> {
    let host = SessionHost {
        provider_id: ProviderId::Codex.as_str().to_owned(),
        workspace: harness.workspace(cx),
        overlays: harness.overlays.clone(),
        capture: crate::runtime::CaptureToken::default(),
        terminal_host: None,
    };
    harness.set_override(id, |meta| {
        meta.hidden = true;
        meta.browser_chat = true;
    }, cx);
    cx.new(|cx| SessionView::new(id.to_owned(), host, window, cx))
}

/// docs/81 A F5: a browser chat's events reach the window (a Dialog here;
/// the ledger, turn count and sign-out banner ride the same path).
#[gpui::test]
fn a_browser_chat_is_subscribed(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f5-chat-subscribed");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let chat = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let chat = chat_view(harness, "s-chat", window, cx);
            harness.set_browser_chat(chat.clone(), cx);
            chat
        })
    });
    vc.update(|_, cx| {
        chat.update(cx, |_, cx| {
            cx.emit(SessionEvent::Dialog { title: "From the chat".into(), detail: "d".into() });
        });
    });
    vc.run_until_parked();
    let title = vc.update(|_, cx| {
        baaz.read(cx).overlays.read(cx).dialog().map(|dialog| dialog.title.to_string())
    });
    assert_eq!(title.as_deref(), Some("From the chat"), "the chat's event reached on_session_event");
    lane_restore(state);
}

/// docs/81 A F7: asking again while the chat is busy replaces it on the
/// line but keeps the busy view alive (its child is not killed); an
/// idle replaced chat is just parked.
#[gpui::test]
fn a_replaced_busy_browser_chat_stays_alive(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f7-chat-replaced");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let busy = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let busy = chat_view(harness, "s-chat-busy", window, cx);
            busy.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, cx| rt.fold_provider_events(
                    vec![provider::ProviderEvent::TurnRunning {
                        session_id: "s-chat-busy".into(),
                        turn_id: "t-1".into(),
                    }],
                    cx,
                ));
            });
            harness.set_browser_chat(busy.clone(), cx);
            let next = chat_view(harness, "s-chat-next", window, cx);
            harness.set_browser_chat(next, cx);
            busy
        })
    });
    let weak = busy.downgrade();
    drop(busy);
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(harness.browser_chat_id(cx).as_deref(), Some("s-chat-next"));
        assert!(harness.find_view("s-chat-busy", cx).is_some(), "the busy chat is still findable");
        assert_eq!(harness.registry.live(harness.active.as_ref()).len(), 2, "and still live, and subscribed with the new one");
    });
    assert!(weak.upgrade().is_some(), "the busy chat's view was not dropped");
    // Shown again from its folder: back on the line, out of the MRU.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| assert!(harness.show_browser_chat("s-chat-busy", cx)));
        let harness = baaz.read(cx);
        assert!(!harness.registry.is_parked("s-chat-busy"), "one holder at a time");
    });
    lane_restore(state);
}

/// docs/81 A F4: a Codex-style rename of a browser chat (provisional id
/// → minted thread id) moves its line state, used pages and reveal.
#[gpui::test]
fn a_renamed_browser_chat_keeps_its_line(cx: &mut gpui::TestAppContext) {
    use crate::app::agent_window::LineUi;
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f4-chat-rekey");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.browser.update(cx, |browser, _| browser.agent_window.line_ui = LineUi::Answer { question: "q".into(), session: "req-1".into() });
            harness.browser.update(cx, |browser, _| browser.agent_window.browser_chat_pages.insert("req-1".into(), vec![("Page".into(), "https://a.test".into())]));
            harness.sidebar_pane.update(cx, |pane, _| pane.reveal_unknown = Some(("req-1".into(), 3)));
            harness.rekey_session("req-1", "thread-9", cx);
            assert_eq!(harness.browser.read(cx).agent_window.line_ui, LineUi::Answer { question: "q".into(), session: "thread-9".into() });
            assert!(harness.browser.read(cx).agent_window.browser_chat_pages.contains_key("thread-9"));
            assert!(!harness.browser.read(cx).agent_window.browser_chat_pages.contains_key("req-1"));
            assert_eq!(harness.sidebar_pane.read(cx).reveal_unknown, Some(("thread-9".into(), 3)));
            harness.browser.update(cx, |browser, _| browser.agent_window.line_ui = LineUi::Drafting { session: "thread-9".into(), tab: 4 });
            harness.rekey_session("thread-9", "thread-10", cx);
            assert_eq!(harness.browser.read(cx).agent_window.line_ui, LineUi::Drafting { session: "thread-10".into(), tab: 4 });
        });
    });
    lane_restore(state);
}

/// docs/81 A F3: a parked session finishing a turn this window ran lands
/// its own byline and title; the open session's row is untouched (the
/// helpers used to read `self.active`).
#[gpui::test]
fn a_parked_finish_records_its_own_byline(cx: &mut gpui::TestAppContext) {
    use aui_protocol::{Block, Session, Turn, TurnMeta};
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f3-parked-byline");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let open = |vc: &mut gpui::VisualTestContext| {
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace(cx);
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| baaz.read(cx).active.clone().expect("a lane opened"))
    };
    let fill = |view: &Entity<SessionView>, ask: &str, reply: &str, cx: &mut gpui::App| {
        let id = view.read(cx).rt(cx).session_id.clone();
        let mut session = Session::new(id, aui_protocol::Provider::Claude, "m", "/tmp");
        session.turns = vec![
            Turn::User { id: "u1".into(), text: ask.into(), attachments: Vec::new(), mentions: Vec::new(), timestamp: None },
            Turn::Assistant {
                id: "a1".into(),
                blocks: vec![Block::Text { text: reply.into(), streaming: false }],
                meta: TurnMeta::default(),
                timestamp: None,
            },
        ];
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_fixture(session, cx)));
    };
    let first = open(vc);
    let second = open(vc);
    let (first_id, second_id) =
        vc.update(|_, cx| (first.read(cx).rt(cx).session_id.clone(), second.read(cx).rt(cx).session_id.clone()));
    assert_ne!(first_id, second_id);
    vc.update(|_, cx| {
        fill(&first, "Parked ask about tests", "Parked reply: all tests pass now.", cx);
        fill(&second, "Open ask about docs", "Open reply: the docs are rewritten.", cx);
        baaz.update(cx, |harness, _| {
            harness.open_state.turns_admitted.insert(first_id.clone());
        });
        first.update(cx, |_, cx| {
            cx.emit(SessionEvent::ProviderTurnFinished {
                session_id: first_id.clone(),
                turn_id: "a1".into(),
                meta: aui_protocol::TurnMeta::default(),
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let parked = harness.session_list.read(cx).overrides.get(&first_id);
        assert!(
            parked.and_then(|m| m.last_ask.as_deref()).is_some_and(|ask| ask.contains("Parked ask")),
            "the parked session's row gets its own byline: {:?}",
            parked.and_then(|m| m.last_ask.as_deref())
        );
        assert!(
            parked.and_then(|m| m.derived_title.as_deref()).is_some_and(|t| t.contains("Parked ask")),
            "the parked session is titled from its own transcript"
        );
        let open = harness.session_list.read(cx).overrides.get(&second_id);
        assert!(open.and_then(|m| m.last_ask.as_deref()).is_none(), "the open session's byline is untouched");
        assert!(open.and_then(|m| m.derived_title.as_deref()).is_none(), "the open session's title is untouched");
    });
    lane_restore(state);
}

/// docs/85 10.4: a compaction settling on an unfocused session is nobody's
/// turn — no byline, no title, no counted turn, no unread mark, so nothing
/// a notification could announce — while a person's turn settling the
/// same way lands all of those (`a_parked_finish_records_its_own_byline`).
#[gpui::test]
fn a_parked_compaction_lands_no_byline_count_or_notification(cx: &mut gpui::TestAppContext) {
    use aui_protocol::{Block, MarkerKind, Session, Turn, TurnMeta};
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("s10-parked-compaction");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let open = |vc: &mut gpui::VisualTestContext| {
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                let workspace = harness.workspace(cx);
                harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| baaz.read(cx).active.clone().expect("a lane opened"))
    };
    let parked = open(vc);
    let _front = open(vc);
    let parked_id = vc.update(|_, cx| parked.read(cx).rt(cx).session_id.clone());
    vc.update(|_, cx| {
        let mut session = Session::new(parked_id.clone(), aui_protocol::Provider::Claude, "m", "/tmp");
        session.turns = vec![Turn::Assistant {
            id: "compaction-1".into(),
            blocks: vec![Block::Marker { kind: MarkerKind::ContextCompacted, text: "Context compacted \u{b7} 29k \u{2192} 1.9k".into() }],
            meta: TurnMeta { cost_usd: 0.07, ..TurnMeta::default() },
            timestamp: None,
        }];
        parked.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.load_fixture(session, cx)));
        baaz.update(cx, |harness, _| {
            harness.open_state.turns_admitted.insert(parked_id.clone());
        });
    });
    let (before, turns_before) = vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|row| row.id == parked_id).map(|row| row.session_state());
        (row, harness.session_list.read(cx).provider_sessions.get(&parked_id).map(|record| record.turns))
    });
    vc.update(|_, cx| {
        parked.update(cx, |_, cx| {
            cx.emit(SessionEvent::ProviderTurnFinished {
                session_id: parked_id.clone(),
                turn_id: "compaction-1".into(),
                meta: TurnMeta { cost_usd: 0.07, ..TurnMeta::default() },
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let meta = harness.session_list.read(cx).overrides.get(&parked_id);
        assert!(meta.and_then(|m| m.last_ask.as_deref()).is_none(), "no byline from a compaction");
        assert!(meta.and_then(|m| m.derived_title.as_deref()).is_none(), "no title from a compaction");
        assert_eq!(
            harness.session_list.read(cx).provider_sessions.get(&parked_id).map(|record| record.turns),
            turns_before,
            "a compaction counts no turn"
        );
        let row = harness.session_list.read(cx).sessions.iter().find(|row| row.id == parked_id).expect("the row");
        assert!(!row.unread, "no unread mark");
        assert_eq!(Some(row.session_state()), before, "the row's state does not move, so nothing is announced");
    });
    lane_restore(state);
}

/// F2/F17 (docs/81 A): the boot-time field subscriptions — the search
/// query, the API-key field — outlive a session activation. Before the
/// split, `activate` cleared them with the per-view ones, so typing in ⌘⇧F
/// stopped re-querying and Enter in the key field did nothing. (The
/// Projects and Commands queries are their palette's own fields since plan
/// 4.6, subscribed by the palette: `palette_arrows_move_the_selection_with_a_query_field`.)
#[gpui::test]
fn field_subscriptions_survive_opening_a_session(cx: &mut gpui::TestAppContext) {
    use gpui_kit::base::input::InputEvent;
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f2-field-subs");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    assert!(vc.update(|_, cx| baaz.read(cx).active.is_some()), "setup: a session is open");

    // Search: a keystroke re-queries (the epoch moves).
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.open_search(window, cx)));
    let before = vc.update(|_, cx| baaz.read(cx).search_epoch);
    vc.update(|_, cx| {
        let field = baaz.read(cx).search_query.clone();
        field.update(cx, |_, cx| cx.emit(InputEvent::Change));
    });
    assert_eq!(vc.update(|_, cx| baaz.read(cx).search_epoch), before + 1, "typing in search re-queries");

    // API key: Enter submits.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.set_login_state(aui::screens::LoginState::ApiKey { can_submit: true, error: None }, cx);
            let field = harness.login.read(cx).api_key_field();
            field.update(cx, |state, cx| state.set_value("sk-test-f2", window, cx));
            field.update(cx, |_, cx| cx.emit(InputEvent::PressEnter { secondary: false, shift: false }));
        });
    });
    assert_ne!(
        vc.update(|_, cx| baaz.read(cx).login.read(cx).state_name()),
        "apikey",
        "Enter in the key field submits"
    );
    lane_restore(state);
}

/// B3b: a parked session whose pending approval was answered with "Deny
/// and stop" (cancel) and whose turn then ends with no item echo no
/// longer reads Needs approval. Before the fix the parked events
/// reached no one, so the row below keeps the stale Needs approval.
#[gpui::test]
fn a_parked_cancelled_approval_stops_reading_needs_approval(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3b-park-approval");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone()
    });
    // A pending MCP elicitation tap: the row reads Needs approval while
    // the session is open — the active behaviour, unchanged by this fix.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut(&first).expect("the record").turns = 1);
            harness.merge_provider_rows(cx);
            let view = harness.active.clone().expect("the lane opened");
            view.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, cx| rt.inject_external_approval(
                    crate::providers::ExternalApproval {
                        id: "mcp-elicitation-0".into(),
                        session_id: rt.session_id.clone(),
                        provider: crate::providers::ProviderId::Codex,
                        options: provider::ApprovalOption::of_choices(&provider_codex::fold::codex_choices(false)),
                        headline: "Allow the baaz MCP server to run tool \"terminal_run\"?".into(),
                        reason: "Allow the baaz MCP server to run tool \"terminal_run\"?".into(),
                        stage_token: None,
                        decision_sent: None,
                    },
                    cx,
                ));
                cx.emit(SessionEvent::ProviderApprovalsChanged { session_id: view.rt(cx).session_id.clone() });
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == first).expect("the row");
        assert_eq!(
            row.row_status(crate::clock::now_local()).kind,
            aui::nav::RowStatusKind::NeedsApproval,
            "setup: the open session reads Needs approval"
        );
    });
    // Switching away parks the session with its pending tap.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert!(
            baaz.read(cx).registry.is_parked(&first),
            "the approval session parks instead of closing"
        );
    });
    // "Deny and stop": the Cancel press travels, and the server ends
    // the turn with no item echo for the card.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness
                .registry.parked(&first)
                .expect("the parked view");
            view.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, cx| rt.decide_external_approval(
                    "mcp-elicitation-0".to_owned(),
                    "decline",
                    None,
                    cx,
                ));
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness
            .registry.parked(&first)
            .expect("the parked view");
        assert_eq!(
            view.read(cx).rt(cx).row_pending(),
            (None, None),
            "the answered tap leaves nothing pending on the parked view"
        );
    });
    // The cancelled turn ends: the parked row must let go of the ask.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness
                .registry.parked(&first)
                .expect("the parked view");
            view.update(cx, |_, cx| {
                cx.emit(SessionEvent::ProviderTurnFinished {
                    session_id: first.clone(),
                    turn_id: "t-parked-1".into(),
                    meta: aui_protocol::TurnMeta::default(),
                });
            });
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == first).expect("the row");
        assert_eq!(
            row.row_status(crate::clock::now_local()).kind,
            aui::nav::RowStatusKind::Settled,
            "the answered, ended turn no longer reads Needs approval"
        );
    });
    lane_restore(state);
}

/// B3b lane-level: a provider view over a scripted child, with the
/// sender half of its lane channel held back for the test to drive.
fn b3b_lane_view(
    vc: &mut gpui::VisualTestContext,
    session_id: &str,
) -> (Entity<SessionView>, futures::channel::mpsc::UnboundedSender<provider::ProviderEvent>) {
    use provider::ProviderAdapter as _;
    let mut adapter = provider::scripted::ScriptedProvider::new();
    adapter.connect(&conn::connect_info()).expect("a scripted provider connects");
    let provider = provider::Provider::new(adapter);
    let (tx, rx) = futures::channel::mpsc::unbounded();
    let session_id = session_id.to_owned();
    let view = vc.update(|window, cx| {
        let host = crate::session::SessionHost {
            provider_id: "codex".to_owned(),
            workspace: "/tmp/b3b-lane".to_owned(),
            overlays: cx.new(|_| crate::overlays::Overlays::default()),
            capture: crate::runtime::CaptureToken::default(),
            terminal_host: None,
        };
        cx.new(|cx| SessionView::new_on_provider(session_id, provider, rx, host, window, cx))
    });
    (view, tx)
}

/// B3b: a late finish for an earlier turn never clears the turn still
/// in flight; the running turn's own finish does.
#[gpui::test]
fn an_earlier_turns_finish_leaves_the_running_turn_busy(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let vc = cx.add_empty_window();
    let (view, tx) = b3b_lane_view(vc, "s-b3b-1");
    let start = |id: &str| aui_protocol::Delta::TurnStarted {
        turn: aui_protocol::Turn::Assistant {
            id: id.into(),
            blocks: Vec::new(),
            meta: aui_protocol::TurnMeta::default(),
            timestamp: None,
        },
    };
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-1".into()),
            deltas: vec![start("a-1"), start("a-2")],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    vc.update(|_, cx| assert!(view.read(cx).rt(cx).busy(), "the open turn reads busy"));
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-1".into()),
            deltas: vec![aui_protocol::Delta::TurnFinished {
                turn_id: "a-1".into(),
                meta: aui_protocol::TurnMeta::default(),
            }],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    vc.update(|_, cx| assert!(view.read(cx).rt(cx).busy(), "a-2 is still in flight after a-1's finish"));
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-1".into()),
            deltas: vec![aui_protocol::Delta::TurnFinished {
                turn_id: "a-2".into(),
                meta: aui_protocol::TurnMeta::default(),
            }],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    vc.update(|_, cx| assert!(!view.read(cx).rt(cx).busy(), "the running turn's own finish stands the lane down"));
}

/// B3b: a finished turn's still-Pending card settles for row purposes.
/// The fold moves a card off Pending only on a server echo, and a
/// cancelled turn can end with no echo; before the fix `row_pending`
/// below kept returning the dead card's command.
#[gpui::test]
fn a_finished_turns_pending_card_settles_for_row_purposes(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let vc = cx.add_empty_window();
    let (view, tx) = b3b_lane_view(vc, "s-b3b-2");
    let open = vec![
        aui_protocol::Delta::TurnStarted {
            turn: aui_protocol::Turn::Assistant {
                id: "a-2".into(),
                blocks: Vec::new(),
                meta: aui_protocol::TurnMeta::default(),
                timestamp: None,
            },
        },
        aui_protocol::Delta::BlockAdded {
            turn_id: "a-2".into(),
            block: aui_protocol::Block::approval(
                "ap-1",
                "Bash",
                "rm -rf /tmp/probe",
                "the agent wants it",
                "/tmp/b3b-lane",
                Vec::new(),
                aui_protocol::ApprovalScope::ThisCommand,
                aui_protocol::ApprovalState::Pending,
                None,
            ),
        },
    ];
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas { session_id: Some("s-b3b-2".into()), deltas: open })
            .expect("the lane channel is open");
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert_eq!(
            view.read(cx).rt(cx).row_pending(),
            (Some("rm -rf /tmp/probe".to_owned()), None),
            "the waiting card stands on the row while its turn runs"
        );
    });
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-2".into()),
            deltas: vec![aui_protocol::Delta::TurnFinished {
                turn_id: "a-2".into(),
                meta: aui_protocol::TurnMeta::default(),
            }],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert_eq!(
            view.read(cx).rt(cx).row_pending(),
            (None, None),
            "a turn that is over settles its cards for the row"
        );
    });
}

/// B3b: a removed turn refreshes the row. Before the fix `TurnRemoved`
/// emitted nothing, so the refresh below never arrived.
#[gpui::test]
fn a_removed_turn_refreshes_the_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let vc = cx.add_empty_window();
    let (view, tx) = b3b_lane_view(vc, "s-b3b-3");
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let seen_in = seen.clone();
    let _sub = vc.update(|_, cx| {
        cx.subscribe(&view.read(cx).runtime().clone(), move |_, event: &SessionEvent, _| {
            if matches!(event, SessionEvent::ProviderApprovalsChanged { .. }) {
                seen_in.borrow_mut().push("approvals".to_owned());
            }
        })
    });
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-3".into()),
            deltas: vec![aui_protocol::Delta::TurnStarted {
                turn: aui_protocol::Turn::User {
                    id: "u-1".into(),
                    text: "hello".into(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    timestamp: None,
                },
            }],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    assert!(seen.borrow().is_empty(), "an echo alone refreshes nothing");
    vc.update(|_, _| {
        tx.unbounded_send(provider::ProviderEvent::Deltas {
            session_id: Some("s-b3b-3".into()),
            deltas: vec![aui_protocol::Delta::TurnRemoved { turn_id: "u-1".into() }],
        })
        .expect("the lane channel is open");
    });
    vc.run_until_parked();
    assert_eq!(
        seen.borrow().as_slice(),
        ["approvals".to_owned()],
        "the removed turn refreshes the row once"
    );
}

/// The W5 delete arm: forgetting a provider session drops its record,
/// its row and its views together — and dropping the views hangs the
/// child up, so nothing is orphaned.
#[gpui::test]
fn deleting_a_provider_session_forgets_it_entirely(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("delete");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let open_id = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone()
    });
    // The deleted view stays alive in this local until teardown
    // (lanes bridge synchronously in tests, see `conn::gate_sync`);
    // the harness-side removal below is what this proves, and the
    // lane tests prove a dropped view shuts its child down.
    let held = vc.update(|_, cx| baaz.read(cx).active.clone());
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.delete_provider_session(&open_id, cx));
    });
    // The store is written off the UI thread now (docs/70): let the
    // background write land before reading the file.
    vc.run_until_parked();
    let _held = held;
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(!harness.session_list.read(cx).provider_sessions.contains_key(&open_id), "the record is gone");
        assert!(
            !harness.session_list.read(cx).sessions.iter().any(|entry| entry.id == open_id),
            "the row is gone"
        );
        assert!(harness.active.is_none(), "the open view is gone");
        assert!(
            !harness.registry.is_parked(&open_id),
            "no parked view survives to reopen it"
        );
        assert!(
            !crate::provider_sessions::read().contains_key(&open_id),
            "the database forgets it too"
        );
    });
    lane_restore(state);
}

/// D-8: forgetting a provider session also removes what Baaz kept for it —
/// its sent pictures (and their pre-database index) and its hand-off
/// snapshot — and offers its transcript in Baaz's own home for the Trash
/// through a dialog: the decision only. The dialog is never confirmed here,
/// so nothing reaches the Trash and the transcript stays.
#[gpui::test]
fn forgetting_a_session_clears_its_files_and_offers_its_transcript_for_the_trash(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("forget-files");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let id = "c0de0000-0000-4000-8000-00000000f0f0";
    let kept: Vec<std::path::PathBuf> = ["sent/x/a.png", "handoff/x.json"]
        .iter()
        .map(|rel| state.2.join(rel))
        .collect();
    let mine = [
        state.2.join(format!("sent/{id}/a1.png")),
        state.2.join(format!("sent/{id}.json")),
        state.2.join(format!("handoff/{id}.json")),
    ];
    let transcript = state.2.join(format!("codex-home/sessions/2026/11/19/rollout-2026-11-19T09-00-00-{id}.jsonl"));
    for path in mine.iter().chain(kept.iter()).chain(std::iter::once(&transcript)) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                crate::provider_sessions::upsert_open(
                    &mut list.provider_sessions,
                    &mut list.overrides,
                    "codex",
                    id,
                    Some("/w".into()),
                    None,
                    Some("Doomed".into()),
                )
            });
            harness.persist_provider_sessions(cx);
            harness.delete_provider_session(id, cx);
        });
    });
    vc.run_until_parked();
    crate::db::flush();
    for path in &mine {
        assert!(!path.exists(), "{} goes with the session", path.display());
    }
    for path in &kept {
        assert!(path.exists(), "{} belongs to another session", path.display());
    }
    assert!(transcript.exists(), "the transcript is only offered, never removed unasked");
    vc.update(|_, cx| {
        let dialog = baaz.read(cx).overlays.read(cx).dialog().cloned().expect("the Trash is offered");
        assert_eq!(dialog.action, crate::overlays::DialogAction::TrashTranscript);
        assert_eq!(dialog.primary, "Move to Trash");
        assert_eq!(dialog.archive_target.as_deref(), Some(transcript.to_string_lossy().as_ref()));
        assert!(!dialog.title.contains("Codex") && !dialog.detail.contains("Codex"), "no provider name in the copy");
    });
    assert!(!crate::provider_sessions::read().contains_key(id));
    assert!(!crate::sessions::read().contains_key(id));
    lane_restore(state);
}


/// Open a Codex lane over the quiet double and return its id, runtime and
/// view: the session the no-view tests below drive by folding the lane's
/// events into the runtime, as the drain task would.
fn open_quiet_lane(
    vc: &mut gpui::VisualTestContext,
    baaz: &gpui::Entity<Harness>,
) -> (String, gpui::Entity<crate::session::SessionRuntime>, gpui::Entity<SessionView>) {
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane opened");
        let runtime = view.read(cx).runtime().clone();
        (runtime.read(cx).session_id.clone(), runtime, view)
    })
}

/// One user turn and its assistant turn, as a lane's deltas.
fn exchange(user: &str, assistant: &str, text: &str) -> Vec<aui_protocol::Delta> {
    vec![
        aui_protocol::Delta::TurnStarted {
            turn: aui_protocol::Turn::User {
                id: user.into(),
                text: format!("ask {user}"),
                attachments: Vec::new(),
                mentions: Vec::new(),
                timestamp: None,
            },
        },
        aui_protocol::Delta::TurnStarted {
            turn: aui_protocol::Turn::Assistant {
                id: assistant.into(),
                blocks: Vec::new(),
                meta: aui_protocol::TurnMeta::default(),
                timestamp: None,
            },
        },
        aui_protocol::Delta::BlockAdded {
            turn_id: assistant.into(),
            block: aui_protocol::Block::Text { text: text.into(), streaming: false },
        },
    ]
}

/// A turn's finish with a cost the ledger keeps.
fn finished(turn_id: &str) -> aui_protocol::Delta {
    aui_protocol::Delta::TurnFinished {
        turn_id: turn_id.into(),
        meta: aui_protocol::TurnMeta { model: "codex-mini".into(), cost_usd: 0.02, ..Default::default() },
    }
}

/// docs/82 4.8 (audit B S-9, A F1): a session runs to completion with no
/// view. A turn starts; the session parks; the cap drops its view (the
/// runtime has work, so it stays); the turn finishes on the runtime alone.
/// The row settles, the ledger gains its row and the store holds the turn,
/// with nothing drawing it. Reopening builds a view over the same runtime,
/// which shows the finished turn.
#[gpui::test]
fn a_session_runs_to_completion_with_no_view(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("no-view-turn");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let (first, runtime, view) = open_quiet_lane(vc, &baaz);
    let weak_view = view.downgrade();
    drop(view);
    // The turn starts: the record counts history, the row reads Working.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut(&first).expect("the record").turns = 1);
            harness.merge_provider_rows(cx);
            harness.session_list.update(cx, |list, _| {
                let row = list.sessions.iter_mut().find(|entry| entry.id == first).expect("the row");
                row.running = true;
                row.turn_started = Some(crate::clock::now_local());
            });
        });
        let mut deltas = exchange("u-1", "a-1", "Working on it");
        deltas.truncate(2);
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(
                vec![
                    provider::ProviderEvent::Deltas { session_id: Some(first.clone()), deltas },
                    provider::ProviderEvent::TurnRunning { session_id: first.clone(), turn_id: "a-1".into() },
                ],
                cx,
            )
        });
        assert!(runtime.read(cx).is_sending(), "setup: the turn runs");
    });
    // Another session opens: the first parks. Then the cap drops its view.
    let _second = open_quiet_lane(vc, &baaz);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(harness.registry.is_parked(&first), "setup: the first session parked");
            let gone = harness.registry.evict_idle(&std::collections::HashSet::new(), 0, cx);
            assert!(gone.iter().all(|(id, _)| *id != first), "a running runtime is never evicted");
            assert!(harness.registry.parked(&first).is_none(), "its view went");
            assert!(harness.registry.is_parked(&first), "its runtime stays");
            harness.resubscribe_live_views(cx);
        });
    });
    vc.run_until_parked();
    assert!(weak_view.upgrade().is_none(), "nothing draws the session any more");
    // The turn finishes on the runtime alone.
    vc.update(|_, cx| {
        let mut deltas = exchange("u-1", "a-1", "Done with no view").split_off(2);
        deltas.push(finished("a-1"));
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(first.clone()), deltas }], cx)
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == first).expect("the row");
        assert!(!row.running, "the view-less finish stands the row down");
        assert_eq!(row.row_status(crate::clock::now_local()).kind, aui::nav::RowStatusKind::Settled);
        let rt = runtime.read(cx);
        assert!(!rt.is_sending(), "the runtime settled");
        assert!(rt.provider_turn_finished("a-1"), "the runtime saw the turn finish");
        assert!(
            rt.session().is_some_and(|session| session.turns.iter().any(|turn| turn.id() == "a-1")),
            "the store holds the turn"
        );
        assert!(crate::usage::find_recorded(&first, "a-1").is_some(), "the ledger got its row");
    });
    // Reopen: a new view over the same runtime shows the finished turn.
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet(first.clone(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the session reopened");
        assert_eq!(*view.read(cx).runtime(), runtime, "the reopen draws the runtime that ran the turn");
        view.update(cx, |view, cx| view.refresh_render_cache(cx));
        assert_eq!(view.read(cx).last_answer_text(), "Done with no view", "the new view shows the finished turn");
    });
    lane_restore(state);
}

/// docs/81 A F14: a reopen's replay of N finished turns is one
/// `HistoryReplayed`, and the root's settle runs once for it — one seed
/// read, one batch of ledger rows, one row rejoin — where it used to run
/// N times. A live turn after it still settles on its own.
#[gpui::test]
fn reopening_n_turns_emits_one_history_replayed_and_settles_once(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f14-replay");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let (id, runtime, _view) = open_quiet_lane(vc, &baaz);
    let replays: std::rc::Rc<std::cell::RefCell<Vec<usize>>> = Default::default();
    let finishes = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let _sub = vc.update(|_, cx| {
        let (replays, finishes) = (replays.clone(), finishes.clone());
        cx.subscribe(&runtime, move |_, event: &SessionEvent, _| match event {
            SessionEvent::HistoryReplayed { turns, .. } => replays.borrow_mut().push(turns.len()),
            SessionEvent::ProviderTurnFinished { .. } => finishes.set(finishes.get() + 1),
            _ => {}
        })
    });
    let settles = || crate::app::sessions::events::SETTLES.with(std::cell::Cell::get);
    let before = settles();
    // The resume's replay: three finished exchanges in one batch, as
    // Claude Code and Codex replay a stored transcript.
    vc.update(|_, cx| {
        let mut deltas = Vec::new();
        for n in 1..=3 {
            deltas.extend(exchange(&format!("u-{n}"), &format!("a-{n}"), &format!("Reply {n}")));
            deltas.push(finished(&format!("a-{n}")));
        }
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(id.clone()), deltas }], cx)
        });
    });
    vc.run_until_parked();
    assert_eq!(*replays.borrow(), vec![3], "one HistoryReplayed carries the three turns");
    assert_eq!(finishes.get(), 0, "no per-turn settle for replayed history");
    assert_eq!(settles() - before, 1, "the root's settle ran once");
    vc.update(|_, cx| {
        assert_eq!(crate::usage::count_recorded(&id), 3, "every replayed turn has its ledger row");
        let record = baaz.read(cx).session_list.read(cx).provider_sessions.get(&id).map(|record| record.turns);
        assert_eq!(record, Some(3), "the record counts the three exchanges");
    });
    // A live turn afterwards settles on its own, as before.
    vc.update(|_, cx| {
        let mut deltas = exchange("u-4", "a-4", "Live reply");
        deltas.truncate(2);
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(
                vec![
                    provider::ProviderEvent::Deltas { session_id: Some(id.clone()), deltas },
                    provider::ProviderEvent::TurnRunning { session_id: id.clone(), turn_id: "a-4".into() },
                    provider::ProviderEvent::Deltas { session_id: Some(id.clone()), deltas: vec![finished("a-4")] },
                ],
                cx,
            )
        });
    });
    vc.run_until_parked();
    assert_eq!(finishes.get(), 1, "the live turn is a ProviderTurnFinished");
    assert_eq!(replays.borrow().len(), 1, "and no second replay");
    assert_eq!(settles() - before, 2, "one more settle, for the live turn");
    lane_restore(state);
}

/// docs/82 4.8 (D Q5 #2): a streamed entry update redraws its view and
/// never the root. The runtime folds the chunk, says which entry changed
/// (`EntryUpdated`), and the view takes it as one list operation; the root
/// hears nothing — no `SessionEvent`, no notify.
#[gpui::test]
fn a_streamed_entry_redraws_its_view_and_never_the_root(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("stream-redraw");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let (id, runtime, view) = open_quiet_lane(vc, &baaz);
    // A reply has started: the entry the chunk will grow.
    vc.update(|_, cx| {
        let deltas = exchange("u-1", "a-1", "The first words");
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(vec![provider::ProviderEvent::Deltas { session_id: Some(id.clone()), deltas }], cx)
        });
    });
    vc.run_until_parked();
    let root_notifies = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let view_notifies = std::rc::Rc::new(std::cell::Cell::new(0u32));
    let entry_events: std::rc::Rc<std::cell::RefCell<Vec<crate::session::RuntimeEvent>>> = Default::default();
    let _watch = vc.update(|_, cx| {
        let (root, drawn, events) = (root_notifies.clone(), view_notifies.clone(), entry_events.clone());
        (
            cx.observe(&baaz, move |_, _| root.set(root.get() + 1)),
            cx.observe(&view, move |_, _| drawn.set(drawn.get() + 1)),
            cx.subscribe(&runtime, move |_, event: &crate::session::RuntimeEvent, _| events.borrow_mut().push(event.clone())),
        )
    });
    // The streamed chunk.
    vc.update(|_, cx| {
        runtime.update(cx, |rt, cx| {
            rt.fold_provider_events(
                vec![provider::ProviderEvent::Deltas {
                    session_id: Some(id.clone()),
                    deltas: vec![aui_protocol::Delta::TextDelta {
                        turn_id: "a-1".into(),
                        block_index: 0,
                        text: " and then some".into(),
                    }],
                }],
                cx,
            )
        });
    });
    vc.run_until_parked();
    assert_eq!(
        *entry_events.borrow(),
        vec![crate::session::RuntimeEvent::EntryUpdated(1)],
        "the runtime names the one entry that changed"
    );
    assert!(view_notifies.get() > 0, "the view redraws");
    assert_eq!(root_notifies.get(), 0, "the root is never notified");
    vc.update(|_, cx| {
        view.update(cx, |view, cx| view.refresh_render_cache(cx));
        assert_eq!(view.read(cx).last_answer_text(), "The first words and then some", "the view draws the chunk");
    });
    lane_restore(state);
}

/// docs/82 6.5: a session whose agent's terminal task still runs carries
/// the count on its row, and the row's "N running" opens that session with
/// the right pane on its Tasks.
#[gpui::test]
fn a_rows_running_tasks_open_that_session_on_its_tasks(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("tasks-row");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&parking_factory());
            mark_muse_up(harness);
        });
    });
    let (first, _, _) = open_quiet_lane(vc, &baaz);
    // A session that has talked (an unsent draft goes when another opens).
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut(&first).expect("the record").turns = 1);
            harness.merge_provider_rows(cx);
        });
    });
    let (second, _, _) = open_quiet_lane(vc, &baaz);
    assert_ne!(first, second);
    // The first session's agent left a dev server running.
    vc.update(|_, cx| {
        let host = baaz.read(cx).terminal_host(cx);
        host.update(cx, |host, _| {
            host.tasks.record(&first, "pnpm dev", "t9", 0);
        });
    });
    vc.update(|_, cx| {
        let rows = baaz.read(cx).visible_sessions(cx);
        let count = |id: &str| rows.iter().find(|entry| entry.id == id).map(|entry| entry.tasks_running);
        assert_eq!(count(&first), Some(1), "the row counts its running task");
        assert_eq!(count(&second).unwrap_or(0), 0, "the other row says nothing");
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.sidebar_intent(aui::screens::sidebar::SidebarIntent::RowAction(first.clone().into(), aui::nav::RowAction::Tasks), window, cx)
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert_eq!(baaz.read(cx).active_id(cx).as_deref(), Some(first.as_str()), "the click opens its session");
        let layout = crate::layout::Layout::get(cx);
        assert!(layout.right_open, "the pane opens");
        assert_eq!(layout.right_kind, Some(crate::layout::RightKind::Tasks), "on Tasks");
    });
    lane_restore(state);
}

/// `child-lost-twice` (gate 6): a live session whose child dies twice in
/// a row turns its sidebar row `Failed` as the second loss gives up —
/// the same mark a relaunch on the stored state draws.
#[gpui::test]
fn a_child_lost_twice_turns_the_live_row_failed(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("lost-twice-row");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("the lane is open"));
    vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.step_send("hello lost probe", cx))))
        .expect("the send lands");
    vc.run_until_parked();
    // The scripted turn runs to its end, as the capture's `wait:` lets it.
    vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.test_drain_provider(cx))));
    vc.run_until_parked();
    let session_id = vc.update(|_, cx| view.read(cx).rt(cx).session_id.clone());
    // The row the sidebar draws (the cached visible list), and the row
    // storage holds — what a relaunch on the stored state rebuilds from.
    let drawn = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| {
            baaz.read(cx)
                .visible_sessions(cx)
                .iter()
                .find(|entry| entry.id == session_id)
                .map(|entry| (entry.session_state(), entry.state()))
        })
    };
    let stored = |cx: &mut gpui::VisualTestContext| {
        cx.update(|_, cx| {
            baaz.read(cx)
                .session_list
                .read(cx)
                .sessions
                .iter()
                .find(|entry| entry.id == session_id)
                .map(|entry| (entry.session_state(), entry.state()))
        })
    };
    // Drawn once before the losses, as the sidebar has by then.
    assert!(drawn(vc).is_some(), "the sent session has a drawn row");
    for _ in 0..2 {
        vc.update(|_, cx| view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.step_child_lost(cx))))
            .expect("the child loss lands");
        vc.run_until_parked();
    }
    vc.update(|_, cx| {
        let error = baaz.read(cx).session_list.read(cx).overrides.get(&session_id).and_then(|meta| meta.last_error.clone());
        assert!(error.is_some(), "the failure is stored");
    });
    let failed = Some((crate::sidebar::SessionState::Failed, aui_tokens::AgentState::Failed));
    assert_eq!(stored(vc), failed, "the stored row reads Failed");
    assert_eq!(drawn(vc), failed, "the drawn row reads Failed live, as a relaunched one does");
    lane_restore(state);
}
