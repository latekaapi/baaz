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

    /// Look one up, for the question surface.
    #[cfg(test)]
    fn get(&self, id: &str) -> Option<&ExternalQuestion> {
        self.questions.get(id)
    }
}

impl SessionView {
    /// Whether this view rides the provider lane. The muse entry points
    /// (`apply`, `seed_session`, `load_replay`, `reconnected`) refuse on it.
    pub(super) fn is_provider_lane(&self) -> bool {
        matches!(self.lane, Lane::Provider(_))
    }

    /// A view over a provider session. Mirrors [`SessionView::new`] minus
    /// the child: the caller hands over an already-connected
    /// [`provider::Provider`] and its bridged event stream, and this spawns
    /// exactly one gpui task draining that stream for the view's whole life.
    ///
    /// W2 constructs the real adapters; until then only tests call this.
    #[allow(dead_code)]
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
        this.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |view, cx| view.on_provider_event(event, cx)).is_err() {
                    return;
                }
            }
        }));
        this
    }

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
            assert_eq!(view.read(cx).tasks.len(), 1, "the lane spawns exactly one drain task");
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
}
