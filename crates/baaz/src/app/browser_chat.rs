//! Browser chats (docs/27 §6, R3): a Baaz session of kind `browser`, started
//! in the hidden browser workspace on Settings › Browser › Assistant's
//! provider and model, held by the window beside the centre's session.
//!
//! The chat's view is never the centre's: it lives in
//! [`Harness::browser_chat`], which [`Harness::find_view`] also searches, so
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
    pub(crate) fn assistant_choice(&self) -> Option<ModelChoice> {
        let statuses = crate::provider_status::live_statuses();
        let default = crate::settings_providers::default_new_provider(&statuses);
        assistant::resolve(&self.layout.assistant, |p| crate::settings_providers::usable_provider(&statuses, p), default)
    }

    /// The browser chat's session id, if one is open.
    pub(crate) fn browser_chat_id(&self, cx: &gpui::App) -> Option<String> {
        self.browser_chat.as_ref().map(|view| view.read(cx).session_id.clone())
    }

    /// Start a browser chat and send `ask` as its first turn: the view is
    /// drawn at once (the card shows it opening), the provider's child
    /// answers in the background, and the queued send goes out when it
    /// lands. Returns the provisional id, or `None` when no provider can
    /// start (the caller says so on the line).
    pub(crate) fn start_browser_chat(&mut self, ask: String, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let choice = self.assistant_choice()?;
        let workspace = assistant::browser_workspace_dir().to_string_lossy().into_owned();
        let request_id = provider::new_request_id();
        self.register_terminal_session(&request_id, std::path::PathBuf::from(workspace.clone()), choice.provider.as_str());
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
        let mode = self.provider_open_mode(Some(&workspace), None);
        let host = SessionHost {
            provider_id: choice.provider.as_str().to_owned(),
            workspace: workspace.clone(),
            overlays: self.overlays.clone(),
            capture: self.capture.clone(),
            terminal_host: Some(self.terminal_host.clone()),
        };
        let view = cx.new(|cx| SessionView::new_provider_opening(request_id.clone(), mode, host, window, cx));
        if !choice.model.is_empty() {
            view.update(cx, |view, cx| view.set_model(choice.model, cx));
        }
        view.update(cx, |view, cx| view.send_or_queue(ask, cx));
        // One chat at a time on the line; the one it replaces keeps its
        // record and its task folder.
        self.browser_chat = Some(view);
        self.starts_in_flight.insert(request_id.clone());
        self.spawn_provider_open(choice.provider, None, workspace, request_id.clone(), mode, cx);
        cx.notify();
        Some(request_id)
    }

    /// Point the line and the card at an existing browser chat, `session`
    /// (its task folder's Open chat): the view must still be live.
    pub(crate) fn show_browser_chat(&mut self, session: &str, cx: &mut Context<Self>) -> bool {
        match self.find_view(session, cx) {
            Some(view) => {
                self.browser_chat = Some(view);
                cx.notify();
                true
            }
            None => false,
        }
    }

    /// Whether `session` is a browser chat (by its record).
    pub(crate) fn is_browser_chat(&self, session: &str) -> bool {
        self.overrides.get(session).is_some_and(|meta| meta.browser_chat)
    }
}
