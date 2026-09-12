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
//!   intents and never waits, and [`crate::wire::WireCall`] is the one shape
//!   every one of those calls has.
//!
//! # Screens
//!
//! Two, and the auth probe decides which: the login screen (spec §3.2,
//! superseded by `docs/diagnosis/login.md`) or the shell. The shell's right
//! pane is not used; the column is always closed.
//!
//! # What lives elsewhere
//!
//! [`Harness`] keeps the fields, boot, connect, route and `render`, but most
//! of its concerns have their own modules and reach back in through one call
//! per seam:
//!
//! * [`crate::login`] — the login screen, the `account/*` lane, the device
//!   and API-key flows, sign-out, and `render_login`.
//! * [`crate::sidebar_view`] — the sidebar column: nav block, session rows,
//!   the empty states, the rename field, the footer, the rail, and the two
//!   popovers anchored to the column.
//! * [`crate::dialogs`] — what floats over the window: the modal, the
//!   palette, the toast stack and the header's overflow menu.
//! * [`crate::billing`] — the tier probe's lifecycle and the banner it hands
//!   to the open session.
//! * [`crate::resize`] — the sidebar divider's drag.
//! * [`crate::steps`] — `--steps` and `--login-steps`: the verb tables, the
//!   parser and the two runners.
//! * [`crate::wire`] — background call, then update.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use aui::composer::composer_state_rows;
use aui::data::{button, icon_button, ButtonSize};
use aui::feedback::{banner, BannerKind, BannerRun};
use aui::keys::{Cancel, FocusNext, FocusPrev, TogglePalette, ToggleSidebar};
use aui::overlay::DialogKind;
use aui::shell::{
    RESIZE_HANDLE_W, app_shell, clamp_sidebar_width, drag_capture_overlay,
    header_cell, resize_handle, sidebar_header,
};
use aui_icons::{provider_mark, IconName, Provider};
use aui_tokens::{scale, ActiveAui, AuiStyled, AuiTheme};
use futures::channel::mpsc::UnboundedReceiver;
use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, AnyElement, App, Context, Entity, FocusHandle, Focusable, KeyBinding,
    ScrollHandle, SharedString, Subscription, Task, Window,
};
use gpui_kit::base::input::{InputEvent, InputState, TextareaState};
use gpui_kit::base::{h_flex, v_flex};
use muse_client::schema::{
    AccountStateKind, SessionListParams, SessionResumeParams, SessionStartParams,
};
use muse_client::{new_command_id, MuseClient, MuseError, MuseEvent};

use crate::auth::Identity;
use crate::conn::{self, Severity};
use crate::index::{self, IndexEntry};
use crate::login::{Auth, Login};
use crate::overlays::{Dialog, DialogAction, MenuKind, Overlays, Palette, PaletteKind};
use crate::resize::ResizeDrag;
use crate::shot::CaptureToken;
use crate::session::{SessionEvent, SessionHost, SessionView};
use crate::tier::Tier;
use crate::sessions::{self, SessionMeta};
use crate::app::list::ListCache;
use crate::sidebar::{self, SessionEntry};
use crate::search::{FileHit, SessionHit};
use crate::wire::WireCall;
use crate::{layout, Args};

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
pub(crate) const RENAME_CONTEXT: &str = "HarnessRename";

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
pub(crate) const TOAST_W: f32 = 320.0;
/// Where the stack hangs from: under the window header, at the right edge.
/// The stack lays its toasts out **downward** from its own box, so it is
/// anchored by its top; hanging it off the bottom would draw the newest toast
/// off the end of the window.
pub(crate) const TOAST_TOP: f32 = 56.0;
/// How much room the fanned stack is given before it would clip.
pub(crate) const TOAST_STACK_H: f32 = 260.0;
/// How long the "Session hidden" toast's Undo stays honest.
const UNDO_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
/// How far below the window's top edge the palette hangs, and how dark the
/// ground behind it goes. The library's `palette_scrim` is a design-card block
/// of a fixed height; a window overlay places itself.
pub(crate) const PALETTE_TOP: f32 = 96.0;
pub(crate) const PALETTE_SCRIM: f32 = 0.4;
/// How many rows the palette lists. The sidebar's search is the way through a
/// longer list; this is the way back to something recent.
pub(crate) const PALETTE_ROWS: usize = 12;
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
        // Same as the red dot: hide the app rather than remove the window,
        // so the session and the `muse serve` child survive and the Dock
        // icon or Cmd-Tab bring the same window back (`main.rs`,
        // `open_shell_window`). Deferred: menu dispatch already holds this
        // window in an update.
        cx.defer(|cx| cx.hide());
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

mod find;
mod lifecycle;
mod list;

/// The connection's own state, which is what the reconnect banner reads.
pub(crate) enum Wire {
    /// Spawning `muse serve` and shaking hands.
    Connecting,
    /// Live.
    Ready,
    /// The child exited; respawning and resuming (docs/01-transport.md §3).
    Reconnecting,
    /// The respawn failed. The dialog offers another try.
    Down(String),
}

/// What one toast's Undo restores: one `/hide` is a batch of one, one
/// "Clear empty" is a batch of everything it hid, and one archive confirm is
/// a batch of one archived session.
#[derive(Clone, Debug, PartialEq, Eq)]
enum UndoBatch {
    Hidden(Vec<String>),
    Archived(Vec<String>),
}

/// What decides the search card's status line: whether the query is blank,
/// and how many sessions and files came back (finding `performance-9`).
type SearchStatusKey = (bool, usize, usize);

/// The whole application.
pub struct Harness {
    pub(crate) args: Args,
    pub(crate) client: Option<Arc<MuseClient>>,
    pub(crate) wire: Wire,
    pub(crate) auth: Auth,
    pub(crate) login: Login,
    /// Rows from `session/list`, joined with the local index.
    pub(crate) sessions: Vec<SessionEntry>,
    /// Bumped by [`Harness::invalidate_list`] whenever anything the sidebar
    /// and the palette read out of [`Harness::sessions`] changed: the rows
    /// themselves, the overrides that relabel them, the index, or the
    /// show-hidden/empty/archived flags. It is the key [`Harness::list_cache`]
    /// is validated against (findings `performance-5`, `support-2`).
    list_epoch: u64,
    /// One sorted visible list and one grouping per change, not per frame.
    list_cache: RefCell<ListCache>,
    index: HashMap<String, IndexEntry>,
    pub(crate) active: Option<Entity<SessionView>>,
    /// The session the UI is pointed at: the sidebar click's target, set the
    /// moment `resume` runs. The centre swaps synchronously, so this usually
    /// names the active view — but the row highlights and the header label
    /// read it, never the view, so the click is acknowledged on its own frame.
    pub(crate) pending_id: Option<String>,
    /// Parked session views, most-recently-opened first: an MRU of eight.
    /// Switching away parks the view (its event subscription dropped, its
    /// fold, scroll position and draft kept); reopening shows it at once and
    /// tops it up from its last cursor.
    session_cache: Vec<(String, Entity<SessionView>)>,
    /// Everything that floats: the modal, the open menu and the toasts. One
    /// entity, shared with the session view, which renders the halves that hang
    /// off the composer's own chips (spec §2.3).
    pub(crate) overlays: Entity<Overlays>,
    pub(crate) sidebar_open: bool,
    /// The sidebar divider's width and whatever drag is in flight over it
    /// (see [`crate::resize`]).
    pub(crate) resize: ResizeDrag,
    /// The sessions list's scroll state, tracked so the Sessions view menu
    /// can anchor under the caption's sliders icon: the caption scrolls with
    /// the list, so its visible position is its content position minus this
    /// offset. One handle for the window's life, so the state persists
    /// across frames.
    pub(crate) sessions_scroll: ScrollHandle,
    /// The two flags a `--screenshot` wait reads out of this window (see
    /// [`crate::shot::CaptureToken`]). Handed to every session view this
    /// window opens and to `capture_and_quit`, so a second window would wait
    /// on its own.
    pub(crate) capture: CaptureToken,
    /// Whether `initialize` granted `userShell`. Requested in `conn::connect`;
    /// a server that did not grant it disables the `!` path with a banner
    /// rather than letting the command fail on the wire.
    user_shell: bool,
    focus_root: FocusHandle,
    pub(crate) focus_dialog: FocusHandle,
    pub(crate) focus_palette: FocusHandle,
    /// Set when the next frame should move the keyboard to the composer.
    focus_composer: bool,
    /// What the billing probe said, or `None` while it has not said it yet
    /// (spec §3.2, Phase 5 A1). A probe that failed is
    /// [`Tier::Unavailable`], never `None`.
    pub(crate) tier: Option<Tier>,
    /// A probe is in flight; a second one is not started on top of it.
    pub(crate) tier_probing: bool,
    /// The harness's own facts about each session: its name, whether it is
    /// hidden, and the title derived from its first shell command (spec §3.7).
    overrides: sessions::Overrides,
    /// Whether hidden sessions are listed anyway (the Sessions menu's toggle).
    pub(crate) show_hidden: bool,
    /// Whether sessions with no turns are listed anyway (the Sessions menu's toggle).
    pub(crate) show_empty: bool,
    /// Whether archived sessions are listed anyway (the Sessions menu's toggle).
    pub(crate) show_archived: bool,
    /// The search palette's query field. The card's own query row shows the
    /// result count; typing here re-queries `search.db` off the UI thread.
    /// There is no sidebar quick-filter: ⌘⇧F and the sidebar search icon open
    /// only this palette, so the two can never be open together.
    pub(crate) search_query: Entity<TextareaState>,
    /// Full-text session hits for the open search palette, latest query only.
    search_sessions: Vec<SessionHit>,
    /// Created-file hits for the open search palette, latest query only.
    search_files: Vec<FileHit>,
    /// Monotonic id for palette queries; only the latest result is applied.
    search_epoch: u64,
    /// The session whose row is being renamed in place, and the field doing it.
    pub(crate) renaming: Option<String>,
    pub(crate) rename: Entity<TextareaState>,
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
    /// The `(list_epoch, active session id)` that title was built for, so the
    /// scan and the `format!` behind it run on a change rather than on every
    /// frame (finding `performance-9`).
    window_title_key: Option<(u64, Option<String>)>,
    /// The search card's status line and the three facts it is made of:
    /// whether the query is blank and the two result counts (finding
    /// `performance-9`). Only ever read while the search palette is open.
    search_status: RefCell<Option<(SearchStatusKey, SharedString)>>,
    /// "Send anyway" was pressed. Once per app run, deliberately: a person who
    /// accepted the bill this morning should be asked again tomorrow.
    pub(crate) send_anyway: bool,
    tasks: Vec<Task<()>>,
    subscriptions: Vec<Subscription>,
}

impl WireCall for Harness {
    fn wire_tasks(&mut self) -> &mut Vec<Task<()>> {
        &mut self.tasks
    }
}

impl Harness {
    /// Boot: read `auth.json`, then connect and finish the probe.
    pub fn new(args: Args, capture: CaptureToken, window: &mut Window, cx: &mut Context<Self>) -> Self {
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
            login: Login::new(api_key.clone()),
            sessions: Vec::new(),
            list_epoch: 0,
            list_cache: RefCell::new(ListCache::default()),
            index: HashMap::new(),
            active: None,
            pending_id: None,
            session_cache: Vec::new(),
            overlays: cx.new(|_| Overlays::default()),
            sidebar_open: true,
            resize: ResizeDrag::restored(restored),
            sessions_scroll: ScrollHandle::new(),
            capture,
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
            window_title_key: None,
            search_status: RefCell::new(None),
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
        self.wire_call(cx, move || conn::connect(&program), |this, result, cx| match result {
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
    }

    /// Drain the bridge onto the UI thread, one event at a time, in wire order.
    pub(crate) fn pump(&mut self, mut events: UnboundedReceiver<MuseEvent>, cx: &mut Context<Self>) {
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
        // A started turn titles the new session's local row: the wire still
        // does not list it, but the prompt the view sent is known.
        let started: Option<String> = match &event {
            MuseEvent::Notification { method, params, session_id, .. } if method == "turn/started" => session_id
                .clone()
                .or_else(|| params.get("sessionId").and_then(|v| v.as_str()).map(str::to_owned)),
            _ => None,
        };
        if let Some(active) = &self.active {
            active.update(cx, |view, cx| view.apply(event, cx));
        }
        if let Some(session_id) = started {
            let prompt = self
                .active
                .as_ref()
                .filter(|view| view.read(cx).session_id == session_id)
                .and_then(|view| view.read(cx).first_prompt_text());
            if let Some(entry) = self.sessions.iter_mut().find(|entry| entry.id == session_id && entry.local) {
                if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty()) {
                    entry.label = prompt;
                }
                self.invalidate_list();
            }
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
    pub(crate) fn reconnect(&mut self, cx: &mut Context<Self>) {
        self.client = None;
        let program = self.args.program.clone();
        let resume = self
            .active
            .as_ref()
            .map(|a| (a.read(cx).session_id.clone(), a.read(cx).last_cursor()));
        let work = move || {
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
        };
        self.wire_call(cx, work, |this, result, cx| match result {
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
    }

    // ---------------------------------------------------------- billing tier

    // ---------------------------------------------------------------- render

    /// Whether this window draws settled rather than entering: a
    /// `--screenshot` run, or a deterministic capture
    /// (`HARNESS_DETERMINISTIC=1`), which is always a static composition even
    /// without a screenshot on the end.
    pub(crate) fn still(&self) -> bool {
        self.args.screenshot.is_some() || crate::clock::deterministic()
    }

    /// The sidebar's collapse toggle: the rail is the column at zero width.
    pub(crate) fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// The centre header: the active session's label ("Harness" with nothing
    /// open), the provider mark, and the overflow menu — and nothing else.
    /// The library's `centre_header` always paints the right-pane toggle and
    /// the right header always paints its close button, so the shell gets a
    /// plain cell with the same title construction instead. The shell's own
    /// drag region wraps the whole header row, and buttons keep their clicks.
    fn render_centre_header(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let p = cx.aui().colors;
        // The click's target, from the list entry — never the view, so the
        // header answers on the click's own frame, before any page arrives.
        let target =
            self.pending_id.clone().or_else(|| self.active.as_ref().map(|view| view.read(cx).session_id.clone()));
        let label = target
            .and_then(|id| self.sessions.iter().find(|e| e.id == id).map(|e| e.label.clone()))
            .unwrap_or_else(|| "Harness".to_owned());
        let overflow =
            cx.listener(|this: &mut Self, _: &gpui::ClickEvent, _, cx| this.open_menu(MenuKind::Overflow, cx));
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
        // No expand button: the header row stands still while the sidebar
        // collapses, so the toggle in the sidebar header stays put and the
        // centre cell never slides under the native lights.
        let cell = header_cell("hd-centre");
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
        let capture = self.capture.clone();
        // The capture names its own session; this id is a placeholder the view
        // replaces the moment the first line is folded.
        let view = cx.new(|cx| {
            let host = SessionHost { provider_id: provider, workspace, overlays, capture };
            let mut view = SessionView::new("replay".to_owned(), None, host, window, cx);
            view.set_at_rest(at_rest);
            view.load_replay(&path, cx);
            view.load_history(cx);
            view
        });
        self.subscriptions.clear();
        self.subscriptions.push(cx.subscribe(&view, |this, view, event, cx| this.on_session_event(view, event, cx)));
        self.sessions = vec![SessionEntry::replayed(&view.read(cx).session_id, &path)];
        self.invalidate_list();
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
        // The one write left in the render tree, and it only ever fires on a
        // `--replay` window's first frame: `open_replay` starts a stream
        // cadenced on the wall clock, so where in the frame it runs decides
        // where in that stream a fixed-delay capture lands. Everything else a
        // frame changes is in [`Self::on_frame`].
        self.open_replay(window, cx);
        let banner = self.render_wire_banner(cx);
        let body = match self.active.clone() {
            Some(view) => view.update(cx, |view, cx| view.render_centre(window, cx)),
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

    pub(crate) fn with_session(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut SessionView, &mut Context<SessionView>)) {
        if let Some(view) = self.active.clone() {
            view.update(cx, |view, cx| f(view, cx));
        }
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
    pub(crate) fn reveal_created(&mut self, rest: &str, cx: &mut Context<Self>) {
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
    /// Three facts decide it — whether the query is blank and how many
    /// sessions and files came back — so it is built when one of them changes
    /// and not on every frame the palette is open (finding `performance-9`).
    pub(crate) fn search_status(&self, cx: &gpui::App) -> SharedString {
        let blank = self.search_query.read(cx).value().trim().is_empty();
        let key = (blank, self.search_sessions.len(), self.search_files.len());
        let mut cache = self.search_status.borrow_mut();
        if let Some((cached, status)) = cache.as_ref() {
            if *cached == key {
                return status.clone();
            }
        }
        let (_, sessions, files) = key;
        let status: SharedString = if blank {
            "Search sessions and created files".into()
        } else if sessions + files == 0 {
            "No matches".into()
        } else {
            let mut parts = Vec::new();
            if sessions > 0 {
                parts.push(format!("{sessions} session{}", if sessions == 1 { "" } else { "s" }));
            }
            if files > 0 {
                parts.push(format!("{files} file{}", if files == 1 { "" } else { "s" }));
            }
            parts.join(" \u{00b7} ").into()
        };
        *cache = Some((key, status.clone()));
        status
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

impl Harness {
    /// Everything a frame changes before it draws anything: the window title,
    /// a resize left armed by a release the window never saw, the `--replay`
    /// kick, and the one-shot composer focus.
    ///
    /// These are lifecycle, not composition (findings `app-core-9` and
    /// `app-core-10`). Keeping them in one pre-pass is what lets `render` and
    /// every `render_*` below it read state and build elements without ever
    /// writing, so what a frame shows is decided before the first element is
    /// made rather than part-way down the tree.
    ///
    /// One kick stays out of it: `--replay`'s (see
    /// [`Self::render_centre`]). It starts a wall-clock-cadenced stream, so
    /// moving it above the sidebar's composition moves the whole replay
    /// forward by however long that composition takes, and the reference
    /// captures are taken at a fixed delay into that stream.
    fn on_frame(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // The window's title is the session's, so a person with three harness
        // windows open can tell them apart in Mission Control. It is built
        // only when the list or the open session changed: the scan and the
        // `format!` behind it have no business in a steady-state frame
        // (finding `performance-9`).
        let stale = {
            let active = self.active.as_ref().map(|a| a.read(cx).session_id.as_str());
            self.window_title_key.as_ref().map(|(epoch, id)| (*epoch, id.as_deref()))
                != Some((self.list_epoch, active))
        };
        if stale {
            let title = self.window_title(cx);
            if self.window_title.as_deref() != Some(title.as_str()) {
                window.set_window_title(&title);
                self.window_title = Some(title);
            }
            self.window_title_key = Some((self.list_epoch, self.active_id(cx)));
        }
        // A release outside the window never reaches the overlay: a drag that
        // is still armed while the window is inactive is over, settled where
        // it stands. One write, on the transition; the frame renders clean.
        if self.resize.active && !window.is_window_active() {
            self.resize.active = false;
            self.resize.persist();
        }
        // The shell's lifecycle only: the login screen owns the whole window
        // and has no session behind it.
        if !matches!(self.auth, Auth::SignedIn(_)) {
            return;
        }
        // A deterministic capture never takes keyboard focus: a focused
        // composer paints the textarea's blinking caret, which lands on a
        // different phase every run.
        if std::mem::take(&mut self.focus_composer) && !crate::clock::deterministic() {
            if let Some(view) = self.active.clone() {
                view.update(cx, |view, cx| view.focus_composer(window, cx));
            }
        }
    }
}

impl Render for Harness {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.on_frame(window, cx);
        // The login screen owns the whole window; the shell is not built behind
        // it, so nothing of the signed-in state can leak into a capture.
        let signed_in = matches!(self.auth, Auth::SignedIn(_));
        let body: AnyElement = if signed_in {
            let sidebar = self.render_sidebar(window, cx);
            let centre = self.render_centre(window, cx);
            // Painted lights off: the window owns real, glossy ones, and
            // the painted set only ever stacked underneath them.
            let shell = app_shell("shell")
                .sidebar_width(px(self.resize.width))
                .resizing(self.resize.active)
                .traffic_lights(false)
                .sidebar_open(self.sidebar_open)
                // The header row stands still while the pane collapses: the
                // sidebar cell keeps its width — and the native-lights
                // reservation — so the toggle and search stay where they are.
                .header_follows_sidebar(false)
                .right_open(false)
                .header_sidebar(
                    sidebar_header("hd-side")
                        .native_lights(true)
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
                        .left(px(self.resize.width - RESIZE_HANDLE_W / 2.0))
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
        let capture: Option<AnyElement> = self.resize.active.then(|| {
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
            self.login_escape(window, cx);
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
