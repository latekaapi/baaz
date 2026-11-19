//! Tests for opening: drafts, new sessions, the parked MRU, the tier probe on open, model and mode defaults.

use super::*;

fn draft_map(names: &[(&str, &str)]) -> HashMap<String, String> {
    names.iter().map(|(p, s)| ((*p).to_owned(), (*s).to_owned())).collect()
}

#[test]
fn a_project_without_a_draft_starts() {
    let drafts = draft_map(&[]);
    assert_eq!(draft_decision(&drafts, "p", |_| panic!("no name to check")), DraftDecision::Start {
        stale: None
    });
}

#[test]
fn a_live_draft_reopens_and_a_dead_name_is_pruned() {
    let drafts = draft_map(&[("p", "s-draft")]);
    assert_eq!(draft_decision(&drafts, "p", |id| id == "s-draft"), DraftDecision::Reuse("s-draft".into()));
    assert_eq!(
        draft_decision(&drafts, "p", |_| false),
        DraftDecision::Start { stale: Some("s-draft".into()) }
    );
}

#[test]
fn drafts_are_per_project() {
    // Two projects' drafts never answer for each other, and a first
    // turn clears only its own session's name.
    let mut drafts = draft_map(&[("a", "s-a"), ("b", "s-b")]);
    assert_eq!(draft_decision(&drafts, "a", |_| true), DraftDecision::Reuse("s-a".into()));
    assert_eq!(draft_decision(&drafts, "b", |_| true), DraftDecision::Reuse("s-b".into()));
    drafts.retain(|_, named| named != "s-a");
    assert_eq!(drafts.get("a"), None);
    assert_eq!(drafts.get("b").map(String::as_str), Some("s-b"));
}

#[test]
fn parked_drafts_survive_the_mru_cap() {
    use std::collections::HashSet;
    let ids: Vec<String> = (0..10).map(|n| format!("s-{n}")).collect();
    let drafts: HashSet<String> = ["s-9"].iter().map(|s| (*s).to_owned()).collect();
    // The oldest non-draft past the cap of eight goes; the draft at the
    // tail stays, wherever it sits.
    assert_eq!(
        evict_parked(&ids, &drafts, 8),
        ["s-0", "s-1", "s-2", "s-3", "s-4", "s-5", "s-6", "s-7", "s-9"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect::<Vec<_>>()
    );
    // With no drafts the cap is a plain truncate.
    assert_eq!(evict_parked(&ids, &HashSet::new(), 8), ids[..8].to_owned());
}

/// A signed-in window over a state dir and an `auth.json` of its own.
/// Holds `MUSE_AUTH_PATH` until the guard drops; tests never read the
/// person's credential to decide a plan.
struct TierWindow {
    state: Option<(std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf)>,
    old_auth: Option<std::ffi::OsString>,
}
impl Drop for TierWindow {
    fn drop(&mut self) {
        match self.old_auth.take() {
            Some(path) => std::env::set_var("MUSE_AUTH_PATH", path),
            None => std::env::remove_var("MUSE_AUTH_PATH"),
        }
        if let Some(state) = self.state.take() {
            lane_restore(state);
        }
    }
}

fn tier_window<'a>(
    name: &str,
    auth_json: &str,
    cx: &'a mut gpui::TestAppContext,
) -> (TierWindow, &'a mut gpui::VisualTestContext, gpui::Entity<Harness>) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state(name);
    let auth = state.2.join("auth.json");
    std::fs::write(&auth, auth_json).expect("the fixture auth.json");
    let old_auth = std::env::var_os("MUSE_AUTH_PATH");
    std::env::set_var("MUSE_AUTH_PATH", &auth);
    let dir = state.2.clone();
    let guard = TierWindow { state: Some(state), old_auth };
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &dir);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.auth = Auth::SignedIn(crate::auth::Identity {
                lane: provider::AccountLane::AccountLogin,
                name: "Ada".into(),
                email: "ada@example.com".into(),
            });
        });
    });
    (guard, vc, baaz)
}

fn high_usage() -> Tier {
    Tier::Subscription {
        plan: "Muse Code High Usage".into(),
        current_pct: Some(2),
        weekly_pct: Some(2),
        resets: vec![],
        usage_unavailable: false,
    }
}

const SIGNED_IN_AUTH: &str = r#"{"providers":{"meta":{"user_email":"ada@example.com","subs_active":true}}}"#;

/// The owner's window, 2026-11-04. A subscribed login whose token has
/// since refreshed (the remembered mtime no longer matches, so the cache
/// answers nothing and a probe runs), and that probe gets no verdict. It
/// used to land `Tier::Unavailable` with the banner on every Muse
/// session until "Check again". The plan stands, no banner.
#[gpui::test]
fn a_failed_probe_keeps_a_confirmed_plan_and_raises_no_banner(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) = tier_window("tier-kept", SIGNED_IN_AUTH, cx);
    crate::tier::remember_at(&high_usage(), Some(1), crate::tier::now_secs());
    assert_eq!(crate::tier::cached(), None, "the refreshed token invalidated the cache");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.land_tier_probe(Err("the /upgrade card did not answer in time".into()), false, cx);
            assert_eq!(harness.tier, Some(high_usage()));
            assert_eq!(harness.tier_banner(), None, "a transient unknown raises no banner");
        });
    });
    let back: crate::tier::Cached = crate::store::read_json(&crate::tier::cache_path());
    assert_eq!(back.tier, Some(high_usage()), "and does not overwrite what was known");
}

/// The same failure on a login nothing ever confirmed: the banner.
#[gpui::test]
fn a_failed_probe_on_a_login_never_confirmed_still_banners(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) = tier_window("tier-unknown", SIGNED_IN_AUTH, cx);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.land_tier_probe(Err("no card".into()), false, cx);
            let banner = harness.tier_banner().expect("nobody knows this login's plan");
            assert!(banner.text.starts_with("Muse did not say which plan"), "{}", banner.text);
            assert!(!banner.blocking);
        });
    });
}

/// Pay-as-you-go still warns: as a verdict over a known subscription,
/// and through a failed re-verify of it.
#[gpui::test]
fn pay_as_you_go_still_warns(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) = tier_window("tier-payg", SIGNED_IN_AUTH, cx);
    crate::tier::remember_at(&high_usage(), Some(1), crate::tier::now_secs());
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.land_tier_probe(Ok(Tier::PayAsYouGo), false, cx);
            assert!(harness.tier_banner().is_some_and(|banner| banner.blocking), "the verdict lands");
            harness.land_tier_probe(Err("no card".into()), false, cx);
            assert_eq!(harness.tier, Some(Tier::PayAsYouGo));
            assert!(harness.tier_banner().is_some_and(|banner| banner.blocking), "a failure does not lift it");
        });
    });
}

/// Muse's own flag says the subscription lapsed: the remembered plan is
/// no longer known, so a failed probe banners rather than vouching.
#[gpui::test]
fn a_lapsed_subscription_is_not_kept(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) =
        tier_window("tier-lapsed", r#"{"providers":{"meta":{"subs_active":false}}}"#, cx);
    crate::tier::remember_at(&high_usage(), Some(1), crate::tier::now_secs());
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.land_tier_probe(Err("no card".into()), false, cx);
            assert!(harness.tier_banner().is_some());
        });
    });
}

/// The re-probe runs by itself on the backoff's clock, then stops: ten
/// idle hours run no more. Each failure stays a failure (a test's probe
/// never reaches a CLI: `billing::run_probe`).
#[gpui::test]
fn a_failed_probe_retries_on_the_backoff_and_stops(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) = tier_window("tier-backoff", SIGNED_IN_AUTH, cx);
    crate::tier::remember_at(&high_usage(), Some(1), crate::tier::now_secs());
    let failures = |vc: &mut gpui::VisualTestContext| vc.update(|_, cx| baaz.read(cx).tier_retry.failures());
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.land_tier_probe(Err("no card".into()), false, cx)));
    assert_eq!(failures(vc), 1);
    let mut seen = Vec::new();
    for delay in crate::tier::PROBE_RETRY_BACKOFF {
        vc.executor().advance_clock(delay);
        vc.run_until_parked();
        seen.push(failures(vc));
    }
    assert_eq!(seen, vec![2, 3, 4]);
    vc.executor().advance_clock(std::time::Duration::from_secs(10 * 3600));
    vc.run_until_parked();
    assert_eq!(failures(vc), 4, "the backoff is spent: no loop");
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.tier_retry_timer.is_none());
        assert_eq!(harness.tier, Some(high_usage()));
    });
    // A wake starts it over.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.tier_probe_on_wake(cx)));
    vc.executor().advance_clock(crate::tier::PROBE_AFTER_WAKE);
    vc.run_until_parked();
    assert_eq!(failures(vc), 1, "the wake's own probe, on a fresh backoff");
    // A verdict ends it, and the pending timer with it.
    vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.land_tier_probe(Ok(high_usage()), false, cx)));
    vc.executor().advance_clock(std::time::Duration::from_secs(3600));
    vc.run_until_parked();
    assert_eq!(failures(vc), 0);
}

/// A Muse lane opening asks again only once the gap has passed.
#[gpui::test]
fn a_muse_lane_open_reprobes_only_past_the_gap(cx: &mut gpui::TestAppContext) {
    let (_guard, vc, baaz) = tier_window("tier-occasion", SIGNED_IN_AUTH, cx);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.land_tier_probe(Err("no card".into()), false, cx);
            harness.tier_probe_on_muse_lane(cx);
            assert!(!harness.tier_probing, "just failed: walking through sessions drives no TUI");
            harness.tier_retry = crate::tier::ProbeRetry::default();
            let long_ago = crate::tier::now_secs() - crate::tier::PROBE_OCCASION_GAP.as_secs();
            harness.tier_retry.failed(long_ago);
            harness.tier_probe_on_muse_lane(cx);
            assert!(harness.tier_probing, "owed and past the gap");
        });
    });
    vc.run_until_parked();
}

/// "New session" shows its draft in the click's own update, before any
/// round trip: the id is minted locally and the view is up while
/// `session/start` is still on the wire. When the start lands, the row
/// goes in from the reply itself — no `session/list` is ever sent for
/// it. Restore the wait (open in the landing) and the first block
/// finds the old centre; restore `load_sessions` there and the log
/// names `session/list`.
#[gpui::test]
fn a_new_session_opens_on_the_click_and_its_row_needs_no_list(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("new-early");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let log = state.2.join("requests.log");
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            install_fake_muse(harness, &state.2, &state.2, &log, None);
            adopt_for_muse(harness, &state.2, cx)
        })
    });
    let minted = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let view = harness.active.clone().expect("the draft is up before the start lands");
            let id = view.read(cx).rt(cx).session_id.clone();
            assert!(view.read(cx).rt(cx).is_starting(), "its start is still on the wire");
            assert!(harness.open_state.starts_in_flight.contains(&id), "scripted verbs wait for it");
            assert!(!harness.open_state.switch_pending, "a new session is no switch");
            assert_eq!(harness.open_state.drafts.get(&project), Some(&id), "a second click reuses it");
            id
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the draft stays open");
        assert_eq!(view.read(cx).rt(cx).session_id, minted, "the start kept the minted id");
        assert!(!view.read(cx).rt(cx).is_starting(), "the start landed");
        assert!(harness.open_state.starts_in_flight.is_empty(), "nothing is still opening");
        let row = harness.session_list.read(cx).sessions.iter().find(|entry| entry.id == minted).expect("the row is in");
        assert_eq!(row.project.as_deref(), Some(project.as_str()), "it files under its project");
        assert_eq!(row.turns, 0, "zero turns: out of view until its first send");
        assert!(!row.running, "opened, not started: a draft that shows the row must not read Working");
        assert!(harness.overlays.read(cx).dialog().is_none(), "no failure dialog");
    });
    let requests = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(requests.contains("\"session/start\""), "the start was sent: {requests}");
    assert!(!requests.contains("session/list"), "no list round trip for one row: {requests}");
    lane_restore(state);
}

/// A start that fails lands the typed failure view (QA 2026-10-30), not
/// the dead draft under a banner — and forgets it as the project's
/// draft, so the next "New session" starts fresh instead of reopening a
/// session that is not.
#[gpui::test]
fn a_failed_early_start_keeps_the_view_and_frees_the_draft(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("new-early-fail");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            mark_muse_up(harness);
            adopt_for_muse(harness, &state.2, cx)
        })
    });
    let minted = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            harness.active.clone().expect("the draft is up").read(cx).rt(cx).session_id.clone()
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "no dead draft in the centre");
        let failure = harness.open_state.provider_open_error.clone().expect("the typed failure view");
        assert_eq!(failure.provider, ProviderId::Muse);
        assert!(harness.find_view(&minted, cx).is_none(), "the dead draft is parked nowhere");
        assert!(harness.open_state.starts_in_flight.is_empty(), "scripted verbs are released");
        assert!(!harness.open_state.drafts.contains_key(&project), "the next New session starts fresh");
        assert!(!harness.session_list.read(cx).sessions.iter().any(|entry| entry.id == minted), "no row for it");
    });
    lane_restore(state);
}

/// docs/81 F C1 / docs/82 0.9: a new session with no project starts in
/// the default workspace on every provider — never in the launch
/// directory (`/` from Finder).
#[gpui::test]
fn an_unfiled_session_never_starts_in_the_launch_directory(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    for provider in [ProviderId::Codex, ProviderId::ClaudeCode, ProviderId::Muse] {
        let state = lane_state(&format!("unfiled-home-{}", provider.as_str()));
        let vc = cx.add_empty_window();
        let baaz = lane_harness(vc, &state.2);
        vc.update(|_, cx| {
            baaz.update(cx, |harness, cx| {
                harness.providers = harness.providers.with_opens_of(&parking_factory());
                mark_muse_up(harness);
                // A Finder launch: no `--workspace`, cwd `/`.
                harness.args.workspace = std::path::PathBuf::from("/");
                harness.args.workspace_explicit = false;
                harness.current_project = None;
                // First run: nothing adopted, so nothing is current.
                harness.session_list.update(cx, |list, _| list.projects.current = None);
                harness.session_list.update(cx, |list, _| list.projects.projects.clear());
                harness.new_provider = provider.as_str().to_owned();
            });
        });
        vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
        vc.run_until_parked();
        let workspace = vc.update(|_, cx| {
            baaz.read(cx).active.clone().map(|view| view.read(cx).rt(cx).workspace_for_tests().to_owned())
        });
        let home = projects::default_workspace().to_string_lossy().into_owned();
        assert_eq!(workspace.as_deref(), Some(home.as_str()), "{provider:?} starts in the default workspace");
        lane_restore(state);
    }
}

/// docs/82 0.14: ⌘Q with a parked session's turn running asks first and
/// names it; with nothing running it quits without asking.
#[gpui::test]
fn quit_asks_when_a_turn_is_running(cx: &mut gpui::TestAppContext) {
    let state = lane_state("quit-turn-running");
    let (factory, _commands) = recording_resumable_factory();
    let (baaz, vc, _project) = provider_draft_window(cx, &state.2, factory);
    let asked_idle = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.maybe_confirm_quit(cx)));
    assert!(!asked_idle, "nothing running: no question");
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let host = SessionHost {
                provider_id: ProviderId::Codex.as_str().to_owned(),
                workspace: harness.workspace(cx),
                overlays: harness.overlays.clone(),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            let view = cx.new(|cx| SessionView::new("s-quit".to_owned(), host, window, cx));
            view.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, cx| rt.fold_provider_events(
                    vec![provider::ProviderEvent::TurnRunning { session_id: "s-quit".into(), turn_id: "t".into() }],
                    cx,
                ));
            });
            harness.park_active(cx);
            harness.active = Some(view);
            harness.park_active(cx);
        });
    });
    let asked = vc.update(|_, cx| baaz.update(cx, |harness, cx| harness.maybe_confirm_quit(cx)));
    assert!(asked, "a running turn asks first");
    let (action, title) = vc
        .update(|_, cx| baaz.read(cx).overlays.read(cx).dialog().map(|d| (d.action, d.title.to_string())))
        .expect("a dialog");
    assert_eq!(action, crate::overlays::DialogAction::QuitWithWorkRunning);
    assert_eq!(title, "Quit while an agent is working?");
    lane_restore(state);
}

/// docs/81 A F1: a parked session with a turn running is never evicted —
/// shutting its lane down would kill the child mid-turn. Nine later
/// sessions park behind it: past the cap it loses its view, never its
/// runtime, which is still alive and settles (docs/82 4.8).
#[gpui::test]
fn a_busy_parked_session_is_never_evicted(cx: &mut gpui::TestAppContext) {
    let state = lane_state("busy-never-evicted");
    let (factory, _commands) = recording_resumable_factory();
    let (baaz, vc, _project) = provider_draft_window(cx, &state.2, factory);
    let busy_id = "s-busy".to_owned();
    let busy = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let host = SessionHost {
                provider_id: ProviderId::Codex.as_str().to_owned(),
                workspace: harness.workspace(cx),
                overlays: harness.overlays.clone(),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            let view = cx.new(|cx| SessionView::new(busy_id.clone(), host, window, cx));
            view.update(cx, |view, cx| {
                view.runtime().clone().update(cx, |rt, cx| rt.fold_provider_events(
                    vec![provider::ProviderEvent::TurnRunning {
                        session_id: busy_id.clone(),
                        turn_id: "t-busy".into(),
                    }],
                    cx,
                ));
            });
            assert!(view.read(cx).rt(cx).is_sending(), "setup: a turn is running");
            harness.open_state.drafts.clear();
            harness.park_active(cx);
            harness.active = Some(view.clone());
            for n in 0..=SESSION_CACHE_LIMIT {
                let host = SessionHost {
                    provider_id: ProviderId::Codex.as_str().to_owned(),
                    workspace: harness.workspace(cx),
                    overlays: harness.overlays.clone(),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                };
                let other = cx.new(|cx| SessionView::new(format!("s-idle-{n}"), host, window, cx));
                harness.park_active(cx);
                harness.active = Some(other);
            }
            harness.park_active(cx);
            assert!(
                harness.registry.is_parked(&busy_id),
                "the busy session survives {} later parks",
                SESSION_CACHE_LIMIT + 1
            );
            let idle = harness.registry.parked_ids().iter().filter(|id| id.starts_with("s-idle-")).count();
            assert_eq!(idle, SESSION_CACHE_LIMIT, "idle views are still capped");
            // Past the cap the busy session keeps no view — views are
            // drawing state (docs/82 4.8) — but its runtime runs on.
            assert!(harness.registry.parked(&busy_id).is_none(), "its view went with the cap");
            let runtime = harness.registry.find_runtime(None, &busy_id, cx).expect("its runtime is kept");
            assert_eq!(runtime, *view.read(cx).runtime(), "the same session, not a new one");
            assert!(runtime.read(cx).is_sending(), "dropping the view never stopped the turn");
            view
        })
    });
    vc.update(|_, cx| {
        busy.update(cx, |view, cx| {
            view.runtime().clone().update(cx, |rt, cx| rt.fold_provider_events(
                vec![provider::ProviderEvent::TurnSettled {
                    session_id: "s-busy".into(),
                    turn_id: "t-busy".into(),
                    outcome: provider::facts::TurnOutcome::Completed,
                }],
                cx,
            ));
        });
    });
    vc.run_until_parked();
    assert!(vc.update(|_, cx| !busy.read(cx).rt(cx).is_sending()), "the parked turn settles");
    // Settled and view-less, it has nothing left in hand: the next park
    // shuts it down rather than keeping an idle child until quit.
    let runtime = vc.update(|_, cx| busy.read(cx).runtime().clone());
    drop(busy);
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let host = SessionHost {
                provider_id: ProviderId::Codex.as_str().to_owned(),
                workspace: harness.workspace(cx),
                overlays: harness.overlays.clone(),
                capture: crate::runtime::CaptureToken::default(),
                terminal_host: None,
            };
            let next = cx.new(|cx| SessionView::new("s-after".to_owned(), host, window, cx));
            harness.active = Some(next);
            harness.park_active(cx);
            assert!(!harness.registry.is_parked(&busy_id), "the idle view-less runtime goes at the next park");
            assert!(harness.registry.find_runtime(None, &busy_id, cx).is_none(), "nothing holds it any more");
            let idle = harness.registry.parked_ids().iter().filter(|id| id.starts_with("s-idle-")).count();
            assert_eq!(idle, SESSION_CACHE_LIMIT - 1, "the views keep their cap");
        })
    });
    drop(runtime);
    lane_restore(state);
}

/// New session, open another session, New session again — twice over:
/// every New session ends on an unsent draft in the centre, never on
/// the session clicked before it. Run under a muse default and a Codex
/// default, the "another" being a stored Codex session either way.
fn new_after_open_lands_a_draft(cx: &mut gpui::TestAppContext, name: &str, provider: ProviderId) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state(name);
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let log = state.2.join("requests.log");
    let (factory, _) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.providers = harness.providers.with_opens_of(&factory);
            install_fake_muse(harness, &state.2, &state.2, &log, None);
            let project = adopt_for_muse(harness, &state.2, cx);
            harness.new_provider = provider.as_str().to_owned();
            z4_seed_stored_codex(harness, cx);
            // The live case: a handoff destination in this project, its
            // one turn the hidden pack acknowledgement — so its row
            // reads zero turns, like a draft's.
            harness.session_list.update(cx, |list, _| {
                let record = list.provider_sessions.get_mut("s-codex-old").expect("seeded");
                record.turns = 1;
                let facts = list.overrides.entry("s-codex-old".into()).or_default();
                facts.project = Some(project);
                facts.handoff_from = Some("s-claude-src".into());
                facts.handoff_from_provider = Some("claude-code".into());
                facts.handoff_title = Some("Project Layout Overview Explained".into());
                facts.provider_title = None;
                facts.first_prompt = None;
            });
            harness.merge_provider_rows(cx);
        });
    });
    let new_lands_a_draft = |vc: &mut gpui::VisualTestContext, round: &str| {
        vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().unwrap_or_else(|| panic!("{round}: a view is open"));
            let id = view.read(cx).rt(cx).session_id.clone();
            assert_ne!(id, "s-codex-old", "{round}: New session left the clicked session up");
            assert!(harness.is_live_draft(&id, cx), "{round}: the centre is an unsent draft");
            assert_eq!(view.read(cx).rt(cx).provider_kind(), provider, "{round}: on the default's lane");
            assert!(!harness.session_opening(), "{round}: nothing is still opening");
            id
        })
    };
    let open_other = |vc: &mut gpui::VisualTestContext, round: &str| {
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let harness = baaz.read(cx);
            let view = harness.active.clone().unwrap_or_else(|| panic!("{round}: a view is open"));
            assert_eq!(view.read(cx).rt(cx).session_id, "s-codex-old", "{round}: the click opened it");
            assert!(!harness.session_opening(), "{round}: the reopen landed");
        });
    };
    let first = new_lands_a_draft(vc, "first new");
    open_other(vc, "first open");
    let second = new_lands_a_draft(vc, "second new");
    open_other(vc, "second open (parked)");
    let third = new_lands_a_draft(vc, "third new");
    assert_eq!(first, second, "the project's unsent draft is reused, not a second one");
    assert_eq!(second, third, "and again");
    lane_restore(state);
}

#[gpui::test]
fn new_after_open_lands_a_draft_on_muse(cx: &mut gpui::TestAppContext) {
    new_after_open_lands_a_draft(cx, "new-open-new-muse", ProviderId::Muse);
}

#[gpui::test]
fn new_after_open_lands_a_draft_on_codex(cx: &mut gpui::TestAppContext) {
    new_after_open_lands_a_draft(cx, "new-open-new-codex", ProviderId::Codex);
}

/// What the person does while the start is on the wire waits for it:
/// the text at Enter is what goes (not the field's later edits), the
/// field clears at once and the view reads busy, and the latest model
/// and mode picks reach the wire after the start lands — the mode chip
/// reading the pick meanwhile. Restore the old flag and the edited
/// text is what is sent; drop the queue and neither pick is sent.
#[gpui::test]
fn what_is_done_while_the_start_is_on_the_wire_goes_when_it_lands(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("new-early-queue");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let log = state.2.join("requests.log");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            install_fake_muse(harness, &state.2, &state.2, &log, None);
            adopt_for_muse(harness, &state.2, cx);
        })
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let view = harness.active.clone().expect("the draft is up");
            view.update(cx, |view, cx| {
                assert!(view.rt(cx).is_starting());
                view.set_draft("captured at enter".into(), window, cx);
                view.send(window, cx);
                assert_eq!(view.draft_text(cx), "", "the field clears like any send");
                assert!(view.rt(cx).busy(), "the queued send reads as pending");
                view.set_draft("edited afterwards".into(), window, cx);
                view.runtime().clone().update(cx, |rt, cx| rt.set_model("muse-early", cx));
                view.runtime().clone().update(cx, |rt, cx| rt.set_mode(aui_protocol::PermissionMode::DenyUnmatched, cx));
                assert_eq!(
                    view.rt(cx).mode_label(),
                    aui_protocol::PermissionMode::DenyUnmatched.label(),
                    "the chip reads the waiting pick"
                );
            });
        })
    });
    vc.run_until_parked();
    let requests = std::fs::read_to_string(&log).unwrap_or_default();
    let turn = requests.lines().find(|line| line.contains("\"turn/start\"")).expect("the queued send went");
    assert!(turn.contains("captured at enter"), "the Enter's text is sent: {turn}");
    assert!(!requests.contains("edited afterwards"), "never the later edit: {requests}");
    assert!(requests.contains("session/setModel") && requests.contains("muse-early"), "{requests}");
    // A pick that lowers access goes profile first (this fake refuses
    // it, so the mode — which would read less than the session has —
    // never follows).
    assert!(
        requests.lines().any(|line| line.contains("\"session/resume\"") && line.contains(":read-only")),
        "{requests}"
    );
    lane_restore(state);
}

/// A start answered under another id than the minted one fails the
/// view visibly — never Starting forever, never a row under the
/// minted id — and a send that waited goes back in the composer.
#[gpui::test]
fn a_start_answered_under_another_id_fails_the_view(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("new-early-foreign");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let log = state.2.join("requests.log");
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            install_fake_muse(harness, &state.2, &state.2, &log, Some("s-foreign"));
            adopt_for_muse(harness, &state.2, cx)
        })
    });
    let minted = vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            let view = harness.active.clone().expect("the draft is up");
            view.update(cx, |view, cx| {
                view.set_draft("kept".into(), window, cx);
                view.send(window, cx);
            });
            view.read(cx).rt(cx).session_id.clone()
        })
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_none(), "no dead draft in the centre, nothing reads as pending");
        assert!(harness.find_view(&minted, cx).is_none(), "the dead draft is parked nowhere");
        let failure = harness.open_state.provider_open_error.clone().expect("the typed failure view");
        // The waiting send rides Retry back into the next draft.
        assert!(
            matches!(&failure.retry, ProviderOpenRetry::OpenNew { draft, .. } if draft == "kept"),
            "the waiting send is kept for Retry"
        );
        assert!(harness.open_state.starts_in_flight.is_empty(), "scripted verbs are released");
        assert!(!harness.open_state.drafts.contains_key(&project), "the next New session starts fresh");
        assert!(!harness.session_list.read(cx).sessions.iter().any(|entry| entry.id == minted), "no row for it");
    });
    let requests = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(!requests.contains("turn/start"), "nothing was sent to a session that never landed: {requests}");
    lane_restore(state);
}

/// V1: "New session" (sidebar row, ⌘N) while the Skills page is open
/// leaves the page and opens the session. Clicking a session already
/// left (via `resume_inner`); the new-session paths activate through
/// `activate` without ever closing it, so the page swallowed the new
/// lane. Drop the `activate` close and the page stays open.
#[gpui::test]
fn a_new_session_leaves_the_skills_page(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("v1-skills-new");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(ProviderId::Codex, cx);
            harness.skills_page.update(cx, |page, _| page.open = true);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.new_session(window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(harness.active.is_some(), "the new session opened");
        assert!(!harness.skills_open(cx), "New session leaves the Skills page");
    });
    lane_restore(state);
}

/// V1: Escape leaves the Skills page from any focus the window's own
/// Cancel reaches. The page's `SkillsClose` key only fires with focus
/// inside the page, so from the dock or the sidebar nothing left it.
/// Drop the `cancel` arm and the page stays open.
#[gpui::test]
fn escape_leaves_the_skills_page_from_outside_it(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("v1-skills-esc");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.auth = Auth::SignedIn(crate::auth::Identity {
                lane: provider::AccountLane::AccountLogin,
                name: "Test".into(),
                email: String::new(),
            });
            harness.skills_page.update(cx, |page, _| page.open = true);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.cancel(window, cx));
    });
    vc.update(|_, cx| {
        assert!(!baaz.read(cx).skills_open(cx), "Escape leaves the Skills page");
    });
    lane_restore(state);
}

/// V1: Escape closes the composer's provider menu. The chip picker
/// lives in the shared overlay stack, so the overlay close at the top
/// of `cancel` takes it — this pins that against a regression that
/// would strand the menu and let the next Enter pick a row.
#[gpui::test]
fn escape_closes_the_composer_provider_menu(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("v1-menu-esc");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
            harness.auth = Auth::SignedIn(crate::auth::Identity {
                lane: provider::AccountLane::AccountLogin,
                name: "Test".into(),
                email: String::new(),
            });
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the lane opened");
        view.update(cx, |view, cx| view.toggle_picker(crate::overlays::MenuKind::Provider, cx));
        assert!(
            baaz.read(cx).overlays.read(cx).menu().is_some(),
            "the provider menu opened"
        );
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.cancel(window, cx));
    });
    vc.update(|_, cx| {
        assert!(
            baaz.read(cx).overlays.read(cx).menu().is_none(),
            "Escape closes the composer's provider menu"
        );
    });
    lane_restore(state);
}

/// V1: Escape closes the composer's `+` menu. It once was plain view
/// state the overlay stack never saw, so `cancel` passed it by and the menu
/// stranded; it is a menu on the stack now, so Escape's pop takes it.
#[gpui::test]
fn escape_closes_the_composer_plus_menu(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("v1-plus-esc");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&scripted_factory());
            mark_muse_up(harness);
            harness.auth = Auth::SignedIn(crate::auth::Identity {
                lane: provider::AccountLane::AccountLogin,
                name: "Test".into(),
                email: String::new(),
            });
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace(cx);
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the lane opened");
        view.update(cx, |view, cx| view.step_plus(cx)).expect("the `+` step runs");
        assert!(view.read(cx).plus_open(cx), "the `+` menu opened");
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.cancel(window, cx));
    });
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let open = harness.active.clone().expect("the lane opened").read(cx).plus_open(cx);
        assert!(!open, "Escape closes the composer's `+` menu");
    });
    lane_restore(state);
}

#[gpui::test]
fn a_reopened_muse_session_is_muse_whatever_the_new_session_default(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b2-muse-reopen");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    for default in [ProviderId::Codex, ProviderId::ClaudeCode] {
        vc.update(|window, cx| {
            baaz.update(cx, |harness, cx| {
                harness.new_provider = default.as_str().to_owned();
                harness.open("muse-row".into(), true, window, cx);
            });
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = baaz.read(cx).active.clone().expect("the row opened");
            assert_eq!(
                view.read(cx).rt(cx).provider_kind(),
                ProviderId::Muse,
                "a Muse row reopened with a {default:?} default still reads Muse"
            );
        });
    }
    lane_restore(state);
}

/// Adopt the harness workspace as a project holding `defaults`
/// (per-provider model ids), park a muse view with `catalog` loaded as
/// the active view, and point `new_provider` at `provider`: everything
/// `new_session_params` reads in the B2fix tests below.
fn b2fix_project(
    vc: &mut gpui::VisualTestContext,
    baaz: &gpui::Entity<Harness>,
    provider: &str,
    defaults: &[(&str, &str)],
    catalog: &[&str],
) -> String {
    let workspace = vc.update(|_, cx| baaz.read(cx).workspace(cx));
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let id = harness.session_list.update(cx, |list, _| list.projects.add(std::path::Path::new(&workspace)).id.clone());
            harness.session_list.update(cx, |list, _| {
                let adopted = list.projects.projects.iter_mut().find(|p| p.id == id).expect("just adopted");
                for &(lane, model) in defaults {
                    adopted.defaults.note_model(lane, model);
                }
            });
            harness.new_provider = provider.to_owned();
            id
        })
    });
    vc.update(|window, cx| {
        let view = cx.new(|cx| {
            SessionView::new("s-b2fix-catalog".to_owned(), SessionHost {
                    provider_id: ProviderId::Muse.as_str().to_owned(),
                    workspace: workspace.clone(),
                    overlays: cx.new(|_| crate::overlays::Overlays::default()),
                    capture: crate::runtime::CaptureToken::default(),
                    terminal_host: None,
                },
                window,
                cx,
            )
        });
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, _| rt.seed_test_catalog(catalog)));
        baaz.update(cx, |harness, _| {
            harness.active = Some(view);
        });
    });
    project
}

/// A provider draft asks for the project's remembered model for that
/// provider (the last pick, which `ModelSelected` stores): the chip names
/// it from the first frame — not the CLI's "Default" row — and the open's
/// landing sends it to the child.
#[gpui::test]
fn a_provider_draft_requests_the_projects_remembered_model(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("remembered-model");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    b2fix_project(vc, &baaz, "claude-code", &[("claude-code", "opus[1m]")], &[]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            harness.active = None;
        });
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    y2b2_drain_until_opens(vc, &shared, 1);
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the draft is drawn");
        assert!(view.read(cx).rt(cx).is_opening_lane());
        assert_eq!(view.read(cx).rt(cx).model_id(), "opus[1m]", "the chip names the requested model");
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the draft landed");
        assert!(!view.read(cx).rt(cx).is_opening_lane());
        assert_eq!(view.read(cx).rt(cx).model_id(), "opus[1m]");
    });
    lane_restore(state);
}

/// Owner 2026-10-21: a new session in harness set to Full access and
/// Medium, then a new session in office-samples — it came up in the
/// default mode and effort. Picks are now one memory ("Last pick,
/// everywhere"), and a Claude Code draft reads the effort (it never
/// did). The picks travel the real event path, made while the first
/// draft could still be opening.
#[gpui::test]
fn a_pick_in_one_project_starts_the_next_session_in_another(cx: &mut gpui::TestAppContext) {
    use crate::session::SessionEvent;
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("last-picks");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    let first = b2fix_project(vc, &baaz, "claude-code", &[], &[]);
    let other_root = std::env::temp_dir().join(format!("baaz-last-picks-{}", std::process::id()));
    std::fs::create_dir_all(&other_root).expect("second root");
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            harness.active = None;
            harness.current_project = Some(first.clone());
        });
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    y2b2_drain_until_opens(vc, &shared, 1);
    // Picked on the draft before its open lands.
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the first draft is drawn");
        baaz.update(cx, |harness, cx| {
            harness.on_session_event(view.read(cx).runtime().clone(), &SessionEvent::ModeSelected { mode: aui_protocol::PermissionMode::AllowAll }, cx);
            harness.on_session_event(view.read(cx).runtime().clone(), &SessionEvent::EffortSelected { effort: Some("medium".into()) }, cx);
        });
    });
    vc.run_until_parked();
    let second = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let id = harness.session_list.update(cx, |list, _| list.projects.add(&other_root).id.clone());
            assert_ne!(id, first, "a second project");
            harness.current_project = Some(id.clone());
            harness.active = None;
            id
        })
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    y2b2_drain_until_opens(vc, &shared, 2);
    vc.run_until_parked();
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the second draft is drawn");
        let (mode, effort) = view.read(cx).rt(cx).draft_settings();
        assert_eq!(mode, aui_protocol::PermissionMode::AllowAll, "Full access carries into {second}");
        assert_eq!(crate::overlays::effort_label(effort), "Medium", "Medium carries into {second}");
    });
    let _ = std::fs::remove_dir_all(&other_root);
    lane_restore(state);
}

/// A Claude Code draft whose project remembers Haiku and defaults to
/// Auto opens in Ask — chip, banner and the open's own mode — before
/// any catalog has answered: Haiku has no Auto, and the CLI was being
/// launched (and relaunched) `--permission-mode auto` on it.
#[gpui::test]
fn a_draft_on_a_remembered_haiku_opens_in_ask(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("remembered-haiku");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, shared) = y2b2_factory();
    let project = b2fix_project(vc, &baaz, "claude-code", &[("claude-code", "haiku")], &[]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.session_list.update(cx, |list, _| {
                let adopted = list.projects.projects.iter_mut().find(|p| p.id == project).expect("adopted");
                adopted.defaults.approval_mode = Some(aui_protocol::PermissionMode::OnRequest);
            });
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            harness.active = None;
        });
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.new_session(window, cx)));
    y2b2_drain_until_opens(vc, &shared, 1);
    vc.update(|_, cx| {
        let view = baaz.read(cx).active.clone().expect("the draft is drawn");
        let view = view.read(cx);
        assert_eq!(view.rt(cx).model_id(), "haiku");
        assert_eq!(view.rt(cx).mode_label(), "Ask", "never Auto on Haiku");
        assert_eq!(
            view.rt(cx).banner_text(),
            Some("Auto is not available for Haiku; Claude Code will ask before acting.")
        );
    });
    assert_eq!(
        shared.inner.lock().expect("y2b2").modes,
        [Some(aui_protocol::PermissionMode::PromptUnmatched)],
        "the child is asked to start in Ask"
    );
    lane_restore(state);
}

/// B2fix: with a muse view's catalog loaded, a Codex default is never
/// checked against the muse catalog — the guarded helper filters muse
/// starts only. Without the provider guard the valid Codex default
/// drops to `None` whenever any muse view has listed models.
#[gpui::test]
fn a_codex_default_survives_a_loaded_muse_catalog(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b2fix-codex-kept");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let project = b2fix_project(vc, &baaz, "codex", &[("codex", "gpt-6-astra")], &["muse-spark-1.3"]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let params =
                harness.start_model(harness.new_provider_id().expect("a registered provider"), Some(project.as_str()), cx);
            assert_eq!(
                params.as_deref(),
                Some("gpt-6-astra"),
                "a codex default never meets the muse catalog"
            );
        });
    });
    lane_restore(state);
}

/// B2fix: the same guard for the Claude Code lane.
#[gpui::test]
fn a_claude_code_default_survives_a_loaded_muse_catalog(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b2fix-claude-kept");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let project =
        b2fix_project(vc, &baaz, "claude-code", &[("claude-code", "claude-opus-4")], &["muse-spark-1.3"]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let params =
                harness.start_model(harness.new_provider_id().expect("a registered provider"), Some(project.as_str()), cx);
            assert_eq!(
                params.as_deref(),
                Some("claude-opus-4"),
                "a claude-code default never meets the muse catalog"
            );
        });
    });
    lane_restore(state);
}

/// B2fix: a handoff destination to muse with a foreign project default
/// starts on muse's own default — no model id is sent. The destination
/// opens through `open_handoff_destination`, which points the window at
/// muse and starts through `new_session`, so the params below are the
/// ones it sends; the pending handoff mirrors that state.
#[gpui::test]
fn a_handoff_destination_to_muse_drops_a_foreign_default(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b2fix-handoff-muse");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let project = b2fix_project(vc, &baaz, "muse", &[("muse", "gpt-6-astra")], &["muse-spark-1.3"]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.handoff_state.pending = Some(crate::handoff::PendingHandoff {
                source_session: "s-source".to_owned(),
                epoch: 0,
                prefix: Vec::new(),
            });
            let params =
                harness.start_model(harness.new_provider_id().expect("a registered provider"), Some(project.as_str()), cx);
            assert_eq!(
                params, None,
                "a foreign default never rides a muse handoff destination"
            );
        });
    });
    lane_restore(state);
}

/// B2fix: a muse start whose stored default the catalog lists keeps it.
#[gpui::test]
fn a_muse_start_keeps_a_listed_model(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("b2fix-muse-kept");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let project =
        b2fix_project(vc, &baaz, "muse", &[("muse", "muse-spark-1.3")], &["muse-spark-1.3"]);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let params =
                harness.start_model(harness.new_provider_id().expect("a registered provider"), Some(project.as_str()), cx);
            assert_eq!(
                params.as_deref(),
                Some("muse-spark-1.3"),
                "a listed muse default rides the start"
            );
        });
    });
    lane_restore(state);
}

#[gpui::test]
fn a_second_new_while_a_switch_is_in_flight_starts_nothing(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("w6-new-twice");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, opens) = counting_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.providers = harness.providers.with_opens_of(&factory);
            mark_muse_up(harness);
            harness.new_provider = ProviderId::Codex.as_str().to_owned();
        });
    });
    // Two `new`s back to back: the second lands while the first open
    // is still in flight. Remove the guard and the factory spawns
    // twice for one action.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.new_session(window, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(opens_of(&opens), 1, "one `new` pair spawns one child");
        let view = harness.active.clone().expect("the session opened");
        assert!(view.read(cx).rt(cx).provider_kind() != ProviderId::Muse, "it rides the provider lane");
        assert!(!harness.open_state.switch_pending, "the switch landed");
    });
    lane_restore(state);
}
