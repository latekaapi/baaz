//! Tests for provider-lane opens: the draft drawn on the click, landing, failure, timeout, forks.

use super::*;

/// Q2b: the seed decision at the `ProviderTurnFinished` call site —
/// through [`should_seed_claude_code_model`], not just the providers
/// helper. A Sonnet fallback under a picked Opus seeds (chip and seed
/// update as before Q2); a Haiku report under a picked Opus never
/// does; a Haiku pick's own Haiku report does; other providers never
/// seed; and a `true` decision persists through the same write the
/// call site uses.
#[test]
fn lifecycle_seed_decision_drops_only_the_subcall() {
    use crate::providers::ProviderId;

    assert!(should_seed_claude_code_model(
        ProviderId::ClaudeCode,
        "claude-sonnet-4-5-20250822",
        "opus"
    ));
    assert!(should_seed_claude_code_model(
        ProviderId::ClaudeCode,
        "claude-3-5-sonnet-20241022",
        "opus"
    ));
    assert!(!should_seed_claude_code_model(
        ProviderId::ClaudeCode,
        "claude-haiku-4-5-20251001",
        "opus"
    ));
    assert!(should_seed_claude_code_model(
        ProviderId::ClaudeCode,
        "claude-haiku-4-5-20251001",
        "haiku"
    ));
    assert!(!should_seed_claude_code_model(ProviderId::Muse, "claude-sonnet-4-5", "opus"));
    assert!(!should_seed_claude_code_model(ProviderId::ClaudeCode, "<synthetic>", "opus"));
    assert!(!should_seed_claude_code_model(ProviderId::ClaudeCode, "", "opus"));

    let _sandbox = crate::providers::TestEnvSandbox::enter("lifecycle-seed");
    let report = "claude-sonnet-4-5-20250822";
    if should_seed_claude_code_model(ProviderId::ClaudeCode, report, "opus") {
        crate::providers::write_claude_code_last_reported_model(report);
    }
    assert_eq!(
        crate::providers::read_claude_code_last_reported_model().as_deref(),
        Some(report),
        "a fallback report persists the seed"
    );
    if should_seed_claude_code_model(
        ProviderId::ClaudeCode,
        "claude-haiku-4-5-20251001",
        "opus",
    ) {
        crate::providers::write_claude_code_last_reported_model("claude-haiku-4-5-20251001");
    }
    assert_eq!(
        crate::providers::read_claude_code_last_reported_model().as_deref(),
        Some(report),
        "a dropped sub-call persists nothing"
    );
}

#[gpui::test]
fn picking_a_non_muse_provider_opens_a_lane_and_starts_no_muse_session(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("open");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = scripted_factory();
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the provider session is open");
        assert!(
            view.read(cx).provider_kind() != ProviderId::Muse,
            "the pick opens a provider lane, not a muse session"
        );
        assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
        assert!(!harness.open_state.switch_pending, "the switch landed");
        assert!(
            harness.overlays.read(cx).dialog.is_none(),
            "no failure dialog: no muse session/start was ever issued at the dead child"
        );
        let open_id = view.read(cx).session_id.clone();
        assert!(
            harness.provider_sessions.contains_key(&open_id),
            "the lane open writes the local record"
        );
        assert!(
            harness
                .sessions
                .iter()
                .any(|entry| entry.id == open_id && entry.provider == crate::providers::ProviderId::Codex),
            "a provider sidebar row names the lane session"
        );
    });
    lane_restore(state);
}

/// docs/71 §3: a failed open is typed from the refusal's own hint, and
/// the kind decides the extra action (Install for a missing CLI).
#[gpui::test]
fn a_missing_cli_fails_the_open_typed_not_installed(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("not-installed");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = std::sync::Arc::new(|id: ProviderId| {
                Err(provider::ProviderError::launch_refusal(
                    provider::RefusalHint::NotInstalled,
                    format!("{} isn't installed", id.label()),
                    "codex was not found on PATH",
                ))
            });
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let failure = baaz.read(cx).open_state.provider_open_error.clone().expect("the open failed inline");
        assert_eq!(failure.kind, crate::provider_status::FailureKind::NotInstalled);
        assert_eq!(failure.kind.action(), Some(crate::provider_status::FailureAction::Install));
        assert!(failure.message.contains("not found on PATH"), "{}", failure.message);
    });
    lane_restore(state);
}

#[gpui::test]
fn a_failed_provider_connect_surfaces_an_error_and_opens_no_view(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("fail");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = failing_factory("no child here");
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_on_provider(ProviderId::ClaudeCode, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "a failed connect opens no view");
        assert!(!harness.open_state.switch_pending, "the verbs waiting on the switch are released");
        assert!(
            harness.overlays.read(cx).dialog.is_none(),
            "no failure dialog: a fresh open fails inline instead"
        );
        let failure = harness.open_state.provider_open_error.clone().expect("the inline failure stands");
        assert!(failure.session_id.is_none(), "a fresh open never earned an id");
        assert_eq!(failure.provider, ProviderId::ClaudeCode);
        assert!(failure.message.contains("no child here"), "the reason survives: {}", failure.message);
    });
    lane_restore(state);
}

/// Real keystrokes into the focused window, without parking the
/// executor: `simulate_input` parks after typing, which would land the
/// provider open these tests hold back.
fn type_unparked(vc: &mut gpui::VisualTestContext, keys: &[&str]) {
    let window = vc.update(|window, _| window.window_handle());
    for key in keys {
        vc.cx.dispatch_keystroke(window, crate::keymap::parse_keystroke(key).expect("a keystroke"));
    }
}

/// "New session" on a Codex default draws the draft in the click's own
/// update, like a muse draft: a provider-lane view, starting, with no
/// child asked yet. Keys typed before the child answers reach it — `/`
/// opens the command menu — and an Enter waits in the start queue: the
/// composer clears, and the words leave exactly once, after the child
/// lands, under the id the child minted. Restore the wait (open the
/// view in the landing) and the first block finds no view.
#[gpui::test]
fn a_new_provider_session_opens_on_the_click_and_sends_once_it_lands(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early");
    let (factory, commands) = recording_resumable_factory();
    let (baaz, vc, project) = provider_draft_window(cx, &state.2, factory);
    let provisional = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let view = harness.active.clone().expect("the draft is up in the click's own update");
            assert!(view.read(cx).is_starting(), "the child has not answered");
            assert!(view.read(cx).provider_kind() != ProviderId::Muse, "it rides the provider lane");
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
            let id = view.read(cx).session_id.clone();
            assert!(harness.open_state.starts_in_flight.contains(&id), "scripted verbs wait for it");
            assert!(!harness.open_state.switch_pending, "a new session is no switch");
            assert_eq!(harness.open_state.drafts.get(&project), Some(&id), "a second click reuses it");
            id
        })
    });
    assert!(commands.lock().expect("commands").is_empty(), "no child was asked yet");
    type_unparked(vc, &["/"]);
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.overlays.read(cx).menu.is_some(), "`/` opens the command menu before the child answers");
        assert!(harness.active.clone().expect("the draft").read(cx).is_starting(), "still opening");
    });
    type_unparked(vc, &["backspace", "h", "e", "l", "l", "o", "enter"]);
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the draft");
        assert!(view.read(cx).is_starting(), "the Enter came before the child answered");
        assert_eq!(view.read(cx).draft_text(cx), "", "the send took the words, as any send does");
    });
    assert!(commands.lock().expect("commands").is_empty(), "nothing left before the open");
    vc.run_until_parked();
    let sent: Vec<_> = commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::SubmitInput { session_id, display_text, .. } => {
                Some((session_id.clone(), display_text.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec![("s-open".to_owned(), Some("hello".to_owned()))], "sent exactly once, after landing");
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the draft stays in the centre");
        assert_eq!(view.read(cx).session_id, "s-open", "the view took the child's id");
        assert!(!view.read(cx).is_starting(), "the open landed");
        assert!(harness.open_state.starts_in_flight.is_empty(), "nothing is still opening");
        assert_eq!(harness.open_state.drafts.get(&project).map(String::as_str), Some("s-open"), "the draft name moved");
        assert!(harness.provider_sessions.contains_key("s-open"), "the landing writes the record");
        assert!(!harness.provider_sessions.contains_key(&provisional), "nothing keeps the provisional id");
        assert!(!harness.overrides.contains_key(&provisional), "nor its overrides");
        assert_eq!(harness.open_state.pending_id.as_deref(), Some("s-open"), "the selection follows the id");
    });
    lane_restore(state);
}

/// An open that fails (a child that dies on its first breath, QA
/// 2026-10-30 with the fake's `basic#die`) lands the typed failure view
/// a failed open gets — no draft under a red "did not start" banner, no
/// send button stuck as a stop square. The send it captured rides Retry
/// back into the new draft, and the next "New session" starts fresh.
#[gpui::test]
fn a_failed_provider_open_lands_the_typed_failure_and_retry_keeps_the_send(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early-fail");
    let (baaz, vc, project) = provider_draft_window(cx, &state.2, failing_factory("no codex here"));
    let provisional = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            harness.active.clone().expect("the draft is up").read(cx).session_id.clone()
        })
    });
    type_unparked(vc, &["h", "i", "enter"]);
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "no dead draft in the centre");
        assert!(harness.find_view(&provisional, cx).is_none(), "the dead draft is gone, parked nowhere");
        let failure = harness.open_state.provider_open_error.clone().expect("the typed failure view");
        assert_eq!(failure.provider, ProviderId::Codex);
        assert_eq!(failure.session_id, None, "a session that never opened");
        assert!(failure.message.contains("no codex here"), "the failure's words: {}", failure.message);
        assert!(
            matches!(&failure.retry, ProviderOpenRetry::OpenNew { draft, .. } if draft == "hi"),
            "Retry carries the captured send"
        );
        assert!(harness.open_state.starts_in_flight.is_empty(), "scripted verbs are released");
        assert!(!harness.open_state.drafts.contains_key(&project), "the next New session starts fresh");
        assert!(harness.provider_sessions.is_empty(), "no record for a session that never was");
        assert!(harness.overlays.read(cx).dialog.is_none(), "inline, not a dialog");
    });
    // Retry on a child that answers: a new draft, the words back in it.
    let (factory, _commands) = recording_resumable_factory();
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.provider_factory = factory;
            harness.retry_provider_open(window, cx);
            assert!(harness.open_state.provider_open_error.is_none(), "the failure view leaves on Retry");
            let view = harness.active.clone().expect("a new draft opens on the click");
            assert_eq!(view.read(cx).draft_text(cx), "hi", "the words came back");
            assert!(!view.read(cx).busy(), "nothing stands busy");
        })
    });
    lane_restore(state);
}

/// A view whose start failed takes no send into a busy state: a send
/// that reached `submit` on it (the scripted `send:` verb does) armed
/// `submitting` with no lane to ever clear it — the stop square stood
/// forever. The words go back in the composer instead.
#[gpui::test]
fn a_send_on_a_failed_start_never_stands_busy(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-failed-send");
    let (baaz, vc, _project) = provider_draft_window(cx, &state.2, failing_factory("no codex here"));
    let view = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            harness.active.clone().expect("the draft is up")
        })
    });
    vc.update(|_, cx| view.update(cx, |view, cx| view.start_failed("no codex here".into(), cx)));
    vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("hello".into(), cx)));
    vc.update(|_, cx| {
        assert!(!view.read(cx).busy(), "no stop square on a session that never started");
        assert_eq!(view.read(cx).draft_text(cx), "hello", "the words are back");
    });
    lane_restore(state);
}

/// A provider draft that lands moves everything its provisional id
/// held — the override a toggle persisted meanwhile, the terminal
/// provider — and keeps showing the mode the open asked for until the
/// provider reports one: the double reports none, so the chip must
/// not fall back to the fold's seeded default.
#[gpui::test]
fn a_landed_provider_draft_moves_its_keys_and_keeps_the_requested_mode(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early-rekey");
    let (factory, _commands) = recording_resumable_factory();
    let (baaz, vc, _project) = provider_draft_window(cx, &state.2, factory);
    let provisional = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.args.approval_mode = Some(aui_protocol::PermissionMode::DenyUnmatched);
            harness.new_session(window, cx);
            let id = harness.active.clone().expect("the draft").read(cx).session_id.clone();
            harness.overrides.entry(id.clone()).or_default();
            crate::sessions::write(&harness.overrides);
            assert!(harness.terminal_dock.read(cx).knows_provider(&id));
            id
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the draft");
        assert_eq!(view.read(cx).session_id, "s-open");
        assert!(!harness.overrides.contains_key(&provisional), "the override moved");
        assert!(harness.overrides.contains_key("s-open"));
        assert!(!crate::sessions::read().contains_key(&provisional), "on disk too");
        assert!(!harness.terminal_dock.read(cx).knows_provider(&provisional), "the terminal provider moved");
        assert!(harness.terminal_dock.read(cx).knows_provider("s-open"));
        assert_eq!(
            view.read(cx).mode_label(),
            aui_protocol::PermissionMode::DenyUnmatched.label(),
            "the chip keeps the requested mode until the provider reports one"
        );
    });
    lane_restore(state);
}

/// Closing a provider draft before its child answers forgets the
/// provisional names at once — ⌘N starts fresh rather than reopening
/// a dead view — and the late answer is hung up, recording nothing.
#[gpui::test]
fn closing_an_opening_provider_draft_forgets_it_and_drops_the_late_child(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early-close");
    let (factory, commands) = recording_resumable_factory();
    let (baaz, vc, project) = provider_draft_window(cx, &state.2, factory);
    let closed = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let id = harness.active.clone().expect("the draft").read(cx).session_id.clone();
            harness.close_view(&id, cx);
            assert!(!harness.open_state.drafts.contains_key(&project), "no draft names the closed view");
            assert!(!harness.open_state.unsent.contains(&id));
            assert!(!harness.open_state.starts_in_flight.contains(&id), "scripted verbs stop waiting on it");
            assert!(!harness.terminal_dock.read(cx).knows_provider(&id));
            id
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(!harness.provider_sessions.contains_key(&closed), "the late child is recorded nowhere");
        assert!(!harness.open_state.drafts.values().any(|id| *id == closed), "⌘N cannot reopen the closed view");
        assert!(harness.find_view(&closed, cx).is_none());
    });
    assert!(
        !commands.lock().expect("commands").iter().any(|c| matches!(c, provider::Command::SubmitInput { .. })),
        "nothing was sent to it"
    );
    lane_restore(state);
}

/// A child that never answers fails visibly past the open timeout: the
/// typed failure view, the captured send riding its Retry, the names
/// forgotten; an answer arriving after that is hung up, not attached.
#[gpui::test]
fn a_provider_open_past_its_timeout_fails_the_view(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early-timeout");
    let (factory, commands) = recording_resumable_factory();
    let (baaz, vc, project) = provider_draft_window(cx, &state.2, factory);
    let provisional = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            harness.active.clone().expect("the draft").read(cx).session_id.clone()
        })
    });
    type_unparked(vc, &["h", "i", "enter"]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.expire_provider_open(&provisional, ProviderId::Codex, Some(project.clone()), workspace, cx)
        });
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "the dead draft left the centre");
        let failure = harness.open_state.provider_open_error.clone().expect("the typed failure view");
        assert!(failure.message.contains("did not answer"), "the failure says why: {}", failure.message);
        assert!(matches!(&failure.retry, ProviderOpenRetry::OpenNew { draft, .. } if draft == "hi"));
        assert!(!harness.open_state.drafts.contains_key(&project));
        assert!(harness.open_state.starts_in_flight.is_empty());
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.find_view(&provisional, cx).is_none(), "the late answer never attached");
        assert!(harness.provider_sessions.is_empty());
    });
    assert!(!commands.lock().expect("commands").iter().any(|c| matches!(c, provider::Command::SubmitInput { .. })));
    lane_restore(state);
}

/// A provider draft still opening is never evicted from the parked
/// views, even one no `drafts` entry protects (a project-less open):
/// it holds the sends queued for its child.
#[gpui::test]
fn an_opening_provider_draft_is_never_evicted(cx: &mut gpui::TestAppContext) {
    let state = lane_state("provider-early-evict");
    let (factory, _commands) = recording_resumable_factory();
    let (baaz, vc, _project) = provider_draft_window(cx, &state.2, factory);
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let id = harness.active.clone().expect("the draft").read(cx).session_id.clone();
            // As a project-less open: no `drafts` entry names it.
            harness.open_state.drafts.clear();
            let workspace = harness.workspace();
            for n in 0..=SESSION_CACHE_LIMIT {
                let host = SessionHost {
                    provider_id: ProviderId::Muse.as_str().to_owned(),
                    workspace: workspace.clone(),
                    overlays: harness.overlays.clone(),
                    capture: crate::shot::CaptureToken::default(),
                    terminal_host: None,
                };
                let other = cx.new(|cx| SessionView::new(format!("s-park-{n}"), host, window, cx));
                harness.park_active(cx);
                harness.active = Some(other);
            }
            harness.park_active(cx);
            assert!(
                harness.registry.is_parked(&id),
                "the opening draft survives {} later parks",
                SESSION_CACHE_LIMIT + 1
            );
            assert!(harness.registry.parked_len() <= SESSION_CACHE_LIMIT + 1);
        })
    });
    lane_restore(state);
}

#[gpui::test]
fn opening_a_fork_resumes_it_on_a_new_lane_of_the_same_provider(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("fork");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = resumable_factory();
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_forked_on_provider(
                ProviderId::Codex,
                "s-fork".to_owned(),
                workspace,
                window,
                cx,
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the fork is open");
        assert!(view.read(cx).provider_kind() != ProviderId::Muse, "the fork opens a provider lane");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex, "on the same provider");
        assert_eq!(view.read(cx).session_id, "s-fork", "resuming the fork, not a fresh session");
        assert!(!harness.open_state.switch_pending, "the switch landed");
        assert!(
            harness.overlays.read(cx).dialog.is_none(),
            "no failure dialog: the resume answered a session"
        );
    });
    lane_restore(state);
}

#[gpui::test]
fn new_session_in_routes_a_provider_pick_to_the_lane_offline(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("route");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    // No client: scripted chrome. The pick alone must route past muse.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.select_new_provider(ProviderId::Codex, cx));
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session_in(None, window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("a session opened");
        assert!(
            view.read(cx).provider_kind() != ProviderId::Muse,
            "`new` on a provider pick ends on a provider lane"
        );
        assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
    });
    lane_restore(state);
}
