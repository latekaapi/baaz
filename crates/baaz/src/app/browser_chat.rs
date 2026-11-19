//! Browser chats (docs/27 §6, R3): a Baaz session of kind `browser`, started
//! in the hidden browser workspace on Settings › Browser › Assistant's
//! provider and model, held by the window beside the centre's session.
//!
//! The chat's view is never the centre's: it lives in
//! [`Harness::browser_chat`] (set only through
//! [`Harness::set_browser_chat`], which subscribes it), which
//! [`Harness::find_view`] also searches, so
//! the provider open lands on it ([`Harness::land_provider_draft`]) exactly as
//! it lands on a parked draft. Its record is hidden from the Sessions
//! sidebar; the line, the card and its task folder reach it.

use gpui::{AppContext as _, Context, Window};

use super::Harness;
use crate::browser::assistant::{self, ModelChoice};
use crate::session::{SessionHost, SessionView};

impl Harness {
    /// The provider and model a new browser chat starts with, or `None`
    /// when no provider can start a session.
    pub(crate) fn assistant_choice(&self, cx: &gpui::App) -> Option<ModelChoice> {
        assistant::current_choice(cx)
    }

    /// The browser chat's session id, if one is open.
    pub(crate) fn browser_chat_id(&self, cx: &gpui::App) -> Option<String> {
        self.registry.browser_chat().map(|view| view.read(cx).rt(cx).session_id.clone())
    }

    /// Start a browser chat and send `ask` as its first turn: the view is
    /// drawn at once (the card shows it opening), the provider's child
    /// answers in the background, and the queued send goes out when it
    /// lands. Returns the provisional id, or `None` when no provider can
    /// start (the caller says so on the line).
    pub(crate) fn start_browser_chat(&mut self, ask: String, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let choice = self.assistant_choice(cx)?;
        let workspace = assistant::browser_workspace_dir().to_string_lossy().into_owned();
        let request_id = provider::new_request_id();
        self.register_terminal_session(&request_id, std::path::PathBuf::from(workspace.clone()), choice.provider.as_str(), cx);
        // Hidden before the open runs, as side sessions are: a crash between
        // the open and the record still keeps it out of the sidebar.
        self.set_override(
            &request_id,
            |meta| {
                meta.hidden = true;
                meta.browser_chat = true;
            },
            cx,
        );
        let mode = self.provider_open_mode(choice.provider, Some(&workspace), None, cx);
        let host = SessionHost {
            provider_id: choice.provider.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host(cx)),
        };
        let view = cx.new(|cx| SessionView::new_provider_opening(request_id.clone(), mode, host, window, cx));
        if !choice.model.is_empty() {
            view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.set_model(choice.model, cx)));
        }
        view.update(cx, |view, cx| view.runtime().clone().update(cx, |rt, cx| rt.send_or_queue(ask, cx)));
        self.set_browser_chat(view, cx);
        self.open_state.starts_in_flight.insert(request_id.clone());
        self.spawn_provider_open(choice.provider, None, workspace, request_id.clone(), mode, cx);
        cx.notify();
        Some(request_id)
    }

    /// Point the line and the card at an existing browser chat, `session`
    /// (its task folder's Open chat): the view must still be live.
    pub(crate) fn show_browser_chat(&mut self, session: &str, cx: &mut Context<Self>) -> bool {
        match self.find_view(session, cx) {
            Some(view) => {
                self.set_browser_chat(view, cx);
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// Make `view` the chat on the line and subscribe it (docs/81 A F5).
    /// One chat at a time on the line; the one it replaces keeps its record
    /// and its task folder, and — while it has work in hand — its view, in
    /// the parked MRU, so its child is not killed mid-turn (F7). An idle
    /// replaced chat parks too and is capped like any idle view.
    pub(crate) fn set_browser_chat(&mut self, view: gpui::Entity<SessionView>, cx: &mut Context<Self>) {
        // Shown again from its folder: it leaves the MRU for the line.
        self.registry.forget_view(&view);
        if let Some(old) = self.registry.replace_browser_chat(view.clone()).filter(|old| *old != view) {
            if old.read(cx).rt(cx).must_stay_alive() && self.active.as_ref() != Some(&old) {
                let id = old.read(cx).rt(cx).session_id.clone();
                self.registry.park(id, old, cx);
            }
        }
        self.resubscribe_live_views(cx);
        // Its turn and its asks move the agent line over the page.
        self.watch_browser_chat(cx);
    }

    /// Whether `session` is a browser chat (by its record).
    pub(crate) fn is_browser_chat(&self, session: &str, cx: &gpui::App) -> bool {
        self.session_list.read(cx).overrides.get(session).is_some_and(|meta| meta.browser_chat)
    }
}
