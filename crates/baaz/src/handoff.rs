//! Handoff's progress steps as the library draws them: the card's four
//! default steps with a run's states marked. The state machine and the pack
//! builder are [`baaz_core::handoff`], re-exported here so
//! `crate::handoff::…` names both. Only the tests read these steps: the
//! transcript re-derives them from the block's own fields.

#[cfg(feature = "probe")]

#[cfg(test)]
use aui::transcript::{HandoffStep, HandoffStepState, default_handoff_steps};
#[cfg(feature = "probe")]
#[cfg(test)]
use aui_protocol::HandoffState;

pub use baaz_core::handoff::*;

#[cfg(feature = "probe")]

#[cfg(test)]
/// The card's progress steps: the library's four defaults with this
/// run's states marked (see [`handoff_steps`], the mapping the
/// transcript shares from the block alone). The current step carries
/// the run's age (`"n s"`); a refused run stays all pending —
/// nothing started, so the state line and the pill carry the reason.
pub fn run_steps(run: &HandoffRun) -> Vec<HandoffStep> {
    handoff_steps(
        &run.state,
        wire_provider(run.to),
        run.pack.as_ref().map(|pack| pack.summary_kind),
        run.summarising,
        run.destination_session.is_some(),
        run.pack.is_some(),
        Some(run.started_at.elapsed().as_secs()),
    )
}

/// Mark the library's four default steps from a run's state — the one
/// mapping both [`run_steps`] and the transcript's card build
/// share (the protocol card carries no step list, so the transcript
/// re-derives the same steps from the block's own fields).
///
/// `summary_kind` is the pack's kind once checkpointed (`None` before);
/// `waiting_summary` is the summary side session still being awaited;
/// `elapsed_secs` details the one current step (`"n s"`), when the
/// caller knows a basis for it. The failed step carries the reason;
/// refused stays all pending (nothing started).
#[cfg(feature = "probe")]
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
pub fn handoff_steps(
    state: &HandoffState,
    to: aui_protocol::Provider,
    summary_kind: Option<SummaryKind>,
    waiting_summary: bool,
    has_destination: bool,
    has_pack: bool,
    elapsed_secs: Option<u64>,
) -> Vec<HandoffStep> {
    let mut steps = default_handoff_steps(to);
    let current = elapsed_secs.map(|secs| format!("{secs} s"));
    let summary_settled = |steps: &mut Vec<HandoffStep>| {
        steps[1].state = match summary_kind {
            Some(SummaryKind::Model) => HandoffStepState::Done,
            _ => HandoffStepState::Skipped,
        };
    };
    match state {
        HandoffState::Requested | HandoffState::Quiescing => {
            steps[0].state = HandoffStepState::Current;
            steps[0].detail = current;
        }
        HandoffState::Checkpointed => {
            steps[0].state = HandoffStepState::Done;
            match summary_kind {
                Some(SummaryKind::Model) => steps[1].state = HandoffStepState::Done,
                _ if waiting_summary => {
                    steps[1].state = HandoffStepState::Current;
                    steps[1].detail = current;
                }
                Some(SummaryKind::Extractive) => steps[1].state = HandoffStepState::Skipped,
                None => steps[1].state = HandoffStepState::Pending,
            }
        }
        HandoffState::Prepared | HandoffState::Acknowledged => {
            // Landing stands the summary wait down: the submitted pack is
            // fixed, so extractive reads skipped from here on.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = HandoffStepState::Done;
            steps[3].state = HandoffStepState::Current;
            steps[3].detail = current;
        }
        HandoffState::Activated => {
            // Settled cards hide the list again (the library's call), but
            // the mapping stays total either way.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = HandoffStepState::Done;
            steps[3].state = HandoffStepState::Done;
        }
        HandoffState::Cancelled => {
            // A cancelled move never reads as complete: what ran stays
            // done, what never happened reads skipped.
            steps[0].state = HandoffStepState::Done;
            summary_settled(&mut steps);
            steps[2].state = if has_destination { HandoffStepState::Done } else { HandoffStepState::Skipped };
            steps[3].state = HandoffStepState::Skipped;
        }
        HandoffState::Failed { reason } => {
            summary_settled(&mut steps);
            if has_destination {
                // The pack turn failed after the open: <To> never confirmed.
                steps[0].state = HandoffStepState::Done;
                steps[2].state = HandoffStepState::Done;
                steps[3].state = HandoffStepState::Failed;
                steps[3].detail = Some(reason.clone());
            } else if has_pack {
                // The destination never opened.
                steps[0].state = HandoffStepState::Done;
                steps[2].state = HandoffStepState::Failed;
                steps[2].detail = Some(reason.clone());
            } else {
                // Nothing to hand off, or the request never started it.
                steps[0].state = HandoffStepState::Failed;
                steps[0].detail = Some(reason.clone());
            }
        }
        HandoffState::Refused { .. } => {}
    }
    steps
}

#[cfg(feature = "probe")]

#[cfg(test)]
mod tests {
    use super::*;
    use aui_protocol::{Block, HandoffState, Session};
    use crate::providers::ProviderId;

    fn run() -> HandoffRun {
        HandoffRun::request(
            "src".to_owned(),
            7,
            ProviderId::Muse,
            ProviderId::Codex,
            "muse".to_owned(),
            "gpt-5".to_owned(),
            false,
            false,
            false,
        )
        .expect("fresh request starts")
    }

    fn session_with_turns() -> Session {
        Session::new("s", aui_protocol::Provider::Muse, "m", "/tmp/proj")
    }

    #[test]
    fn steps_follow_the_run_from_request_to_activation() {
        let mut run = run();
        // The labels are the library's four defaults, naming the destination.
        let expected = default_handoff_steps(wire_provider(ProviderId::Codex));
        let steps = run_steps(&run);
        let labels: Vec<&str> = steps.iter().map(|step| step.label.as_str()).collect();
        assert_eq!(labels[0], "Pack the context");
        assert_eq!(labels[1], "Write a summary");
        assert_eq!(labels[2], expected[2].label.as_str());
        assert_eq!(labels[3], expected[3].label.as_str());
        // Requested: packing now, with the run's age on it.
        let states = |run: &HandoffRun| run_steps(run).iter().map(|step| step.state).collect::<Vec<_>>();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Current,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        assert_eq!(run_steps(&run)[0].detail.as_deref(), Some("0 s"));
        run.note_quiescing();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Current,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        // Checkpointed with no summary wait: the model summary is skipped.
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Pending,
                HandoffStepState::Pending,
            ]
        );
        // Prepared: the destination is open, waiting on its confirm.
        run.note_prepared("dst".to_owned());
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Current,
            ]
        );
        assert_eq!(run_steps(&run)[3].detail.as_deref(), Some("0 s"));
        // Acknowledged, then activated: the confirm lands, then all done.
        assert!(run.acknowledge(7));
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Current,
            ]
        );
        run.activate();
        assert_eq!(
            states(&run),
            vec![
                HandoffStepState::Done,
                HandoffStepState::Skipped,
                HandoffStepState::Done,
                HandoffStepState::Done,
            ]
        );
        assert!(run_steps(&run).iter().all(|step| step.detail.is_none()), "settled steps carry no counter");
    }

    #[test]
    fn steps_fail_at_start_names_the_pack_step() {
        let mut run = run();
        run.note_quiescing();
        run.fail("nothing to carry — the session has no turns".to_owned());
        let steps = run_steps(&run);
        assert!(matches!(steps[0].state, HandoffStepState::Failed));
        assert_eq!(
            steps[0].detail.as_deref(),
            Some("nothing to carry — the session has no turns"),
            "the failed step carries the reason"
        );
        assert!(
            !steps.iter().any(|step| matches!(step.state, HandoffStepState::Current)),
            "nothing is left Current on a dead card"
        );
    }

    #[test]
    fn steps_fail_at_the_pack_turn_names_the_confirm_step() {
        let mut run = run();
        run.note_quiescing();
        run.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        run.note_prepared("dst".to_owned());
        assert!(run.acknowledge(7));
        run.activate();
        run.fail_pack_turn("model `gpt-6-astra` does not exist or you lack access".to_owned());
        assert!(matches!(run.state, HandoffState::Failed { .. }));
        let steps = run_steps(&run);
        assert_eq!(
            (steps[0].state, steps[1].state, steps[2].state),
            (HandoffStepState::Done, HandoffStepState::Skipped, HandoffStepState::Done)
        );
        assert!(matches!(steps[3].state, HandoffStepState::Failed));
        assert_eq!(
            steps[3].detail.as_deref(),
            Some("model `gpt-6-astra` does not exist or you lack access"),
            "the failed step carries the reason"
        );
    }

    #[test]
    fn steps_show_the_summary_waited_or_skipped() {
        // Skipped: checkpointed with no wait — no model summary is used.
        let mut skipped = run();
        skipped.note_quiescing();
        skipped.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        assert!(matches!(run_steps(&skipped)[1].state, HandoffStepState::Skipped));
        assert_eq!(run_steps(&skipped)[1].detail, None);
        // Waited: the summary step is Current with its elapsed counter…
        let mut waited = run();
        waited.note_quiescing();
        waited.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        waited.note_summary_pending();
        assert!(matches!(run_steps(&waited)[1].state, HandoffStepState::Current));
        assert_eq!(run_steps(&waited)[1].detail.as_deref(), Some("0 s"));
        // …while the carried row still names the kind, never the wait.
        if let Block::Handoff { carried, .. } = waited.card() {
            assert_eq!(carried[0].detail.as_deref(), Some("extractive summary"));
        } else {
            panic!("not a handoff card");
        }
        // Harvested before landing: the step reads done.
        waited.apply_model_summary("Did X. Decided Y.".to_owned());
        assert!(matches!(run_steps(&waited)[1].state, HandoffStepState::Done));
        // Landed without a summary: skipped from here on.
        let mut landed = run();
        landed.note_quiescing();
        landed.note_checkpointed(build_pack(&session_with_turns(), "/tmp/proj"));
        landed.note_prepared("dst".to_owned());
        assert!(matches!(run_steps(&landed)[1].state, HandoffStepState::Skipped));
    }

#[cfg(feature = "probe")]

#[cfg(test)]
mod cancelled_steps_tests {
    use super::*;

    #[test]
    fn a_cancelled_move_never_reads_complete() {
        let steps = handoff_steps(&HandoffState::Cancelled, aui_protocol::Provider::Codex, Some(SummaryKind::Extractive), false, false, true, None);
        assert_eq!(steps[3].state, HandoffStepState::Skipped);
        assert_eq!(steps[2].state, HandoffStepState::Skipped);
    }
}
}
