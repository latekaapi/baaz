//! Tests for handoff chains (B3, Y2a3, X3d) and search across providers.

use super::*;

/// Seeds the 2-hop mixed chain every X3b test drives: claude-code (A) →
/// codex (B) → muse (C, head). Lane halves in the provider store, the
/// muse destination's half in the overrides — what activation writes,
/// including the carried chain title (Y2a).
fn seed_mixed_chain(harness: &mut Harness, cx: &mut gpui::App) {
    use std::collections::HashMap;
    let record = |id: &str,
                  provider: &str,
                  turns: u64,
                  prompt: Option<&str>,
                  to: Option<&str>,
                  from: Option<(&str, &str)>| {
        crate::provider_sessions::ProviderSessionRecord {
            provider: provider.to_owned(),
            session_id: id.to_owned(),
            workspace: None,
            project: None,
            created_ms: 1_700_000_000_000,
            updated_ms: 1_700_000_000_000,
            turns,
            title: None,
            first_prompt: prompt.map(str::to_owned),
            handoff_to: to.map(str::to_owned),
            handoff_from: from.map(|(source, _)| source.to_owned()),
            handoff_from_provider: from.map(|(_, provider)| provider.to_owned()),
        handoff_title: None,
            display_texts: HashMap::new(),
            origin: None,
        }
    };
    harness.session_list.update(cx, |list, _| {
        list.provider_sessions.insert(
            "chain-a".into(),
            record("chain-a", "claude-code", 2, Some("Chart the Greyport ferry routes"), Some("chain-b"), None),
        );
    });
    harness.session_list.update(cx, |list, _| {
        list.provider_sessions.insert(
            "chain-b".into(),
            record("chain-b", "codex", 3, Some("Second leg"), Some("chain-c"), Some(("chain-a", "claude-code"))),
        );
    });
    // Activation carries the source's display title onto every
    // destination down the chain — the head reads it, not its own
    // words ("Later words" below stays the stored label).
    harness.session_list.update(cx, |list, _| {
        if let Some(record) = list.provider_sessions.get_mut("chain-b") {
            record.handoff_title = Some("Chart the Greyport ferry routes".into());
        }
    });
    harness.session_list.update(cx, |list, _| {
        list.overrides.insert(
            "chain-c".into(),
            crate::sessions::SessionMeta {
                handoff_from: Some("chain-b".into()),
                handoff_from_provider: Some("codex".into()),
                handoff_title: Some("Chart the Greyport ferry routes".into()),
                ..Default::default()
            },
        );
    });
    // Activation order: the members first, the head appended, then the
    // provider rebuild (which also synthesises a muse-lane head row
    // from the stores when the wire has not listed it yet — skipped
    // here because the head row is already present). Storage keeps
    // every member row (Y2a); the one-row view derives in
    // `visible_sessions`. The head shows visible turns: the wire's 5
    // minus the hidden pack exchange.
    harness.session_list.update(cx, |list, _| {
        list.sessions.push(chain_muse_entry("chain-c", "Later words", sidebar::visible_turns(5, true)))
    });
    harness.merge_provider_rows(cx);
    harness.collapse_handoff_chains(cx);
}

/// A checkpointed handoff run with a two-turn pack, plus the pending
/// open carrying the source's turns — what `start_handoff` leaves
/// behind once the destination opens.
fn seed_b3_handoff(harness: &mut Harness, source: &str) {
    use aui_protocol::{Block as B, Turn as T};
    let user = || T::User {
        id: "b3-u1".to_owned(),
        text: "Chart the ferry routes".to_owned(),
        attachments: vec![],
        mentions: vec![],
        timestamp: None,
    };
    let assistant = || T::Assistant {
        id: "b3-a1".to_owned(),
        blocks: vec![B::Text {
            text: "Three routes: Cormorant, Heron, Gull.".to_owned(),
            streaming: false,
        }],
        meta: Default::default(),
        timestamp: None,
    };
    let mut session =
        aui_protocol::Session::new(source, aui_protocol::Provider::Claude, "m", "/tmp/ws");
    session.turns.push(user());
    session.turns.push(assistant());
    let pack = crate::handoff::build_pack(&session, "/tmp/ws");
    let mut run = crate::handoff::HandoffRun::request(
        source.to_owned(),
        1,
        ProviderId::ClaudeCode,
        ProviderId::Codex,
        "claude".to_owned(),
        String::new(),
        false,
        false,
        false,
    )
    .expect("fresh request starts");
    run.note_quiescing();
    run.note_checkpointed(pack);
    harness.handoff_state.runs.insert(source.to_owned(), run);
    harness.handoff_state.pending = Some(crate::handoff::PendingHandoff {
        source_session: source.to_owned(),
        epoch: 1,
        prefix: vec![user(), assistant()],
    });
}

/// A scripted Codex lane with a seeded handoff waiting to land on it:
/// returns the destination session id.
fn open_b3_destination(
    vc: &mut gpui::VisualTestContext,
    baaz: &gpui::Entity<Harness>,
) -> String {
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                crate::provider_sessions::upsert_open(
                    &mut list.provider_sessions,
                    "claude-code",
                    "b3-src",
                    Some("/tmp/ws".to_owned()),
                    None,
                    Some("Chart the ferry routes".to_owned()),
                );
            });
            harness.merge_provider_rows(cx);
            seed_b3_handoff(harness, "b3-src");
            harness.active.clone().expect("the lane is open").read(cx).session_id.clone()
        })
    })
}

/// B3: landing shows the source prefix before the pack submits — the
/// destination renders prefix + divider from its first frame, and the
/// acknowledgement adds no second frame of its own.
#[gpui::test]
fn land_shows_the_prefix_before_the_pack_submits(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3-land");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let dest = open_b3_destination(vc, &baaz);
    // Landing itself, before the pack's submit round-trips.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.land_handoff_destination(dest.clone(), cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Prepared),
                "landing marks Prepared, state is {:?}",
                run.state
            );
            assert!(harness.handoff_state.pending.is_none(), "landing consumes the pending open");
            let view = harness.active.clone().expect("the destination");
            assert!(
                view.read(cx).has_handoff_prefix(),
                "the prefix lands before the pack submits"
            );
            assert!(
                sidebar::is_handoff_dest(&dest, &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the links land with the prefix, in the same frame"
            );
        });
    });
    // The pack's acknowledgement: the run activates, and the frame
    // still reads prefix, divider, then the destination's own turns.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.acknowledge_handoff(dest.clone(), cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Activated),
                "the ack activates, state is {:?}",
                run.state
            );
            let view = harness.active.clone().expect("the destination");
            assert!(
                view.read(cx).has_handoff_prefix(),
                "the acknowledgement keeps the landed prefix"
            );
            let ids: Vec<String> = view.update(cx, |view, cx| {
                view.handoff_snapshot_turns(cx).iter().map(|turn| turn.id().to_owned()).collect()
            });
            assert_eq!(
                ids,
                vec![
                    "b3-u1".to_owned(),
                    "b3-a1".to_owned(),
                    format!("handoff-divider-{dest}"),
                ],
                "prefix, divider, then the destination's own turns (pack exchange hidden)"
            );
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3: the destination row joins the chain at land — one row for the
/// hop, headed at the destination, from the first frame.
#[gpui::test]
fn a_handoff_destination_row_joins_the_chain_at_land(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3-chain");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let dest = open_b3_destination(vc, &baaz);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.land_handoff_destination(dest.clone(), cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "source and destination, one row");
            assert_eq!(visible[0].id, dest, "the head is what is listed");
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3: no title side session ever starts for a handoff destination —
/// the land-time links make the guard hold before the pack turn
/// starts. `auto_title` stays on, so the guard does the work.
#[gpui::test]
fn a_handoff_destination_never_starts_a_title_side_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3-title");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let dest = open_b3_destination(vc, &baaz);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(crate::layout::Layout::get(cx).auto_title, "the switch is on: the guard does the work");
            harness.land_handoff_destination(dest.clone(), cx);
            harness.maybe_start_title(
                &dest,
                Some("Handed off from Claude Code: Chart the ferry routes".to_owned()),
                cx,
            );
            assert!(
                !harness.session_list.read(cx).titles_pending.contains(&dest),
                "a handoff destination earns no generated title"
            );
            assert!(harness.session_list.read(cx).side_sessions.is_empty(), "and starts no title side session");
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3: a pack turn that ends failed fails the handoff — `turn/started`
/// then `turn/completed{status: failed}` on the destination leaves the
/// run Failed with the turn's reason on the card and the row, links
/// rolled back, never a silent Active.
#[gpui::test]
fn a_pack_turn_that_fails_fails_the_handoff(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3-pack-fail");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let reason = "model `gpt-6-astra` does not exist or you lack access";
    let pack_full = "Continuing a session handed off from Claude Code. Context follows.\n\n## Original goal\nChart the ferry routes";
    // A muse destination with the pack submitted and acknowledged: the run is Active,
    // the row exists, the chain links are in place.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.open("dest-m".to_owned(), false, window, cx);
            let view = harness.active.clone().expect("the destination is open");
            view.update(cx, |view, cx| {
                view.note_handoff_pack(
                    pack_full.to_owned(),
                    "Handed off from Claude Code: Chart the ferry routes".to_owned(),
                    cx,
                );
            });
            let mut session =
                aui_protocol::Session::new("src-m", aui_protocol::Provider::Claude, "m", "/tmp/ws");
            session.turns.push(aui_protocol::Turn::User {
                id: "u1".to_owned(),
                text: "Chart the ferry routes".to_owned(),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            let pack = crate::handoff::build_pack(&session, "/tmp/ws");
            let mut run = crate::handoff::HandoffRun::request(
                "src-m".to_owned(),
                1,
                ProviderId::ClaudeCode,
                ProviderId::Muse,
                "claude".to_owned(),
                String::new(),
                false,
                false,
                false,
            )
            .expect("fresh request starts");
            run.note_quiescing();
            run.note_checkpointed(pack);
            run.note_prepared("dest-m".to_owned());
            // The real order: the pack is acknowledged when its turn
            // STARTS, so the run is already Active when the turn fails.
            assert!(run.acknowledge(1), "the pack ack lands");
            run.activate();
            harness.handoff_state.runs.insert("src-m".to_owned(), run);
            harness.session_list.update(cx, |list, _| {
                list.sessions.push(sidebar::local_started_row(
                    "dest-m",
                    "Chart the ferry routes".to_owned(),
                    None,
                    Some("/tmp/ws".to_owned()),
                    crate::clock::now_local(),
                ));
            });
            harness.set_override("src-m", |meta| meta.handoff_to = Some("dest-m".to_owned()), cx);
            harness.set_override("dest-m", |meta| {
                meta.handoff_from = Some("src-m".to_owned());
                meta.handoff_from_provider = Some("claude-code".to_owned());
                meta.handoff_title = Some("Chart the ferry routes".to_owned());
            }, cx);
        });
    });
    // turn/started, then turn/completed{status: failed} on the
    // destination — through the settled-turn hook the live wire
    // reaches after every muse turn.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination is open");
            let event = |method: &str, params: serde_json::Value| crate::session::wire_note(method, "dest-m", params);
            view.update(cx, |view, cx| {
                view.apply(
                    event("session/started", serde_json::json!({"session": {"sessionId": "dest-m"}})),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(event("turn/started", serde_json::json!({"turnId": "t-1"})), cx)
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "item/completed",
                        serde_json::json!({"item": {
                            "itemId": "m-1",
                            "turnId": "t-1",
                            "kind": "userMessage",
                            "status": "completed",
                            "revision": 1,
                            "text": pack_full,
                        }}),
                    ),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "turn/completed",
                        serde_json::json!({"turnId": "t-1", "terminal": "failed", "error": {
                            "kind": "modelError",
                            "message": reason,
                        }}),
                    ),
                    cx,
                )
            });
            let open = harness.active.clone().expect("the open view");
            harness.maybe_rewrite_byline(&open, cx);
            let run = harness.handoff_state.runs.get("src-m").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Failed { .. }),
                "a failed pack turn fails the handoff, state is {:?}",
                run.state
            );
            assert_eq!(run.reason.as_deref(), Some(reason));
            let entry =
                harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == "dest-m").expect("the row");
            let expected_row = format!("Hand-off failed: {reason}");
            assert_eq!(
                entry.last_error.as_deref(),
                Some(expected_row.as_str()),
                "the row names the handoff failure"
            );
            assert!(
                !sidebar::is_handoff_dest(
                    "dest-m",
                    &harness.session_list.read(cx).provider_sessions,
                    &harness.session_list.read(cx).overrides
                ),
                "the links roll back"
            );
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3fix: landing stands the summary wait down, and anything arriving
/// late lands on nothing — the summary step never sticks on Current
/// and the pack stays extractive. Driven through the app: the real
/// watchdog handler while waiting, land itself, the real ack, then a
/// late answer and the watchdog again after activation.
#[gpui::test]
fn a_late_summary_after_ack_leaves_the_extractive_pack(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3fix-late-summary");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let dest = open_b3_destination(vc, &baaz);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            // The checkpoint path armed the summary wait.
            let mut run = harness.handoff_state.runs.get("b3-src").cloned().expect("the run");
            run.note_summary_pending();
            harness.handoff_state.runs.insert("b3-src".to_owned(), run.clone());
            if let aui_protocol::Block::Handoff { carried, .. } = run.card() {
                assert_eq!(
                    carried[0].detail.as_deref(),
                    Some("extractive summary"),
                    "the wait lives on the summary step now, never the carried row"
                );
                assert!(
                    matches!(
                        run.steps()[1].state,
                        aui::transcript::HandoffStepState::Current
                    ),
                    "the summary step reads Current while waiting"
                );
            } else {
                panic!("not a handoff card");
            }
            // The watchdog while still waiting: the real handler keeps
            // the extractive text and stands the wait down.
            harness.handoff_summary_timeout("b3-src", 1, cx);
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(!run.summarising, "the watchdog stands the wait down");
            assert_eq!(run.pack.as_ref().map(|pack| pack.summary_kind), Some(crate::handoff::SummaryKind::Extractive));
            // Re-arm the wait the way a still-flying side session
            // leaves it, then land: landing stands it down, because
            // the submitted pack is already fixed.
            let mut run = run.clone();
            run.note_summary_pending();
            harness.handoff_state.runs.insert("b3-src".to_owned(), run);
            harness.land_handoff_destination(dest.clone(), cx);
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Prepared),
                "landing marks Prepared, state is {:?}",
                run.state
            );
            assert!(!run.summarising, "landing stands the summary wait down");
            assert_eq!(run.pack.as_ref().map(|pack| pack.summary_kind), Some(crate::handoff::SummaryKind::Extractive));
            if let aui_protocol::Block::Handoff { carried, .. } = run.card() {
                assert_eq!(
                    carried[0].detail.as_deref(),
                    Some("extractive summary"),
                    "the landed card names the extractive summary"
                );
            } else {
                panic!("not a handoff card");
            }
            // The ack activates; the card stays clean.
            harness.acknowledge_handoff(dest.clone(), cx);
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(matches!(run.state, aui_protocol::HandoffState::Activated));
            if let aui_protocol::Block::Handoff { carried, .. } = run.card() {
                assert_ne!(
                    carried[0].detail.as_deref(),
                    Some("Summarising…"),
                    "the settled card never reads Summarising…"
                );
            } else {
                panic!("not a handoff card");
            }
            // A summary answer landing after the ack finds no waiting
            // run, so it is dropped.
            assert!(!harness.handoff_summary_waiting("b3-src", 1), "a late answer lands on nothing");
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert_eq!(run.pack.as_ref().map(|pack| pack.summary_kind), Some(crate::handoff::SummaryKind::Extractive));
            // And the watchdog after activation is a no-op on the same
            // clean state.
            harness.handoff_summary_timeout("b3-src", 1, cx);
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(matches!(run.state, aui_protocol::HandoffState::Activated));
            assert!(!run.summarising);
            if let aui_protocol::Block::Handoff { carried, .. } = run.card() {
                assert_ne!(
                    carried[0].detail.as_deref(),
                    Some("Summarising…"),
                    "the watchdog after ack leaves the card clean"
                );
            } else {
                panic!("not a handoff card");
            }
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3fix: a pack reply that completes — even one the model answered —
/// never fails the handoff: reconciling finds nothing to fail, and
/// the ack activates with the chain intact. A later turn settling
/// failed after activation stays that turn's business.
#[gpui::test]
fn a_completed_pack_turn_activates_and_a_later_failure_stays_out(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3fix-pack-ok");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let pack_full = "Continuing a session handed off from Claude Code. Context follows.\n\n## Original goal\nChart the ferry routes";
    // A muse destination with the pack submitted: the run is Prepared,
    // the row exists, the chain links are in place.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.open("dest-m2".to_owned(), false, window, cx);
            let view = harness.active.clone().expect("the destination is open");
            view.update(cx, |view, cx| {
                view.note_handoff_pack(
                    pack_full.to_owned(),
                    "Handed off from Claude Code: Chart the ferry routes".to_owned(),
                    cx,
                );
            });
            let mut session =
                aui_protocol::Session::new("src-m2", aui_protocol::Provider::Claude, "m", "/tmp/ws");
            session.turns.push(aui_protocol::Turn::User {
                id: "u1".to_owned(),
                text: "Chart the ferry routes".to_owned(),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            let pack = crate::handoff::build_pack(&session, "/tmp/ws");
            let mut run = crate::handoff::HandoffRun::request(
                "src-m2".to_owned(),
                1,
                ProviderId::ClaudeCode,
                ProviderId::Muse,
                "claude".to_owned(),
                String::new(),
                false,
                false,
                false,
            )
            .expect("fresh request starts");
            run.note_quiescing();
            run.note_checkpointed(pack);
            run.note_prepared("dest-m2".to_owned());
            harness.handoff_state.runs.insert("src-m2".to_owned(), run);
            harness.session_list.update(cx, |list, _| {
                list.sessions.push(sidebar::local_started_row(
                    "dest-m2",
                    "Chart the ferry routes".to_owned(),
                    None,
                    Some("/tmp/ws".to_owned()),
                    crate::clock::now_local(),
                ));
            });
            harness.set_override("src-m2", |meta| meta.handoff_to = Some("dest-m2".to_owned()), cx);
            harness.set_override("dest-m2", |meta| {
                meta.handoff_from = Some("src-m2".to_owned());
                meta.handoff_from_provider = Some("claude-code".to_owned());
                meta.handoff_title = Some("Chart the ferry routes".to_owned());
            }, cx);
        });
    });
    // The pack turn completes cleanly: reconcile finds no failure.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination is open");
            let event = |method: &str, params: serde_json::Value| crate::session::wire_note(method, "dest-m2", params);
            view.update(cx, |view, cx| {
                view.apply(
                    event("session/started", serde_json::json!({"session": {"sessionId": "dest-m2"}})),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(event("turn/started", serde_json::json!({"turnId": "t-1"})), cx)
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "item/completed",
                        serde_json::json!({"item": {
                            "itemId": "m-1",
                            "turnId": "t-1",
                            "kind": "userMessage",
                            "status": "completed",
                            "revision": 1,
                            "text": pack_full,
                        }}),
                    ),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "item/completed",
                        serde_json::json!({"item": {
                            "itemId": "m-2",
                            "turnId": "t-1",
                            "kind": "agentMessage",
                            "status": "completed",
                            "revision": 1,
                            "text": "Got it — picking up the ferry routes.",
                        }}),
                    ),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event("turn/completed", serde_json::json!({"turnId": "t-1", "terminal": "completed"})),
                    cx,
                )
            });
            let open = harness.active.clone().expect("the open view");
            harness.maybe_rewrite_byline(&open, cx);
            let run = harness.handoff_state.runs.get("src-m2").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Prepared),
                "a completed pack turn fails nothing, state is {:?}",
                run.state
            );
            // The ack activates with the chain intact.
            harness.acknowledge_handoff("dest-m2".to_owned(), cx);
            let run = harness.handoff_state.runs.get("src-m2").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Activated),
                "the ack activates, state is {:?}",
                run.state
            );
            assert!(
                sidebar::is_handoff_dest("dest-m2", &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the links survive the ack"
            );
            // A later turn settling failed: the destination owns its
            // turns now, so the active handoff stays active and linked.
            view.update(cx, |view, cx| {
                view.apply(event("turn/started", serde_json::json!({"turnId": "t-2"})), cx)
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "item/completed",
                        serde_json::json!({"item": {
                            "itemId": "m-3",
                            "turnId": "t-2",
                            "kind": "userMessage",
                            "status": "completed",
                            "revision": 1,
                            "text": "Keep going",
                        }}),
                    ),
                    cx,
                )
            });
            view.update(cx, |view, cx| {
                view.apply(
                    event(
                        "turn/completed",
                        serde_json::json!({"turnId": "t-2", "terminal": "failed", "error": {
                            "kind": "modelError",
                            "message": "the model was overloaded",
                        }}),
                    ),
                    cx,
                )
            });
            let open = harness.active.clone().expect("the open view");
            harness.maybe_rewrite_byline(&open, cx);
            let run = harness.handoff_state.runs.get("src-m2").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Activated),
                "a later failed turn never fails an active handoff, state is {:?}",
                run.state
            );
            assert!(
                sidebar::is_handoff_dest("dest-m2", &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "a later failed turn never unlinks the chain"
            );
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3fix: the destination view closing before the ack fails the
/// still-Prepared run with a reason and rolls the chain links back —
/// no `HandoffPackFailed` needed, nothing lingers.
#[gpui::test]
fn a_destination_closed_before_ack_fails_the_handoff(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3fix-dest-closed");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let dest = open_b3_destination(vc, &baaz);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.land_handoff_destination(dest.clone(), cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(matches!(run.state, aui_protocol::HandoffState::Prepared));
            assert!(
                sidebar::is_handoff_dest(&dest, &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the links land with the prefix"
            );
        });
    });
    // The destination view dies with no pack outcome in flight.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.close_view(&dest, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let run = harness.handoff_state.runs.get("b3-src").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Failed { .. }),
                "a lost destination fails the handoff, state is {:?}",
                run.state
            );
            assert_eq!(
                run.reason.as_deref(),
                Some("the destination session closed before it acknowledged the pack")
            );
            assert!(
                !sidebar::is_handoff_dest(&dest, &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the links roll back"
            );
            // The close itself removes the never-sent destination's
            // row with its view (the usual unsent-draft close); the
            // run's Failed card on the source is what names the
            // reason.
            assert!(
                harness.session_list.read(cx).sessions.iter().all(|entry| entry.id != dest),
                "the closed destination leaves no row behind"
            );
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// B3fix: the destination session erroring out before the ack fails
/// the still-Prepared run the same way — the `Closed` event carries
/// no pack outcome, so the harness reads the run off the view.
#[gpui::test]
fn a_destination_error_before_ack_fails_the_handoff(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b3fix-dest-error");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    // A muse destination with the pack submitted but never acked.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.open("dest-me".to_owned(), false, window, cx);
            let view = harness.active.clone().expect("the destination is open");
            view.update(cx, |view, cx| {
                view.note_handoff_pack(
                    "Continuing a session handed off from Claude Code. Context follows.".to_owned(),
                    "Handed off from Claude Code: Chart the ferry routes".to_owned(),
                    cx,
                );
            });
            let mut session =
                aui_protocol::Session::new("src-me", aui_protocol::Provider::Claude, "m", "/tmp/ws");
            session.turns.push(aui_protocol::Turn::User {
                id: "u1".to_owned(),
                text: "Chart the ferry routes".to_owned(),
                attachments: vec![],
                mentions: vec![],
                timestamp: None,
            });
            let pack = crate::handoff::build_pack(&session, "/tmp/ws");
            let mut run = crate::handoff::HandoffRun::request(
                "src-me".to_owned(),
                1,
                ProviderId::ClaudeCode,
                ProviderId::Muse,
                "claude".to_owned(),
                String::new(),
                false,
                false,
                false,
            )
            .expect("fresh request starts");
            run.note_quiescing();
            run.note_checkpointed(pack);
            run.note_prepared("dest-me".to_owned());
            harness.handoff_state.runs.insert("src-me".to_owned(), run);
            harness.set_override("src-me", |meta| meta.handoff_to = Some("dest-me".to_owned()), cx);
            harness.set_override("dest-me", |meta| {
                meta.handoff_from = Some("src-me".to_owned());
                meta.handoff_from_provider = Some("claude-code".to_owned());
                meta.handoff_title = Some("Chart the ferry routes".to_owned());
            }, cx);
        });
    });
    // The session errors out with no pack outcome in flight.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination is open");
            harness.on_session_event(view, &crate::session::SessionEvent::Closed, cx);
            let run = harness.handoff_state.runs.get("src-me").expect("the run");
            assert!(
                matches!(run.state, aui_protocol::HandoffState::Failed { .. }),
                "an errored destination fails the handoff, state is {:?}",
                run.state
            );
            assert!(
                !sidebar::is_handoff_dest(
                    "dest-me",
                    &harness.session_list.read(cx).provider_sessions,
                    &harness.session_list.read(cx).overrides
                ),
                "the links roll back"
            );
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// X3b, sidebar half (Y2a: view-time collapse): storage keeps every
/// member row, the list shows one head row with the chain title,
/// opening a member id opens the head, and archiving the head hides
/// all members. Offline, so the opens are local views and the factory
/// never runs.
#[gpui::test]
fn opening_a_chain_member_opens_the_head(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("x3b-chain");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| seed_mixed_chain(harness, cx));
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(harness.session_list.read(cx).sessions.len(), 3, "storage keeps every member row");
        assert!(
            harness.session_list.read(cx).sessions.iter().any(|entry| entry.id == "chain-c" && entry.label == "Later words"),
            "the head's stored row keeps its own words; the title derives at view time"
        );
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "three members, one row");
            let row = &visible[0];
            assert_eq!(row.id, "chain-c", "the head is what is listed");
            assert_eq!(
                row.label, "Chart the Greyport ferry routes",
                "the chain keeps its carried title, not the head's own words"
            );
            assert_eq!(row.turns, 8, "2 + (3 - 1) + (5 - 1): each destination hides its pack exchange");
        });
    });
    // Opening the tail member lands on the head, selected.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("chain-a".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the member open lands somewhere");
        assert_eq!(view.read(cx).session_id, "chain-c", "a member id opens the head");
        assert_eq!(harness.open_state.pending_id.as_deref(), Some("chain-c"), "the head row is selected");
    });
    // Y2a, one identity: header, window title and selection all
    // resolve through the head — Y2a3 reads the collapsed view row
    // itself, so the header names the chain title the one visible
    // row wears, by construction.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let pending = harness.open_state.pending_id.clone().expect("the head row is selected");
            let head = harness.chain_head(&pending, cx);
            let visible = harness.visible_sessions(cx);
            let row =
                visible.iter().find(|entry| entry.id == head).expect("the head is the one listed row");
            assert_eq!(visible.len(), 1);
            let header_text = harness.collapsed_head_label(&head, cx).expect("head label");
            assert_eq!(header_text, row.label, "header title == selected row title");
            assert_eq!(row.label, "Chart the Greyport ferry routes");
        });
    });
    // Archiving the head flags every member, so none resurfaces.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.archive_session("chain-c".into(), Some(window), cx));
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        for id in ["chain-a", "chain-b", "chain-c"] {
            assert!(
                harness.session_list.read(cx).overrides.get(id).is_some_and(|meta| meta.archived),
                "archiving the head archives {id} too"
            );
        }
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(
                harness.visible_sessions(cx).iter().all(|entry| entry.id != "chain-c"),
                "the archived head leaves the visible list"
            );
        });
    });
    lane_restore(state);
}

/// Y2a: restart from the local stores alone — before any wire list —
/// shows the titled chain row, and the source's pin survives on it.
#[gpui::test]
fn a_restart_from_stores_shows_the_titled_chain_row(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2a-restart");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            seed_mixed_chain(harness, cx);
            // Pinned before the hop, the way a person leaves it.
            harness.set_override("chain-a", |meta| meta.pinned = true, cx);
            // The restart: rows gone, stores re-read, no wire yet.
            crate::provider_sessions::write(&harness.session_list.read(cx).provider_sessions);
            harness.session_list.update(cx, |list, _| list.sessions.clear());
            harness.session_list.update(cx, |list, _| list.provider_sessions = crate::provider_sessions::read());
            harness.merge_provider_rows(cx);
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "one chain, one row, before any wire list");
            let row = &visible[0];
            assert_eq!(row.id, "chain-c", "the head is what is listed");
            assert_eq!(
                row.label, "Chart the Greyport ferry routes",
                "the chain title from local stores alone"
            );
            assert!(row.pinned, "the source's pin survives the hop");
        });
    });
    lane_restore(state);
}

/// Y2a: the destination's first real message is a live turn, never a
/// first send — the stored row keeps the chain title, the paid
/// titler stands down, and the pack turn never becomes the byline.
#[gpui::test]
fn a_destinations_first_real_message_keeps_the_chain_title(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2a-first-send");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            seed_mixed_chain(harness, cx);
            let dest = "chain-c";
            assert!(
                sidebar::is_handoff_dest(dest, &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the seeded head is a handoff destination"
            );
            let turns = harness.session_list.update(cx, |list, _| {
                let row = list
                    .sessions
                    .iter_mut()
                    .find(|entry| entry.id == dest)
                    .expect("the head has a stored row");
                // The ack-only destination reads no visible turns, so
                // the next message is the old "first send" shape.
                row.turns = 0;
                row.running = false;
                assert!(sidebar::first_send_update(
                    row,
                    Some("my next message"),
                    crate::clock::now_local(),
                    true
                ));
                assert_eq!(row.label, "Later words", "no rename from the first real message");
                assert!(row.running, "the live turn still shows");
                row.turns
            });
            assert!(
                !crate::titles::should_title(true, true, harness.session_list.read(cx).overrides.get(dest), turns, false),
                "the paid titler never runs on a destination"
            );
            assert!(
                sidebar::is_pack_text("Handed off from Codex: Second leg (2 recent turns, 0 open todos, 0 files touched)"),
                "the pack acknowledgement matches the byline skip"
            );
        });
    });
    lane_restore(state);
}

/// Y2a3: storage keeps every member row across a provider rebuild, a
/// member's live facts survive it, and the one view row carries them —
/// a live turn and needs-you attention on any member show on the head.
#[gpui::test]
fn y2a3_storage_survives_a_rebuild_and_live_state_ors_into_the_head(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2a3-live");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            seed_mixed_chain(harness, cx);
            // A live turn on a non-head member, with needs-you attention.
            let now = crate::clock::now_local();
            {
                harness.session_list.update(cx, |list, _| {
                    let member = list.sessions.iter_mut().find(|entry| entry.id == "chain-a").expect("member row");
                    member.running = true;
                    member.turn_started = Some(now);
                    member.attention = vec![provider::facts::Attention::ApprovalPending];
                });
            }
            // The rebuild re-reads the records around the live rows.
            harness.merge_provider_rows(cx);
            assert_eq!(
                harness.session_list.read(cx).sessions.len(),
                3,
                "storage keeps every member row across a rebuild"
            );
            let member = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == "chain-a").expect("member row");
            assert!(member.running, "a member's live turn survives the rebuild");
            assert_eq!(member.turn_started, Some(now));
            assert_eq!(
                member.attention,
                vec![provider::facts::Attention::ApprovalPending],
                "a member's attention survives the rebuild"
            );
            // One view row, carrying the member's live state.
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "three members, one row");
            let row = &visible[0];
            assert_eq!(row.id, "chain-c", "the head is what is listed");
            assert!(row.running, "the head row shows the member's live turn");
            assert!(
                row.attention.contains(&provider::facts::Attention::ApprovalPending),
                "the head row ORs the member's attention, drew {:?}",
                row.attention
            );
            let header = harness.collapsed_head_label("chain-c", cx).expect("head label");
            assert_eq!(header, row.label, "header title == selected row title");
        });
    });
    lane_restore(state);
}

/// Y2a3: a non-handoff session whose first words start with
/// "Handed off from " keeps them — the record keeps the first prompt,
/// the row titles from it, and the byline skip does not fire. Only a
/// handoff destination ever has a pack turn.
#[gpui::test]
fn y2a3_a_non_handoff_pack_shaped_message_keeps_title_and_byline(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2a3-prefix");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            use std::collections::HashMap;
            harness.session_list.update(cx, |list, _| {
                // A provider-lane session with no handoff link anywhere.
                list.provider_sessions.insert(
                    "plain-1".into(),
                    crate::provider_sessions::ProviderSessionRecord {
                        provider: "codex".to_owned(),
                        session_id: "plain-1".to_owned(),
                        workspace: None,
                        project: None,
                        created_ms: 1_700_000_000_000,
                        updated_ms: 1_700_000_000_000,
                        turns: 0,
                        title: None,
                        first_prompt: None,
                        handoff_to: None,
                        handoff_from: None,
                        handoff_from_provider: None,
                        handoff_title: None,
                        display_texts: HashMap::new(),
                        origin: None,
                    },
                );
            });
            let prompt = "Handed off from Muse: Fix it (3 recent turns, 0 open todos, 0 files touched)";
            assert!(
                !sidebar::is_handoff_dest("plain-1", &harness.session_list.read(cx).provider_sessions, &harness.session_list.read(cx).overrides),
                "the fixture session is no destination"
            );
            harness.session_list.update(cx, |list, _| crate::provider_sessions::note_first_prompt(&mut list.provider_sessions, "plain-1", prompt));
            assert_eq!(
                harness.session_list.read(cx).provider_sessions["plain-1"].first_prompt.as_deref(),
                Some(prompt),
                "a non-destination keeps pack-shaped words as its first prompt"
            );
            harness.merge_provider_rows(cx);
            {
                harness.session_list.update(cx, |list, _| {
                    let row = list.sessions.iter_mut().find(|entry| entry.id == "plain-1").expect("member row");
                    row.label = sidebar::UNNAMED.to_owned();
                    row.named = false;
                    assert!(sidebar::first_send_update(
                        row,
                        Some(prompt),
                        crate::clock::now_local(),
                        false
                    ));
                    assert_eq!(row.label, prompt, "a non-destination titles from its own words");
                });
            }
            let (summary, ask) = sidebar::byline_landable(
                Some("Context received.".into()),
                Some(prompt.into()),
                false,
            );
            assert_eq!(summary.as_deref(), Some("Context received."));
            assert_eq!(ask.as_deref(), Some(prompt), "no byline skip off a destination");
            let visible = harness.visible_sessions(cx);
            assert!(
                visible.iter().any(|entry| entry.id == "plain-1"),
                "the session stays listed"
            );
        });
    });
    lane_restore(state);
}

/// Y2a3: the chain index builds once per storage change — repeated
/// frames and repeated row lookups never rebuild; the next storage
/// change retires it exactly once.
#[gpui::test]
fn y2a3_the_chain_index_builds_once_per_storage_change(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2a3-index");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            seed_mixed_chain(harness, cx);
            let base = harness.session_list.read(cx).chain_index_builds.get();
            let _ = harness.visible_sessions(cx);
            let after_first = harness.session_list.read(cx).chain_index_builds.get();
            assert_eq!(after_first, base + 1, "one index build serves the frame");
            let _ = harness.visible_sessions(cx);
            assert_eq!(
                harness.session_list.read(cx).chain_index_builds.get(),
                after_first,
                "a second frame on unchanged storage rebuilds nothing"
            );
            assert_eq!(harness.chain_head("chain-a", cx), "chain-c");
            assert_eq!(
                harness.chain_members("chain-b", cx),
                vec!["chain-a".to_owned(), "chain-b".to_owned(), "chain-c".to_owned()]
            );
            assert_eq!(
                harness.session_list.read(cx).chain_index_builds.get(),
                after_first,
                "row lookups reuse the frame's build"
            );
            // A storage change retires the index exactly once.
            harness.session_list.update(cx, |list, _| list.provider_sessions.get_mut("chain-a").expect("member record").turns = 99);
            harness.merge_provider_rows(cx);
            let _ = harness.visible_sessions(cx);
            let after_change = harness.session_list.read(cx).chain_index_builds.get();
            assert_eq!(after_change, after_first + 1, "one rebuild for the storage change");
            let _ = harness.visible_sessions(cx);
            assert_eq!(
                harness.session_list.read(cx).chain_index_builds.get(),
                after_change,
                "the rebuilt index serves every later frame"
            );
        });
    });
    lane_restore(state);
}

/// X3b, sidebar half: the fallback divider's source link bypasses the
/// head redirect and opens the retired source itself.
#[gpui::test]
fn the_fallback_source_link_opens_the_source_itself(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("x3b-bypass");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "old-muse".into(),
                    crate::sessions::SessionMeta {
                        handoff_to: Some("new-muse".into()),
                        ..Default::default()
                    },
                );
            });
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "new-muse".into(),
                    crate::sessions::SessionMeta {
                        handoff_from: Some("old-muse".into()),
                        handoff_from_provider: Some("muse".into()),
            handoff_title: None,
                        ..Default::default()
                    },
                );
            });
            harness.session_list.update(cx, |list, _| list.sessions.push(chain_muse_entry("old-muse", "First words", 2)));
            harness.session_list.update(cx, |list, _| {
                list.sessions.push(chain_muse_entry(
                    "new-muse",
                    "Later words",
                    sidebar::visible_turns(1, true),
                ));
            });
            harness.collapse_handoff_chains(cx);
            assert_eq!(harness.session_list.read(cx).sessions.len(), 2, "storage keeps both member rows");
            assert_eq!(harness.chain_head("old-muse", cx), "new-muse");
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "the pair is one row");
            assert_eq!(visible[0].id, "new-muse");
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_source_read_only("old-muse".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the bypass open lands somewhere");
        assert_eq!(
            view.read(cx).session_id,
            "old-muse",
            "the explicit source path bypasses the redirect"
        );
    });
    lane_restore(state);
}

/// X3d: a two-member chain indexes as one search row under the head,
/// with the first member's words finding it.
#[gpui::test]
fn x3d_a_chain_indexes_once_under_the_head(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("x3d-search");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "search-old".into(),
                    crate::sessions::SessionMeta {
                        handoff_to: Some("search-new".into()),
                        ..Default::default()
                    },
                );
            });
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "search-new".into(),
                    crate::sessions::SessionMeta {
                        handoff_from: Some("search-old".into()),
                        handoff_from_provider: Some("muse".into()),
            handoff_title: None,
                        ..Default::default()
                    },
                );
            });
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(
                "search-old".into(),
                IndexEntry {
                    title: "Greyport ferries".into(),
                    first_user_prompt: Some("chart the ferry routes".into()),
                    search_text: "cormorant crossing lighthouse supply run".into(),
                    ..Default::default()
                },
            ));
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(
                "search-new".into(),
                IndexEntry {
                    title: "Later words".into(),
                    first_user_prompt: Some("what else".into()),
                    search_text: "harbour ledger quiet winter".into(),
                    ..Default::default()
                },
            ));
            let rows = harness.search_session_rows(cx);
            assert_eq!(
                rows.len(),
                1,
                "a two-member chain is one search row, drew {:?}",
                rows.iter().map(|row| row.session_id.clone()).collect::<Vec<_>>()
            );
            assert_eq!(rows[0].session_id, "search-new", "the head is what is indexed");
            let mut connection =
                crate::search::open_at(std::path::Path::new(":memory:")).expect("in-memory search db");
            crate::search::rebuild_sessions(&mut connection, &rows).expect("rebuild");
            let hits = crate::search::query_sessions(&connection, "cormorant", crate::search::LIMIT);
            assert_eq!(hits.len(), 1, "the first member's words find the chain");
            assert_eq!(hits[0].session_id, "search-new", "a match in an early member returns the head");
        });
    });
    lane_restore(state);
}

/// P3 (docs/50): Claude Code and Codex sessions are search rows of their
/// own, and their transcript words — read from each provider's stored
/// files — find them, beside a Muse session found through its index.
/// A settled exchange moves only that row's fingerprint.
#[gpui::test]
fn p3_search_finds_every_providers_sessions_by_their_words(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p3-search");
    let workspace = state.2.join("work");
    std::fs::create_dir_all(&workspace).expect("workspace");
    // Claude Code's stored transcript, under Baaz's own claude-home.
    let slug = state.2.join("claude-home").join("projects").join("-work");
    std::fs::create_dir_all(&slug).expect("claude slug");
    std::fs::write(
        slug.join("cc-search-1.jsonl"),
        [
            serde_json::json!({"type":"user","message":{"role":"user","content":"audit the kestrel migration"}}),
            serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"The quillwort table needs an index."}]}}),
        ]
        .map(|line| line.to_string())
        .join("\n"),
    )
    .expect("claude transcript");
    // Codex's rollout, under Baaz's own codex-home.
    let codex_id = "01a14915-3803-7303-aa91-189ad43310e1";
    let day = state.2.join("codex-home").join("sessions").join("2026").join("10").join("17");
    std::fs::create_dir_all(&day).expect("codex day");
    std::fs::write(
        day.join(format!("rollout-2026-10-17T14-28-09-{codex_id}.jsonl")),
        [
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>marram</environment_context>"},{"type":"input_text","text":"why is the tamarisk build slow"}]}}),
            serde_json::json!({"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"The samphire cache is cold."}]}}),
        ]
        .map(|line| line.to_string())
        .join("\n"),
    )
    .expect("codex rollout");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let root = Some(workspace.to_string_lossy().into_owned());
            harness.session_list.update(cx, |list, _| {
                crate::provider_sessions::upsert_open(
                    &mut list.provider_sessions,
                    "claude-code",
                    "cc-search-1",
                    root.clone(),
                    None,
                    Some("Kestrel audit".into()),
                );
            });
            harness.session_list.update(cx, |list, _| crate::provider_sessions::upsert_open(&mut list.provider_sessions, "codex", codex_id, root, None, None));
            harness.session_list.update(cx, |list, _| crate::provider_sessions::note_first_prompt(&mut list.provider_sessions, codex_id, "why is the tamarisk build slow"));
            harness.session_list.update(cx, |list, _| std::sync::Arc::make_mut(&mut list.index).insert(
                "muse-search-1".into(),
                IndexEntry {
                    title: "Harbour ledger".into(),
                    search_text: "cormorant crossing".into(),
                    updated_at_us: Some(7),
                    ..Default::default()
                },
            ));
            let rows = harness.search_sync_rows(cx);
            let ids: Vec<&str> = rows.iter().map(|(row, _, _)| row.session_id.as_str()).collect();
            assert_eq!(ids.len(), 3, "one row per session, every provider: {ids:?}");
            let codex_row = rows.iter().find(|(row, _, _)| row.session_id == codex_id).expect("codex row");
            assert_eq!(codex_row.0.label, "why is the tamarisk build slow", "a title-less row reads its first prompt");
            let claude_row = rows.iter().find(|(row, _, _)| row.session_id == "cc-search-1").expect("claude row");
            assert_eq!(claude_row.0.label, "Kestrel audit");
            assert_eq!(claude_row.1, vec!["cc-search-1".to_owned()], "its body comes from its own transcript");

            // A Muse row's fingerprint is exactly the pre-P3 formula, so
            // upgrading writes only the new provider rows (a changed
            // formula rewrote all 2,612 rows of a real store once, 23 s).
            {
                use std::hash::{Hash, Hasher};
                let (row, body_ids, fingerprint) =
                    rows.iter().find(|(row, _, _)| row.session_id == "muse-search-1").expect("muse row");
                let mut hasher = std::collections::hash_map::DefaultHasher::new();
                (&row.session_id, &row.label, &row.title, &row.first_prompt, &row.body, &row.project, &row.workspace)
                    .hash(&mut hasher);
                for id in body_ids {
                    id.hash(&mut hasher);
                    harness.session_list.read(cx).index.get(id).and_then(|entry| entry.updated_at_us).hash(&mut hasher);
                }
                assert_eq!(*fingerprint, hasher.finish() as i64, "a Muse row's fingerprint is unchanged by P3");
            }
            let owners = harness.search_body_owners(cx);
            assert_eq!(owners.len(), 2, "muse ids are never read from provider files");
            let index_path = state.2.join("no-muse-index.db");
            let mut connection =
                crate::search::open_at(std::path::Path::new(":memory:")).expect("in-memory search db");
            let sync = |connection: &mut rusqlite::Connection, rows: &[(crate::search::SessionRow, Vec<String>, i64)]| {
                let synced: Vec<crate::search::SyncRow<'_>> = rows
                    .iter()
                    .map(|(row, body_ids, fingerprint)| crate::search::SyncRow { row, body_ids, fingerprint: *fingerprint })
                    .collect();
                crate::search::sync_sessions(connection, &synced, &|ids| {
                    crate::app::find::search_bodies(&index_path, &owners, ids)
                })
                .expect("sync")
            };
            assert_eq!(sync(&mut connection, &rows).written, 3);
            let find = |query: &str| -> Vec<String> {
                crate::search::query_sessions(&connection, query, crate::search::LIMIT)
                    .into_iter()
                    .map(|hit| hit.session_id)
                    .collect()
            };
            assert_eq!(find("quillwort"), vec!["cc-search-1"], "a Claude answer finds its session");
            assert_eq!(find("kestrel"), vec!["cc-search-1"], "a Claude prompt finds its session");
            assert_eq!(find("samphire"), vec![codex_id], "a Codex answer finds its session");
            assert!(find("marram").is_empty(), "injected context is not searchable");

            // One Claude exchange settles: only its row is rewritten.
            harness.session_list.update(cx, |list, _| crate::provider_sessions::note_settled_turn_counted(&mut list.provider_sessions, "cc-search-1", 1, true));
            let after = harness.search_sync_rows(cx);
            let stats = sync(&mut connection, &after);
            assert_eq!((stats.written, stats.unchanged), (1, 2), "{stats:?}");
        });
    });
    lane_restore(state);
}

/// X3d: the chain row matches the transcript — each destination hides
/// its pack exchange, so claude-code (1) → codex (2 incl. pack) → muse
/// (2 incl. pack) reads 3 turns.
#[gpui::test]
fn x3d_chain_counts_match_the_transcript(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("x3d-counts");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            use std::collections::HashMap;
            let record =
                |id: &str, provider: &str, turns: u64, to: Option<&str>, from: Option<(&str, &str)>| {
                    crate::provider_sessions::ProviderSessionRecord {
                        provider: provider.to_owned(),
                        session_id: id.to_owned(),
                        workspace: None,
                        project: None,
                        created_ms: 1_700_000_000_000,
                        updated_ms: 1_700_000_000_000,
                        turns,
                        title: None,
                        first_prompt: None,
                        handoff_to: to.map(str::to_owned),
                        handoff_from: from.map(|(source, _)| source.to_owned()),
                        handoff_from_provider: from.map(|(_, provider)| provider.to_owned()),
        handoff_title: None,
                        display_texts: HashMap::new(),
                        origin: None,
                    }
                };
            harness.session_list.update(cx, |list, _| {
                list.provider_sessions.insert(
                    "count-a".into(),
                    record("count-a", "claude-code", 1, Some("count-b"), None),
                );
            });
            harness.session_list.update(cx, |list, _| {
                list.provider_sessions.insert(
                    "count-b".into(),
                    record("count-b", "codex", 2, Some("count-c"), Some(("count-a", "claude-code"))),
                );
            });
            harness.session_list.update(cx, |list, _| {
                list.overrides.insert(
                    "count-c".into(),
                    crate::sessions::SessionMeta {
                        handoff_from: Some("count-b".into()),
                        handoff_from_provider: Some("codex".into()),
            handoff_title: None,
                        ..Default::default()
                    },
                );
            });
            // The muse head through the real join, wire count 2 incl. pack.
            // Built before the provider rebuild so the restart synth
            // below does not double it (Y2a: storage keeps one row per
            // session).
            let wire = crate::list_cache::ListedRow {
                session_id: "count-c".into(),
                created_at: "2026-09-13T10:00:00Z".into(),
                updated_at: "2026-09-13T10:00:00Z".into(),
                status: Some("idle".into()),
                turn_count: 2,
                ..Default::default()
            };
            let head = crate::sidebar::SessionEntry::join(
                &wire,
                harness.session_list.read(cx).index.get("count-c"),
                harness.session_list.read(cx).overrides.get("count-c"),
                &crate::projects::Projects::default(),
            );
            harness.session_list.update(cx, |list, _| list.sessions.push(head));
            harness.merge_provider_rows(cx);
            harness.collapse_handoff_chains(cx);
            assert_eq!(harness.session_list.read(cx).sessions.len(), 3, "storage keeps every member row");
            let visible = harness.visible_sessions(cx);
            assert_eq!(visible.len(), 1, "three members, one row");
            assert_eq!(
                visible[0].turns, 3,
                "1 + (2 - 1) + (2 - 1): each destination hides its pack exchange"
            );
        });
    });
    lane_restore(state);
}
