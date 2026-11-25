//! Goals, the app's half (docs/85 Stage 15): Baaz judges every goal turn
//! itself, on every provider — the check found for the project (14.5b)
//! when there is one, else the provider's own judgement (a native goal) or
//! Baaz's evaluator on the cheapest model (Muse, ACP). It also keeps the
//! limits (15.4), the usage-limit wait, the stall rules, the Mac awake
//! (15.5), the relaunch's one resend (15.6) and the progress note (15.7).
//!
//! The session holds the record (`crate::session::goals`); this side only
//! reads it and answers with [`SessionRuntime::settle_goal`].
//!
//! Keyed by session id: the `goal` table, and the runtimes the registry
//! finds by id (no holder of its own).

use baaz_core::goals::{Goal, GoalState};
use gpui::{AppContext as _, Context, Entity};

use crate::app::Harness;
use crate::session::SessionRuntime;
use crate::wire::WireCall as _;

/// How long a goal's check may run.
const CHECK_DEADLINE: std::time::Duration = std::time::Duration::from_secs(15 * 60);
/// Lines of a failing check sent back to the agent.
const FAILING_TAIL_LINES: usize = 20;
/// How long a provider that holds the goal itself may take to start its own
/// next turn before Baaz sends the failing check (Gate 15 live: Codex
/// carried on by itself and Baaz's message queued behind it, unanswered).
pub(crate) const NATIVE_CONTINUE_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

fn now() -> i64 {
    baaz_core::clock::now_ms() as i64
}

/// When this run began: a goal last changed before it was left by an
/// earlier run (15.6); one changed since is this run's own.
static BOOT_MS: std::sync::OnceLock<i64> = std::sync::OnceLock::new();

/// Stamp this run's start (the window's first build), before any goal can
/// change.
pub(crate) fn note_boot() {
    BOOT_MS.get_or_init(now);
}

fn boot_ms() -> i64 {
    *BOOT_MS.get_or_init(now)
}

fn goal_last_turn(runtime: &Entity<SessionRuntime>, cx: &gpui::App) -> Option<String> {
    runtime.read(cx).goal.as_ref().and_then(|g| g.last_turn.clone())
}

/// The last `n` lines of `text`.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// `cargo test: failed (exit 101)`, `cargo test: passed`.
fn check_line(command: &str, outcome: &baaz_core::checks::Outcome) -> String {
    if outcome.passed {
        format!("{command}: passed")
    } else {
        match outcome.code {
            Some(code) => format!("{command}: failed (exit {code})"),
            None => format!("{command}: did not finish"),
        }
    }
}

impl Harness {
    /// A goal turn ended (`SessionEvent::GoalTurnEnded`): over a ceiling it
    /// pauses and asks; else the check (or the evaluator) decides between
    /// met and one more turn.
    pub(crate) fn goal_turn_ended(&mut self, runtime: Entity<SessionRuntime>, cx: &mut Context<Self>) {
        let (goal, reply, workspace) = {
            let rt = runtime.read(cx);
            let Some(goal) = rt.goal.clone().filter(|g| g.state == GoalState::Running) else { return };
            (goal, rt.last_assistant_text().unwrap_or_default(), self.session_workspace(&rt.session_id, cx))
        };
        if let Some(over) = goal.over_limit(now()) {
            let reason = format!("Reached {over}. Resume to keep going.");
            runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::BudgetReached, None, Some(reason), None, cx));
            self.note_goal_progress(&runtime, &reply, &workspace, cx);
            return;
        }
        self.note_goal_progress(&runtime, &reply, &workspace, cx);
        match goal.check.clone() {
            Some(command) => self.run_goal_check(runtime, goal, command, workspace, cx),
            // A native goal ends its turn when the provider judges it met.
            None if goal.native => runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Met, Some("met, by the agent's own judgement".into()), None, None, cx)),
            None => self.run_goal_evaluator(runtime, goal, reply, cx),
        }
    }

    fn run_goal_check(&mut self, runtime: Entity<SessionRuntime>, goal: Goal, command: String, workspace: String, cx: &mut Context<Self>) {
        let run = command.clone();
        let work = move || baaz_core::checks::run(std::path::Path::new(&workspace), &run, CHECK_DEADLINE);
        self.wire_call(cx, work, move |_, outcome, cx| {
            // The person may have paused or stopped it while the check ran.
            if runtime.read(cx).goal.as_ref().is_none_or(|g| g.state != GoalState::Running) {
                return;
            }
            let line = check_line(&command, &outcome);
            if outcome.passed {
                runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Met, Some(line), None, None, cx));
            } else {
                let tail = last_lines(&outcome.tail, FAILING_TAIL_LINES);
                let next = goal.continue_text(Some((&command, &tail)));
                // The same failure three goal turns running: the agent is
                // not moving it, so the person decides (no more messages).
                let stuck = runtime.update(cx, |rt, _| rt.goal.as_mut().is_some_and(|g| g.note_failure(&tail)));
                if stuck {
                    // Plain words: the pill's caption draws no Markdown.
                    let reason = format!("{command} failed the same way {} turns running. It needs you: answer the agent, then Resume.", baaz_core::goals::SAME_FAILURE_LIMIT);
                    runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Blocked, Some(line), Some(reason), None, cx));
                    return;
                }
                if !runtime.read(cx).provider_kind().has(provider::ProviderTrait::GoalCommand) {
                    runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Running, Some(line), None, Some(next), cx));
                    return;
                }
                // The provider works on by itself while its goal is active:
                // the failing check is sent only if it stopped.
                runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Running, Some(line), None, None, cx));
                let judged = goal_last_turn(&runtime, cx);
                cx.spawn(async move |_, cx| {
                    cx.background_executor().timer(NATIVE_CONTINUE_GRACE).await;
                    runtime.update(cx, |rt, cx| {
                        let unmoved = rt.goal.as_ref().is_some_and(|g| g.state == GoalState::Running && g.last_turn == judged);
                        if unmoved && !rt.turn_running() {
                            rt.send_or_queue(next, cx);
                        }
                    });
                })
                .detach();
            }
        });
    }

    /// Muse and ACP with no check command: the cheapest model reads the
    /// turn's last words and says MET or CONTINUE. With no utility model to
    /// ask, the goal keeps going (the ceilings bound it).
    fn run_goal_evaluator(&mut self, runtime: Entity<SessionRuntime>, goal: Goal, reply: String, cx: &mut Context<Self>) {
        let session_id = goal.session.clone();
        let Some(provider) = self.session_lane_provider(&session_id, cx).filter(|p| self.utility_available(*p)) else {
            let next = goal.continue_text(None);
            runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Running, None, None, Some(next), cx));
            return;
        };
        let prompt = baaz_core::goals::evaluator_prompt(&goal.condition, &reply);
        let model = self.utility_model(&session_id, cx);
        let muse = self.muse_side(provider, "baaz goal check", cx);
        let work = move || crate::utility::complete(provider, model.as_deref(), &prompt, muse);
        self.wire_call(cx, work, move |_, answer, cx| {
            if runtime.read(cx).goal.as_ref().is_none_or(|g| g.state != GoalState::Running) {
                return;
            }
            match answer {
                Ok(answer) if baaz_core::goals::evaluator_says_met(&answer) => {
                    runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Met, Some("met, by Baaz's check of the reply".into()), None, None, cx));
                }
                other => {
                    if let Err(why) = other {
                        crate::baaz_log!("goal: the evaluator did not answer ({why}); the goal keeps going");
                    }
                    let next = goal.continue_text(None);
                    runtime.update(cx, |rt, cx| rt.settle_goal(GoalState::Running, Some("not met yet".into()), None, Some(next), cx));
                }
            }
        });
    }

    /// 15.7: every ten goal turns, and when it pauses, one project note
    /// "Goal progress · <goal>", replaced each time (its file is the
    /// title's slug); the count starts over.
    fn note_goal_progress(&mut self, runtime: &Entity<SessionRuntime>, reply: &str, workspace: &str, cx: &mut Context<Self>) {
        let Some(goal) = runtime.read(cx).goal.clone().filter(Goal::note_due) else { return };
        if workspace.is_empty() {
            return;
        }
        let done = crate::byline::excerpt_line(reply).unwrap_or_else(|| "see the session".to_owned());
        let left = if goal.state == GoalState::Running { "the goal is not met yet".to_owned() } else { goal.reason.clone().unwrap_or_else(|| goal.state.word().to_owned()) };
        let note = baaz_core::memory::Note {
            title: baaz_core::goals::progress_title(&goal.condition),
            description: format!("Where the goal \"{}\" stands", goal.condition),
            kind: "project".to_owned(),
            body: baaz_core::goals::progress_body(&goal, &done, &left),
        };
        let dir = baaz_core::memory::project_dir(std::path::Path::new(workspace));
        if let Err(why) = baaz_core::memory::save(&dir, &note) {
            crate::baaz_log!("goal: the progress note was not saved ({why})");
            return;
        }
        runtime.update(cx, |rt, cx| {
            if let Some(g) = rt.goal.as_mut() {
                g.turns_since_note = 0;
            }
            rt.save_goal(cx);
        });
    }

    /// A goal session's turn failed: a usage limit makes it wait for the
    /// reset and resume by itself (15.4).
    pub(crate) fn goal_turn_failed(&mut self, runtime: Entity<SessionRuntime>, error: Option<&str>, cx: &mut Context<Self>) {
        let rt = runtime.read(cx);
        if !rt.goal.as_ref().is_some_and(|g| g.state == GoalState::Running) || !error.is_some_and(baaz_core::goals::is_usage_limit) {
            return;
        }
        let windows: Vec<(f64, Option<i64>)> = rt.lane_usage().map(|r| r.windows.iter().map(|w| (w.used_fraction, w.resets_at)).collect()).unwrap_or_default();
        let at = baaz_core::goals::resume_at(baaz_core::goals::usage_reset_ms(&windows), now());
        runtime.update(cx, |rt, cx| rt.wait_goal_for_usage(at, cx));
    }

    /// Once a minute (with the task stall rules): a usage wait that is
    /// over resumes; a running goal that went quiet is nudged, interrupted
    /// and resent, then paused for the person (14.7's rules).
    pub(crate) fn goal_tick(&mut self, now: i64, cx: &mut Context<Self>) {
        for runtime in self.registry.runtimes(self.active.as_ref(), cx) {
            let (state, resume_at, stall, last) = {
                let rt = runtime.read(cx);
                let Some(goal) = rt.goal.as_ref() else { continue };
                (goal.state, goal.resume_at, goal.stall, rt.last_event_ms)
            };
            match state {
                GoalState::UsageWait if resume_at.is_some_and(|at| now >= at) => runtime.update(cx, |rt, cx| rt.end_usage_wait(cx)),
                GoalState::Running => {
                    let step = baaz_core::tasks::stall_step(now, last, stall);
                    runtime.update(cx, |rt, cx| match step {
                        baaz_core::tasks::StallStep::Wait => {}
                        baaz_core::tasks::StallStep::Reset => {
                            if let Some(g) = rt.goal.as_mut() {
                                g.stall = None;
                            }
                            rt.save_goal(cx);
                        }
                        baaz_core::tasks::StallStep::Nudge => {
                            if let Some(g) = rt.goal.as_mut() {
                                g.stall = Some((1, now));
                            }
                            let text = rt.goal.as_ref().map(Goal::nudge_text).unwrap_or_default();
                            rt.save_goal(cx);
                            rt.send_or_queue(text, cx);
                        }
                        baaz_core::tasks::StallStep::Interrupt => {
                            let text = rt.goal.as_mut().map(|g| {
                                g.stall = Some((2, now));
                                g.continue_text(None)
                            });
                            rt.save_goal(cx);
                            if rt.turn_running() {
                                rt.interrupt(cx);
                            }
                            if let Some(text) = text {
                                rt.send_or_queue(text, cx);
                            }
                        }
                        baaz_core::tasks::StallStep::NeedsYou => {
                            if let Some(g) = rt.goal.as_mut() {
                                g.stall = None;
                            }
                            rt.pause_goal(Some("It went quiet: nudged, then interrupted, and still nothing.".into()), cx);
                        }
                    });
                }
                _ => {}
            }
        }
        self.update_keep_awake(cx);
    }

    /// 15.5: the Mac stays awake while any goal runs or waits, or any
    /// delegated worker works.
    pub(crate) fn update_keep_awake(&mut self, cx: &mut Context<Self>) {
        let goals = self.registry.runtimes(self.active.as_ref(), cx).iter().any(|rt| rt.read(cx).goal.as_ref().is_some_and(|g| g.state.active()));
        let workers = !self.registry.orchestra.workers.is_empty();
        crate::keep_awake::hold(goals || workers);
    }

    /// 15.6: a goal the last run left running is resent once, as "Continue
    /// from where you stopped", never its first message; it opens in the
    /// background, like a resumed worker. Never in a capture.
    pub(crate) fn resume_goals_on_open(&mut self, cx: &mut Context<Self>) {
        if crate::runtime::captures() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let goals = cx
                .background_spawn(async move { crate::db::call(crate::db::path(), |opened| baaz_core::db::goals::active(&opened.conn).unwrap_or_default()).unwrap_or_default() })
                .await;
            let _ = this.update(cx, |this, cx| {
                // Only what an earlier run left: a goal this run already
                // changed is live here (Gate 15 live: a goal set in the
                // boot's first seconds was resent to itself).
                let boot = boot_ms();
                for goal in goals.into_iter().filter(|g| !g.resent && g.updated_at < boot) {
                    this.resume_goal_after_restart(goal, cx);
                }
            });
        })
        .detach();
    }

    fn resume_goal_after_restart(&mut self, mut goal: Goal, cx: &mut Context<Self>) {
        let session = goal.session.clone();
        goal.resent = true;
        // Live already (its view opened with the boot): resend there.
        if let Some(runtime) = self.registry.find_runtime(self.active.as_ref(), &session, cx) {
            runtime.update(cx, |rt, cx| {
                rt.goal = Some(goal);
                rt.save_goal(cx);
                rt.send_or_queue(crate::orchestra::CONTINUE_TEXT.to_owned(), cx);
            });
            self.update_keep_awake(cx);
            return;
        }
        // Its mode is not recorded with the goal: the default, which asks.
        let mode = aui_protocol::PermissionMode::default();
        self.run_on_main(cx, move |this, window, cx| {
            let wanted = session.clone();
            this.reopen_worker(&wanted, mode, move |this, resumed, cx| match resumed {
                Ok(session) => {
                    if let Some(runtime) = this.registry.find_runtime(this.active.as_ref(), &session, cx) {
                        runtime.update(cx, |rt, cx| {
                            rt.goal = Some(goal);
                            rt.save_goal(cx);
                            rt.send_or_queue(crate::orchestra::CONTINUE_TEXT.to_owned(), cx);
                        });
                    }
                    this.update_keep_awake(cx);
                }
                Err(why) => crate::baaz_log!("goal: {session} could not resume after the restart ({why})"),
            }, window, cx);
        });
    }
}

#[cfg(feature = "probe")]
impl Harness {
    /// `goal-fixture:<running|met|paused|offer>` (docs/85 15.2 captures, the
    /// 15.5 live check): the open session's goal pill in that state, or the
    /// offer chip, with no turn sent and no row written; keep-awake follows
    /// as it would.
    #[cfg(feature = "probe")]
    pub(crate) fn step_goal_fixture(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let view = self.active.clone().ok_or("no open session")?;
        let runtime = view.read(cx).runtime().clone();
        let at = now();
        let fixture = |state: GoalState, check: &str, reason: Option<&str>| {
            let session = runtime.read(cx).session_id.clone();
            let mut g = Goal::new(&session, "codex", "the failing date tests pass", Some("cargo test".into()), true, baaz_core::goals::limits_for(provider::BillingMode::Subscription), at - 14 * 60_000);
            g.state = state;
            g.turns = 6;
            g.tokens = 212_400;
            g.last_check = Some(check.to_owned());
            g.reason = reason.map(str::to_owned);
            g.updated_at = at;
            g
        };
        let (goal, offer) = match rest.trim() {
            "running" => (Some(fixture(GoalState::Running, "cargo test: failed (exit 101)", None)), None),
            "met" => (Some(fixture(GoalState::Met, "cargo test: passed", None)), None),
            "paused" => (Some(fixture(GoalState::Paused, "cargo test: failed (exit 101)", Some("Paused by you."))), None),
            "offer" => (None, Some("the login test passes".to_owned())),
            other => return Err(format!("goal-fixture: `{other}` is not running, met, paused or offer")),
        };
        runtime.update(cx, |rt, cx| {
            rt.goal = goal;
            rt.goal_offer = offer;
            cx.emit(crate::session::RuntimeEvent::BannerChanged);
        });
        self.update_keep_awake(cx);
        crate::baaz_log!("goal-fixture: keep-awake held = {}", crate::keep_awake::held());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_reads_as_one_line_and_its_tail_is_short() {
        let failed = baaz_core::checks::Outcome { passed: false, code: Some(101), tail: (1..=30).map(|n| n.to_string()).collect::<Vec<_>>().join("\n"), seconds: 2 };
        assert_eq!(check_line("cargo test", &failed), "cargo test: failed (exit 101)");
        assert_eq!(last_lines(&failed.tail, 3), "28\n29\n30");
        let passed = baaz_core::checks::Outcome { passed: true, code: Some(0), tail: String::new(), seconds: 1 };
        assert_eq!(check_line("cargo test", &passed), "cargo test: passed");
    }
}
