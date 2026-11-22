//! Tests for the scripted run: boot decisions and step readiness.

use super::*;

/// The whole matrix behind `ensure_boot_session`, from a scripted boot
/// with no session named and no list yet.
fn boot_state() -> BootState<'static> {
    BootState {
        active: false,
        switch_pending: false,
        attempted: false,
        connected: true,
        session_arg: None,
        send: false,
        steps_pending: true,
        steps_begin_with_new: false,
        sessions_loaded: false,
    }
}

#[test]
fn the_boot_session_opens_once_on_a_live_wire() {
    // Steps with no session named: open without the list.
    assert_eq!(boot_decision(boot_state()), BootDecision::Open);
    // A scripted turn wants its session the same way.
    assert_eq!(boot_decision(BootState { send: true, steps_pending: false, ..boot_state() }), BootDecision::Open);
    // An explicit id resumes without the list too.
    assert_eq!(
        boot_decision(BootState { session_arg: Some("s-1"), ..boot_state() }),
        BootDecision::Open
    );
    // The second frame, the second caller, the late list reply: idle.
    assert_eq!(
        boot_decision(BootState { active: true, attempted: true, sessions_loaded: true, ..boot_state() }),
        BootDecision::Idle
    );
    assert_eq!(
        boot_decision(BootState { switch_pending: true, attempted: true, ..boot_state() }),
        BootDecision::Idle
    );
    assert_eq!(
        boot_decision(BootState { attempted: true, sessions_loaded: true, ..boot_state() }),
        BootDecision::Idle
    );
}

#[test]
fn only_latest_waits_for_the_list() {
    assert_eq!(
        boot_decision(BootState { session_arg: Some("latest"), ..boot_state() }),
        BootDecision::WaitForList
    );
    assert_eq!(
        boot_decision(BootState {
            session_arg: Some("latest"),
            sessions_loaded: true,
            ..boot_state()
        }),
        BootDecision::Open
    );
}

#[test]
fn a_script_headed_by_new_boots_no_session() {
    // `new;setprovider:codex` starts its own session: booting one
    // eagerly ahead of it opened two (two lane-open lines, two rows).
    // Remove the arm and boot opens again beside the head's `new`.
    assert_eq!(
        boot_decision(BootState { steps_begin_with_new: true, ..boot_state() }),
        BootDecision::Idle
    );
    // Any other script still boots: the session has to come from
    // somewhere.
    assert_eq!(boot_decision(boot_state()), BootDecision::Open);
    // An explicit `--session` resumes even when the steps begin with
    // `new`: the head's `new` then reuses or replaces, never doubles
    // the boot.
    assert_eq!(
        boot_decision(BootState {
            session_arg: Some("s-1"),
            steps_begin_with_new: true,
            ..boot_state()
        }),
        BootDecision::Open
    );
}

#[test]
fn an_unscripted_boot_opens_nothing() {
    assert_eq!(
        boot_decision(BootState { steps_pending: false, sessions_loaded: true, ..boot_state() }),
        BootDecision::Idle
    );
    // And nothing opens while the wire is down, however scripted.
    assert_eq!(
        boot_decision(BootState { connected: false, ..boot_state() }),
        BootDecision::Idle
    );
    assert_eq!(
        boot_decision(BootState {
            connected: false,
            session_arg: Some("s-1"),
            sessions_loaded: true,
            ..boot_state()
        }),
        BootDecision::Idle
    );
}

#[test]
fn steps_run_live_only_with_a_wire_and_a_session() {
    // Live: both.
    assert!(steps_ready_for(true, false, false, true, true, false, false));
    assert!(!steps_ready_for(true, false, false, true, false, false, false));
    assert!(!steps_ready_for(true, false, false, false, true, false, false));
    assert!(!steps_ready_for(true, false, false, false, false, false, false));
    // Replay and offline have no wire: the open session alone decides,
    // connected or not.
    assert!(steps_ready_for(true, true, false, false, true, false, false));
    assert!(steps_ready_for(true, false, true, false, true, false, false));
    assert!(!steps_ready_for(true, true, false, false, false, false, false));
    assert!(!steps_ready_for(true, false, true, false, false, false, false));
    // A window-only script needs no session at all. This is the arm that
    // was missing: `--login signed-in --no-connect` opens no session, so
    // every window verb was silently skipped and the capture still
    // exited 0. Five right-pane entries were written against that and
    // produced five identical screenshots of an empty shell.
    assert!(steps_ready_for(true, false, true, false, false, true, false));
    assert!(steps_ready_for(true, false, false, false, false, true, false));
    assert!(steps_ready_for(true, true, false, false, false, true, false));
    // An in-flight open — `session/start` or a provider lane — holds
    // session verbs until it lands, exactly like a missing session
    // does: `new;setprovider:codex;send:` failed with `no open
    // session` because `send` ran before the lane finished. Window
    // verbs never touch the session, so they run on regardless.
    assert!(!steps_ready_for(true, false, false, true, true, false, true));
    assert!(!steps_ready_for(true, false, true, false, true, false, true));
    assert!(steps_ready_for(true, false, true, false, false, true, true));
    assert!(steps_ready_for(true, false, false, true, false, true, true));
    // Still nothing to do with no steps pending, whatever else holds.
    assert!(!steps_ready_for(false, false, true, false, false, true, false));
    // No script left, nowhere: never ready, in every mode.
    assert!(!steps_ready_for(false, false, false, true, true, false, false));
    assert!(!steps_ready_for(false, true, false, false, true, false, false));
    assert!(!steps_ready_for(false, false, true, false, true, false, false));
}

#[test]
fn the_second_drain_finds_nothing_to_do() {
    let mut holder = vec!["new".to_owned(), "wait:3000".to_owned()];
    assert_eq!(drain_steps(&mut holder), vec!["new".to_owned(), "wait:3000".to_owned()]);
    assert!(holder.is_empty());
    assert!(drain_steps(&mut holder).is_empty());
}

/// An online script waits for the open session's own lane, not for the shared
/// Muse child a Claude Code or Codex session never uses.
#[test]
fn a_provider_off_the_shared_child_does_not_wait_for_it() {
    use crate::app::sessions::steps::session_lane_up;
    assert!(session_lane_up(false, false), "a Claude Code / Codex lane needs no Muse child");
    assert!(!session_lane_up(true, false), "a Muse session waits for its child");
    assert!(session_lane_up(true, true));
}
