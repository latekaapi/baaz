//! The application entity: the connection, auth, the session list and the shell.
//!
//! # Thread model
//!
//! One [`MuseClient`] per process, owned here behind an `Arc`. It already runs
//! its own reader and writer threads, so the pipe never touches the UI thread.
//! Two directions cross the boundary:
//!
//! * **Events in.** [`crate::conn::connect`] hands back a `futures` receiver fed
//!   by one bridging thread. A single foreground task drains it and calls
//!   [`SessionView::apply`], so folding happens on the UI thread in wire order
//!   and a frame always renders a consistent transcript.
//! * **Commands out.** Every request blocks, so every one of them runs on
//!   `background_spawn` and returns through `update`. The UI thread issues
//!   intents and never waits.
//!
//! # Screens
//!
//! Two, and the auth probe decides which: the device-code login screen (spec
//! §3.2) or the shell. The right pane's slot exists and stays empty in this
//! phase; `ToggleRightPane` is bound and does nothing, so the keymap does not
//! grow a hole later.

use std::collections::HashMap;
use std::sync::Arc;

use aui::data::button;
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, FocusNext, FocusPrev, ToggleRightPane, ToggleSidebar};
use aui::nav::{sidebar_footer, sidebar_view};
use aui::overlay::{dialog, popover_layer, DialogKind};
use aui::screens::{login, LoginIntent, LoginState};
use aui::shell::{app_shell, centre_header, right_header, sidebar_header};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, KeyBinding,
    SharedString, Subscription, Task, Window,
};
use gpui_kit::base::v_flex;
use muse_client::schema::{
    ModelCatalogSource, ModelListParams, SessionListParams, SessionResumeParams, SessionStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::auth::{self, Identity, LoginEvent};
use crate::conn::{self, Severity};
use crate::index::{self, IndexEntry};
use crate::session::{SessionEvent, SessionView};
use crate::sidebar::{self, SessionEntry};
use crate::Args;

actions!(
    harness,
    [
        /// Send the composer's draft (Enter).
        SendTurn,
        /// Stop the running turn and retract its prompt (⌃C).
        Interrupt,
        /// Start a new session in this workspace (⌘N).
        NewSession,
    ]
);

/// The context the composer holder wears, so Enter reaches [`SendTurn`] instead
/// of the textarea. Shift+Enter matches no binding and falls through to the
/// editor as a newline, which is exactly the behaviour §3.9 asks for.
const COMPOSER_CONTEXT: &str = "HarnessComposer";

/// Binds the harness's own keys on top of the library's.
///
/// `aui::init` has already bound ⌘B, ⌘K, ⌘\\, Escape, Tab and the approval
/// triad; these are the ones only this app knows about.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", SendTurn, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("ctrl-c", Interrupt, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-n", NewSession, Some(aui::keys::ROOT_CONTEXT)),
    ]);
}

/// Where the boot probe got to (spec §3.2).
enum Auth {
    /// Reading `auth.json` and asking `model/list` where its catalog came from.
    Probing,
    /// Either half of the probe failed: the login screen.
    SignedOut,
    /// Both halves passed.
    SignedIn(Identity),
}

/// The connection's own state, which is what the reconnect banner reads.
enum Wire {
    /// Spawning `muse serve` and shaking hands.
    Connecting,
    /// Live.
    Ready,
    /// The child exited; respawning and resuming (docs/01-transport.md §3).
    Reconnecting,
    /// The respawn failed. The dialog offers another try.
    Down(String),
}

/// One modal on screen. Only one at a time in this phase.
struct Dialog {
    title: String,
    detail: String,
    kind: DialogKind,
    primary: &'static str,
    action: DialogAction,
}

/// What a dialog's primary button does.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DialogAction {
    /// Close it and carry on.
    Dismiss,
    /// Respawn `muse serve` and resume.
    Reconnect,
    /// Go to the login screen.
    SignIn,
}

/// The login screen's own state, plus the child that drives it.
struct Login {
    state: LoginState,
    url: Option<String>,
    code: Option<String>,
    expires: Option<String>,
    task: Option<Task<()>>,
}

impl Default for Login {
    fn default() -> Self {
        Self { state: LoginState::Idle, url: None, code: None, expires: None, task: None }
    }
}

/// The whole application.
pub struct Harness {
    args: Args,
    client: Option<Arc<MuseClient>>,
    wire: Wire,
    auth: Auth,
    login: Login,
    /// Rows from `session/list`, joined with the local index.
    sessions: Vec<SessionEntry>,
    index: HashMap<String, IndexEntry>,
    active: Option<Entity<SessionView>>,
    dialog: Option<Dialog>,
    sidebar_open: bool,
    /// The right pane's slot exists; nothing opens it in this phase.
    right_open: bool,
    focus_root: FocusHandle,
    focus_dialog: FocusHandle,
    /// Set when the next frame should move the keyboard to the composer.
    focus_composer: bool,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl Harness {
    /// Boot: read `auth.json`, then connect and finish the probe.
    pub fn new(args: Args, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            args,
            client: None,
            wire: Wire::Connecting,
            auth: Auth::Probing,
            login: Login::default(),
            sessions: Vec::new(),
            index: HashMap::new(),
            active: None,
            dialog: None,
            sidebar_open: true,
            right_open: false,
            focus_root: cx.focus_handle(),
            focus_dialog: cx.focus_handle(),
            focus_composer: true,
            tasks: Vec::new(),
            subscriptions: Vec::new(),
        };
        if this.args.offline {
            // `--no-connect`: the chrome without a child, for a screenshot of
            // the login screen with sample data.
            this.auth = Auth::SignedOut;
            this.wire = Wire::Down("not connected".into());
            return this;
        }
        this.connect(cx);
        this.load_index(cx);
        this
    }

    /// The workspace path as the wire and the header want it.
    fn workspace(&self) -> String {
        self.args.workspace.to_string_lossy().into_owned()
    }

    /// The workspace's last path component, which is what the header shows.
    fn workspace_name(&self) -> String {
        self.args
            .workspace
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.workspace())
    }

    // ------------------------------------------------------------ connection

    /// Spawn `muse serve`, initialize, and start draining its events.
    fn connect(&mut self, cx: &mut Context<Self>) {
        let program = self.args.program.clone();
        let call = cx.background_spawn(async move { conn::connect(&program) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((connection, events)) => {
                    let server = &connection.server.server_info;
                    eprintln!("harness: connected to {} {}", server.name, server.version);
                    if let Some(warning) = &connection.warning {
                        // A fingerprint mismatch is additive evolution, never a
                        // failure: say so on stderr and carry on.
                        eprintln!("harness: {warning:?}");
                    }
                    this.client = Some(connection.client);
                    this.wire = Wire::Ready;
                    this.pump(events, cx);
                    this.probe_catalog(cx);
                    cx.notify();
                }
                Err(error) => {
                    this.wire = Wire::Down(error.to_string());
                    this.dialog = Some(Dialog {
                        title: "Muse could not be started".into(),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Try again",
                        action: DialogAction::Reconnect,
                    });
                    cx.notify();
                }
            });
        }));
    }

    /// Drain the bridge onto the UI thread, one event at a time, in wire order.
    fn pump(&mut self, mut events: UnboundedReceiver<MuseEvent>, cx: &mut Context<Self>) {
        self.tasks.push(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                let closed = matches!(event, MuseEvent::Closed(_));
                if this.update(cx, |this, cx| this.route(event, cx)).is_err() {
                    return;
                }
                if closed {
                    return;
                }
            }
        }));
    }

    /// Hand an event to the session it belongs to, and notice a dead child.
    fn route(&mut self, event: MuseEvent, cx: &mut Context<Self>) {
        if let MuseEvent::Closed(code) = &event {
            eprintln!("harness: muse serve exited ({code:?}); reconnecting");
            self.wire = Wire::Reconnecting;
            self.reconnect(cx);
        }
        // A finished turn is when the index has something new to say about the
        // session, so the sidebar is refreshed then rather than on a timer.
        let completed = matches!(&event, MuseEvent::Notification { method, .. } if method == "turn/completed");
        if let Some(active) = &self.active {
            active.update(cx, |view, cx| view.apply(event, cx));
        }
        if completed {
            self.load_index(cx);
            self.load_sessions(cx);
        }
        cx.notify();
    }

    /// The reconnect procedure: respawn, `initialize`, then `session/resume`
    /// from the last observed cursor, which serves `history.mode: "none"` and
    /// streams only the suffix.
    fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        let program = self.args.program.clone();
        let resume = self
            .active
            .as_ref()
            .map(|a| (a.read(cx).session_id.clone(), a.read(cx).last_cursor()));
        let call = cx.background_spawn(async move {
            let connected = conn::connect(&program)?;
            if let Some((session_id, cursor)) = &resume {
                connected.0.client.session_resume(&SessionResumeParams {
                    command_id: new_command_id(),
                    session_id: session_id.clone(),
                    cursor: cursor.clone(),
                    exclude_items: Some(true),
                    history: None,
                })?;
            }
            Ok::<_, MuseError>(connected)
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((connection, events)) => {
                    this.client = Some(connection.client.clone());
                    this.wire = Wire::Ready;
                    if let Some(active) = &this.active {
                        active.update(cx, |view, _| view.reconnected(connection.client.clone()));
                    }
                    this.pump(events, cx);
                    cx.notify();
                }
                Err(error) => {
                    this.wire = Wire::Down(error.to_string());
                    this.dialog = Some(Dialog {
                        title: conn::title(&error),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Reconnect",
                        action: DialogAction::Reconnect,
                    });
                    cx.notify();
                }
            });
        }));
    }

    // ------------------------------------------------------------------ auth

    /// The second half of the boot probe: `model/list` reporting
    /// `source: "providerCatalog"` means the catalog was fetched with a live
    /// credential.
    fn probe_catalog(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let stored = auth::identity();
        let call = cx.background_spawn(async move { client.model_list(&ModelListParams { session_id: None }) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let live = matches!(call.await, Ok(result) if result.source == ModelCatalogSource::ProviderCatalog);
            let _ = this.update(cx, |this, cx| {
                this.auth = match (stored, live) {
                    (Some(identity), true) => Auth::SignedIn(identity),
                    _ => Auth::SignedOut,
                };
                if matches!(this.auth, Auth::SignedIn(_)) {
                    this.load_sessions(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Start `muse login` and follow its stderr.
    fn start_login(&mut self, cx: &mut Context<Self>) {
        self.login = Login { state: LoginState::Starting, ..Login::default() };
        cx.notify();
        let program = self.args.program.clone();
        let events = match auth::spawn_login(&program) {
            Ok(events) => events,
            Err(error) => {
                self.login.state = LoginState::Error { message: error.to_string().into() };
                cx.notify();
                return;
            }
        };
        self.login.task = Some(cx.spawn(async move |this, cx| loop {
            let events = events.clone();
            let next = cx.background_executor().spawn(async move { events.recv().ok() }).await;
            let Some(event) = next else { return };
            if this.update(cx, |this, cx| this.on_login(event, cx)).is_err() {
                return;
            }
        }));
    }

    /// One line from the login child. The URL and the code go straight to the
    /// screen and are never logged.
    fn on_login(&mut self, event: LoginEvent, cx: &mut Context<Self>) {
        match event {
            LoginEvent::Url(url) => self.login.url = Some(url),
            LoginEvent::Code(code) => self.login.code = Some(code),
            LoginEvent::Waiting { expires } => self.login.expires = expires,
            LoginEvent::Success => {
                self.login.state = LoginState::Success;
                // The credential is ambient: `muse serve` picked it up at spawn,
                // so the connection has to be made again before it can use it.
                self.login.task = None;
                self.wire = Wire::Reconnecting;
                self.auth = Auth::Probing;
                self.reconnect_after_login(cx);
                cx.notify();
                return;
            }
            LoginEvent::Failed(message) => {
                self.login.state = LoginState::Error { message: message.into() };
                cx.notify();
                return;
            }
        }
        if let (Some(url), Some(code)) = (&self.login.url, &self.login.code) {
            self.login.state = LoginState::Device {
                url: url.clone().into(),
                code: code.clone().into(),
                expires: self.login.expires.clone().map(SharedString::from),
                waiting: true,
            };
        }
        cx.notify();
    }

    /// After a successful login: drop the child that inherited no credential,
    /// spawn a fresh one and re-probe.
    fn reconnect_after_login(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        self.active = None;
        let program = self.args.program.clone();
        let call = cx.background_spawn(async move { conn::connect(&program) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok((connection, events)) => {
                    this.client = Some(connection.client);
                    this.wire = Wire::Ready;
                    this.login = Login::default();
                    this.pump(events, cx);
                    this.probe_catalog(cx);
                    cx.notify();
                }
                Err(error) => {
                    this.wire = Wire::Down(error.to_string());
                    this.login.state = LoginState::Error { message: error.to_string().into() };
                    this.auth = Auth::SignedOut;
                    cx.notify();
                }
            });
        }));
    }

    /// `muse logout`, then straight back to the login screen.
    fn logout(&mut self, cx: &mut Context<Self>) {
        let program = self.args.program.clone();
        let call = cx.background_spawn(async move { auth::logout(&program) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.active = None;
                this.sessions.clear();
                this.auth = Auth::SignedOut;
                this.login = Login::default();
                if let Err(message) = result {
                    this.login.state = LoginState::Error { message: message.into() };
                }
                cx.notify();
            });
        }));
    }

    // -------------------------------------------------------------- sessions

    /// Read the local index once at boot; it is a cache, not a source of truth.
    fn load_index(&mut self, cx: &mut Context<Self>) {
        let call = cx.background_spawn(async move { index::read() });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let index = call.await;
            let _ = this.update(cx, |this, cx| {
                this.index = index;
                this.rejoin();
                cx.notify();
            });
        }));
    }

    /// `session/list`, filtered to this window's workspace.
    fn load_sessions(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let workspace = self.workspace();
        let call = cx.background_spawn(async move {
            client.session_list(&SessionListParams {
                workspace_root: Some(workspace),
                ..Default::default()
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| {
                if let Ok(list) = result {
                    this.sessions = list
                        .sessions
                        .iter()
                        .map(|s| SessionEntry::join(s, this.index.get(&s.session_id)))
                        .collect();
                }
                this.open_boot_session(window, cx);
                cx.notify();
            });
        }));
    }

    /// `--session <id>` (or `latest`): open one session at boot, once the list
    /// has arrived. Consumed, so a later refresh does not re-open it.
    fn open_boot_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(wanted) = self.args.session.take() else {
            // A scripted turn with no session named needs somewhere to go.
            if self.args.send.is_some() && self.active.is_none() {
                self.new_session(cx);
            }
            return;
        };
        let id = if wanted == "latest" {
            // The list arrives `updatedAt` descending, and the sidebar sorts on
            // the same field, so the newest row is the head of the grouping.
            self.sessions.iter().max_by_key(|e| e.updated).map(|e| e.id.clone())
        } else {
            Some(wanted)
        };
        if let Some(id) = id {
            self.resume(id, window, cx);
        }
    }

    /// `--send <text>`: one scripted turn, once a session is open. The hook a
    /// screenshot of a live turn needs; it goes through the same `send` a key
    /// press does, never straight into the fold.
    fn send_scripted(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.args.send.take() else { return };
        let Some(view) = self.active.clone() else { return };
        view.update(cx, |view, cx| {
            view.set_draft(text, window, cx);
            view.send(window, cx);
        });
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed).
    fn rejoin(&mut self) {
        for entry in &mut self.sessions {
            if let Some(label) = self.index.get(&entry.id).and_then(IndexEntry::label) {
                entry.label = label.to_owned();
            }
        }
    }

    /// `session/start` in this workspace, on the configured provider.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let (workspace, provider) = (self.workspace(), self.args.provider.clone());
        let call = cx.background_spawn(async move {
            client.session_start(&SessionStartParams {
                command_id: new_command_id(),
                workspace_root: Some(workspace),
                provider_id: Some(provider),
                ..Default::default()
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(started) => {
                    this.open(started.session.session_id.clone(), false, window, cx);
                    this.load_sessions(cx);
                }
                Err(error) => this.report(&error, cx),
            });
        }));
    }

    /// `session/resume`, then page the whole transcript in.
    fn resume(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.open(session_id.clone(), true, window, cx);
        let call = cx.background_spawn(async move {
            client.session_resume(&SessionResumeParams {
                command_id: new_command_id(),
                session_id,
                // History comes through `view/page`, which is the contiguous,
                // ordered, bounded path; resume just attaches.
                exclude_items: Some(true),
                cursor: None,
                history: None,
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    this.active = None;
                    this.report(&error, cx);
                }
                cx.notify();
            });
        }));
    }

    /// Put a session in the centre pane and subscribe to what it needs help
    /// with.
    fn open(&mut self, session_id: String, backfill: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let (provider, workspace) = (self.args.provider.clone(), self.workspace());
        let view = cx.new(|cx| SessionView::new(session_id, client, provider, workspace, window, cx));
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, _, event, cx| this.on_session_event(event, cx)));
        if backfill {
            view.update(cx, |view, cx| view.backfill(cx));
        }
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        cx.notify();
    }

    /// What a session cannot decide for itself.
    fn on_session_event(&mut self, event: &SessionEvent, cx: &mut Context<Self>) {
        match event {
            SessionEvent::Dialog { title, detail } => {
                self.dialog = Some(Dialog {
                    title: title.clone(),
                    detail: detail.clone(),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                });
            }
            SessionEvent::SignedOut { message } => {
                self.dialog = Some(Dialog {
                    title: "Signed out of Muse".into(),
                    detail: format!("Muse refused the turn: {message}"),
                    kind: DialogKind::Warning,
                    primary: "Sign in",
                    action: DialogAction::SignIn,
                });
            }
            // The child's exit already reached `route`, which owns the reconnect.
            SessionEvent::Closed => {}
        }
        cx.notify();
    }

    /// A failed command that the application, rather than a session, issued.
    fn report(&mut self, error: &MuseError, cx: &mut Context<Self>) {
        let title = conn::title(error);
        let dialog = Dialog {
            title,
            detail: error.to_string(),
            kind: DialogKind::Error,
            primary: match conn::severity(error) {
                Severity::Dialog => "Reconnect",
                Severity::Banner => "Dismiss",
            },
            action: match conn::severity(error) {
                Severity::Dialog => DialogAction::Reconnect,
                Severity::Banner => DialogAction::Dismiss,
            },
        };
        self.dialog = Some(dialog);
        cx.notify();
    }

    // ---------------------------------------------------------------- render

    fn render_login(&self, cx: &mut Context<Self>) -> AnyElement {
        let intent = cx.listener(|this: &mut Self, intent: &LoginIntent, _, cx| match intent {
            LoginIntent::Start | LoginIntent::Retry => this.start_login(cx),
            LoginIntent::OpenBrowser => {
                if let Some(url) = this.login.url.clone() {
                    // Straight into the child's argv; never into a log.
                    let _ = auth::open_in_browser(&url);
                }
            }
            LoginIntent::CopyCode => {
                if let Some(code) = this.login.code.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(code));
                }
            }
            LoginIntent::Cancel => {
                this.login = Login::default();
                cx.notify();
            }
        });
        login("login", self.login.state.clone())
            .product("Muse")
            .headline("Sign in to Muse")
            .subtitle("The harness drives the muse CLI, so it signs in the same way the CLI does.")
            .provider(aui_icons::Provider::Muse)
            .on_intent(move |i, window, cx| intent(&i, window, cx))
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let grouping = sidebar::grouping(&self.sessions);
        let selected = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
        });
        let mut view = sidebar_view("sessions", grouping).caption("Sessions").on_select(move |id, w, cx| select(id, w, cx));
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        v_flex()
            .size_full()
            .child(div().id("sessions-scroll").flex_1().min_h(px(0.0)).overflow_y_scroll().child(view))
            .child(self.render_footer(cx))
            .into_any_element()
    }

    /// "Signed in as", with the one action a signed-in person needs here.
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Auth::SignedIn(identity) = &self.auth else {
            return div().into_any_element();
        };
        let sign_out = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.logout(cx));
        let mut footer = sidebar_footer("account", identity.initial(), identity.name.clone())
            .trailing(button("sign-out", "Sign out").ghost().xs().on_click(sign_out));
        if !identity.email.is_empty() {
            footer = footer.detail(identity.email.clone());
        }
        footer.into_any_element()
    }

    /// The reconnect banner, above everything in the centre column.
    fn render_wire_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, text) = match &self.wire {
            Wire::Reconnecting => (BannerKind::Waiting, "Muse disconnected, reconnecting\u{2026}".to_owned()),
            Wire::Connecting => (BannerKind::Waiting, "Starting Muse\u{2026}".to_owned()),
            Wire::Down(reason) => (BannerKind::Error, format!("Muse is not running: {reason}")),
            Wire::Ready => return None,
        };
        let _ = cx;
        Some(
            div()
                .w_full()
                .px(px(scale::SP_7))
                .pt(px(scale::SP_4))
                .child(banner("wire", kind, vec![BannerRun::Text(text.into())]))
                .into_any_element(),
        )
    }

    fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let banner = self.render_wire_banner(cx);
        let body = match self.active.clone() {
            Some(view) => {
                if std::mem::take(&mut self.focus_composer) {
                    view.update(cx, |view, cx| view.focus_composer(window, cx));
                }
                view.update(cx, |view, cx| view.render_centre(window, cx))
            }
            None => self.render_no_session(cx),
        };
        v_flex()
            .size_full()
            .key_context(COMPOSER_CONTEXT)
            .on_action(cx.listener(|this, _: &SendTurn, window, cx| {
                if let Some(view) = this.active.clone() {
                    view.update(cx, |view, cx| view.send(window, cx));
                }
            }))
            .children(banner)
            .child(body)
            .into_any_element()
    }

    /// No session open: the one thing to do is start one.
    fn render_no_session(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let new = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.new_session(cx));
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(scale::SP_4))
            .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child("No session open"))
            .child(
                div()
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child(format!("Pick one on the left, or start a new one in {}.", self.workspace_name())),
            )
            .child(button("new-session", "New session").primary().icon(IconName::Plus).on_click(new))
            .into_any_element()
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let modal = self.dialog.as_ref()?;
        let action = modal.action;
        let primary = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.dialog = None;
            match action {
                DialogAction::Dismiss => {}
                DialogAction::Reconnect => {
                    this.wire = Wire::Reconnecting;
                    this.reconnect(cx);
                }
                DialogAction::SignIn => {
                    this.auth = Auth::SignedOut;
                    this.active = None;
                    this.login = Login::default();
                }
            }
            cx.notify();
        });
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.dialog = None;
            cx.notify();
        });
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.dialog = None;
            cx.notify();
        });
        Some(
            popover_layer(
                div()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_dialog)
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                        this.dialog = None;
                        cx.notify();
                    }))
                    .child(
                        dialog("dialog", modal.title.clone())
                            .kind(modal.kind)
                            .body(modal.detail.clone())
                            .secondary("Dismiss")
                            .primary(modal.primary)
                            .on_primary(move |w, cx| primary(&(), w, cx))
                            .on_secondary(move |w, cx| close(&(), w, cx))
                            .on_dismiss(move |w, cx| dismiss(&(), w, cx)),
                    ),
            )
            .into_any_element(),
        )
    }
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The login screen owns the whole window; the shell is not built behind
        // it, so nothing of the signed-in state can leak into a capture.
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let body: AnyElement = if signed_in {
            let sidebar = self.render_sidebar(cx);
            let centre = self.render_centre(window, cx);
            app_shell("shell")
                .traffic_lights(true)
                .sidebar_open(self.sidebar_open)
                .right_open(self.right_open)
                .header_sidebar(
                    sidebar_header("hd-side")
                        .traffic_lights(true)
                        .collapsed(!self.sidebar_open)
                        .on_toggle_sidebar(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                )
                .header_centre({
                    let mut centre = centre_header("hd-centre", self.workspace_name())
                        .provider(aui_icons::Provider::Muse)
                        .on_toggle_right(cx.listener(|this, _, _, cx| this.toggle_right(cx)));
                    if !self.sidebar_open {
                        centre = centre.on_expand_sidebar(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)));
                    }
                    centre
                })
                .header_right(right_header("hd-right").on_close(cx.listener(|this, _, _, cx| this.toggle_right(cx))))
                .sidebar(sidebar)
                // The right pane's slot is present and empty: the shell keeps
                // the column, so nothing has to move when Phase 5 fills it.
                .right(div().size_full())
                .centre(centre)
                .into_any_element()
        } else {
            self.render_login(cx).into_any_element()
        };
        let dialog = self.render_dialog(cx);
        aui::keys::track_pointer(
            div()
                .size_full()
                .relative()
                .key_context(aui::keys::ROOT_CONTEXT)
                .track_focus(&self.focus_root)
                .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
                // The slot is wired and does nothing: the keymap should not
                // grow a hole when the right pane arrives.
                .on_action(|_: &ToggleRightPane, _, _| {})
                .on_action(cx.listener(|this, _: &NewSession, _, cx| this.new_session(cx)))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| this.interrupt(cx)))
                .on_action(cx.listener(|this, _: &Cancel, _, cx| this.cancel(cx)))
                .on_action(|_: &FocusNext, window, cx| {
                    aui::keys::set_keyboard_nav(true, cx);
                    window.focus_next(cx);
                })
                .on_action(|_: &FocusPrev, window, cx| {
                    aui::keys::set_keyboard_nav(true, cx);
                    window.focus_prev(cx);
                })
                .child(body)
                .children(dialog),
        )
    }
}

impl Harness {
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// The right pane is out of scope this phase; the toggle stays wired.
    fn toggle_right(&mut self, _cx: &mut Context<Self>) {}

    /// ⌃C, and Escape on an empty composer: stop and retract.
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.interrupt(cx));
        }
    }

    /// Escape: close whatever is open, and otherwise stop the running turn if
    /// the composer is empty (spec §3.9).
    fn cancel(&mut self, cx: &mut Context<Self>) {
        if self.dialog.take().is_some() {
            cx.notify();
            return;
        }
        let empty = self.active.as_ref().is_some_and(|a| a.read(cx).draft_is_empty(cx));
        if empty {
            self.interrupt(cx);
        }
    }
}
