//! The runtime half of automatic compaction (docs/85 12.1): what the
//! decision reads off this session, the one timer, and the send.
//!
//! [`baaz_core::auto_compact::decide`] is the rule. This module feeds it
//! (context, running, pending, typing, the last turn's end, billing, the
//! provider's own idle compaction) and acts on its answer: a wait arms one
//! timer that looks again; "now" sends the provider's compaction with a
//! focus line. A key in the composer cancels the timer; the next turn's end
//! arms it again.

use gpui::{Context, Task};

use baaz_core::auto_compact::{self, Decision, Inputs};

use super::SessionRuntime;

/// What the runtime keeps between turns for the decision.
#[derive(Default)]
pub(crate) struct AutoCompact {
    /// A key was pressed in the composer since the last turn ended.
    typed_since_turn: bool,
    /// When the last turn ended (ms since the epoch).
    last_turn_ended_ms: Option<i64>,
    /// This idle stretch was compacted (by the timer).
    compacted_since_turn: bool,
    /// The compaction Baaz sent is the turn running now: its own end does
    /// not start a new idle stretch.
    sent: bool,
    /// The one timer: looks again at the decision's time.
    timer: Option<Task<()>>,
    /// The digest's timer (docs/85 12.4): ten minutes after the last live
    /// turn, the application is told the session went idle.
    digest_timer: Option<Task<()>>,
}

impl SessionRuntime {
    /// The decision's inputs, read off this session now.
    pub(crate) fn auto_compact_inputs(&self) -> Inputs {
        let id = self.provider_kind();
        let side = self.store.side();
        let context = self.fake_context.as_ref().or(side.context.as_ref()).map(|reading| reading.used_tokens);
        Inputs {
            context_tokens: context,
            turn_running: self.busy(),
            pending: self.waiting_on_you().is_some() || !side.queued.is_empty(),
            typed_since_turn: self.auto_compact.typed_since_turn,
            last_turn_ended_ms: self.auto_compact.last_turn_ended_ms,
            compacted_since_turn: self.auto_compact.compacted_since_turn,
            billing: baaz_core::billing::mode(id),
            vendor_compacts_idle: crate::providers::compacts_idle_itself(id),
            can_compact: crate::providers::capability_state(id, provider::Capability::CompactSession).allows_attempt(),
        }
    }

    /// A turn finished: a new idle stretch starts (unless the turn was
    /// Baaz's own compaction), and the timer is set for the moment the
    /// rule could say yes — [`auto_compact::LEAD`] before the hour-long
    /// cache ends. The decision itself runs then, on what holds then (the
    /// turn's context reading lands just after its finish).
    pub(crate) fn auto_compact_turn_ended(&mut self, cx: &mut Context<Self>) {
        let state = &mut self.auto_compact;
        if std::mem::take(&mut state.sent) {
            state.compacted_since_turn = true;
            state.timer = None;
            return;
        }
        state.compacted_since_turn = false;
        state.typed_since_turn = false;
        state.digest_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(baaz_core::digest::IDLE).await;
            let _ = this.update(cx, |this, cx| {
                if !this.busy() {
                    cx.emit(super::SessionEvent::DigestDue { session_id: this.session_id.clone() });
                }
            });
        }));
        let now = crate::clock::now_ms() as i64;
        state.last_turn_ended_ms = Some(now);
        let wait = auto_compact::SUBSCRIPTION_CACHE.saturating_sub(auto_compact::LEAD);
        state.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| this.auto_compact_arm(cx));
        }));
    }

    /// A key in the composer: the person is here, so nothing compacts
    /// under their hands this idle stretch.
    pub(crate) fn auto_compact_typed(&mut self) {
        self.auto_compact.typed_since_turn = true;
        self.auto_compact.timer = None;
    }

    /// Ask the decision now and act: wait (one timer), compact, or nothing.
    pub(crate) fn auto_compact_arm(&mut self, cx: &mut Context<Self>) {
        let now = crate::clock::now_ms() as i64;
        match auto_compact::decide(&self.auto_compact_inputs(), now) {
            Decision::Never(_) => self.auto_compact.timer = None,
            Decision::WaitUntil(at) => {
                let wait = std::time::Duration::from_millis(at.saturating_sub(now).max(0) as u64);
                self.auto_compact.timer = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let _ = this.update(cx, |this, cx| this.auto_compact_arm(cx));
                }));
            }
            Decision::CompactNow => self.auto_compact_send(cx),
        }
    }

    /// Send the compaction with its focus line.
    fn auto_compact_send(&mut self, cx: &mut Context<Self>) {
        self.auto_compact.timer = None;
        self.auto_compact.sent = true;
        self.auto_compact.compacted_since_turn = true;
        let finished = self.last_user_text();
        crate::baaz_log!("auto-compact: session={} context over the threshold, idle, cache ends in 2 min", self.session_id);
        self.compact_with(Some(auto_compact::focus_line(finished.as_deref())), cx);
    }

    /// Whether a send asks the topic check first (docs/85 12.1): idle,
    /// above the threshold, on a subscription (rule 27: background model
    /// calls only there), on a provider whose utility completions run
    /// through its CLI and that can compact.
    pub(crate) fn topic_check_applies(&self) -> bool {
        let inputs = self.auto_compact_inputs();
        !inputs.turn_running
            && inputs.can_compact
            && inputs.billing == baaz_core::billing::BillingMode::Subscription
            && inputs.context_tokens.is_some_and(|tokens| tokens > auto_compact::THRESHOLD_TOKENS)
            && baaz_core::utility::supports(self.provider_kind())
    }

    /// Ask the cheap model whether `text` starts another topic, for at most
    /// [`auto_compact::TOPIC_CHECK_LIMIT`]; compact first when it does,
    /// then send `text` (queued behind the compaction).
    pub(super) fn submit_after_topic_check(&mut self, text: String, cx: &mut Context<Self>) {
        self.submitting = true;
        cx.emit(super::RuntimeEvent::TurnChanged);
        let finished = self.last_user_text().unwrap_or_default();
        let prompt = auto_compact::topic_prompt(&finished, &text);
        let provider = self.provider_kind();
        let model = self.model_id();
        let context = self.auto_compact_inputs().context_tokens;
        let (tx, rx) = std::sync::mpsc::channel();
        // The completion runs to its own deadline on its own thread; the
        // send waits for it only as long as the limit allows. A test's
        // stand-in answers on the test's own thread.
        if cfg!(test) {
            let _ = tx.send(baaz_core::utility::complete(provider, Some(&model), &prompt, None));
        } else {
            std::thread::spawn(move || {
                let answer = baaz_core::utility::complete(provider, Some(&model), &prompt, None);
                let _ = tx.send(answer);
            });
        }
        let check = cx.background_executor().spawn(async move {
            match rx.recv_timeout(auto_compact::TOPIC_CHECK_LIMIT) {
                Ok(Ok(answer)) => auto_compact::read_topic_answer(&answer),
                Ok(Err(_)) => auto_compact::TopicCheck::SameTopic,
                Err(_) => auto_compact::TopicCheck::TimedOut,
            }
        });
        cx.spawn(async move |this, cx| {
            let answer = check.await;
            let _ = this.update(cx, |this, cx| {
                this.submitting = false;
                crate::baaz_log!("topic check: {answer:?}");
                if auto_compact::topic_switch(context, || answer) {
                    this.auto_compact.sent = true;
                    this.compact_with(Some(auto_compact::focus_line(Some(&finished))), cx);
                }
                this.submit_on_provider(text, cx);
            });
        })
        .detach();
    }

    /// `digest-now` (probe): tell the application this session went idle,
    /// as the ten-minute timer would (docs/85 12.4).
    #[cfg(feature = "probe")]
    pub(crate) fn step_digest_now(&mut self, cx: &mut Context<Self>) -> crate::steps::StepResult {
        if self.busy() {
            return Err("digest-now: a turn is running".into());
        }
        cx.emit(super::SessionEvent::DigestDue { session_id: self.session_id.clone() });
        Ok(())
    }

    /// `auto-compact:<seconds>` (probe): run the decision as if `seconds`
    /// had passed since the last turn ended, and act on it. `Ok` names what
    /// it decided; the screen shows what it did.
    #[cfg(feature = "probe")]
    pub(crate) fn step_auto_compact(&mut self, rest: &str, cx: &mut Context<Self>) -> crate::steps::StepResult {
        let seconds: i64 = rest.trim().parse().map_err(|_| format!("auto-compact: want seconds, got {rest:?}"))?;
        // A capture has no settled turn of its own: the fixture's end is now.
        let ended = *self.auto_compact.last_turn_ended_ms.get_or_insert(crate::clock::now_ms() as i64);
        let decision = auto_compact::decide(&self.auto_compact_inputs(), ended + seconds * 1000);
        crate::baaz_log!("auto-compact step: {decision:?}");
        if decision == Decision::CompactNow {
            self.auto_compact_send(cx);
        }
        Ok(())
    }
}
