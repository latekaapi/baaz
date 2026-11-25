//! A session's goal (docs/85 15.1, 15.2, 15.4): "keep going until the tests
//! pass" on every provider. The session holds the record
//! ([`baaz_core::goals::Goal`]); the app judges each goal turn
//! (`crate::goals`), so this half only starts, counts, pauses and stops.
//!
//! How a goal reaches the provider:
//! * [`provider::ProviderTrait::GoalCommand`] (Codex): `SetGoal` with the
//!   goal's words and its check; the provider keeps working toward it.
//! * [`provider::ProviderTrait::GoalSlashCommand`] (Claude Code): its own
//!   `/goal <words>` message, queued behind the person's.
//! * Anything else: the message alone; Baaz's own evaluator judges each
//!   turn and sends the next.
//!
//! Keyed by session id: the `goal` table (`baaz_core::db::goals`).

use baaz_core::goals::{Detected, Goal, GoalState};
use gpui::{AppContext as _, Context};

use super::{RuntimeEvent, SessionEvent, SessionRuntime};

/// The first words a goal sends on a provider that holds none natively.
pub(crate) fn work_toward_text(condition: &str) -> String {
    format!("Work toward this goal until it is met: {condition}")
}

/// What a typed `/goal` asks for: set one, or clear it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    Set(String),
    Clear,
}

/// `/goal <condition>` or `/goal clear`; anything else is not a goal line.
pub(crate) fn typed(text: &str) -> Option<Typed> {
    let rest = text.trim().strip_prefix("/goal")?;
    if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
        return None;
    }
    match rest.trim() {
        "" => None,
        "clear" | "stop" => Some(Typed::Clear),
        condition => Some(Typed::Set(condition.to_owned())),
    }
}

impl SessionRuntime {
    /// The person's message, sent: a clear end in it sets the goal, a
    /// possible one offers the chip (15.1). A typed `/goal` is the goal
    /// itself.
    pub(crate) fn send_with_goal(&mut self, text: String, cx: &mut Context<Self>) {
        if let Some(typed) = typed(&text) {
            match typed {
                Typed::Set(condition) => self.start_goal(condition, None, cx),
                Typed::Clear => self.stop_goal(cx),
            }
            return;
        }
        let running_goal = self.goal.as_ref().is_some_and(|g| g.state.active());
        match baaz_core::goals::detect(&text) {
            Detected::Set(condition) if !running_goal => self.start_goal(condition, Some(text), cx),
            Detected::Offer(condition) if !running_goal => {
                self.goal_offer = Some(condition);
                cx.emit(RuntimeEvent::BannerChanged);
                self.submit(text, cx);
            }
            _ => self.submit(text, cx),
        }
    }

    /// Start a goal on `condition`, sending `first` (the person's message)
    /// or, when the goal was typed, the words that start the work.
    pub(crate) fn start_goal(&mut self, condition: String, first: Option<String>, cx: &mut Context<Self>) {
        let provider = self.provider_kind();
        let check = match baaz_core::checks::detect(std::path::Path::new(&self.workspace)) {
            baaz_core::checks::Check::Command(command) => Some(command),
            baaz_core::checks::Check::Parse => None,
        };
        let native = provider.has(provider::ProviderTrait::GoalCommand) || provider.has(provider::ProviderTrait::GoalSlashCommand);
        let limits = baaz_core::goals::limits_for(baaz_core::billing::mode(provider));
        let goal = Goal::new(&self.session_id, provider.as_str(), &condition, check, native, limits, now());
        let native_text = goal.native_text();
        self.goal = Some(goal);
        self.goal_offer = None;
        self.save_goal(cx);
        if provider.has(provider::ProviderTrait::GoalCommand) {
            self.send_goal_command(Some(native_text.clone()), false, cx);
        }
        let opening = first.unwrap_or_else(|| {
            if provider.has(provider::ProviderTrait::GoalSlashCommand) { String::new() } else { work_toward_text(&condition) }
        });
        if !opening.is_empty() {
            self.submit(opening, cx);
        }
        if provider.has(provider::ProviderTrait::GoalSlashCommand) {
            // Queued behind the message: the provider works on until it
            // judges the goal met (probed: "Goal set: …", then the work).
            self.submit(format!("/goal {native_text}"), cx);
        }
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalChanged);
    }

    /// The chip's "Keep going until …": the offered goal, set. The message
    /// already went; the goal's first turn is the work toward it.
    pub(crate) fn accept_goal_offer(&mut self, cx: &mut Context<Self>) {
        if let Some(condition) = self.goal_offer.take() {
            self.start_goal(condition, None, cx);
        }
    }

    /// The chip's dismiss: nothing is set.
    pub(crate) fn dismiss_goal_offer(&mut self, cx: &mut Context<Self>) {
        if self.goal_offer.take().is_some() {
            cx.emit(RuntimeEvent::BannerChanged);
        }
    }

    /// A live turn of the session finished: a running goal counts it and
    /// the app judges it (`crate::goals`).
    pub(super) fn goal_turn_ended(&mut self, turn_id: &str, meta: &aui_protocol::TurnMeta, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| g.state == GoalState::Running) else { return };
        if goal.last_turn.as_deref() == Some(turn_id) {
            return;
        }
        goal.last_turn = Some(turn_id.to_owned());
        goal.count_turn(meta.tokens_in + meta.tokens_out);
        goal.stall = None;
        self.save_goal(cx);
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalTurnEnded);
    }

    /// Pause: the turn stops and nothing more is sent until Resume.
    pub(crate) fn pause_goal(&mut self, reason: Option<String>, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| g.state.active()) else { return };
        goal.state = GoalState::Paused;
        goal.reason = reason;
        let native = goal.native_text();
        self.save_goal(cx);
        if self.provider_kind().has(provider::ProviderTrait::GoalCommand) {
            self.send_goal_command(Some(native), true, cx);
        }
        if self.turn_running() {
            self.interrupt(cx);
        }
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalChanged);
    }

    /// Resume a paused (or budget-reached, or blocked) goal: the ceiling
    /// starts over from now, and the next turn goes.
    pub(crate) fn resume_goal(&mut self, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| !g.state.active() && g.state != GoalState::Met && g.state != GoalState::Stopped) else { return };
        goal.state = GoalState::Running;
        goal.reason = None;
        goal.resume_at = None;
        goal.started_at = now();
        goal.tokens = 0;
        goal.same_failures = 0;
        let (native, text) = (goal.native_text(), goal.continue_text(None));
        self.save_goal(cx);
        if self.provider_kind().has(provider::ProviderTrait::GoalCommand) {
            self.send_goal_command(Some(native), false, cx);
        }
        self.send_or_queue(text, cx);
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalChanged);
    }

    /// Stop: the turn stops, the provider's own goal is cleared, the pill
    /// goes.
    pub(crate) fn stop_goal(&mut self, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| g.state != GoalState::Stopped) else { return };
        goal.state = GoalState::Stopped;
        self.save_goal(cx);
        if self.provider_kind().has(provider::ProviderTrait::GoalCommand) {
            self.send_goal_command(None, false, cx);
        }
        if self.turn_running() {
            self.interrupt(cx);
        }
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalChanged);
    }

    /// The app's verdict on the goal: its new state (met, still running
    /// with the next message, paused at a limit) and the check's line.
    pub(crate) fn settle_goal(&mut self, state: GoalState, check_line: Option<String>, reason: Option<String>, next: Option<String>, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut() else { return };
        goal.state = state;
        if check_line.is_some() {
            goal.last_check = check_line;
        }
        goal.reason = reason;
        let clear_native = state == GoalState::Met && self.provider_kind().has(provider::ProviderTrait::GoalCommand);
        self.save_goal(cx);
        if clear_native {
            self.send_goal_command(None, false, cx);
        }
        if let Some(text) = next {
            self.send_or_queue(text, cx);
        }
        cx.emit(RuntimeEvent::BannerChanged);
        cx.emit(SessionEvent::GoalChanged);
    }

    /// A usage limit stopped the goal: it waits, and resumes by itself at
    /// `resume_at` (15.4).
    pub(crate) fn wait_goal_for_usage(&mut self, resume_at: i64, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| g.state == GoalState::Running) else { return };
        goal.state = GoalState::UsageWait;
        goal.resume_at = Some(resume_at);
        goal.reason = Some("the provider's usage limit".to_owned());
        self.save_goal(cx);
        cx.emit(RuntimeEvent::BannerChanged);
    }

    /// The wait is over: the goal runs again.
    pub(crate) fn end_usage_wait(&mut self, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut().filter(|g| g.state == GoalState::UsageWait) else { return };
        goal.state = GoalState::Running;
        goal.resume_at = None;
        goal.reason = None;
        let text = goal.continue_text(None);
        self.save_goal(cx);
        self.send_or_queue(text, cx);
        cx.emit(RuntimeEvent::BannerChanged);
    }

    /// Write the record (stamped now) off the UI thread.
    pub(crate) fn save_goal(&mut self, cx: &mut Context<Self>) {
        let Some(goal) = self.goal.as_mut() else { return };
        goal.updated_at = now();
        let goal = goal.clone();
        cx.background_spawn(async move {
            crate::db::call(crate::db::path(), move |opened| baaz_core::db::goals::save(&opened.conn, &goal).ok());
        })
        .detach();
    }

    fn send_goal_command(&mut self, objective: Option<String>, paused: bool, cx: &mut Context<Self>) {
        let command = provider::Command::SetGoal { request_id: provider::new_request_id(), session_id: self.session_id.clone(), objective, paused };
        self.provider_send(command, cx, |_, result, _| {
            if let Err(error) = result {
                crate::baaz_log!("goal: the provider refused its goal ({error})");
            }
        });
    }
}

fn now() -> i64 {
    baaz_core::clock::now_ms() as i64
}

/// `212k`, `1.4M`: tokens as the pill words them.
pub(crate) fn token_words(tokens: u64) -> String {
    match tokens {
        0..=999 => tokens.to_string(),
        1_000..=999_999 => format!("{}k", tokens / 1_000),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

/// The pill's model for `goal` at `now`; `None` once it was stopped.
pub(crate) fn pill_model(goal: &Goal, now: i64) -> Option<aui::transcript::GoalPillModel> {
    use aui::transcript::GoalPillState as P;
    let state = match goal.state {
        GoalState::Running => P::Running,
        GoalState::Met => P::Met,
        GoalState::Paused => P::Paused,
        GoalState::Blocked => P::Blocked,
        GoalState::UsageWait => P::UsageWait,
        GoalState::BudgetReached => P::BudgetReached,
        GoalState::Stopped => return None,
    };
    let check = goal.reason.clone().or_else(|| goal.last_check.clone());
    Some(aui::transcript::GoalPillModel {
        condition: goal.condition.clone().into(),
        state,
        elapsed: goal.elapsed(now).into(),
        turns: goal.turns,
        tokens: token_words(goal.tokens).into(),
        check: check.map(Into::into),
        criteria: Vec::new(),
    })
}

impl super::SessionView {
    /// docs/85 15.2: the goal pill above the composer (Pause or Resume,
    /// Stop), or the offer chip for a possible goal (15.1). `None` with
    /// neither.
    pub(crate) fn render_goal(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        use gpui::{prelude::*, px};
        let rt = self.runtime.read(cx);
        let runtime = self.runtime.clone();
        let element = if let Some(model) = rt.goal.as_ref().and_then(|g| pill_model(g, now())) {
            aui::transcript::goal_pill("goal-pill", model)
                .on_action(move |action, _, cx| {
                    runtime.update(cx, |rt, cx| match action {
                        aui::transcript::GoalAction::Pause => rt.pause_goal(Some("Paused by you.".into()), cx),
                        aui::transcript::GoalAction::Resume => rt.resume_goal(cx),
                        aui::transcript::GoalAction::Stop => rt.stop_goal(cx),
                    })
                })
                .into_any_element()
        } else {
            let condition = rt.goal_offer.clone()?;
            aui::transcript::goal_offer("goal-offer", condition)
                .on_action(move |action, _, cx| {
                    runtime.update(cx, |rt, cx| match action {
                        aui::transcript::GoalOfferAction::Accept => rt.accept_goal_offer(cx),
                        aui::transcript::GoalOfferAction::Dismiss => rt.dismiss_goal_offer(cx),
                    })
                })
                .into_any_element()
        };
        // On the transcript's measure, like the composer under it.
        Some(
            gpui::div()
                .w_full()
                .px(px(super::TRANSCRIPT_PAD_X))
                .pb(px(aui_tokens::scale::SP_2))
                .child(gpui::div().w_full().max_w(px(super::TRANSCRIPT_MEASURE)).mx_auto().child(element))
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_words_tokens_and_hides_a_stopped_goal() {
        assert_eq!(token_words(950), "950");
        assert_eq!(token_words(212_400), "212k");
        assert_eq!(token_words(1_400_000), "1.4M");
        let mut g = Goal::new("s", "codex", "tests pass", None, true, baaz_core::goals::limits_for(provider::BillingMode::Subscription), 0);
        g.last_check = Some("cargo test: failed (exit 101)".into());
        let m = pill_model(&g, 14 * 60_000).unwrap();
        assert_eq!((m.elapsed.as_ref(), m.check.as_deref()), ("14m", Some("cargo test: failed (exit 101)")));
        g.state = GoalState::Stopped;
        assert!(pill_model(&g, 0).is_none());
    }

    #[test]
    fn a_typed_goal_line_sets_or_clears() {
        assert_eq!(typed("/goal the tests pass"), Some(Typed::Set("the tests pass".into())));
        assert_eq!(typed("/goal clear"), Some(Typed::Clear));
        assert_eq!(typed("/goal"), None, "no condition");
        assert_eq!(typed("/goals pass"), None, "another word");
        assert_eq!(typed("keep going until done"), None);
    }
}
