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

use aui::composer::composer_state_rows;
use aui::data::button;
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, Confirm, FocusNext, FocusPrev, SelectNext, SelectPrev, ToggleRightPane, ToggleSidebar};
use aui::nav::{sidebar_footer, sidebar_search, sidebar_view, RowAction};
use aui::overlay::{command_palette, dialog, popover_layer, DialogKind, PaletteIcon, PaletteItem, PaletteSection};
use aui::screens::{login, LoginIntent, LoginState};
use aui::shell::{app_shell, centre_header, right_header, sidebar_header};
use aui_icons::IconName;
use aui_tokens::{scale, ActiveAui, AuiStyled};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding,
    SharedString, Subscription, Task, Window,
};
use gpui_kit::base::input::{InputEvent, TextareaState};
use gpui_kit::component::input::Textarea;
use gpui_kit::base::{h_flex, v_flex};
use muse_client::schema::{
    ModelCatalogSource, ModelListParams, SessionListParams, SessionResumeParams, SessionStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::auth::{self, Identity, LoginEvent};
use crate::conn::{self, Severity};
use crate::index::{self, IndexEntry};
use crate::overlays::{Command, Dialog, DialogAction, MenuKind, Overlays, Palette, PaletteKind};
use crate::session::{SessionEvent, SessionView, TierBanner};
use crate::tier::{self, Tier};
use crate::sessions::{self, SessionMeta};
use crate::sidebar::{self, SessionEntry};
use crate::{files, skills, Args};

actions!(
    harness,
    [
        /// Send the composer's draft (Enter).
        SendTurn,
        /// Interject into the running turn (⌘↩).
        SteerTurn,
        /// Stop the running turn and retract its prompt (⌃C).
        Interrupt,
        /// Start a new session in this workspace (⌘N).
        NewSession,
        /// Toggle plan mode (Shift+Tab).
        TogglePlan,
        /// Open the model picker (⌘⇧M).
        OpenModelMenu,
        /// Open the reasoning-effort picker (⌘⇧E).
        OpenEffortMenu,
        /// Open the approval-mode picker (⌘⇧P).
        OpenModeMenu,
        /// Move the open menu's selection up.
        MenuUp,
        /// Move the open menu's selection down.
        MenuDown,
        /// Activate the open menu's selected row.
        MenuConfirm,
        /// Walk back through this workspace's prompt history (↑).
        HistoryPrev,
        /// Walk forward through it (↓).
        HistoryNext,
        /// ⌘V, which is an image attachment when the clipboard holds one.
        PasteMaybeImage,
        /// Attach a file or photo (⌘U).
        AttachFile,
        /// Send what is in an open approval-feedback or question-clarify field.
        ConfirmField,
        /// Put the keyboard in the sidebar's search field (⌘⇧F).
        FocusSearch,
        /// Commit the sidebar row's inline rename (Enter).
        ConfirmRename,
        /// Copy the transcript's held text selection (⌘C).
        CopySelection,
    ]
);

/// The key context the sidebar's inline rename field wears, so Enter commits
/// the name instead of reaching the composer's send.
const RENAME_CONTEXT: &str = "HarnessRename";

/// The context the composer holder wears, so Enter reaches [`SendTurn`] instead
/// of the textarea. Shift+Enter matches no binding and falls through to the
/// editor as a newline, which is exactly the behaviour §3.9 asks for.
///
/// Three more identifiers join it as the frame's state changes, and they are
/// what lets one key mean two things without either meaning being guessed at:
/// `menu` while a popover is open (↑/↓/↩ drive the list), and `histup` /
/// `histdown` while the caret is on the draft's first or last line (↑/↓ walk
/// the prompt history). With none of them set, the arrow keys belong to the
/// editor, where they always did.
const COMPOSER_CONTEXT: &str = "HarnessComposer";

/// The toast stack's own width, the library's `.toast{width:320px}`.
const TOAST_W: f32 = 320.0;
/// Where the stack hangs from: under the window header, at the right edge.
/// The stack lays its toasts out **downward** from its own box, so it is
/// anchored by its top; hanging it off the bottom would draw the newest toast
/// off the end of the window.
const TOAST_TOP: f32 = 56.0;
/// How much room the fanned stack is given before it would clip.
const TOAST_STACK_H: f32 = 260.0;
/// How long the "Session hidden" toast's Undo stays honest.
const UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
/// How far below the window's top edge the palette hangs, and how dark the
/// ground behind it goes. The library's `palette_scrim` is a design-card block
/// of a fixed height; a window overlay places itself.
const PALETTE_TOP: f32 = 96.0;
const PALETTE_SCRIM: f32 = 0.4;
/// How many rows the palette lists. The sidebar's search is the way through a
/// longer list; this is the way back to something recent.
const PALETTE_ROWS: usize = 12;
/// How many sessions one refresh will spend a `session/read` on. A workspace
/// with two hundred untitled sessions should not open two hundred reads on the
/// first frame; the rest are picked up by the next refresh.
const MAX_TITLE_READS: usize = 12;

/// Binds the harness's own keys on top of the library's.
///
/// `aui::init` has already bound ⌘B, ⌘K, ⌘\\, Escape, Tab and the approval
/// triad; these are the ones only this app knows about.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", SendTurn, Some("HarnessComposer && !menu && !field")),
        KeyBinding::new("enter", MenuConfirm, Some("HarnessComposer && menu")),
        // A card's own field owns Enter while it is open: the person is writing
        // a refusal, not a prompt.
        KeyBinding::new("enter", ConfirmField, Some("HarnessComposer && field")),
        KeyBinding::new("cmd-enter", SteerTurn, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("up", MenuUp, Some("HarnessComposer && menu")),
        KeyBinding::new("down", MenuDown, Some("HarnessComposer && menu")),
        KeyBinding::new("up", HistoryPrev, Some("HarnessComposer && histup && !menu")),
        KeyBinding::new("down", HistoryNext, Some("HarnessComposer && histdown && !menu")),
        KeyBinding::new("cmd-v", PasteMaybeImage, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("cmd-u", AttachFile, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("shift-tab", TogglePlan, Some(COMPOSER_CONTEXT)),
        KeyBinding::new("ctrl-c", Interrupt, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-n", NewSession, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-shift-m", OpenModelMenu, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-shift-e", OpenEffortMenu, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-shift-p", OpenModeMenu, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-shift-f", FocusSearch, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("enter", ConfirmRename, Some(RENAME_CONTEXT)),
        // The transcript list wears `TRANSCRIPT_CONTEXT`; the predicate keeps
        // this off the composer and every field, so copy there stays native.
        KeyBinding::new("cmd-c", CopySelection, Some(crate::session::TRANSCRIPT_COPY_KEYS)),
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
    /// A session switch paging history in: the view kept off-stage until its
    /// first backfill batch applies, so no frame flashes empty (C2).
    pending_active: Option<Entity<SessionView>>,
    /// The pending view's backfill landed; the next centre frame swaps it in.
    pending_ready: bool,
    /// Everything that floats: the modal, the open menu and the toasts. One
    /// entity, shared with the session view, which renders the halves that hang
    /// off the composer's own chips (spec §2.3).
    overlays: Entity<Overlays>,
    sidebar_open: bool,
    /// The right pane's slot exists; nothing opens it in this phase.
    right_open: bool,
    /// Whether `initialize` granted `userShell`. Requested in `conn::connect`;
    /// a server that did not grant it disables the `!` path with a banner
    /// rather than letting the command fail on the wire.
    user_shell: bool,
    focus_root: FocusHandle,
    focus_dialog: FocusHandle,
    focus_palette: FocusHandle,
    /// Set when the next frame should move the keyboard to the composer.
    focus_composer: bool,
    /// What the billing probe said, or `None` while it has not said it yet
    /// (spec §3.2, Phase 5 A1). A probe that failed is
    /// [`Tier::Unavailable`], never `None`.
    tier: Option<Tier>,
    /// A probe is in flight; a second one is not started on top of it.
    tier_probing: bool,
    /// The harness's own facts about each session: its name, whether it is
    /// hidden, and the title derived from its first shell command (spec §3.7).
    overrides: sessions::Overrides,
    /// Whether hidden sessions are listed anyway (the footer's toggle).
    show_hidden: bool,
    /// Whether sessions with no turns are listed anyway (the footer's toggle).
    show_empty: bool,
    /// The sidebar's search field, and whether it is on screen. The field is a
    /// slot the library frames and this owns.
    search: Entity<TextareaState>,
    search_open: bool,
    /// The session whose row is being renamed in place, and the field doing it.
    renaming: Option<String>,
    rename: Entity<TextareaState>,
    /// Sessions a `session/read` has already been spent on, so a title that
    /// genuinely is not there is not asked for once a frame (finding F10).
    titled: std::collections::HashSet<String>,
    /// Batches hidden in the last few seconds, newest last: the toast's Undo.
    /// One `/hide` is a batch of one; one "Clear empty" is a batch of
    /// everything it hid, so one Undo restores the whole batch.
    hidden_undo: Vec<Vec<String>>,
    /// The title last given to the window, so it is only set when it changed.
    window_title: Option<String>,
    /// "Send anyway" was pressed. Once per app run, deliberately: a person who
    /// accepted the bill this morning should be asked again tomorrow.
    send_anyway: bool,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl Harness {
    /// Boot: read `auth.json`, then connect and finish the probe.
    pub fn new(args: Args, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| composer_state_rows("Search sessions", 1, 1, window, cx));
        let rename = cx.new(|cx| composer_state_rows("Name this session", 1, 1, window, cx));
        let mut this = Self {
            args,
            client: None,
            wire: Wire::Connecting,
            auth: Auth::Probing,
            login: Login::default(),
            sessions: Vec::new(),
            index: HashMap::new(),
            active: None,
            pending_active: None,
            pending_ready: false,
            overlays: cx.new(|_| Overlays::default()),
            sidebar_open: true,
            right_open: false,
            user_shell: true,
            focus_root: cx.focus_handle(),
            focus_dialog: cx.focus_handle(),
            focus_palette: cx.focus_handle(),
            focus_composer: true,
            tier: None,
            tier_probing: false,
            overrides: sessions::Overrides::new(),
            show_hidden: false,
            show_empty: false,
            search: search.clone(),
            search_open: false,
            renaming: None,
            rename: rename.clone(),
            titled: std::collections::HashSet::new(),
            hidden_undo: Vec::new(),
            window_title: None,
            send_anyway: false,
            tasks: Vec::new(),
            subscriptions: Vec::new(),
        };
        // Typing in either field is what re-filters the list and what redraws
        // the row being renamed.
        for field in [&search, &rename] {
            this.subscriptions.push(cx.subscribe(field, |_: &mut Self, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        this.overrides = sessions::read();
        // `--tier` is a scripted answer to a probe that has not been run, and
        // it applies to every mode — including `--replay`, which is how the
        // banner is captured for nothing.
        this.tier = this.args.tier.clone();
        if this.args.replay.is_some() {
            // `--replay`: a capture, folded, with no child and no credential.
            // The shell is the point — the transcript is what is being looked
            // at — so the auth probe is skipped rather than faked into a login.
            this.auth = Auth::SignedIn(Identity {
                name: "Replay".into(),
                email: String::new(),
                api_key: false,
            });
            this.wire = Wire::Ready;
            return this;
        }
        if this.args.offline {
            // `--no-connect`: the chrome without a child, for a screenshot of
            // the login screen with sample data.
            this.auth = Auth::SignedOut;
            this.wire = Wire::Down("not connected".into());
            return this;
        }
        this.connect(cx);
        this.load_index(cx);
        this.load_menu_sources(cx);
        this
    }

    /// The workspace path as the wire and the header want it.
    fn workspace(&self) -> String {
        self.args.workspace.to_string_lossy().into_owned()
    }

    /// What the window's own title bar says: the open session, or the
    /// workspace when nothing is open.
    fn window_title(&self, cx: &gpui::App) -> String {
        let session = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
        let label = session.and_then(|id| self.sessions.iter().find(|e| e.id == id).map(|e| e.label.clone()));
        match label {
            Some(label) => format!("{label} \u{2014} {}", self.workspace_name()),
            None => self.workspace_name(),
        }
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
                    this.user_shell = connection
                        .server
                        .granted_capabilities
                        .iter()
                        .any(|c| c.as_wire() == Some("userShell"));
                    this.client = Some(connection.client);
                    this.wire = Wire::Ready;
                    this.pump(events, cx);
                    this.probe_catalog(cx);
                    cx.notify();
                }
                Err(error) => {
                    this.wire = Wire::Down(error.to_string());
                    this.set_dialog(cx, Dialog {
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
        self.title_from_transcript(cx);
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
                    this.set_dialog(cx, Dialog {
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
                    // A fresh login has a fresh `auth.json`, so the cache
                    // misses and this is also the re-probe a login asks for.
                    this.probe_tier(false, cx);
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

    // ---------------------------------------------------------- billing tier

    /// Find out what this login is entitled to (Phase 5 A1,
    /// `docs/06-billing.md`).
    ///
    /// The cache answers the ordinary boot; a probe only runs when `auth.json`
    /// has changed since the cached answer was taken, or when `force` says the
    /// person asked. **A probe that fails never stops the app**: it becomes
    /// [`Tier::Unavailable`], which draws a quiet banner and blocks nothing.
    fn probe_tier(&mut self, force: bool, cx: &mut Context<Self>) {
        // `--tier` fakes the probe for a screenshot, and nothing else.
        if let Some(faked) = self.args.tier.clone() {
            self.tier = Some(faked);
            self.push_tier(cx);
            return;
        }
        if !force {
            if let Some(cached) = tier::cached() {
                self.tier = Some(cached);
                self.push_tier(cx);
                return;
            }
        }
        if self.tier_probing {
            return;
        }
        self.tier_probing = true;
        let program = self.args.program.clone();
        let call = cx.background_spawn(async move { tier::probe(&program) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| {
                this.tier_probing = false;
                // The reason is the module's own words, never the terminal's.
                let tier = result.unwrap_or_else(Tier::Unavailable);
                tier::remember(&tier);
                this.tier = Some(tier);
                this.push_tier(cx);
                cx.notify();
            });
        }));
    }

    /// The banner the open session should be drawing, given the tier and
    /// whether "Send anyway" has been pressed.
    fn tier_banner(&self) -> Option<TierBanner> {
        match self.tier.as_ref()? {
            Tier::Subscription { .. } => None,
            Tier::PayAsYouGo => Some(TierBanner {
                text: "This login is on pay-as-you-go: every turn bills API usage. \
                       Sign out and back in after subscribing, or send anyway."
                    .to_owned(),
                blocking: !self.send_anyway,
            }),
            Tier::Unavailable(_) => Some(TierBanner {
                text: "Muse did not say which plan this login is on, so the harness cannot tell \
                       whether turns bill API usage."
                    .to_owned(),
                blocking: false,
            }),
        }
    }

    /// Hand the current banner to whatever session is open.
    fn push_tier(&mut self, cx: &mut Context<Self>) {
        let banner = self.tier_banner();
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.set_tier_banner(banner, cx));
        }
        cx.notify();
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
                        .map(|s| {
                            SessionEntry::join(
                                s,
                                this.index.get(&s.session_id),
                                this.overrides.get(&s.session_id),
                            )
                        })
                        .collect();
                    this.derive_titles(cx);
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
            // A scripted turn or a scripted capture with no session named needs
            // somewhere to go.
            if (self.args.send.is_some() || !self.args.steps.is_empty()) && self.active.is_none() {
                self.new_session(cx);
            }
            return;
        };
        let id = if wanted == "latest" {
            // The list arrives `updatedAt` descending, and the sidebar sorts on
            // the same field, so the newest row is the head of the grouping.
            self.sessions.iter().filter(|e| !e.hidden).max_by_key(|e| e.updated).map(|e| e.id.clone())
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

    /// `--steps`: drive the open session from the command line so a screenshot
    /// is reproducible. Consumed, so a later refresh does not replay them.
    fn run_steps(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let steps = std::mem::take(&mut self.args.steps);
        if steps.is_empty() {
            return;
        }
        let _ = window;
        // The steps run on a task rather than in a loop, because `wait:<ms>` is
        // the only way a scripted approval round-trip can exist: a decision has
        // to reach the wire and its `approval/updated` has to come back before
        // the next `choose:` means anything.
        crate::shot::set_steps_running(true);
        self.tasks.push(cx.spawn(async move |this, cx| {
            for step in steps {
                if let Some(ms) = step.strip_prefix("wait:") {
                    let ms: u64 = ms.parse().unwrap_or(0);
                    cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                    continue;
                }
                let ran = this.update_in(cx, |this, window, cx| {
                    if !this.step(&step, window, cx) {
                        this.with_session(cx, |view, cx| view.step(&step, window, cx));
                    }
                });
                if ran.is_err() {
                    crate::shot::set_steps_running(false);
                    return;
                }
            }
            crate::shot::set_steps_running(false);
        }));
    }

    /// The `--steps` verbs that belong to the window rather than to a session.
    ///
    /// Returns whether the step was one of them; anything else goes on to
    /// [`SessionView::step`].
    fn step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (head, rest) = step.split_once(':').unwrap_or((step, ""));
        match head {
            "search" => {
                self.focus_search(window, cx);
                self.search.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
            }
            "palette" => self.open_palette(PaletteKind::Commands, cx),
            "resume" => self.open_palette(PaletteKind::Resume, cx),
            "fork-picker" => self.open_palette(PaletteKind::Fork, cx),
            "rename" => {
                let session_id = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
                if let Some(session_id) = session_id {
                    self.start_rename(session_id, window, cx);
                    if !rest.is_empty() {
                        self.rename.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
                    }
                }
            }
            "hidden" => {
                self.show_hidden = !self.show_hidden;
                cx.notify();
            }
            "empty" => {
                self.show_empty = !self.show_empty;
                cx.notify();
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    /// Re-label the rows after the index arrives (it usually beats the wire,
    /// but the order is not guaranteed) or after an override changed.
    ///
    /// The same precedence [`SessionEntry::join`] documents, in one place.
    fn rejoin(&mut self) {
        for entry in &mut self.sessions {
            let meta = self.overrides.get(&entry.id);
            let index = self.index.get(&entry.id);
            let name = meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let derived = meta.and_then(|m| m.derived_title.as_deref()).map(str::trim).filter(|s| !s.is_empty());
            let label = name.or_else(|| index.and_then(IndexEntry::label)).or(derived);
            entry.needs_title = label.is_none();
            entry.label = label.unwrap_or(crate::sidebar::UNNAMED).to_owned();
            entry.hidden = meta.is_some_and(|m| m.hidden);
            entry.named = name.is_some();
        }
    }

    /// `session/start` in this workspace, on the configured provider.
    ///
    /// A new session is also the moment to re-walk the workspace: files come
    /// and go while the window is open, and the `@` picker should not offer a
    /// path that was deleted an hour ago.
    fn new_session(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        self.load_menu_sources(cx);
        let (workspace, provider) = (self.workspace(), self.args.provider.clone());
        // `session/start` is the only surface that declares a session's policy
        // up front; `session/setApprovalMode` afterwards is a different thing,
        // and on this server it does not reach `promptUnmatched`.
        let approval_mode = self.args.approval_mode.clone();
        let call = cx.background_spawn(async move {
            client.session_start(&SessionStartParams {
                command_id: new_command_id(),
                workspace_root: Some(workspace),
                provider_id: Some(provider),
                approval_mode,
                ..Default::default()
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(started) => {
                    this.open(started.session.session_id.clone(), false, window, cx);
                    // The result carries the session object `session/started`
                    // would have carried, and it is the only place a mode set
                    // at start-up is reported: `session/start` with an
                    // `approvalMode` raises no `session/approvalModeChanged`,
                    // so a session started under one mode drew the chip of
                    // another until this was folded.
                    if let Some(view) = this.active.clone() {
                        if let Ok(envelope) = serde_json::to_value(&started.session) {
                            view.update(cx, |view, cx| view.seed_session(envelope, cx));
                        }
                    }
                    this.load_sessions(cx);
                }
                Err(error) => this.report(&error, cx),
            });
        }));
    }

    /// `session/resume`, then page the whole transcript in.
    fn resume(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        // A hidden session is never loaded. Hiding is a decision about this
        // window's list, and a list that still opened what it refuses to show
        // would be a list that means nothing.
        if self.overrides.get(&session_id).is_some_and(|m| m.hidden) && !self.show_hidden {
            return;
        }
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
                    // A failed switch keeps the old view (C2): only a boot
                    // open with nothing behind it clears the centre pane.
                    if this.pending_active.take().is_some() {
                        this.pending_ready = false;
                    } else {
                        this.active = None;
                    }
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
        let overlays = self.overlays.clone();
        let view = cx.new(|cx| SessionView::new(session_id, Some(client), provider, workspace, overlays, window, cx));
        // A switch that pages history in does not swap synchronously: the
        // old view keeps rendering until the new view's first backfill batch
        // applies (C2), so no frame flashes the "New session" screen.
        // Backfill failure keeps the old view and reports (see `resume`).
        if backfill && self.active.is_some() {
            view.update(cx, |view, cx| view.backfill(cx));
            let titles: HashMap<String, String> =
                self.sessions.iter().map(|entry| (entry.id.clone(), entry.label.clone())).collect();
            let tier_banner = self.tier_banner();
            view.update(cx, |view, cx| {
                view.set_context(titles, self.user_shell);
                view.set_at_rest(self.args.screenshot.is_some());
                view.set_tier_banner(tier_banner, cx);
            });
            self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
            self.pending_active = Some(view);
            self.pending_ready = false;
            cx.notify();
            return;
        }
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        if backfill {
            view.update(cx, |view, cx| view.backfill(cx));
        }
        let titles: HashMap<String, String> =
            self.sessions.iter().map(|entry| (entry.id.clone(), entry.label.clone())).collect();
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| {
            view.set_context(titles, self.user_shell);
            view.set_at_rest(self.args.screenshot.is_some());
            view.set_tier_banner(tier_banner, cx);
        });
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        self.run_steps(window, cx);
        cx.notify();
    }

    /// Swap the deferred session view in once its backfill landed (C2).
    fn swap_pending_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.pending_active.take() else {
            self.pending_ready = false;
            return;
        };
        self.pending_ready = false;
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        self.active = Some(view);
        self.focus_composer = true;
        self.send_scripted(window, cx);
        self.run_steps(window, cx);
        cx.notify();
    }

    /// What a session cannot decide for itself.
    fn on_session_event(
        &mut self,
        view: Entity<SessionView>,
        event: &SessionEvent,
        cx: &mut Context<Self>,
    ) {
        match event {
            SessionEvent::Dialog { title, detail } => {
                self.set_dialog(cx, Dialog {
                    title: title.clone(),
                    detail: detail.clone(),
                    kind: DialogKind::Error,
                    primary: "Dismiss",
                    action: DialogAction::Dismiss,
                });
            }
            SessionEvent::SignedOut { message } => {
                self.set_dialog(cx, Dialog {
                    title: "Signed out of Muse".into(),
                    detail: format!("Muse refused the turn: {message}"),
                    kind: DialogKind::Warning,
                    primary: "Sign in",
                    action: DialogAction::SignIn,
                });
            }
            // The child's exit already reached `route`, which owns the reconnect.
            SessionEvent::Closed => {}
            // The deferred switch's first backfill batch applied: mark it and
            // let the next centre frame (which owns a `Window`) swap it in.
            SessionEvent::HistoryReady => {
                if self.pending_active.as_ref().is_some_and(|pending| *pending == view) {
                    self.pending_ready = true;
                    cx.notify();
                }
            }
            SessionEvent::NewSession => self.new_session(cx),
            // The fork result is a resume envelope for the **new** session, so
            // it is already attached: opening it and paging it in is all that
            // is left, and the sidebar re-reads itself because there is now one
            // more session in this workspace.
            SessionEvent::Forked { session_id, session } => {
                let (session_id, envelope) = (session_id.clone(), session.clone());
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| {
                        this.open(session_id, true, window, cx);
                        let target = this.pending_active.clone().or_else(|| this.active.clone());
                        if let Some(view) = target {
                            view.update(cx, |view, cx| view.seed_session(envelope, cx));
                        }
                        this.load_sessions(cx);
                    });
                }));
            }
            SessionEvent::Logout => self.logout(cx),
            SessionEvent::Status { detail } => {
                // What the login is entitled to belongs at the top of
                // `/status` and `/usage`: it is the first thing that decides
                // what the next turn costs.
                let plan = self.tier.as_ref().map(Tier::status_lines).unwrap_or_else(|| "Plan: probing\u{2026}".to_owned());
                self.set_dialog(cx, Dialog {
                    title: "Session status".into(),
                    detail: format!("{plan}\n\n{detail}"),
                    kind: DialogKind::Info,
                    primary: "Done",
                    action: DialogAction::Dismiss,
                });
                // `/usage` is a person asking; take the reading again behind
                // the dialog rather than serving a cache they just doubted.
                self.probe_tier(true, cx);
            }
            SessionEvent::TierOverride => {
                self.send_anyway = true;
                self.push_tier(cx);
            }
            SessionEvent::TierRecheck => self.probe_tier(true, cx),
            SessionEvent::Rename { name } => {
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.rename_session(session_id, name.clone(), cx);
                }
            }
            SessionEvent::RenameStart => {
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.tasks.push(cx.spawn(async move |this, cx| {
                        let _ = this.update_in(cx, |this, window, cx| this.start_rename(session_id, window, cx));
                    }));
                }
            }
            SessionEvent::Hide => {
                if let Some(view) = self.active.clone() {
                    let session_id = view.read(cx).session_id.clone();
                    self.hide_session(session_id, cx);
                }
            }
            SessionEvent::ToggleEmpty => {
                self.show_empty = !self.show_empty;
            }
            SessionEvent::Resume => self.open_palette(PaletteKind::Resume, cx),
            SessionEvent::ForkPicker => self.open_palette(PaletteKind::Fork, cx),
        }
        cx.notify();
    }

    /// Put a modal up. Only one at a time, which is what makes Escape's order
    /// (menu, then modal) a single rule.
    fn set_dialog(&mut self, cx: &mut Context<Self>, dialog: Dialog) {
        self.overlays.update(cx, |overlays, _| overlays.dialog = Some(dialog));
        cx.notify();
    }

    fn close_dialog(&mut self, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| overlays.dialog = None);
        cx.notify();
    }

    /// The two lists the `/` and `@` menus are built from, walked once at boot
    /// on the background executor and re-walked when a new session starts.
    ///
    /// Neither is on the wire: skills reach MSP only as `toolCall` items, and
    /// a mention is plain text inside the prompt (research §1.5).
    fn load_menu_sources(&mut self, cx: &mut Context<Self>) {
        let program = self.args.program.clone();
        let root = self.args.workspace.clone();
        let call = cx.background_spawn(async move { (skills::list(&program), files::walk(&root)) });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let (skills, files) = call.await;
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| {
                    overlays.skills = skills;
                    overlays.files = files;
                });
                cx.notify();
            });
        }));
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
        self.set_dialog(cx, dialog);
    }

    // ---------------------------------------------- session operations (A2)

    /// Change one session's override and write the store.
    ///
    /// The write is synchronous, and deliberately: it is a few hundred bytes,
    /// it happens on a gesture rather than in a loop, and a background write
    /// can lose a rename to a window that closed a moment later — which is the
    /// one outcome a store exists to prevent.
    fn set_override(&mut self, session_id: &str, edit: impl FnOnce(&mut SessionMeta), cx: &mut Context<Self>) {
        let meta = self.overrides.entry(session_id.to_owned()).or_default();
        edit(meta);
        self.rejoin();
        sessions::write(&self.overrides);
        cx.notify();
    }

    /// `/name`, and the row's inline field: rename the active session.
    fn rename_session(&mut self, session_id: String, name: Option<String>, cx: &mut Context<Self>) {
        let name = name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty());
        self.set_override(&session_id, |meta| meta.name = name, cx);
        self.renaming = None;
    }

    /// Open the inline field on a row, seeded with what the row says now.
    fn start_rename(&mut self, session_id: String, window: &mut Window, cx: &mut Context<Self>) {
        let current = self
            .overrides
            .get(&session_id)
            .and_then(|m| m.name.clone())
            .or_else(|| self.sessions.iter().find(|e| e.id == session_id).map(|e| e.label.clone()))
            .unwrap_or_default();
        self.rename.update(cx, |state, cx| state.set_value(current, window, cx));
        self.renaming = Some(session_id);
        window.focus(&self.rename.focus_handle(cx), cx);
        cx.notify();
    }

    /// Commit whatever is in the rename field.
    fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session_id) = self.renaming.clone() else { return };
        let text = self.rename.read(cx).value().to_string();
        self.rename_session(session_id, Some(text), cx);
        self.focus_composer = true;
        let _ = window;
    }

    /// `/hide` and the row's eye: take a session out of the list, with a way
    /// back for eight seconds.
    ///
    /// A hidden session is never loaded — the row is gone and so is the
    /// transcript — so the active one is closed when it is the one hidden.
    fn hide_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        if self.active.as_ref().is_some_and(|a| a.read(cx).session_id == session_id) {
            self.active = None;
        }
        self.hide_batch(
            vec![session_id],
            "Session hidden".to_owned(),
            "It is still on disk; Muse keeps its own list.",
            cx,
        );
    }

    /// Hide a batch of sessions with a way back for eight seconds: one
    /// toast, one Undo that restores the whole batch. One `/hide` is a
    /// batch of one; one "Clear empty" is a batch of everything it hid.
    fn hide_batch(&mut self, ids: Vec<String>, title: String, detail: &str, cx: &mut Context<Self>) {
        for session_id in &ids {
            self.set_override(session_id, |meta| meta.hidden = true, cx);
        }
        let toast =
            self.overlays.update(cx, |overlays, _| overlays.toast_with_action(title, detail, "Undo"));
        // The toast's own timer takes it away; this one takes the undo away
        // with it, so a press after it has gone does nothing.
        let undo = ids.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&toast));
                this.hidden_undo.retain(|batch| *batch != undo);
                cx.notify();
            });
        }));
        self.hidden_undo.push(ids);
        cx.notify();
    }

    /// The toast's Undo: put the newest hidden batch back — one row, or one
    /// "Clear empty" whole.
    fn unhide_newest(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.hidden_undo.pop() else { return };
        for session_id in batch {
            self.set_override(&session_id, |meta| meta.hidden = false, cx);
        }
    }

    /// "Clear empty": hide every session with no turns, with a way back for
    /// eight seconds. Rows already hidden stay out of the batch, so Undo
    /// restores exactly what this hid and nothing it did not.
    fn clear_empty(&mut self, cx: &mut Context<Self>) {
        let active = self.active_id(cx);
        let cleared: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| !entry.hidden && entry.is_empty(active.as_deref()))
            .map(|entry| entry.id.clone())
            .collect();
        if cleared.is_empty() {
            return;
        }
        let n = cleared.len();
        self.hide_batch(
            cleared,
            format!("{n} empty session{} hidden", if n == 1 { "" } else { "s" }),
            "They are still on disk; Muse keeps its own list.",
            cx,
        );
    }

    /// The open session's id, which the empty filter never applies to: a
    /// session just created has no turns yet and must stay visible.
    fn active_id(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|a| a.read(cx).session_id.clone())
    }

    /// The rows the sidebar should draw: hidden ones out unless asked for,
    /// sessions with no turns out unless asked for, and the search field's
    /// text applied. The open session is always drawn.
    fn visible_sessions(&self, cx: &gpui::App) -> Vec<SessionEntry> {
        let needle = self.search_text(cx);
        let active = self.active_id(cx);
        let mut rows: Vec<SessionEntry> = self
            .sessions
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .filter(|entry| self.show_empty || !entry.is_empty(active.as_deref()))
            .filter(|entry| entry.matches(&needle))
            .cloned()
            .collect();
        // Newest first. The sidebar's grouping sorts for itself; the palette
        // takes the head of this list, so the order has to be right here.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        rows
    }

    /// What is in the search field, or nothing when it is closed.
    fn search_text(&self, cx: &gpui::App) -> String {
        if !self.search_open {
            return String::new();
        }
        self.search.read(cx).value().to_string()
    }

    /// ⌘⇧F: show the search field and put the keyboard in it.
    fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.search_open = true;
        window.focus(&self.search.focus_handle(cx), cx);
        cx.notify();
    }

    /// Escape in the search field: empty it, close it, and give the keyboard
    /// back to the composer.
    fn clear_search(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.search_open {
            return false;
        }
        self.search.update(cx, |state, cx| state.set_value("", window, cx));
        self.search_open = false;
        self.focus_composer = true;
        cx.notify();
        true
    }

    /// F10. A session with no title anywhere: read its head and take the first
    /// `userShell` command as the row's name.
    ///
    /// `session/read` makes no model call, and the answer is cached in the
    /// store, so this costs one read per session, once, ever. It is also
    /// allowed to come back with nothing: the server decides what history it
    /// serves, and a session no host has loaded can serve none. A row that
    /// still has no title after this is honestly [`crate::sidebar::UNNAMED`].
    fn derive_titles(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let wanted: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| entry.needs_title && !self.titled.contains(&entry.id))
            .map(|entry| entry.id.clone())
            .take(MAX_TITLE_READS)
            .collect();
        if wanted.is_empty() {
            return;
        }
        self.titled.extend(wanted.iter().cloned());
        let call = cx.background_spawn(async move {
            wanted
                .into_iter()
                .map(|session_id| {
                    let read = client.session_read(&muse_client::schema::SessionReadParams {
                        session_id: session_id.clone(),
                        exclude_items: Some(false),
                    });
                    if let Err(error) = &read {
                        eprintln!("harness: session/read for a title failed: {error}");
                    }
                    let title = read.ok().and_then(|read| first_shell_command(&read));
                    (session_id, title)
                })
                .collect::<Vec<_>>()
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let derived = call.await;
            let _ = this.update(cx, |this, cx| {
                for (session_id, title) in derived {
                    let Some(title) = title else { continue };
                    this.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
                }
            });
        }));
    }

    /// F10. The open session's transcript may name it when nothing else does.
    ///
    /// Free — the fold is in memory — and the one path that reaches a session
    /// whose history the server will not serve to a `session/read`.
    fn title_from_transcript(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let session_id = view.read(cx).session_id.clone();
        // The row may not be in the list yet — `session/list` is a round-trip
        // and the transcript is already here — so the question is not "does the
        // row need a title" but "does this session have one".
        let has_title = self.overrides.get(&session_id).is_some_and(|m| m.name.is_some() || m.derived_title.is_some())
            || self.index.get(&session_id).and_then(IndexEntry::label).is_some();
        if has_title {
            return;
        }
        let Some(title) = view.read(cx).first_shell_title() else { return };
        self.set_override(&session_id, |meta| meta.derived_title = Some(title), cx);
    }

    /// Open the palette on one list.
    fn open_palette(&mut self, kind: PaletteKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = if already { None } else { Some(Palette { kind, selected: 0 }) };
        });
        cx.notify();
    }

    /// The palette's rows, in the order it draws them, so the keyboard and the
    /// click agree about what row 3 is.
    fn palette_rows(&self, kind: PaletteKind, cx: &gpui::App) -> Vec<(SharedString, SharedString, SharedString)> {
        match kind {
            PaletteKind::Commands => Command::ALL
                .into_iter()
                .map(|c| (c.slash().into(), c.slash().into(), c.description().into()))
                .collect(),
            PaletteKind::Resume => self
                .visible_sessions(cx)
                .into_iter()
                // Newest first, and only as many as the palette can show: a
                // list taller than the window is a list with a hidden bottom.
                .take(PALETTE_ROWS)
                .map(|entry| {
                    let meta: SharedString =
                        if entry.turns > 0 { format!("{} turns", entry.turns).into() } else { "no turns".into() };
                    (entry.id.clone().into(), entry.label.clone().into(), meta)
                })
                .collect(),
            // The active session's completed turns, newest first; the rows
            // come from the view because the window does not keep a transcript.
            PaletteKind::Fork => self
                .active
                .as_ref()
                .map(|view| view.read(cx).fork_turns())
                .unwrap_or_default()
                .into_iter()
                .take(PALETTE_ROWS)
                .map(|(id, label, detail)| (id.into(), label.into(), detail.into()))
                .collect(),
        }
    }

    /// Run the palette's selected row.
    fn confirm_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((kind, selected)) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected)) else {
            return;
        };
        let rows = self.palette_rows(kind, cx);
        let Some((id, _, _)) = rows.get(selected).cloned() else { return };
        self.overlays.update(cx, |overlays, _| overlays.palette = None);
        match kind {
            PaletteKind::Resume => self.resume(id.to_string(), window, cx),
            PaletteKind::Fork => {
                self.with_session(cx, |view, vc| view.fork(Some(id.to_string()), vc));
            }
            PaletteKind::Commands => {
                if let Some(command) = Command::parse(&id) {
                    self.with_session(cx, |view, cx| view.run_command(command, window, cx));
                }
            }
        }
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

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let visible = self.visible_sessions(cx);
        let empty = self.render_sidebar_empty(&visible, cx);
        let grouping = sidebar::grouping(&visible);
        let selected = self.active.as_ref().map(|a| a.read(cx).session_id.clone());
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
        });
        let act = cx.listener(|this: &mut Self, (id, action): &(SharedString, RowAction), window, cx| {
            match action {
                RowAction::Rename => this.start_rename(id.to_string(), window, cx),
                RowAction::Hide => this.hide_session(id.to_string(), cx),
                _ => {}
            }
        });
        let mut view = sidebar_view("sessions", grouping)
            .caption("Sessions")
            .row_actions(vec![RowAction::Rename, RowAction::Hide])
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |id, action, w, cx| act(&(id.clone(), action), w, cx));
        if let Some(renaming) = self.renaming.clone() {
            view = view.editing(renaming, self.rename_field(window, cx));
        }
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        let mut column = v_flex().size_full();
        if self.search_open {
            column = column.child(self.render_search(window, cx));
        }
        column
            .child(
                div()
                    .id("sessions-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_y_scroll()
                    .child(view)
                    .children(empty),
            )
            .child(self.render_footer(cx))
            .into_any_element()
    }

    /// The sidebar's search row: the library's frame around this window's own
    /// field.
    fn render_search(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let text = self.search_text(cx);
        let clear = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.clear_search(window, cx);
        });
        sidebar_search("sessions-search", Textarea::new(&self.search).text_size(aui_tokens::scaled(scale::FS_12)))
            .clearable(!text.is_empty())
            .on_clear(clear)
            .into_any_element()
    }

    /// The field the row being renamed holds.
    fn rename_field(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(cx.listener(|this, _: &ConfirmRename, window, cx| this.commit_rename(window, cx)))
            .child(Textarea::new(&self.rename).text_size(aui_tokens::scaled(scale::FS_12)))
            .into_any_element()
    }

    /// What the list says when it has nothing to show, and why (spec §5).
    fn render_sidebar_empty(&self, visible: &[SessionEntry], cx: &mut Context<Self>) -> Option<AnyElement> {
        if !visible.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        let needle = self.search_text(cx);
        let searching = !needle.is_empty();
        let active = self.active_id(cx);
        // What each filter alone is keeping out, past the other two: the
        // empty text names its own toggle rather than borrowing hidden's.
        let hidden_only =
            !self.show_hidden && self.sessions.iter().any(|e| e.hidden && e.matches(&needle));
        let empty_only = !self.show_empty
            && self
                .sessions
                .iter()
                .any(|e| (self.show_hidden || !e.hidden) && e.matches(&needle) && e.is_empty(active.as_deref()));
        let (title, detail) = match (searching, hidden_only, empty_only) {
            (true, _, _) => ("No sessions match", "Try fewer letters, or Esc to clear."),
            (false, true, _) => {
                ("Every session here is hidden", "Turn on \u{201c}Show hidden\u{201d} below to bring them back.")
            }
            (false, false, true) => {
                ("Only empty sessions here", "Turn on \u{201c}Show empty\u{201d} below to see them.")
            }
            (false, false, false) => ("No sessions yet", "\u{2318}N starts one."),
        };
        Some(
            v_flex()
                .w_full()
                .px(px(scale::SP_5))
                .py(px(scale::SP_6))
                .gap(px(scale::SP_2))
                .child(div().ui(scale::FS_12).medium().text_color(p.ink_2).child(title))
                .child(div().ui(scale::FS_11).text_color(p.ink_4).child(detail))
                .into_any_element(),
        )
    }

    /// "Signed in as", with the one action a signed-in person needs here.
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Auth::SignedIn(identity) = &self.auth else {
            return div().into_any_element();
        };
        let sign_out = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.logout(cx));
        let toggle_hidden = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.show_hidden = !this.show_hidden;
            cx.notify();
        });
        let toggle_empty = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.show_empty = !this.show_empty;
            cx.notify();
        });
        let clear_empty = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.clear_empty(cx);
        });
        let hidden = self.sessions.iter().filter(|e| e.hidden).count();
        let active = self.active_id(cx);
        let empty = self.sessions.iter().filter(|e| e.is_empty(active.as_deref())).count();
        let mut footer = sidebar_footer("account", identity.initial(), identity.name.clone())
            .trailing(button("sign-out", "Sign out").ghost().xs().on_click(sign_out));
        if !identity.email.is_empty() {
            footer = footer.detail(identity.email.clone());
        }
        // The third row: what this login is entitled to. Warning-tinted for
        // anything that is not a plan in force, because that is the case where
        // the next turn costs money nobody expected.
        if let Some(tier) = &self.tier {
            footer = footer.plan(tier.footer_label(), tier.is_warning());
        } else {
            // No plan row yet and something is hidden: the toggle still needs
            // a line to live on, so the row exists with nothing in it.
            footer = footer.plan("", false);
        }
        // Either toggle only appears once it has something to show: an
        // affordance for an empty set is a question nobody asked. Clearing
        // only appears once the empty rows are on screen to be cleared. The
        // rows stack vertically and right-aligned: the plan label keeps its
        // fixed width and truncates first, so the buttons must stay short
        // rather than squeeze the label into an ellipsis.
        let mut plan_rows: Vec<AnyElement> = Vec::new();
        if hidden > 0 {
            let label =
                if self.show_hidden { format!("Hide hidden ({hidden})") } else { format!("Show hidden ({hidden})") };
            plan_rows.push(button("show-hidden", label).ghost().xs().on_click(toggle_hidden).into_any_element());
        }
        if empty > 0 {
            // While the toggle is on, the rows it shows make the count
            // redundant, and the width is needed for the plan label.
            let label = if self.show_empty { "Hide empty".to_owned() } else { format!("Show empty ({empty})") };
            plan_rows.push(
                h_flex()
                    .gap(px(scale::SP_2))
                    .child(button("show-empty", label).ghost().xs().on_click(toggle_empty))
                    .into_any_element(),
            );
            // "Clear empty" gets its own row: beside the toggle it still
            // squeezed the plan label into an ellipsis.
            if self.show_empty {
                plan_rows.push(
                    button("clear-empty", "Clear empty").ghost().xs().on_click(clear_empty).into_any_element(),
                );
            }
        }
        if !plan_rows.is_empty() {
            footer = footer.plan_trailing(v_flex().items_end().gap(px(scale::SP_1)).children(plan_rows));
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

    /// `--replay`: open one session out of a capture file, once, on the first
    /// frame that has a `Window` to build a composer with.
    fn open_replay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.args.replay.take() else { return };
        let at_rest = self.args.screenshot.is_some();
        let (provider, workspace) = (self.args.provider.clone(), self.workspace());
        let overlays = self.overlays.clone();
        // The capture names its own session; this id is a placeholder the view
        // replaces the moment the first line is folded.
        let view = cx.new(|cx| {
            let mut view = SessionView::new("replay".to_owned(), None, provider, workspace, overlays, window, cx);
            view.set_at_rest(at_rest);
            view.load_replay(&path, cx);
            view
        });
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        self.sessions = vec![SessionEntry::replayed(&view.read(cx).session_id, &path)];
        let tier_banner = self.tier_banner();
        view.update(cx, |view, cx| view.set_tier_banner(tier_banner, cx));
        self.active = Some(view);
        self.run_steps(window, cx);
        cx.notify();
    }

    fn render_centre(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        self.open_replay(window, cx);
        // Deferred session switch (C2): the new view swaps in on its first
        // backfill batch, so no frame ever shows the empty state mid-switch.
        if self.pending_ready {
            self.swap_pending_in(window, cx);
        }
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
            .key_context(gpui::KeyContext::parse(&self.composer_context(cx)).unwrap_or_default())
            .on_action(cx.listener(|this, _: &SendTurn, window, cx| {
                this.with_session(cx, |view, cx| view.send(window, cx));
            }))
            .on_action(cx.listener(|this, _: &SteerTurn, window, cx| {
                this.with_session(cx, |view, cx| view.steer(window, cx));
            }))
            .on_action(cx.listener(|this, _: &MenuConfirm, window, cx| {
                this.with_session(cx, |view, cx| view.confirm_menu(window, cx));
            }))
            .on_action(cx.listener(|this, _: &MenuUp, _, cx| this.move_menu(-1, cx)))
            .on_action(cx.listener(|this, _: &MenuDown, _, cx| this.move_menu(1, cx)))
            .on_action(cx.listener(|this, _: &HistoryPrev, window, cx| {
                this.with_session(cx, |view, cx| view.history_prev(window, cx));
            }))
            .on_action(cx.listener(|this, _: &HistoryNext, window, cx| {
                this.with_session(cx, |view, cx| view.history_next(window, cx));
            }))
            .on_action(cx.listener(|this, _: &TogglePlan, _, cx| {
                this.with_session(cx, |view, cx| {
                    let on = !view.plan_mode();
                    view.set_plan(on, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &PasteMaybeImage, window, cx| this.paste(window, cx)))
            .on_action(cx.listener(|this, _: &AttachFile, _, cx| {
                this.with_session(cx, |view, cx| view.prompt_for_image(cx));
            }))
            .on_action(cx.listener(|this, _: &ConfirmField, window, cx| {
                this.with_session(cx, |view, cx| {
                    view.confirm_field(window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &CopySelection, window, cx| {
                this.with_session(cx, |view, cx| view.copy_selected(window, cx));
            }))
            // 1–9 on a pending approval: the n-th server-minted choice, in the
            // order the server sent them.
            .on_action(cx.listener(|this, nth: &aui::keys::ChooseNth, window, cx| {
                let index = nth.index;
                this.with_session(cx, |view, cx| view.choose_nth(index, window, cx));
            }))
            .children(banner)
            .child(body)
            .into_any_element()
    }

    /// The composer holder's key context for this frame.
    ///
    /// `menu`, `histup` and `histdown` are what let ↑, ↓ and ↩ mean the menu,
    /// the history or the editor without any of the three being guessed at.
    fn composer_context(&self, cx: &gpui::App) -> String {
        let mut context = String::from(COMPOSER_CONTEXT);
        if self.overlays.read(cx).menu.is_some() {
            context.push_str(" menu");
        }
        if let Some(view) = &self.active {
            if view.read(cx).card_field_open() {
                context.push_str(" field");
            }
            // The library binds 1–9 in its own approval context; naming that
            // context here is what hands the digits to the pending card, and
            // only while the composer is empty.
            if view.read(cx).card_has_keys(cx) {
                context.push(' ');
                context.push_str(aui::keys::APPROVAL_CONTEXT);
            }
        }
        if let Some(view) = &self.active {
            let (first, last) = view.read(cx).caret_edges(cx);
            if first {
                context.push_str(" histup");
            }
            if last {
                context.push_str(" histdown");
            }
        }
        context
    }

    fn with_session(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut SessionView, &mut Context<SessionView>)) {
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| f(view, cx));
        }
    }

    /// ↑/↓ in an open menu, wrapping over the rows the menu actually has.
    fn move_menu(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let rows = view.read(cx).menu_rows(cx);
        self.overlays.update(cx, |overlays, _| overlays.move_selection(delta, rows));
        cx.notify();
    }

    /// ⌘V. An image on the clipboard becomes an attachment; anything else is
    /// the textarea's own paste, which is re-dispatched rather than reimplemented.
    fn paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let handled = self
            .active
            .clone()
            .map(|view| view.update(cx, |view, cx| view.paste_image(cx)))
            .unwrap_or(false);
        if !handled {
            window.dispatch_action(Box::new(gpui_kit::base::input::Paste), cx);
        }
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
            .child(div().text_role(aui_tokens::TextRole::Title).text_color(p.ink_2).child(self.workspace_name()))
            .child(
                div()
                    .ui(scale::FS_12)
                    .text_color(p.ink_3)
                    .child("Pick a session on the left, or \u{2318}N to start one."),
            )
            .child(button("new-session", "New session").primary().icon(IconName::Plus).on_click(new))
            .into_any_element()
    }

    /// ⌘K and `/resume`: the command palette, over everything.
    ///
    /// The same primitive for both lists, because they are the same gesture —
    /// a list, an arrow key and a return — and a second picker would be a
    /// second set of keys to learn.
    fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, selected) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected))?;
        let rows = self.palette_rows(kind, cx);
        let (title, placeholder, icon) = match kind {
            PaletteKind::Commands => ("Commands", "Every command in this build", PaletteIcon::Glyph(IconName::Slash)),
            PaletteKind::Resume => ("Sessions", "Resume a session in this workspace", PaletteIcon::Glyph(IconName::Clock)),
            PaletteKind::Fork => {
                ("Fork from", "Pick a completed turn to branch from", PaletteIcon::Glyph(IconName::Git))
            }
        };
        let items: Vec<PaletteItem> = rows
            .iter()
            .map(|(id, label, detail)| PaletteItem::new(id.clone(), icon, label.clone()).context(detail.clone()))
            .collect();
        let select = cx.listener(move |this: &mut Self, id: &SharedString, window, cx| {
            let index = this.palette_rows(kind, cx).iter().position(|(row, _, _)| row == id);
            if let Some(index) = index {
                this.overlays.update(cx, |overlays, _| {
                    if let Some(palette) = overlays.palette.as_mut() {
                        palette.selected = index;
                    }
                });
                this.confirm_palette(window, cx);
            }
        });
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.palette = None);
            cx.notify();
        });
        let count = rows.len();
        Some(
            popover_layer(
                div()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_palette)
                    .on_action(cx.listener(move |this, _: &SelectNext, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(move |this, _: &SelectPrev, _, cx| {
                        this.overlays.update(cx, |o, _| o.move_palette(-1, count));
                        cx.notify();
                    }))
                    .on_action(cx.listener(|this, _: &Confirm, window, cx| this.confirm_palette(window, cx)))
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .absolute()
                    .inset_0()
                    .bg(gpui::black().opacity(PALETTE_SCRIM))
                    // A press on the dimmed ground closes it, which is the
                    // gesture every overlay in this window already answers to.
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.overlays.update(cx, |overlays, _| overlays.palette = None);
                        cx.notify();
                    }))
                    .child(
                        gpui_kit::base::h_flex()
                            .w_full()
                            .justify_center()
                            .pt(px(PALETTE_TOP))
                            .child(
                                command_palette("palette", "", vec![PaletteSection::new(title, items)], selected)
                                    .placeholder(placeholder)
                                    .on_select(move |id, w, cx| select(id, w, cx))
                                    .on_dismiss(move |w, cx| dismiss(&(), w, cx)),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read the modal out whole before anything asks `cx` for a listener:
        // the entity's borrow and `cx.listener` cannot be alive at once.
        let (title, detail, kind, primary_label, action) = {
            let modal = self.overlays.read(cx).dialog.as_ref()?;
            (modal.title.clone(), modal.detail.clone(), modal.kind, modal.primary, modal.action)
        };
        let primary = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.close_dialog(cx);
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
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        Some(
            popover_layer(
                div()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_dialog)
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| this.close_dialog(cx)))
                    .child(
                        dialog("dialog", title)
                            .kind(kind)
                            .body(detail)
                            .secondary("Dismiss")
                            .primary(primary_label)
                            .on_primary(move |w, cx| primary(&(), w, cx))
                            .on_secondary(move |w, cx| close(&(), w, cx))
                            .on_dismiss(move |w, cx| dismiss(&(), w, cx)),
                    ),
            )
            .into_any_element(),
        )
    }
}

/// F10. The first `userShell` command in a session's history, as a row title.
///
/// A session with no user prompt still did something, and what it did is the
/// only honest thing to call it. `session/read` makes no model call.
fn first_shell_command(read: &muse_client::schema::SessionReadResult) -> Option<String> {
    // The server decides what it serves, never the client: `mode: inline`
    // fills `items`, a snapshot fills `snapshot.state.items`, and both carry
    // the same item schema. Reading only the first would be a title that
    // depended on how big the session happened to be.
    let inline = read.history.items.iter().flatten();
    let snapshot = read.history.snapshot.iter().flat_map(|s| s.state.items.iter());
    inline
        .chain(snapshot)
        .filter(|item| item.kind == muse_client::schema::ItemKind::UserShell)
        .find_map(|item| item.command_text.as_deref().and_then(crate::sessions::shell_title))
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The window's title is the session's, so a person with three harness
        // windows open can tell them apart in Mission Control.
        let title = self.window_title(cx);
        if self.window_title.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.window_title = Some(title);
        }
        // The login screen owns the whole window; the shell is not built behind
        // it, so nothing of the signed-in state can leak into a capture.
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let body: AnyElement = if signed_in {
            let sidebar = self.render_sidebar(window, cx);
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
        let palette = self.render_palette(cx);
        let toasts = self.render_toasts(cx);
        // The palette takes the keyboard the frame it opens, so the arrows and
        // the return reach it rather than the composer under it.
        if palette.is_some() && !self.focus_palette.is_focused(window) {
            window.focus(&self.focus_palette, cx);
        }
        aui::keys::track_pointer(
            div()
                .size_full()
                .relative()
                .key_context(aui::keys::ROOT_CONTEXT)
                .track_focus(&self.focus_root)
                .on_action(cx.listener(|this, _: &OpenModelMenu, _, cx| this.open_picker(MenuKind::Model, cx)))
                .on_action(cx.listener(|this, _: &OpenEffortMenu, _, cx| this.open_picker(MenuKind::Effort, cx)))
                .on_action(cx.listener(|this, _: &OpenModeMenu, _, cx| this.open_picker(MenuKind::Mode, cx)))
                .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
                // The slot is wired and does nothing: the keymap should not
                // grow a hole when the right pane arrives.
                .on_action(|_: &ToggleRightPane, _, _| {})
                .on_action(cx.listener(|this, _: &NewSession, _, cx| this.new_session(cx)))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| this.interrupt(cx)))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.focus_search(window, cx)))
                .on_action(cx.listener(|this, _: &aui::keys::TogglePalette, _, cx| {
                    this.open_palette(PaletteKind::Commands, cx)
                }))
                .on_action(|_: &FocusNext, window, cx| {
                    aui::keys::set_keyboard_nav(true, cx);
                    window.focus_next(cx);
                })
                .on_action(|_: &FocusPrev, window, cx| {
                    aui::keys::set_keyboard_nav(true, cx);
                    window.focus_prev(cx);
                })
                .child(body)
                .children(toasts)
                .children(palette)
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

    /// ⌘⇧M / ⌘⇧E / ⌘⇧P: the same toggle the chip's own click does.
    fn open_picker(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        self.with_session(cx, |view, cx| view.toggle_picker(kind, cx));
    }

    /// The toast stack, bottom right. Toasts here are informational — a
    /// compaction that did nothing, a command a later phase brings — so they
    /// carry no action, only a close.
    fn render_toasts(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let toasts = self.overlays.read(cx).toasts.clone();
        if toasts.is_empty() {
            return None;
        }
        let newest = toasts.last().map(|t| t.id.to_string()).unwrap_or_default();
        let dismissed = newest.clone();
        let close = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&dismissed));
            cx.notify();
        });
        // One action exists, and it is Undo on hidden sessions — one row, or
        // one "Clear empty" batch.
        let act = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.unhide_newest(cx);
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&newest));
            cx.notify();
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .right(px(scale::SP_5))
                    .top(px(TOAST_TOP))
                    .w(px(TOAST_W))
                    .h(px(TOAST_STACK_H))
                    .child(
                        aui::feedback::toast_stack("toasts", toasts)
                            .on_action(move |_, window, cx| act(&(), window, cx))
                            .on_close(move |window, cx| close(&(), window, cx)),
                    ),
            )
            .into_any_element(),
        )
    }

    /// ⌃C, and Escape on an empty composer: stop and retract.
    fn interrupt(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| view.interrupt(cx));
        }
    }

    /// Escape: close whatever is open, and otherwise stop the running turn if
    /// the composer is empty (spec §3.9).
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed = self.overlays.update(cx, |overlays, _| overlays.close_topmost());
        if closed {
            cx.notify();
            return;
        }
        // An open rename is the next thing Escape takes back, then the search.
        if self.renaming.take().is_some() {
            self.focus_composer = true;
            cx.notify();
            return;
        }
        if self.clear_search(window, cx) {
            return;
        }
        // A transcript text selection is the next thing Escape takes back.
        if let Some(view) = self.active.clone() {
            if view.update(cx, |view, cx| view.clear_selection(cx)) {
                return;
            }
        }
        // A card's open field is the next thing Escape takes back, before it
        // reaches for the running turn.
        let field = self
            .active
            .clone()
            .map(|view| view.update(cx, |view, cx| view.close_card_field(cx)))
            .unwrap_or(false);
        if field {
            self.focus_composer = true;
            cx.notify();
            return;
        }
        let empty = self.active.as_ref().is_some_and(|a| a.read(cx).draft_is_empty(cx));
        if empty {
            self.interrupt(cx);
        }
    }
}
