//! The provider lane: one owning subscription per non-muse session.
//!
//! Part of [`SessionView`](super::SessionView); see [`crate::session`] for
//! what the entity owns. Every session view is owned by exactly one lane for
//! its whole life: it folds either `MuseEvent`s through `MuseFold::apply`
//! (muse lane) or `ProviderEvent`s into the same `Session` model (provider
//! lane), never both. The lane is fixed at construction — [`Lane`] has no
//! setter — so no code path can attach a second writer.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{Context, Window};

use super::{SessionHost, SessionView};
use crate::providers::{ExternalApproval, ExternalApprovalKind, ProviderId};
use crate::wire::{ProviderCall as _, SharedProvider};
use aui_protocol::{ApprovalState, Block, Delta, Turn};

/// Which event stream owns a session view. Set at construction, never
/// reassigned: there is no setter and no public field.
pub(super) enum Lane {
    /// The legacy pump: `MuseEvent`s folded through `MuseFold::apply`.
    Muse,
    /// A provider task draining `ProviderEvent`s into `MuseFold::apply_deltas`.
    Provider(ProviderLane),
}

/// What a provider-lane view holds beyond the shared view state: the gated
/// provider, behind an `Arc<Mutex<..>>` so background sends can share it
/// without moving it out of the view.
pub(super) struct ProviderLane {
    provider: Arc<Mutex<provider::Provider>>,
}

/// A question a new provider raised, waiting on the person. The full prompt
/// arrived as a delta; this is the tap on the shoulder, kept so the
/// question surface can answer it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ExternalQuestion {
    /// The provider-side question id.
    pub id: String,
    /// The owning session.
    pub session_id: String,
    /// One-line human summary of what is being asked.
    pub headline: String,
}

/// The pending provider questions of one session, keyed by provider-side id.
///
/// Only recorded in W1; W3 routes the answers.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub(super) struct ExternalQuestions {
    questions: HashMap<String, ExternalQuestion>,
}

impl ExternalQuestions {
    /// Park a raised question, replacing any earlier tap under the same id.
    fn record(&mut self, question: ExternalQuestion) {
        self.questions.insert(question.id.clone(), question);
    }

    /// The newest parked question's headline, by id — what the sidebar row
    /// stands on while a provider lane waits on a person.
    pub(super) fn newest_headline(&self) -> Option<String> {
        self.questions.values().max_by(|a, b| a.id.cmp(&b.id)).map(|question| question.headline.clone())
    }

    /// Look one up, for the question surface.
    #[cfg(test)]
    fn get(&self, id: &str) -> Option<&ExternalQuestion> {
        self.questions.get(id)
    }
}

impl SessionView {
    /// Whether this view rides the provider lane. The muse entry points
    /// (`apply`, `seed_session`, `load_replay`, `reconnected`) refuse on it.
    pub(crate) fn is_provider_lane(&self) -> bool {
        matches!(self.lane, Lane::Provider(_))
    }

    /// Hang up the lane's provider child, if this view owns one. Idempotent:
    /// [`provider::Provider::shutdown`] is, and so is calling this twice.
    /// Closing or replacing a provider-lane view calls this so no `claude`
    /// or `codex` child outlives its view; dropping the view does the same
    /// through [`Drop`].
    pub(crate) fn shutdown_lane(&mut self) {
        if let Lane::Provider(lane) = &self.lane {
            if let Ok(mut provider) = lane.provider.lock() {
                provider.shutdown();
            }
        }
    }

    /// A view over a provider session. Mirrors [`SessionView::new`] minus
    /// the child: the caller hands over an already-connected
    /// [`provider::Provider`] and its bridged event stream, and this spawns
    /// exactly one gpui task draining that stream for the view's whole life.
    ///
    /// W2's open path constructs the real adapters through this; tests
    /// drive it with a scripted provider.
    pub fn new_on_provider(
        session_id: String,
        provider: provider::Provider,
        mut events: UnboundedReceiver<provider::ProviderEvent>,
        host: SessionHost,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let agent = provider.id();
        let workspace = host.workspace.clone();
        let id = session_id.clone();
        let mut this = Self::new_unowned(session_id, host, window, cx);
        this.fold.ensure_session(&id, agent, String::new(), workspace);
        this.lane = Lane::Provider(ProviderLane { provider: Arc::new(Mutex::new(provider)) });
        // A (re)opened lane may have missed an approval or a question while
        // it was away: pull the pending set, the way the muse lane's
        // `reconnected` re-reads `approval/listPending` before trusting the
        // next frame. And the model menu lists what the child returns, so
        // the catalog is asked for on the same open.
        this.request_provider_pending(cx);
        this.request_provider_models(cx);
        this.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let lost = matches!(event, provider::ProviderEvent::ConnectionLost { .. });
                if this.update(cx, |view, cx| view.on_provider_event(event, cx)).is_err() {
                    return;
                }
                // The lane's provider is hung up and will never deliver
                // again: stop draining so a late event cannot surface past
                // the banner.
                if lost {
                    return;
                }
            }
        }));
        this
    }

}

impl Drop for SessionView {
    /// A dropped view owns no child anymore: hang up the lane's provider so
    /// no `claude` or `codex` process outlives the view that spawned it.
    /// Idempotent — [`SessionView::shutdown_lane`] is — so an explicit close
    /// beforehand changes nothing.
    fn drop(&mut self) {
        self.shutdown_lane();
    }
}

impl SessionView {
    /// Fold one provider event: deltas into the transcript, approval and
    /// question taps onto their surfaces, a lost connection onto the banner.
    /// Only the lane task calls this, so `apply_deltas` has exactly one
    /// writer.
    fn on_provider_event(&mut self, event: provider::ProviderEvent, cx: &mut Context<Self>) {
        match event {
            provider::ProviderEvent::Deltas { session_id, deltas } => {
                if session_id.as_deref() != Some(self.session_id.as_str()) {
                    return;
                }
                let session_id = self.session_id.clone();
                let landed = self.fold.apply_deltas(&session_id, deltas);
                if !landed.is_empty() {
                    self.follow = true;
                    self.note_provider_deltas(&landed, cx);
                    cx.notify();
                }
            }
            provider::ProviderEvent::ApprovalRequested { session_id, approval_id, headline } => {
                if session_id != self.session_id {
                    return;
                }
                // The card itself arrived as a delta; this is the tap on the
                // shoulder, carrying only what a decision needs.
                let kind = match self.provider_kind() {
                    ProviderId::ClaudeCode => ExternalApprovalKind::ClaudeCanUseTool,
                    _ => ExternalApprovalKind::CodexCommand,
                };
                self.inject_external_approval(
                    ExternalApproval {
                        id: approval_id,
                        session_id,
                        provider: self.provider_kind(),
                        kind,
                        headline: headline.clone(),
                        reason: headline,
                        dont_ask_again: None,
                        stage_token: None,
                        decision_sent: None,
                    },
                    cx,
                );
            }
            provider::ProviderEvent::QuestionRaised { session_id, question_id, headline } => {
                if session_id != self.session_id {
                    return;
                }
                self.external_questions.record(ExternalQuestion {
                    id: question_id,
                    session_id,
                    headline,
                });
                self.follow = true;
                cx.notify();
            }
            provider::ProviderEvent::ConnectionLost { reason } => {
                // Hang up the lane's provider: it will never deliver again.
                if let Lane::Provider(lane) = &self.lane {
                    if let Ok(mut provider) = lane.provider.lock() {
                        provider.shutdown();
                    }
                }
                self.set_banner(&format!("Provider connection lost: {reason}"), None, cx);
            }
        }
    }

    /// The lane's provider, for tests driving the gate directly.
    #[cfg(test)]
    fn test_provider(&self) -> Arc<Mutex<provider::Provider>> {
        match &self.lane {
            Lane::Provider(lane) => lane.provider.clone(),
            Lane::Muse => panic!("a muse view has no provider lane"),
        }
    }
}

impl SessionView {
    /// The one dispatch point for provider-lane sends: every user action on
    /// a provider lane (submit, steer, stop, reclaim, decide, answer,
    /// shell) travels as a [`provider::Command`] through here, and every
    /// muse-lane action keeps its `MuseClient` call at its own site — the
    /// lane decides, once, at each action's branch.
    ///
    /// [`Provider::send`](provider::Provider::send) blocks like every wire
    /// call, so the command runs on the background executor inside
    /// [`ProviderCall::provider_call`] and the ack comes back through
    /// `update`. The lane mutex is held only inside that background closure
    /// and released when `send` returns — never across an await on the UI
    /// thread.
    ///
    /// An `InterruptTurn` is never stuck behind a long in-flight send:
    /// neither adapter's `SubmitInput` spans the turn. Claude Code's
    /// `submit_text` writes one stdin line and returns the submission
    /// handle (`crates/provider-claude-code/src/lib.rs`), and Codex's
    /// `submit_text` sends one `turn/start` frame and waits only for its
    /// `turn_id` (`crates/provider-codex/src/lib.rs`) — both millisecond
    /// RPCs, after which the turn streams back as events. The mutex hold is
    /// that RPC, so a stop press waits behind at most one short send.
    pub(super) fn lane_provider(&self) -> Option<SharedProvider> {
        match &self.lane {
            Lane::Provider(lane) => Some(lane.provider.clone()),
            Lane::Muse => None,
        }
    }

    /// Send one neutral command on the provider lane and hand its ack to
    /// `then` on the UI thread. A no-op on the muse lane: muse actions
    /// never reach here, they keep their `wire_call` sites.
    pub(super) fn provider_send(
        &mut self,
        command: provider::Command,
        cx: &mut Context<Self>,
        then: impl FnOnce(&mut Self, Result<provider::Ack, provider::ProviderError>, &mut Context<Self>) + 'static,
    ) {
        let Some(provider) = self.lane_provider() else { return };
        self.provider_call(cx, provider, command, then);
    }

    /// One place that surfaces a provider refusal: an `Unsupported` shows
    /// its reason in the session banner — never swallowed — and anything
    /// else banners its own text, the same surface muse errors use.
    pub(super) fn report_provider_error(
        &mut self,
        error: &provider::ProviderError,
        cx: &mut Context<Self>,
    ) {
        match error {
            provider::ProviderError::Unsupported { reason, .. } => {
                self.set_banner(reason, None, cx);
            }
            other => {
                let text = other.to_string();
                self.set_banner(&text, None, cx);
            }
        }
    }

    /// Derive the running turn from the folded transcript: the muse lane
    /// reads `turn/started`/`turn/completed`, and the provider lane has no
    /// such events — its turns open with `TurnStarted` and settle with
    /// `TurnFinished` (interrupted and failed turns finish too: Codex ends
    /// `interrupt.jsonl` at `turn/completed` with `status: "interrupted"`,
    /// Claude Code emits one `TurnFinished` per open turn). The stop
    /// button, the "working" indicator and
    /// [`SessionView::reply_complete_for_running_turn`] all read
    /// `self.running`, so they follow this with no second source.
    fn note_provider_deltas(&mut self, landed: &[Delta], cx: &mut Context<Self>) {
        for delta in landed {
            match delta {
                Delta::TurnStarted { turn } => match turn {
                    Turn::Assistant { id, .. } => {
                        self.completed_turns.remove(id);
                        self.running = Some(super::Running {
                            turn_id: id.clone(),
                            started: crate::clock::now_instant(),
                        });
                        self.submitting = false;
                        self.last_tick_secs = None;
                        self.start_ticker(cx);
                    }
                    Turn::User { .. } => {
                        self.submitting = false;
                    }
                },
                Delta::TurnFinished { turn_id, meta, .. } => {
                    if self.completed_turns.len() >= super::MAX_COMPLETED_TURNS {
                        self.completed_turns.clear();
                    }
                    self.completed_turns.insert(turn_id.clone());
                    // The chip's fallback: the last finished turn's model.
                    // Replayed history lands here first, so a reopened
                    // session names its model before any live turn runs —
                    // and a refused pick (see `pick_model`) never clears
                    // it, because that path touches `pending_model` only.
                    if !meta.model.is_empty() {
                        self.history_model = Some(meta.model.clone());
                    }
                    if self.running.as_ref().is_some_and(|r| r.turn_id == *turn_id) {
                        self.clear_running();
                    }
                    self.submitting = false;
                    // The lane's terminal: the application records the
                    // byline, the ledger row and the record bump, the way
                    // the muse route's `turn/completed` arm does for its
                    // lane. Replayed history settles here too; every write
                    // below is idempotent, so a reopen never double-counts.
                    cx.emit(super::SessionEvent::ProviderTurnFinished {
                        session_id: self.session_id.clone(),
                        turn_id: turn_id.clone(),
                        meta: meta.clone(),
                    });
                }
                Delta::TurnRemoved { turn_id } => {
                    if self.running.as_ref().is_some_and(|r| r.turn_id == *turn_id) {
                        self.clear_running();
                    }
                    self.submitting = false;
                }
                // An approval block that leaves `Pending` is the server's
                // resolution: the only thing that ever settles the card
                // after the press, per the no-optimism rule. Either way —
                // a card landing or settling — the sidebar row re-reads
                // the pending words, so it stands on the needs-you state
                // while the approval waits instead of `Working`.
                Delta::BlockAdded { block, .. } | Delta::BlockUpdated { block, .. } => {
                    if let Block::Approval { id, state, .. } = block {
                        if *state != ApprovalState::Pending {
                            self.resolve_external_approval(id, cx);
                        }
                        cx.emit(super::SessionEvent::ProviderApprovalsChanged {
                            session_id: self.session_id.clone(),
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// Adopt the transcript's open assistant turn, if any: an ack can beat
    /// its deltas (admission, never outcome), so a `TurnAccepted` that
    /// finds no running turn syncs onto the turn the deltas already
    /// opened rather than leaving the stop button down.
    pub(super) fn adopt_open_provider_turn(&mut self, cx: &mut Context<Self>) {
        if self.running.is_some() {
            return;
        }
        let open = self.session().and_then(|session| match session.turns.last() {
            Some(Turn::Assistant { id, .. }) if !self.completed_turns.contains(id) => Some(id.clone()),
            _ => None,
        });
        if let Some(turn_id) = open {
            self.running = Some(super::Running { turn_id, started: crate::clock::now_instant() });
            self.last_tick_secs = None;
            self.start_ticker(cx);
            cx.notify();
        }
    }

    /// Ask the lane's child for its model catalog after open, so the model
    /// menu lists what the child returns and the chip reads the effective
    /// model's human name. Claude Code has no catalog surface — Baaz owns
    /// its supplied alias list (the `Emulated` cell) — so that folds
    /// synchronously instead of asking. A refusal or an empty answer
    /// records the typed reason for the picker's stand-in row and stays
    /// quiet otherwise: a background fetch never banners.
    pub(super) fn request_provider_models(&mut self, cx: &mut Context<Self>) {
        if self.provider_kind() == ProviderId::ClaudeCode {
            let current = self.model_id();
            let rows = crate::providers::claude_code_catalog(Some(current.as_str()));
            let provider = self.provider_id.clone();
            self.apply_model_catalog(rows, &provider, cx);
            return;
        }
        let command = provider::Command::ListModels { session: Some(self.session_id.clone()) };
        self.provider_send(command, cx, |this, result, cx| match result {
            Ok(provider::Ack::ModelCatalog { models, provider }) => {
                this.apply_model_catalog(models, &provider, cx);
            }
            Ok(_) => {
                this.models = Vec::new();
                this.models_error = Some("the model catalog answered without a catalog".to_owned());
                cx.notify();
            }
            Err(error) => {
                crate::baaz_log!("provider lane: ListModels refused: {error}");
                this.models = Vec::new();
                this.models_error = Some(error.to_string());
                cx.notify();
            }
        });
    }

    /// Pull the lane's pending set on (re)open, so an approval or a
    /// question that arrived while the view was away still shows. Point in
    /// time — acting on it stays race-safe through the seam's stage and
    /// turn guards.
    pub(super) fn request_provider_pending(&mut self, cx: &mut Context<Self>) {
        let command = provider::Command::ListPending { session_id: self.session_id.clone() };
        self.provider_send(command, cx, |this, result, cx| match result {
            Ok(provider::Ack::PendingWork { approvals, questions }) => {
                this.apply_pending_work(approvals, questions, cx);
            }
            // A lane that cannot list (scripted providers answer
            // `Unsupported`) simply shows what the event stream brings:
            // the pull is a second chance, never a gate.
            Ok(_) => {}
            Err(error) => {
                crate::baaz_log!("provider lane: ListPending refused: {error}");
                let _ = cx;
                let _ = error;
            }
        });
    }

    /// Park a pulled pending set without duplicating live taps: anything
    /// the event stream already parked wins, and a pulled approval carries
    /// the stage token a later decision must echo.
    fn apply_pending_work(
        &mut self,
        approvals: Vec<provider::PendingApproval>,
        questions: Vec<provider::PendingQuestion>,
        cx: &mut Context<Self>,
    ) {
        for pending in approvals {
            if self.external_approvals.get(&pending.id).is_some() {
                continue;
            }
            let kind = match self.provider_kind() {
                ProviderId::ClaudeCode => ExternalApprovalKind::ClaudeCanUseTool,
                _ => ExternalApprovalKind::CodexCommand,
            };
            self.external_approvals.inject(ExternalApproval {
                id: pending.id,
                session_id: pending.session_id,
                provider: self.provider_kind(),
                kind,
                headline: pending.headline.clone(),
                reason: pending.headline,
                dont_ask_again: None,
                stage_token: pending.stage_token,
                decision_sent: None,
            });
        }
        for pending in questions {
            self.external_questions.record(ExternalQuestion {
                id: pending.id,
                session_id: pending.session_id,
                headline: pending.headline,
            });
        }
        self.follow = true;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::MuseEvent;
    use gpui::{AppContext as _, Entity};

    /// A provider-lane view over a connected scripted provider, with the
    /// sender half of its lane channel held back for the test to drive.
    fn open_lane_view(
        vc: &mut gpui::VisualTestContext,
        session_id: &str,
    ) -> (Entity<SessionView>, futures::channel::mpsc::UnboundedSender<provider::ProviderEvent>) {
        use provider::ProviderAdapter as _;

        let mut adapter = provider::scripted::ScriptedProvider::new();
        adapter
            .connect(&provider::ConnectInfo::new("baaz", "0.0.0"))
            .expect("a scripted provider connects");
        let provider = provider::Provider::new(adapter);
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let session_id = session_id.to_owned();
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id: "codex".to_owned(),
                workspace: "/tmp/w1-lane".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::shot::CaptureToken::default(),
            };
            cx.new(|cx| SessionView::new_on_provider(session_id, provider, rx, host, window, cx))
        });
        (view, tx)
    }

    /// Submit through the gate and forward what the scripted provider emits
    /// onto the lane channel, the way the connection bridge does.
    fn submit_and_deliver(
        view: &Entity<SessionView>,
        tx: &futures::channel::mpsc::UnboundedSender<provider::ProviderEvent>,
        vc: &mut gpui::VisualTestContext,
        text: &str,
    ) {
        vc.update(|_, cx| {
            let lane = view.read(cx).test_provider();
            let guard = lane.lock().expect("lane provider");
            guard
                .send(provider::Command::SubmitInput {
                    request_id: "r-1".into(),
                    session_id: "s-1".into(),
                    parts: vec![provider::SubmissionPart::Text(text.to_owned())],
                    display_text: None,
                    effort: None,
                })
                .expect("scripted providers take input");
            for event in guard.events().try_iter() {
                tx.unbounded_send(event).expect("the lane channel is open");
            }
        });
        vc.run_until_parked();
    }

    fn assistant_texts(view: &Entity<SessionView>, vc: &mut gpui::VisualTestContext) -> Vec<String> {
        vc.update(|_, cx| {
            view.read(cx)
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .flat_map(|turn| turn.blocks())
                        .filter_map(|block| match block {
                            aui_protocol::Block::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    #[gpui::test]
    fn provider_lane_folds_scripted_deltas(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        vc.update(|_, cx| {
            // Three tasks: the lane's one drain loop, plus the one-shot
            // `ListPending` the open pulls so a missed approval still
            // shows, plus the one-shot `ListModels` the open pulls so the
            // menu lists what the child returns.
            assert_eq!(view.read(cx).tasks.len(), 3, "the drain loop plus the open's two pulls");
            assert!(view.read(cx).is_provider_lane());
        });
        submit_and_deliver(&view, &tx, vc, "hello");
        let texts = assistant_texts(&view, vc);
        assert!(
            texts.iter().any(|text| text.contains("echo: hello")),
            "the folded session has the scripted turn text, drew {texts:?}"
        );
    }

    #[gpui::test]
    fn provider_lane_refuses_muse_events(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        submit_and_deliver(&view, &tx, vc, "hello");
        let before = vc.update(|_, cx| view.read(cx).session().cloned());
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.apply(
                    MuseEvent::Notification {
                        method: "turn/started".to_owned(),
                        params: serde_json::json!({"turnId": "t-9"}),
                        cursor: None,
                        session_id: Some("s-1".to_owned()),
                    },
                    cx,
                );
            });
        });
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).session().cloned(), before, "a muse event folds nothing");
            assert!(!view.read(cx).busy(), "a refused start never marks running");
        });
    }

    #[gpui::test]
    fn approval_requested_lands_in_external_approvals(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::ApprovalRequested {
                session_id: "s-1".to_owned(),
                approval_id: "ap-1".to_owned(),
                headline: "rm -rf /tmp/probe".to_owned(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let approval = view
                .read(cx)
                .external_approvals
                .get("ap-1")
                .expect("the tap parks on the approvals surface");
            assert_eq!(approval.headline, "rm -rf /tmp/probe");
        });
    }

    #[gpui::test]
    fn a_lane_approval_tap_drives_the_needs_you_row(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::ApprovalRequested {
                session_id: "s-1".to_owned(),
                approval_id: "ap-1".to_owned(),
                headline: "rm -rf /tmp/probe".to_owned(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            // The tap lives beside the fold, so the row reads it from
            // there: without the `row_pending` fallback the row said
            // `Working` while the approval waited.
            let (approval, _) = view.read(cx).row_pending();
            assert_eq!(approval.as_deref(), Some("rm -rf /tmp/probe"));
            assert_eq!(
                view.read(cx).waiting_on_you(),
                Some((1, 0)),
                "the needs-you banner lights for a lane approval too"
            );
        });
    }

    #[gpui::test]
    fn question_raised_is_recorded_and_connection_loss_banners(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::QuestionRaised {
                session_id: "s-1".to_owned(),
                question_id: "q-1".to_owned(),
                headline: "Which region?".to_owned(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let question = view
                .read(cx)
                .external_questions
                .get("q-1")
                .expect("the tap is recorded for the question surface");
            assert_eq!(question.headline, "Which region?");
        });
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::ConnectionLost { reason: "child exited".into() })
                .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            assert!(
                view.read(cx)
                    .banner
                    .as_deref()
                    .is_some_and(|banner| banner.contains("child exited")),
                "the lost connection shows the session error banner"
            );
        });
    }

    #[gpui::test]
    fn seed_session_is_refused_on_a_provider_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, _tx) = open_lane_view(vc, "s-1");
        // The lane records an empty model at construction; a `session/started`
        // seed carrying one would overwrite it if it ever folded.
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).session().map(|s| s.model.clone()), Some(String::new()));
            view.update(cx, |view, cx| {
                view.seed_session(
                    serde_json::json!({
                        "sessionId": "s-1",
                        "modelId": "seeded-model",
                        "createdAt": "2026-09-26T00:00:00Z",
                        "path": "/tmp/w1-lane",
                        "status": "idle",
                        "turnCount": 0,
                        "updatedAt": "2026-09-26T00:00:00Z",
                    }),
                    cx,
                );
            });
        });
        vc.update(|_, cx| {
            assert_eq!(
                view.read(cx).session().map(|s| s.model.clone()),
                Some(String::new()),
                "the refused seed changes nothing about the folded session"
            );
        });
    }

    // `reconnected`'s guard test lives beside the view in `session.rs`:
    // driving it needs a real child handle, and this file must stay out of
    // the seam ratchet's coupling list.

    #[gpui::test]
    fn load_replay_is_refused_on_a_provider_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, _tx) = open_lane_view(vc, "s-1");
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.load_replay(std::path::Path::new("/nonexistent/capture.jsonl"), cx)
            });
        });
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert!(!view.replay, "the refused replay leaves the live view live");
            assert!(view.banner.is_none(), "the refused replay banners nothing");
        });
    }

    #[gpui::test]
    fn dropping_the_view_shuts_its_child_down(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        // The lane's provider, held past the view so the shutdown below is
        // observable: scripted providers share state across handles, and a
        // shut one refuses every later command as shut down.
        let lane = vc.update(|_, cx| view.read(cx).test_provider());
        drop(view);
        drop(tx);
        vc.run_until_parked();
        // Dropped entities are reclaimed at the end of the effect cycle, so
        // the view's `Drop` — which hangs up the child — runs on the next
        // update, not on the `drop` above.
        vc.update(|_, _| {});
        let guard = lane.lock().expect("lane provider");
        match guard.send(provider::Command::SubmitInput {
            request_id: "r-late".into(),
            session_id: "s-1".into(),
            parts: vec![provider::SubmissionPart::Text("too late".into())],
            display_text: None,
            effort: None,
        }) {
            Err(provider::ProviderError::Unavailable { reason }) => {
                assert!(reason.contains("shut down"), "the child hung up, not something else: {reason}");
            }
            other => panic!("a dropped view's child must refuse, answered {other:?}"),
        }
    }

    #[gpui::test]
    fn nothing_surfaces_after_the_connection_is_lost(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, tx) = open_lane_view(vc, "s-1");
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::ConnectionLost { reason: "child exited".into() })
                .expect("the lane channel is open");
        });
        vc.run_until_parked();
        // The drain loop is gone with the connection, so the receiver is
        // dropped: a late tap may not even send, and must never surface.
        let _ = tx.unbounded_send(provider::ProviderEvent::ApprovalRequested {
            session_id: "s-1".to_owned(),
            approval_id: "ap-late".to_owned(),
            headline: "a tap from after the hangup".to_owned(),
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert!(
                view.banner.as_deref().is_some_and(|banner| banner.contains("child exited")),
                "the lost connection still shows the session error banner"
            );
            assert!(
                view.external_approvals.get("ap-late").is_none(),
                "an approval raised after ConnectionLost never reaches the surface"
            );
        });
    }

    // ------------------------------------------------- W3: actions on the lane

    /// W3's recording double: `Native` on every W3 capability (submit,
    /// steer, turn control, approvals, questions, shell, lifecycle,
    /// catalog, transcript) and refusing the rest — so each test below
    /// fails when its routing arm is removed, because the arm's command
    /// never lands in `received`. W4 adds fork, compact and session
    /// config as `Native` for the same reason, and serves a configurable
    /// model catalog on `ListModels` (empty by default, which folds to
    /// the typed-reason row).
    ///
    /// Lives in baaz test code only: the provider crate's own
    /// [`provider::scripted::ScriptedProvider`] answers just three
    /// commands, and the crate itself is untouched.
    struct RecordingProvider {
        inner: std::sync::Arc<RecordingInner>,
    }

    struct RecordingInner {
        state: Mutex<RecordingState>,
        tx: crossbeam_channel::Sender<provider::ProviderEvent>,
        rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
    }

    struct RecordingState {
        connected: bool,
        next_turn: u64,
        received: Vec<provider::Command>,
        pending_approvals: Vec<provider::PendingApproval>,
        pending_questions: Vec<provider::PendingQuestion>,
        catalog: Vec<provider::ModelSummary>,
        catalog_provider: String,
    }

    /// Shared handle to what the double saw, held past the view.
    #[derive(Clone)]
    struct RecordingHandle {
        inner: std::sync::Arc<RecordingInner>,
    }

    impl RecordingHandle {
        fn received(&self) -> Vec<provider::Command> {
            self.inner.state.lock().expect("recording mutex").received.clone()
        }

        fn commands_of(&self, capability: &str) -> Vec<provider::Command> {
            self.received().into_iter().filter(|c| c.capability() == capability).collect()
        }

        fn question_commands(&self) -> Vec<provider::Command> {
            self.received()
                .into_iter()
                .filter(|c| {
                    matches!(
                        c,
                        provider::Command::AnswerQuestion { .. }
                            | provider::Command::DismissQuestion { .. }
                            | provider::Command::ClarifyQuestion { .. }
                    )
                })
                .collect()
        }
    }

    impl RecordingProvider {
        fn new() -> (Self, RecordingHandle) {
            Self::with_pending(Vec::new(), Vec::new())
        }

        /// A double serving a model catalog: `ListModels` answers `models`
        /// with `active` flagging the effective model, the way a live child
        /// does. Fork, compact and session config answer `Native` here so
        /// each W4 routing arm has something to land in `received`.
        fn with_catalog(models: Vec<provider::ModelSummary>) -> (Self, RecordingHandle) {
            let (adapter, handle) = Self::with_pending(Vec::new(), Vec::new());
            {
                let mut state = adapter.inner.state.lock().expect("recording mutex");
                state.catalog = models;
                state.catalog_provider = "recording".to_owned();
            }
            (adapter, handle)
        }

        fn with_pending(
            approvals: Vec<provider::PendingApproval>,
            questions: Vec<provider::PendingQuestion>,
        ) -> (Self, RecordingHandle) {
            let (tx, rx) = crossbeam_channel::unbounded();
            let inner = std::sync::Arc::new(RecordingInner {
                state: Mutex::new(RecordingState {
                    connected: false,
                    next_turn: 0,
                    received: Vec::new(),
                    pending_approvals: approvals,
                    pending_questions: questions,
                    catalog: Vec::new(),
                    catalog_provider: "recording".to_owned(),
                }),
                tx,
                rx,
            });
            (Self { inner: inner.clone() }, RecordingHandle { inner })
        }

        fn emit(&self, event: provider::ProviderEvent) {
            let _ = self.inner.tx.send(event);
        }
    }

    impl provider::ProviderAdapter for RecordingProvider {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Codex
        }

        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            let mut state = self.inner.state.lock().expect("recording mutex");
            if state.connected {
                return Err(provider::ProviderError::Rejected { reason: "already connected".into() });
            }
            state.connected = true;
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Codex,
                agent_name: "recording".into(),
                agent_version: "0.0.0".into(),
            })
        }

        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable {
                reason: "the recording double answers nothing here".into(),
            };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::ForkSession, native.clone()),
                (Capability::CompactSession, native.clone()),
                (Capability::SessionConfig, native.clone()),
                (Capability::SessionShell, native.clone()),
                (Capability::SubmitTurn, native.clone()),
                (Capability::SteerTurn, native.clone()),
                (Capability::TurnControl, native.clone()),
                (Capability::ModelCatalog, native.clone()),
                (Capability::Approvals, native.clone()),
                (Capability::Questions, native.clone()),
                (Capability::Transcript, native.clone()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
            ])
        }

        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            let turn_id = {
                let mut state = self.inner.state.lock().expect("recording mutex");
                if !state.connected {
                    return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
                }
                state.received.push(command.clone());
                state.next_turn += 1;
                format!("a-{}", state.next_turn)
            };
            match command {
                provider::Command::OpenSession { .. } => {
                    Ok(provider::Ack::Session { session_id: "s-1".into(), title: None })
                }
                provider::Command::SubmitInput { session_id, parts, .. } => {
                    // An open turn, deliberately never finished here: the
                    // test finishes it when it wants the stop button down.
                    let text = parts
                        .iter()
                        .filter_map(|part| match part {
                            provider::SubmissionPart::Text(text) => Some(text.clone()),
                            provider::SubmissionPart::Image { .. } => None,
                        })
                        .collect::<Vec<_>>()
                        .join("");
                    let n = turn_id.clone();
                    self.emit(provider::ProviderEvent::Deltas {
                        session_id: Some(session_id),
                        deltas: vec![
                            aui_protocol::Delta::TurnStarted {
                                turn: aui_protocol::Turn::User {
                                    id: format!("u-{n}"),
                                    text,
                                    attachments: Vec::new(),
                                    mentions: Vec::new(),
                                    timestamp: None,
                                },
                            },
                            aui_protocol::Delta::TurnStarted {
                                turn: aui_protocol::Turn::Assistant {
                                    id: turn_id.clone(),
                                    blocks: Vec::new(),
                                    meta: aui_protocol::TurnMeta::default(),
                                    timestamp: None,
                                },
                            },
                        ],
                    });
                    Ok(provider::Ack::TurnAccepted { turn_id })
                }
                provider::Command::SteerInput { .. } => Ok(provider::Ack::Accepted),
                provider::Command::InterruptTurn { session_id, turn, .. } => {
                    // The interrupted turn still finishes — Codex ends
                    // `interrupt.jsonl` at `turn/completed` with
                    // `status: "interrupted"` — so the stop button drops.
                    if let Some(finished) = turn {
                        self.emit(provider::ProviderEvent::Deltas {
                            session_id: Some(session_id),
                            deltas: vec![aui_protocol::Delta::TurnFinished {
                                turn_id: finished,
                                meta: aui_protocol::TurnMeta::default(),
                            }],
                        });
                    }
                    Ok(provider::Ack::Accepted)
                }
                provider::Command::CancelTurn { .. } | provider::Command::ReclaimQueued { .. } => {
                    Ok(provider::Ack::Accepted)
                }
                // No auto-resolve, ever: the card waits for the test's
                // explicit resolution, per the no-optimism rule.
                provider::Command::DecideApproval { .. } => Ok(provider::Ack::Accepted),
                provider::Command::ListPending { .. } => {
                    let state = self.inner.state.lock().expect("recording mutex");
                    Ok(provider::Ack::PendingWork {
                        approvals: state.pending_approvals.clone(),
                        questions: state.pending_questions.clone(),
                    })
                }
                provider::Command::AnswerQuestion { .. }
                | provider::Command::DismissQuestion { .. }
                | provider::Command::ClarifyQuestion { .. } => Ok(provider::Ack::Accepted),
                provider::Command::RunShell { .. } => Ok(provider::Ack::Accepted),
                provider::Command::ListModels { .. } => {
                    let state = self.inner.state.lock().expect("recording mutex");
                    Ok(provider::Ack::ModelCatalog {
                        models: state.catalog.clone(),
                        provider: state.catalog_provider.clone(),
                    })
                }
                provider::Command::SelectModel { .. }
                | provider::Command::SelectApprovalMode { .. }
                | provider::Command::CompactSession { .. } => Ok(provider::Ack::Accepted),
                provider::Command::ForkSession { .. } => {
                    Ok(provider::Ack::Session { session_id: "s-fork".into(), title: None })
                }
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "the recording double answers nothing here",
                )),
            }
        }

        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.inner.rx.clone()
        }

        fn shutdown(&mut self) {}
    }

    /// A provider-lane view over the recording double, with the sender half
    /// of its lane channel held back for the test to drive.
    fn open_recording_view(
        vc: &mut gpui::VisualTestContext,
        session_id: &str,
        provider_id: &str,
        adapter: RecordingProvider,
    ) -> (Entity<SessionView>, futures::channel::mpsc::UnboundedSender<provider::ProviderEvent>) {
        use provider::ProviderAdapter as _;
        let mut adapter = adapter;
        adapter
            .connect(&provider::ConnectInfo::new("baaz", "0.0.0"))
            .expect("a recording provider connects");
        let provider = provider::Provider::new(adapter);
        let (tx, rx) = futures::channel::mpsc::unbounded();
        let session_id = session_id.to_owned();
        let provider_id = provider_id.to_owned();
        let view = vc.update(|window, cx| {
            let host = SessionHost {
                provider_id,
                workspace: "/tmp/w3-lane".to_owned(),
                overlays: cx.new(|_| crate::overlays::Overlays::default()),
                capture: crate::shot::CaptureToken::default(),
            };
            cx.new(|cx| SessionView::new_on_provider(session_id, provider, rx, host, window, cx))
        });
        vc.run_until_parked();
        (view, tx)
    }

    /// Forward what the recording double emitted onto the lane channel, the
    /// way the connection bridge does.
    fn drain_recording(
        view: &Entity<SessionView>,
        tx: &futures::channel::mpsc::UnboundedSender<provider::ProviderEvent>,
        vc: &mut gpui::VisualTestContext,
    ) {
        vc.update(|_, cx| {
            let lane = view.read(cx).test_provider();
            let guard = lane.lock().expect("lane provider");
            for event in guard.events().try_iter() {
                tx.unbounded_send(event).expect("the lane channel is open");
            }
        });
        vc.run_until_parked();
    }

    fn test_image() -> crate::images::Image {
        crate::images::Image {
            id: "img-1".to_owned(),
            name: "shot.png".to_owned(),
            media_type: "image/png".to_owned(),
            base64_data: "aGVsbG8=".to_owned(),
            width: 1,
            height: 1,
            thumb: None,
            pending: false,
        }
    }

    fn question_deltas() -> Vec<aui_protocol::Delta> {
        use aui_protocol::{Block, Delta, QuestionOption, Turn, TurnMeta};
        vec![
            Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: "a-q".to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp: None,
                },
            },
            Delta::BlockAdded {
                turn_id: "a-q".to_owned(),
                block: Block::Question {
                    id: "q-1".to_owned(),
                    header: "Region".to_owned(),
                    prompt: "Which region?".to_owned(),
                    subtitle: String::new(),
                    options: vec![
                        QuestionOption {
                            label: "fast".to_owned(),
                            description: String::new(),
                            key: "1".to_owned(),
                            preview: None,
                        },
                        QuestionOption {
                            label: "safe".to_owned(),
                            description: String::new(),
                            key: "2".to_owned(),
                            preview: None,
                        },
                    ],
                    multi: false,
                    allow_other: false,
                    answer: None,
                    timeout_ms: None,
                },
            },
        ]
    }

    #[gpui::test]
    fn submit_reaches_the_provider_with_text_image_and_display_text(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.images.push(test_image());
                view.send_text("hello there".to_owned(), cx);
            });
        });
        vc.run_until_parked();
        let submits = handle.commands_of("submit-input");
        assert_eq!(submits.len(), 1, "one SubmitInput leaves the lane, drew {submits:?}");
        match &submits[0] {
            provider::Command::SubmitInput { parts, display_text, .. } => {
                assert_eq!(display_text.as_deref(), Some("hello there"));
                assert_eq!(
                    *parts,
                    vec![
                        provider::SubmissionPart::Text("hello there".to_owned()),
                        provider::SubmissionPart::Image {
                            base64_data: "aGVsbG8=".to_owned(),
                            media_type: "image/png".to_owned(),
                        },
                    ],
                    "text plus the composer's image, in order"
                );
            }
            other => panic!("submit must travel as SubmitInput, travelled as {other:?}"),
        }
        // The turn streams back through the lane: the stop button's world
        // is the transcript now.
        drain_recording(&view, &tx, vc);
        vc.update(|_, cx| {
            assert!(view.read(cx).busy(), "the open turn marks the view busy");
        });
    }

    #[gpui::test]
    fn steer_interject_reaches_the_provider_with_the_running_turn(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("work".to_owned(), cx)));
        vc.run_until_parked();
        drain_recording(&view, &tx, vc);
        let running = vc.update(|_, cx| {
            assert!(view.read(cx).busy(), "the turn runs before the steer");
            view.read(cx).running.as_ref().map(|r| r.turn_id.clone()).expect("a running turn id")
        });
        vc.update(|_, cx| view.update(cx, |view, cx| view.steer_text("follow up".to_owned(), cx)));
        vc.run_until_parked();
        let steers = handle.commands_of("steer-input");
        assert_eq!(steers.len(), 1, "one SteerInput leaves the lane, drew {steers:?}");
        match &steers[0] {
            provider::Command::SteerInput { expected_turn, parts, .. } => {
                assert_eq!(expected_turn, &running, "the steer names the running turn");
                assert_eq!(*parts, vec![provider::SubmissionPart::Text("follow up".to_owned())]);
            }
            other => panic!("a steer must travel as SteerInput, travelled as {other:?}"),
        }
    }

    #[gpui::test]
    fn stop_sends_interrupt_and_the_finish_settles_the_view(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("work".to_owned(), cx)));
        vc.run_until_parked();
        drain_recording(&view, &tx, vc);
        vc.update(|_, cx| {
            assert!(view.read(cx).busy(), "the stop button's world: the provider works");
            view.update(cx, |view, cx| view.interrupt(cx));
        });
        vc.run_until_parked();
        let stops = handle.commands_of("interrupt-turn");
        assert_eq!(stops.len(), 1, "one turn-control command leaves the lane, drew {stops:?}");
        match &stops[0] {
            provider::Command::InterruptTurn { turn, retract, .. } => {
                assert!(retract, "stop retracts an un-started turn");
                assert!(turn.is_some(), "stop names the running turn");
            }
            other => panic!("stop must travel as InterruptTurn, travelled as {other:?}"),
        }
        drain_recording(&view, &tx, vc);
        vc.update(|_, cx| {
            assert!(!view.read(cx).busy(), "the finished turn drops the stop button");
        });
    }

    #[gpui::test]
    fn approval_press_sends_decide_and_waits_for_the_resolution(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.inject_external_approval(crate::providers::ExternalApproval {
                    id: "ap-1".to_owned(),
                    session_id: "s-1".to_owned(),
                    provider: crate::providers::ProviderId::Codex,
                    kind: crate::providers::ExternalApprovalKind::CodexCommand,
                    headline: "rm -rf /tmp/probe".to_owned(),
                    reason: "the model wants a clean slate".to_owned(),
                    dont_ask_again: None,
                    stage_token: Some("st-1".to_owned()),
                    decision_sent: None,
                }, cx);
            });
            view.update(cx, |view, cx| {
                view.decide_external_approval(
                    "ap-1".to_owned(),
                    crate::providers::ApprovalChoice::Accept,
                    Some("looks fine".to_owned()),
                    cx,
                );
            });
        });
        vc.run_until_parked();
        let decides = handle.commands_of("decide-approval");
        // `ListPending` on open shares the approvals capability: only the
        // decision carries a choice.
        let decides: Vec<_> = decides
            .into_iter()
            .filter(|c| matches!(c, provider::Command::DecideApproval { .. }))
            .collect();
        assert_eq!(decides.len(), 1, "one DecideApproval leaves the lane, drew {decides:?}");
        match &decides[0] {
            provider::Command::DecideApproval { approval, choice, stage_token, feedback, .. } => {
                assert_eq!(approval, "ap-1");
                assert_eq!(choice, "accept");
                assert_eq!(stage_token.as_deref(), Some("st-1"), "the stage token echoes verbatim");
                assert_eq!(feedback.as_deref(), Some("looks fine"));
            }
            other => panic!("a press must travel as DecideApproval, travelled as {other:?}"),
        }
        vc.update(|_, cx| {
            let card = view.read(cx).external_approvals.get("ap-1").expect("the card waits");
            assert_eq!(card.decision_sent.as_deref(), Some("accept"), "sent, waiting — never settled on the press");
            assert!(
                view.update(cx, |view, cx| view.resolve_external_approval("ap-1", cx)),
                "the resolution settles what the press never may"
            );
            assert!(view.read(cx).external_approvals.get("ap-1").is_none());
        });
    }

    #[gpui::test]
    fn opening_the_lane_pulls_pending_approvals(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::with_pending(
            vec![provider::PendingApproval {
                id: "ap-9".to_owned(),
                session_id: "s-1".to_owned(),
                headline: "sudo make me a sandwich".to_owned(),
                stage_token: Some("st-9".to_owned()),
            }],
            Vec::new(),
        );
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        assert!(
            handle.received().iter().any(|c| matches!(c, provider::Command::ListPending { .. })),
            "open pulls ListPending so a missed approval still shows"
        );
        vc.update(|_, cx| {
            let card =
                view.read(cx).external_approvals.get("ap-9").expect("the pulled approval parks");
            assert_eq!(card.stage_token.as_deref(), Some("st-9"));
            assert!(card.decision_sent.is_none());
        });
    }

    #[gpui::test]
    fn questions_answer_dismiss_and_clarify_through_the_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: question_deltas(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.select_option("q-1".to_owned(), 1, cx));
            view.update(cx, |view, cx| view.answer_question("q-1".to_owned(), cx));
        });
        vc.run_until_parked();
        let answers = handle.question_commands();
        assert_eq!(answers.len(), 1, "one AnswerQuestion leaves the lane, drew {answers:?}");
        match &answers[0] {
            provider::Command::AnswerQuestion { question, answers, .. } => {
                assert_eq!(question, "q-1");
                assert_eq!(answers.len(), 1);
                assert_eq!(answers[0].selected_label.as_deref(), Some("safe"));
            }
            other => panic!("an answer must travel as AnswerQuestion, travelled as {other:?}"),
        }
        vc.update(|_, cx| view.update(cx, |view, cx| view.skip_question("q-1".to_owned(), cx)));
        vc.run_until_parked();
        assert!(
            handle
                .question_commands()
                .iter()
                .any(|c| matches!(c, provider::Command::DismissQuestion { .. })),
            "dismiss travels as DismissQuestion"
        );
        vc.update(|window, cx| {
            view.update(cx, |view, cx| view.clarify_question("q-1".to_owned(), &mut *window, cx));
            view.update(cx, |view, cx| {
                view.clarify.update(cx, |state, cx| state.set_value("use the safe one".to_owned(), window, cx));
            });
            view.update(cx, |view, cx| view.send_clarification("q-1", window, cx));
        });
        vc.run_until_parked();
        assert!(
            handle.question_commands().iter().any(
                |c| matches!(c, provider::Command::ClarifyQuestion { text, .. } if text == "use the safe one")
            ),
            "clarification travels as ClarifyQuestion with its text"
        );
    }

    #[gpui::test]
    fn unavailable_questions_refuse_with_the_registry_reason(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        // Claude Code asks in prose: the registry marks Questions
        // `Unavailable`, so even a stray question card banners instead of
        // sending a command the seam refuses.
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: question_deltas(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.select_option("q-1".to_owned(), 0, cx));
            view.update(cx, |view, cx| view.answer_question("q-1".to_owned(), cx));
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert!(handle.question_commands().is_empty(), "nothing leaves an Unavailable lane");
            assert!(
                view.banner.as_deref().is_some_and(|banner| banner.contains("prose")),
                "the refusal carries the registry's own reason, drew {:?}",
                view.banner
            );
        });
    }

    #[gpui::test]
    fn shell_escape_travels_as_run_shell(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| view.update(cx, |view, cx| view.run_user_shell("echo hi".to_owned(), cx)));
        vc.run_until_parked();
        let shells = handle.commands_of("run-shell");
        assert_eq!(shells.len(), 1, "one RunShell leaves the lane, drew {shells:?}");
        match &shells[0] {
            provider::Command::RunShell { command, .. } => assert_eq!(command, "echo hi"),
            other => panic!("a `!` command must travel as RunShell, travelled as {other:?}"),
        }
    }

    #[gpui::test]
    fn retry_resubmits_the_remembered_text_on_the_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("try me".to_owned(), cx)));
        vc.run_until_parked();
        drain_recording(&view, &tx, vc);
        let request_id = match handle.commands_of("submit-input").as_slice() {
            [provider::Command::SubmitInput { request_id, .. }] => request_id.clone(),
            other => panic!("one submit first, drew {other:?}"),
        };
        vc.update(|_, cx| view.update(cx, |view, cx| view.retry_turn(request_id, cx)));
        vc.run_until_parked();
        let resubmits = handle.commands_of("submit-input");
        assert_eq!(resubmits.len(), 2, "retry resubmits, drew {resubmits:?}");
        match &resubmits[1] {
            provider::Command::SubmitInput { display_text, .. } => {
                assert_eq!(display_text.as_deref(), Some("try me"));
            }
            other => panic!("a retry must travel as SubmitInput, travelled as {other:?}"),
        }
    }

    /// W4c: the composer's effort chip rides the next turn as a neutral
    /// `SubmitInput` effort — the level the adapters map onto their own
    /// channel — and Default omits it. A lane that dropped the field would
    /// send every turn at the provider default whatever the chip says.
    #[gpui::test]
    fn the_chips_effort_rides_submit_input(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.pick_effort(Some(aui_protocol::ReasoningEffort::High), cx)
            });
        });
        vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("push hard".to_owned(), cx)));
        vc.run_until_parked();
        let submits = handle.commands_of("submit-input");
        assert_eq!(submits.len(), 1, "one SubmitInput leaves the lane, drew {submits:?}");
        match &submits[0] {
            provider::Command::SubmitInput { effort, display_text, .. } => {
                assert_eq!(effort.as_deref(), Some("high"), "the chip's level rides the turn");
                assert_eq!(display_text.as_deref(), Some("push hard"));
            }
            other => panic!("a send must travel as SubmitInput, travelled as {other:?}"),
        }
    }

    #[gpui::test]
    fn reclaiming_a_queued_row_travels_as_reclaim_queued(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.unqueue("t-9", super::super::Unqueue::Remove, "queued words".to_owned(), cx)
            });
        });
        vc.run_until_parked();
        let received = handle.received();
        assert!(
            received.iter().any(|c| matches!(c, provider::Command::ReclaimQueued { turn, .. } if turn == "t-9")),
            "reclaim travels as ReclaimQueued, drew {received:?}"
        );
    }

    #[gpui::test]
    fn an_unsupported_refusal_shows_its_reason(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        // The scripted provider answers `SessionShell` as `Unavailable`,
        // so the seam refuses before any adapter code runs.
        let (view, _tx) = open_lane_view(vc, "s-1");
        vc.run_until_parked();
        vc.update(|_, cx| assert!(view.read(cx).banner.is_none(), "the ListPending pull stays quiet"));
        vc.update(|_, cx| view.update(cx, |view, cx| view.run_user_shell("echo hi".to_owned(), cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            assert!(
                view.read(cx).banner.as_deref().is_some_and(|banner| banner.contains("scripted providers only")),
                "the refusal's reason reaches the banner, drew {:?}",
                view.read(cx).banner
            );
        });
    }

    // ------------------------------------------------- W4: model, effort,
    // mode, meter, compact, fork

    /// W4: opening the lane lists the child catalog — the menu carries the
    /// child's rows, the chip reads the effective model's human name, and
    /// a pick sends `SelectModel` and moves the chip. The wire id never
    /// renders anywhere.
    #[gpui::test]
    fn lane_open_lists_models_and_the_pick_travels_as_select_model(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::with_catalog(vec![
            provider::ModelSummary { id: "gx-1".into(), label: "GX One".into(), active: true },
            provider::ModelSummary { id: "gx-2".into(), label: "GX Two".into(), active: false },
        ]);
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            let view = view.read(cx);
            let ids: Vec<&str> = view.models.iter().map(|m| m.model_id.as_str()).collect();
            assert_eq!(ids, ["gx-1", "gx-2"], "the menu lists what the child returned");
            assert_eq!(view.model().as_ref(), "GX One", "the chip reads the active row's human name");
            assert!(!view.model().as_ref().contains("codex"), "never the provider id");
        });
        vc.update(|_, cx| view.update(cx, |view, cx| view.pick_model("gx-2", cx)));
        vc.run_until_parked();
        let selects: Vec<_> = handle
            .received()
            .into_iter()
            .filter(|c| matches!(c, provider::Command::SelectModel { .. }))
            .collect();
        assert_eq!(selects.len(), 1, "one SelectModel leaves the lane, drew {selects:?}");
        match &selects[0] {
            provider::Command::SelectModel { session_id, model, .. } => {
                assert_eq!(session_id, "s-1");
                assert_eq!(model, "gx-2");
            }
            other => panic!("a pick must travel as SelectModel, travelled as {other:?}"),
        }
        vc.update(|_, cx| {
            assert_eq!(view.read(cx).model().as_ref(), "GX Two", "the chip follows the pick");
            assert!(
                view.read(cx).pending_model.as_deref() == Some("gx-2"),
                "recorded in pending_model until confirmed"
            );
        });
    }

    /// W4: a refused pick un-records itself and banners the reason — the
    /// chip never claims a model the child never took. The scripted
    /// provider answers `SessionConfig` as `Unavailable`, so the seam
    /// refuses before any adapter code runs.
    #[gpui::test]
    fn a_refused_model_pick_unrecords_and_banners(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, _tx) = open_lane_view(vc, "s-1");
        vc.run_until_parked();
        // The background `ListModels` pull stays quiet: the reason row
        // explains the catalog, never a banner.
        vc.update(|_, cx| assert!(view.read(cx).banner.is_none(), "the catalog pull stays quiet"));
        vc.update(|_, cx| view.update(cx, |view, cx| view.pick_model("gx-9", cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert!(view.pending_model.is_none(), "the unlanded pick is dropped");
            assert!(
                view.banner.as_deref().is_some_and(|banner| banner.contains("scripted providers only")),
                "the refusal's reason reaches the banner, drew {:?}",
                view.banner
            );
        });
    }

    /// W4: the lane meter reads the folded transcript's `TurnMeta` — real
    /// input/output totals after a turn, no window no percentage, and the
    /// strip stays silent where nothing is refused.
    #[gpui::test]
    fn the_lane_meter_reads_turn_meta_tokens(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, _handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            let meter = view.read(cx).context();
            assert_eq!(meter.used_tokens, 0, "no turns yet: the meter reads zero, honestly");
        });
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: vec![
                    aui_protocol::Delta::TurnStarted {
                        turn: aui_protocol::Turn::Assistant {
                            id: "a-1".to_owned(),
                            blocks: Vec::new(),
                            meta: aui_protocol::TurnMeta::default(),
                            timestamp: None,
                        },
                    },
                    aui_protocol::Delta::TurnFinished {
                        turn_id: "a-1".to_owned(),
                        meta: aui_protocol::TurnMeta {
                            tokens_in: 1200,
                            tokens_out: 300,
                            ..Default::default()
                        },
                    },
                ],
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            let meter = view.context();
            assert_eq!(meter.prompt_tokens, 1200, "input tokens out of TurnMeta");
            assert_eq!(meter.output_tokens, 300, "output tokens out of TurnMeta");
            assert_eq!(meter.total_tokens, 1500);
            assert_eq!(meter.used_tokens, 1500, "the footer meter shows real numbers");
            assert_eq!(meter.window_tokens, None, "no adapter reports a window");
            assert!(meter.label().contains("tokens"), "no window, no percentage: {}", meter.label());
            assert!(
                view.capability_strip_rows().is_empty(),
                "codex refuses nothing: no strip at all"
            );
        });
    }

    /// W5b: a reopened view shows replayed history, appends the next
    /// turn after it, and names the history's model on the chip. History
    /// arrives as one `Deltas` event — what a real `ResumeSession`
    /// replays before any live delta — and the next turn as another; the
    /// transcript keeps arrival order, and the chip reads the last
    /// finished turn's model until a pick or a live catalog row wins.
    #[gpui::test]
    fn a_reopened_view_shows_history_then_appends_and_names_its_model(
        cx: &mut gpui::TestAppContext,
    ) {
        use aui_protocol::{Block, Delta, Turn, TurnMeta};

        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, _handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        vc.update(|_, cx| {
            assert_eq!(
                view.read(cx).model().as_ref(),
                "Claude Code",
                "no model known yet: the chip names the provider"
            );
        });
        // History lands the way a real resume replays it: one event with
        // the prior turns, closed, carrying the session's model.
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: vec![
                    Delta::TurnStarted {
                        turn: Turn::User {
                            id: "u-old".to_owned(),
                            text: "Restore the header".to_owned(),
                            attachments: Vec::new(),
                            mentions: Vec::new(),
                            timestamp: None,
                        },
                    },
                    Delta::TurnStarted {
                        turn: Turn::Assistant {
                            id: "a-old".to_owned(),
                            blocks: Vec::new(),
                            meta: TurnMeta::default(),
                            timestamp: None,
                        },
                    },
                    Delta::BlockAdded {
                        turn_id: "a-old".to_owned(),
                        block: Block::Text {
                            text: "The header is restored".to_owned(),
                            streaming: false,
                        },
                    },
                    Delta::TurnFinished {
                        turn_id: "a-old".to_owned(),
                        meta: TurnMeta {
                            model: "hist-model".to_owned(),
                            ..Default::default()
                        },
                    },
                ],
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert_eq!(
                view.model().as_ref(),
                "hist-model",
                "the chip names the history's model, never the provider"
            );
            assert!(!view.busy(), "the replayed finish settles the view");
        });
        // The next turn arrives after history and appends after it.
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: vec![
                    Delta::TurnStarted {
                        turn: Turn::User {
                            id: "u-new".to_owned(),
                            text: "Now the footer".to_owned(),
                            attachments: Vec::new(),
                            mentions: Vec::new(),
                            timestamp: None,
                        },
                    },
                    Delta::TurnStarted {
                        turn: Turn::Assistant {
                            id: "a-new".to_owned(),
                            blocks: Vec::new(),
                            meta: TurnMeta::default(),
                            timestamp: None,
                        },
                    },
                    Delta::BlockAdded {
                        turn_id: "a-new".to_owned(),
                        block: Block::Text {
                            text: "The footer is done".to_owned(),
                            streaming: false,
                        },
                    },
                    Delta::TurnFinished {
                        turn_id: "a-new".to_owned(),
                        meta: TurnMeta {
                            model: "new-model".to_owned(),
                            ..Default::default()
                        },
                    },
                ],
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            let texts: Vec<String> = view
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .flat_map(|turn| turn.blocks())
                        .filter_map(|block| match block {
                            Block::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                texts,
                ["The header is restored", "The footer is done"],
                "history first, the new turn appended after it: {texts:?}"
            );
            assert_eq!(
                view.model().as_ref(),
                "new-model",
                "the chip follows the latest finished turn"
            );
        });
    }

    /// W4: the strip names only refusals. Claude Code's questions ask in
    /// prose — the one `Unavailable` row — while steering, interruption
    /// and everything else stay attempted and unadvertised.
    #[gpui::test]
    fn the_strip_lists_only_unavailable(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, _handle) = RecordingProvider::new();
        let (claude, _tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        vc.update(|_, cx| {
            let rows = claude.read(cx).capability_strip_rows();
            assert_eq!(rows.len(), 1, "one refusal on this lane, drew {rows:?}");
            assert_eq!(rows[0].0, "Answer questions");
            assert!(rows[0].1.contains("prose"), "the human reason, in plain words");
            assert!(
                rows.iter().all(|(_, reason)| !reason.contains("nobody has probed")),
                "no developer wording about unverified cells: {rows:?}"
            );
        });
        let (adapter, _handle) = RecordingProvider::new();
        let (muse, _tx) = open_recording_view(vc, "s-m", "muse", adapter);
        vc.update(|_, cx| {
            assert!(muse.read(cx).capability_strip_rows().is_empty(), "muse shows no strip");
        });
    }

    /// W4: compact and fork travel as their commands — compact with no cut,
    /// fork through the newest completed turn — and the fork's ack raises
    /// the event that opens the new lane view.
    #[gpui::test]
    fn compact_and_fork_travel_as_their_commands(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        // One finished turn for the fork to name.
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: vec![
                    aui_protocol::Delta::TurnStarted {
                        turn: aui_protocol::Turn::Assistant {
                            id: "a-7".to_owned(),
                            blocks: Vec::new(),
                            meta: aui_protocol::TurnMeta::default(),
                            timestamp: None,
                        },
                    },
                    aui_protocol::Delta::TurnFinished {
                        turn_id: "a-7".to_owned(),
                        meta: aui_protocol::TurnMeta::default(),
                    },
                ],
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| view.update(cx, |view, cx| view.compact(cx)));
        vc.run_until_parked();
        let compacts = handle.commands_of("compact-session");
        assert_eq!(compacts.len(), 1, "one CompactSession leaves the lane, drew {compacts:?}");
        // The fork's ack opens the new view: watch for its event.
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        vc.update(|_, cx| {
            let seen = seen.clone();
            let sub = cx.subscribe(&view, move |_, event: &super::super::SessionEvent, _| {
                if let super::super::SessionEvent::ForkedOnProvider { session_id, provider } = event {
                    assert_eq!(*provider, crate::providers::ProviderId::Codex, "the fork stays on its lane");
                    seen.borrow_mut().push(session_id.clone());
                }
            });
            std::mem::forget(sub);
        });
        vc.update(|_, cx| view.update(cx, |view, cx| view.fork(None, cx)));
        vc.run_until_parked();
        let forks = handle.commands_of("fork-session");
        assert_eq!(forks.len(), 1, "one ForkSession leaves the lane, drew {forks:?}");
        match &forks[0] {
            provider::Command::ForkSession { through_turn, .. } => {
                assert_eq!(through_turn.as_deref(), Some("a-7"), "through the newest completed turn");
            }
            other => panic!("a fork must travel as ForkSession, travelled as {other:?}"),
        }
        assert_eq!(
            seen.borrow().as_slice(),
            ["s-fork"],
            "the fork's ack raises the open-the-lane event"
        );
    }

    /// W4: a refused compact banners its reason. The scripted provider
    /// answers `CompactSession` as `Unavailable`, so the seam refuses
    /// before any adapter code runs — surfaced, never swallowed. (On
    /// Claude Code the same banner carries the no-compact-now reason.)
    #[gpui::test]
    fn a_refused_compact_banners_its_reason(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, _tx) = open_lane_view(vc, "s-1");
        vc.run_until_parked();
        vc.update(|_, cx| view.update(cx, |view, cx| view.compact(cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            assert!(
                view.read(cx).banner.as_deref().is_some_and(|banner| !banner.is_empty()),
                "the refusal's reason reaches the banner, drew {:?}",
                view.read(cx).banner
            );
        });
    }

    /// W4: the approval-mode pick travels as `SelectApprovalMode` with the
    /// provider's own closed mode set — and plan mode attempts the same
    /// switch, standing back down when the child refuses it.
    #[gpui::test]
    fn approval_mode_and_plan_travel_as_select_approval_mode(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| view.pick_mode(aui_protocol::PermissionMode::AllowAll, cx))
        });
        vc.run_until_parked();
        let modes: Vec<_> = handle
            .received()
            .into_iter()
            .filter(|c| matches!(c, provider::Command::SelectApprovalMode { .. }))
            .collect();
        assert_eq!(modes.len(), 1, "one SelectApprovalMode leaves the lane, drew {modes:?}");
        match &modes[0] {
            provider::Command::SelectApprovalMode { mode, .. } => {
                assert_eq!(*mode, aui_protocol::PermissionMode::AllowAll, "the provider's own mode, mapped one-to-one");
            }
            other => panic!("a mode pick must travel as SelectApprovalMode, travelled as {other:?}"),
        }
        // Plan mode attempts the same switch on a lane that accepts it.
        vc.update(|_, cx| view.update(cx, |view, cx| view.set_plan(true, cx)));
        vc.run_until_parked();
        let modes: Vec<_> = handle
            .received()
            .into_iter()
            .filter(|c| matches!(c, provider::Command::SelectApprovalMode { .. }))
            .collect();
        assert_eq!(modes.len(), 2, "plan mode attempts the DenyUnmatched switch, drew {modes:?}");
        vc.update(|_, cx| assert!(view.read(cx).plan_mode(), "the accepted switch holds the pill"));
    }

    /// W4: plan mode stands back down when the child refuses the switch —
    /// the pill never claims enforcement the child never took.
    #[gpui::test]
    fn plan_mode_stands_down_on_refusal(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (view, _tx) = open_lane_view(vc, "s-1");
        vc.run_until_parked();
        vc.update(|_, cx| view.update(cx, |view, cx| view.set_plan(true, cx)));
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            assert!(!view.plan_mode(), "the refused switch stands the pill back down");
            assert!(
                view.banner.as_deref().is_some_and(|banner| banner.contains("scripted providers only")),
                "the refusal's reason reaches the banner, drew {:?}",
                view.banner
            );
        });
    }

    #[gpui::test]
    fn the_tier_banner_never_lands_on_a_provider_lane(cx: &mut gpui::TestAppContext) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, _tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.set_tier_banner(
                    Some(super::super::TierBanner {
                        text: "This login is on pay-as-you-go.".to_owned(),
                        blocking: true,
                        checking: false,
                    }),
                    cx,
                );
            });
        });
        vc.update(|_, cx| {
            assert!(view.read(cx).tier_banner.is_none(), "a muse-account banner never parks on a provider session");
            view.update(cx, |view, cx| {
                assert!(view.render_tier_banner(cx).is_none(), "and never draws there either");
            });
        });
        // A blocking banner would gate the send on a muse lane: here the
        // turn still leaves.
        vc.update(|_, cx| view.update(cx, |view, cx| view.send_text("still sends".to_owned(), cx)));
        vc.run_until_parked();
        assert_eq!(
            handle.commands_of("submit-input").len(),
            1,
            "the send is never gated on a tier the lane has no account behind"
        );
    }

    // ------------------------------------------------- W7b: one approval card

    /// One Write approval card in `state`, the way an adapter folds its
    /// ask: the path face with allow/deny choices.
    fn write_card(state: aui_protocol::ApprovalState) -> aui_protocol::Block {
        use aui_protocol::{
            ApprovalBadges, ApprovalChoice, ApprovalDecision, ApprovalScope, Block,
        };
        Block::Approval {
            id: "ap-1".to_owned(),
            tool: "Write".to_owned(),
            command: "/tmp/probe/note.txt".to_owned(),
            reason: "/tmp/probe/note.txt\nhello".to_owned(),
            cwd: "/tmp/probe".to_owned(),
            capabilities: Vec::new(),
            scope: ApprovalScope::ThisCommand,
            state,
            rule: None,
            choices: vec![
                ApprovalChoice {
                    id: "accept".into(),
                    label: "Allow once".into(),
                    decision: ApprovalDecision::Once,
                    scope: ApprovalScope::ThisCommand,
                    rule_preview: None,
                    accepts_feedback: false,
                },
                ApprovalChoice {
                    id: "decline".into(),
                    label: "Deny".into(),
                    decision: ApprovalDecision::Deny,
                    scope: ApprovalScope::ThisCommand,
                    rule_preview: None,
                    accepts_feedback: false,
                },
            ],
            stages: Vec::new(),
            current_stage: None,
            badges: ApprovalBadges::default(),
            feedback: None,
            resolved_by: None,
        }
    }

    /// One pending Write approval as deltas, the way an adapter folds its
    /// ask: an open assistant turn plus the pending card.
    fn pending_write_deltas() -> Vec<aui_protocol::Delta> {
        use aui_protocol::{ApprovalState, Delta, Turn, TurnMeta};
        vec![
            Delta::TurnStarted {
                turn: Turn::Assistant {
                    id: "a-1".to_owned(),
                    blocks: Vec::new(),
                    meta: TurnMeta::default(),
                    timestamp: None,
                },
            },
            Delta::BlockAdded {
                turn_id: "a-1".to_owned(),
                block: write_card(ApprovalState::Pending),
            },
        ]
    }

    fn inline_approvals(view: &Entity<SessionView>, vc: &mut gpui::VisualTestContext) -> Vec<ApprovalState> {
        vc.update(|_, cx| {
            view.read(cx)
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .flat_map(|turn| turn.blocks())
                        .filter_map(|block| match block {
                            Block::Approval { state, .. } => Some(state.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    /// W7b: one ask is one surface on a provider lane. The tap parks (it
    /// routes the decision) and the fold holds the inline card — but the
    /// strip above the composer stays empty, so the ask never draws twice.
    /// Drop the lane gate and the strip fills.
    #[gpui::test]
    fn provider_lane_shows_one_approval_surface(cx: &mut gpui::TestAppContext) {
        use aui_protocol::ApprovalState;
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, _handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "codex", adapter);
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: pending_write_deltas(),
            })
            .expect("the lane channel is open");
            tx.unbounded_send(provider::ProviderEvent::ApprovalRequested {
                session_id: "s-1".to_owned(),
                approval_id: "ap-1".to_owned(),
                headline: "/tmp/probe/note.txt".to_owned(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        assert_eq!(
            inline_approvals(&view, vc),
            [ApprovalState::Pending],
            "the fold holds the one inline card"
        );
        vc.update(|_, cx| {
            assert!(
                view.read(cx).external_strip().is_empty(),
                "the strip stays empty on a provider lane: the inline card is the surface"
            );
            // The row still stands on the waiting ask, as muse rows do.
            assert_eq!(
                view.read(cx).row_pending(),
                (Some("/tmp/probe/note.txt".to_owned()), None),
                "the fold's pending card feeds the needs-you row"
            );
            assert_eq!(view.read(cx).waiting_on_you(), Some((1, 0)));
        });
    }

    /// W7b: the inline card decides through the lane with the card's own
    /// choice id, and the press settles nothing — only the server's
    /// resolution moves the card, per the no-optimism rule.
    #[gpui::test]
    fn inline_card_decide_settles_only_on_resolution(cx: &mut gpui::TestAppContext) {
        use aui_protocol::{ApprovalState, Delta};
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: pending_write_deltas(),
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.decide_approval("ap-1".to_owned(), "accept".to_owned(), None, cx)
            });
        });
        vc.run_until_parked();
        let decides: Vec<_> = handle
            .commands_of("decide-approval")
            .into_iter()
            .filter(|c| matches!(c, provider::Command::DecideApproval { .. }))
            .collect();
        assert_eq!(decides.len(), 1, "one DecideApproval leaves the lane, drew {decides:?}");
        match &decides[0] {
            provider::Command::DecideApproval { approval, choice, .. } => {
                assert_eq!(approval, "ap-1");
                assert_eq!(choice, "accept", "the card's own choice id travels verbatim");
            }
            other => panic!("a press must travel as DecideApproval, travelled as {other:?}"),
        }
        assert_eq!(
            inline_approvals(&view, vc),
            [ApprovalState::Pending],
            "the press settles nothing: the card waits for the server"
        );
        // The server's resolution settles the card the press never may.
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas: vec![Delta::BlockUpdated {
                    turn_id: "a-1".to_owned(),
                    block_index: 0,
                    block: write_card(ApprovalState::AllowedOnce { exit_code: 0, duration_ms: 0 }),
                }],
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        assert!(
            inline_approvals(&view, vc)
                .iter()
                .any(|state| matches!(state, ApprovalState::AllowedOnce { .. })),
            "the resolution settles the card to allowed"
        );
    }

    /// W8: a folded multi-message Claude Code turn settles the view once.
    /// The whole `edit.jsonl` fold (Write, Edit, DONE — one user bubble,
    /// one assistant turn, one finish) arrives as lane events: the
    /// transcript holds exactly one assistant turn, the view is not busy
    /// (the stop button drops, no `Working` left behind), and the meter
    /// counts the turn's totals once. Folding one turn per message held
    /// three assistant turns here and tripled the meter.
    #[gpui::test]
    fn a_folded_claude_turn_settles_the_view_with_single_totals(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
        let vc = cx.add_empty_window();
        let (adapter, _handle) = RecordingProvider::new();
        let (view, tx) = open_recording_view(vc, "s-1", "claude-code", adapter);
        let text = std::fs::read_to_string(format!(
            "{}/../../fixtures/claude-code/edit.jsonl",
            env!("CARGO_MANIFEST_DIR")
        ))
        .expect("fixture reads");
        let mut fold = provider_claude_code::fold::ClaudeFold::new();
        let mut deltas = Vec::new();
        for line in text.lines() {
            let frame = provider_claude_code::frame::decode_line(line).expect("decodes");
            deltas.extend(fold.apply(&frame));
        }
        // The result frame's own totals, read off the raw JSON.
        let (want_in, want_out) = text
            .lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .find(|value| {
                value.get("type").and_then(serde_json::Value::as_str) == Some("result")
            })
            .map(|value| {
                let usage = value.get("usage").cloned().unwrap_or(serde_json::Value::Null);
                let uint =
                    |key: &str| usage.get(key).and_then(serde_json::Value::as_u64).unwrap_or(0);
                (
                    uint("input_tokens")
                        + uint("cache_read_input_tokens")
                        + uint("cache_creation_input_tokens"),
                    uint("output_tokens"),
                )
            })
            .expect("the fixture carries a result frame");
        vc.update(|_, _| {
            tx.unbounded_send(provider::ProviderEvent::Deltas {
                session_id: Some("s-1".to_owned()),
                deltas,
            })
            .expect("the lane channel is open");
        });
        vc.run_until_parked();
        vc.update(|_, cx| {
            let view = view.read(cx);
            let assistants = view
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .filter(|turn| {
                            matches!(turn, aui_protocol::Turn::Assistant { .. })
                        })
                        .count()
                })
                .unwrap_or(0);
            assert_eq!(assistants, 1, "one user turn is one assistant turn");
            assert!(!view.busy(), "the result settles the running state");
            let meter = view.context();
            assert_eq!(meter.prompt_tokens, want_in, "prompt tokens counted once");
            assert_eq!(meter.output_tokens, want_out, "output tokens counted once");
            assert_eq!(meter.total_tokens, want_in + want_out);
        });
    }
}
