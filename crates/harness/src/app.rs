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
//! §3.2) or the shell. The shell's right pane is not used; the column is
//! always closed.

use std::collections::HashMap;
use std::sync::Arc;

use aui::composer::composer_state_rows;
use aui::data::{button, icon_button, ButtonSize};
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, Confirm, FocusNext, FocusPrev, SelectNext, SelectPrev, TogglePalette, ToggleSidebar};
use aui::nav::{dense_field, nav_item, rail, sidebar_footer, sidebar_search, sidebar_view, view_menu, MenuRow, RailItem, RowAction};
use aui::overlay::{command_palette, dialog, popover_layer, DialogKind, PaletteIcon, PaletteItem, PaletteSection};
use aui::data::secret_field;
use aui::screens::{login, LoginIntent, LoginMethod, LoginState};
use aui::shell::{
    RESIZE_HANDLE_W, SIDEBAR_WIDTH, app_shell, clamp_sidebar_width, drag_capture_overlay,
    header_cell, resize_handle, sidebar_header,
};
use aui_icons::{provider_mark, IconName, Provider};
use aui_tokens::{scale, ActiveAui, AgentState, AuiStyled, AuiTheme};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding,
    SharedString, Subscription, Task, Window,
};
use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use gpui_kit::component::input::Textarea;
use gpui_kit::base::{h_flex, v_flex};
use muse_client::schema::{
    AccountLoginCompletedParams, AccountLoginOutcome, AccountLoginStartParams, AccountLoginType,
    AccountState, AccountStateKind, SessionListParams, SessionResumeParams, SessionStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::auth::{self, Identity};
use crate::conn::{self, Severity};
use crate::index::{self, IndexEntry};
use crate::overlays::{Command, Dialog, DialogAction, Menu, MenuKind, Overlays, Palette, PaletteKind};
use crate::session::{SessionEvent, SessionView, TierBanner};
use crate::tier::{self, Tier};
use crate::sessions::{self, SessionMeta};
use crate::sidebar::{self, SessionEntry};
use crate::search::{FileHit, SessionHit};
use crate::{files, layout, skills, Args, LoginSample};

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
        /// Close the window (⌘W), after the tier-probe cleanup.
        CloseWindow,
        /// Quit the app (⌘Q), after the tier-probe cleanup.
        QuitApp,
        /// Minimize the window (⌘M).
        MinimizeWindow,
        /// Toggle the window's zoom.
        ZoomWindow,
        /// Flip the theme between light and dark.
        ToggleTheme,
        /// Show the About dialog.
        ShowAbout,
        /// Reveal the docs folder in Finder.
        ShowDocs,
        /// Undo (Edit menu, for OS recognition; the focused field owns the keys).
        EditUndo,
        /// Redo (Edit menu, for OS recognition; the focused field owns the keys).
        EditRedo,
        /// Cut (Edit menu, for OS recognition; the focused field owns the keys).
        EditCut,
        /// Copy (Edit menu, for OS recognition; the focused field owns the keys).
        EditCopy,
        /// Paste (Edit menu, for OS recognition; the focused field owns the keys).
        EditPaste,
        /// Select all (Edit menu, for OS recognition; the focused field owns the keys).
        EditSelectAll,
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
/// triad; these are the ones only this app knows about. The window keys live
/// here too, so the native menu bar ([`set_menus`]) can show their shortcuts:
/// macOS reads each item's shortcut from the keymap.
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
        KeyBinding::new("cmd-w", CloseWindow, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-q", QuitApp, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("cmd-m", MinimizeWindow, Some(aui::keys::ROOT_CONTEXT)),
        KeyBinding::new("enter", ConfirmRename, Some(RENAME_CONTEXT)),
        // The transcript list wears `TRANSCRIPT_CONTEXT`; the predicate keeps
        // this off the composer and every field, so copy there stays native.
        KeyBinding::new("cmd-c", CopySelection, Some(crate::session::TRANSCRIPT_COPY_KEYS)),
    ]);
}

/// The native menu bar.
///
/// Called once, after [`bind_keys`]: macOS reads each item's shortcut from
/// the keymap, so an action without a binding shows no shortcut. File, View
/// and Window reuse the actions (and bindings) the app already handles; the
/// Edit items carry [`gpui::OsAction`] for OS recognition but no bindings —
/// rebinding ⌘X/⌘C/⌘V/⌘A/⌘Z globally would steal them from the focused field,
/// which owns editing (docs/08-keymap.md).
pub fn set_menus(cx: &mut App) {
    // Close and Quit are global, not window handlers: validation
    // (`is_action_available`) consults the focused window's dispatch tree,
    // which has nothing under it on the login screen, so window handlers
    // validate dimmed there. A global listener is available in every state,
    // which is what Quit in particular needs. Both run the same probe
    // cleanup as the window-close and app-quit hooks.
    // Close and Quit stay global listeners rather than window handlers.
    // Validation (`is_action_available`) walks the focused window's dispatch
    // tree, which reaches no handler on the login screen, so window handlers
    // validate dimmed there — and a dimmed item's shortcut is dead. A global
    // listener is available in every state, which is what Quit and Close in
    // particular need. Close walks `cx.windows()` instead of
    // `active_window()`, which is unset while a menu has the focus; this is
    // a one-window app, so that is the window. Both run the same probe
    // cleanup as the window-close and app-quit hooks.
    cx.on_action(|_: &CloseWindow, cx: &mut App| {
        crate::harness_log!("CloseWindow");
        crate::tier::cleanup_probes();
        // Deferred: menu dispatch already holds this window in an update
        // (`update_window_id` takes it out of `App.windows` while the
        // dispatch runs), so closing inline fails with "window not found".
        let windows = cx.windows();
        cx.defer(move |cx| {
            for window in windows {
                window.update(cx, |_, window, _| window.remove_window()).ok();
            }
        });
    });
    cx.on_action(|_: &QuitApp, cx: &mut App| {
        crate::harness_log!("QuitApp (global)");
        crate::tier::cleanup_probes();
        cx.quit();
    });
    cx.set_menus([
        gpui::Menu::new("Harness").items([
            gpui::MenuItem::action("About Harness", ShowAbout),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_submenu("Services", gpui::SystemMenuType::Services),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Quit Harness", QuitApp),
        ]),
        gpui::Menu::new("File").items([
            gpui::MenuItem::action("New Session", NewSession),
            gpui::MenuItem::action("Close Window", CloseWindow),
        ]),
        gpui::Menu::new("Edit").items([
            gpui::MenuItem::os_action("Undo", EditUndo, gpui::OsAction::Undo),
            gpui::MenuItem::os_action("Redo", EditRedo, gpui::OsAction::Redo),
            gpui::MenuItem::separator(),
            gpui::MenuItem::os_action("Cut", EditCut, gpui::OsAction::Cut),
            gpui::MenuItem::os_action("Copy", EditCopy, gpui::OsAction::Copy),
            gpui::MenuItem::os_action("Paste", EditPaste, gpui::OsAction::Paste),
            gpui::MenuItem::os_action("Select All", EditSelectAll, gpui::OsAction::SelectAll),
        ]),
        gpui::Menu::new("View").items([
            gpui::MenuItem::action("Toggle Sidebar", ToggleSidebar),
            gpui::MenuItem::action("Command Palette\u{2026}", TogglePalette),
            gpui::MenuItem::action("Find in Sessions", FocusSearch),
            gpui::MenuItem::separator(),
            gpui::MenuItem::action("Toggle Theme", ToggleTheme),
        ]),
        gpui::Menu::new("Window").items([
            gpui::MenuItem::action("Minimize", MinimizeWindow),
            gpui::MenuItem::action("Zoom", ZoomWindow),
        ]),
        gpui::Menu::new("Help").items([
            gpui::MenuItem::action("Harness Documentation", ShowDocs),
        ]),
    ]);
}

/// Where the boot probe got to: sign-in is on the wire now
/// (`docs/diagnosis/login.md`, D22), so this is the `account/read` answer.
enum Auth {
    /// Waiting for `account/read`.
    Probing,
    /// `loggedOut`: the login screen.
    SignedOut,
    /// Any other lane, with the wire's identity.
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

/// The login screen's own state. The screen itself is stateless (it is the
/// library's `aui::screens::login`): this owns the [`LoginState`], the
/// device flow's URL and code, which method is running, and the API-key
/// field. There is no task field: every wire call runs on the background
/// executor and returns through `update`, like every other command.
struct Login {
    state: LoginState,
    url: Option<String>,
    code: Option<String>,
    method: Option<LoginMethod>,
    /// The masked API-key field, created once in [`Harness::new`]. The key
    /// text is read once on submit and the field is cleared when the call
    /// returns; the key never lands in `Harness`, a log, or a fixture.
    api_key: Entity<InputState>,
    /// Mirror of the field's masked flag, kept in sync by both toggle paths
    /// (the eye button and `ToggleReveal`) so the intent can flip from it.
    revealed: bool,
}

impl Login {
    /// The method choice: forget the flow. The field keeps whatever it
    /// holds — callers that leave the key form (`Back`, `ChooseAnother`,
    /// a returned submit) clear it explicitly — so notify after calling.
    fn reset_to_choose(&mut self) {
        self.state = LoginState::Choose;
        self.url = None;
        self.code = None;
        self.method = None;
        crate::harness_log!("login → choose");
    }
}

/// The [`LoginState`] name as the `harness: login → …` stderr line spells it,
/// so a headless `--login-steps` run can be followed from a log. No URL,
/// code or key ever reaches that line.
fn login_state_name(state: &LoginState) -> &'static str {
    match state {
        LoginState::Choose => "choose",
        LoginState::Starting => "starting",
        LoginState::Device { .. } => "device",
        LoginState::ApiKey { .. } => "apikey",
        LoginState::Validating => "validating",
        LoginState::Success => "success",
        LoginState::Error { .. } => "error",
    }
}

/// What one toast's Undo restores: one `/hide` is a batch of one, one
/// "Clear empty" is a batch of everything it hid, and one archive confirm is
/// a batch of one archived session.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UndoBatch {
    Hidden(Vec<String>),
    Archived(Vec<String>),
}

/// Actions of the header's overflow menu, in row order.
#[derive(Clone, Copy)]
enum OverflowAction {
    Rename,
    Fork,
    Archive,
}

/// Actions of the Sessions caption's view menu, in row order.
#[derive(Clone, Copy)]
enum ViewAction {
    ToggleEmpty,
    ToggleHidden,
    ClearEmpty,
    ToggleArchived,
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
    /// The sidebar divider's current x, in window pixels. Local state until
    /// the drag settles, then `layout.json` (see [`crate::layout`]).
    sidebar_width: f32,
    /// A resize drag is in flight: the shell skips its layout spring so the
    /// divider tracks the pointer, and the capture overlay owns every move.
    resizing: bool,
    /// The pointer x where the drag started, in window pixels.
    grab_x: f32,
    /// [`Self::sidebar_width`] when the drag started: every move measures
    /// from here, so a stalled frame can never compound an error.
    start_w: f32,
    /// How far the width has travelled this drag, in pixels. A release with
    /// no travel shortly after the previous one is a double-click, which
    /// resets to the default: the handle reports positions only, never the
    /// click count, so quick taps are the only double-click signal it gives.
    drag_moved: f32,
    /// When the last drag ended, for the double-click reset above.
    last_release: Option<std::time::Instant>,
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
    /// Whether hidden sessions are listed anyway (the Sessions menu's toggle).
    show_hidden: bool,
    /// Whether sessions with no turns are listed anyway (the Sessions menu's toggle).
    show_empty: bool,
    /// Whether archived sessions are listed anyway (the Sessions menu's toggle).
    show_archived: bool,
    /// The search palette's query field. The card's own query row shows the
    /// result count; typing here re-queries `search.db` off the UI thread.
    /// There is no sidebar quick-filter: ⌘⇧F and the sidebar search icon open
    /// only this palette, so the two can never be open together.
    search_query: Entity<TextareaState>,
    /// Full-text session hits for the open search palette, latest query only.
    search_sessions: Vec<SessionHit>,
    /// Created-file hits for the open search palette, latest query only.
    search_files: Vec<FileHit>,
    /// Monotonic id for palette queries; only the latest result is applied.
    search_epoch: u64,
    /// The session whose row is being renamed in place, and the field doing it.
    renaming: Option<String>,
    rename: Entity<TextareaState>,
    /// Sessions a `session/read` has already been spent on, so a title that
    /// genuinely is not there is not asked for once a frame (finding F10).
    titled: std::collections::HashSet<String>,
    /// Batches taken out of the list in the last few seconds, newest last:
    /// the toast's Undo. One `/hide` is a batch of one; one "Clear empty" is
    /// a batch of everything it hid; one archive confirm is a batch of one
    /// archived session. One Undo restores the whole batch.
    undo_stack: Vec<UndoBatch>,
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
        // The rename field is one visual line: soft wrap off, so a long name
        // scrolls under the caret instead of spilling a second line.
        let rename = cx.new(|cx| {
            let mut state = composer_state_rows("Name this session", 1, 1, window, cx);
            state.set_soft_wrap(false, window, cx);
            state
        });
        let search_query = cx.new(|cx| composer_state_rows("Search sessions and created files", 1, 1, window, cx));
        // The API-key field: masked, with the capture-safe placeholder. Enter
        // inside it submits (single-line inputs always emit `PressEnter`).
        let api_key = cx.new(|cx| InputState::new(window, cx).masked(true).placeholder("Paste your key"));
        // The divider's last settled x, or the default for a fresh store.
        let restored = layout::sidebar_width(&layout::read());
        let mut this = Self {
            args,
            client: None,
            wire: Wire::Connecting,
            auth: Auth::Probing,
            login: Login {
                state: LoginState::Choose,
                url: None,
                code: None,
                method: None,
                api_key: api_key.clone(),
                revealed: false,
            },
            sessions: Vec::new(),
            index: HashMap::new(),
            active: None,
            pending_active: None,
            pending_ready: false,
            overlays: cx.new(|_| Overlays::default()),
            sidebar_open: true,
            sidebar_width: restored,
            resizing: false,
            grab_x: 0.0,
            start_w: restored,
            drag_moved: 0.0,
            last_release: None,
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
            show_archived: false,
            search_query: search_query.clone(),
            search_sessions: Vec::new(),
            search_files: Vec::new(),
            search_epoch: 0,
            renaming: None,
            rename: rename.clone(),
            titled: std::collections::HashSet::new(),
            undo_stack: Vec::new(),
            window_title: None,
            send_anyway: false,
            tasks: Vec::new(),
            subscriptions: Vec::new(),
        };
        // Typing in the rename field redraws the row being renamed.
        this.subscriptions.push(cx.subscribe(&rename, |_: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                cx.notify();
            }
        }));
        // Typing in the search palette's query re-queries off the UI thread.
        this.subscriptions.push(cx.subscribe(&search_query, |this: &mut Self, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.refresh_search(cx);
            }
        }));
        // The API-key field: Enter submits, and any change re-renders the
        // form — `can_submit` is recomputed in `render_login`, so the Sign
        // in button tracks the field's non-empty trimmed text.
        this.subscriptions.push(cx.subscribe(&api_key, |this: &mut Self, _, event: &InputEvent, cx| {
            match event {
                InputEvent::PressEnter { .. } => this.submit_api_key(cx),
                InputEvent::Change => cx.notify(),
                _ => {}
            }
        }));
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
                lane: AccountStateKind::AccountLogin,
                name: "Replay".into(),
                email: String::new(),
            });
            this.wire = Wire::Ready;
            return this;
        }
        if this.args.offline {
            // `--no-connect`: the chrome without a child, for a screenshot of
            // the login screen with sample data (`--login` picks the state).
            this.auth = Auth::SignedOut;
            this.wire = Wire::Down("not connected".into());
            this.apply_login_sample(window, cx);
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
                    crate::harness_log!("connected to {} {}", server.name, server.version);
                    if let Some(warning) = &connection.warning {
                        // A fingerprint mismatch is additive evolution, never a
                        // failure: say so on stderr and carry on.
                        crate::harness_log!("{warning:?}");
                    }
                    this.user_shell = connection
                        .server
                        .granted_capabilities
                        .iter()
                        .any(|c| c.as_wire() == Some("userShell"));
                    this.client = Some(connection.client);
                    this.wire = Wire::Ready;
                    this.pump(events, cx);
                    this.probe_account(cx);
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
                        archive_target: None,
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
            crate::harness_log!("muse serve exited ({code:?}); reconnecting");
            self.wire = Wire::Reconnecting;
            self.reconnect(cx);
        }
        // The account notifications own sign-in: they are folded before the
        // session view sees anything, and the session view never sees them.
        if let MuseEvent::Notification { method, params, .. } = &event {
            if method.starts_with("account/") {
                self.route_account(method, params, cx);
                cx.notify();
                return;
            }
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
            self.record_last_summary(cx);
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
                        archive_target: None,
                    });
                    cx.notify();
                }
            });
        }));
    }

    // ------------------------------------------------------------------ auth

    /// Move to `state`, with the one stderr line a headless `--login-steps`
    /// run follows the flow by. The line carries the state name only — never
    /// a URL, a code or a key.
    fn set_login_state(&mut self, state: LoginState, cx: &mut Context<Self>) {
        crate::harness_log!("login → {}", login_state_name(&state));
        self.login.state = state;
        cx.notify();
    }

    /// The boot probe and the re-probe after every `account/changed`:
    /// `account/read` is the only sign-in signal (`model/list` answers from
    /// the provider catalog while logged out, so it never was one).
    fn probe_account(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else { return };
        let call = cx.background_spawn(async move { client.account_read() });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(state) => this.apply_account(state, cx),
                Err(error) => {
                    // The wire is up but the probe failed: say so and show
                    // the login screen, like the old probe did. Never logs
                    // more than the failure itself.
                    crate::harness_log!("account/read failed: {error}");
                    crate::harness_log!("account → loggedOut");
                    this.auth = Auth::SignedOut;
                    this.login.reset_to_choose();
                    this.run_login_steps(cx);
                    cx.notify();
                }
            });
        }));
    }

    /// Rebuild [`Auth`] from an [`AccountState`] — the probe answer and the
    /// `account/changed` notification share this exactly.
    ///
    /// `loggedOut` clears the shell and shows the login screen (with the
    /// "removed outside the app" dialog when a signed-in session loses its
    /// credential); any other lane signs in, loads the sessions, and works
    /// out the tier — the TUI probe for `accountLogin`, pay-as-you-go by
    /// construction for the key lanes.
    fn apply_account(&mut self, state: AccountState, cx: &mut Context<Self>) {
        let lane = state.state.as_wire().unwrap_or("unknown").to_owned();
        match Identity::from_account(&state) {
            Some(identity) => {
                crate::harness_log!("account → {lane}");
                let api_key = identity.is_api_key();
                self.auth = Auth::SignedIn(identity);
                self.load_sessions(cx);
                // `--tier` fakes the probe for a screenshot, and nothing else:
                // it wins over the lane, exactly as `probe_tier` does, so a
                // scripted capture can get past the pay-as-you-go guard.
                if let Some(faked) = self.args.tier.clone() {
                    self.tier = Some(faked);
                    self.push_tier(cx);
                } else if api_key {
                    // The TUI probe is about subscriptions; a stored key or
                    // `META_API_KEY` bills pay-as-you-go by construction, so
                    // the footer says so without probing.
                    self.tier = Some(Tier::PayAsYouGo);
                    self.push_tier(cx);
                } else {
                    // A fresh login is also the re-probe a login asks for.
                    self.probe_tier(false, cx);
                }
                cx.notify();
            }
            None => {
                crate::harness_log!("account → loggedOut");
                let was_in = matches!(self.auth, Auth::SignedIn(_));
                self.active = None;
                self.sessions.clear();
                self.auth = Auth::SignedOut;
                self.login.reset_to_choose();
                if was_in {
                    self.set_dialog(cx, Dialog {
                        title: "Signed out of Muse".into(),
                        detail: "The credential was removed outside the app.".into(),
                        kind: DialogKind::Warning,
                        primary: "Sign in",
                        action: DialogAction::SignIn,
                        archive_target: None,
                    });
                }
                self.run_login_steps(cx);
                cx.notify();
            }
        }
    }

    /// One [`LoginIntent`]: the login screen's buttons, the Escape walk, and
    /// the `--login-steps` verbs all arrive here. Needs the window for the
    /// field (focus, clearing) — background completions that touch the field
    /// go through `update_in` to get one.
    fn login_intent(&mut self, intent: LoginIntent, window: &mut Window, cx: &mut Context<Self>) {
        match intent {
            LoginIntent::StartAccount => self.start_device_flow(cx),
            LoginIntent::UseApiKey => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::ApiKey { can_submit: false, error: None }, cx);
                window.focus(&self.login.api_key.focus_handle(cx), cx);
            }
            LoginIntent::SubmitApiKey => self.submit_api_key(cx),
            LoginIntent::ToggleReveal => {
                let next = !self.login.revealed;
                self.login.revealed = next;
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_masked(next, window, cx));
                cx.notify();
            }
            LoginIntent::Back | LoginIntent::ChooseAnother => {
                self.login.reset_to_choose();
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.clean(window, cx));
                cx.notify();
            }
            LoginIntent::OpenBrowser => {
                if let Some(url) = self.login.url.clone() {
                    // Straight into the child's argv; never into a log.
                    let _ = auth::open_in_browser(&url);
                }
            }
            LoginIntent::CopyCode => {
                if let Some(code) = self.login.code.clone() {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(code));
                }
            }
            LoginIntent::Retry => match self.login.method {
                Some(LoginMethod::Account) => self.start_device_flow(cx),
                Some(LoginMethod::ApiKey) => self.login_intent(LoginIntent::UseApiKey, window, cx),
                None => {
                    self.login.reset_to_choose();
                    cx.notify();
                }
            },
            LoginIntent::Cancel => self.cancel_login_flow(cx),
        }
    }

    /// Start the device-code flow: `account/loginStart {deviceCode}` on the
    /// background executor. The URL and code come back in the result — never
    /// in a notification — and the browser opens once, on entering the
    /// device state (D26). Nothing here is logged but the state name.
    fn start_device_flow(&mut self, cx: &mut Context<Self>) {
        self.login.method = Some(LoginMethod::Account);
        let Some(client) = self.client.clone() else {
            self.set_login_state(
                LoginState::Error { message: "Muse is not running.".into(), method: Some(LoginMethod::Account) },
                cx,
            );
            return;
        };
        self.set_login_state(LoginState::Starting, cx);
        let call = cx.background_spawn(async move {
            client.account_login_start(&AccountLoginStartParams {
                api_key: None,
                r#type: AccountLoginType::DeviceCode,
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(start) => match (start.verification_url, start.user_code) {
                    (Some(url), Some(code)) => {
                        this.login.url = Some(url.clone());
                        this.login.code = Some(code.clone());
                        this.set_login_state(
                            LoginState::Device {
                                url: url.clone().into(),
                                code: code.clone().into(),
                                expires: None,
                                waiting: true,
                            },
                            cx,
                        );
                        let _ = auth::open_in_browser(&url);
                    }
                    _ => {
                        this.set_login_state(
                            LoginState::Error {
                                message: "The server started no device flow.".into(),
                                method: Some(LoginMethod::Account),
                            },
                            cx,
                        );
                    }
                },
                Err(error) => {
                    this.set_login_state(
                        LoginState::Error { message: error.to_string().into(), method: Some(LoginMethod::Account) },
                        cx,
                    );
                }
            });
        }));
    }

    /// Submit the API-key form: read the field once, trim, send
    /// `account/loginStart {apiKey}` on the background executor. The key is
    /// a local in the task closure and nowhere else; the field is cleared
    /// when the call returns, whatever it returned (D24). An empty field
    /// does nothing — the Sign in button is disabled until there is text.
    fn submit_api_key(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.login.state, LoginState::ApiKey { .. }) {
            return;
        }
        let key = self.login.api_key.read(cx).value().to_string();
        let trimmed = key.trim().to_owned();
        if trimmed.is_empty() {
            return;
        }
        let Some(client) = self.client.clone() else {
            self.set_login_state(
                LoginState::ApiKey {
                    can_submit: false,
                    error: Some("Muse is not running.".into()),
                },
                cx,
            );
            return;
        };
        self.login.method = Some(LoginMethod::ApiKey);
        self.set_login_state(LoginState::Validating, cx);
        let call = cx.background_spawn(async move {
            client.account_login_start(&AccountLoginStartParams {
                api_key: Some(trimmed),
                r#type: AccountLoginType::ApiKey,
            })
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            // `update_in` for the window the field-clear needs.
            let _ = this.update_in(cx, |this, window, cx| {
                let api_key = this.login.api_key.clone();
                api_key.update(cx, |state, cx| state.clean(window, cx));
                match result {
                    // A stored key: the signed-in `account/changed` follows
                    // and enters the app; until then the spinner stays.
                    Ok(_) => cx.notify(),
                    Err(error) => {
                        this.set_login_state(
                            LoginState::ApiKey {
                                can_submit: false,
                                error: Some(error.to_string().into()),
                            },
                            cx,
                        );
                    }
                }
            });
        }));
    }

    /// Abandon the running flow: back to the method choice immediately, and
    /// `account/loginCancel` in the background. The `cancelled`
    /// notification that precedes its result is then a no-op — the screen is
    /// already where it would go.
    fn cancel_login_flow(&mut self, cx: &mut Context<Self>) {
        self.login.reset_to_choose();
        cx.notify();
        let Some(client) = self.client.clone() else { return };
        cx.background_spawn(async move {
            let _ = client.account_login_cancel();
        })
        .detach();
    }

    /// After a successful login: drop the child that inherited no credential,
    /// spawn a fresh one and re-probe.
    ///
    /// Delete this once one billed turn, run after a real Meta-account login,
    /// confirms D25: that the device flow is host-owned, so the `muse serve`
    /// that ran it already holds the credential and the app proceeds on
    /// `account/changed` with no reconnect. Until that turn is run, this stays
    /// as dead code kept warm for the case D25 turns out wrong.
    #[allow(dead_code)]
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
                    this.login.reset_to_choose();
                    this.pump(events, cx);
                    this.probe_account(cx);
                    cx.notify();
                }
                Err(error) => {
                    this.wire = Wire::Down(error.to_string());
                    this.set_login_state(
                        LoginState::Error { message: error.to_string().into(), method: this.login.method },
                        cx,
                    );
                    this.auth = Auth::SignedOut;
                }
            });
        }));
    }

    /// Sign out over the wire: `account/logout` in the background, and its
    /// result — an [`AccountState`] — applied like `account/changed`, except
    /// a deliberate sign-out never raises the "removed outside the app"
    /// dialog. An `envKey` lane survives this (the environment still holds
    /// the key), so that case keeps the shell and explains itself in a toast
    /// (D28).
    fn logout(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            self.active = None;
            self.sessions.clear();
            self.auth = Auth::SignedOut;
            self.login.reset_to_choose();
            cx.notify();
            return;
        };
        let call = cx.background_spawn(async move { client.account_logout() });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = call.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(state) => match Identity::from_account(&state) {
                    Some(identity) => {
                        let env_key = identity.lane == AccountStateKind::EnvKey;
                        this.auth = Auth::SignedIn(identity);
                        if env_key {
                            this.overlays.update(cx, |overlays, _| {
                                overlays.toast(
                                    "Still signed in",
                                    "META_API_KEY is set in the environment; unset it and relaunch to sign out.",
                                );
                            });
                        }
                        cx.notify();
                    }
                    None => {
                        this.active = None;
                        this.sessions.clear();
                        this.auth = Auth::SignedOut;
                        this.login.reset_to_choose();
                        cx.notify();
                    }
                },
                Err(error) => {
                    this.set_dialog(cx, Dialog {
                        title: "Sign out failed".into(),
                        detail: error.to_string(),
                        kind: DialogKind::Error,
                        primary: "Dismiss",
                        action: DialogAction::Dismiss,
                        archive_target: None,
                    });
                }
            });
        }));
    }

    /// One `account/*` notification, folded before the session view sees
    /// anything. `account/changed` rebuilds [`Auth`] exactly as
    /// [`Self::probe_account`] does — a signed-in lane while on the login
    /// screen enters the app with no reconnect (D25) — and
    /// `account/loginCompleted` advances the login screen's own state. A
    /// frame that does not decode is stderr and nothing else: the wire owns
    /// the flow, and a malformed outcome must not move the screen.
    fn route_account(&mut self, method: &str, params: &serde_json::Value, cx: &mut Context<Self>) {
        match method {
            "account/changed" => match serde_json::from_value::<AccountState>(params.clone()) {
                Ok(state) => self.apply_account(state, cx),
                Err(error) => crate::harness_log!("ignoring malformed account/changed: {error}"),
            },
            "account/loginCompleted" => {
                match serde_json::from_value::<AccountLoginCompletedParams>(params.clone()) {
                    Ok(completed) => self.on_login_completed(completed, cx),
                    Err(error) => crate::harness_log!("ignoring malformed account/loginCompleted: {error}"),
                }
            }
            _ => {}
        }
    }

    /// The terminal outcome of the running login flow.
    ///
    /// `granted` shows Success (the signed-in `account/changed` that follows
    /// enters the app); `denied` / `expired` / `failed` show the screen for
    /// the running method — the full error card, except an API-key `failed`,
    /// which goes back to the key form so the key can be fixed;
    /// `cancelled` returns to the method choice unless already there.
    fn on_login_completed(&mut self, completed: AccountLoginCompletedParams, cx: &mut Context<Self>) {
        let message = completed.message.filter(|message| !message.trim().is_empty());
        // The outcome and its display message are the server's typed
        // vocabulary — never the URL, the code or a key — so a headless run
        // can be followed from stderr.
        crate::harness_log!(
            "loginCompleted → {}{}",
            completed.outcome.as_wire().unwrap_or("unknown"),
            message.as_deref().map(|m| format!(": {m}")).unwrap_or_default()
        );
        match completed.outcome {
            AccountLoginOutcome::Granted => {
                self.set_login_state(LoginState::Success, cx);
            }
            AccountLoginOutcome::Denied | AccountLoginOutcome::Expired | AccountLoginOutcome::Failed => {
                let fallback = match completed.outcome {
                    AccountLoginOutcome::Denied => "The sign-in request was denied.",
                    AccountLoginOutcome::Expired => "The sign-in request expired before it was approved.",
                    _ => "Sign-in failed.",
                };
                let text: SharedString =
                    message.unwrap_or_else(|| fallback.to_owned()).into();
                // An API-key failure belongs on the key form, where the key
                // can be fixed — not on the error card with its way back.
                if completed.outcome == AccountLoginOutcome::Failed
                    && self.login.method == Some(LoginMethod::ApiKey)
                {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.method },
                        cx,
                    );
                }
            }
            AccountLoginOutcome::Cancelled => {
                if !matches!(self.login.state, LoginState::Choose) {
                    self.login.reset_to_choose();
                    cx.notify();
                }
            }
            // An outcome a newer server invented: the honest card is the
            // method's error, with the server's message when it sent one.
            AccountLoginOutcome::Unknown(_) => {
                let text: SharedString =
                    message.unwrap_or_else(|| "The sign-in ended in a way this build does not understand.".to_owned())
                        .into();
                if self.login.method == Some(LoginMethod::ApiKey) {
                    self.set_login_state(LoginState::ApiKey { can_submit: false, error: Some(text) }, cx);
                } else {
                    self.set_login_state(
                        LoginState::Error { message: text, method: self.login.method },
                        cx,
                    );
                }
            }
        }
    }

    /// `--login-steps <a;b;c>`: drive the login screen from the command line
    /// so a signed-in capture is reproducible without a pointer. Honoured
    /// only when the app is really connected — the offline and replay boots
    /// never call the probe that calls this — one step per item, the same
    /// `;`-separated parsing as `--steps`. Consumed, so a later re-probe
    /// does not replay them.
    fn run_login_steps(&mut self, cx: &mut Context<Self>) {
        let steps = std::mem::take(&mut self.args.login_steps);
        if steps.is_empty() {
            return;
        }
        // Raise the flag the capture waits on, exactly like [`Self::run_steps`]
        // does, so a headless `--screenshot` waits for the login script to
        // finish before the settling delay.
        crate::shot::set_steps_running(true);
        self.tasks.push(cx.spawn(async move |this, cx| {
            for step in steps {
                if let Some(ms) = step.strip_prefix("wait:") {
                    let ms: u64 = ms.parse().unwrap_or(0);
                    cx.background_executor().timer(std::time::Duration::from_millis(ms)).await;
                    continue;
                }
                // `update_in` for the window the field and the focus need.
                let ran = this.update_in(cx, |this, window, cx| this.login_step(&step, window, cx));
                match ran {
                    Ok(true) => {}
                    _ => {
                        crate::shot::set_steps_running(false);
                        return;
                    }
                }
            }
            crate::shot::set_steps_running(false);
        }));
    }

    /// One `--login-steps` verb. Returns whether the run continues; a failed
    /// or unknown step ends it with a stderr line.
    ///
    /// | step | what it does |
    /// |---|---|
    /// | `account` | `LoginIntent::StartAccount` (the device flow; the browser opens) |
    /// | `apikey` | `LoginIntent::UseApiKey` |
    /// | `key-from-env:<VAR>` | put the value of environment variable `VAR` into the API-key field |
    /// | `submit` | `LoginIntent::SubmitApiKey` |
    /// | `wait:<ms>` | let the wire catch up before the next step |
    fn login_step(&mut self, step: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let (head, rest) = step.split_once(':').unwrap_or((step, ""));
        match head {
            "account" => self.login_intent(LoginIntent::StartAccount, window, cx),
            "apikey" => self.login_intent(LoginIntent::UseApiKey, window, cx),
            // The value travels from the environment into the field and then
            // into the wire call: it never appears in argv, a log, or a
            // screenshot argument. An unset variable fails naming the
            // variable, not its value.
            "key-from-env" => match std::env::var(rest) {
                Ok(value) => {
                    let api_key = self.login.api_key.clone();
                    api_key.update(cx, |state, cx| state.set_value(value, window, cx));
                }
                Err(_) => {
                    crate::harness_log!("login step `key-from-env:{rest}` failed: variable is not set");
                    return false;
                }
            },
            "submit" => self.login_intent(LoginIntent::SubmitApiKey, window, cx),
            _ => {
                crate::harness_log!("unknown login step `{step}`");
                return false;
            }
        }
        true
    }

    /// `--no-connect --login <state>`: the login screen's sample data for
    /// captures. The URL, the code and the key-shaped field text are the
    /// example values, never anything the wire sent.
    fn apply_login_sample(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        const URL: &str = "https://example.invalid/device";
        const CODE: &str = "WXYZ-2946";
        // A mask with something behind it: the dots the capture wants.
        const SAMPLE_KEY: &str = "capture-sample-key";
        match self.args.login {
            LoginSample::Choose => {
                self.login.reset_to_choose();
                cx.notify();
            }
            LoginSample::Device => {
                self.login.url = Some(URL.to_owned());
                self.login.code = Some(CODE.to_owned());
                self.login.method = Some(LoginMethod::Account);
                self.set_login_state(
                    LoginState::Device { url: URL.into(), code: CODE.into(), expires: None, waiting: true },
                    cx,
                );
            }
            LoginSample::ApiKey => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::ApiKey { can_submit: true, error: None }, cx);
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(SAMPLE_KEY, window, cx));
            }
            LoginSample::ApiKeyError => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(
                    LoginState::ApiKey {
                        can_submit: true,
                        error: Some("That key was rejected. Check the key and try again.".into()),
                    },
                    cx,
                );
                let api_key = self.login.api_key.clone();
                api_key.update(cx, |state, cx| state.set_value(SAMPLE_KEY, window, cx));
            }
            LoginSample::Validating => {
                self.login.method = Some(LoginMethod::ApiKey);
                self.set_login_state(LoginState::Validating, cx);
            }
            LoginSample::Error => {
                self.login.method = Some(LoginMethod::Account);
                self.set_login_state(
                    LoginState::Error {
                        message: "The sign-in request expired before it was approved.".into(),
                        method: Some(LoginMethod::Account),
                    },
                    cx,
                );
            }
        }
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
                this.rebuild_search_index(cx);
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
                self.open_search(window, cx);
                if !rest.is_empty() {
                    self.search_query.update(cx, |state, cx| state.set_value(rest.to_owned(), window, cx));
                }
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
            // A scripted width for the resize screenshots: clamped and
            // settled exactly like a released drag, minus the pointer.
            "sidebar-width" => {
                if let Ok(width) = rest.parse::<f32>() {
                    self.sidebar_width = clamp_sidebar_width(width);
                    self.resizing = false;
                    self.persist_width();
                }
                cx.notify();
            }
            "sidebar" => self.toggle_sidebar(cx),
            "overflow" => self.open_menu(MenuKind::Overflow, cx),
            "view-menu" => self.open_menu(MenuKind::ViewOptions, cx),
            "account" => self.open_menu(MenuKind::Account, cx),
            "pin" => {
                if let Some(session_id) = self.active_id(cx) {
                    self.toggle_pin(session_id, cx);
                }
            }
            "archive" => {
                if let Some(session_id) = self.active_id(cx) {
                    self.open_archive_dialog(session_id, cx);
                }
            }
            "archive-confirm" => self.confirm_archive_dialog(window, cx),
            "show-archived" => {
                self.show_archived = !self.show_archived;
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
            // Same rule as [`SessionEntry::join`]: a user-given name always
            // earns the first prompt below it, any other label only when it
            // does not already say it.
            let user_named = name.is_some()
                || index
                    .and_then(|i| i.session_name.as_deref())
                    .map(str::trim)
                    .is_some_and(|s| !s.is_empty());
            let text = label.unwrap_or(crate::sidebar::UNNAMED);
            entry.needs_title = label.is_none();
            // A replayed capture names its own row by file, and no source
            // speaks for it: keep that label rather than blanking it to the
            // fallback on every override write.
            match label {
                Some(label) => entry.label = label.to_owned(),
                None if !entry.replayed => entry.label = crate::sidebar::UNNAMED.to_owned(),
                None => {}
            }
            entry.hidden = meta.is_some_and(|m| m.hidden);
            entry.pinned = meta.is_some_and(|m| m.pinned);
            entry.archived = meta.is_some_and(|m| m.archived);
            entry.description = sidebar::describe(meta, index, text, user_named);
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
        // would be a list that means nothing. Archived sessions are the same.
        if self.overrides.get(&session_id).is_some_and(|m| m.hidden) && !self.show_hidden {
            return;
        }
        if self.overrides.get(&session_id).is_some_and(|m| m.archived) && !self.show_archived {
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
        view.update(cx, |view, cx| view.load_history(cx));
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
                view.set_at_rest(self.still());
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
            view.set_at_rest(self.still());
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
                    archive_target: None,
                });
            }
            SessionEvent::SignedOut { message } => {
                self.set_dialog(cx, Dialog {
                    title: "Signed out of Muse".into(),
                    detail: format!("Muse refused the turn: {message}"),
                    kind: DialogKind::Warning,
                    primary: "Sign in",
                    action: DialogAction::SignIn,
                    archive_target: None,
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
                    archive_target: None,
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
            SessionEvent::Search => {
                self.tasks.push(cx.spawn(async move |this, cx| {
                    let _ = this.update_in(cx, |this, window, cx| this.open_search(window, cx));
                }));
            }
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
            if files.truncated {
                crate::harness_log!(
                    "@ mention index stopped at {} files; some workspace files are not mentionable",
                    files::CAP
                );
            }
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| {
                    overlays.skills = skills;
                    overlays.files = files.entries;
                    overlays.files_truncated = files.truncated;
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
            archive_target: None,
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
        self.rebuild_search_index(cx);
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
        self.push_undo(
            UndoBatch::Hidden(ids),
            title,
            detail.to_owned(),
            cx,
        );
        cx.notify();
    }

    /// One toast with one Undo for one undoable batch, and a timer that takes
    /// both away together, so a press after the toast has gone does nothing.
    fn push_undo(&mut self, batch: UndoBatch, title: String, detail: String, cx: &mut Context<Self>) {
        let toast =
            self.overlays.update(cx, |overlays, _| overlays.toast_with_action(title, &detail, "Undo"));
        let undo = batch.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(UNDO_WINDOW).await;
            let _ = this.update(cx, |this, cx| {
                this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&toast));
                this.undo_stack.retain(|batch| *batch != undo);
                cx.notify();
            });
        }));
        self.undo_stack.push(batch);
        cx.notify();
    }

    /// The toast's Undo: put the newest batch back — one hidden row, one
    /// "Clear empty" whole, or one archived session.
    fn undo_newest(&mut self, cx: &mut Context<Self>) {
        let Some(batch) = self.undo_stack.pop() else { return };
        match batch {
            UndoBatch::Hidden(ids) => {
                for session_id in ids {
                    self.set_override(&session_id, |meta| meta.hidden = false, cx);
                }
            }
            UndoBatch::Archived(ids) => {
                for session_id in ids {
                    self.set_override(&session_id, |meta| meta.archived = false, cx);
                }
            }
        }
    }

    /// "Clear empty": hide every session with no turns, with a way back for
    /// eight seconds. Rows already hidden — and archived rows, which Clear
    /// must never sweep — stay out of the batch, so Undo restores exactly
    /// what this hid and nothing it did not.
    fn clear_empty(&mut self, cx: &mut Context<Self>) {
        let active = self.active_id(cx);
        let cleared: Vec<String> = self
            .sessions
            .iter()
            .filter(|entry| !entry.hidden && !entry.archived && entry.is_empty(active.as_deref()))
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

    /// Pin or unpin a session. Purely local: the list regroups around it
    /// and the store keeps it.
    fn toggle_pin(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.set_override(&session_id, |meta| meta.pinned = !meta.pinned, cx);
    }

    /// Ask before archiving: a danger dialog carrying its target, so only its
    /// own Archive button can confirm it.
    fn open_archive_dialog(&mut self, session_id: String, cx: &mut Context<Self>) {
        let label = self
            .sessions
            .iter()
            .find(|e| e.id == session_id)
            .map(|e| e.label.clone())
            .unwrap_or_else(|| sidebar::UNNAMED.to_owned());
        self.set_dialog(cx, Dialog {
            title: format!("Archive \"{label}\"?"),
            detail: "Archived sessions stay on disk and can be shown from the Sessions menu.".into(),
            kind: DialogKind::Warning,
            primary: "Archive",
            action: DialogAction::Archive,
            archive_target: Some(session_id),
        });
    }

    /// The archive dialog's Archive button, or the `archive-confirm` step.
    fn confirm_archive_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = self.overlays.read(cx).dialog.as_ref().and_then(|d| {
            (d.action == DialogAction::Archive).then(|| d.archive_target.clone()).flatten()
        });
        let Some(session_id) = target else { return };
        self.close_dialog(cx);
        self.archive_session(session_id, Some(window), cx);
    }

    /// Archive a session out of the list, with a way back for eight seconds.
    ///
    /// An archived session is never loaded, so the active one closes when it
    /// is the one archived: the newest remaining visible session opens in its
    /// place, or the empty state when nothing remains.
    fn archive_session(&mut self, session_id: String, window: Option<&mut Window>, cx: &mut Context<Self>) {
        let was_active = self.active.as_ref().is_some_and(|a| a.read(cx).session_id == session_id);
        self.set_override(&session_id, |meta| meta.archived = true, cx);
        if was_active {
            self.active = None;
        }
        self.push_undo(
            UndoBatch::Archived(vec![session_id]),
            "Session archived".to_owned(),
            "It is still on disk; show it again from the Sessions menu.".to_owned(),
            cx,
        );
        // The newest remaining visible session opens in place of the archived
        // one; with no window (a step, not a click) the empty state stays
        // until the person picks a session.
        if was_active {
            if let Some(window) = window {
                let next = self.visible_sessions(cx).into_iter().next().map(|e| e.id.clone());
                if self.client.is_some() {
                    if let Some(id) = next {
                        self.resume(id, window, cx);
                    }
                }
            }
        }
        cx.notify();
    }

    /// Put a session back in the list (the Archive tray action on an archived
    /// row, or the toast's Undo through [`Self::undo_newest`]).
    fn unarchive_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.set_override(&session_id, |meta| meta.archived = false, cx);
    }

    /// A turn completed in this app: leave the first line of its last
    /// assistant text on the sidebar row. Free — the fold is in memory — and
    /// skipped when nothing new arrived, so the store is not rewritten on
    /// every completion.
    fn record_last_summary(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.active.clone() else { return };
        let (session_id, summary) = (view.read(cx).session_id.clone(), view.read(cx).last_summary_text());
        let Some(summary) = summary else { return };
        if self.overrides.get(&session_id).and_then(|m| m.last_summary.as_deref()) == Some(summary.as_str()) {
            return;
        }
        self.set_override(&session_id, |meta| meta.last_summary = Some(summary), cx);
    }

    /// The open session's id, which the empty filter never applies to: a
    /// session just created has no turns yet and must stay visible.
    fn active_id(&self, cx: &gpui::App) -> Option<String> {
        self.active.as_ref().map(|a| a.read(cx).session_id.clone())
    }

    /// The rows the sidebar should draw: hidden ones out unless asked for,
    /// archived ones out unless asked for, sessions with no turns out unless
    /// asked for. Text search lives in the search palette (⌘⇧F), never in a
    /// sidebar field, so no needle applies here. The open session is always
    /// drawn.
    fn visible_sessions(&self, cx: &gpui::App) -> Vec<SessionEntry> {
        let active = self.active_id(cx);
        let mut rows: Vec<SessionEntry> = self
            .sessions
            .iter()
            .filter(|entry| self.show_hidden || !entry.hidden)
            .filter(|entry| self.show_archived || !entry.archived)
            .filter(|entry| {
                // An archived row shown on request is explicitly asked for;
                // the empty filter must not swallow it back.
                (self.show_archived && entry.archived)
                    || self.show_empty
                    || !entry.is_empty(active.as_deref())
            })
            .cloned()
            .collect();
        // Newest first. The sidebar's grouping sorts for itself; the palette
        // takes the head of this list, so the order has to be right here.
        rows.sort_by_key(|entry| std::cmp::Reverse(entry.updated));
        rows
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
                        crate::harness_log!("session/read for a title failed: {error}");
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

    /// Open the full-text search palette and put the keyboard in its query.
    ///
    /// Reached from the sidebar search icon, Cmd+Shift+F and `/search`; the
    /// empty query lists recent sessions and recently created files, so the
    /// sidebar's quick-filter is still one keypress away.
    fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlays.update(cx, |overlays, _| {
            overlays.palette = Some(Palette { kind: PaletteKind::Search, selected: 0 });
        });
        self.search_query.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        self.search_sessions.clear();
        self.search_files.clear();
        self.refresh_search(cx);
        window.focus(&self.search_query.focus_handle(cx), cx);
        cx.notify();
    }

    /// Re-query `search.db` off the UI thread, latest keystroke wins.
    ///
    /// A no-op unless the search palette is open: typing anywhere else must
    /// not touch the disk.
    fn refresh_search(&mut self, cx: &mut Context<Self>) {
        if !self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search) {
            return;
        }
        self.search_epoch += 1;
        let epoch = self.search_epoch;
        let query = self.search_query.read(cx).value().to_string();
        let call = cx.background_spawn(async move {
            match crate::search::open() {
                Ok(connection) => (
                    crate::search::query_sessions(&connection, &query, crate::search::LIMIT),
                    crate::search::query_files(&connection, &query, crate::search::LIMIT),
                ),
                Err(_) => (Vec::new(), Vec::new()),
            }
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            let (sessions, files) = call.await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                if this.search_epoch != epoch {
                    return;
                }
                this.search_sessions = sessions;
                this.search_files = files;
                // The selection may point past the new list.
                this.overlays.update(cx, |overlays, _| {
                    if let Some(palette) = overlays.palette.as_mut() {
                        palette.selected = 0;
                    }
                });
                cx.notify();
            });
        }));
    }

    /// Rebuild the session half of `search.db` off the UI thread.
    ///
    /// Runs at boot and after each index refresh; the files half is never
    /// touched here, so recorded files survive a rebuild. When the rebuild
    /// lands while the palette is open, the open query runs again against
    /// the fresh index.
    fn rebuild_search_index(&mut self, cx: &mut Context<Self>) {
        let rows: Vec<crate::search::SessionRow> = self
            .index
            .iter()
            .map(|(session_id, entry)| {
                let meta = self.overrides.get(session_id);
                let name =
                    meta.and_then(|m| m.name.as_deref()).map(str::trim).filter(|s| !s.is_empty());
                let derived = meta
                    .and_then(|m| m.derived_title.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty());
                let label =
                    name.or_else(|| entry.label()).or(derived).unwrap_or(crate::sidebar::UNNAMED);
                crate::search::SessionRow {
                    session_id: session_id.clone(),
                    label: label.to_owned(),
                    title: entry.title.clone(),
                    first_prompt: entry.first_user_prompt.clone().unwrap_or_default(),
                    body: entry.search_text.clone(),
                }
            })
            .collect();
        let call = cx.background_spawn(async move {
            let mut connection = match crate::search::open() {
                Ok(connection) => connection,
                Err(_) => return,
            };
            let _ = crate::search::rebuild_sessions(&mut connection, &rows);
        });
        self.tasks.push(cx.spawn(async move |this, cx| {
            call.await;
            let _ = this.update(cx, |this: &mut Self, cx| {
                this.refresh_search(cx);
            });
        }));
    }

    /// The search palette's rows: session hits, then file hits, in the order
    /// the palette draws them so the keyboard and the click agree.
    ///
    /// Each id carries its section (`s:<session>` or `f:<session>:<path>`).
    /// An empty query is recent sessions from the sidebar order plus recently
    /// recorded files.
    fn search_rows(&self, cx: &gpui::App) -> Vec<(SharedString, SharedString, SharedString)> {
        let mut rows = Vec::new();
        if self.search_query.read(cx).value().trim().is_empty() {
            for entry in self.visible_sessions(cx).into_iter().take(PALETTE_ROWS) {
                rows.push((
                    format!("s:{}", entry.id).into(),
                    entry.label.clone().into(),
                    SharedString::from("recent"),
                ));
            }
        } else {
            for hit in &self.search_sessions {
                let detail =
                    if hit.snippet.is_empty() { SharedString::from("match") } else { hit.snippet.clone().into() };
                rows.push((format!("s:{}", hit.session_id).into(), hit.label.clone().into(), detail));
            }
        }
        for hit in &self.search_files {
            let label = self
                .sessions
                .iter()
                .find(|entry| entry.id == hit.session_id)
                .map(|entry| entry.label.clone())
                .unwrap_or_else(|| "created file".to_owned());
            rows.push((format!("f:{}:{}", hit.session_id, hit.path).into(), hit.path.clone().into(), label.into()));
        }
        rows
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
            PaletteKind::Search => self.search_rows(cx),
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
            PaletteKind::Search => {
                if let Some(session_id) = id.strip_prefix("s:") {
                    self.resume(session_id.to_owned(), window, cx);
                } else if let Some(rest) = id.strip_prefix("f:") {
                    self.reveal_created(rest, cx);
                }
            }
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

    /// Whether this window draws settled rather than entering: a
    /// `--screenshot` run, or a deterministic capture
    /// (`HARNESS_DETERMINISTIC=1`), which is always a static composition even
    /// without a screenshot on the end.
    fn still(&self) -> bool {
        self.args.screenshot.is_some() || crate::clock::deterministic()
    }

    fn render_login(&self, cx: &mut Context<Self>) -> AnyElement {
        let intent = cx.listener(|this: &mut Self, intent: &LoginIntent, window, cx| {
            this.login_intent(*intent, window, cx);
        });
        // `can_submit` tracks the field's non-empty trimmed text, recomputed
        // every frame; the stored bool is only the shape the state needs.
        let state = match &self.login.state {
            LoginState::ApiKey { error, .. } => LoginState::ApiKey {
                can_submit: !self.login.api_key.read(cx).value().trim().is_empty(),
                error: error.clone(),
            },
            other => other.clone(),
        };
        // The eye flips the field's masked flag and keeps `revealed` in sync,
        // so `ToggleReveal` flips from the truth. The component never sees
        // the key: it only reads the masked flag for the glyph.
        let harness = cx.entity().downgrade();
        let toggle = self.login.api_key.clone();
        let field = secret_field("login-key", &self.login.api_key)
            .placeholder("Paste your key")
            .on_toggle_reveal(move |window, cx| {
                let next = !toggle.read(cx).presentation().is_masked();
                toggle.update(cx, |state, cx| state.set_masked(next, window, cx));
                let _ = harness.update(cx, |this, _| this.login.revealed = next);
            });
        // A deterministic capture draws the card settled: the login screen's
        // enter presence never lands on the same frame twice.
        let card = login("login", state)
            .product("Muse")
            .headline("Sign in to Muse")
            .subtitle("The harness signs in over the wire, the same way the muse CLI does.")
            .provider(aui_icons::Provider::Muse)
            .api_key_field(field);
        let card = if crate::clock::deterministic() { card.at_rest() } else { card };
        card.on_intent(move |i, window, cx| intent(&i, window, cx)).into_any_element()
    }

    /// Open a header/footer menu, replacing whatever is open. Clicking its
    /// own button again closes it.
    fn open_menu(&mut self, kind: MenuKind, cx: &mut Context<Self>) {
        let already = self.overlays.read(cx).menu.as_ref().is_some_and(|m| m.kind == kind);
        self.overlays.update(cx, |overlays, _| {
            overlays.menu = if already { None } else { Some(Menu::picker(kind, 0)) };
        });
        cx.notify();
    }

    /// The two rows above the Sessions caption: New session, and Automations
    /// behind a Soon tag until it has somewhere to go.
    fn render_nav_block(&self, cx: &mut Context<Self>) -> AnyElement {
        let new_session = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.new_session(cx));
        let automations = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.overlays.update(cx, |overlays, _| {
                overlays.toast("Automations", "Automations are not wired up yet.");
            });
            cx.notify();
        });
        v_flex()
            .w_full()
            .flex_none()
            .px(px(scale::SP_2))
            .pt(px(scale::SP_2))
            .child(nav_item("nav-new", IconName::Plus, "New session").on_click(new_session))
            .child(nav_item("nav-automations", IconName::Zap, "Automations").count("Soon").on_click(automations))
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
                RowAction::Pin => this.toggle_pin(id.to_string(), cx),
                // The tray carries one archive affordance: on a listed session
                // it asks first, on an archived one it puts it straight back.
                RowAction::Archive => {
                    let archived =
                        this.sessions.iter().find(|e| e.id == id.as_ref()).is_some_and(|e| e.archived);
                    if archived {
                        this.unarchive_session(id.to_string(), cx);
                    } else {
                        this.open_archive_dialog(id.to_string(), cx);
                    }
                }
                _ => {}
            }
        });
        // The sliders icon toggles the view menu like every other popover.
        let open_view = cx.listener(|this: &mut Self, _: &(), _, cx| {
            this.open_menu(MenuKind::ViewOptions, cx);
        });
        let mut view = sidebar_view("sessions", grouping)
            .caption("Sessions")
            .on_view_options(move |w, cx| open_view(&(), w, cx))
            .row_actions(vec![RowAction::Pin, RowAction::Rename, RowAction::Archive])
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |id, action, w, cx| act(&(id.clone(), action), w, cx));
        if let Some(renaming) = self.renaming.clone() {
            view = view.editing(renaming, self.rename_field(window, cx));
        }
        if let Some(selected) = selected {
            view = view.selected(selected);
        }
        // No quick-filter field: ⌘⇧F and the sidebar search icon open the
        // full-text search palette instead, so the two can never share the
        // sidebar.
        v_flex()
            .size_full()
            .child(self.render_nav_block(cx))
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

    /// The field the row being renamed holds: the library's dense recipe —
    /// a borderless, chromeless single line at the row-title size, with the
    /// 1 px focus border on the wrapper instead of the component. The wrapper
    /// is a flex row centring its child, and its height is whatever the
    /// editor's own line-height makes it: the old fixed 22 px box cropped the
    /// glyphs at the top. Clipping is horizontal only, so a long name scrolls
    /// under the caret instead of spilling a second line, and the row keeps
    /// its own height while a rename is open, so siblings never move. The
    /// commit path is unchanged. The same element serves the sidebar row and
    /// the header title.
    fn rename_field(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let focused = self.rename.focus_handle(cx).is_focused(window);
        div()
            .w_full()
            .key_context(RENAME_CONTEXT)
            .on_action(cx.listener(|this, _: &ConfirmRename, window, cx| this.commit_rename(window, cx)))
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .overflow_x_hidden()
                    .px(px(6.0))
                    .rounded(px(scale::R_SM))
                    .border_1()
                    .border_color(if focused { p.accent } else { p.line })
                    .bg(p.surface_1)
                    .child(dense_field(&self.rename).h_auto().whitespace_nowrap().overflow_x_hidden()),
            )
            .into_any_element()
    }

    /// What the list says when it has nothing to show, and why (spec §5).
    fn render_sidebar_empty(&self, visible: &[SessionEntry], cx: &mut Context<Self>) -> Option<AnyElement> {
        if !visible.is_empty() {
            return None;
        }
        let p = cx.aui().colors;
        let active = self.active_id(cx);
        // What each filter alone is keeping out, past the other one: the
        // empty text names its own toggle rather than borrowing hidden's.
        // (There is no sidebar text filter — search lives in the palette —
        // so no "no match" state exists here.)
        let hidden_only = !self.show_hidden && self.sessions.iter().any(|e| e.hidden);
        let empty_only = !self.show_empty
            && self
                .sessions
                .iter()
                .any(|e| (self.show_hidden || !e.hidden) && e.is_empty(active.as_deref()));
        let (title, detail) = match (hidden_only, empty_only) {
            (true, _) => {
                ("Every session here is hidden", "Turn on \u{201c}Show hidden\u{201d} in the Sessions menu above.")
            }
            (false, true) => {
                ("Only empty sessions here", "Turn on \u{201c}Show empty\u{201d} in the Sessions menu above.")
            }
            (false, false) => ("No sessions yet", "\u{2318}N starts one."),
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

    /// "Signed in as", in the library's shape: avatar, name, the email it is
    /// really reporting, the plan row, and the provider usage meter with the
    /// chevron. The whole footer opens the account menu — Sign out lives
    /// there now, and the list-management toggles live in the Sessions menu.
    fn render_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Auth::SignedIn(identity) = &self.auth else {
            return div().into_any_element();
        };
        let account = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| {
            this.open_menu(MenuKind::Account, cx);
        });
        let mut footer = sidebar_footer("account", identity.initial(), identity.footer_name())
            .on_click(move |e, w, cx| account(e, w, cx));
        if !identity.email.is_empty() {
            footer = footer.detail(identity.email.clone());
        }
        // The third row: what this login is entitled to. Warning-tinted for
        // anything that is not a plan in force, because that is the case where
        // the next turn costs money nobody expected. The key lanes say so
        // without a probe: a stored key or `META_API_KEY` is pay-as-you-go by
        // construction.
        let meter = self.tier.as_ref().and_then(|tier| tier.weekly_fraction());
        // `--tier` fakes the probe it names: the footer reads the faked tier
        // like any other probe answer, even on the key lanes.
        if self.args.tier.is_some() {
            if let Some(tier) = &self.tier {
                footer = footer.plan(tier.footer_label(), tier.is_warning());
            }
        } else if identity.is_api_key() {
            footer = footer.plan("Pay-as-you-go · API key", true);
        } else if let Some(tier) = &self.tier {
            footer = footer.plan(tier.footer_label(), tier.is_warning());
        }
        // The meter is the weekly fraction the probe already reports; with no
        // reading there is no meter. Either way the chevron stands, so the
        // account menu stays discoverable — the row's own click opens it too.
        if let Some(fraction) = meter {
            footer = footer.meter(Provider::Muse, fraction);
        } else {
            footer = footer.trailing(
                icon_button("account-chevron", IconName::ChevronDown)
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon_size(px(12.0)),
            );
        }
        footer.into_any_element()
    }

    /// The centre header: the active session's label ("Harness" with nothing
    /// open), the provider mark, and the overflow menu — and nothing else.
    /// The library's `centre_header` always paints the right-pane toggle and
    /// the right header always paints its close button, so the shell gets a
    /// plain cell with the same title construction instead. The shell's own
    /// drag region wraps the whole header row, and buttons keep their clicks.
    fn render_centre_header(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        let label = self
            .active
            .as_ref()
            .and_then(|view| {
                let id = view.read(cx).session_id.clone();
                self.sessions.iter().find(|e| e.id == id).map(|e| e.label.clone())
            })
            .unwrap_or_else(|| "Harness".to_owned());
        let overflow =
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.open_menu(MenuKind::Overflow, cx));
        let expand = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.toggle_sidebar(cx));
        // The title flexes inside the header cell and clips to one line, so
        // a whole first prompt as the derived title can never push the
        // overflow button out; the provider mark is flex-none so it stays
        // painted. There is no width token in aui-tokens, so the flex
        // leftover — not a fixed max — is the constraint, which also holds
        // on narrow windows.
        let title = h_flex()
            .flex_1()
            .min_w(px(0.0))
            .overflow_hidden()
            .gap(px(7.0))
            .text_color(p.ink)
            .ui(scale::FS_13)
            .semibold()
            .child(div().flex_none().child(provider_mark(Provider::Muse)))
            .child(div().flex_1().min_w(px(0.0)).truncate().child(label));
        // Renaming the open session swaps the header title for the same
        // dense field the sidebar row uses (Task A); the commit path is the
        // same `ConfirmRename`, Escape the same `cancel`.
        let renaming_here = self
            .active
            .as_ref()
            .map(|view| view.read(cx).session_id.clone())
            .is_some_and(|id| self.renaming.as_deref() == Some(id.as_str()));
        let title: AnyElement = if renaming_here {
            // The field's own root is `w_full`, so the flex item clips: the
            // overflow button keeps its slot instead of being pushed out.
            div()
                .flex_1()
                .min_w(px(0.0))
                .overflow_hidden()
                .child(self.rename_field(window, cx))
                .into_any_element()
        } else {
            title.into_any_element()
        };
        let _ = window;
        let mut cell = header_cell("hd-centre");
        if !self.sidebar_open {
            cell = cell.child(
                icon_button("hd-centre-expand", IconName::Sidebar)
                    .ghost()
                    .muted()
                    .size(ButtonSize::Sm)
                    .on_click(expand),
            );
            // The native lights own x 9–61 whether the sidebar is open or
            // not; with the rail at 48 px the centre cell starts underneath
            // them, so the title stands this far off.
            cell = cell.child(div().w(px(14.0)).flex_none());
        }
        cell
            .child(title)
            .child(
                icon_button("hd-centre-overflow", IconName::Dots)
                    .ghost()
                    .muted()
                    .size(ButtonSize::Sm)
                    .on_click(overflow),
            )
            .into_any_element()
    }

    /// The collapsed rail: new-session and search cells, a separator, one dot
    /// per running session mirroring the rows, and the account avatar.
    fn render_rail(&self, cx: &mut Context<Self>) -> AnyElement {
        let active = self.active_id(cx);
        let mut items = vec![
            RailItem::nav("new", IconName::Plus),
            RailItem::nav("search", IconName::Search),
            RailItem::separator(),
        ];
        for entry in &self.sessions {
            if entry.hidden || entry.archived || !entry.running {
                continue;
            }
            let mut cell = RailItem::session(entry.id.clone(), AgentState::Running).pulse();
            if active.as_deref() == Some(entry.id.as_str()) {
                cell = cell.selected(true);
            }
            items.push(cell);
        }
        let mut rail = rail("rail", items).flat(true);
        if let Auth::SignedIn(identity) = &self.auth {
            rail = rail.avatar(identity.initial());
        }
        let select = cx.listener(|this: &mut Self, id: &SharedString, window, cx| {
            this.resume(id.to_string(), window, cx);
        });
        let action = cx.listener(|this: &mut Self, name: &str, window, cx| match name {
            "new" => this.new_session(cx),
            // Task E has landed: the rail cell opens the full-text search
            // palette, like the header search icon and ⌘⇧F.
            "search" => this.open_search(window, cx),
            "account" => this.open_menu(MenuKind::Account, cx),
            _ => {}
        });
        rail
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_action(move |name, w, cx| action(name, w, cx))
            .into_any_element()
    }

    /// The header's overflow menu, anchored under the "…" button: Rename swaps
    /// the title for the dense inline field, Fork opens the fork picker, and
    /// Archive asks first through the archive dialog.
    fn render_overflow_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Overflow) {
            return None;
        }
        let rows = vec![
            MenuRow::Toggle { label: "Rename".into(), checked: false },
            MenuRow::Toggle { label: "Fork".into(), checked: false },
            MenuRow::Toggle { label: "Archive".into(), checked: false },
        ];
        let actions = [OverflowAction::Rename, OverflowAction::Fork, OverflowAction::Archive];
        let activate = cx.listener(move |this: &mut Self, index: &usize, window, cx| {
            let action = actions.get(*index).copied();
            this.overlays.update(cx, |overlays, _| overlays.menu = None);
            match action {
                Some(OverflowAction::Rename) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.sidebar_open = true;
                        this.start_rename(session_id, window, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to rename", "No session is open.");
                        });
                    }
                    cx.notify();
                }
                Some(OverflowAction::Fork) => this.open_palette(PaletteKind::Fork, cx),
                Some(OverflowAction::Archive) => {
                    if let Some(session_id) = this.active_id(cx) {
                        this.open_archive_dialog(session_id, cx);
                    } else {
                        this.overlays.update(cx, |overlays, _| {
                            overlays.toast("Nothing to archive", "No session is open.");
                        });
                        cx.notify();
                    }
                }
                None => {}
            }
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .top(px(48.0))
                    .right(px(8.0))
                    .child(view_menu("overflow", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
    }

    /// The Sessions caption's view menu: where list management lives now that
    /// the footer is the library's account row again.
    fn render_view_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::ViewOptions) {
            return None;
        }
        let active = self.active_id(cx);
        let hidden = self.sessions.iter().filter(|e| e.hidden).count();
        let empty = self.sessions.iter().filter(|e| !e.archived && e.is_empty(active.as_deref())).count();
        let archived = self.sessions.iter().filter(|e| e.archived).count();
        let mut rows: Vec<MenuRow> = Vec::new();
        let mut actions: Vec<Option<ViewAction>> = Vec::new();
        // Either toggle only appears once it has something to show: an
        // affordance for an empty set is a question nobody asked.
        if empty > 0 || self.show_empty {
            let label = if self.show_empty {
                "Hide empty".to_owned()
            } else {
                format!("Show empty ({empty})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_empty });
            actions.push(Some(ViewAction::ToggleEmpty));
        }
        if hidden > 0 || self.show_hidden {
            let label = if self.show_hidden {
                "Hide hidden".to_owned()
            } else {
                format!("Show hidden ({hidden})")
            };
            rows.push(MenuRow::Toggle { label: label.into(), checked: self.show_hidden });
            actions.push(Some(ViewAction::ToggleHidden));
        }
        if empty > 0 {
            rows.push(MenuRow::Toggle { label: "Clear empty".into(), checked: false });
            actions.push(Some(ViewAction::ClearEmpty));
        }
        if !rows.is_empty() {
            rows.push(MenuRow::Separator);
            actions.push(None);
        }
        let archived_label = if self.show_archived {
            "Hide archived".to_owned()
        } else {
            format!("Show archived ({archived})")
        };
        rows.push(MenuRow::Toggle { label: archived_label.into(), checked: self.show_archived });
        actions.push(Some(ViewAction::ToggleArchived));
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            match actions.get(*index).copied().flatten() {
                // Toggles keep the menu open, so the check is seen to change.
                Some(ViewAction::ToggleEmpty) => {
                    this.show_empty = !this.show_empty;
                    cx.notify();
                }
                Some(ViewAction::ToggleHidden) => {
                    this.show_hidden = !this.show_hidden;
                    cx.notify();
                }
                Some(ViewAction::ToggleArchived) => {
                    this.show_archived = !this.show_archived;
                    cx.notify();
                }
                Some(ViewAction::ClearEmpty) => {
                    this.overlays.update(cx, |overlays, _| overlays.menu = None);
                    this.clear_empty(cx);
                }
                None => {}
            }
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .top(px(140.0))
                    .left(px(12.0))
                    .child(view_menu("sessions-view", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
    }

    /// The footer's account menu: Sign out, and nothing else. The environment
    /// lane names itself: `META_API_KEY` survives a sign-out, so the row says
    /// where the credential really comes from (D28).
    fn render_account_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).is_open(MenuKind::Account) {
            return None;
        }
        let label = match &self.auth {
            Auth::SignedIn(identity) if identity.lane == AccountStateKind::EnvKey => {
                "Sign out (set by META_API_KEY)"
            }
            _ => "Sign out",
        };
        let rows = vec![MenuRow::Toggle { label: label.into(), checked: false }];
        let activate = cx.listener(move |this: &mut Self, index: &usize, _, cx| {
            if *index == 0 {
                this.overlays.update(cx, |overlays, _| overlays.menu = None);
                this.logout(cx);
            }
        });
        Some(
            popover_layer(
                div()
                    .absolute()
                    .bottom(px(100.0))
                    .left(px(12.0))
                    .child(view_menu("account", rows).at_rest().on_activate(move |i, w, cx| activate(&i, w, cx))),
            )
            .into_any_element(),
        )
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
        let at_rest = self.still();
        let (provider, workspace) = (self.args.provider.clone(), self.workspace());
        let overlays = self.overlays.clone();
        // The capture names its own session; this id is a placeholder the view
        // replaces the moment the first line is folded.
        let view = cx.new(|cx| {
            let mut view = SessionView::new("replay".to_owned(), None, provider, workspace, overlays, window, cx);
            view.set_at_rest(at_rest);
            view.load_replay(&path, cx);
            view.load_history(cx);
            view
        });
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        self.sessions = vec![SessionEntry::replayed(&view.read(cx).session_id, &path)];
        // A replayed window has no wire, but the search palette still needs
        // the host's session index: read it (read-only) and rebuild `search.db`
        // so `--steps search:<query>` screenshots show session hits.
        self.load_index(cx);
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
                // A deterministic capture never takes keyboard focus: a
                // focused composer paints the textarea's blinking caret,
                // which lands on a different phase every run.
                if std::mem::take(&mut self.focus_composer) && !crate::clock::deterministic() {
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

    /// Reveal a created file from the search palette in Finder.
    ///
    /// `rest` is `<session_id>:<workspace-relative path>`. A path that no
    /// longer exists is a toast, not a reveal of whatever happens to sit at
    /// the workspace root.
    fn reveal_created(&mut self, rest: &str, cx: &mut Context<Self>) {
        let Some((_, path)) = rest.split_once(':') else { return };
        let full = self.args.workspace.join(path);
        if !full.is_file() {
            self.overlays.update(cx, |overlays, _| {
                overlays.toast("File not found", format!("{path} is no longer in this workspace."));
            });
            cx.notify();
            return;
        }
        cx.reveal_path(&full);
    }

    /// The search card's status line: what the query found, or what an
    /// empty query offers.
    fn search_status(&self, cx: &gpui::App) -> String {
        if self.search_query.read(cx).value().trim().is_empty() {
            return "Search sessions and created files".to_owned();
        }
        let (sessions, files) = (self.search_sessions.len(), self.search_files.len());
        if sessions + files == 0 {
            return "No matches".to_owned();
        }
        let mut parts = Vec::new();
        if sessions > 0 {
            parts.push(format!("{sessions} session{}", if sessions == 1 { "" } else { "s" }));
        }
        if files > 0 {
            parts.push(format!("{files} file{}", if files == 1 { "" } else { "s" }));
        }
        parts.join(" \u{00b7} ")
    }

    /// The search palette's query field, above the card. The card's own query
    /// row is display-only, so the palette needs a real field to type in;
    /// clearing it returns to the empty state (recents).
    fn render_search_input(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search) {
            return None;
        }
        let query = self.search_query.read(cx).value().to_string();
        let clear = cx.listener(|this: &mut Self, _: &gpui::ClickEvent, window, cx| {
            this.search_query.update(cx, |state, cx| state.set_value(String::new(), window, cx));
        });
        Some(
            sidebar_search("palette-search", Textarea::new(&self.search_query).text_size(aui_tokens::scaled(scale::FS_12)))
                .clearable(!query.is_empty())
                .on_clear(clear)
                .into_any_element(),
        )
    }

    /// ⌘K and `/resume`: the command palette, over everything.
    ///
    /// The same primitive for both lists, because they are the same gesture —
    /// a list, an arrow key and a return — and a second picker would be a
    /// second set of keys to learn.
    fn render_palette(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, selected) = self.overlays.read(cx).palette.as_ref().map(|p| (p.kind, p.selected))?;
        let rows = self.palette_rows(kind, cx);
        // The card's own query row mirrors the query for the picking lists;
        // the search palette edits through its own field above the card, so
        // the card's row carries the result count instead.
        let (query, placeholder, sections) = match kind {
            PaletteKind::Commands => (
                SharedString::from(""),
                SharedString::from("Every command in this build"),
                vec![PaletteSection::new(
                    "Commands",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Slash)),
                )],
            ),
            PaletteKind::Resume => (
                SharedString::from(""),
                SharedString::from("Resume a session in this workspace"),
                vec![PaletteSection::new(
                    "Sessions",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Clock)),
                )],
            ),
            PaletteKind::Fork => (
                SharedString::from(""),
                SharedString::from("Pick a completed turn to branch from"),
                vec![PaletteSection::new(
                    "Fork from",
                    palette_items(&rows, PaletteIcon::Glyph(IconName::Git)),
                )],
            ),
            PaletteKind::Search => {
                let (sessions, files): (Vec<_>, Vec<_>) =
                    rows.iter().partition(|(id, _, _)| id.starts_with("s:"));
                // The row's own match emphasis covers the label only — the
                // library paints `matched` ranges on the label and the
                // context (the snippet) stays muted mono. Primary text is
                // the sidebar label either way; the snippet is display-only.
                let query = self.search_query.read(cx).value().to_string();
                let mut sections = Vec::new();
                if !sessions.is_empty() {
                    sections.push(PaletteSection::new(
                        "Sessions",
                        palette_items_ref_matching(&sessions, PaletteIcon::Glyph(IconName::Clock), &query),
                    ));
                }
                if !files.is_empty() {
                    sections.push(PaletteSection::new(
                        "Files",
                        palette_items_ref_matching(&files, PaletteIcon::Glyph(IconName::File), &query),
                    ));
                }
                (SharedString::from(""), self.search_status(cx).into(), sections)
            }
        };
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
        // A scripted screenshot is a static composition, not an opening: the
        // card's enter presence (fade + rise) never settles inside a capture,
        // so screenshots draw the palette at rest — opaque, one surface.
        // Live opens keep the rise.
        let mut card = command_palette("palette", query, sections, selected)
            .placeholder(placeholder)
            .on_select(move |id, w, cx| select(id, w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        if self.still() {
            card = card.at_rest();
        }
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
                                v_flex()
                                    // The search field is the sidebar list's
                                    // box and carries its side margins;
                                    // centring the column lands the field's
                                    // visible box exactly on the card's, so
                                    // the two read as one surface.
                                    .items_center()
                                    .children(self.render_search_input(cx))
                                    .child(card),
                            ),
                    ),
            )
            .into_any_element(),
        )
    }

    fn render_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Read the modal out whole before anything asks `cx` for a listener:
        // the entity's borrow and `cx.listener` cannot be alive at once.
        let (title, detail, kind, primary_label, action, danger) = {
            let modal = self.overlays.read(cx).dialog.as_ref()?;
            let danger = modal.action == DialogAction::Archive;
            (modal.title.clone(), modal.detail.clone(), modal.kind, modal.primary, modal.action, danger)
        };
        // The archive target stays on the dialog until its own button runs:
        // closing it any other way drops the target with it.
        let secondary = if danger { "Cancel" } else { "Dismiss" };
        let primary = cx.listener(move |this: &mut Self, _: &(), window, cx| {
            if action == DialogAction::Archive {
                this.confirm_archive_dialog(window, cx);
                return;
            }
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
                    this.login.reset_to_choose();
                }
                DialogAction::Archive => {}
            }
            cx.notify();
        });
        let close = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // `cx.listener` hands back an opaque `Fn`, not a `Clone`, so the scrim
        // gets its own rather than sharing the secondary button's.
        let dismiss = cx.listener(|this: &mut Self, _: &(), _, cx| this.close_dialog(cx));
        // A deterministic capture draws the dialog settled rather than rising
        // in: the enter presence never lands on the same frame twice.
        let card = dialog("dialog", title)
            .kind(kind)
            .body(detail)
            .danger(danger)
            .secondary(secondary)
            .primary(primary_label)
            .on_primary(move |w, cx| primary(&(), w, cx))
            .on_secondary(move |w, cx| close(&(), w, cx))
            .on_dismiss(move |w, cx| dismiss(&(), w, cx));
        let card = if crate::clock::deterministic() { card.at_rest() } else { card };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .inset_0()
                    .key_context(aui::keys::MENU_CONTEXT)
                    .track_focus(&self.focus_dialog)
                    .on_action(cx.listener(|this, _: &Cancel, _, cx| this.close_dialog(cx)))
                    .child(card),
            )
            .into_any_element(),
        )
    }
}

/// One palette section's rows under one icon.
fn palette_items(
    rows: &[(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
) -> Vec<PaletteItem> {
    rows.iter().map(|(id, label, detail)| PaletteItem::new(id.clone(), icon, label.clone()).context(detail.clone())).collect()
}

/// [`palette_items`] over partitioned row references, which is what the search
/// palette's two sections are built from.
fn palette_items_ref(
    rows: &[&(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
) -> Vec<PaletteItem> {
    rows.iter().map(|(id, label, detail)| PaletteItem::new((*id).clone(), icon, (*label).clone()).context((*detail).clone())).collect()
}

/// [`palette_items_ref`] with the query's first hit in each label emphasised
/// through the row's own `matched` ranges. An empty query emphasises nothing.
fn palette_items_ref_matching(
    rows: &[&(SharedString, SharedString, SharedString)],
    icon: PaletteIcon,
    needle: &str,
) -> Vec<PaletteItem> {
    palette_items_ref(rows, icon)
        .into_iter()
        .map(|item| if needle.trim().is_empty() { item } else { item.matching(needle) })
        .collect()
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
        // A release outside the window never reaches the overlay: a drag that
        // is still armed while the window is inactive is over, settled where
        // it stands. One write, on the transition; the frame renders clean.
        if self.resizing && !window.is_window_active() {
            self.resizing = false;
            self.persist_width();
        }
        // The login screen owns the whole window; the shell is not built behind
        // it, so nothing of the signed-in state can leak into a capture.
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let body: AnyElement = if signed_in {
            let sidebar = self.render_sidebar(window, cx);
            let centre = self.render_centre(window, cx);
            // Painted lights off: the window owns real, glossy ones, and
            // the painted set only ever stacked underneath them.
            let shell = app_shell("shell")
                .sidebar_width(px(self.sidebar_width))
                .resizing(self.resizing)
                .traffic_lights(false)
                .sidebar_open(self.sidebar_open)
                .right_open(false)
                .header_sidebar(
                    sidebar_header("hd-side")
                        .traffic_lights(false)
                        .collapsed(!self.sidebar_open)
                        .on_toggle_sidebar(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
                        .on_search(cx.listener(|this, _, window, cx| this.open_search(window, cx))),
                )
                .header_centre(self.render_centre_header(window, cx))
                // The right pane is never opened; its header cell carries
                // nothing.
                .header_right(header_cell("hd-right").child(div()))
                .sidebar(sidebar)
                .rail(self.render_rail(cx))
                // The right pane is never opened; the slot stays empty.
                .right(div().size_full())
                .centre(centre)
                .into_any_element();
            // The strip sits over the divider, centred on the settled edge:
            // the handle reports the drag, this only positions it. Hidden
            // with the sidebar: there is no divider to grab on the rail.
            let mut stack = div().relative().size_full().child(shell);
            if self.sidebar_open {
                let press = cx.entity().downgrade();
                let travel = cx.entity().downgrade();
                let release = cx.entity().downgrade();
                stack = stack.child(
                    div()
                        .absolute()
                        .top(px(0.0))
                        .bottom(px(0.0))
                        .left(px(self.sidebar_width - RESIZE_HANDLE_W / 2.0))
                        .child(
                            resize_handle("sidebar-resize")
                                .on_drag_start(move |x, _, cx| {
                                    press.update(cx, |this, cx| this.begin_resize(x, cx)).ok();
                                })
                                .on_drag(move |x, _, cx| {
                                    travel.update(cx, |this, cx| this.drag_resize(x, cx)).ok();
                                })
                                .on_drag_end(move |_, cx| {
                                    release.update(cx, |this, cx| this.end_resize(cx)).ok();
                                }),
                        ),
                );
            }
            stack.into_any_element()
        } else {
            self.render_login(cx).into_any_element()
        };
        let dialog = self.render_dialog(cx);
        let palette = self.render_palette(cx);
        let toasts = self.render_toasts(cx);
        // Mid-drag the overlay covers the window, so the drag survives the
        // pointer outrunning the 6 px strip; moves alone would go silent.
        let capture: Option<AnyElement> = self.resizing.then(|| {
            let travel = cx.entity().downgrade();
            let release = cx.entity().downgrade();
            drag_capture_overlay("resize-capture")
                .on_drag(move |x, _, cx| {
                    travel.update(cx, |this, cx| this.drag_resize(x, cx)).ok();
                })
                .on_drag_end(move |_, cx| {
                    release.update(cx, |this, cx| this.end_resize(cx)).ok();
                })
                .into_any_element()
        });
        let overflow = self.render_overflow_menu(cx);
        let view_options = self.render_view_menu(cx);
        let account = self.render_account_menu(cx);
        // The palette takes the keyboard the frame it opens, so the arrows and
        // the return reach it rather than the composer under it. The search
        // palette is the exception: its query field owns the keyboard, and the
        // arrows and the return reach the list through the overlay's own menu
        // context.
        let searching = self.overlays.read(cx).palette.as_ref().is_some_and(|p| p.kind == PaletteKind::Search);
        if searching {
            let query = self.search_query.focus_handle(cx);
            if !query.is_focused(window) {
                window.focus(&query, cx);
            }
        } else if palette.is_some() && !self.focus_palette.is_focused(window) {
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
                .on_action(cx.listener(|this, _: &NewSession, _, cx| this.new_session(cx)))
                .on_action(cx.listener(|this, _: &Interrupt, _, cx| this.interrupt(cx)))
                .on_action(cx.listener(|this, _: &Cancel, window, cx| this.cancel(window, cx)))
                .on_action(cx.listener(|this, _: &FocusSearch, window, cx| this.open_search(window, cx)))
                .on_action(cx.listener(|_, _: &MinimizeWindow, window, _| window.minimize_window()))
                .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
                .on_action(cx.listener(|_, _: &ToggleTheme, window, cx| AuiTheme::toggle_kind(Some(window), cx)))
                .on_action(cx.listener(|this, _: &ShowAbout, _, cx| this.show_about(cx)))
                .on_action(cx.listener(|this, _: &ShowDocs, _, _| this.show_docs()))
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
                .children(capture)
                .children(toasts)
                .children(palette)
                .children(dialog)
                .children(overflow)
                .children(view_options)
                .children(account),
        )
    }
}

/// Two taps with no travel count as a double-click: the handle reports
/// positions only, never the click count, so recency is the reset signal.
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);
/// Where Help → Harness Documentation looks for the docs folder: beside the
/// working directory first, then three ancestors above the executable
/// (`target/debug/harness` is three levels below the repo root: the exe
/// itself, `debug/`, `target/`). Pure so tests can drive it.
/// `find_docs_dir`, resolved once and cached: neither the working directory
/// nor the executable path change while the app runs, so probing the
/// filesystem for it on every `show_docs` keypress (finding `app-core-17`)
/// gains nothing over doing it the first time.
fn docs_dir() -> Option<&'static std::path::Path> {
    static DOCS_DIR: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    DOCS_DIR
        .get_or_init(|| {
            let cwd = std::env::current_dir().ok();
            let exe = std::env::current_exe().ok();
            find_docs_dir(cwd.as_deref(), exe.as_deref())
        })
        .as_deref()
}

fn find_docs_dir(
    cwd: Option<&std::path::Path>,
    exe: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    let from_cwd = cwd.map(|cwd| cwd.join("docs")).filter(|dir| dir.is_dir());
    if from_cwd.is_some() {
        return from_cwd;
    }
    exe.and_then(|exe| exe.ancestors().nth(3))
        .map(|root| root.join("docs"))
        .filter(|dir| dir.is_dir())
}

impl Harness {
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// The divider's settled x, for `layout.json`. Small and synchronous like
    /// the sessions store: one pretty object, best-effort.
    fn persist_width(&self) {
        layout::write(&layout::Layout { sidebar_width: Some(self.sidebar_width) });
    }

    /// The press on the resize strip: arm the drag from the grab point.
    fn begin_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        self.resizing = true;
        self.grab_x = x;
        self.start_w = self.sidebar_width;
        self.drag_moved = 0.0;
        cx.notify();
    }

    /// A move with the button held: the divider follows from where the drag
    /// started, clamped, with no spring between it and the pointer.
    fn drag_resize(&mut self, x: f32, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        let width = layout::drag_width(self.start_w, self.grab_x, x);
        self.drag_moved = self.drag_moved.max((width - self.start_w).abs());
        self.sidebar_width = width;
        cx.notify();
    }

    /// The release, wherever it lands: disarm, settle, persist. A release
    /// with no travel shortly after the previous one is the handle's
    /// double-click, which resets to the default width instead of keeping a
    /// tap that moved nothing.
    fn end_resize(&mut self, cx: &mut Context<Self>) {
        if !self.resizing {
            return;
        }
        self.resizing = false;
        let now = std::time::Instant::now();
        if self.drag_moved < 2.0
            && self.last_release.is_some_and(|last| now.duration_since(last) < DOUBLE_CLICK_WINDOW)
        {
            self.sidebar_width = SIDEBAR_WIDTH;
            self.last_release = None;
        } else {
            self.last_release = Some(now);
        }
        self.persist_width();
        cx.notify();
    }

    /// Harness → About Harness.
    fn show_about(&mut self, cx: &mut Context<Self>) {
        self.set_dialog(
            cx,
            Dialog {
                title: "About Harness".into(),
                detail: format!(
                    "Harness {} \u{2014} a macOS chat interface to Muse Code.\n\nKeys: docs/08-keymap.md. App: docs/02-app.md.",
                    env!("CARGO_PKG_VERSION")
                ),
                kind: DialogKind::Info,
                primary: "OK",
                action: DialogAction::Dismiss,
                archive_target: None,
            },
        );
    }

    /// Help → Harness Documentation: the docs folder in Finder.
    ///
    /// The docs live beside the repo, so this looks for them next to the
    /// working directory first (`cargo run` from the repo root) and then
    /// three ancestors above the executable (`target/debug/harness` is
    /// three levels below the root). A bundled app moved away from the
    /// repo has no docs beside it, and that is an `eprintln`, not a dialog.
    fn show_docs(&mut self) {
        match docs_dir() {
            Some(dir) => {
                if std::process::Command::new("open").arg(dir).spawn().is_err() {
                    crate::harness_log!("could not reveal {}", dir.display());
                }
            }
            None => crate::harness_log!("no docs folder beside the app"),
        }
    }

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
        // One action exists, and it is Undo — one hidden row, one "Clear
        // empty" batch, or one archived session, whichever the newest toast
        // was for.
        let act = cx.listener(move |this: &mut Self, _: &(), _, cx| {
            this.undo_newest(cx);
            this.overlays.update(cx, |overlays, _| overlays.dismiss_toast(&newest));
            cx.notify();
        });
        // A deterministic capture draws the stack settled: toasts slide in,
        // which never lands on the same frame twice.
        let stack = aui::feedback::toast_stack("toasts", toasts);
        let stack = if crate::clock::deterministic() { stack.at_rest() } else { stack };
        Some(
            popover_layer(
                div()
                    .absolute()
                    .right(px(scale::SP_5))
                    .top(px(TOAST_TOP))
                    .w(px(TOAST_W))
                    .h(px(TOAST_STACK_H))
                    .child(
                        stack
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
    /// the composer is empty (spec §3.9). On the login screen there is no
    /// session stack: Escape walks the login states instead — `Cancel` in
    /// `Starting` / `Device`, `Back` in `ApiKey`, `ChooseAnother` in `Error`,
    /// nothing in the other states.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed = self.overlays.update(cx, |overlays, _| overlays.close_topmost());
        if closed {
            cx.notify();
            return;
        }
        if !matches!(self.auth, Auth::SignedIn(_)) {
            match &self.login.state {
                LoginState::Starting | LoginState::Device { .. } => {
                    self.login_intent(LoginIntent::Cancel, window, cx);
                }
                LoginState::ApiKey { .. } => {
                    self.login_intent(LoginIntent::Back, window, cx);
                }
                LoginState::Error { .. } => {
                    self.login_intent(LoginIntent::ChooseAnother, window, cx);
                }
                _ => {}
            }
            return;
        }
        // An open rename is the next thing Escape takes back. (There is no
        // sidebar search field left to clear: ⌘⇧F owns search now, and its
        // palette closes through the overlay stack above.)
        if self.renaming.take().is_some() {
            self.focus_composer = true;
            cx.notify();
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

#[cfg(test)]
mod tests {
    use super::find_docs_dir;
    use std::path::PathBuf;

    /// A scratch root with an optional `docs/` child, removed on drop.
    struct Scratch {
        root: PathBuf,
    }

    impl Scratch {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "harness-docs-test-{}-{}",
                std::process::id(),
                name
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Self { root }
        }

        fn with_docs(&self) -> &Self {
            std::fs::create_dir_all(self.root.join("docs")).unwrap();
            self
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn docs_prefers_cwd_then_exe_ancestors_then_none() {
        let home = Scratch::new("home");
        home.with_docs();
        // An exe two levels below a root carrying `docs/`.
        let exe_root = Scratch::new("exeroot");
        exe_root.with_docs();
        let exe = exe_root.root.join("target").join("debug").join("harness");
        let elsewhere = Scratch::new("elsewhere");

        assert_eq!(
            find_docs_dir(Some(&home.root), Some(&exe)),
            Some(home.root.join("docs")),
            "a docs folder beside the cwd wins over the exe ancestors",
        );
        assert_eq!(
            find_docs_dir(Some(&elsewhere.root), Some(&exe)),
            Some(exe_root.root.join("docs")),
            "without docs beside the cwd, the exe ancestors are the fallback",
        );
        assert_eq!(
            find_docs_dir(Some(&elsewhere.root), None),
            None,
            "no docs anywhere and no exe to search from is None",
        );
    }
}
