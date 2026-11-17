//! Tests for the pending switch (Q4) and the provider replace (Y2b).

use super::*;

/// Q4: while a provider reopen is pending, `pending_switch_dest` names
/// the destination — the exact value `render_centre` branches on for
/// the loading centre. Q4c: the right pane stays neutral at the click
/// — it describes neither session until the open lands — keeping its
/// width so the centre never resizes. After the open lands the
/// destination is active and the previous session parks with its draft
/// intact. The mid-switch asserts pin that helper plus the neutral
/// pane (restoring the destination's pane at the click fails the pane
/// assert); they never render, so the render branch itself is pinned
/// through its decision value here and at the surrounding decision
/// sites in the Q4b/Q4c tests below.
#[gpui::test]
fn q4_pending_provider_switch_hides_the_previous_transcript(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4-pending");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the first lane is open");
            view.update(cx, |view, cx| view.set_draft("hello unsent".to_owned(), window, cx));
        });
    });
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.set_override(&first, |meta| meta.right = None, cx);
        });
    });
    // The click on the stored session — observed before the reopen
    // lands, so no `run_until_parked` between this and the asserts.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert_eq!(
                harness.open_state.pending_id.as_deref(),
                Some("s-codex-old"),
                "the click's target stays selected"
            );
            assert!(harness.open_state.switch_pending, "the provider switch is pending");
            let active_id =
                harness.active.clone().expect("previous view stays until the open lands");
            assert_eq!(
                active_id.read(cx).session_id,
                first,
                "the previous view is still the active entity mid-switch"
            );
            assert_eq!(
                harness.pending_switch_dest(cx),
                Some("s-codex-old".to_owned()),
                "the centre is the destination's loading state, not the previous view"
            );
            assert!(
                harness.pending_right_neutral(cx),
                "the right pane is neutral while the switch is pending"
            );
            assert!(
                !crate::layout::Layout::get(cx).right_open,
                "the pane keeps the previous session's closed state at the click, never the destination's"
            );
            assert!(
                harness
                    .overrides
                    .get("s-codex-old")
                    .and_then(|meta| meta.right.clone())
                    .is_some_and(|right| right.open),
                "the destination's stored pane is untouched until it lands"
            );
            assert_eq!(
                active_id.read(cx).draft_text(cx),
                "hello unsent",
                "the previous draft is untouched mid-switch"
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination landed");
            assert_eq!(view.read(cx).session_id, "s-codex-old", "the destination is open");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "no pending loading state once the switch landed"
            );
            assert!(!harness.open_state.switch_pending, "the switch landed");
            assert!(harness.open_state.provider_open_error.is_none(), "no inline failure on success");
            let parked = harness
                .registry.parked(&first)
                .expect("the previous session parks intact");
            assert_eq!(
                parked.read(cx).draft_text(cx),
                "hello unsent",
                "the previous draft survives the landing"
            );
            assert!(
                crate::layout::Layout::get(cx).right_open,
                "the landed destination keeps its own pane"
            );
        });
    });
    lane_restore(state);
}

/// Q4c (c): while a provider reopen is pending the pane is neutral —
/// asserted at the `save_right_for_active` decision site (a toggle
/// changes neither session's stored [`RightState`](crate::sessions::RightState)),
/// at the navigation-sync site (neither session's URL persists), and
/// at the render site (one frame creates no webview for the
/// still-open session, so its page is never shown). Before the fix the
/// click restored the destination's pane, the toggle saved onto it,
/// and the frame built the old session's webview under the loading
/// centre.
#[gpui::test]
fn q4c_pending_pane_stays_neutral_and_saves_nothing(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4c-pane");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    // The still-open session stands on an open Files pane with a
    // stored URL; the live kind then moves to Browser with no webview
    // built yet — the production path where the frame would build it.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.toggle_right(cx);
            harness.remember_browser_url_cheap(
                &first,
                "https://a.example/".to_owned(),
                cx,
            );
            crate::layout::Layout::stage(cx, |layout| layout.right_kind = Some(crate::layout::RightKind::Browser));
        });
    });
    let a_pane = vc.update(|_, cx| {
        baaz.read(cx).overrides.get(&first).and_then(|meta| meta.right.clone()).expect("A's pane stored")
    });
    assert!(a_pane.open, "A's stored pane stands open");
    let b_pane = crate::sessions::RightState {
        open: true,
        kind: crate::layout::RightKind::Files,
        browser_url: Some("https://b.example/".to_owned()),
        ..Default::default()
    };
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
        baaz.update(cx, |harness, cx| {
            harness.set_override("s-codex-old", |meta| meta.right = Some(b_pane.clone()), cx);
        });
    });
    // The click on the stored session — observed before the reopen
    // lands, so no `run_until_parked` between this and the asserts.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert_eq!(
                harness.pending_switch_dest(cx),
                Some("s-codex-old".to_owned()),
                "the provider switch is pending"
            );
            assert!(
                harness.pending_right_neutral(cx),
                "the right pane is neutral while the switch is pending"
            );
            assert!(
                crate::layout::Layout::get(cx).right_open,
                "the live pane keeps its width instead of following the destination"
            );
            assert_eq!(
                crate::layout::Layout::get(cx).right_kind,
                Some(crate::layout::RightKind::Browser),
                "the live kind is untouched at the click"
            );
        });
    });
    // One frame while pending: the neutral body builds no webview for
    // the still-open session, so its page is never shown.
    vc.draw(
        gpui::point(gpui::px(0.), gpui::px(0.)),
        gpui::size(gpui::px(1440.), gpui::px(900.)),
        |_, _| baaz.clone().into_any_element(),
    );
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(
                !harness.browser.session_has_page(&first),
                "no webview is built for the still-open session while pending"
            );
            assert_eq!(
                harness.browser.browser_builds, 0,
                "the pending frame builds no webview at all"
            );
            // A toggle flips only the live pane: neither session's
            // stored pane moves.
            harness.toggle_right(cx);
            assert!(
                !crate::layout::Layout::get(cx).right_open,
                "the toggle flips the live pane"
            );
            assert_eq!(
                harness.overrides.get(&first).and_then(|meta| meta.right.clone()),
                Some(a_pane.clone()),
                "the still-open session's stored pane is untouched by the toggle"
            );
            assert_eq!(
                harness
                    .overrides
                    .get("s-codex-old")
                    .and_then(|meta| meta.right.clone()),
                Some(b_pane.clone()),
                "the destination's stored pane is untouched by the toggle"
            );
            // Navigation syncs for either session defer while pending.
            harness.remember_browser_url_cheap(
                &first,
                "https://changed.example/".to_owned(),
                cx,
            );
            harness.remember_browser_url_cheap(
                "s-codex-old",
                "https://changed.example/".to_owned(),
                cx,
            );
            assert_eq!(
                harness
                    .overrides
                    .get(&first)
                    .and_then(|meta| meta.right.clone())
                    .and_then(|right| right.browser_url),
                Some("https://a.example/".to_owned()),
                "no sync overwrites the old session's stored URL mid-switch"
            );
            assert_eq!(
                harness
                    .overrides
                    .get("s-codex-old")
                    .and_then(|meta| meta.right.clone())
                    .and_then(|right| right.browser_url),
                Some("https://b.example/".to_owned()),
                "no sync overwrites the destination's stored URL mid-switch"
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination landed");
            assert_eq!(view.read(cx).session_id, "s-codex-old", "the destination is open");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "no loading state outlives the landing"
            );
            assert!(
                !harness.pending_right_neutral(cx),
                "the pane leaves its neutral state with the landing"
            );
            assert!(
                crate::layout::Layout::get(cx).right_open,
                "the landed destination restores its own open pane"
            );
            assert_eq!(
                crate::layout::Layout::get(cx).right_kind,
                Some(crate::layout::RightKind::Files),
                "the landed destination restores its own kind, not the discarded toggle"
            );
        });
    });
    lane_restore(state);
}

/// Q4b (b): clicking the still-active session while its replacement
/// loads cancels the pending switch — the epoch moves, the loading
/// state drops — and never reopens it. Asserted at the `resume_inner`
/// decision site (exactly one `ResumeSession` ever dispatches, for the
/// discarded destination) and by the single surviving view. Before the
/// fix the click fell into `reopen_provider`, landing a second view
/// under the same id and orphaning the draft.
#[gpui::test]
fn q4b_clicking_the_active_session_cancels_without_reopening(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4b-stay");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    y2b2_set_draft(vc, &baaz, "hello unsent");
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    // The click on the stored session — observed before the reopen
    // lands, so no `run_until_parked` between this and the asserts.
    let pending_epoch = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.resume_quiet("s-codex-old".into(), window, cx);
            assert!(harness.open_state.switch_pending, "the provider switch is pending");
            harness.open_state.open_epoch()
        })
    });
    // Clicking the still-active session: cancel, stay, reopen nothing.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let epoch_before = harness.open_state.open_epoch();
            assert_eq!(epoch_before, pending_epoch, "no open raced the clicks");
            harness.resume_quiet(first.clone(), window, cx);
            assert!(
                harness.open_state.open_epoch() != epoch_before,
                "the cancel invalidates the in-flight open"
            );
            assert!(!harness.open_state.switch_pending, "the loading state drops");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "no destination outlives the cancel"
            );
            assert_eq!(
                harness.open_state.pending_id.as_deref(),
                Some(first.as_str()),
                "the selection follows the restored session"
            );
            let view = harness.active.clone().expect("the session stays open");
            assert_eq!(view.read(cx).session_id, first, "still the same view");
            assert_eq!(
                view.read(cx).draft_text(cx),
                "hello unsent",
                "the draft never leaves its view"
            );
        });
    });
    // The discarded open lands stale only now.
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the session is still open");
            assert_eq!(view.read(cx).session_id, first, "no switch ever landed");
            assert!(
                !harness.registry.is_parked("s-codex-old"),
                "the discarded destination parks no view"
            );
            assert!(
                !harness.registry.is_parked(&first),
                "staying put parks nothing either: exactly one view"
            );
            assert_eq!(
                view.read(cx).draft_text(cx),
                "hello unsent",
                "the draft survives the discarded landing"
            );
            assert!(harness.open_state.provider_open_error.is_none(), "no inline failure");
        });
    });
    let resumes: Vec<String> = commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::ResumeSession { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        resumes,
        vec!["s-codex-old".to_owned()],
        "exactly one resume dispatched — the click on the active session reopened nothing"
    );
    lane_restore(state);
}

/// Q4b (c): clicking another stored session while its reopen is
/// pending supersedes it — asserted at the epoch decision site — so
/// when the first open lands late the newer session stays. Both clicks
/// run through `reopen_provider`, which always joined the epoch, so
/// this passed even before the Q4c fix; it does NOT cover a pending
/// new-session, fork, or retry open, which set no `pending_id` and so
/// moved no epoch on the next click. Those paths are pinned by the
/// Q4c tests below, where the landing itself (`activate`) retires the
/// in-flight open.
#[gpui::test]
fn q4b_late_landing_never_steals_the_centre(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4b-supersede");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    y2b2_set_draft(vc, &baaz, "hello unsent");
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    // A second stored session with the opposite pane: the click target
    // that supersedes the first.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.provider_sessions.insert(
                "s-codex-second".into(),
                crate::provider_sessions::ProviderSessionRecord {
                    provider: "codex".into(),
                    session_id: "s-codex-second".into(),
                    workspace: None,
                    project: None,
                    created_ms: 1_700_000_000_001,
                    updated_ms: 1_700_000_000_001,
                    turns: 3,
                    title: Some("Second inquiry".into()),
                    first_prompt: Some("second prompt".into()),
                    handoff_to: None,
                    handoff_from: None,
                    handoff_from_provider: None,
                    handoff_title: None,
                    display_texts: std::collections::HashMap::new(),
                    origin: None,
                },
            );
            harness.merge_provider_rows();
            harness.set_override(
                "s-codex-second",
                |meta| {
                    meta.right = Some(crate::sessions::RightState {
                        open: false,
                        ..Default::default()
                    })
                },
                cx,
            );
        });
    });
    // The first click — observed before its reopen lands.
    let pending_epoch = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.resume_quiet("s-codex-old".into(), window, cx);
            assert!(harness.open_state.switch_pending, "the first switch is pending");
            harness.open_state.open_epoch()
        })
    });
    // The second click supersedes it: a newer epoch, the pane neutral
    // throughout (neither destination's pane applies at a click), the
    // old view still standing by.
    let superseding_epoch = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.resume_quiet("s-codex-second".into(), window, cx);
            assert!(harness.open_state.switch_pending, "the second switch is pending");
            assert_eq!(
                harness.open_state.pending_id.as_deref(),
                Some("s-codex-second"),
                "the selection follows the newer click"
            );
            assert!(
                harness.pending_right_neutral(cx),
                "the pane stays neutral across the superseding click"
            );
            assert!(
                !crate::layout::Layout::get(cx).right_open,
                "no click applied a destination pane: the live pane is still the first session's closed one"
            );
            assert!(
                harness
                    .overrides
                    .get("s-codex-old")
                    .and_then(|meta| meta.right.clone())
                    .is_some_and(|right| right.open),
                "the superseded destination's stored pane is untouched"
            );
            harness.open_state.open_epoch()
        })
    });
    assert!(
        superseding_epoch != pending_epoch,
        "the newer click invalidates the in-flight open"
    );
    // Both opens land; only the newer one may take the centre.
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("a session is open");
            assert_eq!(
                view.read(cx).session_id, "s-codex-second",
                "the newer click wins, not the late landing"
            );
            assert!(!harness.open_state.switch_pending, "the switch landed");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "no loading state outlives the landing"
            );
            assert!(
                !crate::layout::Layout::get(cx).right_open,
                "the pane is the winner's, never the late landing's"
            );
            let parked = harness
                .registry.parked(&first)
                .expect("the previous session parks intact");
            assert_eq!(
                parked.read(cx).draft_text(cx),
                "hello unsent",
                "the previous draft survives"
            );
            assert!(
                !harness.registry.is_parked("s-codex-old"),
                "the discarded destination parks no view"
            );
        });
    });
    let mut resumes: Vec<String> = commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::ResumeSession { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    resumes.sort();
    assert_eq!(
        resumes,
        vec!["s-codex-old".to_owned(), "s-codex-second".to_owned()],
        "both opens ran — the first landed stale and stole nothing"
    );
    lane_restore(state);
}

/// Q4c (a): a fork open pending (`open_forked_on_provider` sets no
/// `pending_id`, so `cancel_pending_switch` never sees it) still loses
/// to a cached session's landing — asserted at the `activate`
/// decision site, which retires every in-flight provider open. The
/// fork's late finish lands stale (its child shuts down, nothing
/// parks) and never steals the centre. Without the `activate` bump
/// the late landing takes the centre, so this fails on pre-fix code.
#[gpui::test]
fn q4c_fork_pending_loses_to_a_cached_landing(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4c-fork");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    // Two landed lanes: the second parks the first into the cache.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    // The fork starts — observed before it lands, so no
    // `run_until_parked` between this and the click.
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
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(harness.open_state.switch_pending, "the fork is pending");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "a fork pending names no destination: the old centre still shows"
            );
        });
    });
    // The click on the parked lane lands synchronously and retires
    // the fork.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(first.clone(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the cached lane is open");
            assert_eq!(
                view.read(cx).session_id,
                first,
                "the click lands on the cached lane"
            );
        });
    });
    // The fork lands late and steals nothing.
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("a session is open");
            assert_eq!(
                view.read(cx).session_id, first,
                "the late fork never steals the centre"
            );
            assert!(!harness.open_state.switch_pending, "nothing is still opening");
            assert!(
                !harness.registry.is_parked("s-fork"),
                "the discarded fork parks no view"
            );
            assert!(harness.open_state.provider_open_error.is_none(), "no inline failure");
        });
    });
    let resumes: Vec<String> = commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::ResumeSession { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        resumes,
        vec!["s-codex-old".to_owned(), "s-fork".to_owned()],
        "both resumes ran — the fork's finish landed stale and stole nothing"
    );
    lane_restore(state);
}

/// Q4c (b): a retried reopen pending (`retry_provider_open` re-runs
/// the failed open, which sets no `pending_id` while the inline
/// failure stands, so `cancel_pending_switch` never sees it) still
/// loses to a cached session's landing — asserted, like (a), at the
/// `activate` decision site. Without the `activate` bump the late
/// landing takes the centre, so this fails on pre-fix code.
#[gpui::test]
fn q4c_retry_pending_loses_to_a_cached_landing(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4c-retry");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    // The reopen fails for real: the window sits on the inline
    // failure with the first lane parked.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = failing_factory("reopen down");
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _cx| {
            assert!(
                harness.open_state.provider_open_error.is_some(),
                "the failed reopen stands inline"
            );
        });
    });
    // Retry re-runs the failed reopen — observed before it lands, so
    // no `run_until_parked` between this and the click.
    let (retry_factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = retry_factory;
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.retry_provider_open(window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(harness.open_state.switch_pending, "the retry is pending");
            assert!(
                harness.pending_switch_dest(cx).is_none(),
                "a retry pending names no destination while the failure stands"
            );
        });
    });
    // The click on the parked lane lands synchronously and retires
    // the retry.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(first.clone(), window, cx));
    });
    // The retry lands late and steals nothing.
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("a session is open");
            assert_eq!(
                view.read(cx).session_id, first,
                "the late retry never steals the centre"
            );
            assert!(!harness.open_state.switch_pending, "nothing is still opening");
            let cached: Vec<String> =
                harness.registry.parked_ids();
            assert_eq!(
                cached,
                vec!["s-codex-old".to_owned()],
                "only the failure-parked view remains: the discarded retry parks nothing new"
            );
        });
    });
    let resumes: Vec<String> = commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::ResumeSession { session_id, .. } => Some(session_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        resumes,
        vec!["s-codex-old".to_owned()],
        "the retry ran — its finish landed stale and stole nothing"
    );
    lane_restore(state);
}

/// Q4c (d): cancelling a pending reopen back to the still-open
/// session re-arms its composer focus — asserted on the
/// `focus_composer` arm the click consumed — and restores its pane
/// exactly as it was. Without the re-arm the flag stays consumed, so
/// this fails on pre-fix code.
#[gpui::test]
fn q4c_cancel_back_to_the_open_session_rearms_focus(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4c-cancel");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    let first = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the first lane opened").read(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.toggle_right(cx);
        });
    });
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    // The click on the stored session — observed before the reopen
    // lands, so no `run_until_parked` between this and the cancel.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _cx| {
            assert!(harness.open_state.switch_pending, "the provider switch is pending");
            assert!(!harness.focus_composer, "the click consumed the focus arm");
        });
    });
    // Clicking the still-active session cancels back to it.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(first.clone(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(!harness.open_state.switch_pending, "the loading state drops");
            assert!(
                harness.focus_composer,
                "cancelling back re-arms the restored session's composer focus"
            );
            let view = harness.active.clone().expect("the session stays open");
            assert_eq!(view.read(cx).session_id, first, "still the same view");
            assert!(
                crate::layout::Layout::get(cx).right_open,
                "the restored session's pane is exactly as it was"
            );
        });
    });
    // The discarded open lands stale only now.
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the session is still open");
            assert_eq!(view.read(cx).session_id, first, "no switch ever landed");
        });
    });
    lane_restore(state);
}

/// Q4b (d): while a provider reopen is pending, harness actions target
/// nothing — asserted at the `with_session` decision site every key
/// action (send included) routes through — and the hidden composer's
/// focus arm stays consumed, so no keystroke can reach it. Once the
/// destination lands, actions run again.
#[gpui::test]
fn q4b_actions_target_nothing_while_pending(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("q4b-quiet");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    y2b2_set_draft(vc, &baaz, "hello unsent");
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    // The click on the stored session — observed before the reopen
    // lands, so no `run_until_parked` between this and the asserts.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert!(harness.open_state.switch_pending, "the provider switch is pending");
            assert!(
                !harness.focus_composer,
                "the click blurs the hidden composer: no refocus arm"
            );
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let probe = fired.clone();
            harness.with_session(cx, move |_view, _vc| {
                probe.set(true);
            });
            assert!(
                !fired.get(),
                "no harness action reaches the hidden session while pending"
            );
            let view = harness.active.clone().expect("the old view is still open");
            assert_eq!(
                view.read(cx).draft_text(cx),
                "hello unsent",
                "no send touched the hidden draft"
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the destination landed");
            assert_eq!(view.read(cx).session_id, "s-codex-old", "the destination is open");
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let probe = fired.clone();
            harness.with_session(cx, move |_view, _vc| {
                probe.set(true);
            });
            assert!(fired.get(), "actions run again once the destination opens");
        });
    });
    lane_restore(state);
}

#[gpui::test]
fn switch_provider_replaces_the_fresh_draft_without_leaking_its_view(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("switch");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    // A fresh muse draft first: scripted chrome opens it locally.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session_in(None, window, cx));
    });
    let old = vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the draft opened");
        assert!(view.read(cx).provider_kind() == ProviderId::Muse, "the draft starts on the muse lane");
        view.read(cx).session_id.clone()
    });
    // The `SwitchProvider` handler's own sequence: remember the pick,
    // close the empty draft's view, start on the pick.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.open_state.drafts.retain(|_, id| *id != old);
            harness.close_view(&old, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the replacement opened");
        assert_ne!(view.read(cx).session_id, old, "the draft did not survive the switch");
        assert!(view.read(cx).provider_kind() != ProviderId::Muse, "the replacement rides the provider lane");
        assert!(
            !harness.registry.is_parked(&old),
            "the discarded draft was closed, never parked"
        );
        assert!(
            !harness.open_state.drafts.values().any(|id| id == &old),
            "the discarded draft names no project's draft anymore"
        );
    });
    lane_restore(state);
}

#[gpui::test]
fn switch_provider_through_the_handler_leaves_one_row_and_one_open(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("w6-switch");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, opens) = counting_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            harness.new_provider = ProviderId::Codex.as_str().to_owned();
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session(window, cx));
    });
    vc.run_until_parked();
    let draft = vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(opens_of(&opens), 1, "the draft opens one child");
        harness.active.clone().expect("the draft opened").read(cx).session_id.clone()
    });
    // Through the real `SwitchProvider` event, not the handler's steps
    // by hand: the close is synchronous, the replacement starts on a
    // task. Remove the claim (or the synchronous pending) and the
    // replacement never starts — or a following verb fails on the
    // closed draft.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the draft is open");
            harness.on_session_event(
                view,
                &SessionEvent::SwitchProvider { provider: ProviderId::ClaudeCode },
                cx,
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(opens_of(&opens), 2, "draft plus exactly one replacement");
        let view = harness.active.clone().expect("the replacement opened");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::ClaudeCode);
        // (No id inequality: the scripted double mints one fixed id,
        // so draft and replacement share it; the lane change above is
        // what proves the replacement.)
        let _ = draft;
        let rows: Vec<_> =
            harness.sessions.iter().filter(|entry| !entry.provider.lists_sessions()).collect();
        assert_eq!(rows.len(), 1, "exactly one provider row names the lane");
        assert_eq!(rows[0].id, view.read(cx).session_id, "the row is the replacement");
        assert!(!harness.open_state.switch_pending, "the switch landed");
    });
    lane_restore(state);
}

/// Y2b: `SwitchProvider` swaps the view in place. Until the replacement
/// is drawn (one task later — on the click, not on the open's landing)
/// the old view stays active (no empty-state frame: `active` is still
/// `Some` and the header still names the session), the composer refuses
/// sends and the chip already shows the pick; the replacement installs
/// in the same update that closes it, carrying the unsent draft.
/// Before the fix the handler closed the old view synchronously, so the
/// mid-switch `active` was `None` and the draft never carried.
#[gpui::test]
fn y2b_switch_keeps_old_view_until_open_lands_and_carries_draft(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b-inplace");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, opens) = counting_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            harness.new_provider = ProviderId::Codex.as_str().to_owned();
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session(window, cx));
    });
    vc.run_until_parked();
    let old = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the draft opened").read(cx).session_id.clone()
    });
    // An unsent draft waiting on the old view.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the draft is open");
            view.update(cx, |view, cx| view.set_draft("hello unsent".to_owned(), window, cx));
        });
    });
    // Through the real event: the replacement starts on a task.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the draft is open");
            harness.on_session_event(
                view,
                &SessionEvent::SwitchProvider { provider: ProviderId::ClaudeCode },
                cx,
            );
        });
    });
    // Before the open completes: no intermediate empty frame.
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("old view stays until the open lands");
        assert_eq!(view.read(cx).session_id, old, "still the old view mid-switch");
        assert_eq!(
            harness.open_state.pending_id.as_deref(),
            Some(old.as_str()),
            "the header still names the session mid-switch"
        );
        assert_eq!(harness.open_state.replacing_id(), Some(old.as_str()));
        assert!(view.read(cx).is_input_locked(), "composer input disabled mid-switch");
        assert_eq!(
            view.read(cx).display_provider(),
            ProviderId::ClaudeCode,
            "chip already shows the pick mid-switch"
        );
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(opens_of(&opens), 2, "draft plus exactly one replacement");
        let view = harness.active.clone().expect("the replacement opened");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::ClaudeCode);
        assert_eq!(view.read(cx).draft_text(cx), "hello unsent", "unsent draft carries across");
        assert!(!view.read(cx).is_input_locked(), "the new view takes sends");
        assert!(harness.open_state.replacing_id().is_none(), "the marker clears on landing");
        let total = usize::from(harness.active.is_some()) + harness.registry.parked_len();
        assert_eq!(total, 1, "exactly one view exists after the swap");
        assert!(!harness.open_state.switch_pending, "the switch landed");
    });
    lane_restore(state);
}

/// Y2b: a switch's replacement is drawn on the click, so a failed open
/// fails the way a failed New session does: the typed failure view for
/// the picked provider (QA 2026-10-30), with the carried text riding its
/// Retry — never a locked centre, never a dead draft under a banner.
#[gpui::test]
fn y2b_failed_open_fails_on_the_replacement_with_the_text(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b-fail");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = scripted_factory();
            mark_muse_up(harness);
            harness.new_provider = ProviderId::Codex.as_str().to_owned();
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session(window, cx));
    });
    vc.run_until_parked();
    let old = vc.update(|window, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane opened");
        view.update(cx, |view, cx| view.set_draft("hello unsent".to_owned(), window, cx));
        view.read(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = failing_factory("no child here");
        });
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the lane is open");
            harness.on_session_event(
                view,
                &SessionEvent::SwitchProvider { provider: ProviderId::ClaudeCode },
                cx,
            );
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "no dead replacement in the centre");
        assert!(harness.find_view(&old, cx).is_none(), "the old draft closed");
        let failure = harness.open_state.provider_open_error.clone().expect("the typed failure view");
        assert_eq!(failure.provider, ProviderId::ClaudeCode, "it names the picked provider");
        assert!(failure.message.contains("no child here"), "the reason shows: {}", failure.message);
        assert!(
            matches!(&failure.retry, ProviderOpenRetry::OpenNew { draft, .. } if draft == "hello unsent"),
            "the text survives the failure, on Retry"
        );
        assert!(harness.open_state.replacing_id().is_none(), "the marker is gone");
        assert!(!harness.open_state.switch_pending, "the verbs are released");
        assert!(harness.registry.parked_len() == 0, "the old draft closed");
    });
    lane_restore(state);
}

/// Fire a real `SwitchProvider` event on the active view.
fn y2b2_switch(vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>, provider: ProviderId) {
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the old view is open");
            harness.on_session_event(view, &SessionEvent::SwitchProvider { provider }, cx);
        });
    });
}

fn y2b2_set_draft(vc: &mut gpui::VisualTestContext, baaz: &gpui::Entity<Harness>, text: &str) {
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("a view is open");
            view.update(cx, |view, cx| view.set_draft(text.to_owned(), window, cx));
        });
    });
}

/// Y2b2 case 1, now that a switch of an unsent draft draws its
/// replacement on the click: the replacement is the centre at once —
/// the old draft closed with its child hung up, its text carried — so
/// a click on another session parks the replacement like any draft
/// whose child is still answering. The open then lands on the parked
/// draft (its child kept, no second view), and the old draft persists
/// nowhere.
#[gpui::test]
fn y2b2_clicking_another_session_parks_the_switched_draft(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b2-click");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let first = y2b2_open(vc, &baaz, ProviderId::Codex);
    // A repeated ⌘N would reopen the first lane's still-unsent draft
    // instead of spawning another child (see `finish_provider_open`):
    // drop its draft-name so the second open is a genuine second lane.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.open_state.drafts.retain(|_, id| *id != first);
        });
    });
    let second = y2b2_open(vc, &baaz, ProviderId::Codex);
    assert_ne!(first, second, "the setup holds two distinct lanes");
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(first.clone(), window, cx));
    });
    vc.run_until_parked();
    y2b2_set_draft(vc, &baaz, "hello unsent");
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    y2b2_drain_until_opens(vc, &shared, 3);
    let switched = vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the replacement is the centre");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::ClaudeCode, "drawn on the click");
        assert_eq!(view.read(cx).draft_text(cx), "hello unsent", "the typed text carried");
        assert!(harness.open_state.replacing_id().is_none(), "nothing waits to be replaced");
        assert!(!harness.registry.is_parked(&first), "the old draft closed");
        view
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(second.clone(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the clicked lane is open");
        assert_eq!(view.read(cx).session_id, second, "the click wins");
        assert!(!harness.open_state.switch_pending, "the verbs are released");
        assert!(
            harness.registry.is_view_parked(&switched),
            "the replacement parks like any draft"
        );
        assert_eq!(switched.read(cx).draft_text(cx), "hello unsent", "its text stays with it");
        assert!(!switched.read(cx).is_opening_lane(), "its open landed on the parked draft");
        assert!(
            !harness.provider_sessions.contains_key(&first),
            "the replaced draft persists nowhere"
        );
    });
    let landed = vc.update(|_, cx| switched.read(cx).session_id.clone());
    let locked = shared.inner.lock().expect("y2b2");
    assert_eq!(locked.created.len(), 3, "both lanes plus exactly one switch open");
    assert_eq!(locked.shutdowns, vec![first.clone()], "only the replaced draft's child hangs up");
    assert!(!locked.shutdowns.contains(&landed));
    drop(locked);
    lane_restore(state);
}

/// Switching an unsent draft's provider is as instant as New session:
/// before the new child answers, the replacement is the centre on the
/// picked provider, carrying the typed text, the mode and the effort,
/// and none of the old provider's banners.
#[gpui::test]
fn a_draft_switch_draws_the_new_provider_before_its_child_answers(cx: &mut gpui::TestAppContext) {
    use aui_protocol::{PermissionMode, ReasoningEffort};
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("switch-instant");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let old = y2b2_open(vc, &baaz, ProviderId::Codex);
    y2b2_set_draft(vc, &baaz, "carry me");
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the draft");
        view.update(cx, |view, cx| {
            view.set_mode(PermissionMode::DenyUnmatched, cx);
            view.set_initial_effort(Some(ReasoningEffort::High), cx);
            view.show_disabled_notice(ProviderId::Codex, cx);
        });
    });
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    // The open has started, and its answer has not been folded.
    y2b2_drain_until_opens(vc, &shared, 2);
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the replacement is the centre");
        let view = view.read(cx);
        assert_ne!(view.session_id, old);
        assert!(view.is_opening_lane(), "drawn before the child answers");
        assert_eq!(view.display_provider(), ProviderId::ClaudeCode);
        assert_eq!(view.draft_text(cx), "carry me");
        assert_eq!(view.draft_settings(), (PermissionMode::DenyUnmatched, Some(ReasoningEffort::High)));
        assert_eq!(view.banner_text(), None, "no old-provider banner");
        assert!(harness.registry.parked_len() == 0, "the old draft closed");
    });
    assert_eq!(shared.inner.lock().expect("y2b2").shutdowns, vec![old], "its child hung up");
    vc.run_until_parked();
    lane_restore(state);
}

/// Y2b2 case 2: ⌘N while the switched draft's child is still answering
/// reopens that draft — the project's draft, on the picked provider —
/// rather than spawning a second child.
#[gpui::test]
fn y2b2_new_session_reopens_the_switched_draft(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b2-new");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let old = y2b2_open(vc, &baaz, ProviderId::Codex);
    y2b2_set_draft(vc, &baaz, "hello unsent");
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    y2b2_drain_until_opens(vc, &shared, 2);
    let switched = vc.update(|_, cx| baaz.read(cx).active.clone().expect("the replacement"));
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session(window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the draft is open");
        assert!(view == switched, "the switched draft, not a new one");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::ClaudeCode);
        assert_eq!(view.read(cx).draft_text(cx), "hello unsent");
        assert!(harness.open_state.replacing_id().is_none());
        assert!(!harness.open_state.switch_pending, "the verbs are released");
        assert!(harness.registry.parked_len() == 0, "the old draft closed, nothing parks");
        assert!(!harness.provider_sessions.contains_key(&old), "the old draft persists nowhere");
    });
    let locked = shared.inner.lock().expect("y2b2");
    assert_eq!(locked.created.len(), 2, "the lane plus the switch's open, no third");
    assert_eq!(locked.shutdowns, vec![old], "only the replaced draft's child hangs up");
    drop(locked);
    lane_restore(state);
}

/// Y2b2 case 3: two quick switches supersede cleanly — the second
/// switch opens (the first task never steals its claim), the first
/// open is discarded, and the chip and the landed session match the
/// last pick. Before the fix the first task consumed the shared claim,
/// so the second switch never opened at all.
#[gpui::test]
fn y2b2_second_switch_supersedes_the_first(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b2-twice");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let old = y2b2_open(vc, &baaz, ProviderId::Codex);
    y2b2_set_draft(vc, &baaz, "hello unsent");
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    y2b2_drain_until_opens(vc, &shared, 2);
    // The second pick while the first is still pending.
    y2b2_switch(vc, &baaz, ProviderId::Codex);
    y2b2_drain_until_opens(vc, &shared, 3);
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the replacement opened");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex, "the last pick wins");
        assert_eq!(harness.new_provider, ProviderId::Codex.as_str(), "the chip matches the lane");
        assert_eq!(
            view.read(cx).draft_text(cx),
            "hello unsent",
            "the draft carries into the last pick"
        );
        assert!(harness.open_state.replacing_id().is_none(), "the marker is gone");
        assert!(!harness.open_state.switch_pending, "the verbs are released");
        assert!(harness.registry.parked_len() == 0, "nothing parks");
        let records: Vec<String> = harness.provider_sessions.keys().cloned().collect();
        assert_eq!(
            records,
            vec![view.read(cx).session_id.clone()],
            "only the landed session persists, drew {records:?}"
        );
    });
    let locked = shared.inner.lock().expect("y2b2");
    assert_eq!(locked.created.len(), 3, "the lane plus one open per switch");
    let live = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the replacement opened").read(cx).session_id.clone()
    });
    assert!(!locked.shutdowns.contains(&live), "the landed child is never shut down");
    assert_eq!(locked.shutdowns.len(), 2, "the replaced lane and the discarded open shut down");
    assert_ne!(live, old, "the replacement is a fresh session");
    drop(locked);
    lane_restore(state);
}

/// Y2b2: closing the switched draft while its child is still answering
/// leaves the centre empty; the open lands on no view, so its child is
/// hung up and nothing persists.
#[gpui::test]
fn y2b2_closing_the_switched_draft_discards_its_open(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b2-close");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let old = y2b2_open(vc, &baaz, ProviderId::Codex);
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    y2b2_drain_until_opens(vc, &shared, 2);
    let switched = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the replacement").read(cx).session_id.clone()
    });
    assert_ne!(switched, old);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.close_view(&switched, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "the centre stays empty");
        assert!(harness.registry.parked_len() == 0, "nothing parks");
        assert!(harness.open_state.replacing_id().is_none(), "the marker is gone");
        assert!(!harness.open_state.switch_pending, "the verbs are released");
        assert!(
            harness.provider_sessions.is_empty(),
            "neither the closed lane nor the switch persists"
        );
    });
    let locked = shared.inner.lock().expect("y2b2");
    assert_eq!(locked.created.len(), 2, "the lane plus one switch open");
    assert_eq!(locked.shutdowns.len(), 2, "the replaced lane and the discarded open shut down");
    drop(locked);
    lane_restore(state);
}

/// docs/81 A F6: quit hangs up a browser chat's child too.
#[gpui::test]
fn quit_hangs_up_the_browser_chat(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("f6-chat-quit");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let chat = y2b2_open(vc, &baaz, ProviderId::Codex);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.take().expect("the lane opened");
            harness.set_browser_chat(view, cx);
            assert!(harness.active.is_none() && harness.registry.parked_len() == 0, "held by the line alone");
            harness.shutdown_provider_views(cx);
        });
    });
    vc.run_until_parked();
    let locked = shared.inner.lock().expect("y2b2");
    assert!(locked.shutdowns.contains(&chat), "the chat's child hangs up at quit, drew {:?}", locked.shutdowns);
    drop(locked);
    lane_restore(state);
}

/// Y2b2: quitting (which hangs up every lane child) while the switched
/// draft's child is still answering: an open that lands after teardown
/// is hung up, never attached.
#[gpui::test]
fn y2b2_quit_discards_the_switched_drafts_open(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("y2b2-quit");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let old = y2b2_open(vc, &baaz, ProviderId::Codex);
    y2b2_switch(vc, &baaz, ProviderId::ClaudeCode);
    y2b2_drain_until_opens(vc, &shared, 2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.shutdown_provider_views(cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the replacement is still the centre");
        assert!(view.read(cx).is_opening_lane(), "no late open attaches after quit");
        assert!(harness.open_state.replacing_id().is_none(), "the marker is gone");
        assert!(!harness.open_state.switch_pending, "the verbs are released");
    });
    let locked = shared.inner.lock().expect("y2b2");
    assert_eq!(locked.created.len(), 2, "the lane plus one switch open");
    assert!(locked.shutdowns.contains(&old), "the replaced lane hangs up, drew {:?}", locked.shutdowns);
    assert_eq!(locked.shutdowns.len(), 2, "plus the discarded open, drew {:?}", locked.shutdowns);
    drop(locked);
    lane_restore(state);
}

#[gpui::test]
fn a_late_finishing_earlier_open_never_steals_the_lane(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("w6-epoch");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, opens) = counting_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    // Two asks back to back on different lanes; whichever background
    // open finishes first, the Codex ask is stale. Remove the epoch
    // and both finishes land: two rows, and the active lane is
    // whichever happened to finish last.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_on_provider(ProviderId::Codex, None, workspace.clone(), window, cx);
            harness.open_on_provider(ProviderId::ClaudeCode, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(opens_of(&opens), 2, "both asks spawned");
        let view = harness.active.clone().expect("a lane opened");
        assert_eq!(
            view.read(cx).provider_kind(),
            ProviderId::ClaudeCode,
            "the last ask wins, however the finishes order"
        );
        let rows: Vec<_> =
            harness.sessions.iter().filter(|entry| !entry.provider.lists_sessions()).collect();
        assert_eq!(rows.len(), 1, "the stale finish leaves no row");
        assert_eq!(rows[0].id, view.read(cx).session_id, "the row is the winner");
        assert!(!harness.open_state.switch_pending, "the switch landed");
    });
    lane_restore(state);
}

#[gpui::test]
fn closing_a_provider_view_hangs_up_its_child(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("close");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.new_session_in(None, window, cx);
        });
    });
    let open_id = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| harness.close_view(&open_id, cx));
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "the closed view is gone from the centre");
        assert!(
            !harness.registry.is_parked(&open_id),
            "the closed view is gone from the MRU too"
        );
    });
    // The shutdown itself is proven in the lane tests (dropping a lane
    // view hangs up its child: a later command is refused as shut
    // down); here the close path above called it outright.
    lane_restore(state);
}
